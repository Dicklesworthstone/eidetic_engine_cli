//! bd-resume-verb-v0f57: structural contract for the resume wire schema
//! (`ee.resume.v1`).
//!
//! Pins schema identity, `public_schemas()` registry wiring, the report's
//! required field set, the open-loops shape, and the per-item staleness
//! contract, so surface drift fails loudly. Follows
//! `graph_suggest_links_schema.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_ID: &str = "ee.resume.v1";
const SCHEMA_REL: &str = "docs/schemas/ee.resume.v1.json";

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
fn resume_schema_identity_and_registry_are_pinned() -> TestResult {
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
        .ok_or("public schema registry missing ee.resume.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(
        entry.category == "memory",
        "registry category must be memory",
    )?;
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some(SCHEMA_ID)))
        .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "registry definition must embed the schema",
    )
}

#[test]
fn resume_required_fields_and_staleness_contract_are_pinned() -> TestResult {
    let schema = load_schema()?;

    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "workspaceId",
        "episodicTotal",
        "sessions",
        "openLoops",
        "staleCount",
        "nearbyStores",
        "nextCommands",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("report required set drifted: {required:?}"),
    )?;

    let open_loops = string_set(&schema, "/properties/openLoops/required")?;
    let expected_loops: BTreeSet<String> = ["revisitDecisions", "taggedItems"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        open_loops == expected_loops,
        format!("openLoops required set drifted: {open_loops:?}"),
    )?;

    // Every surfaced item must carry the stale field (nullable), and the
    // flag itself must name what superseded the item and why.
    let item_required = string_set(&schema, "/$defs/item/required")?;
    ensure(
        item_required.contains("stale"),
        "item.stale must be a required (nullable) field",
    )?;
    let stale_required = string_set(&schema, "/$defs/item/properties/stale/required")?;
    let expected_stale: BTreeSet<String> = ["supersededBy", "supersededByCreatedAt", "sharedTags"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        stale_required == expected_stale,
        format!("staleness contract drifted: {stale_required:?}"),
    )
}
