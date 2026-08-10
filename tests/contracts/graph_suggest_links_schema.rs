//! bd-3a1op.6: structural contract for the suggest-links wire schema
//! (`ee.graph.suggest_links.v1`, ADR 0066).
//!
//! Pins schema identity, `public_schemas()` registry wiring, the report's
//! required field set, the typed-relation vocabulary, and the per-row signal
//! keys, so surface drift fails loudly. Follows `contention_schema.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_ID: &str = "ee.graph.suggest_links.v1";
const SCHEMA_REL: &str = "docs/schemas/ee.graph.suggest_links.v1.json";

fn load_schema() -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_REL);
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
    let array = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("schema is missing array at {pointer}"))?;
    let mut out = BTreeSet::new();
    for entry in array {
        out.insert(
            entry
                .as_str()
                .ok_or_else(|| format!("{pointer} contains non-string entry: {entry}"))?
                .to_owned(),
        );
    }
    Ok(out)
}

#[test]
fn suggest_links_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "schema title must equal its id",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_ID),
        "properties.schema.const must pin the id",
    )?;

    let registry = public_schemas();
    let entry = registry
        .iter()
        .find(|entry| entry.id == SCHEMA_ID)
        .ok_or("public schema registry missing ee.graph.suggest_links.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(entry.category == "graph", "registry category must be graph")?;
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some(SCHEMA_ID)))
        .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "registry definition must embed the schema",
    )
}

#[test]
fn suggest_links_required_fields_and_vocabularies_are_pinned() -> TestResult {
    let schema = load_schema()?;

    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "suggestions",
        "candidateCount",
        "affinityCold",
        "proposed",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("report required set drifted: {required:?}"),
    )?;

    let relations = string_set(
        &schema,
        "/properties/suggestions/items/properties/suggestedRelation/enum",
    )?;
    let expected_relations: BTreeSet<String> = ["related", "supports", "contradicts"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        relations == expected_relations,
        format!("typed-relation vocabulary drifted: {relations:?}"),
    )?;

    let signal_required = string_set(
        &schema,
        "/properties/suggestions/items/properties/signals/required",
    )?;
    let expected_signals: BTreeSet<String> = [
        "adamicAdar",
        "jaccardTags",
        "ppr",
        "affinity",
        "preferentialAttachment",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        signal_required == expected_signals,
        format!("per-signal key set drifted: {signal_required:?}"),
    )
}
