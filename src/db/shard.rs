//! Planning and rollback-preservation primitives for per-workspace database shard fan-out.
//!
//! Most functions only derive the catalog and shard paths that later migration
//! beads can materialize. Rollback preservation is intentionally file-local and
//! idempotent so migration apply code can make the legacy database recoverable
//! before any shard layout is marked authoritative.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use super::{DatabaseConfig, DbConnection};

pub const SHARD_FANOUT_STATUS_SCHEMA_V1: &str = "ee.shard_fanout.status.v1";
pub const SHARD_FANOUT_ATTACH_PLAN_SCHEMA_V1: &str = "ee.shard_fanout.attach_plan.v1";
pub const SHARD_FANOUT_ATTACH_EXECUTION_SCHEMA_V1: &str = "ee.shard_fanout.attach_execution.v1";
pub const SHARD_FANOUT_MIGRATION_PLAN_SCHEMA_V1: &str = "ee.shard_fanout.migration_plan.v1";
pub const SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1: &str = "ee.shard_fanout.migration_audit.v1";
pub const SHARD_FANOUT_PRESERVE_SOURCE_SCHEMA_V1: &str = "ee.shard_fanout.preserve_source.v1";
pub const SHARD_FANOUT_CATALOG_SCHEMA_VERSION: u32 = 1;
pub const SHARD_CATALOG_FILE_NAME: &str = "catalog.db";
pub const SHARD_FILE_EXTENSION: &str = "db";
pub const PRE_SHARD_FANOUT_FILE_NAME: &str = ".pre-shard-fanout.db";

pub const SHARD_FANOUT_ROOT_UNSAFE_CODE: &str = "shard_fanout_root_unsafe";
pub const SHARD_FANOUT_HOME_UNAVAILABLE_CODE: &str = "shard_fanout_home_unavailable";
pub const SHARD_FANOUT_WORKSPACE_UNAVAILABLE_CODE: &str = "shard_fanout_workspace_unavailable";
pub const SHARD_FANOUT_WORKSPACE_ID_UNSAFE_CODE: &str = "shard_fanout_workspace_id_unsafe";
pub const SHARD_FANOUT_CATALOG_MISSING_CODE: &str = "shard_fanout_catalog_missing";
pub const SHARD_FANOUT_SHARD_MISSING_CODE: &str = "shard_fanout_shard_missing";
pub const SHARD_ATTACH_FAILED_CODE: &str = "shard_attach_failed";
pub const CROSS_SHARD_SKEW_DETECTED_CODE: &str = "cross_shard_skew_detected";

const DEFAULT_DATA_DIR_SUFFIX: &str = ".local/share/ee";
const DEFAULT_SHARDS_DIR_NAME: &str = "shards";

const CATALOG_REQUIRED_FIELDS: &[&str] = &[
    "workspace_id",
    "workspace_registry_mirror",
    "shard_id",
    "shard_path",
    "catalog_schema_version",
    "shard_generation",
    "migration_state",
    "last_verified_hashes",
];

/// Feature-level posture for shard fan-out status and doctor surfaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardFanoutPosture {
    Disabled,
    Enabled,
    MigrationRequired,
    Degraded,
    NotInspected,
}

impl ShardFanoutPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Enabled => "enabled",
            Self::MigrationRequired => "migration_required",
            Self::Degraded => "degraded",
            Self::NotInspected => "not_inspected",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardCatalogContractReport {
    pub schema_version: u32,
    pub required_fields: Vec<&'static str>,
}

impl Default for ShardCatalogContractReport {
    fn default() -> Self {
        Self {
            schema_version: SHARD_FANOUT_CATALOG_SCHEMA_VERSION,
            required_fields: CATALOG_REQUIRED_FIELDS.to_vec(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

impl ShardFanoutDegradation {
    #[must_use]
    pub const fn new(
        code: &'static str,
        severity: &'static str,
        message: &'static str,
        repair: &'static str,
    ) -> Self {
        Self {
            code,
            severity,
            message,
            repair,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutRecoveryAction {
    pub priority: u8,
    pub kind: &'static str,
    pub command: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardVerificationHash {
    pub name: &'static str,
    pub value: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutStatusReport {
    pub schema: &'static str,
    pub enabled: bool,
    pub posture: ShardFanoutPosture,
    pub workspace_id: Option<String>,
    pub workspace_root: Option<PathBuf>,
    pub legacy_database_path: Option<PathBuf>,
    pub data_root: PathBuf,
    pub shard_root: PathBuf,
    pub catalog_path: PathBuf,
    pub shard_path: Option<PathBuf>,
    pub shard_id: Option<String>,
    pub catalog_exists: bool,
    pub shard_exists: bool,
    pub catalog_contract: ShardCatalogContractReport,
    pub shard_generation: Option<u64>,
    pub migration_state: &'static str,
    pub last_verified_hashes: Vec<ShardVerificationHash>,
    pub degraded: Vec<ShardFanoutDegradation>,
    pub recovery: Vec<ShardFanoutRecoveryAction>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardFanoutMigrationWorkspaceInput {
    pub workspace_id: String,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardFanoutMigrationPlanInput {
    pub source_database_path: PathBuf,
    pub shards_dir_override: Option<PathBuf>,
    pub workspaces: Vec<ShardFanoutMigrationWorkspaceInput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationAuditRowPlan {
    pub schema: &'static str,
    pub event: &'static str,
    pub workspace_id: Option<String>,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationWorkspacePlan {
    pub workspace_id: String,
    pub workspace_root: PathBuf,
    pub shard_id: Option<String>,
    pub shard_path: Option<PathBuf>,
    pub source_database_path: PathBuf,
    pub planned_row_count: Option<u64>,
    pub row_counts_by_table: BTreeMap<String, u64>,
    pub source_hash: Option<String>,
    pub expected_audit_rows: Vec<ShardFanoutMigrationAuditRowPlan>,
    pub blockers: Vec<ShardFanoutDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationPlan {
    pub schema: &'static str,
    pub dry_run: bool,
    pub source_database_path: PathBuf,
    pub preserved_source_database_path: PathBuf,
    pub source_database_hash: Option<String>,
    pub shard_root: Option<PathBuf>,
    pub catalog_path: Option<PathBuf>,
    pub catalog_schema_version: u32,
    pub workspaces: Vec<ShardFanoutMigrationWorkspacePlan>,
    pub expected_audit_rows: Vec<ShardFanoutMigrationAuditRowPlan>,
    pub blockers: Vec<ShardFanoutDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutPreservedSourceReport {
    pub schema: &'static str,
    pub source_path: PathBuf,
    pub preserved_path: PathBuf,
    pub copied: bool,
    pub source_hash: String,
    pub preserved_hash: String,
    pub source_size_bytes: u64,
    pub preserved_size_bytes: u64,
    pub rollback_ready: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShardFanoutPreserveSourceError {
    MissingSource {
        path: PathBuf,
    },
    SourceRead {
        path: PathBuf,
        message: String,
    },
    PreservedRead {
        path: PathBuf,
        message: String,
    },
    PreserveWrite {
        path: PathBuf,
        message: String,
    },
    PreservedSourceMismatch {
        source_path: PathBuf,
        preserved_path: PathBuf,
        source_hash: String,
        preserved_hash: String,
    },
}

impl fmt::Display for ShardFanoutPreserveSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSource { path } => write!(
                f,
                "source database is required before shard fan-out migration: {}",
                path.display()
            ),
            Self::SourceRead { path, message } => write!(
                f,
                "could not read source database for shard fan-out migration: {} ({message})",
                path.display()
            ),
            Self::PreservedRead { path, message } => write!(
                f,
                "could not read preserved shard fan-out rollback database: {} ({message})",
                path.display()
            ),
            Self::PreserveWrite { path, message } => write!(
                f,
                "could not preserve shard fan-out rollback database: {} ({message})",
                path.display()
            ),
            Self::PreservedSourceMismatch {
                source_path,
                preserved_path,
                source_hash,
                preserved_hash,
            } => write!(
                f,
                "preserved shard fan-out rollback database does not match source: source={} preserved={} source_hash={} preserved_hash={}",
                source_path.display(),
                preserved_path.display(),
                source_hash,
                preserved_hash
            ),
        }
    }
}

impl std::error::Error for ShardFanoutPreserveSourceError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerShardAttachPlanInput {
    pub enabled: bool,
    pub strict: bool,
    pub local_workspace_id: String,
    pub shards_dir_override: Option<PathBuf>,
    pub peer_workspace_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerShardAttachTarget {
    pub workspace_id: String,
    pub shard_id: Option<String>,
    pub attach_alias: String,
    pub shard_path: Option<PathBuf>,
    pub read_only_uri: Option<String>,
    pub attach_sql: Option<String>,
    pub shard_exists: bool,
    pub attachable: bool,
    pub degraded: Vec<ShardFanoutDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerShardAttachPlan {
    pub schema: &'static str,
    pub enabled: bool,
    pub strict: bool,
    pub local_workspace_id: String,
    pub shard_root: Option<PathBuf>,
    pub catalog_path: Option<PathBuf>,
    pub targets: Vec<PeerShardAttachTarget>,
    pub attachable_count: usize,
    pub blocked: bool,
    pub degraded: Vec<ShardFanoutDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerShardAttachExecutionTarget {
    pub workspace_id: String,
    pub attach_alias: String,
    pub attached: bool,
    pub degraded: Vec<ShardFanoutDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerShardAttachExecution {
    pub schema: &'static str,
    pub attempted_count: usize,
    pub attached_count: usize,
    pub blocked: bool,
    pub query_only_sql: &'static str,
    pub targets: Vec<PeerShardAttachExecutionTarget>,
    pub degraded: Vec<ShardFanoutDegradation>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShardFanoutResolverInput {
    pub enabled: bool,
    pub workspace_id: Option<String>,
    pub workspace_root: Option<PathBuf>,
    pub shards_dir_override: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DbShardRoutingMode {
    Legacy,
    ShardFanout,
}

impl DbShardRoutingMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::ShardFanout => "shard_fanout",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbShardHandle {
    pub routing_mode: DbShardRoutingMode,
    pub workspace_id: String,
    pub shard_id: Option<String>,
    pub database_path: PathBuf,
    pub catalog_path: PathBuf,
    pub legacy_database_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbShardRouter {
    status: ShardFanoutStatusReport,
    handle: DbShardHandle,
    request_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbShardRouterInput {
    pub resolver_input: ShardFanoutResolverInput,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DbShardRouterError {
    WorkspaceRootUnavailable,
    WorkspaceIdUnavailable,
    ShardPathUnavailable,
    ShardNotAuthoritative {
        posture: ShardFanoutPosture,
        degraded_codes: Vec<&'static str>,
    },
}

impl fmt::Display for DbShardRouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkspaceRootUnavailable => {
                f.write_str("workspace root is required for database shard routing")
            }
            Self::WorkspaceIdUnavailable => {
                f.write_str("workspace id is required for database shard routing")
            }
            Self::ShardPathUnavailable => {
                f.write_str("shard fan-out did not produce a shard database path")
            }
            Self::ShardNotAuthoritative {
                posture,
                degraded_codes,
            } => write!(
                f,
                "shard fan-out is not authoritative: posture={} degraded_codes={}",
                posture.as_str(),
                degraded_codes.join(",")
            ),
        }
    }
}

impl std::error::Error for DbShardRouterError {}

impl DbShardRouter {
    pub fn resolve(input: ShardFanoutResolverInput) -> Result<Self, DbShardRouterError> {
        Self::resolve_with_context(DbShardRouterInput {
            resolver_input: input,
            request_id: None,
        })
    }

    pub fn resolve_with_context(input: DbShardRouterInput) -> Result<Self, DbShardRouterError> {
        let started = Instant::now();
        let request_id = input.request_id;
        let status = resolve_shard_fanout_status(input.resolver_input);
        trace_router_event("input", request_id.as_deref(), &status, started);

        let legacy_database_path = status
            .legacy_database_path
            .clone()
            .ok_or(DbShardRouterError::WorkspaceRootUnavailable)?;
        let workspace_id = status
            .workspace_id
            .clone()
            .ok_or(DbShardRouterError::WorkspaceIdUnavailable)?;

        let handle = if !status.enabled {
            DbShardHandle {
                routing_mode: DbShardRoutingMode::Legacy,
                workspace_id,
                shard_id: status.shard_id.clone(),
                database_path: legacy_database_path.clone(),
                catalog_path: status.catalog_path.clone(),
                legacy_database_path,
            }
        } else if status.posture == ShardFanoutPosture::Enabled {
            DbShardHandle {
                routing_mode: DbShardRoutingMode::ShardFanout,
                workspace_id,
                shard_id: status.shard_id.clone(),
                database_path: status
                    .shard_path
                    .clone()
                    .ok_or(DbShardRouterError::ShardPathUnavailable)?,
                catalog_path: status.catalog_path.clone(),
                legacy_database_path,
            }
        } else {
            return Err(DbShardRouterError::ShardNotAuthoritative {
                posture: status.posture,
                degraded_codes: status.degraded.iter().map(|entry| entry.code).collect(),
            });
        };

        trace_router_event("response", request_id.as_deref(), &status, started);
        Ok(Self {
            status,
            handle,
            request_id,
        })
    }

    #[must_use]
    pub const fn handle(&self) -> &DbShardHandle {
        &self.handle
    }

    #[must_use]
    pub const fn status(&self) -> &ShardFanoutStatusReport {
        &self.status
    }

    pub fn open(&self) -> super::Result<DbConnection> {
        let started = Instant::now();
        trace_router_event("write", self.request_id.as_deref(), &self.status, started);
        DbConnection::open(DatabaseConfig::file(self.handle.database_path.clone()))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ShardPathError {
    HomeUnavailable,
    Empty,
    Relative(PathBuf),
    ParentComponent(PathBuf),
    RootDirectory(PathBuf),
    SymlinkComponent(PathBuf),
    InspectFailed(PathBuf, String),
}

impl ShardPathError {
    fn degradation(&self) -> ShardFanoutDegradation {
        match self {
            Self::HomeUnavailable => ShardFanoutDegradation::new(
                SHARD_FANOUT_HOME_UNAVAILABLE_CODE,
                "warning",
                "Shard fan-out could not resolve the default data directory.",
                "Set XDG_DATA_HOME, HOME, or EE_SHARDS_DIR before enabling shard fan-out.",
            ),
            Self::Empty
            | Self::Relative(_)
            | Self::ParentComponent(_)
            | Self::RootDirectory(_)
            | Self::SymlinkComponent(_)
            | Self::InspectFailed(_, _) => ShardFanoutDegradation::new(
                SHARD_FANOUT_ROOT_UNSAFE_CODE,
                "high",
                "Shard fan-out refused an unsafe shard root.",
                "Set EE_SHARDS_DIR to an absolute, non-symlinked directory below an operator-owned data root.",
            ),
        }
    }
}

#[must_use]
pub fn shard_fanout_enabled_from_env_value(value: Option<&str>) -> bool {
    value.is_some_and(|raw| {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

pub fn default_shards_dir_from_env() -> Result<PathBuf, ShardPathError> {
    default_shards_dir_from_values(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

pub fn default_shards_dir_from_values(
    xdg_data_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ShardPathError> {
    if let Some(root) = non_empty_env_path(xdg_data_home) {
        return Ok(root.join("ee").join(DEFAULT_SHARDS_DIR_NAME));
    }
    let Some(home) = non_empty_env_path(home) else {
        return Err(ShardPathError::HomeUnavailable);
    };
    Ok(home
        .join(DEFAULT_DATA_DIR_SUFFIX)
        .join(DEFAULT_SHARDS_DIR_NAME))
}

fn non_empty_env_path(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    if value.as_os_str().is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

#[must_use]
pub fn catalog_path_for_shard_root(shard_root: &Path) -> PathBuf {
    shard_root
        .parent()
        .unwrap_or(shard_root)
        .join(SHARD_CATALOG_FILE_NAME)
}

pub fn normalize_shard_root(path: &Path) -> Result<PathBuf, ShardPathError> {
    let normalized = normalize_absolute_path(path)?;
    reject_existing_symlink_components(&normalized)?;
    Ok(normalized)
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf, ShardPathError> {
    if path.as_os_str().is_empty() {
        return Err(ShardPathError::Empty);
    }
    if !path.is_absolute() {
        return Err(ShardPathError::Relative(path.to_path_buf()));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(ShardPathError::ParentComponent(path.to_path_buf()));
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.parent().is_none() {
        return Err(ShardPathError::RootDirectory(normalized));
    }
    Ok(normalized)
}

fn reject_existing_symlink_components(path: &Path) -> Result<(), ShardPathError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir | Component::Normal(_) => current.push(component.as_os_str()),
        }

        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ShardPathError::SymlinkComponent(current));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(ShardPathError::InspectFailed(current, error.to_string()));
            }
        }
    }
    Ok(())
}

pub fn shard_id_for_workspace_id(workspace_id: &str) -> Result<String, ShardPathError> {
    let trimmed = workspace_id.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(ShardPathError::ParentComponent(PathBuf::from(workspace_id)));
    }
    Ok(trimmed.to_owned())
}

#[must_use]
pub fn shard_file_path(shard_root: &Path, shard_id: &str) -> PathBuf {
    shard_root.join(format!("{shard_id}.{SHARD_FILE_EXTENSION}"))
}

#[must_use]
pub fn preserved_legacy_database_path(source_database_path: &Path) -> PathBuf {
    source_database_path.parent().map_or_else(
        || PathBuf::from(PRE_SHARD_FANOUT_FILE_NAME),
        |parent| parent.join(PRE_SHARD_FANOUT_FILE_NAME),
    )
}

#[must_use]
pub fn plan_shard_fanout_migration(
    input: ShardFanoutMigrationPlanInput,
) -> ShardFanoutMigrationPlan {
    let source_database_path = input.source_database_path;
    let preserved_source_database_path = preserved_legacy_database_path(&source_database_path);
    let source_database_hash = blake3_file_hash(&source_database_path)
        .ok()
        .map(|(hash, _)| hash);
    let mut blockers = Vec::new();
    let shard_root_result = input
        .shards_dir_override
        .as_deref()
        .map_or_else(default_shards_dir_from_env, |path| Ok(PathBuf::from(path)))
        .and_then(|path| normalize_shard_root(&path));

    let (shard_root, catalog_path) = match shard_root_result {
        Ok(root) => {
            let catalog_path = catalog_path_for_shard_root(&root);
            (Some(root), Some(catalog_path))
        }
        Err(error) => {
            blockers.push(error.degradation());
            (None, None)
        }
    };

    let mut workspace_inputs = input.workspaces;
    workspace_inputs.sort_by(|left, right| {
        left.workspace_id
            .cmp(&right.workspace_id)
            .then_with(|| left.workspace_root.cmp(&right.workspace_root))
    });

    let mut expected_audit_rows = vec![ShardFanoutMigrationAuditRowPlan {
        schema: SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1,
        event: "preserve_legacy_database",
        workspace_id: None,
        source_path: source_database_path.clone(),
        target_path: preserved_source_database_path.clone(),
    }];

    let workspaces = workspace_inputs
        .into_iter()
        .map(|workspace| {
            let mut workspace_blockers = Vec::new();
            let shard_resolution = shard_id_for_workspace_id(&workspace.workspace_id)
                .map(|shard_id| {
                    let shard_path = shard_root
                        .as_deref()
                        .map(|root| shard_file_path(root, &shard_id));
                    (Some(shard_id), shard_path)
                })
                .unwrap_or_else(|_| {
                    let degradation = unsafe_workspace_id_degradation();
                    blockers.push(degradation.clone());
                    workspace_blockers.push(degradation);
                    (None, None)
                });

            let (shard_id, shard_path) = shard_resolution;
            let workspace_expected_audit_rows = shard_path
                .as_ref()
                .map(|target_path| {
                    vec![ShardFanoutMigrationAuditRowPlan {
                        schema: SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1,
                        event: "copy_workspace_to_shard",
                        workspace_id: Some(workspace.workspace_id.clone()),
                        source_path: source_database_path.clone(),
                        target_path: target_path.clone(),
                    }]
                })
                .unwrap_or_default();
            expected_audit_rows.extend(workspace_expected_audit_rows.iter().cloned());

            ShardFanoutMigrationWorkspacePlan {
                workspace_id: workspace.workspace_id,
                workspace_root: workspace.workspace_root,
                shard_id,
                shard_path,
                source_database_path: source_database_path.clone(),
                planned_row_count: None,
                row_counts_by_table: BTreeMap::new(),
                source_hash: source_database_hash.clone(),
                expected_audit_rows: workspace_expected_audit_rows,
                blockers: workspace_blockers,
            }
        })
        .collect();

    ShardFanoutMigrationPlan {
        schema: SHARD_FANOUT_MIGRATION_PLAN_SCHEMA_V1,
        dry_run: true,
        source_database_path,
        preserved_source_database_path,
        source_database_hash,
        shard_root,
        catalog_path,
        catalog_schema_version: SHARD_FANOUT_CATALOG_SCHEMA_VERSION,
        workspaces,
        expected_audit_rows,
        blockers,
    }
}

pub fn preserve_shard_fanout_source_database(
    plan: &ShardFanoutMigrationPlan,
) -> Result<ShardFanoutPreservedSourceReport, ShardFanoutPreserveSourceError> {
    let source_path = &plan.source_database_path;
    let preserved_path = &plan.preserved_source_database_path;
    let source_metadata = fs::metadata(source_path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ShardFanoutPreserveSourceError::MissingSource {
            path: source_path.clone(),
        },
        _ => ShardFanoutPreserveSourceError::SourceRead {
            path: source_path.clone(),
            message: error.to_string(),
        },
    })?;
    if !source_metadata.is_file() {
        return Err(ShardFanoutPreserveSourceError::SourceRead {
            path: source_path.clone(),
            message: "source path is not a regular file".to_owned(),
        });
    }

    let (source_hash, source_size_bytes) = blake3_file_hash(source_path).map_err(|error| {
        ShardFanoutPreserveSourceError::SourceRead {
            path: source_path.clone(),
            message: error.to_string(),
        }
    })?;

    let copied = match fs::metadata(preserved_path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(ShardFanoutPreserveSourceError::PreservedRead {
                    path: preserved_path.clone(),
                    message: "preserved path is not a regular file".to_owned(),
                });
            }
            false
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::copy(source_path, preserved_path).map_err(|error| {
                ShardFanoutPreserveSourceError::PreserveWrite {
                    path: preserved_path.clone(),
                    message: error.to_string(),
                }
            })?;
            true
        }
        Err(error) => {
            return Err(ShardFanoutPreserveSourceError::PreservedRead {
                path: preserved_path.clone(),
                message: error.to_string(),
            });
        }
    };

    let (preserved_hash, preserved_size_bytes) =
        blake3_file_hash(preserved_path).map_err(|error| {
            ShardFanoutPreserveSourceError::PreservedRead {
                path: preserved_path.clone(),
                message: error.to_string(),
            }
        })?;
    if source_hash != preserved_hash || source_size_bytes != preserved_size_bytes {
        return Err(ShardFanoutPreserveSourceError::PreservedSourceMismatch {
            source_path: source_path.clone(),
            preserved_path: preserved_path.clone(),
            source_hash,
            preserved_hash,
        });
    }

    Ok(ShardFanoutPreservedSourceReport {
        schema: SHARD_FANOUT_PRESERVE_SOURCE_SCHEMA_V1,
        source_path: source_path.clone(),
        preserved_path: preserved_path.clone(),
        copied,
        source_hash: source_hash.clone(),
        preserved_hash,
        source_size_bytes,
        preserved_size_bytes,
        rollback_ready: true,
    })
}

#[must_use]
pub fn plan_peer_shard_attach(input: PeerShardAttachPlanInput) -> PeerShardAttachPlan {
    let local_workspace_id = input.local_workspace_id.trim().to_owned();
    let mut degraded = Vec::new();

    if !input.enabled {
        return PeerShardAttachPlan {
            schema: SHARD_FANOUT_ATTACH_PLAN_SCHEMA_V1,
            enabled: false,
            strict: input.strict,
            local_workspace_id,
            shard_root: None,
            catalog_path: None,
            targets: Vec::new(),
            attachable_count: 0,
            blocked: false,
            degraded,
        };
    }

    let shard_root_result = input
        .shards_dir_override
        .as_deref()
        .map_or_else(default_shards_dir_from_env, |path| Ok(PathBuf::from(path)))
        .and_then(|path| normalize_shard_root(&path));
    let (shard_root, catalog_path, root_degradation) = match shard_root_result {
        Ok(root) => {
            let catalog_path = catalog_path_for_shard_root(&root);
            (Some(root), Some(catalog_path), None)
        }
        Err(error) => {
            let degradation = error.degradation();
            degraded.push(degradation.clone());
            (None, None, Some(degradation))
        }
    };

    let mut peers = BTreeSet::new();
    for workspace_id in input.peer_workspace_ids {
        let trimmed = workspace_id.trim().to_owned();
        if trimmed.is_empty() || trimmed == local_workspace_id {
            continue;
        }
        peers.insert(trimmed);
    }

    let mut targets = Vec::with_capacity(peers.len());
    for (attach_index, workspace_id) in peers.into_iter().enumerate() {
        let attach_alias = format!("peer_{attach_index:04}");
        let mut target_degraded = Vec::new();
        let (shard_id, shard_path) = match shard_id_for_workspace_id(&workspace_id) {
            Ok(shard_id) => {
                let shard_path = shard_root
                    .as_deref()
                    .map(|root| shard_file_path(root, &shard_id));
                (Some(shard_id), shard_path)
            }
            Err(_) => {
                target_degraded.push(unsafe_workspace_id_degradation());
                (None, None)
            }
        };

        if let Some(degradation) = &root_degradation {
            target_degraded.push(degradation.clone());
        }

        let shard_exists = shard_path.as_ref().is_some_and(|path| path.exists());
        if shard_id.is_some() && root_degradation.is_none() && !shard_exists {
            target_degraded.push(shard_attach_failed_degradation());
        }

        let attachable = shard_exists && target_degraded.is_empty();
        let read_only_uri = shard_path
            .as_ref()
            .map(|path| sqlite_read_only_file_uri(path.as_path()));
        let attach_sql = read_only_uri.as_ref().map(|uri| {
            format!(
                "ATTACH DATABASE {} AS {}",
                sqlite_string_literal(uri),
                sqlite_identifier(&attach_alias)
            )
        });
        degraded.extend(target_degraded.iter().cloned());
        targets.push(PeerShardAttachTarget {
            workspace_id,
            shard_id,
            attach_alias,
            shard_path,
            read_only_uri,
            attach_sql,
            shard_exists,
            attachable,
            degraded: target_degraded,
        });
    }

    let attachable_count = targets.iter().filter(|target| target.attachable).count();
    let blocked = input.strict && attachable_count != targets.len();

    PeerShardAttachPlan {
        schema: SHARD_FANOUT_ATTACH_PLAN_SCHEMA_V1,
        enabled: true,
        strict: input.strict,
        local_workspace_id,
        shard_root,
        catalog_path,
        targets,
        attachable_count,
        blocked,
        degraded,
    }
}

pub fn execute_peer_shard_read_attach_plan(
    connection: &DbConnection,
    plan: &PeerShardAttachPlan,
) -> PeerShardAttachExecution {
    let mut targets = Vec::new();
    let mut degraded = Vec::new();
    let mut attempted_count = 0usize;
    let mut attached_count = 0usize;

    if !plan.enabled || plan.blocked {
        return PeerShardAttachExecution {
            schema: SHARD_FANOUT_ATTACH_EXECUTION_SCHEMA_V1,
            attempted_count,
            attached_count,
            blocked: plan.blocked,
            query_only_sql: "PRAGMA query_only = ON",
            targets,
            degraded: plan.degraded.clone(),
        };
    }

    for target in &plan.targets {
        if !target.attachable {
            targets.push(PeerShardAttachExecutionTarget {
                workspace_id: target.workspace_id.clone(),
                attach_alias: target.attach_alias.clone(),
                attached: false,
                degraded: target.degraded.clone(),
            });
            continue;
        }

        attempted_count = attempted_count.saturating_add(1);
        let Some(attach_sql) = target.attach_sql.as_deref() else {
            let failure = shard_attach_failed_degradation();
            degraded.push(failure.clone());
            targets.push(PeerShardAttachExecutionTarget {
                workspace_id: target.workspace_id.clone(),
                attach_alias: target.attach_alias.clone(),
                attached: false,
                degraded: vec![failure],
            });
            continue;
        };

        match connection.execute_read_snapshot_raw(super::DbOperation::Execute, attach_sql) {
            Ok(()) => {
                attached_count = attached_count.saturating_add(1);
                targets.push(PeerShardAttachExecutionTarget {
                    workspace_id: target.workspace_id.clone(),
                    attach_alias: target.attach_alias.clone(),
                    attached: true,
                    degraded: Vec::new(),
                });
            }
            Err(_) => {
                let failure = shard_attach_failed_degradation();
                degraded.push(failure.clone());
                targets.push(PeerShardAttachExecutionTarget {
                    workspace_id: target.workspace_id.clone(),
                    attach_alias: target.attach_alias.clone(),
                    attached: false,
                    degraded: vec![failure],
                });
            }
        }
    }

    if attached_count > 0
        && connection
            .execute_read_snapshot_raw(super::DbOperation::Execute, "PRAGMA query_only = ON")
            .is_err()
    {
        degraded.push(cross_shard_skew_detected_degradation());
    }

    PeerShardAttachExecution {
        schema: SHARD_FANOUT_ATTACH_EXECUTION_SCHEMA_V1,
        attempted_count,
        attached_count,
        blocked: false,
        query_only_sql: "PRAGMA query_only = ON",
        targets,
        degraded,
    }
}

#[must_use]
pub fn resolve_shard_fanout_status(input: ShardFanoutResolverInput) -> ShardFanoutStatusReport {
    let mut degraded = Vec::new();
    let mut recovery = Vec::new();
    let legacy_database_path = input
        .workspace_root
        .as_ref()
        .map(|root| root.join(".ee").join("ee.db"));

    let shard_root_result = input
        .shards_dir_override
        .as_deref()
        .map_or_else(default_shards_dir_from_env, |path| Ok(PathBuf::from(path)))
        .and_then(|path| normalize_shard_root(&path));

    let shard_root = match shard_root_result {
        Ok(root) => root,
        Err(error) => {
            if input.enabled {
                degraded.push(error.degradation());
            }
            PathBuf::new()
        }
    };
    let data_root = shard_root
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    let catalog_path = if shard_root.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        catalog_path_for_shard_root(&shard_root)
    };

    let mut shard_id = None;
    let mut shard_path = None;
    if let Some(workspace_id) = input.workspace_id.as_deref() {
        match shard_id_for_workspace_id(workspace_id) {
            Ok(id) => {
                if !shard_root.as_os_str().is_empty() {
                    shard_path = Some(shard_file_path(&shard_root, &id));
                }
                shard_id = Some(id);
            }
            Err(_) => {
                if input.enabled {
                    degraded.push(unsafe_workspace_id_degradation());
                }
            }
        }
    }

    if input.enabled && input.workspace_id.is_none() {
        degraded.push(ShardFanoutDegradation::new(
            SHARD_FANOUT_WORKSPACE_UNAVAILABLE_CODE,
            "warning",
            "Shard fan-out is enabled but no workspace was selected for inspection.",
            "Pass --workspace or run ee init --workspace . before relying on shard routing.",
        ));
    }

    let catalog_exists = catalog_path.exists();
    let shard_exists = shard_path.as_ref().is_some_and(|path| path.exists());

    let posture = if !input.enabled {
        ShardFanoutPosture::Disabled
    } else if degraded
        .iter()
        .any(|entry| matches!(entry.severity, "high" | "critical"))
    {
        ShardFanoutPosture::Degraded
    } else if input.workspace_id.is_none() {
        ShardFanoutPosture::NotInspected
    } else if !catalog_exists {
        degraded.push(ShardFanoutDegradation::new(
            SHARD_FANOUT_CATALOG_MISSING_CODE,
            "warning",
            "Shard fan-out is enabled but the catalog database is missing.",
            "Run ee migrate shard-fanout --workspace . --dry-run --json.",
        ));
        recovery.push(ShardFanoutRecoveryAction {
            priority: 1,
            kind: "dry_run",
            command: "ee migrate shard-fanout --workspace . --dry-run --json",
        });
        ShardFanoutPosture::MigrationRequired
    } else if !shard_exists {
        degraded.push(ShardFanoutDegradation::new(
            SHARD_FANOUT_SHARD_MISSING_CODE,
            "warning",
            "Shard fan-out catalog exists but the selected workspace shard is missing.",
            "Run ee migrate shard-fanout --workspace . --dry-run --json.",
        ));
        recovery.push(ShardFanoutRecoveryAction {
            priority: 1,
            kind: "dry_run",
            command: "ee migrate shard-fanout --workspace . --dry-run --json",
        });
        ShardFanoutPosture::MigrationRequired
    } else {
        ShardFanoutPosture::Enabled
    };

    let migration_state = match posture {
        ShardFanoutPosture::Disabled => "legacy_active",
        ShardFanoutPosture::Enabled => "authoritative",
        ShardFanoutPosture::MigrationRequired => "migration_required",
        ShardFanoutPosture::Degraded => "blocked",
        ShardFanoutPosture::NotInspected => "not_inspected",
    };

    ShardFanoutStatusReport {
        schema: SHARD_FANOUT_STATUS_SCHEMA_V1,
        enabled: input.enabled,
        posture,
        workspace_id: input.workspace_id,
        workspace_root: input.workspace_root,
        legacy_database_path,
        data_root,
        shard_root,
        catalog_path,
        shard_path,
        shard_id,
        catalog_exists,
        shard_exists,
        catalog_contract: ShardCatalogContractReport::default(),
        shard_generation: None,
        migration_state,
        last_verified_hashes: vec![
            ShardVerificationHash {
                name: "source_db_hash",
                value: None,
            },
            ShardVerificationHash {
                name: "target_shard_hash",
                value: None,
            },
            ShardVerificationHash {
                name: "catalog_hash",
                value: None,
            },
        ],
        degraded,
        recovery,
    }
}

fn unsafe_workspace_id_degradation() -> ShardFanoutDegradation {
    ShardFanoutDegradation::new(
        SHARD_FANOUT_WORKSPACE_ID_UNSAFE_CODE,
        "high",
        "Shard fan-out refused an unsafe workspace ID for path derivation.",
        "Re-resolve the workspace through ee workspace status before enabling shard fan-out.",
    )
}

fn shard_attach_failed_degradation() -> ShardFanoutDegradation {
    ShardFanoutDegradation::new(
        SHARD_ATTACH_FAILED_CODE,
        "warning",
        "A peer workspace shard could not be attached for read fan-out.",
        "Verify the peer shard exists or run ee migrate shard-fanout --workspace . --dry-run --json.",
    )
}

fn cross_shard_skew_detected_degradation() -> ShardFanoutDegradation {
    ShardFanoutDegradation::new(
        CROSS_SHARD_SKEW_DETECTED_CODE,
        "warning",
        "Cross-shard read planning detected inconsistent peer shard state.",
        "Re-run the search with a fresh shard catalog snapshot or inspect ee status --json.",
    )
}

fn sqlite_read_only_file_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for byte in path.to_string_lossy().as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(char::from(*byte));
            }
            byte => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri.push_str("?mode=ro");
    uri
}

fn sqlite_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn sqlite_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
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
        bytes = bytes.saturating_add(read as u64);
    }
    Ok((format!("blake3:{}", hasher.finalize().to_hex()), bytes))
}

impl From<OsString> for ShardFanoutResolverInput {
    fn from(value: OsString) -> Self {
        Self {
            shards_dir_override: Some(PathBuf::from(value)),
            ..Self::default()
        }
    }
}

fn trace_router_event(
    phase: &'static str,
    request_id: Option<&str>,
    status: &ShardFanoutStatusReport,
    started: Instant,
) {
    let degraded_codes = status
        .degraded
        .iter()
        .map(|entry| entry.code)
        .collect::<Vec<_>>()
        .join(",");
    tracing::info!(
        target: "ee::db::shard",
        surface = "shard_fanout",
        phase,
        workspace_id = status.workspace_id.as_deref().unwrap_or(""),
        shard_id = status.shard_id.as_deref().unwrap_or(""),
        request_id = request_id.unwrap_or(""),
        elapsed_ms = started.elapsed().as_millis() as u64,
        degraded_codes = %degraded_codes,
        "database shard router"
    );
}

#[cfg(test)]
mod tests {
    use super::{
        DbShardRouter, DbShardRouterError, DbShardRoutingMode, PRE_SHARD_FANOUT_FILE_NAME,
        PeerShardAttachPlanInput, SHARD_ATTACH_FAILED_CODE, SHARD_CATALOG_FILE_NAME,
        SHARD_FANOUT_ATTACH_EXECUTION_SCHEMA_V1, SHARD_FANOUT_ATTACH_PLAN_SCHEMA_V1,
        SHARD_FANOUT_CATALOG_MISSING_CODE, SHARD_FANOUT_MIGRATION_PLAN_SCHEMA_V1,
        SHARD_FANOUT_PRESERVE_SOURCE_SCHEMA_V1, SHARD_FANOUT_STATUS_SCHEMA_V1,
        SHARD_FANOUT_WORKSPACE_ID_UNSAFE_CODE, ShardFanoutMigrationPlanInput,
        ShardFanoutMigrationWorkspaceInput, ShardFanoutPosture, ShardFanoutPreserveSourceError,
        ShardFanoutResolverInput, default_shards_dir_from_values,
        execute_peer_shard_read_attach_plan, normalize_shard_root, plan_peer_shard_attach,
        plan_shard_fanout_migration, preserve_shard_fanout_source_database,
        preserved_legacy_database_path, resolve_shard_fanout_status,
        shard_fanout_enabled_from_env_value, shard_file_path,
    };
    use crate::db::{DatabaseConfig, DbConnection};
    use std::path::{Path, PathBuf};

    type TestResult = Result<(), String>;

    fn temp_root(label: &str) -> Result<tempfile::TempDir, String> {
        tempfile::Builder::new()
            .prefix(label)
            .tempdir()
            .map_err(|error| error.to_string())
    }

    #[test]
    fn env_truthy_parser_accepts_explicit_enable_values() {
        assert!(shard_fanout_enabled_from_env_value(Some("1")));
        assert!(shard_fanout_enabled_from_env_value(Some("true")));
        assert!(shard_fanout_enabled_from_env_value(Some("YES")));
        assert!(!shard_fanout_enabled_from_env_value(Some("0")));
        assert!(!shard_fanout_enabled_from_env_value(None));
    }

    #[test]
    fn stable_path_derivation_uses_workspace_id_filename() -> TestResult {
        let root = Path::new("/tmp/ee-shards");
        let path = shard_file_path(root, "wsp_0123456789ABCDEFGHJKMNPQRS");
        if path == PathBuf::from("/tmp/ee-shards/wsp_0123456789ABCDEFGHJKMNPQRS.db") {
            Ok(())
        } else {
            Err(format!("unexpected shard path: {}", path.display()))
        }
    }

    #[test]
    fn disabled_fallback_does_not_require_catalog_or_shard() -> TestResult {
        let temp = temp_root("ee-shard-disabled")?;
        let report = resolve_shard_fanout_status(ShardFanoutResolverInput {
            enabled: false,
            workspace_id: Some("wsp_disabled".to_owned()),
            workspace_root: Some(temp.path().join("workspace")),
            shards_dir_override: Some(temp.path().join("shards")),
        });

        assert_eq!(report.schema, SHARD_FANOUT_STATUS_SCHEMA_V1);
        assert_eq!(report.posture, ShardFanoutPosture::Disabled);
        assert!(report.degraded.is_empty());
        assert_eq!(report.migration_state, "legacy_active");
        assert!(!report.catalog_exists);
        assert!(!report.shard_exists);
        Ok(())
    }

    #[test]
    fn disabled_fallback_ignores_unsafe_planning_inputs() -> TestResult {
        let report = resolve_shard_fanout_status(ShardFanoutResolverInput {
            enabled: false,
            workspace_id: Some("../unsafe".to_owned()),
            workspace_root: Some(PathBuf::from("relative-workspace")),
            shards_dir_override: Some(PathBuf::from("relative-shards")),
        });

        assert_eq!(report.posture, ShardFanoutPosture::Disabled);
        assert!(report.degraded.is_empty());
        assert!(report.recovery.is_empty());
        assert_eq!(report.migration_state, "legacy_active");
        assert!(report.shard_path.is_none());
        Ok(())
    }

    #[test]
    fn enabled_missing_catalog_reports_migration_required() -> TestResult {
        let temp = temp_root("ee-shard-missing-catalog")?;
        let report = resolve_shard_fanout_status(ShardFanoutResolverInput {
            enabled: true,
            workspace_id: Some("wsp_missing_catalog".to_owned()),
            workspace_root: Some(temp.path().join("workspace")),
            shards_dir_override: Some(temp.path().join("shards")),
        });

        assert_eq!(report.posture, ShardFanoutPosture::MigrationRequired);
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == SHARD_FANOUT_CATALOG_MISSING_CODE)
        );
        assert_eq!(
            report.recovery.first().map(|action| action.command),
            Some("ee migrate shard-fanout --workspace . --dry-run --json")
        );
        Ok(())
    }

    #[test]
    fn router_disabled_returns_legacy_database_handle() -> TestResult {
        let temp = temp_root("ee-shard-router-legacy")?;
        let workspace_root = temp.path().join("workspace");
        let router = DbShardRouter::resolve(ShardFanoutResolverInput {
            enabled: false,
            workspace_id: Some("wsp_router_legacy".to_owned()),
            workspace_root: Some(workspace_root.clone()),
            shards_dir_override: Some(temp.path().join("shards")),
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(router.status().posture, ShardFanoutPosture::Disabled);
        assert_eq!(router.handle().routing_mode, DbShardRoutingMode::Legacy);
        assert_eq!(router.handle().workspace_id, "wsp_router_legacy");
        assert_eq!(
            router.handle().database_path,
            workspace_root.join(".ee").join("ee.db")
        );
        assert_eq!(
            router.handle().shard_id.as_deref(),
            Some("wsp_router_legacy")
        );
        Ok(())
    }

    #[test]
    fn router_enabled_returns_authoritative_shard_handle() -> TestResult {
        let temp = temp_root("ee-shard-router-authoritative")?;
        let data_root = temp.path().join("data");
        let shard_root = data_root.join("shards");
        std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;
        std::fs::write(data_root.join(SHARD_CATALOG_FILE_NAME), b"catalog")
            .map_err(|error| error.to_string())?;
        let expected_shard = shard_file_path(&shard_root, "wsp_router_shard");
        std::fs::write(&expected_shard, b"shard").map_err(|error| error.to_string())?;

        let router = DbShardRouter::resolve(ShardFanoutResolverInput {
            enabled: true,
            workspace_id: Some("wsp_router_shard".to_owned()),
            workspace_root: Some(temp.path().join("workspace")),
            shards_dir_override: Some(shard_root.clone()),
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(router.status().posture, ShardFanoutPosture::Enabled);
        assert_eq!(
            router.handle().routing_mode,
            DbShardRoutingMode::ShardFanout
        );
        assert_eq!(router.handle().workspace_id, "wsp_router_shard");
        assert_eq!(router.handle().database_path, expected_shard);
        assert_eq!(
            router.handle().catalog_path,
            data_root.join(SHARD_CATALOG_FILE_NAME)
        );
        assert_eq!(
            router.handle().shard_id.as_deref(),
            Some("wsp_router_shard")
        );
        Ok(())
    }

    #[test]
    fn router_enabled_refuses_non_authoritative_shard_layout() -> TestResult {
        let temp = temp_root("ee-shard-router-missing")?;
        let error = DbShardRouter::resolve(ShardFanoutResolverInput {
            enabled: true,
            workspace_id: Some("wsp_router_missing".to_owned()),
            workspace_root: Some(temp.path().join("workspace")),
            shards_dir_override: Some(temp.path().join("shards")),
        })
        .expect_err("missing catalog must not produce an authoritative router");

        match error {
            DbShardRouterError::ShardNotAuthoritative {
                posture,
                degraded_codes,
            } => {
                assert_eq!(posture, ShardFanoutPosture::MigrationRequired);
                assert!(degraded_codes.contains(&SHARD_FANOUT_CATALOG_MISSING_CODE));
                Ok(())
            }
            other => Err(format!("unexpected router error: {other}")),
        }
    }

    #[test]
    fn path_normalization_rejects_relative_and_parent_components() -> TestResult {
        assert!(normalize_shard_root(Path::new("relative/shards")).is_err());
        assert!(normalize_shard_root(Path::new("/tmp/../shards")).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn path_normalization_rejects_existing_symlink_component() -> TestResult {
        let temp = temp_root("ee-shard-symlink")?;
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        std::fs::create_dir_all(&real).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&real, &link).map_err(|error| error.to_string())?;
        let result = normalize_shard_root(&link.join("shards"));
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn default_shards_dir_prefers_xdg_data_home_shape_without_creating_paths() -> TestResult {
        let temp = temp_root("ee-shard-default")?;
        let path = default_shards_dir_from_values(
            Some(temp.path().as_os_str().to_os_string()),
            Some(temp.path().join("home").into_os_string()),
        );

        assert_eq!(
            path.map_err(|error| format!("{error:?}"))?,
            temp.path().join("ee/shards")
        );
        assert!(!temp.path().join("ee").exists());
        Ok(())
    }

    #[test]
    fn preserved_legacy_database_path_uses_hidden_rollback_file() {
        let source = Path::new("/workspace/.ee/ee.db");
        assert_eq!(
            preserved_legacy_database_path(source),
            PathBuf::from("/workspace/.ee").join(PRE_SHARD_FANOUT_FILE_NAME)
        );
    }

    #[test]
    fn migration_plan_sorts_workspaces_and_preserves_source_database() -> TestResult {
        let temp = temp_root("ee-shard-migration-plan")?;
        let source_database_path = temp.path().join("workspace/.ee/ee.db");
        let shard_root = temp.path().join("data/shards");
        let plan = plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
            source_database_path: source_database_path.clone(),
            shards_dir_override: Some(shard_root.clone()),
            workspaces: vec![
                ShardFanoutMigrationWorkspaceInput {
                    workspace_id: "wsp_b".to_owned(),
                    workspace_root: temp.path().join("workspace-b"),
                },
                ShardFanoutMigrationWorkspaceInput {
                    workspace_id: "wsp_a".to_owned(),
                    workspace_root: temp.path().join("workspace-a"),
                },
            ],
        });

        assert_eq!(plan.schema, SHARD_FANOUT_MIGRATION_PLAN_SCHEMA_V1);
        assert!(plan.dry_run);
        assert!(plan.blockers.is_empty());
        assert_eq!(plan.shard_root.as_deref(), Some(shard_root.as_path()));
        assert_eq!(
            plan.catalog_path.as_deref(),
            Some(temp.path().join("data/catalog.db").as_path())
        );
        assert_eq!(
            plan.preserved_source_database_path,
            source_database_path
                .parent()
                .expect("source db has parent")
                .join(PRE_SHARD_FANOUT_FILE_NAME)
        );
        assert_eq!(
            plan.workspaces
                .iter()
                .map(|workspace| workspace.workspace_id.as_str())
                .collect::<Vec<_>>(),
            vec!["wsp_a", "wsp_b"]
        );
        assert_eq!(
            plan.workspaces[0].shard_path.as_deref(),
            Some(shard_root.join("wsp_a.db").as_path())
        );
        assert_eq!(
            plan.workspaces[1].shard_path.as_deref(),
            Some(shard_root.join("wsp_b.db").as_path())
        );
        assert_eq!(plan.expected_audit_rows.len(), 3);
        assert_eq!(
            plan.expected_audit_rows
                .iter()
                .map(|row| row.event)
                .collect::<Vec<_>>(),
            vec![
                "preserve_legacy_database",
                "copy_workspace_to_shard",
                "copy_workspace_to_shard"
            ]
        );
        Ok(())
    }

    #[test]
    fn migration_plan_hashes_existing_source_database() -> TestResult {
        let temp = temp_root("ee-shard-migration-source-hash")?;
        let source_database_path = temp.path().join("workspace/.ee/ee.db");
        std::fs::create_dir_all(
            source_database_path
                .parent()
                .expect("source database has parent"),
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(&source_database_path, b"legacy-db-v1")
            .map_err(|error| error.to_string())?;

        let plan = plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
            source_database_path,
            shards_dir_override: Some(temp.path().join("data/shards")),
            workspaces: Vec::new(),
        });
        let expected_hash = format!("blake3:{}", blake3::hash(b"legacy-db-v1").to_hex());

        assert_eq!(
            plan.source_database_hash.as_deref(),
            Some(expected_hash.as_str())
        );
        Ok(())
    }

    #[test]
    fn preserve_source_database_copies_once_and_is_idempotent() -> TestResult {
        let temp = temp_root("ee-shard-preserve-source")?;
        let source_database_path = temp.path().join("workspace/.ee/ee.db");
        std::fs::create_dir_all(
            source_database_path
                .parent()
                .expect("source database has parent"),
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(&source_database_path, b"legacy-db-v1")
            .map_err(|error| error.to_string())?;
        let plan = plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
            source_database_path: source_database_path.clone(),
            shards_dir_override: Some(temp.path().join("data/shards")),
            workspaces: Vec::new(),
        });
        let expected_hash = plan
            .source_database_hash
            .clone()
            .ok_or_else(|| "plan should hash existing source database".to_owned())?;

        let first =
            preserve_shard_fanout_source_database(&plan).map_err(|error| error.to_string())?;
        assert_eq!(first.schema, SHARD_FANOUT_PRESERVE_SOURCE_SCHEMA_V1);
        assert!(first.copied);
        assert!(first.rollback_ready);
        assert_eq!(first.source_path, source_database_path);
        assert_eq!(first.preserved_path, plan.preserved_source_database_path);
        assert_eq!(first.source_hash, expected_hash);
        assert_eq!(first.source_hash, first.preserved_hash);
        assert_eq!(first.source_size_bytes, first.preserved_size_bytes);

        let second =
            preserve_shard_fanout_source_database(&plan).map_err(|error| error.to_string())?;
        assert!(!second.copied);
        assert_eq!(second.source_hash, first.source_hash);
        assert_eq!(second.preserved_hash, first.preserved_hash);
        Ok(())
    }

    #[test]
    fn preserve_source_database_refuses_mismatched_existing_rollback_file() -> TestResult {
        let temp = temp_root("ee-shard-preserve-mismatch")?;
        let source_database_path = temp.path().join("workspace/.ee/ee.db");
        std::fs::create_dir_all(
            source_database_path
                .parent()
                .expect("source database has parent"),
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(&source_database_path, b"legacy-db-v1")
            .map_err(|error| error.to_string())?;
        let plan = plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
            source_database_path,
            shards_dir_override: Some(temp.path().join("data/shards")),
            workspaces: Vec::new(),
        });
        std::fs::write(
            &plan.preserved_source_database_path,
            b"stale-preserved-copy",
        )
        .map_err(|error| error.to_string())?;

        let error =
            preserve_shard_fanout_source_database(&plan).expect_err("mismatch must fail closed");
        match error {
            ShardFanoutPreserveSourceError::PreservedSourceMismatch {
                source_path,
                preserved_path,
                source_hash,
                preserved_hash,
            } => {
                assert_eq!(source_path, plan.source_database_path);
                assert_eq!(preserved_path, plan.preserved_source_database_path);
                assert_ne!(source_hash, preserved_hash);
                Ok(())
            }
            other => Err(format!("unexpected preserve error: {other}")),
        }
    }

    #[test]
    fn migration_plan_json_is_deterministic_for_workspace_input_order() -> TestResult {
        let temp = temp_root("ee-shard-migration-deterministic")?;
        let source_database_path = temp.path().join("workspace/.ee/ee.db");
        let shard_root = temp.path().join("data/shards");
        let first = plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
            source_database_path: source_database_path.clone(),
            shards_dir_override: Some(shard_root.clone()),
            workspaces: vec![
                ShardFanoutMigrationWorkspaceInput {
                    workspace_id: "wsp_z".to_owned(),
                    workspace_root: temp.path().join("workspace-z"),
                },
                ShardFanoutMigrationWorkspaceInput {
                    workspace_id: "wsp_a".to_owned(),
                    workspace_root: temp.path().join("workspace-a"),
                },
            ],
        });
        let second = plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
            source_database_path,
            shards_dir_override: Some(shard_root),
            workspaces: vec![
                ShardFanoutMigrationWorkspaceInput {
                    workspace_id: "wsp_a".to_owned(),
                    workspace_root: temp.path().join("workspace-a"),
                },
                ShardFanoutMigrationWorkspaceInput {
                    workspace_id: "wsp_z".to_owned(),
                    workspace_root: temp.path().join("workspace-z"),
                },
            ],
        });

        let first_json = serde_json::to_string(&first).map_err(|error| error.to_string())?;
        let second_json = serde_json::to_string(&second).map_err(|error| error.to_string())?;
        assert_eq!(first_json, second_json);
        Ok(())
    }

    #[test]
    fn migration_plan_marks_unsafe_workspace_id_as_blocker() -> TestResult {
        let temp = temp_root("ee-shard-migration-unsafe-id")?;
        let plan = plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
            source_database_path: temp.path().join("workspace/.ee/ee.db"),
            shards_dir_override: Some(temp.path().join("data/shards")),
            workspaces: vec![ShardFanoutMigrationWorkspaceInput {
                workspace_id: "../unsafe".to_owned(),
                workspace_root: temp.path().join("workspace"),
            }],
        });

        assert!(
            plan.blockers
                .iter()
                .any(|entry| entry.code == SHARD_FANOUT_WORKSPACE_ID_UNSAFE_CODE)
        );
        assert!(
            plan.workspaces[0]
                .blockers
                .iter()
                .any(|entry| entry.code == SHARD_FANOUT_WORKSPACE_ID_UNSAFE_CODE)
        );
        assert_eq!(plan.workspaces[0].shard_path, None);
        assert_eq!(plan.expected_audit_rows.len(), 1);
        assert_eq!(
            plan.expected_audit_rows[0].event,
            "preserve_legacy_database"
        );
        Ok(())
    }

    #[test]
    fn peer_attach_plan_sorts_dedupes_and_skips_local_workspace() -> TestResult {
        let temp = temp_root("ee-shard-peer-attach")?;
        let shard_root = temp.path().join("data/shards");
        std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;
        std::fs::write(shard_file_path(&shard_root, "wsp_a"), b"a")
            .map_err(|error| error.to_string())?;
        std::fs::write(shard_file_path(&shard_root, "wsp_b"), b"b")
            .map_err(|error| error.to_string())?;

        let plan = plan_peer_shard_attach(PeerShardAttachPlanInput {
            enabled: true,
            strict: false,
            local_workspace_id: "wsp_local".to_owned(),
            shards_dir_override: Some(shard_root.clone()),
            peer_workspace_ids: vec![
                "wsp_b".to_owned(),
                "wsp_local".to_owned(),
                "wsp_a".to_owned(),
                "wsp_b".to_owned(),
            ],
        });

        assert_eq!(plan.schema, SHARD_FANOUT_ATTACH_PLAN_SCHEMA_V1);
        assert!(!plan.blocked);
        assert_eq!(plan.attachable_count, 2);
        assert!(plan.degraded.is_empty());
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| (target.workspace_id.as_str(), target.attach_alias.as_str()))
                .collect::<Vec<_>>(),
            vec![("wsp_a", "peer_0000"), ("wsp_b", "peer_0001")]
        );
        let expected_a = shard_root.join("wsp_a.db");
        let expected_b = shard_root.join("wsp_b.db");
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.shard_path.as_deref())
                .collect::<Vec<_>>(),
            vec![Some(expected_a.as_path()), Some(expected_b.as_path())]
        );
        assert!(
            plan.targets[0]
                .read_only_uri
                .as_deref()
                .is_some_and(|uri| uri.starts_with("file:") && uri.ends_with("?mode=ro"))
        );
        assert_eq!(
            plan.targets[0].attach_sql.as_deref(),
            Some(
                format!(
                    "ATTACH DATABASE '{}' AS \"peer_0000\"",
                    plan.targets[0]
                        .read_only_uri
                        .as_deref()
                        .expect("target has read-only URI")
                )
                .as_str()
            )
        );
        Ok(())
    }

    #[test]
    fn peer_attach_plan_missing_peer_degrades_without_blocking_best_effort_reads() -> TestResult {
        let temp = temp_root("ee-shard-peer-missing")?;
        let shard_root = temp.path().join("data/shards");
        std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;
        std::fs::write(shard_file_path(&shard_root, "wsp_present"), b"present")
            .map_err(|error| error.to_string())?;

        let plan = plan_peer_shard_attach(PeerShardAttachPlanInput {
            enabled: true,
            strict: false,
            local_workspace_id: "wsp_local".to_owned(),
            shards_dir_override: Some(shard_root),
            peer_workspace_ids: vec!["wsp_missing".to_owned(), "wsp_present".to_owned()],
        });

        assert!(!plan.blocked);
        assert_eq!(plan.attachable_count, 1);
        assert!(
            plan.degraded
                .iter()
                .any(|entry| entry.code == SHARD_ATTACH_FAILED_CODE)
        );
        let missing = plan
            .targets
            .iter()
            .find(|target| target.workspace_id == "wsp_missing")
            .ok_or_else(|| "missing target not present".to_owned())?;
        assert!(!missing.attachable);
        assert!(!missing.shard_exists);
        assert!(
            missing
                .degraded
                .iter()
                .any(|entry| entry.code == SHARD_ATTACH_FAILED_CODE)
        );
        Ok(())
    }

    #[test]
    fn peer_attach_plan_strict_mode_blocks_when_any_peer_is_unattachable() -> TestResult {
        let temp = temp_root("ee-shard-peer-strict")?;
        let shard_root = temp.path().join("data/shards");
        std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;

        let plan = plan_peer_shard_attach(PeerShardAttachPlanInput {
            enabled: true,
            strict: true,
            local_workspace_id: "wsp_local".to_owned(),
            shards_dir_override: Some(shard_root),
            peer_workspace_ids: vec!["wsp_missing".to_owned()],
        });

        assert!(plan.blocked);
        assert_eq!(plan.attachable_count, 0);
        assert_eq!(plan.targets.len(), 1);
        assert!(
            plan.degraded
                .iter()
                .any(|entry| entry.code == SHARD_ATTACH_FAILED_CODE)
        );
        Ok(())
    }

    #[test]
    fn peer_attach_plan_rejects_unsafe_peer_workspace_id() -> TestResult {
        let temp = temp_root("ee-shard-peer-unsafe")?;
        let shard_root = temp.path().join("data/shards");
        std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;

        let plan = plan_peer_shard_attach(PeerShardAttachPlanInput {
            enabled: true,
            strict: false,
            local_workspace_id: "wsp_local".to_owned(),
            shards_dir_override: Some(shard_root),
            peer_workspace_ids: vec!["../escape".to_owned()],
        });

        assert_eq!(plan.targets.len(), 1);
        assert!(!plan.targets[0].attachable);
        assert_eq!(plan.targets[0].shard_path, None);
        assert!(
            plan.degraded
                .iter()
                .any(|entry| entry.code == SHARD_FANOUT_WORKSPACE_ID_UNSAFE_CODE)
        );
        Ok(())
    }

    #[test]
    fn peer_attach_plan_json_is_deterministic_for_peer_input_order() -> TestResult {
        let temp = temp_root("ee-shard-peer-json")?;
        let shard_root = temp.path().join("data/shards");
        std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;
        std::fs::write(shard_file_path(&shard_root, "wsp_a"), b"a")
            .map_err(|error| error.to_string())?;
        std::fs::write(shard_file_path(&shard_root, "wsp_b"), b"b")
            .map_err(|error| error.to_string())?;

        let first = plan_peer_shard_attach(PeerShardAttachPlanInput {
            enabled: true,
            strict: false,
            local_workspace_id: "wsp_local".to_owned(),
            shards_dir_override: Some(shard_root.clone()),
            peer_workspace_ids: vec!["wsp_b".to_owned(), "wsp_a".to_owned()],
        });
        let second = plan_peer_shard_attach(PeerShardAttachPlanInput {
            enabled: true,
            strict: false,
            local_workspace_id: "wsp_local".to_owned(),
            shards_dir_override: Some(shard_root),
            peer_workspace_ids: vec!["wsp_a".to_owned(), "wsp_b".to_owned()],
        });

        let first_json = serde_json::to_string(&first).map_err(|error| error.to_string())?;
        let second_json = serde_json::to_string(&second).map_err(|error| error.to_string())?;
        assert_eq!(first_json, second_json);
        Ok(())
    }

    #[test]
    fn peer_attach_plan_encodes_read_only_uri_for_spaces_and_quotes() -> TestResult {
        let temp = temp_root("ee-shard-peer-uri with spaces")?;
        let shard_root = temp.path().join("data with spaces/shards");
        std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;
        std::fs::write(shard_file_path(&shard_root, "wsp_peer"), b"peer")
            .map_err(|error| error.to_string())?;

        let plan = plan_peer_shard_attach(PeerShardAttachPlanInput {
            enabled: true,
            strict: false,
            local_workspace_id: "wsp_local".to_owned(),
            shards_dir_override: Some(shard_root),
            peer_workspace_ids: vec!["wsp_peer".to_owned()],
        });

        let target = &plan.targets[0];
        let uri = target
            .read_only_uri
            .as_deref()
            .ok_or_else(|| "target should carry read-only URI".to_owned())?;
        assert!(uri.starts_with("file:"));
        assert!(uri.contains("%20"));
        assert!(uri.ends_with("?mode=ro"));
        assert_eq!(
            target.attach_sql.as_deref(),
            Some(format!("ATTACH DATABASE '{uri}' AS \"peer_0000\"").as_str())
        );
        Ok(())
    }

    #[test]
    fn peer_attach_execution_keeps_best_effort_read_targets() -> TestResult {
        let temp = temp_root("ee-shard-peer-exec")?;
        let shard_root = temp.path().join("data/shards");
        std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;
        DbConnection::open(DatabaseConfig::file(shard_file_path(
            &shard_root,
            "wsp_present",
        )))
        .map_err(|error| error.to_string())?;

        let plan = plan_peer_shard_attach(PeerShardAttachPlanInput {
            enabled: true,
            strict: false,
            local_workspace_id: "wsp_local".to_owned(),
            shards_dir_override: Some(shard_root),
            peer_workspace_ids: vec!["wsp_missing".to_owned(), "wsp_present".to_owned()],
        });
        let connection =
            DbConnection::open(DatabaseConfig::memory()).map_err(|error| error.to_string())?;
        let execution = execute_peer_shard_read_attach_plan(&connection, &plan);

        assert_eq!(execution.schema, SHARD_FANOUT_ATTACH_EXECUTION_SCHEMA_V1);
        assert_eq!(execution.attempted_count, 1);
        assert_eq!(execution.targets.len(), 2);
        assert!(
            execution
                .targets
                .iter()
                .any(|target| target.workspace_id == "wsp_missing" && !target.attached)
        );
        assert!(
            execution
                .degraded
                .iter()
                .all(|entry| entry.code != SHARD_ATTACH_FAILED_CODE),
            "planned missing shards are represented on their target, not as execution failures"
        );
        Ok(())
    }

    #[test]
    fn status_json_serialization_is_deterministic() -> TestResult {
        let temp = temp_root("ee-shard-json")?;
        let input = ShardFanoutResolverInput {
            enabled: true,
            workspace_id: Some("wsp_json".to_owned()),
            workspace_root: Some(temp.path().join("workspace")),
            shards_dir_override: Some(temp.path().join("shards")),
        };
        let first = serde_json::to_string(&resolve_shard_fanout_status(input.clone()))
            .map_err(|error| error.to_string())?;
        let second = serde_json::to_string(&resolve_shard_fanout_status(input))
            .map_err(|error| error.to_string())?;
        assert_eq!(first, second);
        Ok(())
    }
}
