//! Workspace registry, resolution, and alias commands.
//!
//! The registry is a local FrankenSQLite database that reuses the existing
//! `workspaces` table. `workspaces.name` is the stable human alias for a
//! workspace path, while the deterministic workspace ID remains path-derived.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::config::{
    WORKSPACE_MARKER, WorkspaceDiagnostic, WorkspaceResolutionMode, WorkspaceResolutionRequest,
    WorkspaceResolutionSource, WorkspaceScope, derive_workspace_scope,
    diagnose_workspace_resolution, resolve_workspace,
};
use crate::db::{
    CreateAuditInput, CreateWorkspaceInput, DatabaseConfig, DbConnection, StoredWorkspace,
    WorkspaceScopeFields, generate_audit_id,
};
use crate::models::{DomainError, WorkspaceId};

pub const WORKSPACE_REGISTRY_SCHEMA_V1: &str = "ee.workspace.registry.v1";
pub const WORKSPACE_ALIAS_SCHEMA_V1: &str = "ee.workspace.alias.v1";
pub const WORKSPACE_RESOLVE_SCHEMA_V1: &str = "ee.workspace.resolve.v1";
pub const WORKSPACE_REGISTRY_ENV_VAR: &str = "EE_WORKSPACE_REGISTRY";

const WORKSPACE_ALIAS_SET_ACTION: &str = "workspace.alias.set";
const WORKSPACE_ALIAS_CLEAR_ACTION: &str = "workspace.alias.clear";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceListOptions {
    pub registry_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceResolveOptions {
    pub workspace_path: Option<PathBuf>,
    pub target: Option<String>,
    pub registry_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceAliasOptions {
    pub workspace_path: Option<PathBuf>,
    pub pick: Option<String>,
    pub alias: Option<String>,
    pub clear: bool,
    pub dry_run: bool,
    pub registry_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    pub workspace_id: String,
    pub path: String,
    pub alias: Option<String>,
    pub scope_kind: String,
    pub repository_root: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub subproject_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<StoredWorkspace> for WorkspaceEntry {
    fn from(workspace: StoredWorkspace) -> Self {
        Self {
            workspace_id: workspace.id,
            path: workspace.path,
            alias: workspace.name,
            scope_kind: workspace.scope_kind,
            repository_root: workspace.repository_root,
            repository_fingerprint: workspace.repository_fingerprint,
            subproject_path: workspace.subproject_path,
            created_at: workspace.created_at,
            updated_at: workspace.updated_at,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub registry_path: String,
    pub registry_exists: bool,
    pub workspaces: Vec<WorkspaceEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAliasReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub status: &'static str,
    pub registry_path: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub alias: Option<String>,
    pub previous_alias: Option<String>,
    pub scope_kind: String,
    pub repository_root: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub subproject_path: Option<String>,
    pub dry_run: bool,
    pub persisted: bool,
    pub audit_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResolveReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub source: String,
    pub target: Option<String>,
    pub workspace_id: String,
    pub root: String,
    pub canonical_root: String,
    pub marker_present: bool,
    pub alias: Option<String>,
    pub scope_kind: String,
    pub repository_root: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub subproject_path: Option<String>,
    pub registry_path: String,
    pub diagnostics: Vec<WorkspaceDiagnosticEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDiagnosticEntry {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
    pub selected_source: Option<&'static str>,
    pub selected_root: Option<String>,
    pub conflicting_source: Option<&'static str>,
    pub conflicting_root: Option<String>,
    pub marker_roots: Vec<String>,
}

impl From<WorkspaceDiagnostic> for WorkspaceDiagnosticEntry {
    fn from(diagnostic: WorkspaceDiagnostic) -> Self {
        Self {
            code: diagnostic.code,
            severity: diagnostic.severity.as_str(),
            message: diagnostic.message,
            repair: diagnostic.repair,
            selected_source: diagnostic
                .selected_source
                .map(WorkspaceResolutionSource::as_str),
            selected_root: diagnostic
                .selected_root
                .map(|path| path.display().to_string()),
            conflicting_source: diagnostic
                .conflicting_source
                .map(WorkspaceResolutionSource::as_str),
            conflicting_root: diagnostic
                .conflicting_root
                .map(|path| path.display().to_string()),
            marker_roots: diagnostic
                .marker_roots
                .into_iter()
                .map(|path| path.display().to_string())
                .collect(),
        }
    }
}

#[must_use]
pub fn registry_database_path_override(override_path: Option<&Path>) -> PathBuf {
    if let Some(path) = override_path {
        return path.to_path_buf();
    }
    if let Ok(path) = env::var(WORKSPACE_REGISTRY_ENV_VAR) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(xdg_data) = env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg_data).join("ee").join("workspaces.db");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("ee")
            .join("workspaces.db");
    }
    env::temp_dir().join("ee").join("workspaces.db")
}

#[must_use]
pub fn resolve_workspace_alias_for_cli(raw: &Path) -> Option<PathBuf> {
    if looks_like_path(raw) {
        return None;
    }
    let alias = raw.to_str()?;
    let normalized = normalize_alias(alias).ok()?;
    let registry_path = registry_database_path_override(None);
    let row = find_alias_read_only(&registry_path, &normalized).ok()??;
    Some(PathBuf::from(row.path))
}

pub fn list_workspace_registry(
    options: &WorkspaceListOptions,
) -> Result<WorkspaceListReport, DomainError> {
    let registry_path = registry_database_path_override(options.registry_path.as_deref());
    if !registry_path.exists() {
        return Ok(WorkspaceListReport {
            schema: WORKSPACE_REGISTRY_SCHEMA_V1,
            command: "workspace list",
            registry_path: registry_path.display().to_string(),
            registry_exists: false,
            workspaces: Vec::new(),
        });
    }

    let conn = open_registry_read_only(&registry_path)?;
    let workspaces = conn
        .list_workspaces()
        .map_err(|error| storage_error("failed to list workspace registry", error))?
        .into_iter()
        .map(WorkspaceEntry::from)
        .collect();

    Ok(WorkspaceListReport {
        schema: WORKSPACE_REGISTRY_SCHEMA_V1,
        command: "workspace list",
        registry_path: registry_path.display().to_string(),
        registry_exists: true,
        workspaces,
    })
}

pub fn resolve_workspace_report(
    options: &WorkspaceResolveOptions,
) -> Result<WorkspaceResolveReport, DomainError> {
    let registry_path = registry_database_path_override(options.registry_path.as_deref());
    if let Some(target) = options.target.as_deref() {
        if !looks_like_path(Path::new(target)) {
            let alias = normalize_alias(target).map_err(alias_usage_error)?;
            let row = find_alias_read_only(&registry_path, &alias)?.ok_or_else(|| {
                DomainError::NotFound {
                    resource: "workspace alias".to_string(),
                    id: alias.clone(),
                    repair: Some("ee workspace list --json".to_string()),
                }
            })?;
            return Ok(resolve_alias_row_report(&registry_path, target, row));
        }

        return resolve_path_report(&registry_path, Some(target), Some(PathBuf::from(target)));
    }

    resolve_path_report(&registry_path, None, options.workspace_path.clone())
}

pub fn alias_workspace(
    options: &WorkspaceAliasOptions,
) -> Result<WorkspaceAliasReport, DomainError> {
    if options.clear && options.alias.is_some() {
        return Err(DomainError::Usage {
            message: "--clear cannot be combined with --as or a positional alias".to_string(),
            repair: Some(
                "Use either `ee workspace alias --clear` or `ee workspace alias --as <name>`."
                    .to_string(),
            ),
        });
    }

    let normalized_alias = options
        .alias
        .as_deref()
        .map(normalize_alias)
        .transpose()
        .map_err(alias_usage_error)?;

    if !options.clear && normalized_alias.is_none() {
        return Err(DomainError::Usage {
            message: "workspace alias requires --as <name> or --clear".to_string(),
            repair: Some("ee workspace alias --pick <path-or-id> --as <name>".to_string()),
        });
    }

    let registry_path = registry_database_path_override(options.registry_path.as_deref());
    let target = resolve_alias_target(&registry_path, options)?;
    let previous_alias = target.name.clone();

    if let Some(alias) = normalized_alias.as_deref() {
        ensure_alias_available(&registry_path, alias, &target.id)?;
    }

    if options.dry_run {
        return Ok(WorkspaceAliasReport {
            schema: WORKSPACE_ALIAS_SCHEMA_V1,
            command: "workspace alias",
            status: if options.clear {
                "would_clear"
            } else {
                "would_set"
            },
            registry_path: registry_path.display().to_string(),
            workspace_id: target.id,
            workspace_path: target.path,
            alias: normalized_alias,
            previous_alias,
            scope_kind: target.scope_kind,
            repository_root: target.repository_root,
            repository_fingerprint: target.repository_fingerprint,
            subproject_path: target.subproject_path,
            dry_run: true,
            persisted: false,
            audit_id: None,
        });
    }

    let conn = open_registry_write(&registry_path)?;
    let action = if options.clear {
        WORKSPACE_ALIAS_CLEAR_ACTION
    } else {
        WORKSPACE_ALIAS_SET_ACTION
    };
    let audit_id = generate_audit_id();
    let target_id = target.id.clone();
    let target_path = target.path.clone();
    let target_scope_kind = target.scope_kind.clone();
    let target_repository_root = target.repository_root.clone();
    let target_repository_fingerprint = target.repository_fingerprint.clone();
    let target_subproject_path = target.subproject_path.clone();
    let alias_for_write = normalized_alias.clone();
    let details = serde_json::json!({
        "schema": WORKSPACE_ALIAS_SCHEMA_V1,
        "workspaceId": target_id,
        "workspacePath": target_path,
        "previousAlias": previous_alias,
        "alias": alias_for_write,
        "scopeKind": target_scope_kind,
        "repositoryRoot": target_repository_root,
        "repositoryFingerprint": target_repository_fingerprint,
        "subprojectPath": target_subproject_path,
        "dryRun": false
    })
    .to_string();

    conn.with_transaction(|| {
        upsert_workspace_row(&conn, &target)?;
        conn.update_workspace_name(&target.id, alias_for_write.as_deref())?;
        conn.insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(target.id.clone()),
                actor: Some("ee-cli".to_string()),
                action: action.to_string(),
                target_type: Some("workspace".to_string()),
                target_id: Some(target.id.clone()),
                details: Some(details),
            },
        )?;
        Ok(())
    })
    .map_err(|error| storage_error("failed to persist workspace alias", error))?;

    Ok(WorkspaceAliasReport {
        schema: WORKSPACE_ALIAS_SCHEMA_V1,
        command: "workspace alias",
        status: if options.clear { "cleared" } else { "set" },
        registry_path: registry_path.display().to_string(),
        workspace_id: target.id,
        workspace_path: target.path,
        alias: normalized_alias,
        previous_alias,
        scope_kind: target.scope_kind,
        repository_root: target.repository_root,
        repository_fingerprint: target.repository_fingerprint,
        subproject_path: target.subproject_path,
        dry_run: false,
        persisted: true,
        audit_id: Some(audit_id),
    })
}

fn resolve_path_report(
    registry_path: &Path,
    target: Option<&str>,
    workspace_path: Option<PathBuf>,
) -> Result<WorkspaceResolveReport, DomainError> {
    let request = WorkspaceResolutionRequest::from_process(
        workspace_path,
        WorkspaceResolutionMode::AllowUninitialized,
    )
    .map_err(|error| DomainError::Configuration {
        message: error.to_string(),
        repair: Some("Run from a readable directory or pass --workspace <path>.".to_string()),
    })?;
    let resolution = resolve_workspace(&request).map_err(|error| DomainError::Configuration {
        message: error.to_string(),
        repair: Some("ee init --workspace .".to_string()),
    })?;
    let diagnostics = diagnose_workspace_resolution(&request, &resolution)
        .into_iter()
        .map(WorkspaceDiagnosticEntry::from)
        .collect();
    let alias = find_workspace_alias_read_only(registry_path, &resolution.location.root)?;
    let workspace_id = stable_workspace_id(&resolution.canonical_root);

    Ok(WorkspaceResolveReport {
        schema: WORKSPACE_RESOLVE_SCHEMA_V1,
        command: "workspace resolve",
        source: resolution.source.as_str().to_string(),
        target: target.map(str::to_string),
        workspace_id,
        root: resolution.location.root.display().to_string(),
        canonical_root: resolution.canonical_root.display().to_string(),
        marker_present: resolution.marker_present,
        alias,
        scope_kind: resolution.scope.kind.as_str().to_string(),
        repository_root: resolution
            .scope
            .repository_root
            .as_ref()
            .map(|path| path.display().to_string()),
        repository_fingerprint: resolution.scope.repository_fingerprint.clone(),
        subproject_path: resolution
            .scope
            .subproject_path
            .as_ref()
            .map(|path| path.display().to_string()),
        registry_path: registry_path.display().to_string(),
        diagnostics,
    })
}

fn resolve_alias_row_report(
    registry_path: &Path,
    target: &str,
    row: StoredWorkspace,
) -> WorkspaceResolveReport {
    let root = PathBuf::from(&row.path);
    let canonical_root = canonical_or_lexical(&root);
    WorkspaceResolveReport {
        schema: WORKSPACE_RESOLVE_SCHEMA_V1,
        command: "workspace resolve",
        source: "alias".to_string(),
        target: Some(target.to_string()),
        workspace_id: row.id,
        root: root.display().to_string(),
        canonical_root: canonical_root.display().to_string(),
        marker_present: root.join(WORKSPACE_MARKER).is_dir(),
        alias: row.name,
        scope_kind: row.scope_kind,
        repository_root: row.repository_root,
        repository_fingerprint: row.repository_fingerprint,
        subproject_path: row.subproject_path,
        registry_path: registry_path.display().to_string(),
        diagnostics: Vec::new(),
    }
}

fn resolve_alias_target(
    registry_path: &Path,
    options: &WorkspaceAliasOptions,
) -> Result<StoredWorkspace, DomainError> {
    if let Some(pick) = options.pick.as_deref() {
        if pick.starts_with("wsp_") {
            if let Some(row) = find_workspace_id_read_only(registry_path, pick)? {
                return Ok(row);
            }
            return Err(DomainError::NotFound {
                resource: "workspace".to_string(),
                id: pick.to_string(),
                repair: Some("ee workspace list --json".to_string()),
            });
        }
        return workspace_row_for_path(pick);
    }

    let selected = options
        .workspace_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    workspace_row_for_path(&selected.display().to_string())
}

fn workspace_row_for_path(raw: &str) -> Result<StoredWorkspace, DomainError> {
    let root = lexical_absolute(
        &env::current_dir().map_err(|error| DomainError::Configuration {
            message: format!("failed to read current directory: {error}"),
            repair: Some("Run from a readable directory or pass --workspace <path>.".to_string()),
        })?,
        Path::new(raw),
    );
    if !root.exists() {
        return Err(DomainError::Configuration {
            message: format!("workspace path does not exist: {}", root.display()),
            repair: Some("Create the directory or pass an existing --workspace path.".to_string()),
        });
    }
    let marker = root.join(WORKSPACE_MARKER);
    if !marker.is_dir() {
        return Err(DomainError::Configuration {
            message: format!("workspace is not initialized: {}", root.display()),
            repair: Some(format!("ee init --workspace {}", root.display())),
        });
    }
    let canonical = canonical_or_lexical(&root);
    let path = canonical.display().to_string();
    let scope = workspace_scope_fields(&derive_workspace_scope(&canonical));
    Ok(StoredWorkspace {
        id: stable_workspace_id(&canonical),
        path,
        name: None,
        scope_kind: scope.scope_kind,
        repository_root: scope.repository_root,
        repository_fingerprint: scope.repository_fingerprint,
        subproject_path: scope.subproject_path,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn workspace_scope_fields(scope: &WorkspaceScope) -> WorkspaceScopeFields {
    WorkspaceScopeFields {
        scope_kind: scope.kind.as_str().to_string(),
        repository_root: scope
            .repository_root
            .as_ref()
            .map(|path| path.display().to_string()),
        repository_fingerprint: scope.repository_fingerprint.clone(),
        subproject_path: scope
            .subproject_path
            .as_ref()
            .map(|path| path.display().to_string()),
    }
}

fn upsert_workspace_row(conn: &DbConnection, target: &StoredWorkspace) -> crate::db::Result<()> {
    if conn.get_workspace(&target.id)?.is_some() {
        return Ok(());
    }
    if conn.get_workspace_by_path(&target.path)?.is_some() {
        return Ok(());
    }
    let scope = WorkspaceScopeFields {
        scope_kind: target.scope_kind.clone(),
        repository_root: target.repository_root.clone(),
        repository_fingerprint: target.repository_fingerprint.clone(),
        subproject_path: target.subproject_path.clone(),
    };
    conn.insert_workspace_with_scope(
        &target.id,
        &CreateWorkspaceInput {
            path: target.path.clone(),
            name: target.name.clone(),
        },
        &scope,
    )
}

fn ensure_alias_available(
    registry_path: &Path,
    alias: &str,
    workspace_id: &str,
) -> Result<(), DomainError> {
    if let Some(existing) = find_alias_read_only(registry_path, alias)? {
        if existing.id != workspace_id {
            return Err(DomainError::Usage {
                message: format!(
                    "workspace alias `{alias}` already points to {}",
                    existing.path
                ),
                repair: Some(
                    "Choose a different alias or clear the existing workspace alias first."
                        .to_string(),
                ),
            });
        }
    }
    Ok(())
}

fn find_workspace_alias_read_only(
    registry_path: &Path,
    workspace_path: &Path,
) -> Result<Option<String>, DomainError> {
    if !registry_path.exists() {
        return Ok(None);
    }
    let canonical = canonical_or_lexical(workspace_path);
    let path = canonical.display().to_string();
    let conn = open_registry_read_only(registry_path)?;
    Ok(conn
        .get_workspace_by_path(&path)
        .map_err(|error| storage_error("failed to resolve workspace alias", error))?
        .and_then(|row| row.name))
}

fn find_alias_read_only(
    registry_path: &Path,
    alias: &str,
) -> Result<Option<StoredWorkspace>, DomainError> {
    if !registry_path.exists() {
        return Ok(None);
    }
    let conn = open_registry_read_only(registry_path)?;
    let rows = conn
        .list_workspaces()
        .map_err(|error| storage_error("failed to query workspace aliases", error))?;
    Ok(rows
        .into_iter()
        .find(|row| row.name.as_deref() == Some(alias)))
}

fn find_workspace_id_read_only(
    registry_path: &Path,
    workspace_id: &str,
) -> Result<Option<StoredWorkspace>, DomainError> {
    if !registry_path.exists() {
        return Ok(None);
    }
    let conn = open_registry_read_only(registry_path)?;
    conn.get_workspace(workspace_id)
        .map_err(|error| storage_error("failed to query workspace registry", error))
}

fn open_registry_read_only(registry_path: &Path) -> Result<DbConnection, DomainError> {
    let conn = DbConnection::open(DatabaseConfig::file(registry_path))
        .map_err(|error| storage_error("failed to open workspace registry", error))?;
    if conn
        .needs_migration()
        .map_err(|error| storage_error("failed to inspect workspace registry schema", error))?
    {
        return Err(DomainError::MigrationRequired {
            message: format!(
                "workspace registry requires migration: {}",
                registry_path.display()
            ),
            repair: Some("Run a mutating workspace registry command such as `ee workspace alias --as <name>`.".to_string()),
        });
    }
    Ok(conn)
}

fn open_registry_write(registry_path: &Path) -> Result<DbConnection, DomainError> {
    if let Some(parent) = registry_path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create workspace registry directory {}: {error}",
                parent.display()
            ),
            repair: Some(
                "Check permissions or set EE_WORKSPACE_REGISTRY to a writable path.".to_string(),
            ),
        })?;
    }
    let conn = DbConnection::open(DatabaseConfig::file(registry_path))
        .map_err(|error| storage_error("failed to open workspace registry", error))?;
    conn.migrate()
        .map_err(|error| storage_error("failed to migrate workspace registry", error))?;
    Ok(conn)
}

fn normalize_alias(raw: &str) -> Result<String, String> {
    let alias = raw.trim();
    if alias.is_empty() {
        return Err("workspace alias cannot be empty".to_string());
    }
    if alias == "." || alias == ".." {
        return Err("workspace alias cannot be `.` or `..`".to_string());
    }
    if alias.len() > 64 {
        return Err("workspace alias cannot exceed 64 bytes".to_string());
    }
    if alias
        .chars()
        .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.'))
    {
        return Err(
            "workspace alias may only contain ASCII letters, numbers, dots, dashes, and underscores"
                .to_string(),
        );
    }
    Ok(alias.to_string())
}

fn alias_usage_error(message: String) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some("Use an alias like `project-main`, `client.api`, or `repo_1`.".to_string()),
    }
}

fn storage_error(context: &str, error: crate::db::DbError) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some(
            "Run `ee doctor --json` and verify the workspace registry database.".to_string(),
        ),
    }
}

fn looks_like_path(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    let rendered = path.to_string_lossy();
    rendered.starts_with('.')
        || rendered.starts_with('~')
        || rendered.contains('/')
        || rendered.contains('\\')
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| lexical_absolute(Path::new("."), path))
}

fn lexical_absolute(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize_lexical(&joined)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !path.is_absolute() {
                    out.push("..");
                }
            }
            Component::Normal(segment) => out.push(segment),
        }
    }
    out
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes().iter().copied()) {
        *target = source;
    }
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    type TestResult = Result<(), String>;

    fn unique_dir(prefix: &str) -> Result<PathBuf, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        Ok(env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id())))
    }

    fn initialized_workspace(prefix: &str) -> Result<PathBuf, String> {
        let root = unique_dir(prefix)?;
        fs::create_dir_all(root.join(WORKSPACE_MARKER)).map_err(|error| error.to_string())?;
        Ok(root)
    }

    #[test]
    fn alias_validation_rejects_paths() {
        assert!(normalize_alias("client-api").is_ok());
        assert!(normalize_alias("client/api").is_err());
        assert!(normalize_alias(".").is_err());
    }

    #[test]
    fn alias_command_registers_and_resolves_workspace() -> TestResult {
        let workspace = initialized_workspace("ee-workspace-alias")?;
        let registry = unique_dir("ee-workspace-registry")?.join("registry.db");
        let report = alias_workspace(&WorkspaceAliasOptions {
            workspace_path: Some(workspace.clone()),
            pick: None,
            alias: Some("client-api".to_string()),
            clear: false,
            dry_run: false,
            registry_path: Some(registry.clone()),
        })
        .map_err(|error| error.message())?;

        assert!(report.persisted);
        assert_eq!(report.alias.as_deref(), Some("client-api"));

        let resolved = resolve_workspace_report(&WorkspaceResolveOptions {
            workspace_path: None,
            target: Some("client-api".to_string()),
            registry_path: Some(registry),
        })
        .map_err(|error| error.message())?;

        assert_eq!(resolved.source, "alias");
        assert_eq!(
            PathBuf::from(resolved.root),
            workspace.canonicalize().unwrap_or(workspace)
        );
        Ok(())
    }

    #[test]
    fn alias_dry_run_does_not_create_registry() -> TestResult {
        let workspace = initialized_workspace("ee-workspace-alias-dry")?;
        let registry = unique_dir("ee-workspace-registry-dry")?.join("registry.db");
        let report = alias_workspace(&WorkspaceAliasOptions {
            workspace_path: Some(workspace),
            pick: None,
            alias: Some("dry-run".to_string()),
            clear: false,
            dry_run: true,
            registry_path: Some(registry.clone()),
        })
        .map_err(|error| error.message())?;

        assert!(!report.persisted);
        assert!(!registry.exists());
        Ok(())
    }
}
