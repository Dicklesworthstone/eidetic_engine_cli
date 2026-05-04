//! `ee model status` / `ee model list` reporting (EE-294).
//!
//! Surfaces the state of the workspace's local embedding/model registry in a
//! stable, machine-readable shape. `ee` does not pick embedding models —
//! Frankensearch owns that decision. These commands expose what the registry
//! knows so agents can introspect availability and degraded-mode posture
//! without scraping `ee index status`.

use std::path::{Path, PathBuf};

use crate::db::{DbConnection, DbError, StoredModelRegistryEntry};
use crate::models::DomainError;
use crate::search::HashEmbedder;
use frankensearch::Embedder;

/// Convert a DbError to DomainError, preserving MigrationDrift as a distinct error code.
///
/// Bug: eidetic_engine_cli-wfgr
fn db_error_to_domain(error: DbError, context: &str, repair: Option<String>) -> DomainError {
    match error {
        DbError::MigrationDrift {
            version,
            expected_name,
            actual_name,
            expected_checksum,
            actual_checksum,
        } => DomainError::MigrationDrift {
            message: format!(
                "{context}: migration {version} drifted; expected {} ({}), found {actual_name} ({actual_checksum})",
                expected_name.as_deref().unwrap_or("<missing>"),
                expected_checksum.as_deref().unwrap_or("<missing>"),
            ),
            repair: Some("Reinstall ee or restore database from backup".to_string()),
        },
        other => DomainError::Storage {
            message: format!("{context}: {other}"),
            repair,
        },
    }
}

/// Schema identifier for `ee model status` JSON output.
pub const MODEL_STATUS_SCHEMA_V1: &str = "ee.model.status.v1";
/// Schema identifier for `ee model list` JSON output.
pub const MODEL_LIST_SCHEMA_V1: &str = "ee.model.list.v1";

const DEFAULT_DB_FILE: &str = "ee.db";

/// Options for `ee model status`.
#[derive(Clone, Debug)]
pub struct ModelStatusOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
}

/// Options for `ee model list`.
#[derive(Clone, Debug)]
pub struct ModelListOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
}

/// Single registry entry shaped for public output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRegistryEntryView {
    pub id: String,
    pub provider: String,
    pub model_name: String,
    pub purpose: String,
    pub status: String,
    pub dimension: Option<u32>,
    pub distance_metric: Option<String>,
    pub version: Option<String>,
    pub source_uri: Option<String>,
    pub content_hash: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_checked_at: Option<String>,
}

impl ModelRegistryEntryView {
    fn from_stored(entry: StoredModelRegistryEntry) -> Self {
        Self {
            id: entry.id,
            provider: entry.provider.as_str().to_string(),
            model_name: entry.model_name,
            purpose: entry.purpose.as_str().to_string(),
            status: entry.status.as_str().to_string(),
            dimension: entry.dimension,
            distance_metric: entry
                .distance_metric
                .map(|metric| metric.as_str().to_string()),
            version: entry.version,
            source_uri: entry.source_uri,
            content_hash: entry.content_hash,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            last_checked_at: entry.last_checked_at,
        }
    }

    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "provider": self.provider,
            "modelName": self.model_name,
            "purpose": self.purpose,
            "status": self.status,
            "dimension": self.dimension,
            "distanceMetric": self.distance_metric,
            "version": self.version,
            "sourceUri": self.source_uri,
            "contentHash": self.content_hash,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
            "lastCheckedAt": self.last_checked_at,
        })
    }
}

/// Resolved active embedder shaped for public output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelStatusActive {
    pub fast_model_id: String,
    pub fast_dimension: usize,
    pub quality_model_id: Option<String>,
    pub quality_dimension: Option<usize>,
    pub semantic: bool,
    pub deterministic: bool,
    pub source: String,
    pub selected_registry_entry: Option<ModelRegistryEntryView>,
}

impl ModelStatusActive {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "fastModelId": self.fast_model_id,
            "fastDimension": self.fast_dimension,
            "qualityModelId": self.quality_model_id,
            "qualityDimension": self.quality_dimension,
            "semantic": self.semantic,
            "deterministic": self.deterministic,
            "source": self.source,
            "selectedRegistryEntry": self
                .selected_registry_entry
                .as_ref()
                .map(ModelRegistryEntryView::data_json),
        })
    }
}

/// Stable degradation marker for model status / list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

impl ModelDegradation {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

const DEG_NO_REGISTRY_ENTRIES: ModelDegradation = ModelDegradation {
    code: "model_registry_empty",
    severity: "low",
    message: "No models are registered for this workspace; running on deterministic hash fallback.",
    repair: "ee index reembed --workspace .",
};

const DEG_NO_AVAILABLE_MODEL: ModelDegradation = ModelDegradation {
    code: "model_registry_no_available_entry",
    severity: "medium",
    message: "Model registry has entries but none are marked available; semantic search is degraded.",
    repair: "ee doctor --json",
};

/// Report shape returned by `ee model status`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelStatusReport {
    pub schema: &'static str,
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub active: ModelStatusActive,
    pub registered_count: usize,
    pub available_count: usize,
    pub degradations: Vec<ModelDegradation>,
}

impl ModelStatusReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "workspacePath": self.workspace_path.to_string_lossy(),
            "databasePath": self.database_path.to_string_lossy(),
            "active": self.active.data_json(),
            "registeredCount": self.registered_count,
            "availableCount": self.available_count,
            "degradations": self
                .degradations
                .iter()
                .map(ModelDegradation::data_json)
                .collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!(
            "Active embedder: {} (dim {}{}semantic={}, deterministic={})\n",
            self.active.fast_model_id,
            self.active.fast_dimension,
            self.active
                .quality_model_id
                .as_ref()
                .map_or_else(String::new, |id| format!(", quality {id} ")),
            self.active.semantic,
            self.active.deterministic,
        ));
        output.push_str(&format!("Source: {}\n", self.active.source));
        if let Some(selected) = &self.active.selected_registry_entry {
            output.push_str(&format!(
                "Selected registry model: {} ({}/{}, status {})\n",
                selected.id, selected.provider, selected.model_name, selected.status,
            ));
        }
        output.push_str(&format!(
            "Registered models: {} (available: {})\n",
            self.registered_count, self.available_count,
        ));
        if !self.degradations.is_empty() {
            output.push_str("Degraded:\n");
            for degradation in &self.degradations {
                output.push_str(&format!(
                    "  [{}] {} -> {}\n",
                    degradation.severity, degradation.message, degradation.repair,
                ));
            }
        }
        output
    }
}

/// Report shape returned by `ee model list`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelListReport {
    pub schema: &'static str,
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub workspace_id: String,
    pub entries: Vec<ModelRegistryEntryView>,
    pub degradations: Vec<ModelDegradation>,
}

impl ModelListReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "workspacePath": self.workspace_path.to_string_lossy(),
            "databasePath": self.database_path.to_string_lossy(),
            "workspaceId": self.workspace_id,
            "entries": self
                .entries
                .iter()
                .map(ModelRegistryEntryView::data_json)
                .collect::<Vec<_>>(),
            "degradations": self
                .degradations
                .iter()
                .map(ModelDegradation::data_json)
                .collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!(
            "Workspace: {} ({})\n",
            self.workspace_path.display(),
            self.workspace_id,
        ));
        if self.entries.is_empty() {
            output.push_str("No registered models.\n");
        } else {
            output.push_str(&format!("Models ({}):\n", self.entries.len()));
            for entry in &self.entries {
                output.push_str(&format!(
                    "  {}  {}/{}  purpose={}  status={}{}\n",
                    entry.id,
                    entry.provider,
                    entry.model_name,
                    entry.purpose,
                    entry.status,
                    entry
                        .dimension
                        .map_or_else(String::new, |dim| format!("  dim={dim}")),
                ));
            }
        }
        if !self.degradations.is_empty() {
            output.push_str("Degraded:\n");
            for degradation in &self.degradations {
                output.push_str(&format!(
                    "  [{}] {} -> {}\n",
                    degradation.severity, degradation.message, degradation.repair,
                ));
            }
        }
        output
    }
}

fn resolve_workspace_path(path: &Path) -> Result<PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    match absolute.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(error) => Err(DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_string()),
        }),
    }
}

fn resolved_database_path(
    workspace_path: &Path,
    database_path: Option<&Path>,
) -> Result<PathBuf, DomainError> {
    let path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join(DEFAULT_DB_FILE));
    if path.exists() {
        Ok(path)
    } else {
        Err(DomainError::Storage {
            message: format!("Database not found at {}", path.display()),
            repair: Some("ee init --workspace .".to_string()),
        })
    }
}

fn resolve_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DomainError> {
    let path_str = workspace_path.to_string_lossy().into_owned();
    let workspace = connection
        .get_workspace_by_path(&path_str)
        .map_err(|error| {
            db_error_to_domain(
                error,
                "Failed to resolve workspace",
                Some("ee init --workspace .".to_string()),
            )
        })?;
    workspace
        .map(|workspace| workspace.id)
        .ok_or_else(|| DomainError::Configuration {
            message: format!("Workspace not registered for path {path_str}"),
            repair: Some("ee init --workspace .".to_string()),
        })
}

/// Build a `ee model status` report.
pub fn build_model_status_report(
    options: &ModelStatusOptions<'_>,
) -> Result<ModelStatusReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    let database_path = resolved_database_path(&workspace_path, options.database_path)?;
    let connection = DbConnection::open_file(&database_path).map_err(|error| {
        db_error_to_domain(
            error,
            "Failed to open database",
            Some("ee init --workspace .".to_string()),
        )
    })?;
    let workspace_id = resolve_workspace_id(&connection, &workspace_path)?;

    let entries = connection
        .list_model_registry_entries(&workspace_id)
        .map_err(|error| {
            db_error_to_domain(
                error,
                "Failed to list model registry entries",
                Some("ee doctor".to_string()),
            )
        })?;

    let registered_count = entries.len();
    let available_count = entries
        .iter()
        .filter(|entry| entry.status.as_str() == "available")
        .count();

    let selected_registry_entry = entries
        .iter()
        .find(|entry| entry.status.as_str() == "available")
        .cloned()
        .map(ModelRegistryEntryView::from_stored);

    let fast_embedder = HashEmbedder::default_256();
    let quality_embedder = HashEmbedder::default_384();

    let active = ModelStatusActive {
        fast_model_id: fast_embedder.id().to_string(),
        fast_dimension: fast_embedder.dimension(),
        quality_model_id: Some(quality_embedder.id().to_string()),
        quality_dimension: Some(quality_embedder.dimension()),
        semantic: fast_embedder.is_semantic() || quality_embedder.is_semantic(),
        deterministic: true,
        source: if selected_registry_entry.is_some() {
            "registry_observed".to_string()
        } else {
            "frankensearch_hash_fallback".to_string()
        },
        selected_registry_entry,
    };

    let mut degradations = Vec::new();
    if registered_count == 0 {
        degradations.push(DEG_NO_REGISTRY_ENTRIES);
    } else if available_count == 0 {
        degradations.push(DEG_NO_AVAILABLE_MODEL);
    }

    Ok(ModelStatusReport {
        schema: MODEL_STATUS_SCHEMA_V1,
        workspace_path,
        database_path,
        active,
        registered_count,
        available_count,
        degradations,
    })
}

/// Build a `ee model list` report.
pub fn build_model_list_report(
    options: &ModelListOptions<'_>,
) -> Result<ModelListReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    let database_path = resolved_database_path(&workspace_path, options.database_path)?;
    let connection = DbConnection::open_file(&database_path).map_err(|error| {
        db_error_to_domain(
            error,
            "Failed to open database",
            Some("ee init --workspace .".to_string()),
        )
    })?;
    let workspace_id = resolve_workspace_id(&connection, &workspace_path)?;

    let entries = connection
        .list_model_registry_entries(&workspace_id)
        .map_err(|error| {
            db_error_to_domain(
                error,
                "Failed to list model registry entries",
                Some("ee doctor".to_string()),
            )
        })?;

    let mut degradations = Vec::new();
    if entries.is_empty() {
        degradations.push(DEG_NO_REGISTRY_ENTRIES);
    }

    Ok(ModelListReport {
        schema: MODEL_LIST_SCHEMA_V1,
        workspace_path,
        database_path,
        workspace_id,
        entries: entries
            .into_iter()
            .map(ModelRegistryEntryView::from_stored)
            .collect(),
        degradations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateModelRegistryInput, CreateWorkspaceInput};
    use crate::models::model_registry::{
        ModelDistanceMetric, ModelProvider, ModelPurpose, ModelRegistryStatus,
    };
    use std::fs;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn fresh_db_for_workspace(workspace_path: &Path) -> Result<(PathBuf, String), String> {
        fs::create_dir_all(workspace_path.join(".ee"))
            .map_err(|error| format!("create .ee: {error}"))?;
        let database_path = workspace_path.join(".ee").join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| format!("open db: {error}"))?;
        connection
            .migrate()
            .map_err(|error| format!("migrate: {error}"))?;
        let workspace_id = "wsp_01HQ3K5Z00000000000000WORK".to_string();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: workspace_path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned()),
                },
            )
            .map_err(|error| format!("insert workspace: {error}"))?;
        Ok((database_path, workspace_id))
    }

    fn insert_registry_entry(
        database_path: &Path,
        workspace_id: &str,
        id: &str,
        provider: ModelProvider,
        name: &str,
        status: ModelRegistryStatus,
    ) -> TestResult {
        let connection = DbConnection::open_file(database_path)
            .map_err(|error| format!("reopen db: {error}"))?;
        connection
            .insert_model_registry_entry(
                id,
                &CreateModelRegistryInput {
                    workspace_id: workspace_id.to_string(),
                    provider,
                    model_name: name.to_string(),
                    purpose: ModelPurpose::Embedding,
                    dimension: Some(384),
                    distance_metric: Some(ModelDistanceMetric::Cosine),
                    status,
                    version: Some("v1".to_string()),
                    source_uri: None,
                    content_hash: None,
                    metadata_json: None,
                    last_checked_at: None,
                },
            )
            .map_err(|error| format!("insert registry entry: {error}"))
    }

    fn make_workspace() -> Result<(tempfile::TempDir, PathBuf), String> {
        let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let workspace_path = temp
            .path()
            .canonicalize()
            .map_err(|error| format!("canonicalize: {error}"))?;
        Ok((temp, workspace_path))
    }

    #[test]
    fn status_reports_empty_registry_with_degradation() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        fresh_db_for_workspace(&workspace_path)?;

        let report = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("status: {error:?}"))?;

        ensure(report.schema == MODEL_STATUS_SCHEMA_V1, "schema constant")?;
        ensure(report.registered_count == 0, "registered_count")?;
        ensure(report.available_count == 0, "available_count")?;
        ensure(
            report.active.source == "frankensearch_hash_fallback",
            "fallback source",
        )?;
        ensure(report.degradations.len() == 1, "degradation count")?;
        ensure(
            report.degradations[0].code == "model_registry_empty",
            "degradation code",
        )
    }

    #[test]
    fn status_picks_first_available_registry_entry() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        insert_registry_entry(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000001",
            ModelProvider::Hash,
            "fnv1a-256",
            ModelRegistryStatus::Available,
        )?;
        insert_registry_entry(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000002",
            ModelProvider::Model2Vec,
            "minilm",
            ModelRegistryStatus::Disabled,
        )?;

        let report = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("status: {error:?}"))?;

        ensure(report.registered_count == 2, "registered_count")?;
        ensure(report.available_count == 1, "available_count")?;
        ensure(report.degradations.is_empty(), "no degradations")?;
        ensure(
            report.active.source == "registry_observed",
            "registry_observed source",
        )?;
        let selected = report
            .active
            .selected_registry_entry
            .as_ref()
            .ok_or("missing selected entry")?;
        ensure(selected.status == "available", "selected available")
    }

    #[test]
    fn status_marks_no_available_entry_when_all_disabled() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        insert_registry_entry(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000003",
            ModelProvider::Hash,
            "fnv1a-256",
            ModelRegistryStatus::Disabled,
        )?;

        let report = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("status: {error:?}"))?;

        ensure(report.registered_count == 1, "registered_count")?;
        ensure(report.available_count == 0, "available_count")?;
        ensure(report.degradations.len() == 1, "degradation count")?;
        ensure(
            report.degradations[0].code == "model_registry_no_available_entry",
            "degradation code",
        )
    }

    #[test]
    fn list_returns_entries_in_registry_order() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        insert_registry_entry(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000004",
            ModelProvider::Model2Vec,
            "minilm",
            ModelRegistryStatus::Available,
        )?;
        insert_registry_entry(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000005",
            ModelProvider::Hash,
            "fnv1a-256",
            ModelRegistryStatus::Available,
        )?;

        let report = build_model_list_report(&ModelListOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("list: {error:?}"))?;

        ensure(report.schema == MODEL_LIST_SCHEMA_V1, "schema constant")?;
        ensure(report.entries.len() == 2, "entries length")?;
        // list_model_registry_entries orders by purpose, provider, model_name, id
        ensure(report.entries[0].provider == "hash", "first hash")?;
        ensure(
            report.entries[1].provider == "model2vec",
            "second model2vec",
        )?;
        ensure(report.degradations.is_empty(), "no degradations")
    }

    #[test]
    fn json_renderings_are_stable_and_versioned() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        fresh_db_for_workspace(&workspace_path)?;

        let status = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("status: {error:?}"))?;
        let status_json = status.data_json();
        ensure(
            status_json["schema"] == MODEL_STATUS_SCHEMA_V1,
            "status schema",
        )?;
        ensure(
            status_json["active"]["fastModelId"].is_string(),
            "fastModelId is string",
        )?;
        ensure(status_json["registeredCount"] == 0, "registeredCount json")?;

        let list = build_model_list_report(&ModelListOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("list: {error:?}"))?;
        let list_json = list.data_json();
        ensure(list_json["schema"] == MODEL_LIST_SCHEMA_V1, "list schema")?;
        ensure(list_json["entries"].is_array(), "entries is array")
    }
}
