//! bd-d67os.11: structural contract for `ee.diag.contention.v1`.
//!
//! Pins the read-only contention-diagnostic wire shape, the schema/struct
//! agreement, and the `public_schemas()` registry wiring that the Track D CLI
//! leaf (bd-d67os.12) will emit. Follows the `ee.diag.plan_cache.v1` pattern
//! (co-located schema tag, registered in `public_schemas()`, not in
//! `KNOWN_SCHEMAS`). See ADR 0079.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::core::contention::{ContentionInputs, build_contention_report};
use ee::models::contention::CONTENTION_DIAG_SCHEMA_V1;
use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.diag.contention.v1.json";

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
fn contention_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(CONTENTION_DIAG_SCHEMA_V1),
        "schema title must be ee.diag.contention.v1",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some("ee.response.v2"),
        "envelope schema.const must be ee.response.v2",
    )?;
    ensure(
        schema
            .pointer("/properties/data/properties/command/const")
            .and_then(Value::as_str)
            == Some("diag contention"),
        "data.command const must be 'diag contention'",
    )?;
    ensure(
        schema
            .pointer("/$defs/contentionReport/properties/schemaTag/const")
            .and_then(Value::as_str)
            == Some(CONTENTION_DIAG_SCHEMA_V1),
        "report schemaTag const must pin ee.diag.contention.v1",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with("ee.diag.contention.v1.json")),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "envelope must forbid additional properties",
    )?;

    let registry = public_schemas();
    let entry = registry
        .iter()
        .find(|entry| entry.id == CONTENTION_DIAG_SCHEMA_V1)
        .ok_or("public schema registry missing ee.diag.contention.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(entry.category == "ops", "registry category must be ops")?;
    let exported: Value =
        serde_json::from_str(&render_schema_export_json(Some(CONTENTION_DIAG_SCHEMA_V1)))
            .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(CONTENTION_DIAG_SCHEMA_V1),
        "registry definition must embed the contention schema",
    )
}

#[test]
fn contention_report_required_fields_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let required = string_set(&schema, "/$defs/contentionReport/required")?;
    let expected: BTreeSet<String> = [
        "schemaTag",
        "overallPosture",
        "writeLock",
        "readPool",
        "singleflight",
        "topContention",
        "unavailableSources",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("contentionReport required set drifted: {required:?}"),
    )
}

#[test]
fn builder_output_conforms_to_schema_shape() -> TestResult {
    // A report with all three core sources present and no future-feature
    // sub-reports: serialized keys must exactly equal the schema's required set.
    let report = build_contention_report(&ContentionInputs::default());
    let value = serde_json::to_value(&report).map_err(|e| format!("serialize report: {e}"))?;
    let object = value
        .as_object()
        .ok_or("serialized report must be an object")?;

    let keys: BTreeSet<String> = object.keys().cloned().collect();
    let required = string_set(&load_json(SCHEMA_REL)?, "/$defs/contentionReport/required")?;
    ensure(
        keys == required,
        format!("serialized report keys {keys:?} do not match schema required {required:?}"),
    )?;

    ensure(
        value.pointer("/schemaTag").and_then(Value::as_str) == Some(CONTENTION_DIAG_SCHEMA_V1),
        "schemaTag must be emitted",
    )?;
    ensure(
        value.pointer("/overallPosture").and_then(Value::as_str) == Some("ok"),
        "default inputs yield overall posture ok",
    )?;
    // Omit-safe future-feature sub-reports must be absent, not null.
    ensure(
        value.get("groupCommit").is_none()
            && value.get("indexIntake").is_none()
            && value.get("l2Cache").is_none(),
        "future-feature sub-reports must be omitted when absent",
    )?;
    // With no inputs, all three core sources are reported as unavailable gaps.
    let gaps = value
        .pointer("/unavailableSources")
        .and_then(Value::as_array)
        .ok_or("unavailableSources must be an array")?;
    ensure(
        gaps.len() == 3,
        format!(
            "expected 3 source gaps from empty inputs, got {}",
            gaps.len()
        ),
    )
}
