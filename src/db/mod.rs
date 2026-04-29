use chrono::Utc;
use sqlmodel_core::{IsolationLevel, Row, Value};
use sqlmodel_frankensqlite::FrankenConnection;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

pub const SUBSYSTEM: &str = "db";
pub const MIGRATION_TABLE_NAME: &str = "ee_schema_migrations";

const MIGRATION_TABLE_DDL: &str = "CREATE TABLE IF NOT EXISTS ee_schema_migrations (
    version INTEGER PRIMARY KEY CHECK (version > 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    checksum TEXT NOT NULL CHECK (length(trim(checksum)) > 0),
    applied_at TEXT NOT NULL CHECK (length(trim(applied_at)) > 0)
)";
const MIGRATION_TABLE_NAME_INDEX_DDL: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_ee_schema_migrations_name ON ee_schema_migrations(name)";

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseLocation {
    Memory,
    File(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseOpenMode {
    ReadWrite,
    SchemaOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    location: DatabaseLocation,
    mode: DatabaseOpenMode,
}

impl DatabaseConfig {
    pub fn memory() -> Self {
        Self {
            location: DatabaseLocation::Memory,
            mode: DatabaseOpenMode::ReadWrite,
        }
    }

    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            location: DatabaseLocation::File(path.into()),
            mode: DatabaseOpenMode::ReadWrite,
        }
    }

    pub fn schema_only(path: impl Into<PathBuf>) -> Self {
        Self {
            location: DatabaseLocation::File(path.into()),
            mode: DatabaseOpenMode::SchemaOnly,
        }
    }

    pub const fn location(&self) -> &DatabaseLocation {
        &self.location
    }

    pub const fn mode(&self) -> DatabaseOpenMode {
        self.mode
    }
}

pub struct DbConnection {
    inner: FrankenConnection,
    location: DatabaseLocation,
    mode: DatabaseOpenMode,
}

impl DbConnection {
    pub fn open(config: DatabaseConfig) -> Result<Self> {
        let inner = match (&config.location, config.mode) {
            (DatabaseLocation::Memory, DatabaseOpenMode::ReadWrite) => {
                FrankenConnection::open_memory()
                    .map_err(|source| DbError::sqlmodel(DbOperation::OpenMemory, source))?
            }
            (DatabaseLocation::Memory, DatabaseOpenMode::SchemaOnly) => {
                return Err(DbError::InvalidMode {
                    location: config.location,
                    mode: config.mode,
                    message: "schema-only mode requires a file database".to_string(),
                });
            }
            (DatabaseLocation::File(path), DatabaseOpenMode::ReadWrite) => {
                let path = database_path_string(path, DbOperation::OpenReadWrite)?;
                FrankenConnection::open_file(path)
                    .map_err(|source| DbError::sqlmodel(DbOperation::OpenReadWrite, source))?
            }
            (DatabaseLocation::File(path), DatabaseOpenMode::SchemaOnly) => {
                let path = database_path_string(path, DbOperation::OpenSchemaOnly)?;
                FrankenConnection::open_schema_only(path)
                    .map_err(|source| DbError::sqlmodel(DbOperation::OpenSchemaOnly, source))?
            }
        };

        Ok(Self {
            inner,
            location: config.location,
            mode: config.mode,
        })
    }

    pub fn open_memory() -> Result<Self> {
        Self::open(DatabaseConfig::memory())
    }

    pub fn open_file(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open(DatabaseConfig::file(path))
    }

    pub fn open_schema_only(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open(DatabaseConfig::schema_only(path))
    }

    pub fn path(&self) -> &str {
        self.inner.path()
    }

    pub const fn location(&self) -> &DatabaseLocation {
        &self.location
    }

    pub const fn mode(&self) -> DatabaseOpenMode {
        self.mode
    }

    pub fn ping(&self) -> Result<()> {
        self.query("SELECT 1", &[]).map(|_| ())
    }

    pub fn close(self) -> Result<()> {
        self.inner
            .close_sync()
            .map_err(|source| DbError::sqlmodel(DbOperation::Close, source))
    }

    /// Begin a transaction with the specified isolation level.
    /// For SQLite, uses DEFERRED (default), IMMEDIATE, or EXCLUSIVE.
    pub fn begin_transaction(&self, isolation: IsolationLevel) -> Result<()> {
        let sql = match isolation {
            IsolationLevel::ReadUncommitted | IsolationLevel::ReadCommitted => "BEGIN DEFERRED",
            IsolationLevel::RepeatableRead => "BEGIN IMMEDIATE",
            IsolationLevel::Serializable => "BEGIN EXCLUSIVE",
        };
        self.execute_raw_for(DbOperation::BeginTransaction, sql)
    }

    /// Begin a transaction with the default isolation level (DEFERRED).
    pub fn begin(&self) -> Result<()> {
        self.execute_raw_for(DbOperation::BeginTransaction, "BEGIN DEFERRED")
    }

    /// Commit the current transaction.
    pub fn commit(&self) -> Result<()> {
        self.execute_raw_for(DbOperation::CommitTransaction, "COMMIT")
    }

    /// Rollback the current transaction.
    pub fn rollback(&self) -> Result<()> {
        self.execute_raw_for(DbOperation::RollbackTransaction, "ROLLBACK")
    }

    /// Execute a closure within a transaction.
    /// Commits on success, rolls back on error.
    pub fn with_transaction<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        self.begin()?;
        match f() {
            Ok(result) => {
                self.commit()?;
                Ok(result)
            }
            Err(err) => {
                let _ = self.rollback();
                Err(err)
            }
        }
    }

    pub fn execute_raw(&self, sql: &str) -> Result<()> {
        self.inner
            .execute_raw(sql)
            .map_err(|source| DbError::sqlmodel(DbOperation::Execute, source))
    }

    pub fn ensure_migration_table(&self) -> Result<()> {
        self.execute_raw_for(DbOperation::EnsureMigrationTable, MIGRATION_TABLE_DDL)?;
        self.execute_raw_for(
            DbOperation::EnsureMigrationTable,
            MIGRATION_TABLE_NAME_INDEX_DDL,
        )
    }

    pub fn migration_table_exists(&self) -> Result<bool> {
        let rows = self.query_for(
            DbOperation::InspectMigrationTable,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            &[Value::Text(MIGRATION_TABLE_NAME.to_string())],
        )?;

        Ok(!rows.is_empty())
    }

    pub fn migration_table_columns(&self) -> Result<Vec<MigrationTableColumn>> {
        let rows = self.query_for(
            DbOperation::InspectMigrationTable,
            "PRAGMA table_info(ee_schema_migrations)",
            &[],
        )?;

        rows.iter()
            .map(MigrationTableColumn::from_pragma_row)
            .collect()
    }

    pub fn record_migration(&self, migration: &MigrationRecord) -> Result<()> {
        migration.validate()?;

        self.execute_for(
            DbOperation::RecordMigration,
            "INSERT INTO ee_schema_migrations (version, name, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
            &[
                Value::BigInt(i64::from(migration.version)),
                Value::Text(migration.name.clone()),
                Value::Text(migration.checksum.clone()),
                Value::Text(migration.applied_at.clone()),
            ],
        )
        .map(|_| ())
    }

    pub fn applied_migrations(&self) -> Result<Vec<MigrationRecord>> {
        let rows = self.query_for(
            DbOperation::ListMigrations,
            "SELECT version, name, checksum, applied_at FROM ee_schema_migrations ORDER BY version ASC",
            &[],
        )?;

        rows.iter().map(MigrationRecord::from_row).collect()
    }

    pub fn has_migration(&self, version: u32) -> Result<bool> {
        validate_migration_version(version)?;

        let rows = self.query_for(
            DbOperation::CheckMigration,
            "SELECT 1 FROM ee_schema_migrations WHERE version = ?1 LIMIT 1",
            &[Value::BigInt(i64::from(version))],
        )?;

        Ok(!rows.is_empty())
    }

    pub(crate) fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        self.query_for(DbOperation::Query, sql, params)
    }

    fn execute_raw_for(&self, operation: DbOperation, sql: &str) -> Result<()> {
        self.inner
            .execute_raw(sql)
            .map_err(|source| DbError::sqlmodel(operation, source))
    }

    fn execute_for(&self, operation: DbOperation, sql: &str, params: &[Value]) -> Result<u64> {
        self.inner
            .execute_sync(sql, params)
            .map_err(|source| DbError::sqlmodel(operation, source))
    }

    fn query_for(&self, operation: DbOperation, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        self.inner
            .query_sync(sql, params)
            .map_err(|source| DbError::sqlmodel(operation, source))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRecord {
    version: u32,
    name: String,
    checksum: String,
    applied_at: String,
}

impl MigrationRecord {
    pub fn new(
        version: u32,
        name: impl Into<String>,
        checksum: impl Into<String>,
        applied_at: impl Into<String>,
    ) -> Result<Self> {
        let record = Self {
            version,
            name: name.into().trim().to_string(),
            checksum: checksum.into().trim().to_string(),
            applied_at: applied_at.into().trim().to_string(),
        };
        record.validate()?;
        Ok(record)
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    pub fn applied_at(&self) -> &str {
        &self.applied_at
    }

    fn validate(&self) -> Result<()> {
        validate_migration_version(self.version)?;
        validate_required_text(MigrationField::Name, &self.name)?;
        validate_required_text(MigrationField::Checksum, &self.checksum)?;
        validate_required_text(MigrationField::AppliedAt, &self.applied_at)
    }

    fn from_row(row: &Row) -> Result<Self> {
        let version = required_i64(row, 0, DbOperation::ListMigrations, "version")?;
        let version = u32::try_from(version).map_err(|_| DbError::MalformedRow {
            operation: DbOperation::ListMigrations,
            message: format!("migration version must fit u32, got {version}"),
        })?;
        let name = required_text(row, 1, DbOperation::ListMigrations, "name")?;
        let checksum = required_text(row, 2, DbOperation::ListMigrations, "checksum")?;
        let applied_at = required_text(row, 3, DbOperation::ListMigrations, "applied_at")?;

        Self::new(version, name, checksum, applied_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationTableColumn {
    name: String,
    sql_type: String,
    not_null: bool,
    primary_key_position: u32,
}

impl MigrationTableColumn {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn sql_type(&self) -> &str {
        &self.sql_type
    }

    pub const fn not_null(&self) -> bool {
        self.not_null
    }

    pub const fn primary_key_position(&self) -> u32 {
        self.primary_key_position
    }

    fn from_pragma_row(row: &Row) -> Result<Self> {
        let name = required_text(row, 1, DbOperation::InspectMigrationTable, "name")?;
        let sql_type = required_text(row, 2, DbOperation::InspectMigrationTable, "type")?;
        let not_null = required_i64(row, 3, DbOperation::InspectMigrationTable, "notnull")? != 0;
        let primary_key_position = required_i64(row, 5, DbOperation::InspectMigrationTable, "pk")?;
        let primary_key_position =
            u32::try_from(primary_key_position).map_err(|_| DbError::MalformedRow {
                operation: DbOperation::InspectMigrationTable,
                message: "migration table primary-key position must fit u32".to_string(),
            })?;

        Ok(Self {
            name: name.to_string(),
            sql_type: sql_type.to_string(),
            not_null,
            primary_key_position,
        })
    }
}

#[derive(Debug)]
pub enum DbError {
    SqlModel {
        operation: DbOperation,
        source: Box<sqlmodel_core::Error>,
    },
    InvalidPath {
        operation: DbOperation,
        path: PathBuf,
        message: String,
    },
    InvalidMode {
        location: DatabaseLocation,
        mode: DatabaseOpenMode,
        message: String,
    },
    InvalidMigration {
        field: MigrationField,
        message: String,
    },
    MalformedRow {
        operation: DbOperation,
        message: String,
    },
}

impl DbError {
    fn sqlmodel(operation: DbOperation, source: sqlmodel_core::Error) -> Self {
        Self::SqlModel {
            operation,
            source: Box::new(source),
        }
    }

    pub const fn operation(&self) -> Option<DbOperation> {
        match self {
            Self::SqlModel { operation, .. } | Self::InvalidPath { operation, .. } => {
                Some(*operation)
            }
            Self::MalformedRow { operation, .. } => Some(*operation),
            Self::InvalidMode { .. } | Self::InvalidMigration { .. } => None,
        }
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SqlModel { operation, source } => {
                write!(f, "database {} failed: {}", operation, source)
            }
            Self::InvalidPath {
                operation,
                path,
                message,
            } => write!(
                f,
                "database {} failed for path '{}': {}",
                operation,
                path.display(),
                message
            ),
            Self::InvalidMode {
                location,
                mode,
                message,
            } => write!(
                f,
                "database open mode {:?} is invalid for {:?}: {}",
                mode, location, message
            ),
            Self::InvalidMigration { field, message } => {
                write!(f, "invalid migration {}: {}", field, message)
            }
            Self::MalformedRow { operation, message } => {
                write!(
                    f,
                    "database {} returned malformed row: {}",
                    operation, message
                )
            }
        }
    }
}

impl Error for DbError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SqlModel { source, .. } => Some(source.as_ref()),
            Self::InvalidPath { .. }
            | Self::InvalidMode { .. }
            | Self::InvalidMigration { .. }
            | Self::MalformedRow { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbOperation {
    OpenMemory,
    OpenReadWrite,
    OpenSchemaOnly,
    Query,
    Execute,
    Close,
    BeginTransaction,
    CommitTransaction,
    RollbackTransaction,
    EnsureMigrationTable,
    InspectMigrationTable,
    RecordMigration,
    ListMigrations,
    CheckMigration,
}

impl fmt::Display for DbOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenMemory => f.write_str("memory open"),
            Self::OpenReadWrite => f.write_str("read-write open"),
            Self::OpenSchemaOnly => f.write_str("schema-only open"),
            Self::Query => f.write_str("query"),
            Self::Execute => f.write_str("execute"),
            Self::Close => f.write_str("close"),
            Self::BeginTransaction => f.write_str("transaction begin"),
            Self::CommitTransaction => f.write_str("transaction commit"),
            Self::RollbackTransaction => f.write_str("transaction rollback"),
            Self::EnsureMigrationTable => f.write_str("migration table ensure"),
            Self::InspectMigrationTable => f.write_str("migration table inspect"),
            Self::RecordMigration => f.write_str("migration record insert"),
            Self::ListMigrations => f.write_str("migration list"),
            Self::CheckMigration => f.write_str("migration check"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationField {
    Version,
    Name,
    Checksum,
    AppliedAt,
}

impl fmt::Display for MigrationField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version => f.write_str("version"),
            Self::Name => f.write_str("name"),
            Self::Checksum => f.write_str("checksum"),
            Self::AppliedAt => f.write_str("applied_at"),
        }
    }
}

fn database_path_string(path: &Path, operation: DbOperation) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| DbError::InvalidPath {
            operation,
            path: path.to_path_buf(),
            message: "FrankenSQLite database paths must be valid UTF-8".to_string(),
        })
}

fn validate_migration_version(version: u32) -> Result<()> {
    if version == 0 {
        Err(DbError::InvalidMigration {
            field: MigrationField::Version,
            message: "version must be greater than zero".to_string(),
        })
    } else {
        Ok(())
    }
}

fn validate_required_text(field: MigrationField, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(DbError::InvalidMigration {
            field,
            message: "value must not be empty".to_string(),
        })
    } else {
        Ok(())
    }
}

fn required_value<'a>(
    row: &'a Row,
    index: usize,
    operation: DbOperation,
    column: &str,
) -> Result<&'a Value> {
    row.get(index).ok_or_else(|| DbError::MalformedRow {
        operation,
        message: format!("missing {column} column at index {index}"),
    })
}

fn required_i64(row: &Row, index: usize, operation: DbOperation, column: &str) -> Result<i64> {
    required_value(row, index, operation, column)?
        .as_i64()
        .ok_or_else(|| DbError::MalformedRow {
            operation,
            message: format!("{column} column at index {index} is not an integer"),
        })
}

fn required_text<'a>(
    row: &'a Row,
    index: usize,
    operation: DbOperation,
    column: &str,
) -> Result<&'a str> {
    required_value(row, index, operation, column)?
        .as_str()
        .ok_or_else(|| DbError::MalformedRow {
            operation,
            message: format!("{column} column at index {index} is not text"),
        })
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

/// A migration definition with version, name, SQL statements, and checksum.
#[derive(Debug, Clone)]
pub struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
    checksum: &'static str,
}

impl Migration {
    /// Construct a migration. Checksum is `blake3:<hex>` of the SQL text.
    pub const fn new(
        version: u32,
        name: &'static str,
        sql: &'static str,
        checksum: &'static str,
    ) -> Self {
        Self {
            version,
            name,
            sql,
            checksum,
        }
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn sql(&self) -> &'static str {
        self.sql
    }

    pub const fn checksum(&self) -> &'static str {
        self.checksum
    }
}

/// V001: Initial schema — workspaces, agents, memories, memory_tags, audit_log.
pub const V001_INIT_SCHEMA: Migration = Migration::new(
    1,
    "init_schema",
    r#"
-- Workspace registry
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY CHECK (id GLOB 'wsp_*' AND length(id) = 30),
    path TEXT NOT NULL UNIQUE CHECK (length(trim(path)) > 0),
    name TEXT CHECK (name IS NULL OR length(trim(name)) > 0),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0)
);
CREATE INDEX idx_workspaces_path ON workspaces(path);

-- Agent registry (tracks agents that have interacted with this ee instance)
CREATE TABLE agents (
    id TEXT PRIMARY KEY CHECK (id GLOB 'agt_*' AND length(id) = 30),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    model TEXT CHECK (model IS NULL OR length(trim(model)) > 0),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    last_seen_at TEXT NOT NULL CHECK (length(trim(last_seen_at)) > 0)
);
CREATE INDEX idx_agents_workspace ON agents(workspace_id);
CREATE INDEX idx_agents_name ON agents(name);

-- Memories (core storage)
CREATE TABLE memories (
    id TEXT PRIMARY KEY CHECK (id GLOB 'mem_*' AND length(id) = 30),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    level TEXT NOT NULL CHECK (level IN ('working', 'episodic', 'semantic', 'procedural')),
    kind TEXT NOT NULL CHECK (length(trim(kind)) > 0),
    content TEXT NOT NULL CHECK (length(trim(content)) > 0 AND length(content) <= 65536),
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    utility REAL NOT NULL CHECK (utility >= 0.0 AND utility <= 1.0),
    importance REAL NOT NULL CHECK (importance >= 0.0 AND importance <= 1.0),
    provenance_uri TEXT CHECK (provenance_uri IS NULL OR length(trim(provenance_uri)) > 0),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
    tombstoned_at TEXT CHECK (tombstoned_at IS NULL OR length(trim(tombstoned_at)) > 0)
);
CREATE INDEX idx_memories_workspace ON memories(workspace_id);
CREATE INDEX idx_memories_level ON memories(level);
CREATE INDEX idx_memories_kind ON memories(kind);
CREATE INDEX idx_memories_tombstoned ON memories(tombstoned_at);

-- Memory tags (many-to-many)
CREATE TABLE memory_tags (
    memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    tag TEXT NOT NULL CHECK (length(trim(tag)) > 0 AND length(tag) <= 64),
    PRIMARY KEY (memory_id, tag)
);
CREATE INDEX idx_memory_tags_tag ON memory_tags(tag);

-- Audit log
CREATE TABLE audit_log (
    id TEXT PRIMARY KEY CHECK (id GLOB 'audit_*' AND length(id) = 32),
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE SET NULL,
    timestamp TEXT NOT NULL CHECK (length(trim(timestamp)) > 0),
    actor TEXT CHECK (actor IS NULL OR length(trim(actor)) > 0),
    action TEXT NOT NULL CHECK (length(trim(action)) > 0),
    target_type TEXT CHECK (target_type IS NULL OR length(trim(target_type)) > 0),
    target_id TEXT CHECK (target_id IS NULL OR length(trim(target_id)) > 0),
    details TEXT CHECK (details IS NULL OR length(trim(details)) > 0)
);
CREATE INDEX idx_audit_log_workspace ON audit_log(workspace_id);
CREATE INDEX idx_audit_log_timestamp ON audit_log(timestamp);
CREATE INDEX idx_audit_log_action ON audit_log(action);
CREATE INDEX idx_audit_log_target ON audit_log(target_type, target_id);
"#,
    "blake3:v001_wsp_audit_2026_04_29",
);

/// V002: Add trust class fields to memories (ADR-0009).
pub const V002_TRUST_CLASS: Migration = Migration::new(
    2,
    "add_trust_class",
    r#"
-- Add trust class fields to memories (ADR-0009)
ALTER TABLE memories ADD COLUMN trust_class TEXT NOT NULL DEFAULT 'agent_assertion'
    CHECK (trust_class IN ('human_explicit', 'agent_validated', 'agent_assertion', 'cass_evidence', 'legacy_import'));

ALTER TABLE memories ADD COLUMN trust_subclass TEXT
    CHECK (trust_subclass IS NULL OR length(trim(trust_subclass)) > 0);

-- Create index for trust class filtering
CREATE INDEX idx_memories_trust_class ON memories(trust_class);
"#,
    "blake3:v002_trust_class_2026_04_29",
);

/// V003: Add curation candidates table (EE-180, ADR-0006).
pub const V003_CURATION_CANDIDATES: Migration = Migration::new(
    3,
    "curation_candidates",
    r#"
-- Curation candidates table (EE-180, ADR-0006)
-- Every promotion, consolidation, or tombstone goes through this auditable queue.
CREATE TABLE curation_candidates (
    id TEXT PRIMARY KEY CHECK (id GLOB 'curate_*' AND length(id) = 33),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    candidate_type TEXT NOT NULL CHECK (candidate_type IN (
        'consolidate', 'promote', 'deprecate', 'supersede', 'tombstone', 'merge', 'split', 'retract'
    )),
    target_memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    proposed_content TEXT CHECK (proposed_content IS NULL OR length(trim(proposed_content)) > 0),
    proposed_confidence REAL CHECK (proposed_confidence IS NULL OR (proposed_confidence >= 0.0 AND proposed_confidence <= 1.0)),
    proposed_trust_class TEXT CHECK (proposed_trust_class IS NULL OR proposed_trust_class IN (
        'human_explicit', 'agent_validated', 'agent_assertion', 'cass_evidence', 'legacy_import'
    )),
    source_type TEXT NOT NULL CHECK (source_type IN (
        'agent_inference', 'rule_engine', 'human_request', 'feedback_event', 'contradiction_detected', 'decay_trigger'
    )),
    source_id TEXT CHECK (source_id IS NULL OR length(trim(source_id)) > 0),
    reason TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected', 'expired', 'applied')),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    reviewed_at TEXT CHECK (reviewed_at IS NULL OR length(trim(reviewed_at)) > 0),
    reviewed_by TEXT CHECK (reviewed_by IS NULL OR length(trim(reviewed_by)) > 0),
    applied_at TEXT CHECK (applied_at IS NULL OR length(trim(applied_at)) > 0),
    ttl_expires_at TEXT CHECK (ttl_expires_at IS NULL OR length(trim(ttl_expires_at)) > 0)
);

CREATE INDEX idx_curation_candidates_workspace ON curation_candidates(workspace_id);
CREATE INDEX idx_curation_candidates_target ON curation_candidates(target_memory_id);
CREATE INDEX idx_curation_candidates_status ON curation_candidates(status);
CREATE INDEX idx_curation_candidates_type ON curation_candidates(candidate_type);
CREATE INDEX idx_curation_candidates_created ON curation_candidates(created_at);
CREATE INDEX idx_curation_candidates_ttl ON curation_candidates(ttl_expires_at) WHERE ttl_expires_at IS NOT NULL;
"#,
    "blake3:v003_curation_candidates_2026_04_29",
);

/// V004: Add procedural_rules table (EE-084).
pub const V004_PROCEDURAL_RULES: Migration = Migration::new(
    4,
    "procedural_rules",
    r#"
-- Procedural rules table (EE-084)
-- Distilled lessons, patterns, and policies from experience.
CREATE TABLE procedural_rules (
    id TEXT PRIMARY KEY CHECK (id GLOB 'rule_*' AND length(id) = 31),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    content TEXT NOT NULL CHECK (length(trim(content)) > 0 AND length(content) <= 8192),
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    utility REAL NOT NULL CHECK (utility >= 0.0 AND utility <= 1.0),
    importance REAL NOT NULL CHECK (importance >= 0.0 AND importance <= 1.0),
    trust_class TEXT NOT NULL CHECK (trust_class IN (
        'human_explicit', 'agent_validated', 'agent_assertion', 'cass_evidence', 'legacy_import'
    )),
    scope TEXT NOT NULL DEFAULT 'workspace' CHECK (scope IN (
        'global', 'workspace', 'project', 'directory', 'file_pattern'
    )),
    scope_pattern TEXT CHECK (scope_pattern IS NULL OR length(trim(scope_pattern)) > 0),
    maturity TEXT NOT NULL DEFAULT 'candidate' CHECK (maturity IN (
        'draft', 'candidate', 'validated', 'deprecated', 'superseded'
    )),
    positive_feedback_count INTEGER NOT NULL DEFAULT 0 CHECK (positive_feedback_count >= 0),
    negative_feedback_count INTEGER NOT NULL DEFAULT 0 CHECK (negative_feedback_count >= 0),
    last_applied_at TEXT CHECK (last_applied_at IS NULL OR length(trim(last_applied_at)) > 0),
    last_validated_at TEXT CHECK (last_validated_at IS NULL OR length(trim(last_validated_at)) > 0),
    superseded_by TEXT REFERENCES procedural_rules(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
    tombstoned_at TEXT CHECK (tombstoned_at IS NULL OR length(trim(tombstoned_at)) > 0)
);

CREATE INDEX idx_procedural_rules_workspace ON procedural_rules(workspace_id);
CREATE INDEX idx_procedural_rules_maturity ON procedural_rules(maturity);
CREATE INDEX idx_procedural_rules_trust_class ON procedural_rules(trust_class);
CREATE INDEX idx_procedural_rules_scope ON procedural_rules(scope);
CREATE INDEX idx_procedural_rules_confidence ON procedural_rules(confidence);
CREATE INDEX idx_procedural_rules_tombstoned ON procedural_rules(tombstoned_at);

-- Rule source memories junction (many-to-many)
CREATE TABLE rule_source_memories (
    rule_id TEXT NOT NULL REFERENCES procedural_rules(id) ON DELETE CASCADE,
    memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    PRIMARY KEY (rule_id, memory_id)
);
CREATE INDEX idx_rule_source_memories_memory ON rule_source_memories(memory_id);

-- Rule tags (many-to-many)
CREATE TABLE rule_tags (
    rule_id TEXT NOT NULL REFERENCES procedural_rules(id) ON DELETE CASCADE,
    tag TEXT NOT NULL CHECK (length(trim(tag)) > 0 AND length(tag) <= 64),
    PRIMARY KEY (rule_id, tag)
);
CREATE INDEX idx_rule_tags_tag ON rule_tags(tag);
"#,
    "blake3:v004_procedural_rules_2026_04_29",
);

/// V005: Add search_index_jobs table (EE-123).
pub const V005_SEARCH_INDEX_JOBS: Migration = Migration::new(
    5,
    "search_index_jobs",
    r#"
-- Search index jobs table (EE-123)
-- Tracks indexing jobs for Frankensearch integration.
CREATE TABLE search_index_jobs (
    id TEXT PRIMARY KEY CHECK (id GLOB 'sidx_*' AND length(id) = 31),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    job_type TEXT NOT NULL CHECK (job_type IN (
        'full_rebuild', 'incremental', 'single_document'
    )),
    document_source TEXT CHECK (document_source IS NULL OR document_source IN (
        'memory', 'session', 'rule', 'import'
    )),
    document_id TEXT CHECK (document_id IS NULL OR length(trim(document_id)) > 0),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
        'pending', 'running', 'completed', 'failed', 'cancelled'
    )),
    documents_total INTEGER NOT NULL DEFAULT 0 CHECK (documents_total >= 0),
    documents_indexed INTEGER NOT NULL DEFAULT 0 CHECK (documents_indexed >= 0),
    error_message TEXT CHECK (error_message IS NULL OR length(trim(error_message)) > 0),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    started_at TEXT CHECK (started_at IS NULL OR length(trim(started_at)) > 0),
    completed_at TEXT CHECK (completed_at IS NULL OR length(trim(completed_at)) > 0)
);

CREATE INDEX idx_search_index_jobs_workspace ON search_index_jobs(workspace_id);
CREATE INDEX idx_search_index_jobs_status ON search_index_jobs(status);
CREATE INDEX idx_search_index_jobs_created ON search_index_jobs(created_at);
CREATE INDEX idx_search_index_jobs_type ON search_index_jobs(job_type);
"#,
    "blake3:v005_search_index_jobs_2026_04_29",
);

/// All migrations in version order.
pub const MIGRATIONS: &[Migration] = &[
    V001_INIT_SCHEMA,
    V002_TRUST_CLASS,
    V003_CURATION_CANDIDATES,
    V004_PROCEDURAL_RULES,
    V005_SEARCH_INDEX_JOBS,
];

/// Result of applying migrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationResult {
    applied: Vec<u32>,
    skipped: Vec<u32>,
}

impl MigrationResult {
    pub fn applied(&self) -> &[u32] {
        &self.applied
    }

    pub fn skipped(&self) -> &[u32] {
        &self.skipped
    }

    pub fn is_empty(&self) -> bool {
        self.applied.is_empty() && self.skipped.is_empty()
    }
}

impl DbConnection {
    /// Apply all pending migrations in version order.
    pub fn migrate(&self) -> Result<MigrationResult> {
        self.ensure_migration_table()?;

        let mut applied = Vec::new();
        let mut skipped = Vec::new();

        for migration in MIGRATIONS {
            if self.has_migration(migration.version)? {
                skipped.push(migration.version);
                continue;
            }

            self.execute_raw_for(DbOperation::Execute, migration.sql)?;

            let now = Utc::now().to_rfc3339();
            let record =
                MigrationRecord::new(migration.version, migration.name, migration.checksum, now)?;
            self.record_migration(&record)?;
            applied.push(migration.version);
        }

        Ok(MigrationResult { applied, skipped })
    }

    /// Check if the database schema is up to date.
    pub fn needs_migration(&self) -> Result<bool> {
        if !self.migration_table_exists()? {
            return Ok(true);
        }

        for migration in MIGRATIONS {
            if !self.has_migration(migration.version)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Return the current schema version (highest applied migration).
    pub fn schema_version(&self) -> Result<Option<u32>> {
        if !self.migration_table_exists()? {
            return Ok(None);
        }

        let migrations = self.applied_migrations()?;
        Ok(migrations.last().map(|m| m.version()))
    }
}

/// Input for creating a new workspace.
#[derive(Debug, Clone)]
pub struct CreateWorkspaceInput {
    pub path: String,
    pub name: Option<String>,
}

/// A stored workspace row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredWorkspace {
    pub id: String,
    pub path: String,
    pub name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl DbConnection {
    /// Insert a new workspace.
    pub fn insert_workspace(&self, id: &str, input: &CreateWorkspaceInput) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO workspaces (id, path, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                Value::Text(id.to_string()),
                Value::Text(input.path.clone()),
                input.name.as_ref().map_or(Value::Null, |n| Value::Text(n.clone())),
                Value::Text(now.clone()),
                Value::Text(now),
            ],
        )?;

        Ok(())
    }

    /// Get a workspace by ID.
    pub fn get_workspace(&self, id: &str) -> Result<Option<StoredWorkspace>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, path, name, created_at, updated_at FROM workspaces WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first().map(stored_workspace_from_row).transpose()
    }

    /// Get a workspace by path.
    pub fn get_workspace_by_path(&self, path: &str) -> Result<Option<StoredWorkspace>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, path, name, created_at, updated_at FROM workspaces WHERE path = ?1",
            &[Value::Text(path.to_string())],
        )?;

        rows.first().map(stored_workspace_from_row).transpose()
    }

    /// List all workspaces.
    pub fn list_workspaces(&self) -> Result<Vec<StoredWorkspace>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, path, name, created_at, updated_at FROM workspaces ORDER BY path ASC",
            &[],
        )?;

        rows.iter().map(stored_workspace_from_row).collect()
    }

    /// Update workspace name.
    pub fn update_workspace_name(&self, id: &str, name: Option<&str>) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE workspaces SET name = ?1, updated_at = ?2 WHERE id = ?3",
            &[
                name.map_or(Value::Null, |n| Value::Text(n.to_string())),
                Value::Text(now),
                Value::Text(id.to_string()),
            ],
        )?;
        Ok(affected > 0)
    }
}

fn stored_workspace_from_row(row: &Row) -> Result<StoredWorkspace> {
    Ok(StoredWorkspace {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        path: required_text(row, 1, DbOperation::Query, "path")?.to_string(),
        name: optional_text(row, 2)?.map(str::to_string),
        created_at: required_text(row, 3, DbOperation::Query, "created_at")?.to_string(),
        updated_at: required_text(row, 4, DbOperation::Query, "updated_at")?.to_string(),
    })
}

/// Input for creating a new memory.
#[derive(Debug, Clone)]
pub struct CreateMemoryInput {
    pub workspace_id: String,
    pub level: String,
    pub kind: String,
    pub content: String,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub provenance_uri: Option<String>,
    pub trust_class: String,
    pub trust_subclass: Option<String>,
    pub tags: Vec<String>,
}

/// A stored memory row.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMemory {
    pub id: String,
    pub workspace_id: String,
    pub level: String,
    pub kind: String,
    pub content: String,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub provenance_uri: Option<String>,
    pub trust_class: String,
    pub trust_subclass: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub tombstoned_at: Option<String>,
}

impl DbConnection {
    /// Insert a new memory and its tags.
    pub fn insert_memory(&self, id: &str, input: &CreateMemoryInput) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, provenance_uri, trust_class, trust_subclass, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            &[
                Value::Text(id.to_string()),
                Value::Text(input.workspace_id.clone()),
                Value::Text(input.level.clone()),
                Value::Text(input.kind.clone()),
                Value::Text(input.content.clone()),
                Value::Float(input.confidence),
                Value::Float(input.utility),
                Value::Float(input.importance),
                input.provenance_uri.as_ref().map_or(Value::Null, |uri| Value::Text(uri.clone())),
                Value::Text(input.trust_class.clone()),
                input.trust_subclass.as_ref().map_or(Value::Null, |s| Value::Text(s.clone())),
                Value::Text(now.clone()),
                Value::Text(now),
            ],
        )?;

        for tag in &input.tags {
            self.execute_for(
                DbOperation::Execute,
                "INSERT INTO memory_tags (memory_id, tag) VALUES (?1, ?2)",
                &[Value::Text(id.to_string()), Value::Text(tag.clone())],
            )?;
        }

        Ok(())
    }

    /// Get a memory by ID.
    pub fn get_memory(&self, id: &str) -> Result<Option<StoredMemory>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, level, kind, content, confidence, utility, importance, provenance_uri, trust_class, trust_subclass, created_at, updated_at, tombstoned_at FROM memories WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first().map(stored_memory_from_row).transpose()
    }

    /// List memories in a workspace, optionally filtering by level and/or tombstone status.
    pub fn list_memories(
        &self,
        workspace_id: &str,
        level: Option<&str>,
        include_tombstoned: bool,
    ) -> Result<Vec<StoredMemory>> {
        let mut sql = String::from(
            "SELECT id, workspace_id, level, kind, content, confidence, utility, importance, provenance_uri, trust_class, trust_subclass, created_at, updated_at, tombstoned_at FROM memories WHERE workspace_id = ?1",
        );
        let mut params: Vec<Value> = vec![Value::Text(workspace_id.to_string())];

        if let Some(lvl) = level {
            sql.push_str(" AND level = ?2");
            params.push(Value::Text(lvl.to_string()));
        }

        if !include_tombstoned {
            sql.push_str(" AND tombstoned_at IS NULL");
        }

        sql.push_str(" ORDER BY id ASC");

        let rows = self.query_for(DbOperation::Query, &sql, &params)?;
        rows.iter().map(stored_memory_from_row).collect()
    }

    /// Tombstone a memory (soft delete).
    pub fn tombstone_memory(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE memories SET tombstoned_at = ?1, updated_at = ?1 WHERE id = ?2 AND tombstoned_at IS NULL",
            &[Value::Text(now), Value::Text(id.to_string())],
        )?;
        Ok(affected > 0)
    }

    /// Get tags for a memory.
    pub fn get_memory_tags(&self, memory_id: &str) -> Result<Vec<String>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT tag FROM memory_tags WHERE memory_id = ?1 ORDER BY tag ASC",
            &[Value::Text(memory_id.to_string())],
        )?;

        rows.iter()
            .map(|row| required_text(row, 0, DbOperation::Query, "tag").map(|s| s.to_string()))
            .collect()
    }

    /// Add tags to a memory (idempotent).
    pub fn add_memory_tags(&self, memory_id: &str, tags: &[String]) -> Result<()> {
        for tag in tags {
            self.execute_for(
                DbOperation::Execute,
                "INSERT OR IGNORE INTO memory_tags (memory_id, tag) VALUES (?1, ?2)",
                &[Value::Text(memory_id.to_string()), Value::Text(tag.clone())],
            )?;
        }
        Ok(())
    }

    /// Remove tags from a memory.
    pub fn remove_memory_tags(&self, memory_id: &str, tags: &[String]) -> Result<()> {
        for tag in tags {
            self.execute_for(
                DbOperation::Execute,
                "DELETE FROM memory_tags WHERE memory_id = ?1 AND tag = ?2",
                &[Value::Text(memory_id.to_string()), Value::Text(tag.clone())],
            )?;
        }
        Ok(())
    }
}

fn stored_memory_from_row(row: &Row) -> Result<StoredMemory> {
    Ok(StoredMemory {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        workspace_id: required_text(row, 1, DbOperation::Query, "workspace_id")?.to_string(),
        level: required_text(row, 2, DbOperation::Query, "level")?.to_string(),
        kind: required_text(row, 3, DbOperation::Query, "kind")?.to_string(),
        content: required_text(row, 4, DbOperation::Query, "content")?.to_string(),
        confidence: required_f64(row, 5, DbOperation::Query, "confidence")? as f32,
        utility: required_f64(row, 6, DbOperation::Query, "utility")? as f32,
        importance: required_f64(row, 7, DbOperation::Query, "importance")? as f32,
        provenance_uri: optional_text(row, 8)?.map(str::to_string),
        trust_class: required_text(row, 9, DbOperation::Query, "trust_class")?.to_string(),
        trust_subclass: optional_text(row, 10)?.map(str::to_string),
        created_at: required_text(row, 11, DbOperation::Query, "created_at")?.to_string(),
        updated_at: required_text(row, 12, DbOperation::Query, "updated_at")?.to_string(),
        tombstoned_at: optional_text(row, 13)?.map(str::to_string),
    })
}

fn required_f64(row: &Row, index: usize, operation: DbOperation, column: &str) -> Result<f64> {
    required_value(row, index, operation, column)?
        .as_f64()
        .ok_or_else(|| DbError::MalformedRow {
            operation,
            message: format!("{column} column at index {index} is not a float"),
        })
}

fn optional_text(row: &Row, index: usize) -> Result<Option<&str>> {
    match row.get(index) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => Ok(value.as_str()),
    }
}

/// Input for creating a new audit log entry.
#[derive(Debug, Clone)]
pub struct CreateAuditInput {
    pub workspace_id: Option<String>,
    pub actor: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub details: Option<String>,
}

/// A stored audit log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAuditEntry {
    pub id: String,
    pub workspace_id: Option<String>,
    pub timestamp: String,
    pub actor: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub details: Option<String>,
}

impl DbConnection {
    /// Insert a new audit log entry.
    pub fn insert_audit(&self, id: &str, input: &CreateAuditInput) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO audit_log (id, workspace_id, timestamp, actor, action, target_type, target_id, details) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            &[
                Value::Text(id.to_string()),
                input.workspace_id.as_ref().map_or(Value::Null, |w| Value::Text(w.clone())),
                Value::Text(now),
                input.actor.as_ref().map_or(Value::Null, |a| Value::Text(a.clone())),
                Value::Text(input.action.clone()),
                input.target_type.as_ref().map_or(Value::Null, |t| Value::Text(t.clone())),
                input.target_id.as_ref().map_or(Value::Null, |t| Value::Text(t.clone())),
                input.details.as_ref().map_or(Value::Null, |d| Value::Text(d.clone())),
            ],
        )?;

        Ok(())
    }

    /// Get an audit log entry by ID.
    pub fn get_audit(&self, id: &str) -> Result<Option<StoredAuditEntry>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, timestamp, actor, action, target_type, target_id, details FROM audit_log WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first().map(stored_audit_from_row).transpose()
    }

    /// List audit log entries for a workspace, ordered by timestamp descending.
    pub fn list_audit_entries(
        &self,
        workspace_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<StoredAuditEntry>> {
        let mut sql = String::from(
            "SELECT id, workspace_id, timestamp, actor, action, target_type, target_id, details FROM audit_log",
        );
        let mut params: Vec<Value> = Vec::new();

        if let Some(wid) = workspace_id {
            sql.push_str(" WHERE workspace_id = ?1");
            params.push(Value::Text(wid.to_string()));
        }

        sql.push_str(" ORDER BY timestamp DESC");

        if let Some(lim) = limit {
            sql.push_str(&format!(" LIMIT {}", lim));
        }

        let rows = self.query_for(DbOperation::Query, &sql, &params)?;
        rows.iter().map(stored_audit_from_row).collect()
    }

    /// List audit log entries for a specific target.
    pub fn list_audit_by_target(
        &self,
        target_type: &str,
        target_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<StoredAuditEntry>> {
        let mut sql = String::from(
            "SELECT id, workspace_id, timestamp, actor, action, target_type, target_id, details FROM audit_log WHERE target_type = ?1 AND target_id = ?2 ORDER BY timestamp DESC",
        );

        if let Some(lim) = limit {
            sql.push_str(&format!(" LIMIT {}", lim));
        }

        let rows = self.query_for(
            DbOperation::Query,
            &sql,
            &[
                Value::Text(target_type.to_string()),
                Value::Text(target_id.to_string()),
            ],
        )?;
        rows.iter().map(stored_audit_from_row).collect()
    }

    /// List audit log entries by action type.
    pub fn list_audit_by_action(
        &self,
        action: &str,
        limit: Option<u32>,
    ) -> Result<Vec<StoredAuditEntry>> {
        let mut sql = String::from(
            "SELECT id, workspace_id, timestamp, actor, action, target_type, target_id, details FROM audit_log WHERE action = ?1 ORDER BY timestamp DESC",
        );

        if let Some(lim) = limit {
            sql.push_str(&format!(" LIMIT {}", lim));
        }

        let rows = self.query_for(DbOperation::Query, &sql, &[Value::Text(action.to_string())])?;
        rows.iter().map(stored_audit_from_row).collect()
    }
}

fn stored_audit_from_row(row: &Row) -> Result<StoredAuditEntry> {
    Ok(StoredAuditEntry {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        workspace_id: optional_text(row, 1)?.map(str::to_string),
        timestamp: required_text(row, 2, DbOperation::Query, "timestamp")?.to_string(),
        actor: optional_text(row, 3)?.map(str::to_string),
        action: required_text(row, 4, DbOperation::Query, "action")?.to_string(),
        target_type: optional_text(row, 5)?.map(str::to_string),
        target_id: optional_text(row, 6)?.map(str::to_string),
        details: optional_text(row, 7)?.map(str::to_string),
    })
}

/// Job type for search indexing operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchIndexJobType {
    FullRebuild,
    Incremental,
    SingleDocument,
}

impl SearchIndexJobType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FullRebuild => "full_rebuild",
            Self::Incremental => "incremental",
            Self::SingleDocument => "single_document",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "full_rebuild" => Some(Self::FullRebuild),
            "incremental" => Some(Self::Incremental),
            "single_document" => Some(Self::SingleDocument),
            _ => None,
        }
    }
}

/// Status of a search index job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchIndexJobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl SearchIndexJobStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Input for creating a new search index job.
#[derive(Debug, Clone)]
pub struct CreateSearchIndexJobInput {
    pub workspace_id: String,
    pub job_type: SearchIndexJobType,
    pub document_source: Option<String>,
    pub document_id: Option<String>,
    pub documents_total: u32,
}

/// A stored search index job row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSearchIndexJob {
    pub id: String,
    pub workspace_id: String,
    pub job_type: String,
    pub document_source: Option<String>,
    pub document_id: Option<String>,
    pub status: String,
    pub documents_total: u32,
    pub documents_indexed: u32,
    pub error_message: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

impl StoredSearchIndexJob {
    #[must_use]
    pub fn job_type_enum(&self) -> Option<SearchIndexJobType> {
        SearchIndexJobType::from_str(&self.job_type)
    }

    #[must_use]
    pub fn status_enum(&self) -> Option<SearchIndexJobStatus> {
        SearchIndexJobStatus::from_str(&self.status)
    }
}

impl DbConnection {
    /// Insert a new search index job.
    pub fn insert_search_index_job(
        &self,
        id: &str,
        input: &CreateSearchIndexJobInput,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO search_index_jobs (id, workspace_id, job_type, document_source, document_id, status, documents_total, documents_indexed, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            &[
                Value::Text(id.to_string()),
                Value::Text(input.workspace_id.clone()),
                Value::Text(input.job_type.as_str().to_string()),
                input.document_source.as_ref().map_or(Value::Null, |s| Value::Text(s.clone())),
                input.document_id.as_ref().map_or(Value::Null, |s| Value::Text(s.clone())),
                Value::Text(SearchIndexJobStatus::Pending.as_str().to_string()),
                Value::BigInt(i64::from(input.documents_total)),
                Value::BigInt(0),
                Value::Text(now),
            ],
        )?;

        Ok(())
    }

    /// Get a search index job by ID.
    pub fn get_search_index_job(&self, id: &str) -> Result<Option<StoredSearchIndexJob>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, job_type, document_source, document_id, status, documents_total, documents_indexed, error_message, created_at, started_at, completed_at FROM search_index_jobs WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first()
            .map(stored_search_index_job_from_row)
            .transpose()
    }

    /// List search index jobs for a workspace, optionally filtered by status.
    pub fn list_search_index_jobs(
        &self,
        workspace_id: &str,
        status: Option<SearchIndexJobStatus>,
    ) -> Result<Vec<StoredSearchIndexJob>> {
        let mut sql = String::from(
            "SELECT id, workspace_id, job_type, document_source, document_id, status, documents_total, documents_indexed, error_message, created_at, started_at, completed_at FROM search_index_jobs WHERE workspace_id = ?1",
        );
        let mut params: Vec<Value> = vec![Value::Text(workspace_id.to_string())];

        if let Some(s) = status {
            sql.push_str(" AND status = ?2");
            params.push(Value::Text(s.as_str().to_string()));
        }

        sql.push_str(" ORDER BY created_at DESC");

        let rows = self.query_for(DbOperation::Query, &sql, &params)?;
        rows.iter()
            .map(stored_search_index_job_from_row)
            .collect()
    }

    /// Start a search index job (set status to running).
    pub fn start_search_index_job(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE search_index_jobs SET status = ?1, started_at = ?2 WHERE id = ?3 AND status = ?4",
            &[
                Value::Text(SearchIndexJobStatus::Running.as_str().to_string()),
                Value::Text(now),
                Value::Text(id.to_string()),
                Value::Text(SearchIndexJobStatus::Pending.as_str().to_string()),
            ],
        )?;
        Ok(affected > 0)
    }

    /// Update progress of a search index job.
    pub fn update_search_index_job_progress(
        &self,
        id: &str,
        documents_indexed: u32,
    ) -> Result<bool> {
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE search_index_jobs SET documents_indexed = ?1 WHERE id = ?2 AND status = ?3",
            &[
                Value::BigInt(i64::from(documents_indexed)),
                Value::Text(id.to_string()),
                Value::Text(SearchIndexJobStatus::Running.as_str().to_string()),
            ],
        )?;
        Ok(affected > 0)
    }

    /// Complete a search index job successfully.
    pub fn complete_search_index_job(&self, id: &str, documents_indexed: u32) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE search_index_jobs SET status = ?1, documents_indexed = ?2, completed_at = ?3 WHERE id = ?4 AND status = ?5",
            &[
                Value::Text(SearchIndexJobStatus::Completed.as_str().to_string()),
                Value::BigInt(i64::from(documents_indexed)),
                Value::Text(now),
                Value::Text(id.to_string()),
                Value::Text(SearchIndexJobStatus::Running.as_str().to_string()),
            ],
        )?;
        Ok(affected > 0)
    }

    /// Fail a search index job with an error message.
    pub fn fail_search_index_job(&self, id: &str, error_message: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE search_index_jobs SET status = ?1, error_message = ?2, completed_at = ?3 WHERE id = ?4 AND status = ?5",
            &[
                Value::Text(SearchIndexJobStatus::Failed.as_str().to_string()),
                Value::Text(error_message.to_string()),
                Value::Text(now),
                Value::Text(id.to_string()),
                Value::Text(SearchIndexJobStatus::Running.as_str().to_string()),
            ],
        )?;
        Ok(affected > 0)
    }

    /// Cancel a pending search index job.
    pub fn cancel_search_index_job(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE search_index_jobs SET status = ?1, completed_at = ?2 WHERE id = ?3 AND status = ?4",
            &[
                Value::Text(SearchIndexJobStatus::Cancelled.as_str().to_string()),
                Value::Text(now),
                Value::Text(id.to_string()),
                Value::Text(SearchIndexJobStatus::Pending.as_str().to_string()),
            ],
        )?;
        Ok(affected > 0)
    }

    /// Get the latest search index job for a workspace (regardless of status).
    pub fn latest_search_index_job(
        &self,
        workspace_id: &str,
    ) -> Result<Option<StoredSearchIndexJob>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, job_type, document_source, document_id, status, documents_total, documents_indexed, error_message, created_at, started_at, completed_at FROM search_index_jobs WHERE workspace_id = ?1 ORDER BY created_at DESC LIMIT 1",
            &[Value::Text(workspace_id.to_string())],
        )?;

        rows.first()
            .map(stored_search_index_job_from_row)
            .transpose()
    }
}

fn stored_search_index_job_from_row(row: &Row) -> Result<StoredSearchIndexJob> {
    Ok(StoredSearchIndexJob {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        workspace_id: required_text(row, 1, DbOperation::Query, "workspace_id")?.to_string(),
        job_type: required_text(row, 2, DbOperation::Query, "job_type")?.to_string(),
        document_source: optional_text(row, 3)?.map(str::to_string),
        document_id: optional_text(row, 4)?.map(str::to_string),
        status: required_text(row, 5, DbOperation::Query, "status")?.to_string(),
        documents_total: u32::try_from(required_i64(
            row,
            6,
            DbOperation::Query,
            "documents_total",
        )?)
        .map_err(|_| DbError::MalformedRow {
            operation: DbOperation::Query,
            message: "documents_total must fit u32".to_string(),
        })?,
        documents_indexed: u32::try_from(required_i64(
            row,
            7,
            DbOperation::Query,
            "documents_indexed",
        )?)
        .map_err(|_| DbError::MalformedRow {
            operation: DbOperation::Query,
            message: "documents_indexed must fit u32".to_string(),
        })?,
        error_message: optional_text(row, 8)?.map(str::to_string),
        created_at: required_text(row, 9, DbOperation::Query, "created_at")?.to_string(),
        started_at: optional_text(row, 10)?.map(str::to_string),
        completed_at: optional_text(row, 11)?.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;
    use std::fmt;
    use std::path::PathBuf;

    use super::{
        DatabaseConfig, DatabaseLocation, DatabaseOpenMode, DbConnection, DbError, DbOperation,
        MIGRATION_TABLE_NAME, MigrationRecord, MigrationTableColumn, subsystem_name,
    };
    use sqlmodel_core::Row;
    use sqlmodel_core::Value;

    type TestResult = std::result::Result<(), TestFailure>;

    #[derive(Debug)]
    struct TestFailure(String);

    impl TestFailure {
        fn new(message: impl Into<String>) -> Self {
            Self(message.into())
        }
    }

    impl fmt::Display for TestFailure {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl StdError for TestFailure {}

    impl From<DbError> for TestFailure {
        fn from(error: DbError) -> Self {
            Self(error.to_string())
        }
    }

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(TestFailure::new(message))
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(TestFailure::new(format!(
                "{context}: expected {expected:?}, got {actual:?}"
            )))
        }
    }

    fn first_value<'a>(
        rows: &'a [Row],
        index: usize,
        context: &str,
    ) -> std::result::Result<&'a Value, TestFailure> {
        rows.first()
            .and_then(|row| row.get(index))
            .ok_or_else(|| TestFailure::new(format!("{context}: missing first-row column {index}")))
    }

    fn first_migration<'a>(
        migrations: &'a [MigrationRecord],
        context: &str,
    ) -> std::result::Result<&'a MigrationRecord, TestFailure> {
        migrations
            .first()
            .ok_or_else(|| TestFailure::new(format!("{context}: missing first migration")))
    }

    fn column_signature(columns: &[MigrationTableColumn]) -> Vec<(&str, &str, bool, u32)> {
        columns
            .iter()
            .map(|column| {
                (
                    column.name(),
                    column.sql_type(),
                    column.not_null(),
                    column.primary_key_position(),
                )
            })
            .collect()
    }

    #[test]
    fn subsystem_name_is_stable() -> TestResult {
        ensure_equal(&subsystem_name(), &"db", "db subsystem name")
    }

    #[test]
    fn memory_config_uses_read_write_mode() -> TestResult {
        let config = DatabaseConfig::memory();

        ensure_equal(
            config.location(),
            &DatabaseLocation::Memory,
            "memory config location",
        )?;
        ensure_equal(
            &config.mode(),
            &DatabaseOpenMode::ReadWrite,
            "memory config mode",
        )
    }

    #[test]
    fn schema_only_config_requires_file_location() -> TestResult {
        let path = PathBuf::from("memory.db");
        let config = DatabaseConfig::schema_only(&path);

        ensure_equal(
            config.location(),
            &DatabaseLocation::File(PathBuf::from("memory.db")),
            "schema-only config location",
        )?;
        ensure_equal(
            &config.mode(),
            &DatabaseOpenMode::SchemaOnly,
            "schema-only config mode",
        )
    }

    #[test]
    fn opens_memory_connection_through_sqlmodel_frankensqlite() -> TestResult {
        let connection = DbConnection::open_memory()?;

        ensure_equal(&connection.path(), &":memory:", "memory database path")?;
        ensure_equal(
            connection.location(),
            &DatabaseLocation::Memory,
            "memory connection location",
        )?;
        ensure_equal(
            &connection.mode(),
            &DatabaseOpenMode::ReadWrite,
            "memory connection mode",
        )?;
        connection.ping()?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn executes_queries_through_sqlmodel_frankensqlite() -> TestResult {
        let connection = DbConnection::open_memory()?;

        connection
            .execute_raw("CREATE TABLE memories (id INTEGER PRIMARY KEY, body TEXT NOT NULL)")?;
        connection.execute_raw(
            "INSERT INTO memories (id, body) VALUES (1, 'Run cargo fmt --check before release.')",
        )?;
        let rows = connection.query(
            "SELECT body FROM memories WHERE id = ?1",
            &[Value::BigInt(1)],
        )?;

        ensure_equal(&rows.len(), &1, "memory query row count")?;
        ensure_equal(
            &first_value(&rows, 0, "memory query")?.as_str(),
            &Some("Run cargo fmt --check before release."),
            "memory query body",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn rejects_schema_only_memory_connections() -> TestResult {
        let result = DbConnection::open(DatabaseConfig {
            location: DatabaseLocation::Memory,
            mode: DatabaseOpenMode::SchemaOnly,
        });

        ensure(
            matches!(result, Err(DbError::InvalidMode { .. })),
            "schema-only memory connection must return InvalidMode",
        )
    }

    #[test]
    fn migration_table_name_is_stable() -> TestResult {
        ensure_equal(
            &MIGRATION_TABLE_NAME,
            &"ee_schema_migrations",
            "migration table name",
        )
    }

    #[test]
    fn ensure_migration_table_is_idempotent_and_introspectable() -> TestResult {
        let connection = DbConnection::open_memory()?;

        ensure(
            !connection.migration_table_exists()?,
            "fresh database must not report migration table",
        )?;
        connection.ensure_migration_table()?;
        connection.ensure_migration_table()?;

        ensure(
            connection.migration_table_exists()?,
            "migration table must exist after ensure",
        )?;
        let columns = connection.migration_table_columns()?;
        let signature = column_signature(&columns);
        ensure_equal(
            &signature,
            &vec![
                ("version", "INTEGER", false, 1),
                ("name", "TEXT", true, 0),
                ("checksum", "TEXT", true, 0),
                ("applied_at", "TEXT", true, 0),
            ],
            "migration table column signature",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn record_migration_persists_deterministic_order() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.ensure_migration_table()?;
        let second = MigrationRecord::new(
            2,
            "add_memory_indexes",
            "sha256:22222222222222222222222222222222",
            "2026-04-29T20:00:00Z",
        )?;
        let first = MigrationRecord::new(
            1,
            "init_schema",
            "sha256:11111111111111111111111111111111",
            "2026-04-29T19:59:00Z",
        )?;

        connection.record_migration(&second)?;
        connection.record_migration(&first)?;

        let applied = connection.applied_migrations()?;
        ensure_equal(
            &applied,
            &vec![first.clone(), second.clone()],
            "applied migrations must be ordered by version",
        )?;
        ensure(connection.has_migration(1)?, "version 1 must be present")?;
        ensure(connection.has_migration(2)?, "version 2 must be present")?;
        ensure(!connection.has_migration(3)?, "version 3 must be absent")?;
        let first_applied = first_migration(&applied, "applied migrations")?;
        ensure_equal(&first_applied.version(), &1, "first migration version")?;
        ensure_equal(
            &first_applied.name(),
            &"init_schema",
            "first migration name",
        )?;
        ensure_equal(
            &first_applied.checksum(),
            &"sha256:11111111111111111111111111111111",
            "first migration checksum",
        )?;
        ensure_equal(
            &first_applied.applied_at(),
            &"2026-04-29T19:59:00Z",
            "first migration timestamp",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn record_migration_rejects_invalid_metadata_without_writing() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.ensure_migration_table()?;
        let invalid = MigrationRecord {
            version: 0,
            name: "init_schema".to_string(),
            checksum: "sha256:11111111111111111111111111111111".to_string(),
            applied_at: "2026-04-29T19:59:00Z".to_string(),
        };
        let result = connection.record_migration(&invalid);

        ensure(
            matches!(
                result,
                Err(DbError::InvalidMigration {
                    field: super::MigrationField::Version,
                    ..
                })
            ),
            "zero migration version must be rejected",
        )?;
        ensure_equal(
            &connection.applied_migrations()?.len(),
            &0,
            "invalid migration must not be written",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn duplicate_migration_version_is_rejected_by_storage() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.ensure_migration_table()?;
        let first = MigrationRecord::new(
            1,
            "init_schema",
            "sha256:11111111111111111111111111111111",
            "2026-04-29T19:59:00Z",
        )?;
        let duplicate = MigrationRecord::new(
            1,
            "duplicate_schema",
            "sha256:22222222222222222222222222222222",
            "2026-04-29T20:00:00Z",
        )?;

        connection.record_migration(&first)?;
        let result = connection.record_migration(&duplicate);

        ensure(
            matches!(
                result,
                Err(DbError::SqlModel {
                    operation: DbOperation::RecordMigration,
                    ..
                })
            ),
            "duplicate migration version must return a storage error",
        )?;
        ensure_equal(
            &connection.applied_migrations()?.len(),
            &1,
            "duplicate insert must preserve one migration row",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn v001_migration_creates_all_core_tables() -> TestResult {
        let connection = DbConnection::open_memory()?;
        let result = connection.migrate()?;

        ensure_equal(
            &result.applied().to_vec(),
            &vec![1u32, 2, 3, 4, 5],
            "V001-V005 must be applied",
        )?;
        ensure_equal(&result.skipped().len(), &0, "no migrations skipped")?;

        let tables = connection.query(
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
            &[],
        )?;
        let table_names: Vec<&str> = tables
            .iter()
            .filter_map(|row| row.get(0).and_then(|v| v.as_str()))
            .collect();

        ensure(
            table_names.contains(&"workspaces"),
            "workspaces table must exist",
        )?;
        ensure(table_names.contains(&"agents"), "agents table must exist")?;
        ensure(
            table_names.contains(&"memories"),
            "memories table must exist",
        )?;
        ensure(
            table_names.contains(&"memory_tags"),
            "memory_tags table must exist",
        )?;
        ensure(
            table_names.contains(&"audit_log"),
            "audit_log table must exist",
        )?;
        ensure(
            table_names.contains(&"ee_schema_migrations"),
            "migration table must exist",
        )?;
        ensure(
            table_names.contains(&"search_index_jobs"),
            "search_index_jobs table must exist",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn migrate_is_idempotent() -> TestResult {
        let connection = DbConnection::open_memory()?;

        let first = connection.migrate()?;
        ensure_equal(
            &first.applied().to_vec(),
            &vec![1u32, 2, 3, 4, 5],
            "first run applies V001-V005",
        )?;

        let second = connection.migrate()?;
        ensure_equal(&second.applied().len(), &0, "second run applies nothing")?;
        ensure_equal(
            &second.skipped().to_vec(),
            &vec![1u32, 2, 3, 4, 5],
            "second run skips V001-V005",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn needs_migration_detects_fresh_database() -> TestResult {
        let connection = DbConnection::open_memory()?;

        ensure(
            connection.needs_migration()?,
            "fresh database needs migration",
        )?;

        connection.migrate()?;

        ensure(
            !connection.needs_migration()?,
            "migrated database does not need migration",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn schema_version_tracks_applied_migrations() -> TestResult {
        let connection = DbConnection::open_memory()?;

        ensure_equal(
            &connection.schema_version()?,
            &None,
            "fresh database has no schema version",
        )?;

        connection.migrate()?;

        ensure_equal(
            &connection.schema_version()?,
            &Some(5),
            "after migrations, schema version is 5",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn v001_enforces_id_format_constraints() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let bad_workspace = connection.execute_raw(
            "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('bad', '/tmp', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        );
        ensure(
            bad_workspace.is_err(),
            "workspace with invalid id format must be rejected",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn v001_enforces_memory_level_enum() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        connection.execute_raw(
            "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('wsp_01234567890123456789012345', '/tmp/test', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )?;

        let bad_level = connection.execute_raw(
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, created_at, updated_at) VALUES ('mem_01234567890123456789012345', 'wsp_01234567890123456789012345', 'invalid', 'rule', 'test', 0.5, 0.5, 0.5, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        );
        ensure(
            bad_level.is_err(),
            "memory with invalid level must be rejected",
        )?;

        let good_level = connection.execute_raw(
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, created_at, updated_at) VALUES ('mem_01234567890123456789012345', 'wsp_01234567890123456789012345', 'procedural', 'rule', 'test', 0.5, 0.5, 0.5, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        );
        ensure(
            good_level.is_ok(),
            "memory with valid level must be accepted",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn v001_enforces_score_bounds() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        connection.execute_raw(
            "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('wsp_01234567890123456789012345', '/tmp/test', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )?;

        let bad_confidence = connection.execute_raw(
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, created_at, updated_at) VALUES ('mem_01234567890123456789012345', 'wsp_01234567890123456789012345', 'semantic', 'fact', 'test', 1.5, 0.5, 0.5, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        );
        ensure(
            bad_confidence.is_err(),
            "memory with confidence > 1.0 must be rejected",
        )?;

        let bad_negative = connection.execute_raw(
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, created_at, updated_at) VALUES ('mem_01234567890123456789012345', 'wsp_01234567890123456789012345', 'semantic', 'fact', 'test', -0.1, 0.5, 0.5, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        );
        ensure(
            bad_negative.is_err(),
            "memory with negative confidence must be rejected",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn migration_struct_accessors() -> TestResult {
        let migration = super::Migration::new(42, "test_migration", "SELECT 1", "blake3:abc123");

        ensure_equal(&migration.version(), &42, "migration version")?;
        ensure_equal(&migration.name(), &"test_migration", "migration name")?;
        ensure_equal(&migration.sql(), &"SELECT 1", "migration sql")?;
        ensure_equal(
            &migration.checksum(),
            &"blake3:abc123",
            "migration checksum",
        )?;

        Ok(())
    }

    #[test]
    fn migration_result_accessors() -> TestResult {
        let result = super::MigrationResult {
            applied: vec![1, 2],
            skipped: vec![3],
        };

        ensure_equal(
            &result.applied().to_vec(),
            &vec![1u32, 2],
            "applied migrations",
        )?;
        ensure_equal(
            &result.skipped().to_vec(),
            &vec![3u32],
            "skipped migrations",
        )?;
        ensure(!result.is_empty(), "result with content is not empty")?;

        let empty = super::MigrationResult {
            applied: vec![],
            skipped: vec![],
        };
        ensure(empty.is_empty(), "empty result is empty")?;

        Ok(())
    }

    fn setup_workspace(connection: &DbConnection) -> TestResult {
        connection.execute_raw(
            "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('wsp_01234567890123456789012345', '/tmp/test', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )?;
        Ok(())
    }

    #[test]
    fn insert_and_get_memory() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "procedural".to_string(),
            kind: "rule".to_string(),
            content: "Always run cargo fmt before commit.".to_string(),
            confidence: 0.9,
            utility: 0.7,
            importance: 0.8,
            provenance_uri: Some("file://AGENTS.md#L42".to_string()),
            trust_class: "human_explicit".to_string(),
            trust_subclass: Some("project-rule".to_string()),
            tags: vec!["cargo".to_string(), "formatting".to_string()],
        };

        connection.insert_memory("mem_01234567890123456789012345", &input)?;

        let memory = connection.get_memory("mem_01234567890123456789012345")?;
        ensure(memory.is_some(), "memory must be found")?;

        let memory = memory.ok_or_else(|| TestFailure::new("memory not found"))?;
        ensure_equal(&memory.id.as_str(), &"mem_01234567890123456789012345", "id")?;
        ensure_equal(
            &memory.workspace_id.as_str(),
            &"wsp_01234567890123456789012345",
            "workspace_id",
        )?;
        ensure_equal(&memory.level.as_str(), &"procedural", "level")?;
        ensure_equal(&memory.kind.as_str(), &"rule", "kind")?;
        ensure_equal(
            &memory.content.as_str(),
            &"Always run cargo fmt before commit.",
            "content",
        )?;
        ensure(
            (memory.confidence - 0.9).abs() < 0.001,
            "confidence must be ~0.9",
        )?;
        ensure((memory.utility - 0.7).abs() < 0.001, "utility must be ~0.7")?;
        ensure(
            (memory.importance - 0.8).abs() < 0.001,
            "importance must be ~0.8",
        )?;
        ensure_equal(
            &memory.provenance_uri,
            &Some("file://AGENTS.md#L42".to_string()),
            "provenance_uri",
        )?;
        ensure_equal(
            &memory.trust_class.as_str(),
            &"human_explicit",
            "trust_class",
        )?;
        ensure_equal(
            &memory.trust_subclass,
            &Some("project-rule".to_string()),
            "trust_subclass",
        )?;
        ensure(memory.tombstoned_at.is_none(), "not tombstoned")?;

        let tags = connection.get_memory_tags("mem_01234567890123456789012345")?;
        ensure_equal(
            &tags,
            &vec!["cargo".to_string(), "formatting".to_string()],
            "tags",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn get_nonexistent_memory_returns_none() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let memory = connection.get_memory("mem_nonexistent0000000000000")?;
        ensure(memory.is_none(), "nonexistent memory must be None")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_memories_filters_by_workspace_and_level() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let rule = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "procedural".to_string(),
            kind: "rule".to_string(),
            content: "Rule content".to_string(),
            confidence: 0.9,
            utility: 0.7,
            importance: 0.8,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec![],
        };

        let fact = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Fact content".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "cass_evidence".to_string(),
            trust_subclass: None,
            tags: vec![],
        };

        connection.insert_memory("mem_00000000000000000000000001", &rule)?;
        connection.insert_memory("mem_00000000000000000000000002", &fact)?;

        let all = connection.list_memories("wsp_01234567890123456789012345", None, false)?;
        ensure_equal(&all.len(), &2, "list all returns 2")?;

        let procedural = connection.list_memories(
            "wsp_01234567890123456789012345",
            Some("procedural"),
            false,
        )?;
        ensure_equal(&procedural.len(), &1, "filter by procedural returns 1")?;
        ensure_equal(
            &procedural[0].kind.as_str(),
            &"rule",
            "filtered memory is rule",
        )?;

        let semantic =
            connection.list_memories("wsp_01234567890123456789012345", Some("semantic"), false)?;
        ensure_equal(&semantic.len(), &1, "filter by semantic returns 1")?;
        ensure_equal(
            &semantic[0].kind.as_str(),
            &"fact",
            "filtered memory is fact",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn tombstone_memory_soft_deletes() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "episodic".to_string(),
            kind: "decision".to_string(),
            content: "Decided to use Rust.".to_string(),
            confidence: 1.0,
            utility: 0.5,
            importance: 0.9,
            provenance_uri: None,
            trust_class: "human_explicit".to_string(),
            trust_subclass: None,
            tags: vec![],
        };

        connection.insert_memory("mem_00000000000000000000000003", &input)?;

        let before = connection.list_memories("wsp_01234567890123456789012345", None, false)?;
        ensure_equal(&before.len(), &1, "before tombstone: 1 memory")?;

        let affected = connection.tombstone_memory("mem_00000000000000000000000003")?;
        ensure(affected, "tombstone must affect a row")?;

        let after = connection.list_memories("wsp_01234567890123456789012345", None, false)?;
        ensure_equal(&after.len(), &0, "after tombstone: 0 non-tombstoned")?;

        let with_tombstoned =
            connection.list_memories("wsp_01234567890123456789012345", None, true)?;
        ensure_equal(&with_tombstoned.len(), &1, "include tombstoned: 1 memory")?;
        ensure(
            with_tombstoned[0].tombstoned_at.is_some(),
            "memory has tombstoned_at",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn add_and_remove_tags() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "working".to_string(),
            kind: "command".to_string(),
            content: "cargo test".to_string(),
            confidence: 0.5,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["initial".to_string()],
        };

        connection.insert_memory("mem_00000000000000000000000004", &input)?;

        connection.add_memory_tags(
            "mem_00000000000000000000000004",
            &["added".to_string(), "initial".to_string()],
        )?;

        let tags = connection.get_memory_tags("mem_00000000000000000000000004")?;
        ensure_equal(&tags.len(), &2, "2 unique tags after add")?;
        ensure(tags.contains(&"initial".to_string()), "has initial")?;
        ensure(tags.contains(&"added".to_string()), "has added")?;

        connection
            .remove_memory_tags("mem_00000000000000000000000004", &["initial".to_string()])?;

        let tags_after = connection.get_memory_tags("mem_00000000000000000000000004")?;
        ensure_equal(
            &tags_after,
            &vec!["added".to_string()],
            "only added remains",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn insert_and_get_workspace() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let input = super::CreateWorkspaceInput {
            path: "/home/user/projects/test".to_string(),
            name: Some("Test Project".to_string()),
        };

        connection.insert_workspace("wsp_01234567890123456789012345", &input)?;

        let workspace = connection.get_workspace("wsp_01234567890123456789012345")?;
        ensure(workspace.is_some(), "workspace must be found")?;

        let workspace = workspace.ok_or_else(|| TestFailure::new("workspace not found"))?;
        ensure_equal(
            &workspace.id.as_str(),
            &"wsp_01234567890123456789012345",
            "id",
        )?;
        ensure_equal(
            &workspace.path.as_str(),
            &"/home/user/projects/test",
            "path",
        )?;
        ensure_equal(&workspace.name, &Some("Test Project".to_string()), "name")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn get_workspace_by_path() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let input = super::CreateWorkspaceInput {
            path: "/home/user/projects/by-path".to_string(),
            name: None,
        };

        connection.insert_workspace("wsp_bypath00000000000000000000", &input)?;

        let workspace = connection.get_workspace_by_path("/home/user/projects/by-path")?;
        ensure(workspace.is_some(), "workspace must be found by path")?;

        let workspace = workspace.ok_or_else(|| TestFailure::new("workspace not found"))?;
        ensure_equal(
            &workspace.id.as_str(),
            &"wsp_bypath00000000000000000000",
            "id matches",
        )?;
        ensure(workspace.name.is_none(), "name is None")?;

        let not_found = connection.get_workspace_by_path("/nonexistent")?;
        ensure(not_found.is_none(), "nonexistent path returns None")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_workspaces_ordered_by_path() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let ws1 = super::CreateWorkspaceInput {
            path: "/z/last".to_string(),
            name: Some("Last".to_string()),
        };
        let ws2 = super::CreateWorkspaceInput {
            path: "/a/first".to_string(),
            name: Some("First".to_string()),
        };

        connection.insert_workspace("wsp_zzzzzzzzzzzzzzzzzzzzzzzzzz", &ws1)?;
        connection.insert_workspace("wsp_aaaaaaaaaaaaaaaaaaaaaaaaaa", &ws2)?;

        let workspaces = connection.list_workspaces()?;
        ensure_equal(&workspaces.len(), &2, "two workspaces")?;
        ensure_equal(
            &workspaces[0].path.as_str(),
            &"/a/first",
            "first by path order",
        )?;
        ensure_equal(
            &workspaces[1].path.as_str(),
            &"/z/last",
            "second by path order",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn update_workspace_name() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let input = super::CreateWorkspaceInput {
            path: "/home/user/update-name".to_string(),
            name: None,
        };

        connection.insert_workspace("wsp_update00000000000000000000", &input)?;

        let before = connection.get_workspace("wsp_update00000000000000000000")?;
        ensure(before.is_some(), "workspace exists")?;
        ensure(
            before.expect("checked above").name.is_none(),
            "name is None before update",
        )?;

        let affected = connection
            .update_workspace_name("wsp_update00000000000000000000", Some("Updated Name"))?;
        ensure(affected, "update affected a row")?;

        let after = connection.get_workspace("wsp_update00000000000000000000")?;
        ensure(after.is_some(), "workspace still exists")?;
        ensure_equal(
            &after.expect("checked above").name,
            &Some("Updated Name".to_string()),
            "name updated",
        )?;

        let cleared = connection.update_workspace_name("wsp_update00000000000000000000", None)?;
        ensure(cleared, "clear affected a row")?;

        let final_state = connection.get_workspace("wsp_update00000000000000000000")?;
        ensure(final_state.is_some(), "workspace still exists")?;
        ensure(
            final_state.expect("checked above").name.is_none(),
            "name cleared to None",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn workspace_path_uniqueness_enforced() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let input = super::CreateWorkspaceInput {
            path: "/unique/path".to_string(),
            name: None,
        };

        connection.insert_workspace("wsp_unique00000000000000000000", &input)?;

        let duplicate = super::CreateWorkspaceInput {
            path: "/unique/path".to_string(),
            name: Some("Different Name".to_string()),
        };

        let result = connection.insert_workspace("wsp_dup0000000000000000000000", &duplicate);
        ensure(result.is_err(), "duplicate path must be rejected")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn get_nonexistent_workspace_returns_none() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let workspace = connection.get_workspace("wsp_nonexistent00000000000000")?;
        ensure(workspace.is_none(), "nonexistent workspace must be None")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn insert_and_get_audit() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateAuditInput {
            workspace_id: Some("wsp_01234567890123456789012345".to_string()),
            actor: Some("human:jeff".to_string()),
            action: "memory.create".to_string(),
            target_type: Some("memory".to_string()),
            target_id: Some("mem_01234567890123456789012345".to_string()),
            details: Some(r#"{"kind":"rule"}"#.to_string()),
        };

        connection.insert_audit("audit_01234567890123456789012345", &input)?;

        let audit = connection.get_audit("audit_01234567890123456789012345")?;
        ensure(audit.is_some(), "audit entry must be found")?;

        let audit = audit.ok_or_else(|| TestFailure::new("audit not found"))?;
        ensure_equal(
            &audit.id.as_str(),
            &"audit_01234567890123456789012345",
            "id",
        )?;
        ensure_equal(
            &audit.workspace_id,
            &Some("wsp_01234567890123456789012345".to_string()),
            "workspace_id",
        )?;
        ensure_equal(&audit.actor, &Some("human:jeff".to_string()), "actor")?;
        ensure_equal(&audit.action.as_str(), &"memory.create", "action")?;
        ensure_equal(
            &audit.target_type,
            &Some("memory".to_string()),
            "target_type",
        )?;
        ensure_equal(
            &audit.target_id,
            &Some("mem_01234567890123456789012345".to_string()),
            "target_id",
        )?;
        ensure_equal(
            &audit.details,
            &Some(r#"{"kind":"rule"}"#.to_string()),
            "details",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn get_nonexistent_audit_returns_none() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let audit = connection.get_audit("audit_nonexistent000000000000000")?;
        ensure(audit.is_none(), "nonexistent audit must be None")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_audit_entries_by_workspace() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let entry1 = super::CreateAuditInput {
            workspace_id: Some("wsp_01234567890123456789012345".to_string()),
            actor: None,
            action: "memory.create".to_string(),
            target_type: None,
            target_id: None,
            details: None,
        };
        let entry2 = super::CreateAuditInput {
            workspace_id: Some("wsp_01234567890123456789012345".to_string()),
            actor: None,
            action: "memory.update".to_string(),
            target_type: None,
            target_id: None,
            details: None,
        };

        connection.insert_audit("audit_aaaaaaaaaaaaaaaaaaaaaaaaaa", &entry1)?;
        connection.insert_audit("audit_bbbbbbbbbbbbbbbbbbbbbbbbbb", &entry2)?;

        let entries =
            connection.list_audit_entries(Some("wsp_01234567890123456789012345"), None)?;
        ensure_equal(&entries.len(), &2, "two entries for workspace")?;

        let limited =
            connection.list_audit_entries(Some("wsp_01234567890123456789012345"), Some(1))?;
        ensure_equal(&limited.len(), &1, "limited to 1")?;

        let all = connection.list_audit_entries(None, None)?;
        ensure_equal(&all.len(), &2, "all entries")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_audit_by_target() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let entry1 = super::CreateAuditInput {
            workspace_id: Some("wsp_01234567890123456789012345".to_string()),
            actor: None,
            action: "memory.create".to_string(),
            target_type: Some("memory".to_string()),
            target_id: Some("mem_target00000000000000000001".to_string()),
            details: None,
        };
        let entry2 = super::CreateAuditInput {
            workspace_id: Some("wsp_01234567890123456789012345".to_string()),
            actor: None,
            action: "memory.update".to_string(),
            target_type: Some("memory".to_string()),
            target_id: Some("mem_target00000000000000000001".to_string()),
            details: None,
        };
        let entry3 = super::CreateAuditInput {
            workspace_id: Some("wsp_01234567890123456789012345".to_string()),
            actor: None,
            action: "workspace.create".to_string(),
            target_type: Some("workspace".to_string()),
            target_id: Some("wsp_01234567890123456789012345".to_string()),
            details: None,
        };

        connection.insert_audit("audit_target00000000000000000001", &entry1)?;
        connection.insert_audit("audit_target00000000000000000002", &entry2)?;
        connection.insert_audit("audit_target00000000000000000003", &entry3)?;

        let memory_entries =
            connection.list_audit_by_target("memory", "mem_target00000000000000000001", None)?;
        ensure_equal(&memory_entries.len(), &2, "two entries for memory target")?;

        let workspace_entries =
            connection.list_audit_by_target("workspace", "wsp_01234567890123456789012345", None)?;
        ensure_equal(
            &workspace_entries.len(),
            &1,
            "one entry for workspace target",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_audit_by_action() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let create = super::CreateAuditInput {
            workspace_id: Some("wsp_01234567890123456789012345".to_string()),
            actor: None,
            action: "memory.create".to_string(),
            target_type: None,
            target_id: None,
            details: None,
        };
        let update = super::CreateAuditInput {
            workspace_id: Some("wsp_01234567890123456789012345".to_string()),
            actor: None,
            action: "memory.update".to_string(),
            target_type: None,
            target_id: None,
            details: None,
        };

        connection.insert_audit("audit_action00000000000000000001", &create)?;
        connection.insert_audit("audit_action00000000000000000002", &create)?;
        connection.insert_audit("audit_action00000000000000000003", &update)?;

        let create_entries = connection.list_audit_by_action("memory.create", None)?;
        ensure_equal(&create_entries.len(), &2, "two create entries")?;

        let update_entries = connection.list_audit_by_action("memory.update", None)?;
        ensure_equal(&update_entries.len(), &1, "one update entry")?;

        let limited = connection.list_audit_by_action("memory.create", Some(1))?;
        ensure_equal(&limited.len(), &1, "limited to 1")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn audit_with_null_workspace() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let input = super::CreateAuditInput {
            workspace_id: None,
            actor: Some("system".to_string()),
            action: "global.init".to_string(),
            target_type: None,
            target_id: None,
            details: None,
        };

        connection.insert_audit("audit_nullws00000000000000000000", &input)?;

        let audit = connection.get_audit("audit_nullws00000000000000000000")?;
        ensure(audit.is_some(), "audit with null workspace must be found")?;

        let audit = audit.ok_or_else(|| TestFailure::new("audit not found"))?;
        ensure(audit.workspace_id.is_none(), "workspace_id is None")?;
        ensure_equal(&audit.action.as_str(), &"global.init", "action")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn transaction_commit_persists_changes() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        connection.begin()?;

        let input = super::CreateWorkspaceInput {
            path: "/tmp/txn-commit".to_string(),
            name: Some("Transaction Test".to_string()),
        };
        connection.insert_workspace("wsp_txncommit00000000000000000", &input)?;

        connection.commit()?;

        let workspace = connection.get_workspace("wsp_txncommit00000000000000000")?;
        ensure(workspace.is_some(), "committed workspace must persist")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn transaction_rollback_discards_changes() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        connection.begin()?;

        let input = super::CreateWorkspaceInput {
            path: "/tmp/txn-rollback".to_string(),
            name: Some("Rollback Test".to_string()),
        };
        connection.insert_workspace("wsp_txnrollback000000000000000", &input)?;

        connection.rollback()?;

        let workspace = connection.get_workspace("wsp_txnrollback000000000000000")?;
        ensure(workspace.is_none(), "rolled back workspace must not exist")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn with_transaction_commits_on_success() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let result = connection.with_transaction(|| {
            let input = super::CreateWorkspaceInput {
                path: "/tmp/with-txn-ok".to_string(),
                name: Some("With Transaction OK".to_string()),
            };
            connection.insert_workspace("wsp_withtxnok00000000000000000", &input)?;
            Ok("success")
        })?;

        ensure_equal(&result, &"success", "transaction returned success")?;

        let workspace = connection.get_workspace("wsp_withtxnok00000000000000000")?;
        ensure(workspace.is_some(), "workspace persisted after success")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn with_transaction_rollbacks_on_error() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let input = super::CreateWorkspaceInput {
            path: "/tmp/with-txn-err".to_string(),
            name: Some("With Transaction Err".to_string()),
        };
        connection.insert_workspace("wsp_withtxnerr0000000000000000", &input)?;

        let result: std::result::Result<(), _> = connection.with_transaction(|| {
            let duplicate = super::CreateWorkspaceInput {
                path: "/tmp/with-txn-err".to_string(),
                name: Some("Duplicate".to_string()),
            };
            connection.insert_workspace("wsp_withtxnerr0000000000000001", &duplicate)?;
            Ok(())
        });

        ensure(result.is_err(), "transaction failed on duplicate path")?;

        let workspace = connection.get_workspace("wsp_withtxnerr0000000000000001")?;
        ensure(workspace.is_none(), "failed insert was rolled back")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn begin_transaction_with_isolation_levels() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        connection.begin_transaction(sqlmodel_core::IsolationLevel::ReadCommitted)?;
        connection.rollback()?;

        connection.begin_transaction(sqlmodel_core::IsolationLevel::RepeatableRead)?;
        connection.rollback()?;

        connection.begin_transaction(sqlmodel_core::IsolationLevel::Serializable)?;
        connection.rollback()?;

        connection.close()?;
        Ok(())
    }
}
