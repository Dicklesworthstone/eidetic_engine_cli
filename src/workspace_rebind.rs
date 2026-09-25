//! Explicit recovery of a moved, single-workspace project-local store.
//!
//! Rebinding changes the workspace's location, never its durable identity.
//! Preview is read-only; applying requires the exact preview commitment and
//! rechecks its preconditions under the database's transaction/writer fence.
//! This deliberately does not merge stores, rewrite provenance, migrate a
//! schema, modify the global alias registry, or declare copied indexes ready.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::config::{WORKSPACE_MARKER, derive_workspace_scope};
use crate::core::workspace::WorkspaceEntry;
use crate::db::{CreateAuditInput, DbConnection, DbError, DbOperation, generate_audit_id};
use crate::models::DomainError;
use crate::policy::store_auth::{
    Mac, MacDomain, StoreAuthError, StoreAuthRoot, workspace_keys_dir,
};

pub const WORKSPACE_REBIND_SCHEMA: &str = "ee.workspace.rebind.v1";
const REBIND_ACTION: &str = "workspace.rebound";
const CONFLICT_PREFIX: &str = "workspace rebind: ";
const COMMITMENT_PREFIX: &str = "ee-rebind-v1";

/// Both source selectors are required: recovery must not guess which identity
/// a copied database belongs to. With no `apply_plan`, this only previews.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRebindOptions {
    pub workspace_path: PathBuf,
    /// Existing source authentication material selected by the caller, never
    /// by database metadata. It must match the copied store's current root.
    pub source_keys_dir: PathBuf,
    pub expected_workspace_id: String,
    /// The exact stored path, even when it no longer exists on this host.
    pub expected_source_path: String,
    /// The `planHash` returned by a preview of this exact store and binding.
    pub apply_plan: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRebindDestination {
    pub path: String,
    pub scope_kind: String,
    pub repository_root: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub subproject_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRebindPlan {
    pub schema: &'static str,
    pub database_path: String,
    pub source_keys_dir: String,
    pub source_auth_key_id: String,
    pub previous: WorkspaceEntry,
    pub destination: WorkspaceRebindDestination,
    /// Run only after returning the local store to its original directory.
    /// Recovery never moves files or overwrites the original store itself.
    pub rollback_preview_command: String,
    pub content_hash: String,
    /// Opaque MAC-authenticated commitment accepted by `apply_plan`.
    pub plan_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRebindReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub status: &'static str,
    pub dry_run: bool,
    pub persisted: bool,
    pub plan: WorkspaceRebindPlan,
    pub audit_id: Option<String>,
    /// Historical memory/evidence/rule/pack identities are not rekeyed.
    pub workspace_id_preserved: bool,
    pub derived_assets_modified: bool,
    /// A rebind is not proof that copied derived assets are usable here.
    pub index_rebuild_command: String,
}

/// Preview or explicitly apply an identity-preserving workspace relocation.
///
/// Only an existing `<workspace>/.ee/ee.db` is addressed. A typo, an old schema,
/// a symlink, or an ambiguous multi-workspace store is refused; no database,
/// directory, or registry is created by the preview. Keep other processes
/// from moving/replacing the store files while recovering it.
///
/// # Errors
/// Returns an error for invalid paths, stale/foreign commitments, ambiguous
/// bindings, migration requirements, or a failed atomic database operation.
pub fn rebind_workspace(
    options: &WorkspaceRebindOptions,
) -> Result<WorkspaceRebindReport, DomainError> {
    let (database, destination) = local_destination(&options.workspace_path)?;
    let database_text = utf8_path(&database)?;
    let source_keys_dir = existing_directory(&options.source_keys_dir)?;
    let copied_keys_dir = workspace_keys_dir(Path::new(&destination.path));
    let source_auth = StoreAuthRoot::open(&source_keys_dir).map_err(authentication_error)?;
    let copied_auth = StoreAuthRoot::open(&copied_keys_dir).map_err(authentication_error)?;
    let options = WorkspaceRebindOptions {
        source_keys_dir,
        ..options.clone()
    };
    let read = DbConnection::open_file_read_only(&database).map_err(storage_error)?;
    if read.needs_migration().map_err(storage_error)? {
        return Err(DomainError::MigrationRequired {
            message: "Workspace rebind requires an already-migrated local store.".to_owned(),
            repair: Some("Migrate the store explicitly before previewing the rebind.".to_owned()),
        });
    }
    let plan = plan_for_connection(&read, &database_text, &destination, &options, &source_auth)
        .map_err(storage_error)?;
    verify_plan_commitment(&plan, &plan.plan_hash, &copied_auth).map_err(storage_error)?;
    drop(read);
    let Some(commitment) = options.apply_plan.as_deref() else {
        return Ok(report(plan, None));
    };
    verify_plan_commitment(&plan, commitment, &source_auth).map_err(storage_error)?;
    drop(source_auth);
    drop(copied_auth);

    // Recheck the destination before opening a writer. The second plan check
    // below is the authoritative one: previewing must not create a TOCTOU
    // window for another database writer to change the selected identity.
    let (checked_database, checked_destination) = local_destination(&options.workspace_path)?;
    if checked_database != database || checked_destination != destination {
        return Err(storage_error(conflict("destination changed after preview")));
    }
    if existing_directory(&options.source_keys_dir)? != options.source_keys_dir {
        return Err(storage_error(conflict(
            "source key directory changed after preview",
        )));
    }
    // Hold both roots across the writer transaction so a normal key rotation
    // cannot invalidate the authenticated selection between check and commit.
    // Two shared locks also work when the caller explicitly selected the
    // copied store's own keys; neither path creates or rotates key material.
    let source_auth =
        StoreAuthRoot::open_read_locked(&options.source_keys_dir).map_err(authentication_error)?;
    let copied_auth =
        StoreAuthRoot::open_read_locked(&copied_keys_dir).map_err(authentication_error)?;
    let db = DbConnection::open_file(&database).map_err(storage_error)?;
    let audit_id = generate_audit_id();
    let applied = db
        .with_transaction(|| {
            apply_on_connection(
                &db,
                &database_text,
                &destination,
                &options,
                &source_auth,
                &copied_auth,
                commitment,
                &audit_id,
            )
        })
        .map_err(storage_error)?;
    Ok(report(applied, Some(audit_id)))
}

fn local_destination(
    workspace: &Path,
) -> Result<(PathBuf, WorkspaceRebindDestination), DomainError> {
    let canonical = existing_directory(workspace)?;
    let marker = canonical.join(WORKSPACE_MARKER);
    require_file_kind(&marker, true)?;
    let database = marker.join("ee.db");
    require_file_kind(&database, false)?;
    let scope = derive_workspace_scope(&canonical);
    Ok((
        database,
        WorkspaceRebindDestination {
            path: utf8_path(&canonical)?,
            scope_kind: scope.kind.as_str().to_owned(),
            repository_root: scope
                .repository_root
                .as_deref()
                .map(utf8_path)
                .transpose()?,
            repository_fingerprint: scope.repository_fingerprint,
            subproject_path: scope
                .subproject_path
                .as_deref()
                .map(utf8_path)
                .transpose()?,
        },
    ))
}

fn existing_directory(path: &Path) -> Result<PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| usage("cannot resolve the current directory"))?
            .join(path)
    };
    let mut component_path = PathBuf::new();
    for component in absolute.components() {
        if component == Component::ParentDir {
            return Err(usage("use paths without parent-directory components"));
        }
        component_path.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        require_file_kind(&component_path, true)?;
    }
    absolute
        .canonicalize()
        .map_err(|_| usage("an existing readable directory is required"))
}

fn require_file_kind(path: &Path, directory: bool) -> Result<(), DomainError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| usage("an existing, readable project-local store is required"))?;
    if metadata.file_type().is_symlink()
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        }
    {
        return Err(usage(
            "symlinks and non-regular local store paths are not accepted",
        ));
    }
    Ok(())
}

fn utf8_path(path: &Path) -> Result<String, DomainError> {
    path.to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| usage("destination paths must be valid UTF-8 without control characters"))
}

fn plan_for_connection(
    db: &DbConnection,
    database: &str,
    destination: &WorkspaceRebindDestination,
    options: &WorkspaceRebindOptions,
    source_auth: &StoreAuthRoot,
) -> Result<WorkspaceRebindPlan, DbError> {
    let previous = single_workspace(db)?;
    if previous.workspace_id != options.expected_workspace_id
        || previous.path != options.expected_source_path
    {
        return Err(conflict(
            "stored workspace ID or source path does not match the explicit selection",
        ));
    }
    if previous.path == destination.path {
        return Err(conflict("store is already bound to the destination"));
    }
    if !Path::new(&previous.path).is_absolute() || previous.path.chars().any(char::is_control) {
        return Err(conflict(
            "the stored source path must be absolute without control characters",
        ));
    }
    let source_keys_dir = options
        .source_keys_dir
        .to_str()
        .filter(|path| !path.chars().any(char::is_control))
        .ok_or_else(|| conflict("source key paths must be valid UTF-8 without control characters"))?
        .to_owned();
    let rollback_preview_command = format!(
        "ee workspace rebind --workspace {} --source-keys-dir {} --expected-workspace-id {} --expected-source-path {}",
        shell_quote(&previous.path),
        shell_quote(
            workspace_keys_dir(Path::new(&previous.path))
                .to_str()
                .ok_or_else(|| conflict("rollback key path is not valid UTF-8"))?,
        ),
        shell_quote(&previous.workspace_id),
        shell_quote(&destination.path),
    );
    let mut plan = WorkspaceRebindPlan {
        schema: WORKSPACE_REBIND_SCHEMA,
        database_path: database.to_owned(),
        source_keys_dir,
        source_auth_key_id: source_auth.current_key_id().to_hex(),
        previous,
        destination: destination.clone(),
        rollback_preview_command,
        content_hash: String::new(),
        plan_hash: String::new(),
    };
    // The fixed struct field order and empty commitment field make the digest
    // reproducible. Include all source binding metadata (including updatedAt)
    // so alias/scope edits invalidate a previously approved plan as well.
    let bytes =
        serde_json::to_vec(&plan).map_err(|_| conflict("cannot encode the preview commitment"))?;
    plan.content_hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
    let mac = source_auth
        .mac(MacDomain::WorkspaceRebindPlan, &commitment_message(&plan)?)
        .map_err(authentication_conflict)?;
    plan.plan_hash = format!(
        "{COMMITMENT_PREFIX}:{}:{}",
        plan.source_auth_key_id,
        mac.to_hex()
    );
    Ok(plan)
}

fn commitment_message(plan: &WorkspaceRebindPlan) -> Result<Vec<u8>, DbError> {
    let mut unsigned = plan.clone();
    unsigned.plan_hash.clear();
    serde_json::to_vec(&unsigned).map_err(|_| conflict("cannot encode the preview commitment"))
}

fn verify_plan_commitment(
    plan: &WorkspaceRebindPlan,
    commitment: &str,
    auth: &StoreAuthRoot,
) -> Result<(), DbError> {
    let refused =
        || conflict("preview commitment is stale or source authentication does not match");
    let Some((prefix, rest)) = commitment.split_once(':') else {
        return Err(refused());
    };
    let Some((key_id, encoded_mac)) = rest.split_once(':') else {
        return Err(refused());
    };
    if prefix != COMMITMENT_PREFIX
        || key_id != plan.source_auth_key_id
        || key_id != auth.current_key_id().to_hex()
    {
        return Err(refused());
    }
    let mac = Mac::from_hex(encoded_mac).map_err(|_| refused())?;
    if encoded_mac != mac.to_hex()
        || !auth
            .verify(
                MacDomain::WorkspaceRebindPlan,
                &commitment_message(plan)?,
                &mac,
            )
            .map_err(authentication_conflict)?
    {
        return Err(refused());
    }
    Ok(())
}

fn single_workspace(db: &DbConnection) -> Result<WorkspaceEntry, DbError> {
    let mut workspaces = db.list_workspaces()?;
    if workspaces.len() != 1 {
        return Err(conflict(
            "recovery requires exactly one stored workspace; no identity was guessed",
        ));
    }
    workspaces
        .pop()
        .map(WorkspaceEntry::from)
        .ok_or_else(|| conflict("stored workspace disappeared"))
}

// The caller MUST hold with_transaction for this complete read/check/write/audit
// sequence. Keeping this helper private prevents a partial public apply path.
fn apply_on_connection(
    db: &DbConnection,
    database: &str,
    destination: &WorkspaceRebindDestination,
    options: &WorkspaceRebindOptions,
    source_auth: &StoreAuthRoot,
    copied_auth: &StoreAuthRoot,
    commitment: &str,
    audit_id: &str,
) -> Result<WorkspaceRebindPlan, DbError> {
    let plan = plan_for_connection(db, database, destination, options, source_auth)?;
    verify_plan_commitment(&plan, commitment, source_auth)?;
    verify_plan_commitment(&plan, commitment, copied_auth)?;
    // This MUST remain an UPDATE, never an INSERT/REPLACE or identity upsert.
    // The primary key, alias, creation timestamp and all child rows stay put.
    // DbConnection's raw writer has no parameter-binding public counterpart;
    // encode every value as a checked SQL literal, with no dynamic identifiers.
    db.execute_raw(&format!(
        "UPDATE workspaces SET path = {}, scope_kind = {}, repository_root = {}, \
         repository_fingerprint = {}, subproject_path = {}, updated_at = {} \
         WHERE id = {} AND path = {}",
        sql_text(&destination.path)?,
        sql_text(&destination.scope_kind)?,
        sql_optional_text(destination.repository_root.as_deref())?,
        sql_optional_text(destination.repository_fingerprint.as_deref())?,
        sql_optional_text(destination.subproject_path.as_deref())?,
        sql_text(&chrono::Utc::now().to_rfc3339())?,
        sql_text(&plan.previous.workspace_id)?,
        sql_text(&plan.previous.path)?,
    ))?;
    // Verify the location update before the transaction can commit, including
    // the original creation time, alias and exactly-one-workspace condition.
    let actual = single_workspace(db)?;
    let mut expected = plan.previous.clone();
    expected.path = destination.path.clone();
    expected.scope_kind = destination.scope_kind.clone();
    expected.repository_root = destination.repository_root.clone();
    expected.repository_fingerprint = destination.repository_fingerprint.clone();
    expected.subproject_path = destination.subproject_path.clone();
    expected.updated_at = actual.updated_at.clone();
    if actual != expected {
        return Err(conflict(
            "workspace update did not preserve the approved identity",
        ));
    }
    db.insert_audit(
        audit_id,
        &CreateAuditInput {
            workspace_id: Some(plan.previous.workspace_id.clone()),
            actor: Some("ee workspace rebind".to_owned()),
            action: REBIND_ACTION.to_owned(),
            target_type: Some("workspace".to_owned()),
            target_id: Some(plan.previous.workspace_id.clone()),
            details: Some(
                serde_json::to_string(&plan)
                    .map_err(|_| conflict("cannot encode the rebind audit"))?,
            ),
        },
    )?;
    Ok(plan)
}

fn sql_text(value: &str) -> Result<String, DbError> {
    if value.contains('\0') {
        return Err(conflict("a workspace binding contains a NUL byte"));
    }
    Ok(format!("'{}'", value.replace('\'', "''")))
}

fn sql_optional_text(value: Option<&str>) -> Result<String, DbError> {
    value.map_or_else(|| Ok("NULL".to_owned()), sql_text)
}

fn report(plan: WorkspaceRebindPlan, audit_id: Option<String>) -> WorkspaceRebindReport {
    let persisted = audit_id.is_some();
    let index_rebuild_command = format!(
        "ee index rebuild --workspace {} --database {}",
        shell_quote(&plan.destination.path),
        shell_quote(&plan.database_path),
    );
    WorkspaceRebindReport {
        schema: WORKSPACE_REBIND_SCHEMA,
        command: "workspace rebind",
        status: if persisted { "rebound" } else { "preview" },
        dry_run: !persisted,
        persisted,
        plan,
        audit_id,
        workspace_id_preserved: true,
        derived_assets_modified: false,
        index_rebuild_command,
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn conflict(message: &str) -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Execute,
        message: format!("{CONFLICT_PREFIX}{message}"),
    }
}

fn authentication_conflict(error: StoreAuthError) -> DbError {
    conflict(&error.message())
}

fn authentication_error(error: StoreAuthError) -> DomainError {
    DomainError::Usage {
        message: format!("{CONFLICT_PREFIX}{}", error.message()),
        repair: Some(
            "Select the existing source key directory and preserve the matching protected keys with the copied store; rebind never creates authentication keys."
                .to_owned(),
        ),
    }
}

fn usage(message: &str) -> DomainError {
    DomainError::Usage {
        message: format!("{CONFLICT_PREFIX}{message}"),
        repair: Some(
            "Inspect the copied local store and preview using its exact stored ID and path."
                .to_owned(),
        ),
    }
}

fn storage_error(error: DbError) -> DomainError {
    match error {
        DbError::MalformedRow { message, .. } if message.starts_with(CONFLICT_PREFIX) => {
            DomainError::Usage {
                message,
                repair: Some(
                    "Inspect the store again and request a fresh rebind preview.".to_owned(),
                ),
            }
        }
        other => DomainError::Storage {
            message: format!("workspace rebind failed: {other}"),
            repair: Some(
                "Inspect the local store; a failed transaction does not apply a partial rebind."
                    .to_owned(),
            ),
        },
    }
}

#[cfg(test)]
#[path = "workspace_rebind_tests.rs"]
mod tests;
