//! Contract coverage for `ee model status` and `ee model list` (EE-294).
//!
//! Pinning the JSON shape and the degraded-mode codes so future revisions of
//! the model registry do not silently churn the public output.

use std::fs;
use std::path::Path;

use ee::core::model::{
    MODEL_LIST_SCHEMA_V1, MODEL_STATUS_SCHEMA_V2, ModelListOptions, ModelStatusOptions,
    build_model_list_report, build_model_status_report,
};
use ee::db::{CreateModelRegistryInput, CreateWorkspaceInput, DbConnection};
use ee::models::model_registry::{
    ModelDistanceMetric, ModelProvider, ModelPurpose, ModelRegistryStatus,
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn fresh_db_for_workspace(workspace_path: &Path) -> Result<(std::path::PathBuf, String), String> {
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
    let connection =
        DbConnection::open_file(database_path).map_err(|error| format!("reopen db: {error}"))?;
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
        .map_err(|error| format!("insert registry entry: {error}"))?;
    Ok(())
}

fn insert_reranker_entry(
    database_path: &Path,
    workspace_id: &str,
    id: &str,
    name: &str,
    status: ModelRegistryStatus,
) -> TestResult {
    let connection =
        DbConnection::open_file(database_path).map_err(|error| format!("reopen db: {error}"))?;
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
        .map_err(|error| format!("insert registry entry: {error}"))?;
    Ok(())
}

#[test]
fn model_status_empty_registry_emits_versioned_schema_and_degradation() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize: {error}"))?;
    fresh_db_for_workspace(&workspace_path)?;

    let report = build_model_status_report(&ModelStatusOptions {
        workspace_path: &workspace_path,
        database_path: None,
    })
    .map_err(|error| format!("status report: {error:?}"))?;

    ensure(report.schema == MODEL_STATUS_SCHEMA_V2, "schema constant")?;
    ensure(report.registered_count == 0, "registered_count == 0")?;
    ensure(report.available_count == 0, "available_count == 0")?;
    ensure(
        report.reranker.registered_count == 0 && report.reranker.available_count == 0,
        "reranker counts should be empty",
    )?;
    ensure(
        report.active.source == "frankensearch_hash_fallback",
        "fallback source when registry empty",
    )?;
    ensure(
        report.degradations.len() == 1 && report.degradations[0].code == "model_registry_empty",
        "expected model_registry_empty degradation",
    )?;

    let json = report.data_json();
    ensure(
        json.get("schema").and_then(|value| value.as_str()) == Some(MODEL_STATUS_SCHEMA_V2),
        "json schema field present",
    )?;
    let active = json.get("active").ok_or("missing active")?;
    let reranker = json.get("reranker").ok_or("missing reranker")?;
    ensure(
        active.get("fastModelId").is_some(),
        "fastModelId present in active",
    )?;
    ensure(
        active.get("source").and_then(|value| value.as_str())
            == Some("frankensearch_hash_fallback"),
        "active.source matches fallback",
    )?;
    ensure(
        reranker
            .get("availableModelIds")
            .and_then(|value| value.as_array())
            .is_some_and(Vec::is_empty),
        "reranker available model IDs empty",
    )?;
    let degradations = json
        .get("degradations")
        .and_then(|value| value.as_array())
        .ok_or("degradations array")?;
    ensure(degradations.len() == 1, "one degradation in JSON")?;
    Ok(())
}

#[test]
fn model_status_picks_first_available_registry_entry() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize: {error}"))?;
    let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
    insert_registry_entry(
        &database_path,
        &workspace_id,
        "mdl_01HQ3K5Z000000000000000010",
        ModelProvider::Hash,
        "fnv1a-256",
        ModelRegistryStatus::Available,
    )?;
    insert_registry_entry(
        &database_path,
        &workspace_id,
        "mdl_01HQ3K5Z000000000000000011",
        ModelProvider::Model2Vec,
        "minilm",
        ModelRegistryStatus::Disabled,
    )?;

    let report = build_model_status_report(&ModelStatusOptions {
        workspace_path: &workspace_path,
        database_path: None,
    })
    .map_err(|error| format!("status report: {error:?}"))?;

    ensure(report.registered_count == 2, "registered_count")?;
    ensure(report.available_count == 1, "available_count")?;
    ensure(
        report.active.source == "registry_observed",
        "registry_observed source",
    )?;
    ensure(report.degradations.is_empty(), "no degradations expected")?;
    let selected = report
        .active
        .selected_registry_entry
        .as_ref()
        .ok_or("selected entry missing")?;
    ensure(selected.status == "available", "selected available status")?;
    ensure(
        report.reranker.registered_count == 0 && report.reranker.available_count == 0,
        "no rerankers in embedding-only registry",
    )?;
    Ok(())
}

#[test]
fn model_status_reports_reranker_registry_separately() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize: {error}"))?;
    let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
    insert_reranker_entry(
        &database_path,
        &workspace_id,
        "mdl_01HQ3K5Z000000000000000030",
        "ms-marco-minilm-l-6-v2",
        ModelRegistryStatus::Available,
    )?;

    let report = build_model_status_report(&ModelStatusOptions {
        workspace_path: &workspace_path,
        database_path: None,
    })
    .map_err(|error| format!("status report: {error:?}"))?;

    ensure(report.registered_count == 1, "registered_count")?;
    ensure(report.available_count == 1, "available_count")?;
    ensure(
        report.active.source == "frankensearch_hash_fallback",
        "reranker entry should not select active embedder",
    )?;
    ensure(
        report.active.selected_registry_entry.is_none(),
        "active embedder selection should ignore rerankers",
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
        "available reranker ids",
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
        "reranker availableModelIds JSON",
    )?;
    Ok(())
}

#[test]
fn model_list_returns_entries_in_stable_order() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize: {error}"))?;
    let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
    insert_registry_entry(
        &database_path,
        &workspace_id,
        "mdl_01HQ3K5Z000000000000000020",
        ModelProvider::Model2Vec,
        "minilm",
        ModelRegistryStatus::Available,
    )?;
    insert_registry_entry(
        &database_path,
        &workspace_id,
        "mdl_01HQ3K5Z000000000000000021",
        ModelProvider::Hash,
        "fnv1a-256",
        ModelRegistryStatus::Available,
    )?;

    let report = build_model_list_report(&ModelListOptions {
        workspace_path: &workspace_path,
        database_path: None,
    })
    .map_err(|error| format!("list report: {error:?}"))?;

    ensure(report.schema == MODEL_LIST_SCHEMA_V1, "schema constant")?;
    ensure(report.entries.len() == 2, "entry count")?;
    // list_model_registry_entries orders by purpose, provider, model_name, id
    // hash precedes model2vec lexicographically.
    ensure(
        report.entries[0].provider == "hash",
        "first entry should be hash provider",
    )?;
    ensure(
        report.entries[1].provider == "model2vec",
        "second entry should be model2vec provider",
    )?;
    ensure(report.degradations.is_empty(), "no degradations expected")?;

    let json = report.data_json();
    ensure(
        json.get("schema").and_then(|value| value.as_str()) == Some(MODEL_LIST_SCHEMA_V1),
        "json schema",
    )?;
    let entries = json
        .get("entries")
        .and_then(|value| value.as_array())
        .ok_or("entries array")?;
    ensure(entries.len() == 2, "json entries length")?;
    ensure(
        entries[0].get("provider").and_then(|value| value.as_str()) == Some("hash"),
        "json first provider",
    )?;
    Ok(())
}

#[test]
fn model_list_empty_workspace_emits_degradation() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize: {error}"))?;
    fresh_db_for_workspace(&workspace_path)?;

    let report = build_model_list_report(&ModelListOptions {
        workspace_path: &workspace_path,
        database_path: None,
    })
    .map_err(|error| format!("list report: {error:?}"))?;

    ensure(report.entries.is_empty(), "no entries")?;
    ensure(
        report.degradations.len() == 1 && report.degradations[0].code == "model_registry_empty",
        "empty registry degradation",
    )?;
    Ok(())
}

#[test]
fn model_list_reports_reranker_only_registry_as_no_available_embedding() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize: {error}"))?;
    let (database_path, workspace_id) = fresh_db_for_workspace(&workspace_path)?;
    insert_reranker_entry(
        &database_path,
        &workspace_id,
        "mdl_01HQ3K5Z000000000000000031",
        "ms-marco-minilm-l-6-v2",
        ModelRegistryStatus::Available,
    )?;

    let report = build_model_list_report(&ModelListOptions {
        workspace_path: &workspace_path,
        database_path: None,
    })
    .map_err(|error| format!("list report: {error:?}"))?;

    ensure(
        report.entries.len() == 1,
        "reranker entry should remain listed",
    )?;
    ensure(report.degradations.len() == 1, "degradation count")?;
    ensure(
        report.degradations[0].code == "model_registry_no_available_entry",
        "reranker-only registry should degrade semantic embedding list status",
    )?;
    Ok(())
}
