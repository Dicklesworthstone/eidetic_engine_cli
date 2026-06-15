//! Integration coverage for model-lifecycle readiness on search/recall surfaces.

use std::fs;
use std::path::{Path, PathBuf};

use ee::core::model::build_model_lifecycle_report_for_workspace;
use ee::core::recall::{RecallQuery, run_recall};
use ee::db::{CreateMemoryInput, CreateModelRegistryInput, CreateWorkspaceInput, DbConnection};
use ee::models::model_registry::{
    ModelDistanceMetric, ModelProvider, ModelPurpose, ModelRegistryStatus,
};

type TestResult = Result<(), String>;

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
    let index_dir = workspace_path.join(".ee").join("index");
    fs::create_dir_all(&index_dir).map_err(|error| format!("create index dir: {error}"))?;
    fs::write(
        index_dir.join("meta.json"),
        serde_json::json!({
            "schema": "ee.index_metadata.v1",
            "sourceGeneration": 0,
            "lastRebuildAt": "2026-01-01T00:00:00Z",
            "storedDimension": stored_dimension,
            "storedDistanceMetric": "cosine"
        })
        .to_string(),
    )
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
