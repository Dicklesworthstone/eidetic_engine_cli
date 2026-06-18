//! bd-1et0v.1: structural contract for `ee.embedding_posture.v1`.
//!
//! This leaf defines the shared retrieval-truth posture block before runtime
//! surfaces consume it. The schema is intentionally redaction-safe: mode,
//! model ids, dimensions, registry counts, and vector coverage only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::models::{
    EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH, EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
    EMBEDDING_POSTURE_MODE_NEURAL_REMOTE_BLOCKED, EMBEDDING_POSTURE_SCHEMA_V1, KNOWN_SCHEMAS,
};
use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.embedding_posture.v1.json";

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn load_json(relative: &str) -> Result<Value, String> {
    let path = repo_path(relative);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn string_set(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let node = value
        .pointer(pointer)
        .ok_or_else(|| format!("schema is missing pointer {pointer}"))?;
    let array = node
        .as_array()
        .ok_or_else(|| format!("{pointer} must be a JSON array"))?;
    let mut out = BTreeSet::new();
    for entry in array {
        let value = entry
            .as_str()
            .ok_or_else(|| format!("{pointer} contains non-string entry: {entry}"))?;
        out.insert(value.to_owned());
    }
    Ok(out)
}

#[test]
fn embedding_posture_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        KNOWN_SCHEMAS.contains(&EMBEDDING_POSTURE_SCHEMA_V1),
        "KNOWN_SCHEMAS must include ee.embedding_posture.v1",
    )?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(EMBEDDING_POSTURE_SCHEMA_V1),
        "schema title must be ee.embedding_posture.v1",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(EMBEDDING_POSTURE_SCHEMA_V1),
        "schema.const must pin ee.embedding_posture.v1",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with("ee.embedding_posture.v1.json")),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "embedding posture schema must forbid additional top-level properties",
    )?;

    let registry = public_schemas();
    let entry = registry
        .iter()
        .find(|entry| entry.id == EMBEDDING_POSTURE_SCHEMA_V1)
        .ok_or("public schema registry missing ee.embedding_posture.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(
        entry.category == "search",
        "registry category must be search",
    )?;
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some(
        EMBEDDING_POSTURE_SCHEMA_V1,
    )))
    .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(EMBEDDING_POSTURE_SCHEMA_V1),
        "registry definition must embed the embedding-posture schema",
    )
}

#[test]
fn embedding_posture_required_fields_and_modes_are_closed() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;

    let modes = string_set(&schema, "/properties/mode/enum")?;
    let expected_modes: BTreeSet<String> = [
        EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
        EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH,
        EMBEDDING_POSTURE_MODE_NEURAL_REMOTE_BLOCKED,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        modes == expected_modes,
        format!("embedding posture mode closed set drifted: {modes:?}"),
    )?;

    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "mode",
        "semantic",
        "source",
        "fast_model_id",
        "fast_dimension",
        "quality_model_id",
        "quality_dimension",
        "deterministic",
        "registered_model_count",
        "available_model_count",
        "selected_registry_model",
        "vector_coverage",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("required field set drifted: {required:?}"),
    )?;

    let coverage_required = string_set(&schema, "/properties/vector_coverage/required")?;
    let expected_coverage: BTreeSet<String> = ["embedded", "total"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        coverage_required == expected_coverage,
        format!("vector_coverage required fields drifted: {coverage_required:?}"),
    )
}

#[test]
fn embedding_posture_selected_registry_model_shape_is_redaction_safe() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let selected_model_schema = schema
        .pointer("/properties/selected_registry_model/oneOf/1")
        .ok_or("selected_registry_model object branch missing")?;
    ensure(
        selected_model_schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "selected_registry_model object must forbid additional properties",
    )?;

    let required = string_set(selected_model_schema, "/required")?;
    let expected: BTreeSet<String> = [
        "id",
        "provider",
        "model_name",
        "status",
        "dimension",
        "deterministic",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("selected_registry_model required fields drifted: {required:?}"),
    )?;

    for forbidden in ["content", "query", "embedding", "vector", "raw_embedding"] {
        ensure(
            selected_model_schema
                .pointer(&format!("/properties/{forbidden}"))
                .is_none(),
            format!("selected_registry_model must not expose `{forbidden}`"),
        )?;
    }

    Ok(())
}
