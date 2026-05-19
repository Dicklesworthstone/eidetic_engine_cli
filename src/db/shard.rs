//! Read-only planning primitives for per-workspace database shard fan-out.
//!
//! This module intentionally does not open or create databases. It only derives
//! the catalog and shard paths that later migration/router beads can materialize.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use super::{DatabaseConfig, DbConnection};

pub const SHARD_FANOUT_STATUS_SCHEMA_V1: &str = "ee.shard_fanout.status.v1";
pub const SHARD_FANOUT_CATALOG_SCHEMA_VERSION: u32 = 1;
pub const SHARD_CATALOG_FILE_NAME: &str = "catalog.db";
pub const SHARD_FILE_EXTENSION: &str = "db";

pub const SHARD_FANOUT_ROOT_UNSAFE_CODE: &str = "shard_fanout_root_unsafe";
pub const SHARD_FANOUT_HOME_UNAVAILABLE_CODE: &str = "shard_fanout_home_unavailable";
pub const SHARD_FANOUT_WORKSPACE_UNAVAILABLE_CODE: &str = "shard_fanout_workspace_unavailable";
pub const SHARD_FANOUT_WORKSPACE_ID_UNSAFE_CODE: &str = "shard_fanout_workspace_id_unsafe";
pub const SHARD_FANOUT_CATALOG_MISSING_CODE: &str = "shard_fanout_catalog_missing";
pub const SHARD_FANOUT_SHARD_MISSING_CODE: &str = "shard_fanout_shard_missing";

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

#[must_use]
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
                    degraded.push(ShardFanoutDegradation::new(
                        SHARD_FANOUT_WORKSPACE_ID_UNSAFE_CODE,
                        "high",
                        "Shard fan-out refused an unsafe workspace ID for path derivation.",
                        "Re-resolve the workspace through ee workspace status before enabling shard fan-out.",
                    ));
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
        DbShardRouter, DbShardRouterError, DbShardRoutingMode, SHARD_CATALOG_FILE_NAME,
        SHARD_FANOUT_CATALOG_MISSING_CODE, SHARD_FANOUT_STATUS_SCHEMA_V1, ShardFanoutPosture,
        ShardFanoutResolverInput, default_shards_dir_from_values, normalize_shard_root,
        resolve_shard_fanout_status, shard_fanout_enabled_from_env_value, shard_file_path,
    };
    use std::ffi::OsString;
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
