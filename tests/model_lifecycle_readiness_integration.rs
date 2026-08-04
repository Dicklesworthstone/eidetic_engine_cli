//! Integration coverage for model-lifecycle readiness on search/recall surfaces.

use std::fs;
use std::path::{Path, PathBuf};

use ee::core::index::{INDEX_METADATA_SCHEMA_V2, expected_index_corpus_revision};
use ee::core::model::build_model_lifecycle_report_for_workspace;
use ee::core::recall::{RecallQuery, run_recall};
use ee::db::{
    CreateMemoryInput, CreateModelRegistryInput, CreateWorkspaceInput, DbConnection,
    EVIDENCE_SECURITY_POLICY_EPOCH,
};
use ee::models::model_registry::{
    EmbeddingMetadataRecord, ModelDistanceMetric, ModelProvider, ModelPurpose, ModelRegistryStatus,
};
use serde_json::Value;

type TestResult = Result<(), String>;

const OFFLINE_READY_GOLDEN_REL: &str =
    "tests/fixtures/golden/model_lifecycle/offline_local_readiness.json.golden";
const OFFLINE_MODEL_ID: &str = "mdl_01HQ3K5Z000000000000000060";
const OFFLINE_MODEL_REVISION: &str = "hash-fixture-v1";
const OFFLINE_MODEL_SOURCE_URI: &str = "models/hash-embedder-fixture.json";
const OFFLINE_MODEL_CHECKED_AT: &str = "2026-06-15T00:00:00Z";
const CANONICAL_GENERATED_AT: &str = "2026-06-15T00:00:01Z";
const CANONICAL_WORKSPACE_FINGERPRINT: &str = "0123456789ab";

struct WorkspaceFixture {
    _temp: tempfile::TempDir,
    workspace_path: PathBuf,
    database_path: PathBuf,
    connection: DbConnection,
    workspace_id: String,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn fresh_workspace() -> Result<WorkspaceFixture, String> {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?;
    fs::create_dir_all(workspace_path.join(".ee"))
        .map_err(|error| format!("create .ee: {error}"))?;
    let database_path = workspace_path.join(".ee").join("ee.db");
    let connection =
        DbConnection::open_file(&database_path).map_err(|error| format!("open db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    let workspace_id = format!("wsp_{:026}", 42);
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace_path.to_string_lossy().into_owned(),
                name: Some("model-lifecycle-readiness".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;

    Ok(WorkspaceFixture {
        _temp: temp,
        workspace_path,
        database_path,
        connection,
        workspace_id,
    })
}

fn insert_embedding_model(
    connection: &DbConnection,
    workspace_id: &str,
    dimension: u32,
) -> TestResult {
    let model_id = format!("mdl_{:026}", 42);
    connection
        .insert_model_registry_entry(
            &model_id,
            &CreateModelRegistryInput {
                workspace_id: workspace_id.to_owned(),
                provider: ModelProvider::Hash,
                model_name: "hash-384".to_owned(),
                purpose: ModelPurpose::Embedding,
                dimension: Some(dimension),
                distance_metric: Some(ModelDistanceMetric::Cosine),
                status: ModelRegistryStatus::Available,
                version: Some("v1".to_owned()),
                source_uri: None,
                content_hash: None,
                metadata_json: None,
                last_checked_at: None,
            },
        )
        .map_err(|error| format!("insert model registry: {error}"))
}

fn write_index_metadata(workspace_path: &Path, stored_dimension: u32) -> TestResult {
    write_index_metadata_value(
        workspace_path,
        serde_json::json!({
            "schema": INDEX_METADATA_SCHEMA_V2,
            "sourceGeneration": 0,
            "corpusRevision": expected_index_corpus_revision().as_str(),
            "evidenceSecurityPolicyEpoch": EVIDENCE_SECURITY_POLICY_EPOCH,
            "documentCount": 0,
            "documentCounts": {
                "memories": 0,
                "sessions": 0,
                "artifacts": 0,
                "rules": 0,
                "evidence": 0
            },
            "tierDocumentCounts": {
                "fast": 0,
                "quality": null,
                "lexical": cfg!(feature = "lexical-bm25").then_some(0)
            },
            "lastRebuildAt": "2026-01-01T00:00:00Z",
            "storedDimension": stored_dimension,
            "storedDistanceMetric": "cosine"
        }),
    )
}

fn write_index_metadata_value(workspace_path: &Path, metadata: Value) -> TestResult {
    let index_dir = workspace_path.join(".ee").join("index");
    fs::create_dir_all(&index_dir).map_err(|error| format!("create index dir: {error}"))?;
    let bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|error| format!("encode index metadata: {error}"))?;
    fs::write(index_dir.join("meta.json"), bytes)
        .map_err(|error| format!("write index metadata: {error}"))
}

fn insert_memory(connection: &DbConnection, workspace_id: &str) -> TestResult {
    let memory_id = format!("mem_{:026}", 42);
    connection
        .insert_memory(
            &memory_id,
            &CreateMemoryInput {
                workspace_id: workspace_id.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Check `src/core/model.rs` before changing lifecycle readiness."
                    .to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("test://model-lifecycle-readiness".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: vec!["model-lifecycle".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| format!("insert memory: {error}"))
}

fn blake3_hash(content: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(content).to_hex())
}

fn embedding_metadata_json(dimension: u32) -> Result<String, String> {
    let mut metadata = EmbeddingMetadataRecord::new(dimension, ModelDistanceMetric::Cosine);
    metadata.deterministic = true;
    metadata.model_revision = Some(OFFLINE_MODEL_REVISION.to_owned());
    metadata
        .to_canonical_json()
        .map_err(|error| format!("embedding metadata json: {error}"))
}

fn insert_offline_local_model(fixture: &WorkspaceFixture) -> Result<String, String> {
    let asset_bytes = br#"{"schema":"ee.test_model_asset.v1","provider":"hash","dimension":384,"purpose":"model-lifecycle-readiness"}"#;
    let asset_path = fixture.workspace_path.join(OFFLINE_MODEL_SOURCE_URI);
    if let Some(parent) = asset_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("create model asset dir: {error}"))?;
    }
    fs::write(&asset_path, asset_bytes).map_err(|error| format!("write model asset: {error}"))?;
    let content_hash = blake3_hash(asset_bytes);

    fixture
        .connection
        .insert_model_registry_entry(
            OFFLINE_MODEL_ID,
            &CreateModelRegistryInput {
                workspace_id: fixture.workspace_id.clone(),
                provider: ModelProvider::Hash,
                model_name: "fnv1a-384-local".to_owned(),
                purpose: ModelPurpose::Embedding,
                dimension: Some(384),
                distance_metric: Some(ModelDistanceMetric::Cosine),
                status: ModelRegistryStatus::Available,
                version: Some(OFFLINE_MODEL_REVISION.to_owned()),
                source_uri: Some(OFFLINE_MODEL_SOURCE_URI.to_owned()),
                content_hash: Some(content_hash.clone()),
                metadata_json: Some(embedding_metadata_json(384)?),
                last_checked_at: Some(OFFLINE_MODEL_CHECKED_AT.to_owned()),
            },
        )
        .map_err(|error| format!("insert offline local model: {error}"))?;

    Ok(content_hash)
}

fn write_ready_semantic_index(fixture: &WorkspaceFixture, model_hash: &str) -> TestResult {
    let source_generation = fixture
        .connection
        .get_workspace_generation(&fixture.workspace_id)
        .map_err(|error| format!("read workspace generation: {error}"))?
        .ok_or_else(|| "workspace generation row missing".to_owned())?;

    write_index_metadata_value(
        &fixture.workspace_path,
        serde_json::json!({
            "schema": INDEX_METADATA_SCHEMA_V2,
            "sourceGeneration": source_generation,
            "corpusRevision": expected_index_corpus_revision().as_str(),
            "evidenceSecurityPolicyEpoch": EVIDENCE_SECURITY_POLICY_EPOCH,
            "documentCount": 0,
            "documentCounts": {
                "memories": 0,
                "sessions": 0,
                "artifacts": 0,
                "rules": 0,
                "evidence": 0
            },
            "tierDocumentCounts": {
                "fast": 0,
                "quality": null,
                "lexical": cfg!(feature = "lexical-bm25").then_some(0)
            },
            "lastRebuildAt": "2026-06-15T00:00:01Z",
            "storedModelId": OFFLINE_MODEL_ID,
            "storedModelRevision": OFFLINE_MODEL_REVISION,
            "storedModelHash": model_hash,
            "storedDimension": 384,
            "storedDistanceMetric": "cosine",
            "storedVectorDtype": "float32",
            "derivedFrom": [".ee/ee.db", OFFLINE_MODEL_SOURCE_URI]
        }),
    )
}

fn canonicalize_model_lifecycle_json(mut value: Value) -> Value {
    value["generatedAt"] = serde_json::json!(CANONICAL_GENERATED_AT);
    value["workspaceFingerprint"] = serde_json::json!(CANONICAL_WORKSPACE_FINGERPRINT);
    value
}

fn pretty_json(value: &Value) -> Result<String, String> {
    let mut text =
        serde_json::to_string_pretty(value).map_err(|error| format!("pretty json: {error}"))?;
    text.push('\n');
    Ok(text)
}

fn assert_json_golden(relative_path: &str, actual: &Value) -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("read golden {}: {error}", path.display()))?;
    let actual = pretty_json(actual)?;
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "golden mismatch for {}\nexpected:\n{}\nactual:\n{}",
        path.display(),
        expected,
        actual
    ))
}

fn assert_no_workspace_path_leak(value: &Value, workspace_path: &Path) -> TestResult {
    let text = pretty_json(value)?;
    let workspace = workspace_path.to_string_lossy();
    ensure(
        !text.contains(workspace.as_ref()),
        "model lifecycle golden must not leak the temp workspace path",
    )
}

#[test]
fn search_surface_reports_dimension_incompatible_readiness() -> TestResult {
    let fixture = fresh_workspace()?;
    insert_embedding_model(&fixture.connection, &fixture.workspace_id, 384)?;
    write_index_metadata(&fixture.workspace_path, 128)?;

    let report = build_model_lifecycle_report_for_workspace(
        &fixture.workspace_path,
        Some(&fixture.database_path),
        Some(&fixture.connection),
    )
    .map_err(|error| format!("lifecycle report: {error:?}"))?;
    let degradation = report
        .semantic_surface_degradation("search")
        .ok_or("missing search lifecycle degradation")?;

    ensure(
        degradation.code == "embed_model_unavailable",
        "dimension mismatch reuses semantic-unavailable code",
    )?;
    ensure(
        degradation.severity == "high",
        "dimension mismatch severity",
    )?;
    ensure(
        degradation.message.contains("dimension-incompatible"),
        "search message names dimension-incompatible readiness",
    )
}

#[test]
fn recall_surface_reports_lexical_only_readiness() -> TestResult {
    let fixture = fresh_workspace()?;
    insert_memory(&fixture.connection, &fixture.workspace_id)?;

    let report = run_recall(
        &fixture.connection,
        &fixture.workspace_id,
        &RecallQuery {
            paths: vec!["src/core/model.rs".to_owned()],
            ..RecallQuery::default()
        },
    )
    .map_err(|error| format!("run recall: {error}"))?;

    ensure(
        report.items.len() == 1,
        "anchored memory should still return",
    )?;
    let lifecycle = report
        .degraded
        .iter()
        .find(|degradation| degradation.code == "embed_model_unavailable")
        .ok_or("missing model lifecycle degradation")?;
    ensure(
        lifecycle.message.contains("lexical-only"),
        "recall message names lexical-only readiness",
    )
}

#[test]
fn offline_local_model_lifecycle_matches_redacted_golden() -> TestResult {
    let fixture = fresh_workspace()?;
    let model_hash = insert_offline_local_model(&fixture)?;
    write_ready_semantic_index(&fixture, &model_hash)?;

    let report = build_model_lifecycle_report_for_workspace(
        &fixture.workspace_path,
        Some(&fixture.database_path),
        Some(&fixture.connection),
    )
    .map_err(|error| format!("lifecycle report: {error:?}"))?;
    ensure(
        report.semantic_readiness.state == "available",
        "offline local model/index fixture should be semantically ready",
    )?;
    ensure(
        report.degraded.is_empty(),
        "semantically ready fixture should not emit lifecycle degradations",
    )?;

    let actual = canonicalize_model_lifecycle_json(report.data_json());
    assert_no_workspace_path_leak(&actual, &fixture.workspace_path)?;
    assert_json_golden(OFFLINE_READY_GOLDEN_REL, &actual)
}
