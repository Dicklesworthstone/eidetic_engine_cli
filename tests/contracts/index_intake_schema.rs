//! bd-d67os.5: structural contract for `ee.index_intake.v1`.
//!
//! The first incremental-index slice is a public telemetry contract, not a live
//! collector: it pins the redaction-safe intake-mode and document-count shape,
//! registry wiring, and the closed fallback-to-full reason vocabulary that the
//! Track C core/integration leaves (bd-d67os.6/.7/.8) must emit. See ADR 0078.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::models::{
    INDEX_INTAKE_FALLBACK_DELTA_OVER_THRESHOLD, INDEX_INTAKE_FALLBACK_FORCED_REINDEX,
    INDEX_INTAKE_FALLBACK_GENERATION_SKEW, INDEX_INTAKE_FALLBACK_INDEX_ABSENT,
    INDEX_INTAKE_FALLBACK_TIER_UNAVAILABLE, INDEX_INTAKE_MODE_FULL_REBUILD,
    INDEX_INTAKE_MODE_INCREMENTAL, INDEX_INTAKE_MODE_SEGMENT_MERGE, INDEX_INTAKE_SCHEMA_V1,
    KNOWN_SCHEMAS,
};
use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.index_intake.v1.json";
const REDACTION_CONST: &str = "counts_modes_no_content";

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
fn index_intake_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        KNOWN_SCHEMAS.contains(&INDEX_INTAKE_SCHEMA_V1),
        "KNOWN_SCHEMAS must include ee.index_intake.v1",
    )?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(INDEX_INTAKE_SCHEMA_V1),
        "schema title must be ee.index_intake.v1",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(INDEX_INTAKE_SCHEMA_V1),
        "schema.const must pin ee.index_intake.v1",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with("ee.index_intake.v1.json")),
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
        .find(|entry| entry.id == INDEX_INTAKE_SCHEMA_V1)
        .ok_or("public schema registry missing ee.index_intake.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(
        entry.category == "performance",
        "registry category must be performance",
    )?;
    let exported: Value =
        serde_json::from_str(&render_schema_export_json(Some(INDEX_INTAKE_SCHEMA_V1)))
            .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(INDEX_INTAKE_SCHEMA_V1),
        "registry definition must embed the index-intake schema",
    )
}

#[test]
fn index_intake_modes_and_fallback_reasons_are_closed_sets() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;

    let modes = string_set(&schema, "/properties/intakeMode/enum")?;
    let expected_modes: BTreeSet<String> = [
        INDEX_INTAKE_MODE_FULL_REBUILD,
        INDEX_INTAKE_MODE_INCREMENTAL,
        INDEX_INTAKE_MODE_SEGMENT_MERGE,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        modes == expected_modes,
        format!("intakeMode closed set drifted: {modes:?}"),
    )?;

    let reasons = string_set(&schema, "/properties/fallbackToFull/properties/reason/enum")?;
    let expected_reasons: BTreeSet<String> = [
        INDEX_INTAKE_FALLBACK_INDEX_ABSENT,
        INDEX_INTAKE_FALLBACK_GENERATION_SKEW,
        INDEX_INTAKE_FALLBACK_TIER_UNAVAILABLE,
        INDEX_INTAKE_FALLBACK_FORCED_REINDEX,
        INDEX_INTAKE_FALLBACK_DELTA_OVER_THRESHOLD,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        reasons == expected_reasons,
        format!("fallbackToFull.reason closed set drifted: {reasons:?}"),
    )?;

    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "generatedAt",
        "redactionStatus",
        "intakeMode",
        "docsTouched",
        "baseDocCount",
        "segmentDocCount",
        "rebuildAvoidedCount",
        "mergeCount",
        "fallbackToFull",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("top-level required set drifted: {required:?}"),
    )
}
