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

use crate::config::workspace_fingerprint;
use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::core::index::{
    DEFAULT_INDEX_SUBDIR, EmbeddingPosture, IndexHealth, IndexStatusOptions,
    current_embedding_posture, get_index_status_with_connection,
};
use crate::db::{
    CreateEmbeddingMetadataInput, CreateModelRegistryInput, DbConnection, DbError,
    StoredModelRegistryEntry,
};
use crate::models::DomainError;
use crate::models::model_registry::{
    EmbeddingMetadataRecord, EmbeddingPooling, ModelDistanceMetric, ModelProvider, ModelPurpose,
    ModelRegistryStatus,
};

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
pub const MODEL_LIFECYCLE_SCHEMA_V1: &str = "ee.model_lifecycle.v1";

const MODEL_LIFECYCLE_REDACTION_STATUS: &str = "paths_workspace_relative_or_hashed_no_content";
const MODEL_LIFECYCLE_INDEX_ID: &str = "search-main";
const MODEL_LIFECYCLE_INDEX_METADATA_FILE: &str = "meta.json";
const MODEL_LIFECYCLE_INDEX_METADATA_LIMIT: u64 = 4 * 1024 * 1024;
const HASH_FALLBACK_MODEL_ID: &str = "frankensearch-hash-fallback";

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
    pub posture: EmbeddingPosture,
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
    fn from_embedding_posture(
        posture: EmbeddingPosture,
        selected_registry_entry: Option<ModelRegistryEntryView>,
    ) -> Self {
        Self {
            fast_model_id: posture.fast_model_id.clone(),
            fast_dimension: posture.fast_dimension,
            quality_model_id: posture.quality_model_id.clone(),
            quality_dimension: posture.quality_dimension,
            semantic: posture.semantic,
            deterministic: posture.deterministic,
            source: posture.source.clone(),
            selected_registry_entry,
            posture,
        }
    }

    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "posture": self.posture.data_json(),
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLifecycleReport {
    pub generated_at: String,
    pub workspace_fingerprint: String,
    pub semantic_readiness: ModelLifecycleSemanticReadiness,
    pub models: Vec<ModelLifecycleModelRow>,
    pub indexes: Vec<ModelLifecycleIndexRow>,
    pub degraded: Vec<ModelLifecycleDegradation>,
}

impl ModelLifecycleReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": MODEL_LIFECYCLE_SCHEMA_V1,
            "generatedAt": self.generated_at,
            "workspaceFingerprint": self.workspace_fingerprint,
            "redactionStatus": MODEL_LIFECYCLE_REDACTION_STATUS,
            "semanticReadiness": self.semantic_readiness.data_json(),
            "models": self
                .models
                .iter()
                .map(ModelLifecycleModelRow::data_json)
                .collect::<Vec<_>>(),
            "indexes": self
                .indexes
                .iter()
                .map(ModelLifecycleIndexRow::data_json)
                .collect::<Vec<_>>(),
            "degraded": lifecycle_degraded_data_json(&self.degraded),
        })
    }

    #[must_use]
    pub fn semantic_surface_degradation(
        &self,
        surface: &'static str,
    ) -> Option<ModelLifecycleDegradation> {
        self.semantic_readiness
            .semantic_surface_degradation(surface)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLifecycleSemanticReadiness {
    pub state: &'static str,
    pub mode: &'static str,
    pub selected_model_id: Option<String>,
    pub selected_index_id: Option<String>,
    pub dimension_compatibility: ModelLifecycleDimensionCompatibility,
    pub degraded: Vec<ModelLifecycleDegradation>,
}

impl ModelLifecycleSemanticReadiness {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "mode": self.mode,
            "selectedModelId": self.selected_model_id,
            "selectedIndexId": self.selected_index_id,
            "dimensionCompatibility": self.dimension_compatibility.data_json(),
            "degraded": lifecycle_degraded_data_json(&self.degraded),
        })
    }

    #[must_use]
    pub fn semantic_surface_degradation(
        &self,
        surface: &'static str,
    ) -> Option<ModelLifecycleDegradation> {
        if self.state == "available" && self.mode == "semantic" {
            return None;
        }

        let primary = self.degraded.first();
        let repair = self
            .dimension_compatibility
            .repair
            .clone()
            .or_else(|| primary.and_then(|degradation| degradation.repair.clone()))
            .or_else(|| Some("ee index reembed --workspace .".to_string()));
        let reason = self
            .dimension_compatibility
            .mismatch_reason
            .clone()
            .or_else(|| primary.map(|degradation| degradation.message.clone()))
            .unwrap_or_else(|| {
                format!(
                    "semantic readiness state `{}` is not available in mode `{}`",
                    self.state, self.mode
                )
            });

        if self.dimension_compatibility.compatible == Some(false)
            || self.state == "dimension_mismatch"
        {
            return Some(ModelLifecycleDegradation {
                code: "embed_model_unavailable",
                severity: "high",
                message: format!(
                    "Model lifecycle reports {surface} semantic quality is dimension-incompatible: {reason}. Explicit memories remain available through lexical or anchored retrieval."
                ),
                repair,
            });
        }

        if let Some(index_degradation) = self.degraded.iter().find(|degradation| {
            matches!(
                degradation.code,
                "index_stale" | "index_missing" | "index_corrupt"
            )
        }) {
            return Some(ModelLifecycleDegradation {
                code: index_degradation.code,
                severity: index_degradation.severity,
                message: format!(
                    "Model lifecycle reports {surface} semantic quality is stale or unavailable: {} Results remain available through lexical or anchored retrieval when those indexes are usable.",
                    index_degradation.message
                ),
                repair: index_degradation.repair.clone().or(repair),
            });
        }

        Some(ModelLifecycleDegradation {
            code: "embed_model_unavailable",
            severity: primary.map_or("warning", |degradation| degradation.severity),
            message: format!(
                "Model lifecycle reports {surface} semantic quality is lexical-only: {reason}. Explicit memories remain available through lexical or anchored retrieval."
            ),
            repair,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLifecycleModelRow {
    pub model_id: String,
    pub provider: String,
    pub purpose: String,
    pub registry_status: String,
    pub state: &'static str,
    pub asset_provenance: ModelLifecycleAssetProvenance,
    pub embedding_metadata: Option<serde_json::Value>,
    pub dimension_compatibility: ModelLifecycleDimensionCompatibility,
    pub degraded: Vec<ModelLifecycleDegradation>,
}

impl ModelLifecycleModelRow {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "modelId": self.model_id,
            "provider": self.provider,
            "purpose": self.purpose,
            "registryStatus": self.registry_status,
            "state": self.state,
            "assetProvenance": self.asset_provenance.data_json(),
            "embeddingMetadata": self.embedding_metadata,
            "dimensionCompatibility": self.dimension_compatibility.data_json(),
            "degraded": lifecycle_degraded_data_json(&self.degraded),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLifecycleIndexRow {
    pub index_id: String,
    pub kind: &'static str,
    pub state: &'static str,
    pub stored_model_id: Option<String>,
    pub stored_model_revision: Option<String>,
    pub stored_model_hash: Option<String>,
    pub stored_dimension: Option<u32>,
    pub stored_distance_metric: Option<String>,
    pub stored_vector_dtype: Option<String>,
    pub last_rebuild_at: Option<String>,
    pub derived_from: Vec<String>,
    pub dimension_compatibility: ModelLifecycleDimensionCompatibility,
    pub degraded: Vec<ModelLifecycleDegradation>,
}

impl ModelLifecycleIndexRow {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "indexId": self.index_id,
            "kind": self.kind,
            "state": self.state,
            "storedModelId": self.stored_model_id,
            "storedModelRevision": self.stored_model_revision,
            "storedModelHash": self.stored_model_hash,
            "storedDimension": self.stored_dimension,
            "storedDistanceMetric": self.stored_distance_metric,
            "storedVectorDtype": self.stored_vector_dtype,
            "lastRebuildAt": self.last_rebuild_at,
            "derivedFrom": self.derived_from,
            "dimensionCompatibility": self.dimension_compatibility.data_json(),
            "degraded": lifecycle_degraded_data_json(&self.degraded),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLifecycleAssetProvenance {
    pub source_kind: &'static str,
    pub source_uri: Option<String>,
    pub registry_entry_id: Option<String>,
    pub model_revision: Option<String>,
    pub content_hash: Option<String>,
    pub asset_hash: Option<String>,
    pub manifest_hash: Option<String>,
    pub checked_at: Option<String>,
    pub provenance_complete: bool,
}

impl ModelLifecycleAssetProvenance {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "sourceKind": self.source_kind,
            "sourceUri": self.source_uri,
            "registryEntryId": self.registry_entry_id,
            "modelRevision": self.model_revision,
            "contentHash": self.content_hash,
            "assetHash": self.asset_hash,
            "manifestHash": self.manifest_hash,
            "checkedAt": self.checked_at,
            "provenanceComplete": self.provenance_complete,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLifecycleDimensionCompatibility {
    pub expected_dimension: Option<u32>,
    pub actual_dimension: Option<u32>,
    pub index_dimension: Option<u32>,
    pub distance_metric: Option<String>,
    pub vector_dtype: Option<String>,
    pub compatible: Option<bool>,
    pub rule: &'static str,
    pub mismatch_reason: Option<String>,
    pub repair: Option<String>,
}

impl ModelLifecycleDimensionCompatibility {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "expectedDimension": self.expected_dimension,
            "actualDimension": self.actual_dimension,
            "indexDimension": self.index_dimension,
            "distanceMetric": self.distance_metric,
            "vectorDtype": self.vector_dtype,
            "compatible": self.compatible,
            "rule": self.rule,
            "mismatchReason": self.mismatch_reason,
            "repair": self.repair,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLifecycleDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

impl ModelLifecycleDegradation {
    fn new(
        code: &'static str,
        severity: &'static str,
        message: impl Into<String>,
        repair: Option<&'static str>,
    ) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            repair: repair.map(str::to_owned),
        }
    }

    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ModelLifecycleIndexMetadata {
    stored_model_id: Option<String>,
    stored_model_revision: Option<String>,
    stored_model_hash: Option<String>,
    stored_dimension: Option<u32>,
    stored_distance_metric: Option<String>,
    stored_vector_dtype: Option<String>,
    derived_from: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelLifecycleAssetInspection {
    state: &'static str,
    content_hash: Option<String>,
    asset_hash: Option<String>,
    degraded: Vec<ModelLifecycleDegradation>,
    provenance_complete: bool,
}

/// Report shape returned by `ee model status`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelStatusReport {
    pub schema: &'static str,
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub active: ModelStatusActive,
    pub reranker: ModelStatusReranker,
    pub model_lifecycle: ModelLifecycleReport,
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
            "modelLifecycle": self.model_lifecycle.data_json(),
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

fn lifecycle_degraded_data_json(
    degradations: &[ModelLifecycleDegradation],
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for degradation in degradations {
        if seen.insert(degradation.code) {
            out.push(degradation.data_json());
        }
    }
    out
}

fn build_model_lifecycle_report(
    workspace_path: &Path,
    database_path: &Path,
    connection: &DbConnection,
    entries: &[StoredModelRegistryEntry],
    selected_embedding_entry: Option<&StoredModelRegistryEntry>,
) -> ModelLifecycleReport {
    let generated_at = Utc::now().to_rfc3339();
    let fingerprint = workspace_fingerprint(workspace_path);
    let index_status = get_index_status_with_connection(
        &IndexStatusOptions {
            workspace_path: workspace_path.to_path_buf(),
            database_path: Some(database_path.to_path_buf()),
            index_dir: None,
        },
        Some(connection),
    );
    let index_metadata = index_status
        .as_ref()
        .ok()
        .and_then(|status| read_model_lifecycle_index_metadata(&status.index_dir).ok())
        .unwrap_or_default();
    let mut index_degraded = index_status.as_ref().map_or_else(
        |error| index_status_error_degradation(error),
        |status| index_health_degradations(status.health, status.last_check_error.as_deref()),
    );
    if index_status.is_ok()
        && index_metadata == ModelLifecycleIndexMetadata::default()
        && selected_embedding_entry.is_some()
    {
        index_degraded.push(ModelLifecycleDegradation::new(
            "model_lifecycle_unknown",
            "warning",
            "Semantic index metadata did not record model dimension or hash evidence.",
            Some("ee index rebuild --workspace ."),
        ));
    }

    let selected_entry_id = selected_embedding_entry.map(|entry| entry.id.as_str());
    let mut models = entries
        .iter()
        .map(|entry| {
            model_lifecycle_row(
                entry,
                workspace_path,
                &generated_at,
                &index_metadata,
                selected_entry_id == Some(entry.id.as_str()),
            )
        })
        .collect::<Vec<_>>();
    if selected_embedding_entry.is_none() {
        models.push(hash_fallback_lifecycle_row(&generated_at));
    }

    let index_row = model_lifecycle_index_row(
        workspace_path,
        database_path,
        index_status.as_ref().ok(),
        &index_metadata,
        selected_embedding_entry,
        index_degraded,
    );
    let semantic_readiness =
        semantic_readiness_from_lifecycle(selected_embedding_entry, &models, &index_row);

    let mut degraded = semantic_readiness.degraded.clone();
    for model in &models {
        degraded.extend(model.degraded.clone());
    }
    degraded.extend(index_row.degraded.clone());
    if entries.is_empty() {
        degraded.push(ModelLifecycleDegradation::new(
            "model_registry_empty",
            "warning",
            "No available semantic model registry row was found.",
            Some("record or enable a local embedding model before semantic rebuild"),
        ));
    } else if selected_embedding_entry.is_none() {
        degraded.push(ModelLifecycleDegradation::new(
            "model_registry_no_available_entry",
            "warning",
            "Model registry has no available embedding model row.",
            Some("enable a local embedding model or repair the model registry"),
        ));
    }

    ModelLifecycleReport {
        generated_at,
        workspace_fingerprint: fingerprint,
        semantic_readiness,
        models,
        indexes: vec![index_row],
        degraded,
    }
}

fn model_lifecycle_row(
    entry: &StoredModelRegistryEntry,
    workspace_path: &Path,
    generated_at: &str,
    index_metadata: &ModelLifecycleIndexMetadata,
    selected: bool,
) -> ModelLifecycleModelRow {
    let parsed_metadata = entry
        .metadata_json
        .as_deref()
        .map(EmbeddingMetadataRecord::from_json)
        .transpose();
    let mut degraded = Vec::new();
    let metadata = match parsed_metadata {
        Ok(metadata) => metadata,
        Err(error) => {
            degraded.push(ModelLifecycleDegradation::new(
                "model_asset_corrupt",
                "high",
                format!(
                    "Embedding metadata for registry row {} is invalid: {error}",
                    entry.id
                ),
                Some("repair the model registry row so embedding metadata validates"),
            ));
            None
        }
    };
    let asset = inspect_model_lifecycle_asset(entry, workspace_path);
    degraded.extend(asset.degraded.clone());

    let dimension_compatibility =
        model_dimension_compatibility(entry, metadata.as_ref(), index_metadata, selected);
    if dimension_compatibility.compatible == Some(false) {
        degraded.push(ModelLifecycleDegradation::new(
            "model_dimension_mismatch",
            "high",
            dimension_compatibility
                .mismatch_reason
                .clone()
                .unwrap_or_else(|| "Model and index vector metadata do not match.".to_string()),
            Some("ee index reembed --workspace ."),
        ));
    }

    let state = model_lifecycle_state(entry, asset.state, &dimension_compatibility);
    let embedding_metadata = metadata
        .as_ref()
        .and_then(|metadata| serde_json::to_value(metadata).ok());

    ModelLifecycleModelRow {
        model_id: entry.id.clone(),
        provider: entry.provider.as_str().to_string(),
        purpose: entry.purpose.as_str().to_string(),
        registry_status: entry.status.as_str().to_string(),
        state,
        asset_provenance: ModelLifecycleAssetProvenance {
            source_kind: "model_registry",
            source_uri: entry
                .source_uri
                .as_deref()
                .map(|source| redact_lifecycle_source_uri(source, workspace_path)),
            registry_entry_id: Some(entry.id.clone()),
            model_revision: entry.version.clone(),
            content_hash: asset.content_hash,
            asset_hash: asset.asset_hash,
            manifest_hash: None,
            checked_at: entry
                .last_checked_at
                .clone()
                .or_else(|| Some(generated_at.to_string())),
            provenance_complete: asset.provenance_complete && metadata.is_some(),
        },
        embedding_metadata,
        dimension_compatibility,
        degraded,
    }
}

fn hash_fallback_lifecycle_row(generated_at: &str) -> ModelLifecycleModelRow {
    let degradation = ModelLifecycleDegradation::new(
        "lexical_fallback",
        "warning",
        "Hash fallback can keep lexical search honest but cannot prove semantic readiness.",
        None,
    );
    ModelLifecycleModelRow {
        model_id: HASH_FALLBACK_MODEL_ID.to_string(),
        provider: "hash".to_string(),
        purpose: "embedding".to_string(),
        registry_status: "unknown".to_string(),
        state: "lexical_fallback",
        asset_provenance: ModelLifecycleAssetProvenance {
            source_kind: "hash_fallback",
            source_uri: None,
            registry_entry_id: None,
            model_revision: None,
            content_hash: None,
            asset_hash: None,
            manifest_hash: None,
            checked_at: Some(generated_at.to_string()),
            provenance_complete: false,
        },
        embedding_metadata: None,
        dimension_compatibility: lexical_dimension_compatibility(
            Some("hash fallback is not a semantic model"),
            Some("enable a semantic embedding model before semantic indexing"),
        ),
        degraded: vec![degradation],
    }
}

fn inspect_model_lifecycle_asset(
    entry: &StoredModelRegistryEntry,
    workspace_path: &Path,
) -> ModelLifecycleAssetInspection {
    let mut degraded = Vec::new();
    let content_hash = match entry.content_hash.as_deref() {
        Some(hash) => match normalize_blake3_hash(hash) {
            Some(hash) => Some(hash),
            None => {
                degraded.push(ModelLifecycleDegradation::new(
                    "model_asset_corrupt",
                    "high",
                    format!(
                        "Registry row {} has an invalid content_hash shape; expected blake3:<64-hex>.",
                        entry.id
                    ),
                    Some("repair the model registry content_hash and re-check the model asset"),
                ));
                None
            }
        },
        None => None,
    };

    let Some(source_path) = entry
        .source_uri
        .as_deref()
        .and_then(|source| model_lifecycle_local_source_path(source, workspace_path))
    else {
        let corrupt = degraded
            .iter()
            .any(|degradation| degradation.code == "model_asset_corrupt");
        let provenance_complete = content_hash.is_some();
        return ModelLifecycleAssetInspection {
            state: if corrupt { "corrupt" } else { "available" },
            content_hash,
            asset_hash: None,
            degraded,
            provenance_complete,
        };
    };

    let metadata = match fs::symlink_metadata(&source_path) {
        Ok(metadata) => metadata,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            degraded.push(ModelLifecycleDegradation::new(
                "model_asset_missing",
                "high",
                format!("Model asset for registry row {} is missing.", entry.id),
                Some("fetch or rebuild the configured local model asset"),
            ));
            return ModelLifecycleAssetInspection {
                state: "missing",
                content_hash,
                asset_hash: None,
                degraded,
                provenance_complete: false,
            };
        }
        Err(error) => {
            degraded.push(ModelLifecycleDegradation::new(
                "model_asset_corrupt",
                "high",
                format!(
                    "Failed to inspect model asset for registry row {}: {error}",
                    entry.id
                ),
                Some("check permissions or repair the configured local model asset"),
            ));
            return ModelLifecycleAssetInspection {
                state: "corrupt",
                content_hash,
                asset_hash: None,
                degraded,
                provenance_complete: false,
            };
        }
    };

    if !metadata.file_type().is_file() {
        degraded.push(ModelLifecycleDegradation::new(
            "model_asset_corrupt",
            "high",
            format!(
                "Model asset for registry row {} is not a regular file.",
                entry.id
            ),
            Some("replace the configured model asset with a regular file"),
        ));
        return ModelLifecycleAssetInspection {
            state: "corrupt",
            content_hash,
            asset_hash: None,
            degraded,
            provenance_complete: false,
        };
    }

    let asset_hash = hash_model_asset(&source_path);
    match (&content_hash, &asset_hash) {
        (Some(expected), Ok(actual)) if expected != actual => {
            degraded.push(ModelLifecycleDegradation::new(
                "model_asset_corrupt",
                "high",
                format!(
                    "Model asset hash for registry row {} does not match content_hash.",
                    entry.id
                ),
                Some("replace the model asset or update the registry after a trusted rebuild"),
            ));
            ModelLifecycleAssetInspection {
                state: "corrupt",
                content_hash,
                asset_hash: Some(actual.clone()),
                degraded,
                provenance_complete: false,
            }
        }
        (_, Ok(actual)) => ModelLifecycleAssetInspection {
            state: if degraded
                .iter()
                .any(|degradation| degradation.code == "model_asset_corrupt")
            {
                "corrupt"
            } else {
                "available"
            },
            provenance_complete: content_hash.is_some(),
            content_hash,
            asset_hash: Some(actual.clone()),
            degraded,
        },
        (_, Err(error)) => {
            degraded.push(ModelLifecycleDegradation::new(
                "model_asset_corrupt",
                "high",
                format!(
                    "Failed to hash model asset for registry row {}: {error}",
                    entry.id
                ),
                Some("check permissions or repair the configured local model asset"),
            ));
            ModelLifecycleAssetInspection {
                state: "corrupt",
                content_hash,
                asset_hash: None,
                degraded,
                provenance_complete: false,
            }
        }
    }
}

fn model_lifecycle_state(
    entry: &StoredModelRegistryEntry,
    asset_state: &'static str,
    dimension_compatibility: &ModelLifecycleDimensionCompatibility,
) -> &'static str {
    if asset_state == "missing" || asset_state == "corrupt" {
        return asset_state;
    }
    if dimension_compatibility.compatible == Some(false) {
        return "dimension_mismatch";
    }
    match entry.status {
        ModelRegistryStatus::Available => "available",
        ModelRegistryStatus::Unavailable => "cold",
        ModelRegistryStatus::Disabled => "unsupported_feature",
    }
}

fn model_dimension_compatibility(
    entry: &StoredModelRegistryEntry,
    metadata: Option<&EmbeddingMetadataRecord>,
    index_metadata: &ModelLifecycleIndexMetadata,
    selected: bool,
) -> ModelLifecycleDimensionCompatibility {
    if entry.purpose != ModelPurpose::Embedding {
        return ModelLifecycleDimensionCompatibility {
            expected_dimension: None,
            actual_dimension: entry.dimension,
            index_dimension: index_metadata.stored_dimension,
            distance_metric: entry
                .distance_metric
                .map(|metric| metric.as_str().to_string()),
            vector_dtype: metadata.map(|metadata| metadata.vector_dtype.as_str().to_string()),
            compatible: None,
            rule: "unsupported_feature",
            mismatch_reason: Some("model purpose is not embedding".to_string()),
            repair: None,
        };
    }

    let actual_dimension = metadata.map_or(entry.dimension, |metadata| Some(metadata.dimension));
    let distance_metric = metadata
        .map(|metadata| metadata.distance_metric.as_str().to_string())
        .or_else(|| {
            entry
                .distance_metric
                .map(|metric| metric.as_str().to_string())
        });
    let vector_dtype = metadata.map(|metadata| metadata.vector_dtype.as_str().to_string());
    let index_dimension = index_metadata.stored_dimension;

    if !selected || entry.status != ModelRegistryStatus::Available {
        return ModelLifecycleDimensionCompatibility {
            expected_dimension: actual_dimension,
            actual_dimension,
            index_dimension,
            distance_metric,
            vector_dtype,
            compatible: None,
            rule: "unknown",
            mismatch_reason: if selected {
                Some("model is not available for semantic readiness".to_string())
            } else {
                None
            },
            repair: None,
        };
    }

    if let (Some(actual), Some(index)) = (actual_dimension, index_dimension)
        && actual != index
    {
        return ModelLifecycleDimensionCompatibility {
            expected_dimension: Some(actual),
            actual_dimension,
            index_dimension,
            distance_metric: distance_metric.clone(),
            vector_dtype: vector_dtype.clone(),
            compatible: Some(false),
            rule: "exact_dimension_metric_dtype",
            mismatch_reason: Some(format!(
                "selected embedding dimension {actual} does not match index dimension {index}"
            )),
            repair: Some("ee index reembed --workspace .".to_string()),
        };
    }

    if let (Some(model_metric), Some(index_metric)) = (
        distance_metric.as_deref(),
        index_metadata.stored_distance_metric.as_deref(),
    ) && model_metric != index_metric
    {
        return ModelLifecycleDimensionCompatibility {
            expected_dimension: actual_dimension,
            actual_dimension,
            index_dimension,
            distance_metric: distance_metric.clone(),
            vector_dtype: vector_dtype.clone(),
            compatible: Some(false),
            rule: "exact_dimension_metric_dtype",
            mismatch_reason: Some(format!(
                "selected embedding metric {model_metric} does not match index metric {index_metric}"
            )),
            repair: Some("ee index reembed --workspace .".to_string()),
        };
    }

    if let (Some(model_dtype), Some(index_dtype)) = (
        vector_dtype.as_deref(),
        index_metadata.stored_vector_dtype.as_deref(),
    ) && model_dtype != index_dtype
    {
        return ModelLifecycleDimensionCompatibility {
            expected_dimension: actual_dimension,
            actual_dimension,
            index_dimension,
            distance_metric: distance_metric.clone(),
            vector_dtype: vector_dtype.clone(),
            compatible: Some(false),
            rule: "exact_dimension_metric_dtype",
            mismatch_reason: Some(format!(
                "selected embedding vector dtype {model_dtype} does not match index dtype {index_dtype}"
            )),
            repair: Some("ee index reembed --workspace .".to_string()),
        };
    }

    ModelLifecycleDimensionCompatibility {
        expected_dimension: actual_dimension,
        actual_dimension,
        index_dimension,
        distance_metric,
        vector_dtype,
        compatible: if actual_dimension.is_some() && index_dimension.is_some() {
            Some(true)
        } else {
            None
        },
        rule: if actual_dimension.is_some() && index_dimension.is_some() {
            "exact_dimension_metric_dtype"
        } else {
            "unknown"
        },
        mismatch_reason: if index_dimension.is_none() {
            Some("semantic index metadata does not record a vector dimension".to_string())
        } else {
            None
        },
        repair: if index_dimension.is_none() {
            Some("ee index rebuild --workspace .".to_string())
        } else {
            None
        },
    }
}

fn model_lifecycle_index_row(
    workspace_path: &Path,
    database_path: &Path,
    index_status: Option<&crate::core::index::IndexStatusReport>,
    metadata: &ModelLifecycleIndexMetadata,
    selected_embedding_entry: Option<&StoredModelRegistryEntry>,
    mut degraded: Vec<ModelLifecycleDegradation>,
) -> ModelLifecycleIndexRow {
    let selected_dimension = selected_embedding_entry.and_then(|entry| entry.dimension);
    let dimension_compatibility = index_dimension_compatibility(selected_embedding_entry, metadata);
    if dimension_compatibility.compatible == Some(false) {
        degraded.push(ModelLifecycleDegradation::new(
            "model_dimension_mismatch",
            "high",
            dimension_compatibility
                .mismatch_reason
                .clone()
                .unwrap_or_else(|| {
                    "Index and selected model vector metadata do not match.".to_string()
                }),
            Some("ee index reembed --workspace ."),
        ));
    }

    let mut derived_from = metadata.derived_from.clone();
    if derived_from.is_empty() {
        derived_from.push(redact_lifecycle_path(database_path, workspace_path));
    }

    let state = if dimension_compatibility.compatible == Some(false) {
        "dimension_mismatch"
    } else {
        match index_status.map(|status| status.health) {
            Some(IndexHealth::Ready) if selected_embedding_entry.is_some() => "available",
            Some(IndexHealth::Ready) => "lexical_fallback",
            Some(IndexHealth::Stale) => "stale_index_model",
            Some(IndexHealth::Missing) => "missing",
            Some(IndexHealth::Corrupt) => "corrupt",
            None => "unknown",
        }
    };
    let kind = if metadata.stored_dimension.is_some() || selected_dimension.is_some() {
        "semantic"
    } else {
        "lexical"
    };

    ModelLifecycleIndexRow {
        index_id: MODEL_LIFECYCLE_INDEX_ID.to_string(),
        kind,
        state,
        stored_model_id: metadata.stored_model_id.clone(),
        stored_model_revision: metadata.stored_model_revision.clone(),
        stored_model_hash: metadata.stored_model_hash.clone(),
        stored_dimension: metadata.stored_dimension,
        stored_distance_metric: metadata.stored_distance_metric.clone(),
        stored_vector_dtype: metadata.stored_vector_dtype.clone(),
        last_rebuild_at: index_status.and_then(|status| status.last_rebuild_at.clone()),
        derived_from,
        dimension_compatibility,
        degraded,
    }
}

fn index_dimension_compatibility(
    selected_embedding_entry: Option<&StoredModelRegistryEntry>,
    metadata: &ModelLifecycleIndexMetadata,
) -> ModelLifecycleDimensionCompatibility {
    let Some(entry) = selected_embedding_entry else {
        return lexical_dimension_compatibility(
            Some("lexical index has no semantic vector dimension"),
            None,
        );
    };
    let actual_dimension = entry.dimension;
    let index_dimension = metadata.stored_dimension;
    let distance_metric = entry
        .distance_metric
        .map(|metric| metric.as_str().to_string())
        .or_else(|| metadata.stored_distance_metric.clone());
    let vector_dtype = metadata.stored_vector_dtype.clone();
    if let (Some(actual), Some(index)) = (actual_dimension, index_dimension)
        && actual != index
    {
        return ModelLifecycleDimensionCompatibility {
            expected_dimension: actual_dimension,
            actual_dimension,
            index_dimension,
            distance_metric,
            vector_dtype,
            compatible: Some(false),
            rule: "exact_dimension_metric_dtype",
            mismatch_reason: Some(format!(
                "selected embedding dimension {actual} does not match index dimension {index}"
            )),
            repair: Some("ee index reembed --workspace .".to_string()),
        };
    }
    ModelLifecycleDimensionCompatibility {
        expected_dimension: actual_dimension,
        actual_dimension,
        index_dimension,
        distance_metric,
        vector_dtype,
        compatible: if actual_dimension.is_some() && index_dimension.is_some() {
            Some(true)
        } else {
            None
        },
        rule: if actual_dimension.is_some() && index_dimension.is_some() {
            "exact_dimension_metric_dtype"
        } else {
            "unknown"
        },
        mismatch_reason: if index_dimension.is_none() {
            Some("semantic index metadata does not record a vector dimension".to_string())
        } else {
            None
        },
        repair: if index_dimension.is_none() {
            Some("ee index rebuild --workspace .".to_string())
        } else {
            None
        },
    }
}

fn semantic_readiness_from_lifecycle(
    selected_embedding_entry: Option<&StoredModelRegistryEntry>,
    models: &[ModelLifecycleModelRow],
    index_row: &ModelLifecycleIndexRow,
) -> ModelLifecycleSemanticReadiness {
    let Some(selected) = selected_embedding_entry else {
        let degraded = vec![ModelLifecycleDegradation::new(
            "lexical_fallback",
            "warning",
            "Semantic retrieval is unavailable; lexical retrieval remains available.",
            Some("install or enable a local Frankensearch embedding model"),
        )];
        return ModelLifecycleSemanticReadiness {
            state: "lexical_fallback",
            mode: "lexical_fallback",
            selected_model_id: None,
            selected_index_id: Some(MODEL_LIFECYCLE_INDEX_ID.to_string()),
            dimension_compatibility: lexical_dimension_compatibility(
                Some("no available semantic embedding model"),
                Some(
                    "install or enable a local Frankensearch embedding model, then rebuild the semantic index",
                ),
            ),
            degraded,
        };
    };

    let Some(selected_model) = models.iter().find(|model| model.model_id == selected.id) else {
        return ModelLifecycleSemanticReadiness {
            state: "unknown",
            mode: "unknown",
            selected_model_id: Some(selected.id.clone()),
            selected_index_id: Some(MODEL_LIFECYCLE_INDEX_ID.to_string()),
            dimension_compatibility: index_row.dimension_compatibility.clone(),
            degraded: vec![ModelLifecycleDegradation::new(
                "model_lifecycle_unknown",
                "high",
                "Selected embedding registry row was not present in lifecycle model rows.",
                Some("ee doctor --json"),
            )],
        };
    };
    if matches!(
        selected_model.state,
        "missing" | "corrupt" | "dimension_mismatch"
    ) {
        return ModelLifecycleSemanticReadiness {
            state: selected_model.state,
            mode: "blocked",
            selected_model_id: Some(selected.id.clone()),
            selected_index_id: Some(MODEL_LIFECYCLE_INDEX_ID.to_string()),
            dimension_compatibility: selected_model.dimension_compatibility.clone(),
            degraded: selected_model.degraded.clone(),
        };
    }
    if index_row.state == "dimension_mismatch" {
        return ModelLifecycleSemanticReadiness {
            state: "dimension_mismatch",
            mode: "blocked",
            selected_model_id: Some(selected.id.clone()),
            selected_index_id: Some(MODEL_LIFECYCLE_INDEX_ID.to_string()),
            dimension_compatibility: index_row.dimension_compatibility.clone(),
            degraded: index_row.degraded.clone(),
        };
    }
    if matches!(index_row.state, "missing" | "corrupt" | "stale_index_model") {
        let degraded = index_row.degraded.clone();
        return ModelLifecycleSemanticReadiness {
            state: "lexical_fallback",
            mode: "lexical_fallback",
            selected_model_id: Some(selected.id.clone()),
            selected_index_id: Some(MODEL_LIFECYCLE_INDEX_ID.to_string()),
            dimension_compatibility: index_row.dimension_compatibility.clone(),
            degraded,
        };
    }
    if selected_model.dimension_compatibility.compatible == Some(true)
        && index_row.dimension_compatibility.compatible == Some(true)
    {
        return ModelLifecycleSemanticReadiness {
            state: "available",
            mode: "semantic",
            selected_model_id: Some(selected.id.clone()),
            selected_index_id: Some(MODEL_LIFECYCLE_INDEX_ID.to_string()),
            dimension_compatibility: selected_model.dimension_compatibility.clone(),
            degraded: Vec::new(),
        };
    }

    ModelLifecycleSemanticReadiness {
        state: "unknown",
        mode: "unknown",
        selected_model_id: Some(selected.id.clone()),
        selected_index_id: Some(MODEL_LIFECYCLE_INDEX_ID.to_string()),
        dimension_compatibility: selected_model.dimension_compatibility.clone(),
        degraded: vec![ModelLifecycleDegradation::new(
            "model_lifecycle_unknown",
            "warning",
            "Semantic model and index compatibility evidence is incomplete.",
            Some("ee index rebuild --workspace ."),
        )],
    }
}

fn lexical_dimension_compatibility(
    mismatch_reason: Option<&'static str>,
    repair: Option<&'static str>,
) -> ModelLifecycleDimensionCompatibility {
    ModelLifecycleDimensionCompatibility {
        expected_dimension: None,
        actual_dimension: None,
        index_dimension: None,
        distance_metric: None,
        vector_dtype: None,
        compatible: None,
        rule: "lexical_no_dimension",
        mismatch_reason: mismatch_reason.map(str::to_string),
        repair: repair.map(str::to_string),
    }
}

fn index_health_degradations(
    health: IndexHealth,
    last_check_error: Option<&str>,
) -> Vec<ModelLifecycleDegradation> {
    match health {
        IndexHealth::Ready => Vec::new(),
        IndexHealth::Stale => vec![ModelLifecycleDegradation::new(
            "index_stale",
            "high",
            "Search index is stale relative to database generation.",
            Some("ee index rebuild --workspace ."),
        )],
        IndexHealth::Missing => vec![ModelLifecycleDegradation::new(
            "index_missing",
            "medium",
            "Search index is missing.",
            Some("ee index rebuild --workspace ."),
        )],
        IndexHealth::Corrupt => vec![ModelLifecycleDegradation::new(
            "index_corrupt",
            "high",
            last_check_error.unwrap_or("Search index metadata is corrupt."),
            Some("ee index rebuild --workspace ."),
        )],
    }
}

fn index_status_error_degradation(
    error: &crate::core::index::IndexStatusError,
) -> Vec<ModelLifecycleDegradation> {
    vec![ModelLifecycleDegradation::new(
        "search_index_degraded",
        "high",
        format!("Failed to inspect search index status: {error}"),
        Some("ee doctor --json"),
    )]
}

fn read_model_lifecycle_index_metadata(
    index_dir: &Path,
) -> Result<ModelLifecycleIndexMetadata, String> {
    let meta_path = index_dir.join(MODEL_LIFECYCLE_INDEX_METADATA_FILE);
    let Some(content) = read_model_lifecycle_index_metadata_contents(&meta_path)? else {
        return Ok(ModelLifecycleIndexMetadata::default());
    };
    let parsed: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
        format!(
            "failed to parse model lifecycle index metadata '{}': {error}",
            meta_path.display()
        )
    })?;
    let object = parsed.as_object().ok_or_else(|| {
        format!(
            "model lifecycle index metadata '{}' must be a JSON object",
            meta_path.display()
        )
    })?;
    let stored_model_hash = first_string(
        &parsed,
        &[
            "storedModelHash",
            "stored_model_hash",
            "modelHash",
            "model_hash",
            "contentHash",
            "content_hash",
        ],
    )
    .and_then(|hash| normalize_blake3_hash(&hash));
    let derived_from = object
        .get("derivedFrom")
        .or_else(|| object.get("derived_from"))
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(redact_lifecycle_metadata_path)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(ModelLifecycleIndexMetadata {
        stored_model_id: first_string(
            &parsed,
            &[
                "storedModelId",
                "stored_model_id",
                "modelId",
                "model_id",
                "embeddingModelId",
                "embedding_model_id",
            ],
        ),
        stored_model_revision: first_string(
            &parsed,
            &[
                "storedModelRevision",
                "stored_model_revision",
                "modelRevision",
                "model_revision",
            ],
        ),
        stored_model_hash,
        stored_dimension: first_u32(
            &parsed,
            &[
                "storedDimension",
                "stored_dimension",
                "dimension",
                "embeddingDimension",
                "embedding_dimension",
            ],
        ),
        stored_distance_metric: first_string(
            &parsed,
            &[
                "storedDistanceMetric",
                "stored_distance_metric",
                "distanceMetric",
                "distance_metric",
            ],
        ),
        stored_vector_dtype: first_string(
            &parsed,
            &[
                "storedVectorDtype",
                "stored_vector_dtype",
                "vectorDtype",
                "vector_dtype",
            ],
        ),
        derived_from,
    })
}

fn read_model_lifecycle_index_metadata_contents(
    meta_path: &Path,
) -> Result<Option<String>, String> {
    let metadata = match fs::symlink_metadata(meta_path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => {
            return Err(format!(
                "index metadata '{}' is not a regular file",
                meta_path.display()
            ));
        }
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect index metadata '{}': {error}",
                meta_path.display()
            ));
        }
    };
    if metadata.len() > MODEL_LIFECYCLE_INDEX_METADATA_LIMIT {
        return Err(format!(
            "index metadata '{}' exceeds the {MODEL_LIFECYCLE_INDEX_METADATA_LIMIT} byte cap",
            meta_path.display()
        ));
    }
    let file = fs::File::open(meta_path).map_err(|error| {
        format!(
            "failed to read index metadata '{}': {error}",
            meta_path.display()
        )
    })?;
    let mut bytes = Vec::new();
    file.take(MODEL_LIFECYCLE_INDEX_METADATA_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "failed to read index metadata '{}': {error}",
                meta_path.display()
            )
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MODEL_LIFECYCLE_INDEX_METADATA_LIMIT {
        return Err(format!(
            "index metadata '{}' exceeds the {MODEL_LIFECYCLE_INDEX_METADATA_LIMIT} byte cap during read",
            meta_path.display()
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        format!(
            "index metadata '{}' is not valid UTF-8: {error}",
            meta_path.display()
        )
    })
}

fn first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|value| value.as_str()))
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}

fn first_u32(value: &serde_json::Value, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|value| value.as_u64()))
        .and_then(|value| u32::try_from(value).ok())
}

fn model_lifecycle_local_source_path(source: &str, workspace_path: &Path) -> Option<PathBuf> {
    if source.contains("://") || source.starts_with("urn:") || source.starts_with("model:") {
        return None;
    }
    let path = Path::new(source);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_path.join(path)
    })
}

fn redact_lifecycle_source_uri(source: &str, workspace_path: &Path) -> String {
    if source.contains("://") || source.starts_with("urn:") || source.starts_with("model:") {
        return short_hashed_path(source);
    }
    let path = Path::new(source);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_path.join(path)
    };
    redact_lifecycle_path(&absolute, workspace_path)
}

fn redact_lifecycle_metadata_path(value: &str) -> String {
    if value.starts_with('/') || value.contains("://") {
        short_hashed_path(value)
    } else {
        value.trim_start_matches("./").to_string()
    }
}

fn redact_lifecycle_path(path: &Path, workspace_path: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(workspace_path) {
        let rendered = relative.to_string_lossy();
        let trimmed = rendered.trim_start_matches("./");
        if trimmed.is_empty() {
            ".".to_string()
        } else {
            trimmed.to_string()
        }
    } else {
        short_hashed_path(&path.to_string_lossy())
    }
}

fn short_hashed_path(value: &str) -> String {
    let digest = blake3::hash(value.as_bytes()).to_hex().to_string();
    format!("hashed:{}", &digest[..12])
}

fn normalize_blake3_hash(value: &str) -> Option<String> {
    let hex = value.strip_prefix("blake3:")?;
    if hex.len() == 64 && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(format!("blake3:{}", hex.to_ascii_lowercase()))
    } else {
        None
    }
}

fn hash_model_asset(path: &Path) -> Result<String, std::io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
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

/// Build the reusable model-lifecycle report for non-`ee model status`
/// surfaces that already hold a DB connection.
pub fn build_model_lifecycle_report_for_workspace(
    workspace_path: &Path,
    database_path: Option<&Path>,
    connection: Option<&DbConnection>,
) -> Result<ModelLifecycleReport, DomainError> {
    let workspace_path = resolve_workspace_path(workspace_path)?;
    let database_path = if connection.is_some() {
        let path = database_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_path.join(".ee").join(DEFAULT_DB_FILE));
        ensure_no_model_database_symlink_components(&path)?;
        path
    } else {
        resolved_database_path(&workspace_path, database_path)?
    };

    let owned_connection;
    let connection = match connection {
        Some(connection) => connection,
        None => {
            owned_connection = DbConnection::open_file(&database_path).map_err(|error| {
                db_error_to_domain(
                    error,
                    "Failed to open database",
                    Some("ee init --workspace .".to_string()),
                )
            })?;
            &owned_connection
        }
    };
    let workspace_id = resolve_workspace_id(connection, &workspace_path)?;
    let entries = connection
        .list_model_registry_entries(&workspace_id)
        .map_err(|error| {
            db_error_to_domain(
                error,
                "Failed to list model registry entries",
                Some("ee doctor".to_string()),
            )
        })?;
    let selected_embedding_entry = entries
        .iter()
        .find(|entry| entry_is_available_embedding(entry));

    Ok(build_model_lifecycle_report(
        &workspace_path,
        &database_path,
        connection,
        &entries,
        selected_embedding_entry,
    ))
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

    let selected_embedding_entry = entries
        .iter()
        .find(|entry| entry_is_available_embedding(entry))
        .cloned();

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

    let embedding_posture = current_embedding_posture(
        &connection,
        &workspace_id,
        &workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR),
    )
    .map_err(|error| {
        db_error_to_domain(
            error,
            "Failed to build embedding posture",
            Some("ee index reembed --workspace .".to_string()),
        )
    })?;
    let selected_registry_entry = embedding_posture
        .selected_registry_model
        .as_ref()
        .and_then(|selected| {
            entries
                .iter()
                .find(|entry| entry.id == selected.id)
                .cloned()
        })
        .map(ModelRegistryEntryView::from_stored);
    let active =
        ModelStatusActive::from_embedding_posture(embedding_posture, selected_registry_entry);

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
    let model_lifecycle = build_model_lifecycle_report(
        &workspace_path,
        &database_path,
        &connection,
        &entries,
        selected_embedding_entry.as_ref(),
    );

    Ok(ModelStatusReport {
        schema: MODEL_STATUS_SCHEMA_V2,
        workspace_path,
        database_path,
        active,
        reranker,
        model_lifecycle,
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

// ---------------------------------------------------------------------------
// Bundled default embedding model registration (bd-1et0v.3).
//
// ADR 0080 selects `minishlab/potion-multilingual-128M` (256-dimension,
// Apache-2.0, deterministic model2vec static embedder) as the bundled,
// on-by-default local embedding model. This block is the single source of
// truth for that model's registry identity, plus an idempotent registrar so a
// fresh workspace has `registered_model_count >= 1` for the bundled model with
// NO operator action — the discoverability fix the 12 West analyst asked for.
//
// Honesty (epic HARD CONSTRAINT — no silent fallback): the declared entry is
// registered as `Unavailable` until the artifact is actually present. The
// download→`Available` flip is performed by the index-build path
// (`ensure_active_embedding_registry_record`, src/core/index.rs), which inserts
// an `Available` entry once the active fast embedder reports `is_semantic()`.
// Reconciling an existing `Unavailable` declared entry up to `Available` on a
// later download needs a registry status-update/upsert (none exists yet); that
// reconcile + the `ee model fetch` embedding pre-download trigger are the
// remaining bd-1et0v.3 wiring (tracked, this is the code-first foundation).
// ---------------------------------------------------------------------------

/// Registry id of the bundled default embedding model (ADR 0080).
pub const BUNDLED_EMBEDDING_MODEL_ID: &str = "potion-multilingual-128M";

/// Output dimension of the bundled default embedding model (ADR 0080).
pub const BUNDLED_EMBEDDING_DIMENSION: u32 = 256;

/// Pinned revision of the bundled model2vec artifact (ADR 0080). Mirrors
/// `src/core/index.rs::DEFAULT_MODEL2VEC_REVISION`; a `bundled_revision_matches`
/// test guards against drift. (Follow-up: unify into one exported constant.)
pub const BUNDLED_EMBEDDING_MODEL_REVISION: &str = "a28f4eebecd4dc585034f605e52d414878a0417c";

/// Canonical, redaction-safe embedding-metadata record for the bundled model.
///
/// Deterministic by construction: the same ADR-pinned identity always yields a
/// byte-identical record, so callers (registrar, status report, golden tests)
/// share one source of truth. The record is schema-valid by
/// [`EmbeddingMetadataRecord::validate`].
#[must_use]
pub fn bundled_embedding_metadata_record() -> EmbeddingMetadataRecord {
    let mut metadata =
        EmbeddingMetadataRecord::new(BUNDLED_EMBEDDING_DIMENSION, ModelDistanceMetric::Cosine);
    metadata.pooling = EmbeddingPooling::ModelDefault;
    metadata.tokenizer = Some("tokenizer.json".to_owned());
    metadata.model_revision = Some(BUNDLED_EMBEDDING_MODEL_REVISION.to_owned());
    // model2vec is a static distilled embedder: same input → same output.
    metadata.deterministic = true;
    metadata
}

/// Build the registry-insert input for the bundled embedding model at `status`.
///
/// `status` carries the honest availability: [`ModelRegistryStatus::Available`]
/// only when the artifact is actually loadable, otherwise
/// [`ModelRegistryStatus::Unavailable`] (declared-but-not-downloaded). The
/// `dimension == metadata.dimension` registry invariant is upheld by
/// construction.
#[must_use]
pub fn bundled_embedding_registry_input(
    workspace_id: &str,
    status: ModelRegistryStatus,
) -> CreateEmbeddingMetadataInput {
    let metadata = bundled_embedding_metadata_record();
    CreateEmbeddingMetadataInput {
        workspace_id: workspace_id.to_owned(),
        provider: ModelProvider::Model2Vec,
        model_name: BUNDLED_EMBEDDING_MODEL_ID.to_owned(),
        dimension: BUNDLED_EMBEDDING_DIMENSION,
        distance_metric: ModelDistanceMetric::Cosine,
        status,
        version: Some(BUNDLED_EMBEDDING_MODEL_REVISION.to_owned()),
        source_uri: Some(format!(
            "frankensearch://{provider}/{model}",
            provider = ModelProvider::Model2Vec.as_str(),
            model = BUNDLED_EMBEDDING_MODEL_ID
        )),
        content_hash: None,
        metadata,
        last_checked_at: None,
    }
}

/// Idempotently register the bundled default embedding model in the registry so
/// a fresh workspace reports `registered_model_count >= 1` out of the box.
///
/// Insert-if-absent on the `(Model2Vec, potion-multilingual-128M, Embedding)`
/// key, so it never duplicates an entry the index-build path already created and
/// never downgrades an existing `Available` entry. Returns `true` when it
/// inserted a new declared entry, `false` when one already existed.
///
/// The declared entry is registered `Unavailable` (honest: the artifact may not
/// be downloaded yet); the index-build path flips a fresh registry to
/// `Available` once the model is actually loaded.
///
/// # Errors
///
/// Returns [`DbError`] if the registry lookup or insert fails.
pub fn ensure_bundled_embedding_model_registered(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<bool, DbError> {
    if db
        .find_model_registry_entry(
            workspace_id,
            ModelProvider::Model2Vec,
            BUNDLED_EMBEDDING_MODEL_ID,
            ModelPurpose::Embedding,
        )?
        .is_some()
    {
        return Ok(false);
    }

    let input = bundled_embedding_registry_input(workspace_id, ModelRegistryStatus::Unavailable);
    db.insert_embedding_metadata_record(&generate_model_registry_id(), &input)?;
    Ok(true)
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
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
    use crate::core::index::{EmbeddingPosture, EmbeddingVectorCoverage, ReembedEmbeddingSummary};
    use crate::db::{CreateModelRegistryInput, CreateWorkspaceInput};
    use crate::models::model_registry::{
        ModelDistanceMetric, ModelProvider, ModelPurpose, ModelRegistryStatus,
    };
    use crate::models::{
        EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH, EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
        EMBEDDING_POSTURE_SCHEMA_V1,
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

    #[test]
    fn bundled_descriptor_is_canonical_and_valid() -> TestResult {
        let metadata = bundled_embedding_metadata_record();
        ensure(
            metadata.dimension == BUNDLED_EMBEDDING_DIMENSION && metadata.dimension == 256,
            "bundled dimension is the ADR-pinned 256",
        )?;
        ensure(
            metadata.distance_metric == ModelDistanceMetric::Cosine,
            "bundled distance metric is cosine",
        )?;
        ensure(
            metadata.pooling == EmbeddingPooling::ModelDefault,
            "bundled pooling is model_default",
        )?;
        ensure(metadata.deterministic, "model2vec is deterministic")?;
        ensure(
            metadata.model_revision.as_deref() == Some(BUNDLED_EMBEDDING_MODEL_REVISION),
            "bundled record carries the pinned revision",
        )?;
        // The canonical record must be durably storable.
        metadata
            .validate()
            .map_err(|error| format!("bundled metadata must be schema-valid: {error}"))
    }

    #[test]
    fn bundled_revision_is_the_adr_pinned_artifact() -> TestResult {
        // Drift guard: ADR 0080 pins this revision; it mirrors
        // index.rs::DEFAULT_MODEL2VEC_REVISION. A bump must touch both.
        ensure(
            BUNDLED_EMBEDDING_MODEL_REVISION == "a28f4eebecd4dc585034f605e52d414878a0417c",
            "bundled revision matches ADR 0080",
        )?;
        ensure(
            BUNDLED_EMBEDDING_MODEL_ID == "potion-multilingual-128M",
            "bundled model id matches ADR 0080",
        )
    }

    #[test]
    fn bundled_registry_input_upholds_invariants() -> TestResult {
        let input = bundled_embedding_registry_input("wsp_x", ModelRegistryStatus::Unavailable);
        ensure(
            input.dimension == input.metadata.dimension,
            "registry dimension must equal metadata dimension (db invariant)",
        )?;
        ensure(
            input.provider == ModelProvider::Model2Vec,
            "bundled provider is model2vec",
        )?;
        ensure(
            input.model_name == BUNDLED_EMBEDDING_MODEL_ID,
            "bundled model name is the ADR id",
        )?;
        ensure(
            input.status == ModelRegistryStatus::Unavailable,
            "status passes through verbatim",
        )?;
        ensure(
            input
                .source_uri
                .as_deref()
                .is_some_and(|uri| uri.contains(BUNDLED_EMBEDDING_MODEL_ID)),
            "source uri names the bundled model",
        )?;
        // A passed Available status is honored (download path uses it).
        let available = bundled_embedding_registry_input("wsp_x", ModelRegistryStatus::Available);
        ensure(
            available.status == ModelRegistryStatus::Available,
            "available status passes through too",
        )
    }

    #[test]
    fn ensure_bundled_registers_once_and_is_idempotent() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("open db: {error}"))?;

        // Fresh workspace: registers exactly one declared bundled entry.
        let inserted = ensure_bundled_embedding_model_registered(&connection, &workspace_id)
            .map_err(|error| format!("first ensure: {error}"))?;
        ensure(inserted, "first call registers the bundled model")?;

        let records = connection
            .list_embedding_metadata_records(&workspace_id)
            .map_err(|error| format!("list records: {error}"))?;
        ensure(
            records.len() == 1,
            format!(
                "registered_model_count >= 1 out of the box, got {}",
                records.len()
            ),
        )?;

        let entry = connection
            .find_model_registry_entry(
                &workspace_id,
                ModelProvider::Model2Vec,
                BUNDLED_EMBEDDING_MODEL_ID,
                ModelPurpose::Embedding,
            )
            .map_err(|error| format!("find: {error}"))?
            .ok_or("bundled entry must exist after ensure")?;
        ensure(
            entry.status == ModelRegistryStatus::Unavailable,
            "declared entry is honestly Unavailable until downloaded",
        )?;

        // Idempotent: a second call inserts nothing and does not duplicate.
        let again = ensure_bundled_embedding_model_registered(&connection, &workspace_id)
            .map_err(|error| format!("second ensure: {error}"))?;
        ensure(!again, "second call is a no-op")?;
        let after = connection
            .list_embedding_metadata_records(&workspace_id)
            .map_err(|error| format!("list after: {error}"))?;
        ensure(after.len() == 1, "no duplicate bundled entry")
    }

    #[test]
    fn ensure_bundled_does_not_downgrade_an_available_entry() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;

        // Simulate the index-build path having already registered the model
        // Available (the real artifact is loaded).
        insert_registry_entry(
            &database_path,
            &workspace_id,
            "mdl_preexisting_available",
            ModelProvider::Model2Vec,
            BUNDLED_EMBEDDING_MODEL_ID,
            ModelRegistryStatus::Available,
        )?;

        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("open db: {error}"))?;
        let inserted = ensure_bundled_embedding_model_registered(&connection, &workspace_id)
            .map_err(|error| format!("ensure: {error}"))?;
        ensure(!inserted, "ensure is a no-op when an entry already exists")?;

        let entry = connection
            .find_model_registry_entry(
                &workspace_id,
                ModelProvider::Model2Vec,
                BUNDLED_EMBEDDING_MODEL_ID,
                ModelPurpose::Embedding,
            )
            .map_err(|error| format!("find: {error}"))?
            .ok_or("entry must still exist")?;
        ensure(
            entry.status == ModelRegistryStatus::Available,
            "ensure must never downgrade an existing Available entry",
        )
    }

    fn insert_embedding_metadata_entry(
        database_path: &Path,
        workspace_id: &str,
        id: &str,
        provider: ModelProvider,
        name: &str,
        status: ModelRegistryStatus,
    ) -> TestResult {
        insert_embedding_metadata_entry_with_dimension(
            database_path,
            workspace_id,
            id,
            provider,
            name,
            status,
            384,
        )
    }

    fn insert_embedding_metadata_entry_with_dimension(
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
        let mut metadata = EmbeddingMetadataRecord::new(dimension, ModelDistanceMetric::Cosine);
        metadata.deterministic = matches!(provider, ModelProvider::Hash | ModelProvider::Model2Vec);
        connection
            .insert_embedding_metadata_record(
                id,
                &CreateEmbeddingMetadataInput {
                    workspace_id: workspace_id.to_string(),
                    provider,
                    model_name: name.to_string(),
                    dimension,
                    distance_metric: ModelDistanceMetric::Cosine,
                    status,
                    version: Some("v1".to_string()),
                    source_uri: None,
                    content_hash: None,
                    metadata,
                    last_checked_at: None,
                },
            )
            .map_err(|error| format!("insert embedding metadata entry: {error}"))
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

    fn write_index_metadata(workspace_path: &Path, source_generation: u64) -> TestResult {
        let index_dir = workspace_path.join(".ee").join("index");
        fs::create_dir_all(&index_dir).map_err(|error| format!("create index dir: {error}"))?;
        fs::write(
            index_dir.join("meta.json"),
            serde_json::json!({
                "schema": "ee.index_metadata.v1",
                "sourceGeneration": source_generation,
                "lastRebuildAt": "2026-01-01T00:00:00Z",
                "storedDimension": 128,
                "storedDistanceMetric": "cosine",
                "storedVectorDtype": "f32"
            })
            .to_string(),
        )
        .map_err(|error| format!("write index metadata: {error}"))
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

    fn fixture_embedding_posture(
        semantic: bool,
        source: &str,
        fast_model_id: &str,
        fast_dimension: usize,
    ) -> EmbeddingPosture {
        EmbeddingPosture {
            schema: EMBEDDING_POSTURE_SCHEMA_V1,
            mode: if semantic {
                EMBEDDING_POSTURE_MODE_NEURAL_LOCAL
            } else {
                EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH
            },
            semantic,
            source: source.to_owned(),
            fast_model_id: fast_model_id.to_owned(),
            fast_dimension,
            quality_model_id: None,
            quality_dimension: None,
            deterministic: true,
            registered_model_count: usize::from(semantic),
            available_model_count: usize::from(semantic),
            selected_registry_model: None,
            vector_coverage: EmbeddingVectorCoverage::new(0, 0),
        }
    }

    fn fixture_model_lifecycle_report(workspace_path: &Path) -> ModelLifecycleReport {
        let generated_at = "2026-06-14T00:00:00Z".to_string();
        ModelLifecycleReport {
            generated_at: generated_at.clone(),
            workspace_fingerprint: workspace_fingerprint(workspace_path),
            semantic_readiness: ModelLifecycleSemanticReadiness {
                state: "lexical_fallback",
                mode: "lexical_fallback",
                selected_model_id: None,
                selected_index_id: Some(MODEL_LIFECYCLE_INDEX_ID.to_string()),
                dimension_compatibility: lexical_dimension_compatibility(
                    Some("unit fixture has no available semantic embedding model"),
                    None,
                ),
                degraded: Vec::new(),
            },
            models: vec![hash_fallback_lifecycle_row(&generated_at)],
            indexes: vec![ModelLifecycleIndexRow {
                index_id: MODEL_LIFECYCLE_INDEX_ID.to_string(),
                kind: "lexical",
                state: "lexical_fallback",
                stored_model_id: None,
                stored_model_revision: None,
                stored_model_hash: None,
                stored_dimension: None,
                stored_distance_metric: None,
                stored_vector_dtype: None,
                last_rebuild_at: None,
                derived_from: vec![".ee/ee.db".to_string()],
                dimension_compatibility: lexical_dimension_compatibility(
                    Some("lexical index has no semantic vector dimension"),
                    None,
                ),
                degraded: Vec::new(),
            }],
            degraded: Vec::new(),
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
    fn lifecycle_surface_degradation_reports_lexical_only_readiness() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, _workspace_id) = fresh_db_for_workspace(&workspace_path)?;

        let report =
            build_model_lifecycle_report_for_workspace(&workspace_path, Some(&database_path), None)
                .map_err(|error| format!("lifecycle report: {error:?}"))?;
        let degradation = report
            .semantic_surface_degradation("search")
            .ok_or("missing search lifecycle degradation")?;

        ensure(
            degradation.code == "embed_model_unavailable",
            "lexical-only readiness should use the established embedder code",
        )?;
        ensure(
            degradation.message.contains("lexical-only"),
            "message should tell agents the quality mode is lexical-only",
        )
    }

    #[test]
    fn lifecycle_surface_degradation_reports_dimension_incompatible_readiness() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        insert_registry_entry_with_dimension(
            &database_path,
            &workspace_id,
            "mdl_01HQ3K5Z000000000000000099",
            ModelProvider::Hash,
            "hash-384",
            ModelRegistryStatus::Available,
            384,
        )?;
        write_index_metadata(&workspace_path, 0)?;

        let report =
            build_model_lifecycle_report_for_workspace(&workspace_path, Some(&database_path), None)
                .map_err(|error| format!("lifecycle report: {error:?}"))?;
        let degradation = report
            .semantic_surface_degradation("search")
            .ok_or("missing search lifecycle degradation")?;

        ensure(
            degradation.code == "embed_model_unavailable",
            "dimension mismatch should reuse semantic-unavailable code",
        )?;
        ensure(
            degradation.severity == "high",
            "dimension mismatch severity",
        )?;
        ensure(
            degradation.message.contains("dimension-incompatible"),
            "message should distinguish dimension-incompatible quality",
        )?;
        ensure(
            degradation.repair.as_deref() == Some("ee index reembed --workspace ."),
            "dimension mismatch repair",
        )
    }

    #[test]
    fn status_picks_first_available_registry_entry() -> TestResult {
        let (_temp, workspace_path) = make_workspace()?;
        let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
        insert_embedding_metadata_entry(
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
        insert_embedding_metadata_entry_with_dimension(
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
            active: ModelStatusActive::from_embedding_posture(
                fixture_embedding_posture(false, "unit_fixture", "hash:deterministic", 384),
                None,
            ),
            reranker: empty_reranker_status(),
            model_lifecycle: fixture_model_lifecycle_report(&workspace_path),
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
            active: ModelStatusActive::from_embedding_posture(
                fixture_embedding_posture(true, "registry_observed", "registry:private-model", 384),
                Some(entry.clone()),
            ),
            reranker: empty_reranker_status(),
            model_lifecycle: fixture_model_lifecycle_report(&workspace_path),
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
    fn model_status_and_reembed_share_byte_identical_embedding_posture() -> TestResult {
        let posture =
            fixture_embedding_posture(true, "registry_observed", "potion-multilingual-128M", 256)
                .with_vector_coverage(EmbeddingVectorCoverage::new(7, 11));
        let active = ModelStatusActive::from_embedding_posture(posture.clone(), None);
        let reembed = ReembedEmbeddingSummary::from_posture(posture);

        ensure(
            active.data_json()["posture"] == reembed.data_json()["posture"],
            "model status active and index reembed should emit byte-identical posture JSON",
        )?;
        ensure(
            active.data_json()["posture"]["schema"] == EMBEDDING_POSTURE_SCHEMA_V1,
            "shared posture schema should be pinned",
        )?;
        ensure(
            active.data_json()["posture"]["vector_coverage"]
                == serde_json::json!({"embedded": 7, "total": 11}),
            "shared posture should carry vector coverage",
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
