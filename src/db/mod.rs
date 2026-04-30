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

    /// Run SQLite PRAGMA integrity_check and return results.
    pub fn check_integrity(&self) -> Result<IntegrityCheckResult> {
        let rows = self.query_for(DbOperation::IntegrityCheck, "PRAGMA integrity_check", &[])?;

        let mut issues = Vec::new();
        for row in &rows {
            if let Some(msg) = row.get(0).and_then(|v| v.as_str()) {
                if msg != "ok" {
                    issues.push(msg.to_string());
                }
            }
        }

        Ok(IntegrityCheckResult {
            passed: issues.is_empty(),
            issues,
        })
    }

    /// Run SQLite PRAGMA foreign_key_check and return violations.
    pub fn check_foreign_keys(&self) -> Result<ForeignKeyCheckResult> {
        let rows = self.query_for(
            DbOperation::ForeignKeyCheck,
            "PRAGMA foreign_key_check",
            &[],
        )?;

        let mut violations = Vec::new();
        for row in &rows {
            let table = row
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let rowid = row.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
            let parent = row
                .get(2)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let fkid = row.get(3).and_then(|v| v.as_i64()).unwrap_or(0) as u32;

            violations.push(ForeignKeyViolation {
                table,
                rowid,
                parent,
                fkid,
            });
        }

        Ok(ForeignKeyCheckResult {
            passed: violations.is_empty(),
            violations,
        })
    }

    /// Run a full database integrity report.
    pub fn integrity_report(&self) -> Result<IntegrityReport> {
        let integrity = self.check_integrity()?;
        let foreign_keys = self.check_foreign_keys()?;
        let schema_version = self.schema_version()?;
        let needs_migration = self.needs_migration()?;

        Ok(IntegrityReport {
            integrity_check: integrity,
            foreign_key_check: foreign_keys,
            schema_version,
            needs_migration,
        })
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

/// Result of PRAGMA integrity_check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityCheckResult {
    pub passed: bool,
    pub issues: Vec<String>,
}

/// A foreign key violation found by PRAGMA foreign_key_check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyViolation {
    pub table: String,
    pub rowid: i64,
    pub parent: String,
    pub fkid: u32,
}

/// Result of PRAGMA foreign_key_check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyCheckResult {
    pub passed: bool,
    pub violations: Vec<ForeignKeyViolation>,
}

/// Full database integrity report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    pub integrity_check: IntegrityCheckResult,
    pub foreign_key_check: ForeignKeyCheckResult,
    pub schema_version: Option<u32>,
    pub needs_migration: bool,
}

impl IntegrityReport {
    /// Returns true if the database passes all integrity checks.
    pub fn is_healthy(&self) -> bool {
        self.integrity_check.passed && self.foreign_key_check.passed && !self.needs_migration
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
    IntegrityCheck,
    ForeignKeyCheck,
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
            Self::IntegrityCheck => f.write_str("integrity check"),
            Self::ForeignKeyCheck => f.write_str("foreign key check"),
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

fn required_u32(row: &Row, index: usize, operation: DbOperation, column: &str) -> Result<u32> {
    let value = required_i64(row, index, operation, column)?;
    u32::try_from(value).map_err(|_| DbError::MalformedRow {
        operation,
        message: format!("{column} column at index {index} must fit u32"),
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

/// V006: Add pack_records table (EE-142).
pub const V006_PACK_RECORDS: Migration = Migration::new(
    6,
    "pack_records",
    r#"
-- Pack records table (EE-142)
-- Stores persisted context packs for audit, inspection, and ee why support.
CREATE TABLE pack_records (
    id TEXT PRIMARY KEY CHECK (id GLOB 'pack_*' AND length(id) = 31),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    query TEXT NOT NULL CHECK (length(trim(query)) > 0),
    profile TEXT NOT NULL CHECK (profile IN ('compact', 'balanced', 'thorough')),
    max_tokens INTEGER NOT NULL CHECK (max_tokens > 0),
    used_tokens INTEGER NOT NULL CHECK (used_tokens >= 0 AND used_tokens <= max_tokens),
    item_count INTEGER NOT NULL CHECK (item_count >= 0),
    omitted_count INTEGER NOT NULL CHECK (omitted_count >= 0),
    pack_hash TEXT NOT NULL CHECK (length(trim(pack_hash)) > 0),
    degraded_json TEXT CHECK (degraded_json IS NULL OR json_valid(degraded_json)),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    created_by TEXT CHECK (created_by IS NULL OR length(trim(created_by)) > 0)
);

CREATE INDEX idx_pack_records_workspace ON pack_records(workspace_id);
CREATE INDEX idx_pack_records_created ON pack_records(created_at);
CREATE INDEX idx_pack_records_hash ON pack_records(pack_hash);

-- Pack items junction (many-to-many, ordered by rank)
CREATE TABLE pack_items (
    pack_id TEXT NOT NULL REFERENCES pack_records(id) ON DELETE CASCADE,
    memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    rank INTEGER NOT NULL CHECK (rank > 0),
    section TEXT NOT NULL CHECK (section IN (
        'procedural_rules', 'decisions', 'failures', 'evidence', 'artifacts'
    )),
    estimated_tokens INTEGER NOT NULL CHECK (estimated_tokens > 0),
    relevance REAL NOT NULL CHECK (relevance >= 0.0 AND relevance <= 1.0),
    utility REAL NOT NULL CHECK (utility >= 0.0 AND utility <= 1.0),
    why TEXT NOT NULL CHECK (length(trim(why)) > 0),
    diversity_key TEXT CHECK (diversity_key IS NULL OR length(trim(diversity_key)) > 0),
    PRIMARY KEY (pack_id, memory_id)
);

CREATE INDEX idx_pack_items_memory ON pack_items(memory_id);
CREATE INDEX idx_pack_items_section ON pack_items(section);
CREATE INDEX idx_pack_items_rank ON pack_items(pack_id, rank);

-- Pack omissions (tracks what was left out and why)
CREATE TABLE pack_omissions (
    pack_id TEXT NOT NULL REFERENCES pack_records(id) ON DELETE CASCADE,
    memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    estimated_tokens INTEGER NOT NULL CHECK (estimated_tokens > 0),
    reason TEXT NOT NULL CHECK (reason IN ('token_budget_exceeded')),
    PRIMARY KEY (pack_id, memory_id)
);

CREATE INDEX idx_pack_omissions_memory ON pack_omissions(memory_id);
"#,
    "blake3:v006_pack_records_2026_04_29",
);

/// V007: Add memory_links table (EE-162).
pub const V007_MEMORY_LINKS: Migration = Migration::new(
    7,
    "memory_links",
    r#"
-- Memory links table (EE-162)
-- Durable typed graph edges between memories. Graph projections derive from
-- these records and can be rebuilt through FrankenNetworkX.
CREATE TABLE memory_links (
    id TEXT PRIMARY KEY CHECK (id GLOB 'link_*' AND length(id) = 31),
    src_memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    dst_memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    relation TEXT NOT NULL CHECK (relation IN (
        'supports', 'contradicts', 'derived_from', 'supersedes', 'related', 'co_tag', 'co_mention'
    )),
    weight REAL NOT NULL DEFAULT 1.0 CHECK (weight >= 0.0 AND weight <= 1.0),
    confidence REAL NOT NULL DEFAULT 1.0 CHECK (confidence >= 0.0 AND confidence <= 1.0),
    directed INTEGER NOT NULL DEFAULT 1 CHECK (directed IN (0, 1)),
    evidence_count INTEGER NOT NULL DEFAULT 1 CHECK (evidence_count >= 0),
    last_reinforced_at TEXT CHECK (last_reinforced_at IS NULL OR length(trim(last_reinforced_at)) > 0),
    source TEXT NOT NULL DEFAULT 'agent' CHECK (source IN ('agent', 'auto', 'import', 'maintenance', 'human')),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    created_by TEXT CHECK (created_by IS NULL OR length(trim(created_by)) > 0),
    metadata_json TEXT CHECK (metadata_json IS NULL OR json_valid(metadata_json)),
    CHECK (src_memory_id <> dst_memory_id),
    UNIQUE (src_memory_id, dst_memory_id, relation)
);

CREATE INDEX idx_memory_links_src ON memory_links(src_memory_id);
CREATE INDEX idx_memory_links_dst ON memory_links(dst_memory_id);
CREATE INDEX idx_memory_links_relation ON memory_links(relation);
CREATE INDEX idx_memory_links_source ON memory_links(source);
CREATE INDEX idx_memory_links_created ON memory_links(created_at);
"#,
    "blake3:v007_memory_links_2026_04_29",
);

/// V008: Add sessions table (EE-103).
pub const V008_SESSIONS: Migration = Migration::new(
    8,
    "sessions",
    r#"
-- CASS sessions imported through the stable robot/JSON contract.
CREATE TABLE sessions (
    id TEXT PRIMARY KEY CHECK (id GLOB 'sess_*' AND length(id) = 31),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    cass_session_id TEXT NOT NULL CHECK (length(trim(cass_session_id)) > 0),
    source_path TEXT CHECK (source_path IS NULL OR length(trim(source_path)) > 0),
    agent_name TEXT CHECK (agent_name IS NULL OR length(trim(agent_name)) > 0),
    model TEXT CHECK (model IS NULL OR length(trim(model)) > 0),
    started_at TEXT CHECK (started_at IS NULL OR length(trim(started_at)) > 0),
    ended_at TEXT CHECK (ended_at IS NULL OR length(trim(ended_at)) > 0),
    message_count INTEGER NOT NULL DEFAULT 0 CHECK (message_count >= 0),
    token_count INTEGER CHECK (token_count IS NULL OR token_count >= 0),
    content_hash TEXT NOT NULL CHECK (length(trim(content_hash)) > 0),
    metadata_json TEXT CHECK (metadata_json IS NULL OR json_valid(metadata_json)),
    imported_at TEXT NOT NULL CHECK (length(trim(imported_at)) > 0),
    updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
    UNIQUE (workspace_id, cass_session_id)
);

CREATE INDEX idx_sessions_workspace ON sessions(workspace_id);
CREATE INDEX idx_sessions_cass_id ON sessions(cass_session_id);
CREATE INDEX idx_sessions_started ON sessions(started_at);
CREATE INDEX idx_sessions_content_hash ON sessions(content_hash);
"#,
    "blake3:v008_sessions_2026_04_30",
);

/// V009: Add evidence_spans table (EE-104).
pub const V009_EVIDENCE_SPANS: Migration = Migration::new(
    9,
    "evidence_spans",
    r#"
-- Evidence spans imported from CASS session transcripts.
CREATE TABLE evidence_spans (
    id TEXT PRIMARY KEY CHECK (id GLOB 'ev_*' AND length(id) = 29),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    memory_id TEXT REFERENCES memories(id) ON DELETE SET NULL,
    cass_span_id TEXT NOT NULL CHECK (length(trim(cass_span_id)) > 0),
    span_kind TEXT NOT NULL CHECK (span_kind IN (
        'message', 'tool_call', 'tool_result', 'file', 'summary'
    )),
    start_line INTEGER NOT NULL CHECK (start_line > 0),
    end_line INTEGER NOT NULL CHECK (end_line >= start_line),
    start_byte INTEGER CHECK (start_byte IS NULL OR start_byte >= 0),
    end_byte INTEGER CHECK (end_byte IS NULL OR (
        end_byte >= 0 AND (start_byte IS NULL OR end_byte >= start_byte)
    )),
    role TEXT CHECK (role IS NULL OR length(trim(role)) > 0),
    excerpt TEXT NOT NULL CHECK (length(trim(excerpt)) > 0 AND length(excerpt) <= 65536),
    content_hash TEXT NOT NULL CHECK (length(trim(content_hash)) > 0),
    metadata_json TEXT CHECK (metadata_json IS NULL OR json_valid(metadata_json)),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
    UNIQUE (session_id, cass_span_id)
);

CREATE INDEX idx_evidence_spans_workspace ON evidence_spans(workspace_id);
CREATE INDEX idx_evidence_spans_session ON evidence_spans(session_id);
CREATE INDEX idx_evidence_spans_memory ON evidence_spans(memory_id) WHERE memory_id IS NOT NULL;
CREATE INDEX idx_evidence_spans_kind ON evidence_spans(span_kind);
CREATE INDEX idx_evidence_spans_content_hash ON evidence_spans(content_hash);
"#,
    "blake3:v009_evidence_spans_2026_04_30",
);

/// V010: Add import_ledger table (EE-105).
pub const V010_IMPORT_LEDGER: Migration = Migration::new(
    10,
    "import_ledger",
    r#"
-- Resumable import ledger for CASS robot/JSON imports.
CREATE TABLE import_ledger (
    id TEXT PRIMARY KEY CHECK (id GLOB 'imp_*' AND length(id) = 30),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('cass')),
    source_id TEXT NOT NULL CHECK (length(trim(source_id)) > 0),
    status TEXT NOT NULL CHECK (status IN (
        'pending', 'running', 'completed', 'failed', 'skipped'
    )),
    cursor_json TEXT CHECK (cursor_json IS NULL OR json_valid(cursor_json)),
    imported_session_count INTEGER NOT NULL DEFAULT 0 CHECK (imported_session_count >= 0),
    imported_span_count INTEGER NOT NULL DEFAULT 0 CHECK (imported_span_count >= 0),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    error_code TEXT CHECK (error_code IS NULL OR length(trim(error_code)) > 0),
    error_message TEXT CHECK (error_message IS NULL OR length(trim(error_message)) > 0),
    started_at TEXT CHECK (started_at IS NULL OR length(trim(started_at)) > 0),
    completed_at TEXT CHECK (completed_at IS NULL OR length(trim(completed_at)) > 0),
    metadata_json TEXT CHECK (metadata_json IS NULL OR json_valid(metadata_json)),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
    UNIQUE (workspace_id, source_kind, source_id),
    CHECK (
        (status = 'completed' AND completed_at IS NOT NULL)
        OR status <> 'completed'
    )
);

CREATE INDEX idx_import_ledger_workspace ON import_ledger(workspace_id);
CREATE INDEX idx_import_ledger_source ON import_ledger(source_kind, source_id);
CREATE INDEX idx_import_ledger_status ON import_ledger(status);
CREATE INDEX idx_import_ledger_updated ON import_ledger(updated_at);
"#,
    "blake3:v010_import_ledger_2026_04_30",
);

/// V011: Add feedback_events table (EE-080).
pub const V011_FEEDBACK_EVENTS: Migration = Migration::new(
    11,
    "feedback_events",
    r#"
-- Feedback events table (EE-080)
-- Captures positive/negative feedback signals with evidence for scoring memories and rules.
CREATE TABLE feedback_events (
    id TEXT PRIMARY KEY CHECK (id GLOB 'fb_*' AND length(id) = 29),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    target_type TEXT NOT NULL CHECK (target_type IN (
        'memory', 'rule', 'session', 'source', 'pack', 'candidate'
    )),
    target_id TEXT NOT NULL CHECK (length(trim(target_id)) > 0),
    signal TEXT NOT NULL CHECK (signal IN (
        'positive', 'negative', 'neutral', 'contradiction', 'confirmation',
        'harmful', 'helpful', 'stale', 'inaccurate', 'outdated'
    )),
    weight REAL NOT NULL DEFAULT 1.0 CHECK (weight >= 0.0 AND weight <= 10.0),
    source_type TEXT NOT NULL CHECK (source_type IN (
        'human_explicit', 'agent_inference', 'automated_check', 'outcome_observed',
        'contradiction_detected', 'usage_pattern', 'decay_trigger'
    )),
    source_id TEXT CHECK (source_id IS NULL OR length(trim(source_id)) > 0),
    reason TEXT CHECK (reason IS NULL OR length(trim(reason)) > 0),
    evidence_json TEXT CHECK (evidence_json IS NULL OR json_valid(evidence_json)),
    session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    applied_at TEXT CHECK (applied_at IS NULL OR length(trim(applied_at)) > 0),
    created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0)
);

CREATE INDEX idx_feedback_events_workspace ON feedback_events(workspace_id);
CREATE INDEX idx_feedback_events_target ON feedback_events(target_type, target_id);
CREATE INDEX idx_feedback_events_signal ON feedback_events(signal);
CREATE INDEX idx_feedback_events_source ON feedback_events(source_type);
CREATE INDEX idx_feedback_events_session ON feedback_events(session_id) WHERE session_id IS NOT NULL;
CREATE INDEX idx_feedback_events_created ON feedback_events(created_at);
CREATE INDEX idx_feedback_events_applied ON feedback_events(applied_at) WHERE applied_at IS NOT NULL;
"#,
    "blake3:v011_feedback_events_2026_04_30",
);

/// All migrations in version order.
pub const MIGRATIONS: &[Migration] = &[
    V001_INIT_SCHEMA,
    V002_TRUST_CLASS,
    V003_CURATION_CANDIDATES,
    V004_PROCEDURAL_RULES,
    V005_SEARCH_INDEX_JOBS,
    V006_PACK_RECORDS,
    V007_MEMORY_LINKS,
    V008_SESSIONS,
    V009_EVIDENCE_SPANS,
    V010_IMPORT_LEDGER,
    V011_FEEDBACK_EVENTS,
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

/// Input for recording a CASS session import row.
#[derive(Debug, Clone)]
pub struct CreateSessionInput {
    pub workspace_id: String,
    pub cass_session_id: String,
    pub source_path: Option<String>,
    pub agent_name: Option<String>,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub message_count: u32,
    pub token_count: Option<u32>,
    pub content_hash: String,
    pub metadata_json: Option<String>,
}

/// A stored CASS session row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSession {
    pub id: String,
    pub workspace_id: String,
    pub cass_session_id: String,
    pub source_path: Option<String>,
    pub agent_name: Option<String>,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub message_count: u32,
    pub token_count: Option<u32>,
    pub content_hash: String,
    pub metadata_json: Option<String>,
    pub imported_at: String,
    pub updated_at: String,
}

impl DbConnection {
    /// Insert a new CASS session row.
    pub fn insert_session(&self, id: &str, input: &CreateSessionInput) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO sessions (id, workspace_id, cass_session_id, source_path, agent_name, model, started_at, ended_at, message_count, token_count, content_hash, metadata_json, imported_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            &[
                Value::Text(id.to_string()),
                Value::Text(input.workspace_id.clone()),
                Value::Text(input.cass_session_id.clone()),
                input
                    .source_path
                    .as_ref()
                    .map_or(Value::Null, |path| Value::Text(path.clone())),
                input
                    .agent_name
                    .as_ref()
                    .map_or(Value::Null, |agent| Value::Text(agent.clone())),
                input
                    .model
                    .as_ref()
                    .map_or(Value::Null, |model| Value::Text(model.clone())),
                input
                    .started_at
                    .as_ref()
                    .map_or(Value::Null, |started| Value::Text(started.clone())),
                input
                    .ended_at
                    .as_ref()
                    .map_or(Value::Null, |ended| Value::Text(ended.clone())),
                Value::BigInt(i64::from(input.message_count)),
                input
                    .token_count
                    .map_or(Value::Null, |count| Value::BigInt(i64::from(count))),
                Value::Text(input.content_hash.clone()),
                input
                    .metadata_json
                    .as_ref()
                    .map_or(Value::Null, |metadata| Value::Text(metadata.clone())),
                Value::Text(now.clone()),
                Value::Text(now),
            ],
        )?;

        Ok(())
    }

    /// Get a CASS session by its internal ee session ID.
    pub fn get_session(&self, id: &str) -> Result<Option<StoredSession>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, cass_session_id, source_path, agent_name, model, started_at, ended_at, message_count, token_count, content_hash, metadata_json, imported_at, updated_at FROM sessions WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first().map(stored_session_from_row).transpose()
    }

    /// Get a CASS session by the upstream CASS session identifier.
    pub fn get_session_by_cass_id(
        &self,
        workspace_id: &str,
        cass_session_id: &str,
    ) -> Result<Option<StoredSession>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, cass_session_id, source_path, agent_name, model, started_at, ended_at, message_count, token_count, content_hash, metadata_json, imported_at, updated_at FROM sessions WHERE workspace_id = ?1 AND cass_session_id = ?2",
            &[
                Value::Text(workspace_id.to_string()),
                Value::Text(cass_session_id.to_string()),
            ],
        )?;

        rows.first().map(stored_session_from_row).transpose()
    }

    /// List CASS sessions for a workspace in stable upstream-id order.
    pub fn list_sessions(&self, workspace_id: &str) -> Result<Vec<StoredSession>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, cass_session_id, source_path, agent_name, model, started_at, ended_at, message_count, token_count, content_hash, metadata_json, imported_at, updated_at FROM sessions WHERE workspace_id = ?1 ORDER BY cass_session_id ASC, id ASC",
            &[Value::Text(workspace_id.to_string())],
        )?;

        rows.iter().map(stored_session_from_row).collect()
    }
}

fn stored_session_from_row(row: &Row) -> Result<StoredSession> {
    Ok(StoredSession {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        workspace_id: required_text(row, 1, DbOperation::Query, "workspace_id")?.to_string(),
        cass_session_id: required_text(row, 2, DbOperation::Query, "cass_session_id")?.to_string(),
        source_path: optional_text(row, 3)?.map(str::to_string),
        agent_name: optional_text(row, 4)?.map(str::to_string),
        model: optional_text(row, 5)?.map(str::to_string),
        started_at: optional_text(row, 6)?.map(str::to_string),
        ended_at: optional_text(row, 7)?.map(str::to_string),
        message_count: required_u32(row, 8, DbOperation::Query, "message_count")?,
        token_count: optional_u32(row, 9, DbOperation::Query, "token_count")?,
        content_hash: required_text(row, 10, DbOperation::Query, "content_hash")?.to_string(),
        metadata_json: optional_text(row, 11)?.map(str::to_string),
        imported_at: required_text(row, 12, DbOperation::Query, "imported_at")?.to_string(),
        updated_at: required_text(row, 13, DbOperation::Query, "updated_at")?.to_string(),
    })
}

/// Input for recording a CASS evidence span.
#[derive(Debug, Clone)]
pub struct CreateEvidenceSpanInput {
    pub workspace_id: String,
    pub session_id: String,
    pub memory_id: Option<String>,
    pub cass_span_id: String,
    pub span_kind: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: Option<u32>,
    pub end_byte: Option<u32>,
    pub role: Option<String>,
    pub excerpt: String,
    pub content_hash: String,
    pub metadata_json: Option<String>,
}

/// A stored evidence_spans row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvidenceSpan {
    pub id: String,
    pub workspace_id: String,
    pub session_id: String,
    pub memory_id: Option<String>,
    pub cass_span_id: String,
    pub span_kind: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: Option<u32>,
    pub end_byte: Option<u32>,
    pub role: Option<String>,
    pub excerpt: String,
    pub content_hash: String,
    pub metadata_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl DbConnection {
    /// Insert a CASS evidence span row.
    pub fn insert_evidence_span(&self, id: &str, input: &CreateEvidenceSpanInput) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO evidence_spans (id, workspace_id, session_id, memory_id, cass_span_id, span_kind, start_line, end_line, start_byte, end_byte, role, excerpt, content_hash, metadata_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            &[
                Value::Text(id.to_string()),
                Value::Text(input.workspace_id.clone()),
                Value::Text(input.session_id.clone()),
                input
                    .memory_id
                    .as_ref()
                    .map_or(Value::Null, |memory| Value::Text(memory.clone())),
                Value::Text(input.cass_span_id.clone()),
                Value::Text(input.span_kind.clone()),
                Value::BigInt(i64::from(input.start_line)),
                Value::BigInt(i64::from(input.end_line)),
                input
                    .start_byte
                    .map_or(Value::Null, |offset| Value::BigInt(i64::from(offset))),
                input
                    .end_byte
                    .map_or(Value::Null, |offset| Value::BigInt(i64::from(offset))),
                input
                    .role
                    .as_ref()
                    .map_or(Value::Null, |role| Value::Text(role.clone())),
                Value::Text(input.excerpt.clone()),
                Value::Text(input.content_hash.clone()),
                input
                    .metadata_json
                    .as_ref()
                    .map_or(Value::Null, |metadata| Value::Text(metadata.clone())),
                Value::Text(now.clone()),
                Value::Text(now),
            ],
        )?;

        Ok(())
    }

    /// Get an evidence span by its ee evidence ID.
    pub fn get_evidence_span(&self, id: &str) -> Result<Option<StoredEvidenceSpan>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, session_id, memory_id, cass_span_id, span_kind, start_line, end_line, start_byte, end_byte, role, excerpt, content_hash, metadata_json, created_at, updated_at FROM evidence_spans WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first().map(stored_evidence_span_from_row).transpose()
    }

    /// List evidence spans for a session in transcript order.
    pub fn list_evidence_spans_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<StoredEvidenceSpan>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, session_id, memory_id, cass_span_id, span_kind, start_line, end_line, start_byte, end_byte, role, excerpt, content_hash, metadata_json, created_at, updated_at FROM evidence_spans WHERE session_id = ?1 ORDER BY start_line ASC, end_line ASC, id ASC",
            &[Value::Text(session_id.to_string())],
        )?;

        rows.iter().map(stored_evidence_span_from_row).collect()
    }

    /// List evidence spans linked to a memory in deterministic order.
    pub fn list_evidence_spans_for_memory(
        &self,
        memory_id: &str,
    ) -> Result<Vec<StoredEvidenceSpan>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, session_id, memory_id, cass_span_id, span_kind, start_line, end_line, start_byte, end_byte, role, excerpt, content_hash, metadata_json, created_at, updated_at FROM evidence_spans WHERE memory_id = ?1 ORDER BY session_id ASC, start_line ASC, end_line ASC, id ASC",
            &[Value::Text(memory_id.to_string())],
        )?;

        rows.iter().map(stored_evidence_span_from_row).collect()
    }
}

fn stored_evidence_span_from_row(row: &Row) -> Result<StoredEvidenceSpan> {
    Ok(StoredEvidenceSpan {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        workspace_id: required_text(row, 1, DbOperation::Query, "workspace_id")?.to_string(),
        session_id: required_text(row, 2, DbOperation::Query, "session_id")?.to_string(),
        memory_id: optional_text(row, 3)?.map(str::to_string),
        cass_span_id: required_text(row, 4, DbOperation::Query, "cass_span_id")?.to_string(),
        span_kind: required_text(row, 5, DbOperation::Query, "span_kind")?.to_string(),
        start_line: required_u32(row, 6, DbOperation::Query, "start_line")?,
        end_line: required_u32(row, 7, DbOperation::Query, "end_line")?,
        start_byte: optional_u32(row, 8, DbOperation::Query, "start_byte")?,
        end_byte: optional_u32(row, 9, DbOperation::Query, "end_byte")?,
        role: optional_text(row, 10)?.map(str::to_string),
        excerpt: required_text(row, 11, DbOperation::Query, "excerpt")?.to_string(),
        content_hash: required_text(row, 12, DbOperation::Query, "content_hash")?.to_string(),
        metadata_json: optional_text(row, 13)?.map(str::to_string),
        created_at: required_text(row, 14, DbOperation::Query, "created_at")?.to_string(),
        updated_at: required_text(row, 15, DbOperation::Query, "updated_at")?.to_string(),
    })
}

/// Input for recording a resumable import ledger row.
#[derive(Debug, Clone)]
pub struct CreateImportLedgerInput {
    pub workspace_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub status: String,
    pub cursor_json: Option<String>,
    pub imported_session_count: u32,
    pub imported_span_count: u32,
    pub attempt_count: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub metadata_json: Option<String>,
}

/// Input for updating resumable import progress.
#[derive(Debug, Clone)]
pub struct UpdateImportLedgerInput {
    pub status: String,
    pub cursor_json: Option<String>,
    pub imported_session_count: u32,
    pub imported_span_count: u32,
    pub attempt_count: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// A stored import_ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImportLedger {
    pub id: String,
    pub workspace_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub status: String,
    pub cursor_json: Option<String>,
    pub imported_session_count: u32,
    pub imported_span_count: u32,
    pub attempt_count: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub metadata_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl DbConnection {
    /// Insert a resumable import ledger row.
    pub fn insert_import_ledger(&self, id: &str, input: &CreateImportLedgerInput) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO import_ledger (id, workspace_id, source_kind, source_id, status, cursor_json, imported_session_count, imported_span_count, attempt_count, error_code, error_message, started_at, completed_at, metadata_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            &[
                Value::Text(id.to_string()),
                Value::Text(input.workspace_id.clone()),
                Value::Text(input.source_kind.clone()),
                Value::Text(input.source_id.clone()),
                Value::Text(input.status.clone()),
                input
                    .cursor_json
                    .as_ref()
                    .map_or(Value::Null, |cursor| Value::Text(cursor.clone())),
                Value::BigInt(i64::from(input.imported_session_count)),
                Value::BigInt(i64::from(input.imported_span_count)),
                Value::BigInt(i64::from(input.attempt_count)),
                input
                    .error_code
                    .as_ref()
                    .map_or(Value::Null, |code| Value::Text(code.clone())),
                input
                    .error_message
                    .as_ref()
                    .map_or(Value::Null, |message| Value::Text(message.clone())),
                input
                    .started_at
                    .as_ref()
                    .map_or(Value::Null, |started| Value::Text(started.clone())),
                input
                    .completed_at
                    .as_ref()
                    .map_or(Value::Null, |completed| Value::Text(completed.clone())),
                input
                    .metadata_json
                    .as_ref()
                    .map_or(Value::Null, |metadata| Value::Text(metadata.clone())),
                Value::Text(now.clone()),
                Value::Text(now),
            ],
        )?;

        Ok(())
    }

    /// Get an import ledger row by its ee import ID.
    pub fn get_import_ledger(&self, id: &str) -> Result<Option<StoredImportLedger>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, source_kind, source_id, status, cursor_json, imported_session_count, imported_span_count, attempt_count, error_code, error_message, started_at, completed_at, metadata_json, created_at, updated_at FROM import_ledger WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first().map(stored_import_ledger_from_row).transpose()
    }

    /// Get an import ledger row by its stable upstream source key.
    pub fn get_import_ledger_by_source(
        &self,
        workspace_id: &str,
        source_kind: &str,
        source_id: &str,
    ) -> Result<Option<StoredImportLedger>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, source_kind, source_id, status, cursor_json, imported_session_count, imported_span_count, attempt_count, error_code, error_message, started_at, completed_at, metadata_json, created_at, updated_at FROM import_ledger WHERE workspace_id = ?1 AND source_kind = ?2 AND source_id = ?3",
            &[
                Value::Text(workspace_id.to_string()),
                Value::Text(source_kind.to_string()),
                Value::Text(source_id.to_string()),
            ],
        )?;

        rows.first().map(stored_import_ledger_from_row).transpose()
    }

    /// List import ledger rows for a workspace in stable resume order.
    pub fn list_import_ledgers(&self, workspace_id: &str) -> Result<Vec<StoredImportLedger>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, source_kind, source_id, status, cursor_json, imported_session_count, imported_span_count, attempt_count, error_code, error_message, started_at, completed_at, metadata_json, created_at, updated_at FROM import_ledger WHERE workspace_id = ?1 ORDER BY source_kind ASC, source_id ASC, id ASC",
            &[Value::Text(workspace_id.to_string())],
        )?;

        rows.iter().map(stored_import_ledger_from_row).collect()
    }

    /// List import ledger rows by status in deterministic order.
    pub fn list_import_ledgers_by_status(
        &self,
        workspace_id: &str,
        status: &str,
    ) -> Result<Vec<StoredImportLedger>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, source_kind, source_id, status, cursor_json, imported_session_count, imported_span_count, attempt_count, error_code, error_message, started_at, completed_at, metadata_json, created_at, updated_at FROM import_ledger WHERE workspace_id = ?1 AND status = ?2 ORDER BY source_kind ASC, source_id ASC, id ASC",
            &[
                Value::Text(workspace_id.to_string()),
                Value::Text(status.to_string()),
            ],
        )?;

        rows.iter().map(stored_import_ledger_from_row).collect()
    }

    /// Update resumable import progress for an existing ledger row.
    pub fn update_import_ledger(&self, id: &str, input: &UpdateImportLedgerInput) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE import_ledger SET status = ?1, cursor_json = ?2, imported_session_count = ?3, imported_span_count = ?4, attempt_count = ?5, error_code = ?6, error_message = ?7, started_at = ?8, completed_at = ?9, updated_at = ?10 WHERE id = ?11",
            &[
                Value::Text(input.status.clone()),
                input
                    .cursor_json
                    .as_ref()
                    .map_or(Value::Null, |cursor| Value::Text(cursor.clone())),
                Value::BigInt(i64::from(input.imported_session_count)),
                Value::BigInt(i64::from(input.imported_span_count)),
                Value::BigInt(i64::from(input.attempt_count)),
                input
                    .error_code
                    .as_ref()
                    .map_or(Value::Null, |code| Value::Text(code.clone())),
                input
                    .error_message
                    .as_ref()
                    .map_or(Value::Null, |message| Value::Text(message.clone())),
                input
                    .started_at
                    .as_ref()
                    .map_or(Value::Null, |started| Value::Text(started.clone())),
                input
                    .completed_at
                    .as_ref()
                    .map_or(Value::Null, |completed| Value::Text(completed.clone())),
                Value::Text(now),
                Value::Text(id.to_string()),
            ],
        )?;

        Ok(affected > 0)
    }
}

fn stored_import_ledger_from_row(row: &Row) -> Result<StoredImportLedger> {
    Ok(StoredImportLedger {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        workspace_id: required_text(row, 1, DbOperation::Query, "workspace_id")?.to_string(),
        source_kind: required_text(row, 2, DbOperation::Query, "source_kind")?.to_string(),
        source_id: required_text(row, 3, DbOperation::Query, "source_id")?.to_string(),
        status: required_text(row, 4, DbOperation::Query, "status")?.to_string(),
        cursor_json: optional_text(row, 5)?.map(str::to_string),
        imported_session_count: required_u32(row, 6, DbOperation::Query, "imported_session_count")?,
        imported_span_count: required_u32(row, 7, DbOperation::Query, "imported_span_count")?,
        attempt_count: required_u32(row, 8, DbOperation::Query, "attempt_count")?,
        error_code: optional_text(row, 9)?.map(str::to_string),
        error_message: optional_text(row, 10)?.map(str::to_string),
        started_at: optional_text(row, 11)?.map(str::to_string),
        completed_at: optional_text(row, 12)?.map(str::to_string),
        metadata_json: optional_text(row, 13)?.map(str::to_string),
        created_at: required_text(row, 14, DbOperation::Query, "created_at")?.to_string(),
        updated_at: required_text(row, 15, DbOperation::Query, "updated_at")?.to_string(),
    })
}

/// Input for creating a new feedback event (EE-080).
#[derive(Debug, Clone)]
pub struct CreateFeedbackEventInput {
    pub workspace_id: String,
    pub target_type: String,
    pub target_id: String,
    pub signal: String,
    pub weight: f32,
    pub source_type: String,
    pub source_id: Option<String>,
    pub reason: Option<String>,
    pub evidence_json: Option<String>,
    pub session_id: Option<String>,
}

/// A stored feedback_events row (EE-080).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFeedbackEvent {
    pub id: String,
    pub workspace_id: String,
    pub target_type: String,
    pub target_id: String,
    pub signal: String,
    pub weight: f32,
    pub source_type: String,
    pub source_id: Option<String>,
    pub reason: Option<String>,
    pub evidence_json: Option<String>,
    pub session_id: Option<String>,
    pub applied_at: Option<String>,
    pub created_at: String,
}

impl DbConnection {
    /// Insert a new feedback event.
    pub fn insert_feedback_event(&self, id: &str, input: &CreateFeedbackEventInput) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO feedback_events (id, workspace_id, target_type, target_id, signal, weight, source_type, source_id, reason, evidence_json, session_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            &[
                Value::Text(id.to_string()),
                Value::Text(input.workspace_id.clone()),
                Value::Text(input.target_type.clone()),
                Value::Text(input.target_id.clone()),
                Value::Text(input.signal.clone()),
                Value::Double(f64::from(input.weight)),
                Value::Text(input.source_type.clone()),
                input
                    .source_id
                    .as_ref()
                    .map_or(Value::Null, |source| Value::Text(source.clone())),
                input
                    .reason
                    .as_ref()
                    .map_or(Value::Null, |reason| Value::Text(reason.clone())),
                input
                    .evidence_json
                    .as_ref()
                    .map_or(Value::Null, |evidence| Value::Text(evidence.clone())),
                input
                    .session_id
                    .as_ref()
                    .map_or(Value::Null, |session| Value::Text(session.clone())),
                Value::Text(now),
            ],
        )?;

        Ok(())
    }

    /// Get a feedback event by its ID.
    pub fn get_feedback_event(&self, id: &str) -> Result<Option<StoredFeedbackEvent>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, target_type, target_id, signal, weight, source_type, source_id, reason, evidence_json, session_id, applied_at, created_at FROM feedback_events WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first().map(stored_feedback_event_from_row).transpose()
    }

    /// List feedback events for a target in deterministic order.
    pub fn list_feedback_events_for_target(
        &self,
        target_type: &str,
        target_id: &str,
    ) -> Result<Vec<StoredFeedbackEvent>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, target_type, target_id, signal, weight, source_type, source_id, reason, evidence_json, session_id, applied_at, created_at FROM feedback_events WHERE target_type = ?1 AND target_id = ?2 ORDER BY created_at ASC, id ASC",
            &[
                Value::Text(target_type.to_string()),
                Value::Text(target_id.to_string()),
            ],
        )?;

        rows.iter().map(stored_feedback_event_from_row).collect()
    }

    /// List feedback events for a workspace in deterministic order.
    pub fn list_feedback_events(&self, workspace_id: &str) -> Result<Vec<StoredFeedbackEvent>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, target_type, target_id, signal, weight, source_type, source_id, reason, evidence_json, session_id, applied_at, created_at FROM feedback_events WHERE workspace_id = ?1 ORDER BY created_at ASC, id ASC",
            &[Value::Text(workspace_id.to_string())],
        )?;

        rows.iter().map(stored_feedback_event_from_row).collect()
    }

    /// List feedback events by signal type.
    pub fn list_feedback_events_by_signal(
        &self,
        workspace_id: &str,
        signal: &str,
    ) -> Result<Vec<StoredFeedbackEvent>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, workspace_id, target_type, target_id, signal, weight, source_type, source_id, reason, evidence_json, session_id, applied_at, created_at FROM feedback_events WHERE workspace_id = ?1 AND signal = ?2 ORDER BY created_at ASC, id ASC",
            &[
                Value::Text(workspace_id.to_string()),
                Value::Text(signal.to_string()),
            ],
        )?;

        rows.iter().map(stored_feedback_event_from_row).collect()
    }

    /// Mark a feedback event as applied.
    pub fn apply_feedback_event(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE feedback_events SET applied_at = ?1 WHERE id = ?2 AND applied_at IS NULL",
            &[Value::Text(now), Value::Text(id.to_string())],
        )?;

        Ok(affected > 0)
    }

    /// Count feedback events by signal for a target (for scoring).
    pub fn count_feedback_by_signal(
        &self,
        target_type: &str,
        target_id: &str,
    ) -> Result<FeedbackCounts> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT signal, SUM(weight) as total_weight, COUNT(*) as count FROM feedback_events WHERE target_type = ?1 AND target_id = ?2 GROUP BY signal",
            &[
                Value::Text(target_type.to_string()),
                Value::Text(target_id.to_string()),
            ],
        )?;

        let mut counts = FeedbackCounts::default();
        for row in &rows {
            let signal = optional_text(row, 0)?.unwrap_or_default();
            let weight = row.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let count = row.get(2).and_then(|v| v.as_i64()).unwrap_or(0) as u32;

            match signal {
                "positive" | "helpful" | "confirmation" => {
                    counts.positive_weight += weight;
                    counts.positive_count += count;
                }
                "negative" | "harmful" | "contradiction" | "inaccurate" => {
                    counts.negative_weight += weight;
                    counts.negative_count += count;
                }
                "stale" | "outdated" => {
                    counts.decay_weight += weight;
                    counts.decay_count += count;
                }
                _ => {
                    counts.neutral_weight += weight;
                    counts.neutral_count += count;
                }
            }
        }

        Ok(counts)
    }
}

/// Aggregated feedback counts for scoring (EE-080).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedbackCounts {
    pub positive_weight: f32,
    pub positive_count: u32,
    pub negative_weight: f32,
    pub negative_count: u32,
    pub neutral_weight: f32,
    pub neutral_count: u32,
    pub decay_weight: f32,
    pub decay_count: u32,
}

impl FeedbackCounts {
    pub fn total_count(&self) -> u32 {
        self.positive_count + self.negative_count + self.neutral_count + self.decay_count
    }

    pub fn net_score(&self) -> f32 {
        self.positive_weight - self.negative_weight - (self.decay_weight * 0.5)
    }
}

fn stored_feedback_event_from_row(row: &Row) -> Result<StoredFeedbackEvent> {
    Ok(StoredFeedbackEvent {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        workspace_id: required_text(row, 1, DbOperation::Query, "workspace_id")?.to_string(),
        target_type: required_text(row, 2, DbOperation::Query, "target_type")?.to_string(),
        target_id: required_text(row, 3, DbOperation::Query, "target_id")?.to_string(),
        signal: required_text(row, 4, DbOperation::Query, "signal")?.to_string(),
        weight: row
            .get(5)
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(1.0),
        source_type: required_text(row, 6, DbOperation::Query, "source_type")?.to_string(),
        source_id: optional_text(row, 7)?.map(str::to_string),
        reason: optional_text(row, 8)?.map(str::to_string),
        evidence_json: optional_text(row, 9)?.map(str::to_string),
        session_id: optional_text(row, 10)?.map(str::to_string),
        applied_at: optional_text(row, 11)?.map(str::to_string),
        created_at: required_text(row, 12, DbOperation::Query, "created_at")?.to_string(),
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

    /// List all unique tags in use across all memories in a workspace.
    pub fn list_all_tags(&self, workspace_id: &str) -> Result<Vec<String>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT DISTINCT mt.tag FROM memory_tags mt JOIN memories m ON mt.memory_id = m.id WHERE m.workspace_id = ?1 AND m.tombstoned_at IS NULL ORDER BY mt.tag ASC",
            &[Value::Text(workspace_id.to_string())],
        )?;

        rows.iter()
            .map(|row| required_text(row, 0, DbOperation::Query, "tag").map(|s| s.to_string()))
            .collect()
    }

    /// Get tag usage counts for a workspace.
    pub fn get_tag_counts(&self, workspace_id: &str) -> Result<Vec<TagCount>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT mt.tag, COUNT(*) as count FROM memory_tags mt JOIN memories m ON mt.memory_id = m.id WHERE m.workspace_id = ?1 AND m.tombstoned_at IS NULL GROUP BY mt.tag ORDER BY count DESC, mt.tag ASC",
            &[Value::Text(workspace_id.to_string())],
        )?;

        rows.iter()
            .map(|row| {
                let tag = required_text(row, 0, DbOperation::Query, "tag")?.to_string();
                let count = required_i64(row, 1, DbOperation::Query, "count")? as u32;
                Ok(TagCount { tag, count })
            })
            .collect()
    }

    /// List memory IDs that have a specific tag in a workspace.
    pub fn list_memories_by_tag(&self, workspace_id: &str, tag: &str) -> Result<Vec<String>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT m.id FROM memories m JOIN memory_tags mt ON m.id = mt.memory_id WHERE m.workspace_id = ?1 AND mt.tag = ?2 AND m.tombstoned_at IS NULL ORDER BY m.id ASC",
            &[Value::Text(workspace_id.to_string()), Value::Text(tag.to_string())],
        )?;

        rows.iter()
            .map(|row| required_text(row, 0, DbOperation::Query, "id").map(|s| s.to_string()))
            .collect()
    }

    /// Replace all tags on a memory atomically.
    pub fn set_memory_tags(&self, memory_id: &str, tags: &[String]) -> Result<()> {
        self.execute_for(
            DbOperation::Execute,
            "DELETE FROM memory_tags WHERE memory_id = ?1",
            &[Value::Text(memory_id.to_string())],
        )?;

        for tag in tags {
            self.execute_for(
                DbOperation::Execute,
                "INSERT INTO memory_tags (memory_id, tag) VALUES (?1, ?2)",
                &[Value::Text(memory_id.to_string()), Value::Text(tag.clone())],
            )?;
        }
        Ok(())
    }
}

/// Tag usage count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
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

fn optional_u32(
    row: &Row,
    index: usize,
    operation: DbOperation,
    column: &str,
) -> Result<Option<u32>> {
    let Some(value) = row.get(index) else {
        return Ok(None);
    };
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let value = value.as_i64().ok_or_else(|| DbError::MalformedRow {
        operation,
        message: format!("{column} column at index {index} is not an integer"),
    })?;
    u32::try_from(value)
        .map(Some)
        .map_err(|_| DbError::MalformedRow {
            operation,
            message: format!("{column} column at index {index} must fit u32"),
        })
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

    pub fn parse(s: &str) -> Option<Self> {
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

    pub fn parse(s: &str) -> Option<Self> {
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
        SearchIndexJobType::parse(&self.job_type)
    }

    #[must_use]
    pub fn status_enum(&self) -> Option<SearchIndexJobStatus> {
        SearchIndexJobStatus::parse(&self.status)
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
        rows.iter().map(stored_search_index_job_from_row).collect()
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

/// Typed relation stored in the memory graph edge table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLinkRelation {
    Supports,
    Contradicts,
    DerivedFrom,
    Supersedes,
    Related,
    CoTag,
    CoMention,
}

impl MemoryLinkRelation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::DerivedFrom => "derived_from",
            Self::Supersedes => "supersedes",
            Self::Related => "related",
            Self::CoTag => "co_tag",
            Self::CoMention => "co_mention",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "supports" => Some(Self::Supports),
            "contradicts" => Some(Self::Contradicts),
            "derived_from" => Some(Self::DerivedFrom),
            "supersedes" => Some(Self::Supersedes),
            "related" => Some(Self::Related),
            "co_tag" => Some(Self::CoTag),
            "co_mention" => Some(Self::CoMention),
            _ => None,
        }
    }
}

/// Origin of a stored memory link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLinkSource {
    Agent,
    Auto,
    Import,
    Maintenance,
    Human,
}

impl MemoryLinkSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Auto => "auto",
            Self::Import => "import",
            Self::Maintenance => "maintenance",
            Self::Human => "human",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "auto" => Some(Self::Auto),
            "import" => Some(Self::Import),
            "maintenance" => Some(Self::Maintenance),
            "human" => Some(Self::Human),
            _ => None,
        }
    }
}

/// Input for creating a typed edge between two memories.
#[derive(Debug, Clone)]
pub struct CreateMemoryLinkInput {
    pub src_memory_id: String,
    pub dst_memory_id: String,
    pub relation: MemoryLinkRelation,
    pub weight: f32,
    pub confidence: f32,
    pub directed: bool,
    pub evidence_count: u32,
    pub last_reinforced_at: Option<String>,
    pub source: MemoryLinkSource,
    pub created_by: Option<String>,
    pub metadata_json: Option<String>,
}

/// A stored memory_links row.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMemoryLink {
    pub id: String,
    pub src_memory_id: String,
    pub dst_memory_id: String,
    pub relation: String,
    pub weight: f32,
    pub confidence: f32,
    pub directed: bool,
    pub evidence_count: u32,
    pub last_reinforced_at: Option<String>,
    pub source: String,
    pub created_at: String,
    pub created_by: Option<String>,
    pub metadata_json: Option<String>,
}

impl StoredMemoryLink {
    #[must_use]
    pub fn relation_enum(&self) -> Option<MemoryLinkRelation> {
        MemoryLinkRelation::parse(&self.relation)
    }

    #[must_use]
    pub fn source_enum(&self) -> Option<MemoryLinkSource> {
        MemoryLinkSource::parse(&self.source)
    }
}

impl DbConnection {
    /// Insert a typed memory link.
    pub fn insert_memory_link(&self, id: &str, input: &CreateMemoryLinkInput) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.execute_for(
            DbOperation::Execute,
            "INSERT INTO memory_links (id, src_memory_id, dst_memory_id, relation, weight, confidence, directed, evidence_count, last_reinforced_at, source, created_at, created_by, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            &[
                Value::Text(id.to_string()),
                Value::Text(input.src_memory_id.clone()),
                Value::Text(input.dst_memory_id.clone()),
                Value::Text(input.relation.as_str().to_string()),
                Value::Float(input.weight),
                Value::Float(input.confidence),
                Value::BigInt(if input.directed { 1 } else { 0 }),
                Value::BigInt(i64::from(input.evidence_count)),
                input
                    .last_reinforced_at
                    .as_ref()
                    .map_or(Value::Null, |timestamp| Value::Text(timestamp.clone())),
                Value::Text(input.source.as_str().to_string()),
                Value::Text(now),
                input
                    .created_by
                    .as_ref()
                    .map_or(Value::Null, |created_by| Value::Text(created_by.clone())),
                input
                    .metadata_json
                    .as_ref()
                    .map_or(Value::Null, |metadata| Value::Text(metadata.clone())),
            ],
        )?;

        Ok(())
    }

    /// Get a memory link by ID.
    pub fn get_memory_link(&self, id: &str) -> Result<Option<StoredMemoryLink>> {
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT id, src_memory_id, dst_memory_id, relation, weight, confidence, directed, evidence_count, last_reinforced_at, source, created_at, created_by, metadata_json FROM memory_links WHERE id = ?1",
            &[Value::Text(id.to_string())],
        )?;

        rows.first().map(stored_memory_link_from_row).transpose()
    }

    /// List links incident to a memory in deterministic graph-projection order.
    pub fn list_memory_links_for_memory(
        &self,
        memory_id: &str,
        relation: Option<MemoryLinkRelation>,
    ) -> Result<Vec<StoredMemoryLink>> {
        let mut sql = String::from(
            "SELECT id, src_memory_id, dst_memory_id, relation, weight, confidence, directed, evidence_count, last_reinforced_at, source, created_at, created_by, metadata_json FROM memory_links WHERE (src_memory_id = ?1 OR dst_memory_id = ?1)",
        );
        let mut params: Vec<Value> = vec![Value::Text(memory_id.to_string())];

        if let Some(relation) = relation {
            sql.push_str(" AND relation = ?2");
            params.push(Value::Text(relation.as_str().to_string()));
        }

        sql.push_str(" ORDER BY relation ASC, src_memory_id ASC, dst_memory_id ASC, id ASC");

        let rows = self.query_for(DbOperation::Query, &sql, &params)?;
        rows.iter().map(stored_memory_link_from_row).collect()
    }
}

fn stored_memory_link_from_row(row: &Row) -> Result<StoredMemoryLink> {
    let evidence_count = u32::try_from(required_i64(row, 7, DbOperation::Query, "evidence_count")?)
        .map_err(|_| DbError::MalformedRow {
            operation: DbOperation::Query,
            message: "evidence_count must fit u32".to_string(),
        })?;

    Ok(StoredMemoryLink {
        id: required_text(row, 0, DbOperation::Query, "id")?.to_string(),
        src_memory_id: required_text(row, 1, DbOperation::Query, "src_memory_id")?.to_string(),
        dst_memory_id: required_text(row, 2, DbOperation::Query, "dst_memory_id")?.to_string(),
        relation: required_text(row, 3, DbOperation::Query, "relation")?.to_string(),
        weight: required_f64(row, 4, DbOperation::Query, "weight")? as f32,
        confidence: required_f64(row, 5, DbOperation::Query, "confidence")? as f32,
        directed: required_i64(row, 6, DbOperation::Query, "directed")? != 0,
        evidence_count,
        last_reinforced_at: optional_text(row, 8)?.map(str::to_string),
        source: required_text(row, 9, DbOperation::Query, "source")?.to_string(),
        created_at: required_text(row, 10, DbOperation::Query, "created_at")?.to_string(),
        created_by: optional_text(row, 11)?.map(str::to_string),
        metadata_json: optional_text(row, 12)?.map(str::to_string),
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
            &vec![1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            "V001-V010 must be applied",
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
        ensure(
            table_names.contains(&"pack_records"),
            "pack_records table must exist",
        )?;
        ensure(
            table_names.contains(&"pack_items"),
            "pack_items table must exist",
        )?;
        ensure(
            table_names.contains(&"pack_omissions"),
            "pack_omissions table must exist",
        )?;
        ensure(
            table_names.contains(&"memory_links"),
            "memory_links table must exist",
        )?;
        ensure(
            table_names.contains(&"sessions"),
            "sessions table must exist",
        )?;
        ensure(
            table_names.contains(&"evidence_spans"),
            "evidence_spans table must exist",
        )?;
        ensure(
            table_names.contains(&"import_ledger"),
            "import_ledger table must exist",
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
            &vec![1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            "first run applies V001-V010",
        )?;

        let second = connection.migrate()?;
        ensure_equal(&second.applied().len(), &0, "second run applies nothing")?;
        ensure_equal(
            &second.skipped().to_vec(),
            &vec![1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            "second run skips V001-V010",
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
            &Some(10),
            "after migrations, schema version is 10",
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

    fn session_input(cass_session_id: &str) -> super::CreateSessionInput {
        super::CreateSessionInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            cass_session_id: cass_session_id.to_string(),
            source_path: Some("/home/agent/.cass/sessions/session.jsonl".to_string()),
            agent_name: Some("codex".to_string()),
            model: Some("gpt-5".to_string()),
            started_at: Some("2026-04-29T20:00:00Z".to_string()),
            ended_at: Some("2026-04-29T20:30:00Z".to_string()),
            message_count: 42,
            token_count: Some(12_345),
            content_hash: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            metadata_json: Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.to_string()),
        }
    }

    fn evidence_span_input(
        session_id: &str,
        cass_span_id: &str,
        start_line: u32,
    ) -> super::CreateEvidenceSpanInput {
        super::CreateEvidenceSpanInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            session_id: session_id.to_string(),
            memory_id: None,
            cass_span_id: cass_span_id.to_string(),
            span_kind: "message".to_string(),
            start_line,
            end_line: start_line + 2,
            start_byte: Some(start_line * 100),
            end_byte: Some(start_line * 100 + 80),
            role: Some("assistant".to_string()),
            excerpt: "Use SQLModel Rust plus FrankenSQLite for durable imports.".to_string(),
            content_hash: "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_string(),
            metadata_json: Some(
                r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.to_string(),
            ),
        }
    }

    fn import_ledger_input(source_id: &str, status: &str) -> super::CreateImportLedgerInput {
        super::CreateImportLedgerInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            source_kind: "cass".to_string(),
            source_id: source_id.to_string(),
            status: status.to_string(),
            cursor_json: Some(r#"{"after":"cass-session-a","batch":2}"#.to_string()),
            imported_session_count: 2,
            imported_span_count: 18,
            attempt_count: 1,
            error_code: None,
            error_message: None,
            started_at: Some("2026-04-29T20:00:00Z".to_string()),
            completed_at: (status == "completed").then(|| "2026-04-29T20:05:00Z".to_string()),
            metadata_json: Some(r#"{"source":"cass","schema":"ee.import_ledger.v1"}"#.to_string()),
        }
    }

    #[test]
    fn insert_and_get_session() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = session_input("cass-session-2026-04-29-a");
        connection.insert_session("sess_01234567890123456789012345", &input)?;

        let session = connection.get_session("sess_01234567890123456789012345")?;
        ensure(session.is_some(), "session must be found by ee id")?;
        let session = session.ok_or_else(|| TestFailure::new("session not found"))?;
        ensure_equal(
            &session.id.as_str(),
            &"sess_01234567890123456789012345",
            "id",
        )?;
        ensure_equal(
            &session.workspace_id.as_str(),
            &"wsp_01234567890123456789012345",
            "workspace_id",
        )?;
        ensure_equal(
            &session.cass_session_id.as_str(),
            &"cass-session-2026-04-29-a",
            "cass_session_id",
        )?;
        ensure_equal(
            &session.source_path,
            &Some("/home/agent/.cass/sessions/session.jsonl".to_string()),
            "source_path",
        )?;
        ensure_equal(
            &session.agent_name,
            &Some("codex".to_string()),
            "agent_name",
        )?;
        ensure_equal(&session.model, &Some("gpt-5".to_string()), "model")?;
        ensure_equal(
            &session.started_at,
            &Some("2026-04-29T20:00:00Z".to_string()),
            "started_at",
        )?;
        ensure_equal(
            &session.ended_at,
            &Some("2026-04-29T20:30:00Z".to_string()),
            "ended_at",
        )?;
        ensure_equal(&session.message_count, &42, "message_count")?;
        ensure_equal(&session.token_count, &Some(12_345), "token_count")?;
        ensure_equal(
            &session.content_hash.as_str(),
            &input.content_hash.as_str(),
            "content_hash",
        )?;
        ensure_equal(
            &session.metadata_json,
            &Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.to_string()),
            "metadata_json",
        )?;
        ensure(!session.imported_at.is_empty(), "imported_at is populated")?;
        ensure(!session.updated_at.is_empty(), "updated_at is populated")?;

        let by_cass = connection.get_session_by_cass_id(
            "wsp_01234567890123456789012345",
            "cass-session-2026-04-29-a",
        )?;
        ensure_equal(&by_cass, &Some(session), "lookup by CASS id matches")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_sessions_filters_workspace_and_sorts_by_cass_id() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;
        connection.execute_raw(
            "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('wsp_11234567890123456789012345', '/tmp/other', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )?;

        connection.insert_session(
            "sess_21234567890123456789012345",
            &session_input("cass-session-b"),
        )?;
        connection.insert_session(
            "sess_11234567890123456789012345",
            &session_input("cass-session-a"),
        )?;

        let mut other_workspace = session_input("cass-session-c");
        other_workspace.workspace_id = "wsp_11234567890123456789012345".to_string();
        connection.insert_session("sess_31234567890123456789012345", &other_workspace)?;

        let sessions = connection.list_sessions("wsp_01234567890123456789012345")?;
        let cass_ids: Vec<&str> = sessions
            .iter()
            .map(|session| session.cass_session_id.as_str())
            .collect();
        ensure_equal(
            &cass_ids,
            &vec!["cass-session-a", "cass-session-b"],
            "sessions sorted within requested workspace",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn sessions_enforce_unique_upstream_id_and_valid_json() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = session_input("cass-session-unique");
        connection.insert_session("sess_41234567890123456789012345", &input)?;

        let duplicate = connection.insert_session("sess_51234567890123456789012345", &input);
        ensure(
            matches!(
                duplicate,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "duplicate workspace CASS session id must be rejected",
        )?;

        let mut invalid_json = session_input("cass-session-invalid-json");
        invalid_json.metadata_json = Some("{not-json}".to_string());
        let invalid = connection.insert_session("sess_61234567890123456789012345", &invalid_json);
        ensure(
            matches!(
                invalid,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "metadata_json must be valid JSON when present",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn insert_and_get_evidence_span() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;
        connection.insert_session(
            "sess_01234567890123456789012345",
            &session_input("cass-session-evidence-a"),
        )?;
        connection.insert_memory(
            "mem_01234567890123456789012345",
            &super::CreateMemoryInput {
                workspace_id: "wsp_01234567890123456789012345".to_string(),
                level: "episodic".to_string(),
                kind: "cass_import".to_string(),
                content: "Imported CASS evidence.".to_string(),
                confidence: 0.45,
                utility: 0.5,
                importance: 0.4,
                provenance_uri: Some("cass-session://cass-session-evidence-a#L10-12".to_string()),
                trust_class: "cass_evidence".to_string(),
                trust_subclass: Some("session-span".to_string()),
                tags: vec!["cass".to_string()],
            },
        )?;

        let mut input = evidence_span_input("sess_01234567890123456789012345", "span-a", 10);
        input.memory_id = Some("mem_01234567890123456789012345".to_string());
        connection.insert_evidence_span("ev_01234567890123456789012345", &input)?;

        let span = connection.get_evidence_span("ev_01234567890123456789012345")?;
        ensure(span.is_some(), "evidence span must be found")?;
        let span = span.ok_or_else(|| TestFailure::new("evidence span not found"))?;
        ensure_equal(&span.id.as_str(), &"ev_01234567890123456789012345", "id")?;
        ensure_equal(
            &span.workspace_id.as_str(),
            &"wsp_01234567890123456789012345",
            "workspace_id",
        )?;
        ensure_equal(
            &span.session_id.as_str(),
            &"sess_01234567890123456789012345",
            "session_id",
        )?;
        ensure_equal(
            &span.memory_id,
            &Some("mem_01234567890123456789012345".to_string()),
            "memory_id",
        )?;
        ensure_equal(&span.cass_span_id.as_str(), &"span-a", "cass_span_id")?;
        ensure_equal(&span.span_kind.as_str(), &"message", "span_kind")?;
        ensure_equal(&span.start_line, &10, "start_line")?;
        ensure_equal(&span.end_line, &12, "end_line")?;
        ensure_equal(&span.start_byte, &Some(1000), "start_byte")?;
        ensure_equal(&span.end_byte, &Some(1080), "end_byte")?;
        ensure_equal(&span.role, &Some("assistant".to_string()), "role")?;
        ensure_equal(&span.excerpt.as_str(), &input.excerpt.as_str(), "excerpt")?;
        ensure_equal(
            &span.content_hash.as_str(),
            &input.content_hash.as_str(),
            "content_hash",
        )?;
        ensure(!span.created_at.is_empty(), "created_at is populated")?;
        ensure(!span.updated_at.is_empty(), "updated_at is populated")?;

        let by_memory =
            connection.list_evidence_spans_for_memory("mem_01234567890123456789012345")?;
        ensure_equal(&by_memory, &vec![span], "linked memory evidence list")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_evidence_spans_for_session_filters_and_sorts() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;
        connection.insert_session(
            "sess_11234567890123456789012345",
            &session_input("cass-session-evidence-b"),
        )?;
        connection.insert_session(
            "sess_21234567890123456789012345",
            &session_input("cass-session-evidence-c"),
        )?;

        connection.insert_evidence_span(
            "ev_21234567890123456789012345",
            &evidence_span_input("sess_11234567890123456789012345", "span-line-20", 20),
        )?;
        connection.insert_evidence_span(
            "ev_11234567890123456789012345",
            &evidence_span_input("sess_11234567890123456789012345", "span-line-10", 10),
        )?;
        connection.insert_evidence_span(
            "ev_31234567890123456789012345",
            &evidence_span_input("sess_21234567890123456789012345", "span-other", 5),
        )?;

        let spans =
            connection.list_evidence_spans_for_session("sess_11234567890123456789012345")?;
        let cass_span_ids: Vec<&str> = spans
            .iter()
            .map(|span| span.cass_span_id.as_str())
            .collect();
        ensure_equal(
            &cass_span_ids,
            &vec!["span-line-10", "span-line-20"],
            "session evidence spans sorted by source position",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn evidence_spans_enforce_unique_upstream_id_bounds_and_json() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;
        connection.insert_session(
            "sess_31234567890123456789012345",
            &session_input("cass-session-evidence-d"),
        )?;

        let input = evidence_span_input("sess_31234567890123456789012345", "span-unique", 10);
        connection.insert_evidence_span("ev_41234567890123456789012345", &input)?;

        let duplicate = connection.insert_evidence_span("ev_51234567890123456789012345", &input);
        ensure(
            matches!(
                duplicate,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "duplicate CASS span id within a session must be rejected",
        )?;

        let mut inverted =
            evidence_span_input("sess_31234567890123456789012345", "span-inverted-lines", 30);
        inverted.end_line = 29;
        let inverted_result =
            connection.insert_evidence_span("ev_61234567890123456789012345", &inverted);
        ensure(
            matches!(
                inverted_result,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "end_line before start_line must be rejected",
        )?;

        let mut invalid_json =
            evidence_span_input("sess_31234567890123456789012345", "span-invalid-json", 40);
        invalid_json.metadata_json = Some("{not-json}".to_string());
        let invalid_json_result =
            connection.insert_evidence_span("ev_71234567890123456789012345", &invalid_json);
        ensure(
            matches!(
                invalid_json_result,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "metadata_json must be valid JSON when present",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn insert_get_and_update_import_ledger() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = import_ledger_input("cass://sessions?workspace=test", "running");
        connection.insert_import_ledger("imp_01234567890123456789012345", &input)?;

        let ledger = connection.get_import_ledger("imp_01234567890123456789012345")?;
        ensure(ledger.is_some(), "import ledger must be found by ee id")?;
        let ledger = ledger.ok_or_else(|| TestFailure::new("import ledger not found"))?;
        ensure_equal(&ledger.id.as_str(), &"imp_01234567890123456789012345", "id")?;
        ensure_equal(
            &ledger.workspace_id.as_str(),
            &"wsp_01234567890123456789012345",
            "workspace_id",
        )?;
        ensure_equal(&ledger.source_kind.as_str(), &"cass", "source_kind")?;
        ensure_equal(
            &ledger.source_id.as_str(),
            &"cass://sessions?workspace=test",
            "source_id",
        )?;
        ensure_equal(&ledger.status.as_str(), &"running", "status")?;
        ensure_equal(
            &ledger.cursor_json,
            &Some(r#"{"after":"cass-session-a","batch":2}"#.to_string()),
            "cursor_json",
        )?;
        ensure_equal(&ledger.imported_session_count, &2, "imported_session_count")?;
        ensure_equal(&ledger.imported_span_count, &18, "imported_span_count")?;
        ensure_equal(&ledger.attempt_count, &1, "attempt_count")?;
        ensure_equal(&ledger.error_code, &None, "error_code")?;
        ensure_equal(&ledger.error_message, &None, "error_message")?;
        ensure(!ledger.created_at.is_empty(), "created_at is populated")?;
        ensure(!ledger.updated_at.is_empty(), "updated_at is populated")?;

        let by_source = connection.get_import_ledger_by_source(
            "wsp_01234567890123456789012345",
            "cass",
            "cass://sessions?workspace=test",
        )?;
        ensure_equal(
            &by_source,
            &Some(ledger.clone()),
            "lookup by source matches",
        )?;

        let updated = connection.update_import_ledger(
            "imp_01234567890123456789012345",
            &super::UpdateImportLedgerInput {
                status: "completed".to_string(),
                cursor_json: Some(r#"{"after":"cass-session-z","batch":9}"#.to_string()),
                imported_session_count: 9,
                imported_span_count: 81,
                attempt_count: 2,
                error_code: None,
                error_message: None,
                started_at: Some("2026-04-29T20:00:00Z".to_string()),
                completed_at: Some("2026-04-29T20:10:00Z".to_string()),
            },
        )?;
        ensure(updated, "existing import ledger row must update")?;

        let updated_ledger = connection
            .get_import_ledger("imp_01234567890123456789012345")?
            .ok_or_else(|| TestFailure::new("updated import ledger not found"))?;
        ensure_equal(
            &updated_ledger.status.as_str(),
            &"completed",
            "updated status",
        )?;
        ensure_equal(
            &updated_ledger.cursor_json,
            &Some(r#"{"after":"cass-session-z","batch":9}"#.to_string()),
            "updated cursor",
        )?;
        ensure_equal(
            &updated_ledger.imported_session_count,
            &9,
            "updated session count",
        )?;
        ensure_equal(
            &updated_ledger.imported_span_count,
            &81,
            "updated span count",
        )?;
        ensure_equal(&updated_ledger.attempt_count, &2, "updated attempt count")?;
        ensure_equal(
            &updated_ledger.completed_at,
            &Some("2026-04-29T20:10:00Z".to_string()),
            "completed_at",
        )?;

        let missing = connection.update_import_ledger(
            "imp_91234567890123456789012345",
            &super::UpdateImportLedgerInput {
                status: "failed".to_string(),
                cursor_json: None,
                imported_session_count: 0,
                imported_span_count: 0,
                attempt_count: 1,
                error_code: Some("not_found".to_string()),
                error_message: Some("missing ledger".to_string()),
                started_at: None,
                completed_at: None,
            },
        )?;
        ensure(!missing, "missing import ledger update reports false")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_import_ledgers_filters_workspace_status_and_sorts() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;
        connection.execute_raw(
            "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('wsp_11234567890123456789012345', '/tmp/other', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )?;

        connection.insert_import_ledger(
            "imp_21234567890123456789012345",
            &import_ledger_input("cass://session-b", "running"),
        )?;
        connection.insert_import_ledger(
            "imp_11234567890123456789012345",
            &import_ledger_input("cass://session-a", "completed"),
        )?;
        let mut other_workspace = import_ledger_input("cass://session-c", "running");
        other_workspace.workspace_id = "wsp_11234567890123456789012345".to_string();
        connection.insert_import_ledger("imp_31234567890123456789012345", &other_workspace)?;

        let ledgers = connection.list_import_ledgers("wsp_01234567890123456789012345")?;
        let source_ids: Vec<&str> = ledgers
            .iter()
            .map(|ledger| ledger.source_id.as_str())
            .collect();
        ensure_equal(
            &source_ids,
            &vec!["cass://session-a", "cass://session-b"],
            "import ledgers sorted by source key inside requested workspace",
        )?;

        let running = connection
            .list_import_ledgers_by_status("wsp_01234567890123456789012345", "running")?;
        ensure_equal(&running.len(), &1, "one running import ledger in workspace")?;
        let running_ledger = running
            .first()
            .ok_or_else(|| TestFailure::new("running import ledger not found"))?;
        ensure_equal(
            &running_ledger.source_id.as_str(),
            &"cass://session-b",
            "running ledger source",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn import_ledger_enforces_unique_source_status_completion_and_json() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = import_ledger_input("cass://session-unique", "running");
        connection.insert_import_ledger("imp_41234567890123456789012345", &input)?;

        let duplicate = connection.insert_import_ledger("imp_51234567890123456789012345", &input);
        ensure(
            matches!(
                duplicate,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "duplicate workspace source key must be rejected",
        )?;

        let invalid_status = import_ledger_input("cass://bad-status", "paused");
        let invalid_status_result =
            connection.insert_import_ledger("imp_61234567890123456789012345", &invalid_status);
        ensure(
            matches!(
                invalid_status_result,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "unknown import status must be rejected",
        )?;

        let mut completed_without_timestamp =
            import_ledger_input("cass://complete-without-timestamp", "completed");
        completed_without_timestamp.completed_at = None;
        let completed_result = connection.insert_import_ledger(
            "imp_71234567890123456789012345",
            &completed_without_timestamp,
        );
        ensure(
            matches!(
                completed_result,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "completed ledger rows must record completed_at",
        )?;

        let mut invalid_json = import_ledger_input("cass://bad-json", "running");
        invalid_json.cursor_json = Some("{not-json}".to_string());
        let invalid_json_result =
            connection.insert_import_ledger("imp_81234567890123456789012345", &invalid_json);
        ensure(
            matches!(
                invalid_json_result,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "cursor_json must be valid JSON when present",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn insert_and_get_feedback_event() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateFeedbackEventInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            target_type: "memory".to_string(),
            target_id: "mem_01234567890123456789012345".to_string(),
            signal: "positive".to_string(),
            weight: 1.0,
            source_type: "human_explicit".to_string(),
            source_id: Some("agent-123".to_string()),
            reason: Some("rule helped fix build".to_string()),
            evidence_json: Some(r#"{"outcome":"success"}"#.to_string()),
            session_id: None,
        };

        connection.insert_feedback_event("fb_0123456789012345678901234", &input)?;

        let event = connection.get_feedback_event("fb_0123456789012345678901234")?;
        ensure(event.is_some(), "feedback event must be found")?;

        let event = event.ok_or_else(|| TestFailure::new("feedback event not found"))?;
        ensure_equal(&event.id.as_str(), &"fb_0123456789012345678901234", "id")?;
        ensure_equal(
            &event.workspace_id.as_str(),
            &"wsp_01234567890123456789012345",
            "workspace_id",
        )?;
        ensure_equal(&event.target_type.as_str(), &"memory", "target_type")?;
        ensure_equal(
            &event.target_id.as_str(),
            &"mem_01234567890123456789012345",
            "target_id",
        )?;
        ensure_equal(&event.signal.as_str(), &"positive", "signal")?;
        ensure((event.weight - 1.0).abs() < 0.001, "weight must be ~1.0")?;
        ensure_equal(
            &event.source_type.as_str(),
            &"human_explicit",
            "source_type",
        )?;
        ensure_equal(
            &event.source_id,
            &Some("agent-123".to_string()),
            "source_id",
        )?;
        ensure_equal(
            &event.reason,
            &Some("rule helped fix build".to_string()),
            "reason",
        )?;
        ensure_equal(
            &event.evidence_json,
            &Some(r#"{"outcome":"success"}"#.to_string()),
            "evidence_json",
        )?;
        ensure_equal(&event.applied_at, &None, "applied_at is null initially")?;
        ensure(!event.created_at.is_empty(), "created_at is populated")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_feedback_events_and_apply() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let positive_input = super::CreateFeedbackEventInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            target_type: "memory".to_string(),
            target_id: "mem_01234567890123456789012345".to_string(),
            signal: "positive".to_string(),
            weight: 1.5,
            source_type: "agent_inference".to_string(),
            source_id: None,
            reason: None,
            evidence_json: None,
            session_id: None,
        };
        connection.insert_feedback_event("fb_1123456789012345678901234", &positive_input)?;

        let negative_input = super::CreateFeedbackEventInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            target_type: "memory".to_string(),
            target_id: "mem_01234567890123456789012345".to_string(),
            signal: "negative".to_string(),
            weight: 0.5,
            source_type: "outcome_observed".to_string(),
            source_id: None,
            reason: Some("build failed after applying rule".to_string()),
            evidence_json: None,
            session_id: None,
        };
        connection.insert_feedback_event("fb_2123456789012345678901234", &negative_input)?;

        let events = connection
            .list_feedback_events_for_target("memory", "mem_01234567890123456789012345")?;
        ensure_equal(&events.len(), &2, "two feedback events for target")?;
        ensure_equal(
            &events[0].id.as_str(),
            &"fb_1123456789012345678901234",
            "first event by create order",
        )?;

        let applied = connection.apply_feedback_event("fb_1123456789012345678901234")?;
        ensure(applied, "apply_feedback_event must succeed")?;

        let applied_event = connection
            .get_feedback_event("fb_1123456789012345678901234")?
            .ok_or_else(|| TestFailure::new("applied event not found"))?;
        ensure(applied_event.applied_at.is_some(), "applied_at is now set")?;

        let re_apply = connection.apply_feedback_event("fb_1123456789012345678901234")?;
        ensure(!re_apply, "second apply returns false (already applied)")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn count_feedback_by_signal_aggregates_correctly() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let target_type = "rule";
        let target_id = "rule_01234567890123456789012345";

        let signals = [
            ("fb_a123456789012345678901234", "positive", 1.0),
            ("fb_b123456789012345678901234", "helpful", 2.0),
            ("fb_c123456789012345678901234", "negative", 1.0),
            ("fb_d123456789012345678901234", "harmful", 0.5),
            ("fb_e123456789012345678901234", "stale", 1.0),
            ("fb_f123456789012345678901234", "neutral", 1.0),
        ];

        for (id, signal, weight) in signals {
            let input = super::CreateFeedbackEventInput {
                workspace_id: "wsp_01234567890123456789012345".to_string(),
                target_type: target_type.to_string(),
                target_id: target_id.to_string(),
                signal: signal.to_string(),
                weight,
                source_type: "automated_check".to_string(),
                source_id: None,
                reason: None,
                evidence_json: None,
                session_id: None,
            };
            connection.insert_feedback_event(id, &input)?;
        }

        let counts = connection.count_feedback_by_signal(target_type, target_id)?;
        ensure(
            (counts.positive_weight - 3.0).abs() < 0.001,
            "positive + helpful = 3.0",
        )?;
        ensure_equal(&counts.positive_count, &2, "two positive signals")?;
        ensure(
            (counts.negative_weight - 1.5).abs() < 0.001,
            "negative + harmful = 1.5",
        )?;
        ensure_equal(&counts.negative_count, &2, "two negative signals")?;
        ensure((counts.decay_weight - 1.0).abs() < 0.001, "stale = 1.0")?;
        ensure_equal(&counts.decay_count, &1, "one decay signal")?;
        ensure_equal(&counts.total_count(), &6, "six total events")?;

        let net = counts.net_score();
        ensure(
            (net - 1.0).abs() < 0.001,
            "net score = 3.0 - 1.5 - 0.5*1.0 = 1.0",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn feedback_events_constraint_validation() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let invalid_target_type = super::CreateFeedbackEventInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            target_type: "unknown_type".to_string(),
            target_id: "test".to_string(),
            signal: "positive".to_string(),
            weight: 1.0,
            source_type: "human_explicit".to_string(),
            source_id: None,
            reason: None,
            evidence_json: None,
            session_id: None,
        };
        let result =
            connection.insert_feedback_event("fb_x123456789012345678901234", &invalid_target_type);
        ensure(
            matches!(
                result,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "invalid target_type must be rejected",
        )?;

        let invalid_signal = super::CreateFeedbackEventInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            target_type: "memory".to_string(),
            target_id: "test".to_string(),
            signal: "unknown_signal".to_string(),
            weight: 1.0,
            source_type: "human_explicit".to_string(),
            source_id: None,
            reason: None,
            evidence_json: None,
            session_id: None,
        };
        let result =
            connection.insert_feedback_event("fb_y123456789012345678901234", &invalid_signal);
        ensure(
            matches!(
                result,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "invalid signal must be rejected",
        )?;

        let invalid_source_type = super::CreateFeedbackEventInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            target_type: "memory".to_string(),
            target_id: "test".to_string(),
            signal: "positive".to_string(),
            weight: 1.0,
            source_type: "unknown_source".to_string(),
            source_id: None,
            reason: None,
            evidence_json: None,
            session_id: None,
        };
        let result =
            connection.insert_feedback_event("fb_z123456789012345678901234", &invalid_source_type);
        ensure(
            matches!(
                result,
                Err(DbError::SqlModel {
                    operation: DbOperation::Execute,
                    ..
                })
            ),
            "invalid source_type must be rejected",
        )?;

        connection.close()?;
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

    #[test]
    fn search_index_job_type_enum_stable() -> TestResult {
        ensure_equal(
            &super::SearchIndexJobType::FullRebuild.as_str(),
            &"full_rebuild",
            "full_rebuild string",
        )?;
        ensure_equal(
            &super::SearchIndexJobType::Incremental.as_str(),
            &"incremental",
            "incremental string",
        )?;
        ensure_equal(
            &super::SearchIndexJobType::SingleDocument.as_str(),
            &"single_document",
            "single_document string",
        )?;

        ensure_equal(
            &super::SearchIndexJobType::parse("full_rebuild"),
            &Some(super::SearchIndexJobType::FullRebuild),
            "parse full_rebuild",
        )?;
        ensure_equal(
            &super::SearchIndexJobType::parse("invalid"),
            &None,
            "invalid returns None",
        )?;

        Ok(())
    }

    #[test]
    fn search_index_job_status_enum_stable() -> TestResult {
        ensure_equal(
            &super::SearchIndexJobStatus::Pending.as_str(),
            &"pending",
            "pending string",
        )?;
        ensure_equal(
            &super::SearchIndexJobStatus::Running.as_str(),
            &"running",
            "running string",
        )?;
        ensure_equal(
            &super::SearchIndexJobStatus::Completed.as_str(),
            &"completed",
            "completed string",
        )?;
        ensure_equal(
            &super::SearchIndexJobStatus::Failed.as_str(),
            &"failed",
            "failed string",
        )?;
        ensure_equal(
            &super::SearchIndexJobStatus::Cancelled.as_str(),
            &"cancelled",
            "cancelled string",
        )?;

        ensure(
            !super::SearchIndexJobStatus::Pending.is_terminal(),
            "pending is not terminal",
        )?;
        ensure(
            !super::SearchIndexJobStatus::Running.is_terminal(),
            "running is not terminal",
        )?;
        ensure(
            super::SearchIndexJobStatus::Completed.is_terminal(),
            "completed is terminal",
        )?;
        ensure(
            super::SearchIndexJobStatus::Failed.is_terminal(),
            "failed is terminal",
        )?;
        ensure(
            super::SearchIndexJobStatus::Cancelled.is_terminal(),
            "cancelled is terminal",
        )?;

        Ok(())
    }

    #[test]
    fn insert_and_get_search_index_job() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateSearchIndexJobInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            job_type: super::SearchIndexJobType::FullRebuild,
            document_source: None,
            document_id: None,
            documents_total: 100,
        };

        connection.insert_search_index_job("sidx_01234567890123456789012345", &input)?;

        let job = connection.get_search_index_job("sidx_01234567890123456789012345")?;
        ensure(job.is_some(), "job must be found")?;

        let job = job.ok_or_else(|| TestFailure::new("job not found"))?;
        ensure_equal(&job.id.as_str(), &"sidx_01234567890123456789012345", "id")?;
        ensure_equal(
            &job.workspace_id.as_str(),
            &"wsp_01234567890123456789012345",
            "workspace_id",
        )?;
        ensure_equal(&job.job_type.as_str(), &"full_rebuild", "job_type")?;
        ensure(job.document_source.is_none(), "document_source is None")?;
        ensure(job.document_id.is_none(), "document_id is None")?;
        ensure_equal(&job.status.as_str(), &"pending", "status is pending")?;
        ensure_equal(&job.documents_total, &100, "documents_total")?;
        ensure_equal(&job.documents_indexed, &0, "documents_indexed starts at 0")?;
        ensure(job.error_message.is_none(), "no error message")?;
        ensure(job.started_at.is_none(), "not started yet")?;
        ensure(job.completed_at.is_none(), "not completed yet")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn search_index_job_lifecycle() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateSearchIndexJobInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            job_type: super::SearchIndexJobType::Incremental,
            document_source: Some("memory".to_string()),
            document_id: None,
            documents_total: 50,
        };

        connection.insert_search_index_job("sidx_lifecycle00000000000000000", &input)?;

        let started = connection.start_search_index_job("sidx_lifecycle00000000000000000")?;
        ensure(started, "job started successfully")?;

        let job = connection
            .get_search_index_job("sidx_lifecycle00000000000000000")?
            .ok_or_else(|| TestFailure::new("job not found"))?;
        ensure_equal(&job.status.as_str(), &"running", "status is running")?;
        ensure(job.started_at.is_some(), "started_at is set")?;

        let progress_updated =
            connection.update_search_index_job_progress("sidx_lifecycle00000000000000000", 25)?;
        ensure(progress_updated, "progress updated")?;

        let job = connection
            .get_search_index_job("sidx_lifecycle00000000000000000")?
            .ok_or_else(|| TestFailure::new("job not found"))?;
        ensure_equal(&job.documents_indexed, &25, "25 documents indexed")?;

        let completed =
            connection.complete_search_index_job("sidx_lifecycle00000000000000000", 50)?;
        ensure(completed, "job completed successfully")?;

        let job = connection
            .get_search_index_job("sidx_lifecycle00000000000000000")?
            .ok_or_else(|| TestFailure::new("job not found"))?;
        ensure_equal(&job.status.as_str(), &"completed", "status is completed")?;
        ensure_equal(&job.documents_indexed, &50, "all 50 documents indexed")?;
        ensure(job.completed_at.is_some(), "completed_at is set")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn search_index_job_failure() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateSearchIndexJobInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            job_type: super::SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_string()),
            document_id: Some("mem_01234567890123456789012345".to_string()),
            documents_total: 1,
        };

        connection.insert_search_index_job("sidx_failure0000000000000000000", &input)?;
        connection.start_search_index_job("sidx_failure0000000000000000000")?;

        let failed = connection
            .fail_search_index_job("sidx_failure0000000000000000000", "Document not found")?;
        ensure(failed, "job failed successfully")?;

        let job = connection
            .get_search_index_job("sidx_failure0000000000000000000")?
            .ok_or_else(|| TestFailure::new("job not found"))?;
        ensure_equal(&job.status.as_str(), &"failed", "status is failed")?;
        ensure_equal(
            &job.error_message,
            &Some("Document not found".to_string()),
            "error message set",
        )?;
        ensure(job.completed_at.is_some(), "completed_at is set on failure")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn search_index_job_cancellation() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateSearchIndexJobInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            job_type: super::SearchIndexJobType::FullRebuild,
            document_source: None,
            document_id: None,
            documents_total: 200,
        };

        connection.insert_search_index_job("sidx_cancel00000000000000000000", &input)?;

        let cancelled = connection.cancel_search_index_job("sidx_cancel00000000000000000000")?;
        ensure(cancelled, "job cancelled successfully")?;

        let job = connection
            .get_search_index_job("sidx_cancel00000000000000000000")?
            .ok_or_else(|| TestFailure::new("job not found"))?;
        ensure_equal(&job.status.as_str(), &"cancelled", "status is cancelled")?;
        ensure(job.completed_at.is_some(), "completed_at is set on cancel")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_search_index_jobs_by_status() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let pending = super::CreateSearchIndexJobInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            job_type: super::SearchIndexJobType::FullRebuild,
            document_source: None,
            document_id: None,
            documents_total: 10,
        };

        connection.insert_search_index_job("sidx_list_pending00000000000000", &pending)?;
        connection.insert_search_index_job("sidx_list_running00000000000000", &pending)?;
        connection.start_search_index_job("sidx_list_running00000000000000")?;

        let all = connection.list_search_index_jobs("wsp_01234567890123456789012345", None)?;
        ensure_equal(&all.len(), &2, "two jobs total")?;

        let pending_jobs = connection.list_search_index_jobs(
            "wsp_01234567890123456789012345",
            Some(super::SearchIndexJobStatus::Pending),
        )?;
        ensure_equal(&pending_jobs.len(), &1, "one pending job")?;

        let running_jobs = connection.list_search_index_jobs(
            "wsp_01234567890123456789012345",
            Some(super::SearchIndexJobStatus::Running),
        )?;
        ensure_equal(&running_jobs.len(), &1, "one running job")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn latest_search_index_job() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateSearchIndexJobInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            job_type: super::SearchIndexJobType::FullRebuild,
            document_source: None,
            document_id: None,
            documents_total: 10,
        };

        connection.insert_search_index_job("sidx_latest00000000000000000001", &input)?;
        connection.insert_search_index_job("sidx_latest00000000000000000002", &input)?;

        let latest = connection.latest_search_index_job("wsp_01234567890123456789012345")?;
        ensure(latest.is_some(), "latest job found")?;

        let latest = latest.ok_or_else(|| TestFailure::new("latest not found"))?;
        ensure_equal(
            &latest.id.as_str(),
            &"sidx_latest00000000000000000002",
            "latest is most recent",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn search_index_job_stored_accessors() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateSearchIndexJobInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            job_type: super::SearchIndexJobType::FullRebuild,
            document_source: None,
            document_id: None,
            documents_total: 10,
        };

        connection.insert_search_index_job("sidx_accessors00000000000000000", &input)?;

        let job = connection
            .get_search_index_job("sidx_accessors00000000000000000")?
            .ok_or_else(|| TestFailure::new("job not found"))?;

        ensure_equal(
            &job.job_type_enum(),
            &Some(super::SearchIndexJobType::FullRebuild),
            "job_type_enum",
        )?;
        ensure_equal(
            &job.status_enum(),
            &Some(super::SearchIndexJobStatus::Pending),
            "status_enum",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn get_nonexistent_search_index_job_returns_none() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let job = connection.get_search_index_job("sidx_nonexistent000000000000000")?;
        ensure(job.is_none(), "nonexistent job must be None")?;

        connection.close()?;
        Ok(())
    }

    fn insert_link_memory(connection: &DbConnection, id: &str, content: &str) -> TestResult {
        let input = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: content.to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec![],
        };

        connection.insert_memory(id, &input)?;
        Ok(())
    }

    fn setup_link_memories(connection: &DbConnection) -> TestResult {
        setup_workspace(connection)?;
        insert_link_memory(
            connection,
            "mem_00000000000000000000000011",
            "Graph source memory",
        )?;
        insert_link_memory(
            connection,
            "mem_00000000000000000000000012",
            "Graph destination memory",
        )
    }

    fn memory_link_input(relation: super::MemoryLinkRelation) -> super::CreateMemoryLinkInput {
        super::CreateMemoryLinkInput {
            src_memory_id: "mem_00000000000000000000000011".to_string(),
            dst_memory_id: "mem_00000000000000000000000012".to_string(),
            relation,
            weight: 0.75,
            confidence: 0.9,
            directed: true,
            evidence_count: 2,
            last_reinforced_at: Some("2026-04-29T20:00:00Z".to_string()),
            source: super::MemoryLinkSource::Agent,
            created_by: Some("agent:test".to_string()),
            metadata_json: Some(r#"{"reason":"explicit"}"#.to_string()),
        }
    }

    #[test]
    fn memory_link_relation_and_source_strings_are_stable() -> TestResult {
        ensure_equal(
            &super::MemoryLinkRelation::Supports.as_str(),
            &"supports",
            "supports relation",
        )?;
        ensure_equal(
            &super::MemoryLinkRelation::DerivedFrom.as_str(),
            &"derived_from",
            "derived_from relation",
        )?;
        ensure_equal(
            &super::MemoryLinkRelation::parse("co_tag"),
            &Some(super::MemoryLinkRelation::CoTag),
            "parse co_tag",
        )?;
        ensure_equal(
            &super::MemoryLinkRelation::parse("unknown"),
            &None,
            "unknown relation",
        )?;
        ensure_equal(
            &super::MemoryLinkSource::Maintenance.as_str(),
            &"maintenance",
            "maintenance source",
        )?;
        ensure_equal(
            &super::MemoryLinkSource::parse("human"),
            &Some(super::MemoryLinkSource::Human),
            "parse human source",
        )
    }

    #[test]
    fn insert_and_get_memory_link() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_link_memories(&connection)?;

        let input = memory_link_input(super::MemoryLinkRelation::Supports);
        connection.insert_memory_link("link_00000000000000000000000001", &input)?;

        let link = connection.get_memory_link("link_00000000000000000000000001")?;
        ensure(link.is_some(), "memory link must be found")?;

        let link = link.ok_or_else(|| TestFailure::new("memory link not found"))?;
        ensure_equal(&link.id.as_str(), &"link_00000000000000000000000001", "id")?;
        ensure_equal(
            &link.src_memory_id.as_str(),
            &"mem_00000000000000000000000011",
            "src",
        )?;
        ensure_equal(
            &link.dst_memory_id.as_str(),
            &"mem_00000000000000000000000012",
            "dst",
        )?;
        ensure_equal(
            &link.relation_enum(),
            &Some(super::MemoryLinkRelation::Supports),
            "relation",
        )?;
        ensure_equal(
            &link.source_enum(),
            &Some(super::MemoryLinkSource::Agent),
            "source",
        )?;
        ensure((link.weight - 0.75).abs() < 0.001, "weight must round-trip")?;
        ensure(
            (link.confidence - 0.9).abs() < 0.001,
            "confidence must round-trip",
        )?;
        ensure(link.directed, "link is directed")?;
        ensure_equal(&link.evidence_count, &2, "evidence count")?;
        ensure_equal(
            &link.last_reinforced_at,
            &Some("2026-04-29T20:00:00Z".to_string()),
            "last_reinforced_at",
        )?;
        ensure_equal(
            &link.metadata_json,
            &Some(r#"{"reason":"explicit"}"#.to_string()),
            "metadata_json",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_memory_links_for_memory_orders_and_filters() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_link_memories(&connection)?;

        connection.insert_memory_link(
            "link_00000000000000000000000002",
            &memory_link_input(super::MemoryLinkRelation::Supports),
        )?;
        connection.insert_memory_link(
            "link_00000000000000000000000003",
            &memory_link_input(super::MemoryLinkRelation::Contradicts),
        )?;

        let all =
            connection.list_memory_links_for_memory("mem_00000000000000000000000011", None)?;
        ensure_equal(&all.len(), &2, "two links incident to source")?;
        ensure_equal(
            &all[0].relation_enum(),
            &Some(super::MemoryLinkRelation::Contradicts),
            "contradicts sorts before supports",
        )?;
        ensure_equal(
            &all[1].relation_enum(),
            &Some(super::MemoryLinkRelation::Supports),
            "supports second",
        )?;

        let supports = connection.list_memory_links_for_memory(
            "mem_00000000000000000000000011",
            Some(super::MemoryLinkRelation::Supports),
        )?;
        ensure_equal(&supports.len(), &1, "one supports link")?;
        ensure_equal(
            &supports[0].id.as_str(),
            &"link_00000000000000000000000002",
            "supports id",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn memory_links_reject_self_links_and_duplicate_edges() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_link_memories(&connection)?;

        let mut self_link = memory_link_input(super::MemoryLinkRelation::Related);
        self_link.dst_memory_id = self_link.src_memory_id.clone();
        let self_result =
            connection.insert_memory_link("link_00000000000000000000000004", &self_link);
        ensure(self_result.is_err(), "self links must be rejected")?;

        let input = memory_link_input(super::MemoryLinkRelation::Related);
        connection.insert_memory_link("link_00000000000000000000000005", &input)?;
        let duplicate = connection.insert_memory_link("link_00000000000000000000000006", &input);
        ensure(duplicate.is_err(), "duplicate typed edge must be rejected")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn check_integrity_passes_on_healthy_database() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let result = connection.check_integrity()?;
        ensure(result.passed, "integrity check must pass")?;
        ensure(result.issues.is_empty(), "no integrity issues")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn check_foreign_keys_passes_on_healthy_database() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let result = connection.check_foreign_keys()?;
        ensure(result.passed, "foreign key check must pass")?;
        ensure(result.violations.is_empty(), "no foreign key violations")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn integrity_report_on_healthy_database() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;

        let report = connection.integrity_report()?;
        ensure(report.is_healthy(), "database is healthy")?;
        ensure(report.integrity_check.passed, "integrity passed")?;
        ensure(report.foreign_key_check.passed, "foreign keys passed")?;
        ensure(!report.needs_migration, "no migration needed")?;
        ensure_equal(&report.schema_version, &Some(10), "schema version is 10")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn integrity_report_detects_pending_migration() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.ensure_migration_table()?;

        let report = connection.integrity_report()?;
        ensure(!report.is_healthy(), "database needs migration")?;
        ensure(report.needs_migration, "migration needed")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_all_tags_returns_unique_sorted_tags() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let mem1 = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "First memory".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["zebra".to_string(), "apple".to_string()],
        };
        let mem2 = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Second memory".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["apple".to_string(), "banana".to_string()],
        };

        connection.insert_memory("mem_taglist0000000000000000001", &mem1)?;
        connection.insert_memory("mem_taglist0000000000000000002", &mem2)?;

        let tags = connection.list_all_tags("wsp_01234567890123456789012345")?;
        ensure_equal(
            &tags,
            &vec![
                "apple".to_string(),
                "banana".to_string(),
                "zebra".to_string(),
            ],
            "unique tags sorted alphabetically",
        )?;

        connection.tombstone_memory("mem_taglist0000000000000000001")?;
        let tags_after = connection.list_all_tags("wsp_01234567890123456789012345")?;
        ensure_equal(
            &tags_after,
            &vec!["apple".to_string(), "banana".to_string()],
            "tombstoned memory tags excluded",
        )?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn get_tag_counts_returns_sorted_by_count() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let mem1 = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Memory one".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["common".to_string(), "rare".to_string()],
        };
        let mem2 = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Memory two".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["common".to_string()],
        };
        let mem3 = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Memory three".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["common".to_string()],
        };

        connection.insert_memory("mem_tagcount000000000000000001", &mem1)?;
        connection.insert_memory("mem_tagcount000000000000000002", &mem2)?;
        connection.insert_memory("mem_tagcount000000000000000003", &mem3)?;

        let counts = connection.get_tag_counts("wsp_01234567890123456789012345")?;
        ensure_equal(&counts.len(), &2, "two unique tags")?;
        ensure_equal(
            &counts[0].tag.as_str(),
            &"common",
            "common is first (count 3)",
        )?;
        ensure_equal(&counts[0].count, &3, "common count is 3")?;
        ensure_equal(&counts[1].tag.as_str(), &"rare", "rare is second (count 1)")?;
        ensure_equal(&counts[1].count, &1, "rare count is 1")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn list_memories_by_tag() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let mem1 = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Tagged memory".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["target".to_string()],
        };
        let mem2 = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Also tagged".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["target".to_string(), "extra".to_string()],
        };
        let mem3 = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Not tagged".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["other".to_string()],
        };

        connection.insert_memory("mem_bytag000000000000000000001", &mem1)?;
        connection.insert_memory("mem_bytag000000000000000000002", &mem2)?;
        connection.insert_memory("mem_bytag000000000000000000003", &mem3)?;

        let memories =
            connection.list_memories_by_tag("wsp_01234567890123456789012345", "target")?;
        ensure_equal(&memories.len(), &2, "two memories with target tag")?;
        ensure(
            memories.contains(&"mem_bytag000000000000000000001".to_string()),
            "first memory included",
        )?;
        ensure(
            memories.contains(&"mem_bytag000000000000000000002".to_string()),
            "second memory included",
        )?;

        let other = connection.list_memories_by_tag("wsp_01234567890123456789012345", "other")?;
        ensure_equal(&other.len(), &1, "one memory with other tag")?;

        let none =
            connection.list_memories_by_tag("wsp_01234567890123456789012345", "nonexistent")?;
        ensure(none.is_empty(), "no memories with nonexistent tag")?;

        connection.close()?;
        Ok(())
    }

    #[test]
    fn set_memory_tags_replaces_all_tags() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        setup_workspace(&connection)?;

        let input = super::CreateMemoryInput {
            workspace_id: "wsp_01234567890123456789012345".to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: "Replaceable tags".to_string(),
            confidence: 0.8,
            utility: 0.6,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_string(),
            trust_subclass: None,
            tags: vec!["old1".to_string(), "old2".to_string()],
        };

        connection.insert_memory("mem_settags0000000000000000001", &input)?;

        let before = connection.get_memory_tags("mem_settags0000000000000000001")?;
        ensure_equal(&before.len(), &2, "two initial tags")?;

        connection.set_memory_tags(
            "mem_settags0000000000000000001",
            &["new1".to_string(), "new2".to_string(), "new3".to_string()],
        )?;

        let after = connection.get_memory_tags("mem_settags0000000000000000001")?;
        ensure_equal(&after.len(), &3, "three new tags")?;
        ensure(after.contains(&"new1".to_string()), "has new1")?;
        ensure(after.contains(&"new2".to_string()), "has new2")?;
        ensure(after.contains(&"new3".to_string()), "has new3")?;
        ensure(!after.contains(&"old1".to_string()), "old1 removed")?;
        ensure(!after.contains(&"old2".to_string()), "old2 removed")?;

        connection.set_memory_tags("mem_settags0000000000000000001", &[])?;
        let cleared = connection.get_memory_tags("mem_settags0000000000000000001")?;
        ensure(cleared.is_empty(), "all tags cleared")?;

        connection.close()?;
        Ok(())
    }
}
