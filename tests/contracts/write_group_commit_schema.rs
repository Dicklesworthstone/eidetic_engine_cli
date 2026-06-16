//! bd-d67os.1: structural contract for `ee.write_group_commit.v1`.
//!
//! The first group-commit slice is a public telemetry contract, not a live
//! collector: it pins the redaction-safe counter shape, registry wiring, and
//! the closed fallback-reason vocabulary that the Track B core/integration
//! leaves (bd-d67os.2/.3/.4) must emit. See ADR 0077.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::models::{
    KNOWN_SCHEMAS, WRITE_GROUP_COMMIT_FALLBACK_DEGRADED, WRITE_GROUP_COMMIT_FALLBACK_DISABLED,
    WRITE_GROUP_COMMIT_FALLBACK_OVERSIZED, WRITE_GROUP_COMMIT_FALLBACK_SINGLE_WRITER,
    WRITE_GROUP_COMMIT_SCHEMA_V1,
};
use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.write_group_commit.v1.json";
const REDACTION_CONST: &str = "counts_latencies_no_content";

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

fn key_set(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let node = value
        .pointer(pointer)
        .ok_or_else(|| format!("schema is missing pointer {pointer}"))?;
    let object = node
        .as_object()
        .ok_or_else(|| format!("{pointer} must be a JSON object"))?;
    Ok(object.keys().cloned().collect())
}

#[test]
fn write_group_commit_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        KNOWN_SCHEMAS.contains(&WRITE_GROUP_COMMIT_SCHEMA_V1),
        "KNOWN_SCHEMAS must include ee.write_group_commit.v1",
    )?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(WRITE_GROUP_COMMIT_SCHEMA_V1),
        "schema title must be ee.write_group_commit.v1",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(WRITE_GROUP_COMMIT_SCHEMA_V1),
        "schema.const must pin ee.write_group_commit.v1",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with("ee.write_group_commit.v1.json")),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/properties/redactionStatus/const")
            .and_then(Value::as_str)
            == Some(REDACTION_CONST),
        "redactionStatus const drifted",
    )?;
    ensure(
        schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "telemetry schema must forbid additional properties",
    )?;

    let registry = public_schemas();
    let entry = registry
        .iter()
        .find(|entry| entry.id == WRITE_GROUP_COMMIT_SCHEMA_V1)
        .ok_or("public schema registry missing ee.write_group_commit.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(
        entry.category == "performance",
        "registry category must be performance",
    )?;
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some(
        WRITE_GROUP_COMMIT_SCHEMA_V1,
    )))
    .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(WRITE_GROUP_COMMIT_SCHEMA_V1),
        "registry definition must embed the write-group-commit schema",
    )
}

#[test]
fn write_group_commit_required_fields_and_fallback_reasons_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "generatedAt",
        "enabled",
        "redactionStatus",
        "batches",
        "writesCoalesced",
        "avgBatchSize",
        "fsyncCount",
        "fsyncSaved",
        "commitLatencyP50Us",
        "commitLatencyP99Us",
        "fallbackCount",
        "fallbackReasons",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("top-level required set drifted: {required:?}"),
    )?;

    // The fallbackReasons object keys are the closed reason set, and must match
    // the exported constants so emitters and consumers agree on the vocabulary.
    let reasons = key_set(&schema, "/properties/fallbackReasons/properties")?;
    let expected_reasons: BTreeSet<String> = [
        WRITE_GROUP_COMMIT_FALLBACK_DISABLED,
        WRITE_GROUP_COMMIT_FALLBACK_DEGRADED,
        WRITE_GROUP_COMMIT_FALLBACK_OVERSIZED,
        WRITE_GROUP_COMMIT_FALLBACK_SINGLE_WRITER,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        reasons == expected_reasons,
        format!("fallbackReasons closed set drifted: {reasons:?}"),
    )?;
    ensure(
        schema
            .pointer("/properties/fallbackReasons/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "fallbackReasons must forbid reasons outside the closed set",
    )
}
