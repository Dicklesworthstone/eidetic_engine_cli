//! `ee model status` / `ee model list` reporting (EE-294).
//!
//! Surfaces the state of the workspace's local embedding/model registry in a
//! stable, machine-readable shape. `ee` does not pick embedding models —
//! Frankensearch owns that decision. These commands expose what the registry
//! knows so agents can introspect availability and degraded-mode posture
//! without scraping `ee index status`.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::db::{CreateModelRegistryInput, DbConnection, DbError, StoredModelRegistryEntry};
use crate::models::DomainError;
use crate::models::model_registry::{ModelProvider, ModelPurpose, ModelRegistryStatus};
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

pub use crate::models::{MODEL_LIST_SCHEMA_V1, MODEL_STATUS_SCHEMA_V2};

const DEFAULT_DB_FILE: &str = "ee.db";
const RERANK_MODEL_MANIFEST_JSON: &str = include_str!("../data/rerank_model_manifest.json");
const DEFAULT_RERANK_MODEL_ALIAS: &str = "rerank-default";
const DEFAULT_RERANK_MODEL_ARTIFACT_NAME: &str = "rerank-default-v1.tar.zst";

pub const RERANK_MODEL_MANIFEST_SCHEMA_V1: &str = "ee.model_manifest.v1";
pub const MODEL_FETCH_SCHEMA_V1: &str = "ee.model_fetch.v1";

#[derive(Debug)]
struct VerifiedRerankArtifact {
    bytes: Vec<u8>,
    content_length_bytes: u64,
    hash_blake3: String,
    hash_sha256: String,
}

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

/// Options for `ee model fetch`.
#[derive(Clone, Debug)]
pub struct ModelFetchOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub model_id: &'a str,
    pub from_file: Option<&'a Path>,
    pub model_store_root: Option<&'a Path>,
}

/// Bundled local-first model manifest for the default reranker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RerankModelManifest {
    pub schema: String,
    pub model_id: String,
    pub hash_blake3: String,
    pub hash_sha256: String,
    pub content_length_bytes: u64,
    pub source_uri: String,
    pub fallback_source_uris: Vec<String>,
    pub license: String,
    pub license_uri: String,
    pub quantization: String,
    pub inference_dimensions: RerankModelInferenceDimensions,
    pub signed_attestation: RerankModelSignedAttestation,
}

impl RerankModelManifest {
    fn validate(&self) -> Result<(), String> {
        if self.schema != RERANK_MODEL_MANIFEST_SCHEMA_V1 {
            return Err(format!(
                "unexpected rerank model manifest schema `{}`",
                self.schema
            ));
        }
        if self.model_id.trim().is_empty() {
            return Err("rerank model manifest has an empty model_id".to_string());
        }
        if !is_hex_hash_64(&self.hash_blake3) {
            return Err("rerank model manifest hash_blake3 must be 64 hex characters".to_string());
        }
        if !is_hex_hash_64(&self.hash_sha256) {
            return Err("rerank model manifest hash_sha256 must be 64 hex characters".to_string());
        }
        if self.content_length_bytes == 0 {
            return Err("rerank model manifest content_length_bytes must be positive".to_string());
        }
        if !is_https_uri(&self.source_uri) {
            return Err("rerank model manifest source_uri must be HTTPS".to_string());
        }
        if self
            .fallback_source_uris
            .iter()
            .any(|source| !is_https_uri(source))
        {
            return Err("rerank model manifest fallback_source_uris must all be HTTPS".to_string());
        }
        if self.license.trim().is_empty() || !is_https_uri(&self.license_uri) {
            return Err(
                "rerank model manifest must include a license and HTTPS license_uri".to_string(),
            );
        }
        if self.quantization.trim().is_empty()
            || self.inference_dimensions.input_max_tokens == 0
            || self.inference_dimensions.output_dimension == 0
        {
            return Err("rerank model manifest inference dimensions are incomplete".to_string());
        }
        if self.signed_attestation.sigstore_bundle.trim().is_empty()
            || self.signed_attestation.signer_identity.trim().is_empty()
            || self.signed_attestation.signed_at.trim().is_empty()
        {
            return Err("rerank model manifest signed_attestation is incomplete".to_string());
        }
        Ok(())
    }

    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "modelId": self.model_id,
            "hashBlake3": self.hash_blake3,
            "hashSha256": self.hash_sha256,
            "contentLengthBytes": self.content_length_bytes,
            "sourceUri": redact_model_source_uri(&self.source_uri),
            "fallbackSourceUris": self
                .fallback_source_uris
                .iter()
                .map(|source| redact_model_source_uri(source))
                .collect::<Vec<_>>(),
            "license": self.license,
            "licenseUri": self.license_uri,
            "quantization": self.quantization,
            "inferenceDimensions": {
                "inputMaxTokens": self.inference_dimensions.input_max_tokens,
                "outputDimension": self.inference_dimensions.output_dimension,
            },
            "signedAttestation": {
                "sigstoreBundle": self.signed_attestation.sigstore_bundle,
                "signerIdentity": self.signed_attestation.signer_identity,
                "signedAt": self.signed_attestation.signed_at,
            },
        })
    }
}

/// Inference shape declared by the rerank model manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RerankModelInferenceDimensions {
    pub input_max_tokens: u32,
    pub output_dimension: u32,
}

/// Sigstore provenance pointer declared by the rerank model manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RerankModelSignedAttestation {
    pub sigstore_bundle: String,
    pub signer_identity: String,
    pub signed_at: String,
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
        let source_uri = self.source_uri.as_deref().map(redact_model_source_uri);
        serde_json::json!({
            "id": self.id,
            "provider": self.provider,
            "modelName": self.model_name,
            "purpose": self.purpose,
            "status": self.status,
            "dimension": self.dimension,
            "distanceMetric": self.distance_metric,
            "version": self.version,
            "sourceUri": source_uri,
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

/// Local reranker registry posture shaped for public output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelStatusReranker {
    pub registered_count: usize,
    pub available_count: usize,
    pub available_model_ids: Vec<String>,
    pub selected_registry_entry: Option<ModelRegistryEntryView>,
    pub manifest: RerankModelManifest,
    pub fetch_command: String,
}

impl ModelStatusReranker {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "registeredCount": self.registered_count,
            "availableCount": self.available_count,
            "availableModelIds": self.available_model_ids,
            "selectedRegistryEntry": self
                .selected_registry_entry
                .as_ref()
                .map(ModelRegistryEntryView::data_json),
            "manifest": self.manifest.data_json(),
            "fetchCommand": self.fetch_command,
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

const DEG_NO_REGISTRY_ENTRIES: ModelDegradation = ModelDegradation {
    code: "model_registry_empty",
    severity: "low",
    message: "No models are registered for this workspace; running on deterministic hash fallback.",
    repair: "ee index reembed --workspace .",
};

const DEG_NO_AVAILABLE_MODEL: ModelDegradation = ModelDegradation {
    code: "model_registry_no_available_entry",
    severity: "medium",
    message: "Model registry has entries but no embedding model is marked available; semantic search is degraded.",
    repair: "ee doctor --json",
};

const DEG_RERANK_MODEL_MISSING: ModelDegradation = ModelDegradation {
    code: "rerank_model_missing",
    severity: "warning",
    message: "A reranker is registered but no default rerank model artifact is marked available.",
    repair: "ee model fetch rerank-default --from-file /path/to/rerank-default-v1.tar.zst",
};

const DEG_RERANK_MODEL_CORRUPT: ModelDegradation = ModelDegradation {
    code: "rerank_model_corrupt",
    severity: "high",
    message: "The registered default rerank model hash does not match the bundled manifest.",
    repair: "Remove the corrupt model artifact and rerun `ee model fetch rerank-default --from-file /path/to/rerank-default-v1.tar.zst`.",
};

const SEMANTIC_DIMENSION_BUDGET: u32 = 384;

const DEG_SEMANTIC_DIMENSION_EXCEEDS_BUDGET: ModelDegradation = ModelDegradation {
    code: "semantic_dimension_exceeds_budget",
    severity: "medium",
    message: "Available embedding model dimension exceeds the configured budget; semantic search is degraded.",
    repair: "select a smaller local embedding model or run `ee index reembed --workspace .`",
};

/// Report shape returned by `ee model status`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelStatusReport {
    pub schema: &'static str,
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub active: ModelStatusActive,
    pub reranker: ModelStatusReranker,
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
            "reranker": self.reranker.data_json(),
            "registeredCount": self.registered_count,
            "availableCount": self.available_count,
            "degradations": model_degradations_data_json("model_status", &self.degradations),
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
        output.push_str(&format!(
            "Rerankers: {} (available: {})\n",
            self.reranker.registered_count, self.reranker.available_count,
        ));
        if let Some(selected) = &self.reranker.selected_registry_entry {
            output.push_str(&format!(
                "Selected reranker: {} ({}/{}, status {})\n",
                selected.id, selected.provider, selected.model_name, selected.status,
            ));
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

/// Report shape returned by `ee model fetch`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelFetchReport {
    pub schema: &'static str,
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub model_id: String,
    pub source_path: PathBuf,
    pub stored_path: PathBuf,
    pub copied: bool,
    pub content_length_bytes: u64,
    pub hash_blake3: String,
    pub hash_sha256: String,
    pub registry_entry: ModelRegistryEntryView,
}

impl ModelFetchReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "workspacePath": self.workspace_path.to_string_lossy(),
            "databasePath": self.database_path.to_string_lossy(),
            "modelId": self.model_id,
            "sourcePath": redact_model_source_uri(&self.source_path.to_string_lossy()),
            "storedPath": redact_model_source_uri(&self.stored_path.to_string_lossy()),
            "copied": self.copied,
            "contentLengthBytes": self.content_length_bytes,
            "hashBlake3": self.hash_blake3,
            "hashSha256": self.hash_sha256,
            "registryEntry": self.registry_entry.data_json(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        format!(
            "Fetched reranker model {} ({} bytes, blake3:{})\nRegistered model: {}\n",
            self.model_id, self.content_length_bytes, self.hash_blake3, self.registry_entry.id,
        )
    }
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
            "degradations": model_degradations_data_json("model_list", &self.degradations),
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

fn model_degradations_data_json(
    source: &'static str,
    degradations: &[ModelDegradation],
) -> Vec<serde_json::Value> {
    aggregate_degraded_entries(degradations.iter().map(|entry| {
        DegradationAggregationInput::new(
            source,
            entry.code,
            entry.severity,
            entry.message,
            entry.repair,
        )
    }))
    .into_iter()
    .map(|entry| {
        serde_json::json!({
            "code": entry.code,
            "severity": entry.severity,
            "message": entry.message,
            "repair": entry.repair,
            "sources": entry.sources,
        })
    })
    .collect()
}

fn redact_model_source_uri(value: &str) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(value).content;
    redact_model_source_path_like_segments(&secret_redacted)
}

fn redact_model_source_path_like_segments(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some((relative_index, _)) = value[cursor..].char_indices().find(|(_, c)| *c == '/')
        else {
            output.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + relative_index;
        if !model_source_path_starts_sensitive_segment(&value[start..]) {
            output.push_str(&value[cursor..=start]);
            cursor = start + 1;
            continue;
        }

        output.push_str(&value[cursor..start]);
        output.push_str("[REDACTED_PATH]");
        cursor = value[start..]
            .char_indices()
            .find_map(|(index, c)| model_source_path_boundary(c).then_some(start + index))
            .unwrap_or(value.len());
    }
    output
}

fn model_source_path_starts_sensitive_segment(value: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "/Users/",
        "/Volumes/",
        "/private/",
        "/var/",
        "/tmp/",
        "/home/",
        "/data/",
        "/dp/",
        "/workspace/",
        "/repo/",
        "/etc/",
    ];

    PREFIXES.iter().any(|prefix| value.starts_with(prefix))
}

fn model_source_path_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '?' | '#' | '"' | '\'' | ')' | ']' | '}' | ',' | ';')
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

    ensure_no_model_database_symlink_components(&path)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(path),
        Ok(_) => Err(DomainError::Storage {
            message: format!("Database path {} is not a regular file", path.display()),
            repair: Some(
                "Replace it with an ee database file or run `ee init --workspace .`.".to_string(),
            ),
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => Err(DomainError::Storage {
            message: format!("Database not found at {}", path.display()),
            repair: Some("ee init --workspace .".to_string()),
        }),
        Err(error) if error.kind() == ErrorKind::NotADirectory => Err(DomainError::Storage {
            message: format!("Database path {} is not reachable: {error}", path.display()),
            repair: Some("ee init --workspace .".to_string()),
        }),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to inspect database path {}: {error}",
                path.display()
            ),
            repair: Some("Check workspace permissions or run `ee doctor --json`.".to_string()),
        }),
    }
}

fn ensure_no_model_database_symlink_components(path: &Path) -> Result<(), DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Database path {} contains symlink component {}",
                        path.display(),
                        current.display()
                    ),
                    repair: Some(
                        "Use a real ee database path inside the workspace and rerun `ee init --workspace .` if needed."
                            .to_string(),
                    ),
                });
            }
            Ok(_) => {}
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Failed to inspect database path component {}: {error}",
                        current.display()
                    ),
                    repair: Some(
                        "Check workspace permissions or run `ee doctor --json`.".to_string(),
                    ),
                });
            }
        }
    }
    Ok(())
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
    let manifest = bundled_rerank_model_manifest()?;
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
        .find(|entry| entry_is_available_embedding(entry))
        .cloned()
        .map(ModelRegistryEntryView::from_stored);

    let reranker_registered_count = entries
        .iter()
        .filter(|entry| entry_is_reranker(entry))
        .count();
    let reranker_available_entries: Vec<_> = entries
        .iter()
        .filter(|entry| entry_is_available_reranker(entry))
        .collect();
    let reranker = ModelStatusReranker {
        registered_count: reranker_registered_count,
        available_count: reranker_available_entries.len(),
        available_model_ids: reranker_available_entries
            .iter()
            .map(|entry| entry.model_name.clone())
            .collect(),
        selected_registry_entry: reranker_available_entries
            .first()
            .map(|entry| (*entry).clone())
            .map(ModelRegistryEntryView::from_stored),
        manifest: manifest.clone(),
        fetch_command: format!("ee model fetch {DEFAULT_RERANK_MODEL_ALIAS}"),
    };

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
    } else if active.selected_registry_entry.is_none() {
        degradations.push(DEG_NO_AVAILABLE_MODEL);
    }
    if entries.iter().any(entry_exceeds_semantic_dimension_budget) {
        degradations.push(DEG_SEMANTIC_DIMENSION_EXCEEDS_BUDGET);
    }
    degradations.extend(rerank_model_degradations(
        &entries,
        &manifest,
        reranker_registered_count,
        reranker_available_entries.len(),
    ));

    Ok(ModelStatusReport {
        schema: MODEL_STATUS_SCHEMA_V2,
        workspace_path,
        database_path,
        active,
        reranker,
        registered_count,
        available_count,
        degradations,
    })
}

fn entry_exceeds_semantic_dimension_budget(entry: &StoredModelRegistryEntry) -> bool {
    entry_is_available_embedding(entry)
        && entry
            .dimension
            .is_some_and(|dimension| dimension > SEMANTIC_DIMENSION_BUDGET)
}

fn entry_is_available_embedding(entry: &StoredModelRegistryEntry) -> bool {
    entry.purpose.as_str() == "embedding" && entry.status.as_str() == "available"
}

fn entry_is_reranker(entry: &StoredModelRegistryEntry) -> bool {
    entry.purpose.as_str() == "reranker"
}

fn entry_is_available_reranker(entry: &StoredModelRegistryEntry) -> bool {
    entry_is_reranker(entry) && entry.status.as_str() == "available"
}

/// Build a `ee model list` report.
pub fn build_model_list_report(
    options: &ModelListOptions<'_>,
) -> Result<ModelListReport, DomainError> {
    let manifest = bundled_rerank_model_manifest()?;
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
    } else if !entries.iter().any(entry_is_available_embedding) {
        degradations.push(DEG_NO_AVAILABLE_MODEL);
    }
    let reranker_registered_count = entries
        .iter()
        .filter(|entry| entry_is_reranker(entry))
        .count();
    let reranker_available_count = entries
        .iter()
        .filter(|entry| entry_is_available_reranker(entry))
        .count();
    degradations.extend(rerank_model_degradations(
        &entries,
        &manifest,
        reranker_registered_count,
        reranker_available_count,
    ));

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

/// Parse and validate the bundled manifest for the default rerank model.
pub fn bundled_rerank_model_manifest() -> Result<RerankModelManifest, DomainError> {
    let manifest: RerankModelManifest =
        serde_json::from_str(RERANK_MODEL_MANIFEST_JSON).map_err(|error| {
            DomainError::Configuration {
                message: format!("Bundled rerank model manifest is invalid JSON: {error}"),
                repair: Some("Fix src/data/rerank_model_manifest.json.".to_string()),
            }
        })?;
    manifest
        .validate()
        .map_err(|message| DomainError::Configuration {
            message,
            repair: Some("Fix src/data/rerank_model_manifest.json.".to_string()),
        })?;
    Ok(manifest)
}

/// Fetch and register the default rerank model.
pub fn fetch_rerank_model(
    options: &ModelFetchOptions<'_>,
) -> Result<ModelFetchReport, DomainError> {
    let manifest = resolve_rerank_model_manifest(options.model_id)?;
    let Some(source_path) = options.from_file else {
        return Err(DomainError::Configuration {
            message: "Network model fetch is not available in this build; use the explicit offline artifact path."
                .to_string(),
            repair: Some(format!(
                "ee model fetch {DEFAULT_RERANK_MODEL_ALIAS} --from-file /path/to/{DEFAULT_RERANK_MODEL_ARTIFACT_NAME}"
            )),
        });
    };

    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    let database_path = resolved_database_path(&workspace_path, options.database_path)?;
    let source_artifact =
        read_verified_rerank_model_artifact(source_path, &manifest, "rerank model artifact")?;
    let content_length_bytes = source_artifact.content_length_bytes;
    let hash_blake3 = source_artifact.hash_blake3.clone();
    let hash_sha256 = source_artifact.hash_sha256.clone();

    let store_root = options
        .model_store_root
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(default_model_store_root)?;
    let stored_dir = store_root.join("rerank").join(&manifest.model_id);
    let stored_path = stored_dir.join(DEFAULT_RERANK_MODEL_ARTIFACT_NAME);
    ensure_no_model_artifact_symlink_components(&stored_dir, "model store directory")?;
    fs::create_dir_all(&stored_dir).map_err(|error| DomainError::Configuration {
        message: format!(
            "Failed to create rerank model store {}: {error}",
            stored_dir.display()
        ),
        repair: Some("Check model store permissions.".to_string()),
    })?;
    ensure_no_model_artifact_symlink_components(&stored_dir, "model store directory")?;
    let copied = match fs::symlink_metadata(&stored_path) {
        Ok(_) => {
            let existing_artifact = read_verified_rerank_model_artifact(
                &stored_path,
                &manifest,
                "existing rerank model artifact",
            )?;
            if existing_artifact.hash_blake3 != manifest.hash_blake3 {
                return Err(DomainError::Configuration {
                    message: format!(
                        "Existing rerank model artifact {} does not match the bundled manifest",
                        stored_path.display()
                    ),
                    repair: Some("Move the bad artifact aside and rerun model fetch.".to_string()),
                });
            }
            false
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            write_rerank_model_artifact(&stored_path, &source_artifact.bytes)?;
            true
        }
        Err(error) => {
            return Err(DomainError::Configuration {
                message: format!(
                    "Failed to inspect existing rerank model artifact {}: {error}",
                    stored_path.display(),
                ),
                repair: Some("Move the bad artifact aside and rerun model fetch.".to_string()),
            });
        }
    };

    let connection = DbConnection::open_file(&database_path).map_err(|error| {
        db_error_to_domain(
            error,
            "Failed to open database",
            Some("ee init --workspace .".to_string()),
        )
    })?;
    let workspace_id = resolve_workspace_id(&connection, &workspace_path)?;
    let registry_entry = match connection
        .find_model_registry_entry(
            &workspace_id,
            ModelProvider::External,
            &manifest.model_id,
            ModelPurpose::Reranker,
        )
        .map_err(|error| {
            db_error_to_domain(
                error,
                "Failed to inspect existing rerank model registry entry",
                Some("ee model status --workspace . --json".to_string()),
            )
        })? {
        Some(entry)
            if entry.status == ModelRegistryStatus::Available
                && entry
                    .content_hash
                    .as_deref()
                    .is_some_and(|hash| model_content_hash_matches_manifest(hash, &manifest)) =>
        {
            entry
        }
        Some(entry) if entry.status == ModelRegistryStatus::Available => {
            return Err(DomainError::Configuration {
                message: format!(
                    "Rerank model registry entry {} is available but does not match the bundled manifest",
                    entry.id
                ),
                repair: Some(
                    "Inspect the existing model entry before fetching again: ee model status --workspace . --json"
                        .to_string(),
                ),
            });
        }
        Some(entry) => {
            return Err(DomainError::Configuration {
                message: format!(
                    "Rerank model registry entry {} already exists with status {}",
                    entry.id, entry.status
                ),
                repair: Some(
                    "Use ee diag model-registry to inspect the stale entry before fetching again."
                        .to_string(),
                ),
            });
        }
        None => {
            let id = generate_model_registry_id();
            connection
                .insert_model_registry_entry(
                    &id,
                    &CreateModelRegistryInput {
                        workspace_id: workspace_id.clone(),
                        provider: ModelProvider::External,
                        model_name: manifest.model_id.clone(),
                        purpose: ModelPurpose::Reranker,
                        dimension: Some(manifest.inference_dimensions.output_dimension),
                        distance_metric: None,
                        status: ModelRegistryStatus::Available,
                        version: Some(manifest.model_id.clone()),
                        source_uri: Some(stored_path.to_string_lossy().into_owned()),
                        content_hash: Some(format!("blake3:{}", manifest.hash_blake3)),
                        metadata_json: Some(rerank_model_metadata_json(&manifest, &stored_path)?),
                        last_checked_at: Some(Utc::now().to_rfc3339()),
                    },
                )
                .map_err(|error| {
                    db_error_to_domain(
                        error,
                        "Failed to register rerank model",
                        Some("ee model status --workspace . --json".to_string()),
                    )
                })?;
            connection
                .get_model_registry_entry(&id)
                .map_err(|error| {
                    db_error_to_domain(
                        error,
                        "Failed to reload registered rerank model",
                        Some("ee model status --workspace . --json".to_string()),
                    )
                })?
                .ok_or_else(|| DomainError::Storage {
                    message: format!("Registered rerank model {id} was not readable"),
                    repair: Some("ee doctor --json".to_string()),
                })?
        }
    };

    connection
        .insert_audit(
            &crate::db::generate_audit_id(),
            &crate::db::CreateAuditInput {
                workspace_id: Some(workspace_id),
                actor: None,
                action: "model.fetched".to_string(),
                target_type: Some("model_registry".to_string()),
                target_id: Some(registry_entry.id.clone()),
                details: Some(
                    serde_json::json!({
                        "schema": MODEL_FETCH_SCHEMA_V1,
                        "modelId": manifest.model_id.clone(),
                        "storedPath": stored_path.to_string_lossy(),
                        "hashBlake3": hash_blake3.clone(),
                        "hashSha256": hash_sha256.clone(),
                        "copied": copied,
                    })
                    .to_string(),
                ),
            },
        )
        .map_err(|error| {
            db_error_to_domain(
                error,
                "Failed to audit rerank model fetch",
                Some("ee audit verify --workspace . --json".to_string()),
            )
        })?;

    Ok(ModelFetchReport {
        schema: MODEL_FETCH_SCHEMA_V1,
        workspace_path,
        database_path,
        model_id: manifest.model_id,
        source_path: source_path.to_path_buf(),
        stored_path,
        copied,
        content_length_bytes,
        hash_blake3,
        hash_sha256,
        registry_entry: ModelRegistryEntryView::from_stored(registry_entry),
    })
}

fn read_verified_rerank_model_artifact(
    path: &Path,
    manifest: &RerankModelManifest,
    label: &str,
) -> Result<VerifiedRerankArtifact, DomainError> {
    ensure_no_model_artifact_symlink_components(path, label)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| DomainError::Configuration {
        message: format!("Failed to inspect {label} {}: {error}", path.display()),
        repair: Some("Pass a readable artifact path to --from-file.".to_string()),
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(DomainError::Configuration {
            message: format!("{label} {} is not a regular file", path.display()),
            repair: Some("Pass a regular model artifact file.".to_string()),
        });
    }
    if metadata.len() != manifest.content_length_bytes {
        return Err(rerank_artifact_length_mismatch(
            path,
            manifest,
            metadata.len(),
        ));
    }

    let file = open_model_artifact_file_for_read_no_follow(path).map_err(|error| {
        DomainError::Configuration {
            message: format!("Failed to read {label} {}: {error}", path.display()),
            repair: Some("Pass a readable artifact path to --from-file.".to_string()),
        }
    })?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to inspect opened {label} {}: {error}",
                path.display()
            ),
            repair: Some("Pass a readable artifact path to --from-file.".to_string()),
        })?;
    if !opened_metadata.file_type().is_file() {
        return Err(DomainError::Configuration {
            message: format!("Opened {label} {} is not a regular file", path.display()),
            repair: Some("Pass a regular model artifact file.".to_string()),
        });
    }
    if opened_metadata.len() != manifest.content_length_bytes {
        return Err(rerank_artifact_length_mismatch(
            path,
            manifest,
            opened_metadata.len(),
        ));
    }

    let mut bytes = Vec::new();
    file.take(manifest.content_length_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| DomainError::Configuration {
            message: format!("Failed to read {label} {}: {error}", path.display()),
            repair: Some("Pass a readable artifact path to --from-file.".to_string()),
        })?;
    let content_length_bytes =
        u64::try_from(bytes.len()).map_err(|error| DomainError::Configuration {
            message: format!("Rerank model artifact is too large to measure: {error}"),
            repair: Some("Use the manifest-sized rerank artifact.".to_string()),
        })?;
    if content_length_bytes != manifest.content_length_bytes {
        return Err(rerank_artifact_length_mismatch(
            path,
            manifest,
            content_length_bytes,
        ));
    }

    let hash_blake3 = blake3_hash_hex(&bytes);
    let hash_sha256 = sha256_hash_hex(&bytes);
    if hash_blake3 != manifest.hash_blake3 || hash_sha256 != manifest.hash_sha256 {
        return Err(DomainError::Configuration {
            message: "Rerank model artifact hash mismatch against bundled manifest.".to_string(),
            repair: Some(format!(
                "Re-fetch {} from the manifest source and rerun with --from-file.",
                manifest.model_id
            )),
        });
    }

    Ok(VerifiedRerankArtifact {
        bytes,
        content_length_bytes,
        hash_blake3,
        hash_sha256,
    })
}

fn rerank_artifact_length_mismatch(
    path: &Path,
    manifest: &RerankModelManifest,
    actual_len: u64,
) -> DomainError {
    DomainError::Configuration {
        message: format!(
            "Rerank model artifact length mismatch for {}: expected {}, found {}",
            path.display(),
            manifest.content_length_bytes,
            actual_len
        ),
        repair: Some(format!(
            "Use the artifact documented in src/data/rerank_model_manifest.json for {}.",
            manifest.model_id
        )),
    }
}

fn write_rerank_model_artifact(path: &Path, bytes: &[u8]) -> Result<(), DomainError> {
    ensure_no_model_artifact_symlink_components(path, "model artifact destination")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    configure_model_artifact_open_no_follow(&mut options);
    let mut file = options
        .open(path)
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to copy rerank model artifact to {}: {error}",
                path.display()
            ),
            repair: Some("Check model store permissions and free space.".to_string()),
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to write rerank model artifact to {}: {error}",
                path.display()
            ),
            repair: Some("Check model store permissions and free space.".to_string()),
        })
}

fn open_model_artifact_file_for_read_no_follow(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_model_artifact_open_no_follow(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_model_artifact_open_no_follow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_model_artifact_open_no_follow(_options: &mut fs::OpenOptions) {}

fn ensure_no_model_artifact_symlink_components(
    path: &Path,
    label: &str,
) -> Result<(), DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DomainError::Configuration {
                    message: format!(
                        "{label} {} contains symlink component {}",
                        path.display(),
                        current.display()
                    ),
                    repair: Some("Use real, non-symlink model artifact paths.".to_string()),
                });
            }
            Ok(_) => {}
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(DomainError::Configuration {
                    message: format!(
                        "Failed to inspect {label} path component {}: {error}",
                        current.display()
                    ),
                    repair: Some("Check model artifact path permissions.".to_string()),
                });
            }
        }
    }
    Ok(())
}

fn resolve_rerank_model_manifest(model_id: &str) -> Result<RerankModelManifest, DomainError> {
    let manifest = bundled_rerank_model_manifest()?;
    if model_id == DEFAULT_RERANK_MODEL_ALIAS || model_id == manifest.model_id {
        Ok(manifest)
    } else {
        Err(DomainError::Usage {
            message: format!(
                "unknown model `{model_id}`; expected `{DEFAULT_RERANK_MODEL_ALIAS}` or `{}`",
                manifest.model_id
            ),
            repair: Some(format!("ee model fetch {DEFAULT_RERANK_MODEL_ALIAS}")),
        })
    }
}

fn rerank_model_degradations(
    entries: &[StoredModelRegistryEntry],
    manifest: &RerankModelManifest,
    reranker_registered_count: usize,
    reranker_available_count: usize,
) -> Vec<ModelDegradation> {
    let mut degradations = Vec::new();
    if reranker_registered_count > 0 && reranker_available_count == 0 {
        degradations.push(DEG_RERANK_MODEL_MISSING);
    }
    if entries
        .iter()
        .filter(|entry| entry_is_available_reranker(entry))
        .any(|entry| {
            entry.model_name == manifest.model_id
                && entry
                    .content_hash
                    .as_deref()
                    .is_some_and(|hash| !model_content_hash_matches_manifest(hash, manifest))
        })
    {
        degradations.push(DEG_RERANK_MODEL_CORRUPT);
    }
    degradations
}

fn model_content_hash_matches_manifest(hash: &str, manifest: &RerankModelManifest) -> bool {
    hash.strip_prefix("blake3:")
        .is_some_and(|value| value.eq_ignore_ascii_case(&manifest.hash_blake3))
}

fn rerank_model_metadata_json(
    manifest: &RerankModelManifest,
    stored_path: &Path,
) -> Result<String, DomainError> {
    serde_json::to_string(&serde_json::json!({
        "schema": "ee.rerank_model_registry_metadata.v1",
        "manifest": manifest.data_json(),
        "storedPath": stored_path.to_string_lossy(),
    }))
    .map_err(|error| DomainError::Configuration {
        message: format!("Failed to render rerank model metadata: {error}"),
        repair: Some("Check src/data/rerank_model_manifest.json.".to_string()),
    })
}

fn default_model_store_root() -> Result<PathBuf, DomainError> {
    let home = std::env::var_os("HOME").ok_or_else(|| DomainError::Configuration {
        message: "HOME is not set; cannot resolve the default ee model store.".to_string(),
        repair: Some("Pass a model store through the calling harness or set HOME.".to_string()),
    })?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("ee")
        .join("models"))
}

fn generate_model_registry_id() -> String {
    let simple = uuid::Uuid::now_v7().simple().to_string();
    format!("mdl_{}", &simple[..26])
}

fn blake3_hash_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn sha256_hash_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn is_hex_hash_64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_https_uri(value: &str) -> bool {
    value.starts_with("https://")
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
        insert_registry_entry_with_dimension(
            database_path,
            workspace_id,
            id,
            provider,
            name,
            status,
            384,
        )
    }

    fn insert_registry_entry_with_dimension(
        database_path: &Path,
        workspace_id: &str,
        id: &str,
        provider: ModelProvider,
        name: &str,
        status: ModelRegistryStatus,
        dimension: u32,
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
                    dimension: Some(dimension),
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

    fn insert_reranker_entry(
        database_path: &Path,
        workspace_id: &str,
        id: &str,
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
                    provider: ModelProvider::FastEmbed,
                    model_name: name.to_string(),
                    purpose: ModelPurpose::Reranker,
                    dimension: None,
                    distance_metric: None,
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

    fn empty_reranker_status() -> ModelStatusReranker {
        let manifest =
            bundled_rerank_model_manifest().expect("bundled rerank model manifest should parse");
        ModelStatusReranker {
            registered_count: 0,
            available_count: 0,
            available_model_ids: Vec::new(),
            selected_registry_entry: None,
            manifest,
            fetch_command: format!("ee model fetch {DEFAULT_RERANK_MODEL_ALIAS}"),
        }
    }

    fn model_entry_with_source(source_uri: &str) -> ModelRegistryEntryView {
        ModelRegistryEntryView {
            id: "mdl_output_redaction".to_owned(),
            provider: "model2vec".to_owned(),
            model_name: "private-model".to_owned(),
            purpose: "embedding".to_owned(),
            status: "available".to_owned(),
            dimension: Some(384),
            distance_metric: Some("cosine".to_owned()),
            version: Some("v1".to_owned()),
            source_uri: Some(source_uri.to_owned()),
            content_hash: None,
            created_at: "2026-05-17T00:00:00Z".to_owned(),
            updated_at: "2026-05-17T00:01:00Z".to_owned(),
            last_checked_at: None,
        }
    }

    fn make_workspace() -> Result<(tempfile::TempDir, PathBuf), String> {
        let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let workspace_path = temp
            .path()
            .canonicalize()
            .map_err(|error| format!("canonicalize: {error}"))?;
        Ok((temp, workspace_path))
    }

    fn manifest_for_artifact(bytes: &[u8]) -> Result<RerankModelManifest, String> {
        let mut manifest =
            bundled_rerank_model_manifest().map_err(|error| error.message().to_owned())?;
        manifest.content_length_bytes =
            u64::try_from(bytes.len()).map_err(|error| error.to_string())?;
        manifest.hash_blake3 = blake3_hash_hex(bytes);
        manifest.hash_sha256 = sha256_hash_hex(bytes);
        Ok(manifest)
    }

    #[test]
    fn rerank_model_artifact_read_rejects_length_mismatch_before_hashing() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let path = temp.path().join("rerank-default-v1.tar.zst");
        fs::write(&path, b"too long").map_err(|error| format!("write model artifact: {error}"))?;
        let manifest = manifest_for_artifact(b"short")?;

        let error = read_verified_rerank_model_artifact(&path, &manifest, "rerank model artifact")
            .expect_err("length-mismatched model artifact should be rejected");

        ensure(
            error.message().contains("length mismatch"),
            "length mismatch error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn rerank_model_artifact_read_rejects_symlinked_source() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let real_path = temp.path().join("real.tar.zst");
        let linked_path = temp.path().join("linked.tar.zst");
        fs::write(&real_path, b"model bytes")
            .map_err(|error| format!("write model artifact: {error}"))?;
        std::os::unix::fs::symlink(&real_path, &linked_path)
            .map_err(|error| format!("symlink model artifact: {error}"))?;
        let manifest = manifest_for_artifact(b"model bytes")?;

        let error =
            read_verified_rerank_model_artifact(&linked_path, &manifest, "rerank model artifact")
                .expect_err("symlinked model artifact source should be rejected");

        ensure(
            error.message().contains("symlink component"),
            "symlinked model source error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn rerank_model_artifact_write_rejects_existing_symlink_destination() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let real_path = temp.path().join("real.tar.zst");
        let linked_path = temp.path().join("linked.tar.zst");
        fs::write(&real_path, b"outside")
            .map_err(|error| format!("write existing artifact: {error}"))?;
        std::os::unix::fs::symlink(&real_path, &linked_path)
            .map_err(|error| format!("symlink destination: {error}"))?;

        let error = write_rerank_model_artifact(&linked_path, b"model bytes")
            .expect_err("symlinked model destination should be rejected");

        ensure(
            error.message().contains("symlink component"),
            "symlinked model destination error",
        )?;
        ensure(
            fs::read(&real_path).map_err(|error| error.to_string())? == b"outside",
            "symlink destination target must remain unchanged",
        )
    }

    #[test]
    fn status_preserves_database_not_found_error() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;

        let error = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .expect_err("missing database should return a storage error");

        ensure(
            error.message().contains("Database not found"),
            "missing database error",
        )
    }

    #[test]
    fn status_rejects_non_regular_database_path() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let database_path = workspace_path.join(".ee").join(DEFAULT_DB_FILE);
        fs::create_dir_all(&database_path).map_err(|error| format!("create db dir: {error}"))?;

        let error = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .expect_err("directory database path should be rejected");

        ensure(
            error.message().contains("not a regular file"),
            "non-regular database error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn list_rejects_symlinked_database_path() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        fs::create_dir_all(workspace_path.join(".ee"))
            .map_err(|error| format!("create .ee: {error}"))?;
        let outside = workspace_path.join("outside-ee.db");
        fs::write(&outside, b"not sqlite").map_err(|error| format!("write outside db: {error}"))?;
        std::os::unix::fs::symlink(&outside, workspace_path.join(".ee").join(DEFAULT_DB_FILE))
            .map_err(|error| format!("symlink db: {error}"))?;

        let error = build_model_list_report(&ModelListOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .expect_err("symlinked database path should be rejected");

        ensure(
            error.message().contains("symlink component"),
            "symlinked database error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn status_rejects_database_under_symlinked_parent() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let workspace_path = temp
            .path()
            .join("workspace")
            .canonicalize()
            .unwrap_or_else(|_| temp.path().join("workspace"));
        fs::create_dir_all(&workspace_path)
            .map_err(|error| format!("create workspace: {error}"))?;
        let real_ee = temp.path().join("real-ee");
        fs::create_dir_all(&real_ee).map_err(|error| format!("create real-ee: {error}"))?;
        fs::write(real_ee.join(DEFAULT_DB_FILE), b"not sqlite")
            .map_err(|error| format!("write real db: {error}"))?;
        std::os::unix::fs::symlink(&real_ee, workspace_path.join(".ee"))
            .map_err(|error| format!("symlink .ee: {error}"))?;
        let workspace_path = workspace_path
            .canonicalize()
            .map_err(|error| format!("canonicalize workspace: {error}"))?;

        let error = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .expect_err("database under symlinked parent should be rejected");

        ensure(
            error.message().contains("symlink component"),
            "symlinked database parent error",
        )
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

        ensure(report.schema == MODEL_STATUS_SCHEMA_V2, "schema constant")?;
        ensure(report.registered_count == 0, "registered_count")?;
        ensure(report.available_count == 0, "available_count")?;
        ensure(
            report.reranker.registered_count == 0 && report.reranker.available_count == 0,
            "reranker counts empty",
        )?;
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
    fn status_reports_available_reranker_without_selecting_it_as_embedder() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        insert_reranker_entry(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000012",
            "ms-marco-minilm-l-6-v2",
            ModelRegistryStatus::Available,
        )?;

        let report = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("status: {error:?}"))?;

        ensure(report.registered_count == 1, "registered_count")?;
        ensure(report.available_count == 1, "available_count")?;
        ensure(
            report.active.source == "frankensearch_hash_fallback",
            "reranker must not become the active embedder source",
        )?;
        ensure(
            report.active.selected_registry_entry.is_none(),
            "active embedding selection should ignore reranker entries",
        )?;
        ensure(report.degradations.len() == 1, "degradation count")?;
        ensure(
            report.degradations[0].code == "model_registry_no_available_entry",
            "reranker-only registry should degrade semantic embedding status",
        )?;
        ensure(report.reranker.registered_count == 1, "reranker registered")?;
        ensure(report.reranker.available_count == 1, "reranker available")?;
        ensure(
            report.reranker.available_model_ids == vec!["ms-marco-minilm-l-6-v2"],
            "available reranker model ids",
        )?;
        let selected = report
            .reranker
            .selected_registry_entry
            .as_ref()
            .ok_or("selected reranker missing")?;
        ensure(selected.purpose == "reranker", "selected reranker purpose")?;

        let json = report.data_json();
        ensure(
            json["reranker"]["availableModelIds"] == serde_json::json!(["ms-marco-minilm-l-6-v2"]),
            "reranker JSON available ids",
        )
    }

    #[test]
    fn status_marks_oversized_available_embedding_model() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        insert_registry_entry_with_dimension(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000006",
            ModelProvider::Hash,
            "oversized-4096",
            ModelRegistryStatus::Available,
            SEMANTIC_DIMENSION_BUDGET + 1,
        )?;

        let report = build_model_status_report(&ModelStatusOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("status: {error:?}"))?;

        ensure(report.registered_count == 1, "registered_count")?;
        ensure(report.available_count == 1, "available_count")?;
        ensure(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "semantic_dimension_exceeds_budget"),
            "semantic dimension degradation",
        )
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
    fn status_json_aggregates_duplicate_model_degradations() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let report = ModelStatusReport {
            schema: MODEL_STATUS_SCHEMA_V2,
            workspace_path: workspace_path.clone(),
            database_path: workspace_path.join(".ee").join("ee.db"),
            active: ModelStatusActive {
                fast_model_id: "hash:deterministic".to_string(),
                fast_dimension: 384,
                quality_model_id: None,
                quality_dimension: None,
                semantic: false,
                deterministic: true,
                source: "unit_fixture".to_string(),
                selected_registry_entry: None,
            },
            reranker: empty_reranker_status(),
            registered_count: 2,
            available_count: 0,
            degradations: vec![
                ModelDegradation {
                    code: "model_registry_no_available_entry",
                    severity: "low",
                    message: "No available model entry.",
                    repair: "ee model list --workspace . --json",
                },
                ModelDegradation {
                    code: "model_registry_no_available_entry",
                    severity: "medium",
                    message: "Model registry has no available semantic model.",
                    repair: "ee doctor --json",
                },
            ],
        };

        let json = report.data_json();
        let degraded = json["degradations"]
            .as_array()
            .ok_or_else(|| "model status degradations should be an array".to_string())?;

        ensure(
            degraded.len() == 1,
            format!("duplicate model degradations should collapse: {degraded:?}"),
        )?;
        ensure(
            degraded[0]["code"] == "model_registry_no_available_entry",
            "aggregate should preserve the model degraded code",
        )?;
        ensure(
            degraded[0]["severity"] == "medium",
            "aggregate should escalate to the worst severity",
        )?;
        ensure(
            degraded[0]["repair"] == "ee doctor --json",
            "aggregate should keep the highest-severity repair hint",
        )?;
        ensure(
            degraded[0]["sources"] == serde_json::json!(["model_status"]),
            "aggregate should expose the model status source label",
        )
    }

    #[test]
    fn model_registry_entry_json_redacts_sensitive_source_uri() -> TestResult {
        let entry = model_entry_with_source(
            "file:///Users/alice/private/models/model.json?api_key=redaction-fixture",
        );

        let json = entry.data_json().to_string();

        ensure(
            json.contains("[REDACTED_PATH]"),
            format!("model entry JSON should redact absolute path: {json}"),
        )?;
        ensure(
            json.contains("[REDACTED:"),
            format!("model entry JSON should redact secret-like source URI: {json}"),
        )?;
        ensure(
            !json.contains("/Users/alice") && !json.contains("redaction-fixture"),
            format!("model entry JSON leaked sensitive source URI: {json}"),
        )
    }

    #[test]
    fn model_status_and_list_redact_selected_entry_source_uri() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let entry = model_entry_with_source(
            "file:///Volumes/USBNVME16TB/private/models/model.json#token=redaction-fixture",
        );
        let report = ModelStatusReport {
            schema: MODEL_STATUS_SCHEMA_V2,
            workspace_path: workspace_path.clone(),
            database_path: workspace_path.join(".ee").join("ee.db"),
            active: ModelStatusActive {
                fast_model_id: "registry:private-model".to_owned(),
                fast_dimension: 384,
                quality_model_id: None,
                quality_dimension: None,
                semantic: true,
                deterministic: true,
                source: "registry_observed".to_owned(),
                selected_registry_entry: Some(entry.clone()),
            },
            reranker: empty_reranker_status(),
            registered_count: 1,
            available_count: 1,
            degradations: Vec::new(),
        };
        let list = ModelListReport {
            schema: MODEL_LIST_SCHEMA_V1,
            workspace_path: workspace_path.clone(),
            database_path: workspace_path.join(".ee").join("ee.db"),
            workspace_id: "wsp_output_redaction".to_owned(),
            entries: vec![entry],
            degradations: Vec::new(),
        };

        for (surface, value) in [("status", report.data_json()), ("list", list.data_json())] {
            let json = value.to_string();
            ensure(
                json.contains("[REDACTED_PATH]"),
                format!("model {surface} JSON should redact absolute path: {json}"),
            )?;
            ensure(
                !json.contains("/Volumes/USBNVME16TB") && !json.contains("redaction-fixture"),
                format!("model {surface} JSON leaked sensitive source URI: {json}"),
            )?;
        }
        Ok(())
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
    fn list_reports_reranker_only_registry_as_no_available_embedding() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        insert_reranker_entry(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000013",
            "ms-marco-minilm-l-6-v2",
            ModelRegistryStatus::Available,
        )?;

        let report = build_model_list_report(&ModelListOptions {
            workspace_path: &workspace_path,
            database_path: None,
        })
        .map_err(|error| format!("list: {error:?}"))?;

        ensure(report.entries.len() == 1, "reranker entry listed")?;
        ensure(report.degradations.len() == 1, "degradation count")?;
        ensure(
            report.degradations[0].code == "model_registry_no_available_entry",
            "reranker-only list should degrade semantic embedding status",
        )
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
            status_json["schema"] == MODEL_STATUS_SCHEMA_V2,
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
