//! Mutating apply-mode entry point for the shard fan-out migration owned by
//! `bd-f6jfs.4`. This module wraps the existing planner and source-preservation
//! helpers in [`crate::db::shard`] to provide a single deterministic call site
//! the CLI invokes when running `ee migrate shard-fanout` without `--dry-run`.
//!
//! Per the bead's scope and `AGENTS.md`, the apply path:
//!
//! * never deletes the legacy database — preservation is by rename/copy via
//!   [`shard::preserve_shard_fanout_source_database`];
//! * refuses to advance when the planner reported blockers, returning an
//!   `Outcome::Blocked` report rather than partially mutating state;
//! * copies workspace-owned rows into deterministic per-workspace shard DBs;
//! * writes a thin catalog row for each migrated workspace with source,
//!   target, rollback, and verification hashes;
//! * uses stable ordering, fixed schema-migration timestamps, and deterministic
//!   migration audit IDs so idempotent reruns do not duplicate rows.
//!
//! Down-stream beads (`bd-f6jfs.7`, `bd-f6jfs.9`) consume the structured
//! report shape defined here so backup/restore and rollback paths can compose
//! against a single source of truth for "what did the migration actually do".

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use sqlmodel_core::Value;

use crate::db::shard::{
    self, SHARD_FANOUT_CATALOG_SCHEMA_VERSION, SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1,
    ShardFanoutMigrationPlan, ShardFanoutMigrationPlanInput, ShardFanoutMigrationWorkspaceInput,
    ShardFanoutMigrationWorkspacePlan, ShardFanoutPreserveSourceError,
    ShardFanoutPreservedSourceReport,
};
use crate::db::{DatabaseConfig, DbConnection, MIGRATIONS, MigrationRecord, WalCheckpointMode};
use crate::models::DomainError;

/// Schema id for the apply-mode report. Distinct from the planner schema so
/// CLI consumers can discriminate dry-run output from a mutating-apply outcome.
pub const SHARD_FANOUT_MIGRATION_APPLY_REPORT_SCHEMA_V1: &str =
    "ee.migration.shard_fanout.apply.v1";

/// Degraded code emitted when the planner reported blockers and the apply
/// path refuses to advance. The structured blockers are echoed into the
/// report's `degraded` array via `ShardFanoutMigrationApplyDegradation`.
pub const SHARD_FANOUT_APPLY_PLAN_BLOCKED_CODE: &str = "shard_fanout_apply_plan_blocked";

pub const SHARD_FANOUT_TARGET_HASH_MISMATCH_CODE: &str = "shard_fanout_target_hash_mismatch";

const DETERMINISTIC_MIGRATION_APPLIED_AT: &str = "1970-01-01T00:00:00Z";
const MIGRATION_AUDIT_TIMESTAMP: &str = "1970-01-01T00:00:00Z";
const MIGRATION_AUDIT_ACTION: &str = "migration.shard_fanout";
const MIGRATION_ACTOR: &str = "ee migrate shard-fanout";
const CATALOG_TABLE: &str = "shard_fanout_catalog";
const MIGRATION_TABLE_DDL: &str = "CREATE TABLE IF NOT EXISTS ee_schema_migrations (
    version INTEGER PRIMARY KEY CHECK (version > 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    checksum TEXT NOT NULL CHECK (length(trim(checksum)) > 0),
    applied_at TEXT NOT NULL CHECK (length(trim(applied_at)) > 0)
)";
const MIGRATION_TABLE_NAME_INDEX_DDL: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_ee_schema_migrations_name ON ee_schema_migrations(name)";
const CATALOG_DDL: &str = "CREATE TABLE IF NOT EXISTS shard_fanout_catalog (
    workspace_id TEXT PRIMARY KEY CHECK (length(trim(workspace_id)) > 0),
    workspace_registry_mirror TEXT NOT NULL CHECK (json_valid(workspace_registry_mirror)),
    shard_id TEXT NOT NULL CHECK (length(trim(shard_id)) > 0),
    shard_path TEXT NOT NULL CHECK (length(trim(shard_path)) > 0),
    catalog_schema_version INTEGER NOT NULL CHECK (catalog_schema_version > 0),
    shard_generation INTEGER NOT NULL CHECK (shard_generation > 0),
    migration_state TEXT NOT NULL CHECK (length(trim(migration_state)) > 0),
    last_verified_hashes TEXT NOT NULL CHECK (json_valid(last_verified_hashes)),
    rollback_available INTEGER NOT NULL CHECK (rollback_available IN (0, 1)),
    preserved_source_path TEXT NOT NULL CHECK (length(trim(preserved_source_path)) > 0),
    source_database_path TEXT NOT NULL CHECK (length(trim(source_database_path)) > 0),
    source_database_hash TEXT,
    target_database_hash TEXT,
    migrated_at TEXT NOT NULL CHECK (length(trim(migrated_at)) > 0)
)";

/// Outcome classification used by the apply report and persisted into the
/// `localBuildPolicy`-style structured-error surface so support bundles and
/// completion-audit consumers can describe the migration without re-running
/// it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardFanoutMigrationOutcome {
    /// Apply succeeded for everything the current slice covers: source was
    /// preserved and per-workspace shard paths were resolved.
    Applied,
    /// A prior apply already produced the preserved-source artifact with a
    /// matching hash; nothing was mutated this call.
    AlreadyApplied,
    /// The planner emitted blockers or the source database was unreachable;
    /// the apply path refused to advance and reported the blockers verbatim.
    Blocked,
}

impl ShardFanoutMigrationOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::AlreadyApplied => "already_applied",
            Self::Blocked => "blocked",
        }
    }
}

/// Per-workspace summary returned by the apply path. Mirrors the planner
/// row counts but drops blocker text (echoed at report level) so consumers
/// can iterate by workspace without re-walking the planner output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationWorkspaceOutcome {
    pub workspace_id: String,
    pub workspace_root: PathBuf,
    pub shard_id: Option<String>,
    pub shard_path: Option<PathBuf>,
    pub row_copy_status: &'static str,
    pub planned_row_count: Option<u64>,
    pub row_counts_by_table: BTreeMap<String, u64>,
    pub copied_row_counts_by_table: BTreeMap<String, u64>,
    pub copied_row_count: u64,
    pub source_hash: Option<String>,
    pub target_database_hash: Option<String>,
    pub audit_event_id: Option<String>,
    pub blocker_count: usize,
}

impl ShardFanoutMigrationWorkspaceOutcome {
    fn blocked_from_plan(plan: &ShardFanoutMigrationWorkspacePlan) -> Self {
        Self {
            workspace_id: plan.workspace_id.clone(),
            workspace_root: plan.workspace_root.clone(),
            shard_id: plan.shard_id.clone(),
            shard_path: plan.shard_path.clone(),
            row_copy_status: "blocked_by_plan",
            planned_row_count: plan.planned_row_count,
            row_counts_by_table: plan.row_counts_by_table.clone(),
            copied_row_counts_by_table: BTreeMap::new(),
            copied_row_count: 0,
            source_hash: plan.source_hash.clone(),
            target_database_hash: None,
            audit_event_id: None,
            blocker_count: plan.blockers.len(),
        }
    }
}

/// Structured degradation echoed into the apply report. Keeps the planner's
/// stable codes verbatim and lets callers distinguish planner blockers from
/// stubbed row-copy work without re-parsing message text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationApplyDegradation {
    pub code: String,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

/// Apply-mode report. Stable, redaction-safe shape. The `preservedSource`
/// field is `Some(..)` whenever the apply path actually invoked the source
/// preservation helper; on `Blocked` outcomes it is `None`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationApplyReport {
    pub schema: &'static str,
    pub outcome: ShardFanoutMigrationOutcome,
    pub plan_schema: &'static str,
    pub elapsed_ms: u64,
    pub source_database_path: PathBuf,
    pub preserved_source_database_path: PathBuf,
    pub source_database_hash: Option<String>,
    pub shard_root: Option<PathBuf>,
    pub catalog_path: Option<PathBuf>,
    pub catalog_rows_written: usize,
    pub workspaces: Vec<ShardFanoutMigrationWorkspaceOutcome>,
    pub preserved_source: Option<ShardFanoutPreservedSourceReport>,
    pub degraded: Vec<ShardFanoutMigrationApplyDegradation>,
}

impl ShardFanoutMigrationApplyReport {
    /// Compatibility shim for the initial apply stub. Real row copy now
    /// executes in this module, so this must remain false.
    #[must_use]
    pub fn row_copy_unimplemented(&self) -> bool {
        false
    }
}

/// Build the shard fan-out migration plan from a source database and annotate
/// each workspace with deterministic per-table row counts.
pub fn plan_shard_fanout_migration_from_database(
    source_database_path: PathBuf,
    shards_dir_override: Option<PathBuf>,
) -> Result<ShardFanoutMigrationPlan, DomainError> {
    let mut workspaces = Vec::new();
    let source_exists = source_database_path.exists();
    let source = if source_exists {
        let conn = DbConnection::open_file(&source_database_path).map_err(|error| {
            storage_error(
                format!("Failed to open source database: {error}"),
                Some("ee doctor".to_owned()),
            )
        })?;
        workspaces = conn
            .list_workspaces()
            .map_err(|error| {
                storage_error(
                    format!("Failed to enumerate workspaces: {error}"),
                    Some("ee doctor".to_owned()),
                )
            })?
            .into_iter()
            .map(|workspace| ShardFanoutMigrationWorkspaceInput {
                workspace_id: workspace.id,
                workspace_root: PathBuf::from(workspace.path),
            })
            .collect();
        Some(conn)
    } else {
        None
    };

    let mut plan = shard::plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
        source_database_path,
        shards_dir_override,
        workspaces,
    });

    if let Some(source) = source.as_ref() {
        annotate_workspace_row_counts(source, &mut plan)?;
    }

    Ok(plan)
}

/// Apply the shard fan-out migration described by `plan`.
///
/// The plan MUST come from [`plan_shard_fanout_migration_from_database`] or
/// [`shard::plan_shard_fanout_migration`]. Apply mode is deterministic and
/// idempotent: it preserves the source DB, creates a catalog, materializes one
/// shard DB per workspace, copies workspace-owned rows with `INSERT OR IGNORE`,
/// writes deterministic migration audit rows, verifies row counts, and records
/// target hashes in the catalog.
pub fn apply_shard_fanout_migration(
    plan: &ShardFanoutMigrationPlan,
) -> Result<ShardFanoutMigrationApplyReport, DomainError> {
    let started = Instant::now();
    if !plan.blockers.is_empty() {
        let degraded = plan
            .blockers
            .iter()
            .map(|blocker| ShardFanoutMigrationApplyDegradation {
                code: blocker.code.to_owned(),
                severity: blocker.severity,
                message: blocker.message.to_owned(),
                repair: if blocker.repair.is_empty() {
                    None
                } else {
                    Some(blocker.repair.to_owned())
                },
            })
            .chain(std::iter::once(ShardFanoutMigrationApplyDegradation {
                code: SHARD_FANOUT_APPLY_PLAN_BLOCKED_CODE.to_owned(),
                severity: "high",
                message: "Refusing to apply shard fan-out migration: planner reported blockers."
                    .to_owned(),
                repair: Some(
                    "Resolve the listed blockers and rerun `ee migrate shard-fanout --dry-run --json` before retrying."
                        .to_owned(),
                ),
            }))
            .collect::<Vec<_>>();

        return Ok(ShardFanoutMigrationApplyReport {
            schema: SHARD_FANOUT_MIGRATION_APPLY_REPORT_SCHEMA_V1,
            outcome: ShardFanoutMigrationOutcome::Blocked,
            plan_schema: plan.schema,
            elapsed_ms: elapsed_ms(started),
            source_database_path: plan.source_database_path.clone(),
            preserved_source_database_path: plan.preserved_source_database_path.clone(),
            source_database_hash: plan.source_database_hash.clone(),
            shard_root: plan.shard_root.clone(),
            catalog_path: plan.catalog_path.clone(),
            catalog_rows_written: 0,
            workspaces: plan
                .workspaces
                .iter()
                .map(ShardFanoutMigrationWorkspaceOutcome::blocked_from_plan)
                .collect(),
            preserved_source: None,
            degraded,
        });
    }

    let shard_root = plan.shard_root.as_ref().ok_or_else(|| {
        storage_error(
            "Shard fan-out migration cannot apply without a resolved shard root.".to_owned(),
            Some(
                "Rerun `ee migrate shard-fanout --dry-run --json` and resolve blockers.".to_owned(),
            ),
        )
    })?;
    let catalog_path = plan.catalog_path.as_ref().ok_or_else(|| {
        storage_error(
            "Shard fan-out migration cannot apply without a resolved catalog path.".to_owned(),
            Some(
                "Rerun `ee migrate shard-fanout --dry-run --json` and resolve blockers.".to_owned(),
            ),
        )
    })?;

    create_dir_all(shard_root)?;
    if let Some(parent) = catalog_path.parent() {
        create_dir_all(parent)?;
    }

    let preserved_source = match shard::preserve_shard_fanout_source_database(plan) {
        Ok(report) => report,
        Err(error) => {
            return Err(domain_error_for_preserve_failure(plan, &error));
        }
    };

    let source = DbConnection::open_file(&plan.source_database_path).map_err(|error| {
        storage_error(
            format!("Failed to open source database for shard fan-out copy: {error}"),
            Some(
                "Inspect the source database and rerun `ee migrate shard-fanout --dry-run --json`."
                    .to_owned(),
            ),
        )
    })?;
    let source_tables = source_user_tables(&source)?;

    let catalog =
        DbConnection::open(DatabaseConfig::file(catalog_path.clone())).map_err(|error| {
            storage_error(
                format!(
                    "Failed to open shard fan-out catalog {}: {error}",
                    catalog_path.display()
                ),
                Some("Inspect catalog directory permissions and rerun the migration.".to_owned()),
            )
        })?;
    ensure_catalog_schema(&catalog)?;

    let mut outcomes = Vec::with_capacity(plan.workspaces.len());
    for workspace in &plan.workspaces {
        outcomes.push(copy_workspace_to_shard(
            plan,
            workspace,
            &source,
            &source_tables,
            &catalog,
        )?);
    }
    catalog
        .wal_checkpoint(WalCheckpointMode::Truncate)
        .map_err(|error| {
            storage_error(format!("Failed to checkpoint shard catalog: {error}"), None)
        })?;

    let already_applied = !preserved_source.copied
        && outcomes
            .iter()
            .all(|workspace| workspace.copied_row_count == 0);
    let outcome = if already_applied {
        ShardFanoutMigrationOutcome::AlreadyApplied
    } else {
        ShardFanoutMigrationOutcome::Applied
    };

    Ok(ShardFanoutMigrationApplyReport {
        schema: SHARD_FANOUT_MIGRATION_APPLY_REPORT_SCHEMA_V1,
        outcome,
        plan_schema: plan.schema,
        elapsed_ms: elapsed_ms(started),
        source_database_path: plan.source_database_path.clone(),
        preserved_source_database_path: plan.preserved_source_database_path.clone(),
        source_database_hash: plan.source_database_hash.clone(),
        shard_root: plan.shard_root.clone(),
        catalog_path: plan.catalog_path.clone(),
        catalog_rows_written: outcomes.len(),
        workspaces: outcomes,
        preserved_source: Some(preserved_source),
        degraded: Vec::new(),
    })
}

fn domain_error_for_preserve_failure(
    plan: &ShardFanoutMigrationPlan,
    error: &ShardFanoutPreserveSourceError,
) -> DomainError {
    DomainError::Storage {
        message: format!(
            "shard fan-out migration preserve-source step failed for {}: {}",
            plan.source_database_path.display(),
            error
        ),
        repair: Some(
            "Inspect the legacy database file permissions and disk space, then rerun `ee migrate shard-fanout --dry-run --json` before retrying the apply path."
                .to_owned(),
        ),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CopyScope {
    WorkspaceRegistry,
    WorkspaceColumn,
    MemoryTags,
    RuleSourceMemories,
    RuleTags,
    PackItems,
    PackOmissions,
    MemoryLinks,
    ArtifactLinks,
    RationaleTraceLinks,
    RecorderEvents,
    Skip,
}

fn annotate_workspace_row_counts(
    source: &DbConnection,
    plan: &mut ShardFanoutMigrationPlan,
) -> Result<(), DomainError> {
    let source_tables = source_user_tables(source)?;
    for workspace in &mut plan.workspaces {
        let counts = workspace_row_counts(source, &source_tables, &workspace.workspace_id, None)?;
        workspace.planned_row_count = Some(counts.values().copied().sum());
        workspace.row_counts_by_table = counts;
    }
    Ok(())
}

fn copy_workspace_to_shard(
    plan: &ShardFanoutMigrationPlan,
    workspace: &ShardFanoutMigrationWorkspacePlan,
    _source: &DbConnection,
    source_tables: &BTreeSet<String>,
    catalog: &DbConnection,
) -> Result<ShardFanoutMigrationWorkspaceOutcome, DomainError> {
    let shard_path = workspace.shard_path.as_ref().ok_or_else(|| {
        storage_error(
            format!(
                "Shard fan-out migration cannot apply workspace {} without a target shard path.",
                workspace.workspace_id
            ),
            Some(
                "Rerun `ee migrate shard-fanout --dry-run --json` and resolve blockers.".to_owned(),
            ),
        )
    })?;
    let shard_id = workspace.shard_id.as_ref().ok_or_else(|| {
        storage_error(
            format!(
                "Shard fan-out migration cannot apply workspace {} without a shard id.",
                workspace.workspace_id
            ),
            Some(
                "Rerun `ee migrate shard-fanout --dry-run --json` and resolve blockers.".to_owned(),
            ),
        )
    })?;

    if let Some(parent) = shard_path.parent() {
        create_dir_all(parent)?;
    }

    reject_catalog_hash_mismatch(catalog, workspace, shard_path)?;

    let target = DbConnection::open(DatabaseConfig::file(shard_path.clone())).map_err(|error| {
        storage_error(
            format!(
                "Failed to open shard database {}: {error}",
                shard_path.display()
            ),
            Some("Inspect shard directory permissions and rerun the migration.".to_owned()),
        )
    })?;
    apply_schema_migrations_deterministically(&target)?;

    let target_tables = source_user_tables(&target)?;
    let before_counts = workspace_row_counts_excluding_migration_audit(
        &target,
        &target_tables,
        &workspace.workspace_id,
        None,
    )?;
    attach_source_database(&target, &plan.source_database_path)?;
    let copy_result = target.with_transaction(|| {
        for table in ordered_copy_tables(source_tables, &target_tables) {
            copy_table_for_workspace(
                &target,
                &table,
                &workspace.workspace_id,
                source_tables,
                &target_tables,
            )?;
        }
        insert_migration_audit_event(&target, plan, workspace, shard_id)?;
        Ok(())
    });
    let detach_result = target.execute_raw("DETACH DATABASE \"source\"");
    copy_result.map_err(|error| {
        storage_error(
            format!("Failed to copy workspace {} into shard: {error}", workspace.workspace_id),
            Some("Inspect the source and shard databases, then rerun `ee migrate shard-fanout --dry-run --json`.".to_owned()),
        )
    })?;
    detach_result.map_err(|error| {
        storage_error(
            format!("Failed to detach source database after shard fan-out copy: {error}"),
            Some("Rerun `ee doctor --json` before retrying the migration.".to_owned()),
        )
    })?;

    let after_counts = workspace_row_counts_excluding_migration_audit(
        &target,
        &target_tables,
        &workspace.workspace_id,
        None,
    )?;
    verify_workspace_counts(workspace, &after_counts)?;
    target
        .wal_checkpoint(WalCheckpointMode::Truncate)
        .map_err(|error| {
            storage_error(
                format!(
                    "Failed to checkpoint shard {}: {error}",
                    shard_path.display()
                ),
                None,
            )
        })?;
    let (target_hash, _) = blake3_file_hash(shard_path).map_err(|error| {
        storage_error(
            format!(
                "Failed to hash shard database {}: {error}",
                shard_path.display()
            ),
            Some("Inspect the shard database file and rerun the migration.".to_owned()),
        )
    })?;

    write_catalog_row(catalog, plan, workspace, shard_id, shard_path, &target_hash)?;

    let copied_row_counts_by_table = diff_counts(&before_counts, &after_counts)?;
    let copied_row_count = copied_row_counts_by_table.values().copied().sum();

    Ok(ShardFanoutMigrationWorkspaceOutcome {
        workspace_id: workspace.workspace_id.clone(),
        workspace_root: workspace.workspace_root.clone(),
        shard_id: workspace.shard_id.clone(),
        shard_path: workspace.shard_path.clone(),
        row_copy_status: if copied_row_count == 0 {
            "already_present"
        } else {
            "copied"
        },
        planned_row_count: workspace.planned_row_count,
        row_counts_by_table: workspace.row_counts_by_table.clone(),
        copied_row_counts_by_table,
        copied_row_count,
        source_hash: workspace.source_hash.clone(),
        target_database_hash: Some(target_hash),
        audit_event_id: Some(migration_audit_id(&workspace.workspace_id)),
        blocker_count: workspace.blockers.len(),
    })
}

fn source_user_tables(conn: &DbConnection) -> Result<BTreeSet<String>, DomainError> {
    conn.list_user_tables()
        .map(|tables| tables.into_iter().collect())
        .map_err(|error| storage_error(format!("Failed to list database tables: {error}"), None))
}

fn workspace_row_counts(
    conn: &DbConnection,
    tables: &BTreeSet<String>,
    workspace_id: &str,
    schema: Option<&str>,
) -> Result<BTreeMap<String, u64>, DomainError> {
    workspace_row_counts_with_excluded_audit_id(conn, tables, workspace_id, schema, None)
}

fn workspace_row_counts_excluding_migration_audit(
    conn: &DbConnection,
    tables: &BTreeSet<String>,
    workspace_id: &str,
    schema: Option<&str>,
) -> Result<BTreeMap<String, u64>, DomainError> {
    let excluded_audit_id = migration_audit_id(workspace_id);
    workspace_row_counts_with_excluded_audit_id(
        conn,
        tables,
        workspace_id,
        schema,
        Some(&excluded_audit_id),
    )
}

fn workspace_row_counts_with_excluded_audit_id(
    conn: &DbConnection,
    tables: &BTreeSet<String>,
    workspace_id: &str,
    schema: Option<&str>,
    excluded_audit_id: Option<&str>,
) -> Result<BTreeMap<String, u64>, DomainError> {
    let mut counts = BTreeMap::new();
    for table in ordered_count_tables(tables) {
        let source_columns = table_columns(conn, &table)?;
        let scope = copy_scope_for_table(&table, &source_columns);
        if scope == CopyScope::Skip {
            continue;
        }
        let Some(predicate) = workspace_predicate(scope, workspace_id, schema) else {
            continue;
        };
        let predicate = if table == "audit_log" {
            excluded_audit_id.map_or(predicate.clone(), |audit_id| {
                format!(
                    "({predicate}) AND row.\"id\" != {}",
                    sqlite_string_literal(audit_id)
                )
            })
        } else {
            predicate
        };
        let sql = format!(
            "SELECT COALESCE(COUNT(*), 0) FROM {} AS row WHERE {}",
            table_ref(schema, &table),
            predicate
        );
        let rows = conn.query(&sql, &[]).map_err(|error| {
            storage_error(
                format!("Failed to count {table} rows for workspace {workspace_id}: {error}"),
                None,
            )
        })?;
        let count = rows
            .first()
            .and_then(|row| row.get(0).and_then(sql_count_value_to_u64))
            .ok_or_else(|| {
                storage_error(
                    format!("Failed to read {table} row count for workspace {workspace_id}"),
                    None,
                )
            })?;
        counts.insert(table, count);
    }
    Ok(counts)
}

fn sql_count_value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_i64()
        .and_then(|count| u64::try_from(count).ok())
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        .or_else(|| {
            let count = value.as_f64()?;
            if count.is_finite() && count >= 0.0 && count.fract() == 0.0 {
                format!("{count:.0}").parse::<u64>().ok()
            } else {
                None
            }
        })
}

fn apply_schema_migrations_deterministically(conn: &DbConnection) -> Result<(), DomainError> {
    conn.execute_raw(MIGRATION_TABLE_DDL)
        .and_then(|()| conn.execute_raw(MIGRATION_TABLE_NAME_INDEX_DDL))
        .map_err(|error| {
            storage_error(
                format!("Failed to initialize shard migration table: {error}"),
                None,
            )
        })?;

    for migration in MIGRATIONS {
        let already_applied = conn.has_migration(migration.version()).map_err(|error| {
            storage_error(
                format!("Failed to inspect shard migration state: {error}"),
                None,
            )
        })?;
        if already_applied {
            continue;
        }
        let record = MigrationRecord::new(
            migration.version(),
            migration.name(),
            migration.checksum(),
            DETERMINISTIC_MIGRATION_APPLIED_AT,
        )
        .map_err(|error| {
            storage_error(format!("Failed to prepare migration record: {error}"), None)
        })?;
        conn.with_transaction(|| {
            if conn.has_migration(migration.version())? {
                return Ok(());
            }
            conn.execute_raw(migration.sql())?;
            conn.record_migration(&record)
        })
        .map_err(|error| {
            storage_error(
                format!(
                    "Failed to apply deterministic shard schema migration v{} ({}): {error}",
                    migration.version(),
                    migration.name()
                ),
                None,
            )
        })?;
    }
    Ok(())
}

fn ensure_catalog_schema(catalog: &DbConnection) -> Result<(), DomainError> {
    catalog.execute_raw(CATALOG_DDL).map_err(|error| {
        storage_error(format!("Failed to initialize shard catalog: {error}"), None)
    })
}

fn reject_catalog_hash_mismatch(
    catalog: &DbConnection,
    workspace: &ShardFanoutMigrationWorkspacePlan,
    shard_path: &Path,
) -> Result<(), DomainError> {
    if !shard_path.exists() {
        return Ok(());
    }
    let Some(recorded_hash) = catalog_target_hash(catalog, &workspace.workspace_id)? else {
        return Ok(());
    };
    let (current_hash, _) = blake3_file_hash(shard_path).map_err(|error| {
        storage_error(
            format!(
                "Failed to hash existing shard {}: {error}",
                shard_path.display()
            ),
            None,
        )
    })?;
    if recorded_hash == current_hash {
        return Ok(());
    }
    Err(storage_error(
        format!(
            "Existing shard hash mismatch for workspace {}: catalog recorded {}, current file is {}.",
            workspace.workspace_id, recorded_hash, current_hash
        ),
        Some("Move the corrupt shard aside after operator review, then rerun `ee migrate shard-fanout --dry-run --json`.".to_owned()),
    ))
}

fn catalog_target_hash(
    catalog: &DbConnection,
    workspace_id: &str,
) -> Result<Option<String>, DomainError> {
    let sql = format!(
        "SELECT target_database_hash FROM {} WHERE workspace_id = ?1 AND migration_state = 'completed'",
        sqlite_identifier(CATALOG_TABLE)
    );
    let rows = catalog
        .query(&sql, &[Value::Text(workspace_id.to_owned())])
        .map_err(|error| {
            storage_error(format!("Failed to inspect shard catalog: {error}"), None)
        })?;
    Ok(rows
        .first()
        .and_then(|row| row.get(0).and_then(|value| value.as_str()))
        .map(str::to_owned))
}

fn attach_source_database(target: &DbConnection, source_path: &Path) -> Result<(), DomainError> {
    let sql = format!(
        "ATTACH DATABASE {} AS {}",
        sqlite_string_literal(&path_string(source_path)?),
        sqlite_identifier("source")
    );
    target.execute_raw(&sql).map_err(|error| {
        storage_error(
            format!(
                "Failed to attach source database {}: {error}",
                source_path.display()
            ),
            None,
        )
    })
}

fn copy_table_for_workspace(
    target: &DbConnection,
    table: &str,
    workspace_id: &str,
    source_tables: &BTreeSet<String>,
    target_tables: &BTreeSet<String>,
) -> crate::db::Result<()> {
    if !source_tables.contains(table) || !target_tables.contains(table) {
        return Ok(());
    }
    let source_columns = table_columns_db(target, table, Some("source"))?;
    let target_columns = table_columns_db(target, table, None)?;
    let scope = copy_scope_for_table(table, &source_columns);
    if scope == CopyScope::Skip {
        return Ok(());
    }
    let Some(predicate) = workspace_predicate(scope, workspace_id, Some("source")) else {
        return Ok(());
    };
    let target_column_set = target_columns.iter().collect::<BTreeSet<_>>();
    let columns = source_columns
        .iter()
        .filter(|column| target_column_set.contains(column))
        .cloned()
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Ok(());
    }
    let insert_columns = columns
        .iter()
        .map(|column| sqlite_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let select_columns = columns
        .iter()
        .map(|column| format!("row.{}", sqlite_identifier(column)))
        .collect::<Vec<_>>()
        .join(", ");
    let order_by = columns
        .iter()
        .map(|column| format!("row.{}", sqlite_identifier(column)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT OR IGNORE INTO {} ({}) SELECT {} FROM {} AS row WHERE {} ORDER BY {}",
        sqlite_identifier(table),
        insert_columns,
        select_columns,
        table_ref(Some("source"), table),
        predicate,
        order_by
    );
    target.execute_raw(&sql)
}

fn insert_migration_audit_event(
    target: &DbConnection,
    plan: &ShardFanoutMigrationPlan,
    workspace: &ShardFanoutMigrationWorkspacePlan,
    shard_id: &str,
) -> crate::db::Result<()> {
    let details = serde_json::json!({
        "schema": SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1,
        "workspace_id": workspace.workspace_id.clone(),
        "workspace_path": workspace.workspace_root.display().to_string(),
        "shard_id": shard_id,
        "catalog_path": plan.catalog_path.as_ref().map(|path| path.display().to_string()),
        "source_db_path": plan.source_database_path.display().to_string(),
        "target_db_path": workspace.shard_path.as_ref().map(|path| path.display().to_string()),
        "preserved_source_path": plan.preserved_source_database_path.display().to_string(),
        "source_db_hash": plan.source_database_hash.clone(),
        "target_db_hash": serde_json::Value::Null,
        "migration_phase": "audit_written",
        "dry_run": false,
        "actor": MIGRATION_ACTOR,
        "elapsed_ms": 0,
        "row_counts": workspace.row_counts_by_table.clone(),
        "rollback_available": true,
        "degraded_codes": [],
        "recovery": [],
        "message": "Shard fan-out migration copied workspace rows and preserved source database."
    });
    let sql = format!(
        "INSERT OR IGNORE INTO audit_log (id, workspace_id, timestamp, actor, action, target_type, target_id, details, surface, mutation_kind, before_hash, after_hash, prev_row_hash, this_row_hash) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, NULL, NULL, NULL, NULL)",
        sqlite_string_literal(&migration_audit_id(&workspace.workspace_id)),
        sqlite_string_literal(&workspace.workspace_id),
        sqlite_string_literal(MIGRATION_AUDIT_TIMESTAMP),
        sqlite_string_literal(MIGRATION_ACTOR),
        sqlite_string_literal(MIGRATION_AUDIT_ACTION),
        sqlite_string_literal("workspace"),
        sqlite_string_literal(&workspace.workspace_id),
        sqlite_string_literal(&details.to_string()),
        sqlite_string_literal("shard_fanout"),
        sqlite_string_literal("migration")
    );
    target.execute_raw(&sql)
}

fn write_catalog_row(
    catalog: &DbConnection,
    plan: &ShardFanoutMigrationPlan,
    workspace: &ShardFanoutMigrationWorkspacePlan,
    shard_id: &str,
    shard_path: &Path,
    target_hash: &str,
) -> Result<(), DomainError> {
    let workspace_registry_mirror = serde_json::json!({
        "workspaceId": workspace.workspace_id.clone(),
        "workspaceRoot": workspace.workspace_root.display().to_string(),
    });
    let verified_hashes = serde_json::json!({
        "sourceDatabaseHash": plan.source_database_hash.clone(),
        "targetDatabaseHash": target_hash,
        "preservedSourceHash": plan.source_database_hash.clone(),
    });
    let sql = format!(
        "INSERT INTO {} (workspace_id, workspace_registry_mirror, shard_id, shard_path, catalog_schema_version, shard_generation, migration_state, last_verified_hashes, rollback_available, preserved_source_path, source_database_path, source_database_hash, target_database_hash, migrated_at) VALUES ({}, {}, {}, {}, {}, 1, 'completed', {}, 1, {}, {}, {}, {}, {}) ON CONFLICT(workspace_id) DO UPDATE SET workspace_registry_mirror = excluded.workspace_registry_mirror, shard_id = excluded.shard_id, shard_path = excluded.shard_path, catalog_schema_version = excluded.catalog_schema_version, shard_generation = excluded.shard_generation, migration_state = excluded.migration_state, last_verified_hashes = excluded.last_verified_hashes, rollback_available = excluded.rollback_available, preserved_source_path = excluded.preserved_source_path, source_database_path = excluded.source_database_path, source_database_hash = excluded.source_database_hash, target_database_hash = excluded.target_database_hash, migrated_at = excluded.migrated_at",
        sqlite_identifier(CATALOG_TABLE),
        sqlite_string_literal(&workspace.workspace_id),
        sqlite_string_literal(&workspace_registry_mirror.to_string()),
        sqlite_string_literal(shard_id),
        sqlite_string_literal(&path_string(shard_path)?),
        SHARD_FANOUT_CATALOG_SCHEMA_VERSION,
        sqlite_string_literal(&verified_hashes.to_string()),
        sqlite_string_literal(&path_string(&plan.preserved_source_database_path)?),
        sqlite_string_literal(&path_string(&plan.source_database_path)?),
        optional_sqlite_string_literal(plan.source_database_hash.as_deref()),
        sqlite_string_literal(target_hash),
        sqlite_string_literal(MIGRATION_AUDIT_TIMESTAMP)
    );
    catalog.execute_raw(&sql).map_err(|error| {
        storage_error(
            format!(
                "Failed to write shard catalog row for workspace {}: {error}",
                workspace.workspace_id
            ),
            None,
        )
    })
}

fn verify_workspace_counts(
    workspace: &ShardFanoutMigrationWorkspacePlan,
    actual: &BTreeMap<String, u64>,
) -> Result<(), DomainError> {
    for (table, expected) in &workspace.row_counts_by_table {
        let actual_count = actual.get(table).copied().unwrap_or_default();
        if actual_count != *expected {
            return Err(storage_error(
                format!(
                    "Shard row-count verification failed for workspace {} table {}: expected {}, found {}.",
                    workspace.workspace_id, table, expected, actual_count
                ),
                Some("Inspect the source and shard databases, then rerun `ee migrate shard-fanout --dry-run --json`.".to_owned()),
            ));
        }
    }
    Ok(())
}

fn diff_counts(
    before: &BTreeMap<String, u64>,
    after: &BTreeMap<String, u64>,
) -> Result<BTreeMap<String, u64>, DomainError> {
    let mut copied = BTreeMap::new();
    for (table, after_count) in after {
        let before_count = before.get(table).copied().unwrap_or_default();
        let copied_count = after_count.checked_sub(before_count).ok_or_else(|| {
            storage_error(
                format!(
                    "Shard row-count regression for table {table}: before {before_count}, after {after_count}."
                ),
                Some(
                    "Inspect the shard database for concurrent mutation or corruption before retrying `ee migrate shard-fanout --json`."
                        .to_owned(),
                ),
            )
        })?;
        copied.insert(table.clone(), copied_count);
    }
    Ok(copied)
}

fn ordered_count_tables(tables: &BTreeSet<String>) -> Vec<String> {
    ordered_copy_tables(tables, tables)
}

fn ordered_copy_tables(
    source_tables: &BTreeSet<String>,
    target_tables: &BTreeSet<String>,
) -> Vec<String> {
    const KNOWN_ORDER: &[&str] = &[
        "workspaces",
        "agents",
        "memories",
        "audit_log",
        "memory_tags",
        "curation_candidates",
        "procedural_rules",
        "rule_source_memories",
        "rule_tags",
        "search_index_jobs",
        "pack_records",
        "pack_items",
        "pack_omissions",
        "memory_links",
        "sessions",
        "evidence_spans",
        "import_ledger",
        "feedback_events",
        "task_episodes",
        "model_registry",
        "graph_snapshots",
        "graph_algorithm_witnesses",
        "graph_algorithm_results",
        "agent_installations",
        "agent_history_sources",
        "artifacts",
        "artifact_links",
        "feedback_quarantine",
        "tripwires",
        "tripwire_check_events",
        "rationale_traces",
        "rationale_trace_links",
        "recorder_runs",
        "recorder_events",
        "certificates",
        "trust_quarantine",
        "learning_observations",
        "procedures",
        "procedure_events",
        "plan_recipes",
        "causal_evidence",
        "ee_wal_holds",
        "preflight_bypass_tokens",
        "agent_context_profiles",
        "mesh_peers",
        "mesh_peer_cursors",
        "mesh_import_ledger",
        "mesh_memory_mappings",
        "mesh_body_cache_metadata",
    ];
    let mut ordered = Vec::new();
    let mut seen = BTreeSet::new();
    for table in KNOWN_ORDER {
        if source_tables.contains(*table) && target_tables.contains(*table) {
            ordered.push((*table).to_owned());
            seen.insert((*table).to_owned());
        }
    }
    for table in source_tables.intersection(target_tables) {
        if !seen.contains(table) && table != "ee_schema_migrations" && table != "ee_advisory_locks"
        {
            ordered.push(table.clone());
        }
    }
    ordered
}

fn copy_scope_for_table(table: &str, columns: &[String]) -> CopyScope {
    match table {
        "ee_schema_migrations" | "ee_advisory_locks" => CopyScope::Skip,
        "workspaces" => CopyScope::WorkspaceRegistry,
        "memory_tags" => CopyScope::MemoryTags,
        "rule_source_memories" => CopyScope::RuleSourceMemories,
        "rule_tags" => CopyScope::RuleTags,
        "pack_items" => CopyScope::PackItems,
        "pack_omissions" => CopyScope::PackOmissions,
        "memory_links" => CopyScope::MemoryLinks,
        "artifact_links" => CopyScope::ArtifactLinks,
        "rationale_trace_links" => CopyScope::RationaleTraceLinks,
        "recorder_events" => CopyScope::RecorderEvents,
        _ if columns.iter().any(|column| column == "workspace_id") => CopyScope::WorkspaceColumn,
        _ => CopyScope::Skip,
    }
}

fn workspace_predicate(
    scope: CopyScope,
    workspace_id: &str,
    schema: Option<&str>,
) -> Option<String> {
    let workspace = sqlite_string_literal(workspace_id);
    let memories = table_ref(schema, "memories");
    let rules = table_ref(schema, "procedural_rules");
    let packs = table_ref(schema, "pack_records");
    let artifacts = table_ref(schema, "artifacts");
    let traces = table_ref(schema, "rationale_traces");
    let recorder_runs = table_ref(schema, "recorder_runs");
    match scope {
        CopyScope::WorkspaceRegistry => Some(format!("row.\"id\" = {workspace}")),
        CopyScope::WorkspaceColumn => Some(format!("row.\"workspace_id\" = {workspace}")),
        CopyScope::MemoryTags => Some(format!(
            "EXISTS (SELECT 1 FROM {memories} AS m WHERE m.\"id\" = row.\"memory_id\" AND m.\"workspace_id\" = {workspace})"
        )),
        CopyScope::RuleSourceMemories => Some(format!(
            "EXISTS (SELECT 1 FROM {rules} AS r WHERE r.\"id\" = row.\"rule_id\" AND r.\"workspace_id\" = {workspace}) AND EXISTS (SELECT 1 FROM {memories} AS m WHERE m.\"id\" = row.\"memory_id\" AND m.\"workspace_id\" = {workspace})"
        )),
        CopyScope::RuleTags => Some(format!(
            "EXISTS (SELECT 1 FROM {rules} AS r WHERE r.\"id\" = row.\"rule_id\" AND r.\"workspace_id\" = {workspace})"
        )),
        CopyScope::PackItems | CopyScope::PackOmissions => Some(format!(
            "EXISTS (SELECT 1 FROM {packs} AS p WHERE p.\"id\" = row.\"pack_id\" AND p.\"workspace_id\" = {workspace}) AND EXISTS (SELECT 1 FROM {memories} AS m WHERE m.\"id\" = row.\"memory_id\" AND m.\"workspace_id\" = {workspace})"
        )),
        CopyScope::MemoryLinks => Some(format!(
            "EXISTS (SELECT 1 FROM {memories} AS src WHERE src.\"id\" = row.\"src_memory_id\" AND src.\"workspace_id\" = {workspace}) AND EXISTS (SELECT 1 FROM {memories} AS dst WHERE dst.\"id\" = row.\"dst_memory_id\" AND dst.\"workspace_id\" = {workspace})"
        )),
        CopyScope::ArtifactLinks => Some(format!(
            "EXISTS (SELECT 1 FROM {artifacts} AS a WHERE a.\"id\" = row.\"artifact_id\" AND a.\"workspace_id\" = {workspace})"
        )),
        CopyScope::RationaleTraceLinks => Some(format!(
            "EXISTS (SELECT 1 FROM {traces} AS t WHERE t.\"trace_id\" = row.\"trace_id\" AND t.\"workspace_id\" = {workspace})"
        )),
        CopyScope::RecorderEvents => Some(format!(
            "EXISTS (SELECT 1 FROM {recorder_runs} AS r WHERE r.\"run_id\" = row.\"run_id\" AND r.\"workspace_id\" = {workspace})"
        )),
        CopyScope::Skip => None,
    }
}

fn table_columns(conn: &DbConnection, table: &str) -> Result<Vec<String>, DomainError> {
    table_columns_db(conn, table, None)
        .map_err(|error| storage_error(format!("Failed to inspect table {table}: {error}"), None))
}

fn table_columns_db(
    conn: &DbConnection,
    table: &str,
    schema: Option<&str>,
) -> crate::db::Result<Vec<String>> {
    let sql = match schema {
        Some(schema) => format!(
            "PRAGMA {}.table_info({})",
            sqlite_identifier(schema),
            sqlite_string_literal(table)
        ),
        None => format!("PRAGMA table_info({})", sqlite_string_literal(table)),
    };
    let rows = conn.query(&sql, &[])?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            row.get(1)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect())
}

fn table_ref(schema: Option<&str>, table: &str) -> String {
    match schema {
        Some(schema) => format!("{}.{}", sqlite_identifier(schema), sqlite_identifier(table)),
        None => sqlite_identifier(table),
    }
}

fn sqlite_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sqlite_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn optional_sqlite_string_literal(value: Option<&str>) -> String {
    value.map_or_else(|| "NULL".to_owned(), sqlite_string_literal)
}

fn path_string(path: &Path) -> Result<String, DomainError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        storage_error(
            format!("Path is not valid UTF-8: {}", path.display()),
            Some("Use a UTF-8 database path for shard fan-out migration.".to_owned()),
        )
    })
}

fn create_dir_all(path: &Path) -> Result<(), DomainError> {
    fs::create_dir_all(path).map_err(|error| {
        storage_error(
            format!("Failed to create directory {}: {error}", path.display()),
            Some("Inspect directory permissions and available disk space.".to_owned()),
        )
    })
}

fn blake3_file_hash(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = checked_add_file_hash_bytes(bytes, read)?;
    }
    Ok((format!("blake3:{}", hasher.finalize().to_hex()), bytes))
}

fn checked_add_file_hash_bytes(total: u64, read: usize) -> io::Result<u64> {
    let read_len = u64::try_from(read).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file hash read length {read} does not fit u64"),
        )
    })?;
    total.checked_add(read_len).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "file hash byte count exceeds u64",
        )
    })
}

fn migration_audit_id(workspace_id: &str) -> String {
    let hash = blake3::hash(format!("bd-f6jfs.4:{workspace_id}:audit_written").as_bytes());
    format!("audit_{}", &hash.to_hex()[..26])
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn storage_error(message: String, repair: Option<String>) -> DomainError {
    DomainError::Storage { message, repair }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbConnection;
    use crate::db::shard::{
        SHARD_FANOUT_CATALOG_SCHEMA_VERSION, SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1,
        SHARD_FANOUT_MIGRATION_PLAN_SCHEMA_V1, ShardFanoutDegradation,
        ShardFanoutMigrationWorkspacePlan,
    };

    type TestResult = Result<(), String>;

    const WSP_A: &str = "wsp_aaaaaaaaaaaaaaaaaaaaaaaaaa";
    const WSP_B: &str = "wsp_bbbbbbbbbbbbbbbbbbbbbbbbbb";
    const MEM_A: &str = "mem_aaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MEM_B: &str = "mem_bbbbbbbbbbbbbbbbbbbbbbbbbb";
    const PACK_A: &str = "pack_aaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PACK_B: &str = "pack_bbbbbbbbbbbbbbbbbbbbbbbbbb";
    const HASH_A: &str = "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn file_hash_byte_count_overflow_is_rejected() {
        assert_eq!(checked_add_file_hash_bytes(41, 1).unwrap(), 42);
        let error = checked_add_file_hash_bytes(u64::MAX, 1)
            .expect_err("overflowing file byte count must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeds u64"));
    }

    #[test]
    fn diff_counts_rejects_decreasing_table_counts() -> TestResult {
        let before = BTreeMap::from([("memories".to_string(), 2), ("pack_items".to_string(), 4)]);
        let after = BTreeMap::from([("memories".to_string(), 5), ("pack_items".to_string(), 4)]);
        let copied = diff_counts(&before, &after).map_err(|error| error.to_string())?;
        assert_eq!(copied.get("memories"), Some(&3));
        assert_eq!(copied.get("pack_items"), Some(&0));

        let regressed = BTreeMap::from([("memories".to_string(), 1)]);
        let error =
            diff_counts(&before, &regressed).expect_err("decreasing table counts must fail closed");
        assert!(error.to_string().contains("row-count regression"));
        assert!(error.to_string().contains("memories"));
        Ok(())
    }

    #[test]
    fn workspace_counts_exclude_deterministic_migration_audit_row() -> TestResult {
        let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;
        conn.execute_raw(
            "CREATE TABLE audit_log (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL
            )",
        )
        .map_err(|error| error.to_string())?;
        let migration_audit_id = migration_audit_id(WSP_A);
        conn.execute_raw(&format!(
            "INSERT INTO audit_log (id, workspace_id) VALUES \
             ('audit_source_event_aaaaaaaaaaaa', '{WSP_A}'), \
             ({}, '{WSP_A}'), \
             ('audit_other_workspace_bbbbbbbbbb', '{WSP_B}')",
            sqlite_string_literal(&migration_audit_id)
        ))
        .map_err(|error| error.to_string())?;
        let tables = BTreeSet::from(["audit_log".to_owned()]);

        let raw_counts =
            workspace_row_counts(&conn, &tables, WSP_A, None).map_err(|error| error.to_string())?;
        assert_eq!(raw_counts.get("audit_log"), Some(&2));

        let filtered_counts =
            workspace_row_counts_excluding_migration_audit(&conn, &tables, WSP_A, None)
                .map_err(|error| error.to_string())?;
        assert_eq!(filtered_counts.get("audit_log"), Some(&1));

        let workspace = ShardFanoutMigrationWorkspacePlan {
            workspace_id: WSP_A.to_owned(),
            workspace_root: PathBuf::from("/tmp/workspace-a"),
            shard_id: Some("shard_a".to_owned()),
            shard_path: Some(PathBuf::from("/tmp/shard-a.db")),
            source_database_path: PathBuf::from("/tmp/source.db"),
            planned_row_count: Some(1),
            row_counts_by_table: BTreeMap::from([("audit_log".to_owned(), 1)]),
            source_hash: None,
            expected_audit_rows: Vec::new(),
            blockers: Vec::new(),
        };
        verify_workspace_counts(&workspace, &filtered_counts).map_err(|error| error.to_string())
    }

    fn blocking_plan() -> ShardFanoutMigrationPlan {
        ShardFanoutMigrationPlan {
            schema: SHARD_FANOUT_MIGRATION_PLAN_SCHEMA_V1,
            dry_run: false,
            source_database_path: PathBuf::from("/tmp/ee-test-source.db"),
            preserved_source_database_path: PathBuf::from(
                "/tmp/ee-test-source.pre-shard-fanout.db",
            ),
            source_database_hash: None,
            shard_root: None,
            catalog_path: None,
            catalog_schema_version: SHARD_FANOUT_CATALOG_SCHEMA_VERSION,
            workspaces: Vec::new(),
            expected_audit_rows: vec![shard::ShardFanoutMigrationAuditRowPlan {
                schema: SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1,
                event: "preserve_legacy_database",
                workspace_id: None,
                source_path: PathBuf::from("/tmp/ee-test-source.db"),
                target_path: PathBuf::from("/tmp/ee-test-source.pre-shard-fanout.db"),
            }],
            blockers: vec![ShardFanoutDegradation {
                code: "shards_dir_unresolved",
                severity: "warning",
                message: "shards directory could not be resolved from configuration",
                repair: "set EE_SHARDS_DIR or pass --shards-dir",
            }],
        }
    }

    #[test]
    fn apply_refuses_to_advance_when_plan_blockers_present() {
        let plan = blocking_plan();
        let report =
            apply_shard_fanout_migration(&plan).expect("apply should succeed structurally");

        assert_eq!(report.schema, SHARD_FANOUT_MIGRATION_APPLY_REPORT_SCHEMA_V1);
        assert_eq!(report.outcome, ShardFanoutMigrationOutcome::Blocked);
        assert!(report.preserved_source.is_none());
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "shards_dir_unresolved")
        );
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == SHARD_FANOUT_APPLY_PLAN_BLOCKED_CODE)
        );
        assert!(
            !report.row_copy_unimplemented(),
            "blocked outcome must not emit the row-copy-unimplemented code: that only \
             applies after source preservation actually ran"
        );
    }

    #[test]
    fn shard_fanout_migration_outcome_serializes_to_stable_snake_case() {
        assert_eq!(ShardFanoutMigrationOutcome::Applied.as_str(), "applied");
        assert_eq!(
            ShardFanoutMigrationOutcome::AlreadyApplied.as_str(),
            "already_applied"
        );
        assert_eq!(ShardFanoutMigrationOutcome::Blocked.as_str(), "blocked");

        let json = serde_json::to_value(ShardFanoutMigrationOutcome::Applied).expect("serialize");
        assert_eq!(json, serde_json::json!("applied"));
    }

    #[test]
    fn apply_partitions_workspace_rows_and_is_idempotent() -> TestResult {
        let temp = tempfile::Builder::new()
            .prefix("ee-shard-fanout-apply")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let source_path = temp.path().join("workspace/.ee/ee.db");
        let shard_root = temp.path().join("data/shards");
        std::fs::create_dir_all(
            source_path
                .parent()
                .ok_or_else(|| "source path should have parent".to_owned())?,
        )
        .map_err(|error| error.to_string())?;

        let source = DbConnection::open_file(&source_path).map_err(|error| error.to_string())?;
        apply_schema_migrations_deterministically(&source).map_err(|error| error.to_string())?;
        seed_two_workspace_source(&source)?;
        drop(source);

        let plan = plan_shard_fanout_migration_from_database(
            source_path.clone(),
            Some(shard_root.clone()),
        )
        .map_err(|error| error.to_string())?;
        let workspace_a = plan
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == WSP_A)
            .ok_or_else(|| "workspace A should be planned".to_owned())?;
        assert_eq!(workspace_a.row_counts_by_table.get("memories"), Some(&1));
        assert_eq!(
            workspace_a.row_counts_by_table.get("pack_records"),
            Some(&1)
        );
        assert_eq!(
            workspace_a
                .row_counts_by_table
                .get("agent_context_profiles"),
            Some(&1)
        );

        let first = apply_shard_fanout_migration(&plan).map_err(|error| error.to_string())?;
        assert_eq!(first.outcome, ShardFanoutMigrationOutcome::Applied);
        assert_eq!(first.catalog_rows_written, 2);
        assert!(
            first
                .preserved_source
                .as_ref()
                .is_some_and(|report| report.rollback_ready)
        );

        let shard_a = DbConnection::open_file(shard_root.join(format!("{WSP_A}.db")))
            .map_err(|error| error.to_string())?;
        let shard_b = DbConnection::open_file(shard_root.join(format!("{WSP_B}.db")))
            .map_err(|error| error.to_string())?;
        assert_eq!(
            scalar_count(&shard_a, "SELECT COUNT(*) FROM workspaces")?,
            1
        );
        assert_eq!(scalar_count(&shard_a, "SELECT COUNT(*) FROM memories")?, 1);
        assert_eq!(
            scalar_count(
                &shard_a,
                "SELECT COUNT(*) FROM memories WHERE workspace_id = 'wsp_bbbbbbbbbbbbbbbbbbbbbbbbbb'"
            )?,
            0
        );
        assert_eq!(
            scalar_count(&shard_a, "SELECT COUNT(*) FROM pack_items")?,
            1
        );
        assert_eq!(
            scalar_count(&shard_a, "SELECT COUNT(*) FROM graph_snapshots")?,
            1
        );
        assert_eq!(
            scalar_count(&shard_a, "SELECT COUNT(*) FROM agent_context_profiles")?,
            1
        );
        assert_eq!(scalar_count(&shard_b, "SELECT COUNT(*) FROM memories")?, 1);
        drop(shard_a);
        drop(shard_b);

        let second = apply_shard_fanout_migration(&plan).map_err(|error| error.to_string())?;
        assert_eq!(second.outcome, ShardFanoutMigrationOutcome::AlreadyApplied);
        assert!(
            second
                .workspaces
                .iter()
                .all(|workspace| workspace.copied_row_count == 0)
        );

        Ok(())
    }

    fn seed_two_workspace_source(conn: &DbConnection) -> TestResult {
        conn.execute_raw(&format!(
            "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES \
             ('{WSP_A}', '/tmp/workspace-a', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'), \
             ('{WSP_B}', '/tmp/workspace-b', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')"
        ))
        .map_err(|error| error.to_string())?;
        conn.execute_raw(&format!(
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, created_at, updated_at) VALUES \
             ('{MEM_A}', '{WSP_A}', 'procedural', 'rule', 'alpha rule', 0.9, 0.8, 0.7, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'), \
             ('{MEM_B}', '{WSP_B}', 'semantic', 'fact', 'beta fact', 0.9, 0.8, 0.7, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')"
        ))
        .map_err(|error| error.to_string())?;
        conn.execute_raw(&format!(
            "INSERT INTO audit_log (id, workspace_id, timestamp, actor, action, target_type, target_id, details) VALUES \
             ('audit_aaaaaaaaaaaaaaaaaaaaaaaaaa', '{WSP_A}', '2026-01-01T00:00:00Z', 'tester', 'memory.create', 'memory', '{MEM_A}', '{{\"ok\":true}}'), \
             ('audit_bbbbbbbbbbbbbbbbbbbbbbbbbb', '{WSP_B}', '2026-01-01T00:00:00Z', 'tester', 'memory.create', 'memory', '{MEM_B}', '{{\"ok\":true}}')"
        ))
        .map_err(|error| error.to_string())?;
        conn.execute_raw(&format!(
            "INSERT INTO pack_records (id, workspace_id, query, profile, max_tokens, used_tokens, item_count, omitted_count, pack_hash, created_at) VALUES \
             ('{PACK_A}', '{WSP_A}', 'alpha', 'compact', 100, 10, 1, 0, '{HASH_A}', '2026-01-01T00:00:00Z'), \
             ('{PACK_B}', '{WSP_B}', 'beta', 'compact', 100, 10, 1, 0, '{HASH_B}', '2026-01-01T00:00:00Z')"
        ))
        .map_err(|error| error.to_string())?;
        conn.execute_raw(&format!(
            "INSERT INTO pack_items (pack_id, memory_id, rank, section, estimated_tokens, relevance, utility, why) VALUES \
             ('{PACK_A}', '{MEM_A}', 1, 'procedural_rules', 5, 0.9, 0.8, 'alpha why'), \
             ('{PACK_B}', '{MEM_B}', 1, 'decisions', 5, 0.9, 0.8, 'beta why')"
        ))
        .map_err(|error| error.to_string())?;
        conn.execute_raw(&format!(
            "INSERT INTO graph_snapshots (id, workspace_id, snapshot_version, schema_version, graph_type, node_count, edge_count, metrics_json, content_hash, source_generation, created_at) VALUES \
             ('gsnap_aaaaaaaaaaaaaaaaaaaaaaaaa', '{WSP_A}', 1, 'ee.graph.snapshot.v1', 'memory_links', 1, 0, '{{}}', '{HASH_A}', 1, '2026-01-01T00:00:00Z'), \
             ('gsnap_bbbbbbbbbbbbbbbbbbbbbbbbb', '{WSP_B}', 1, 'ee.graph.snapshot.v1', 'memory_links', 1, 0, '{{}}', '{HASH_B}', 1, '2026-01-01T00:00:00Z')"
        ))
        .map_err(|error| error.to_string())?;
        conn.execute_raw(&format!(
            "INSERT INTO agent_context_profiles (workspace_id, agent_name, memory_id, last_seen_at) VALUES \
             ('{WSP_A}', 'tester', '{MEM_A}', '2026-01-01T00:00:00Z'), \
             ('{WSP_B}', 'tester', '{MEM_B}', '2026-01-01T00:00:00Z')"
        ))
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn scalar_count(conn: &DbConnection, sql: &str) -> Result<i64, String> {
        let rows = conn.query(sql, &[]).map_err(|error| error.to_string())?;
        rows.first()
            .and_then(|row| row.get(0).and_then(|value| value.as_i64()))
            .ok_or_else(|| format!("query returned no count: {sql}"))
    }
}
