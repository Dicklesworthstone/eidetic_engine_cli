//! bd-3a1op.6: structural contract for the graph-diff wire schema
//! (`ee.graph.diff.v1`, ADR 0066).
//!
//! Pins schema identity, `public_schemas()` registry wiring, the report's
//! required field set, the summary counters, and the community-delta /
//! mover shapes, so surface drift fails loudly. Follows
//! `graph_suggest_links_schema.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_ID: &str = "ee.graph.diff.v1";
const SCHEMA_REL: &str = "docs/schemas/ee.graph.diff.v1.json";

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
fn graph_diff_schema_identity_and_registry_are_pinned() -> TestResult {
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
        .ok_or("public schema registry missing ee.graph.diff.v1")?;
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
fn graph_diff_required_fields_and_shapes_are_pinned() -> TestResult {
    let schema = load_schema()?;

    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "graphType",
        "from",
        "to",
        "summary",
        "nodesAdded",
        "nodesRemoved",
        "edgesAdded",
        "edgesRemoved",
        "communities",
        "movers",
        "detailCap",
        "truncated",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("report required set drifted: {required:?}"),
    )?;

    let summary = string_set(&schema, "/properties/summary/required")?;
    let expected_summary: BTreeSet<String> = [
        "nodesAdded",
        "nodesRemoved",
        "edgesAdded",
        "edgesRemoved",
        "communitiesMatched",
        "communityBirths",
        "communityDeaths",
        "centralityMovers",
        "centralityOmitted",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        summary == expected_summary,
        format!("summary counter set drifted: {summary:?}"),
    )?;

    // Communities: matched pairs carry fingerprints + jaccard + churn;
    // births/deaths are endpoints; zero-churn matches are only counted.
    let communities = string_set(&schema, "/properties/communities/required")?;
    let expected_communities: BTreeSet<String> = ["matched", "births", "deaths", "unchanged"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        communities == expected_communities,
        format!("community-delta shape drifted: {communities:?}"),
    )?;
    let matched = string_set(
        &schema,
        "/properties/communities/properties/matched/items/required",
    )?;
    let expected_matched: BTreeSet<String> = [
        "fromFingerprint",
        "toFingerprint",
        "jaccard",
        "joined",
        "left",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        matched == expected_matched,
        format!("matched-community shape drifted: {matched:?}"),
    )?;

    // Movers come strictly from persisted per-side pagerank.
    let mover = string_set(&schema, "/properties/movers/items/required")?;
    let expected_mover: BTreeSet<String> = ["memoryId", "fromPagerank", "toPagerank", "delta"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        mover == expected_mover,
        format!("mover shape drifted: {mover:?}"),
    )?;

    // Edges are content-hash keyed.
    let edge = string_set(&schema, "/$defs/edge/required")?;
    let expected_edge: BTreeSet<String> = ["key", "source", "target", "relation", "directed"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        edge == expected_edge,
        format!("edge shape drifted: {edge:?}"),
    )
}
