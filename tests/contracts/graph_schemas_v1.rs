//! Contract checks for the graph-accretion schema governance bead.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

type TestResult = Result<(), String>;

const GRAPH_SCHEMA_IDS: &[&str] = &[
    "ee.insights.v1",
    "ee.context.pack_dna.v1",
    "ee.why.causal.v1",
    "ee.health.structural.v1",
    "ee.status.skyline.v1",
    "ee.memory.impact_analysis.v1",
    "ee.proximity.v1",
    "ee.why.v1",
    "ee.context.v1",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn schema_file_name(schema_id: &str) -> String {
    format!("{schema_id}.json")
}

fn snapshot_file_name(schema_id: &str) -> String {
    let normalized = schema_id.replace('.', "_");
    format!("graph_schemas_v1__{normalized}.snap")
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

#[test]
fn graph_accretion_schemas_are_supported_and_documented() -> TestResult {
    let supported = ee::core::supported_schemas()
        .into_iter()
        .map(|schema| schema.schema)
        .collect::<Vec<_>>();

    for schema_id in GRAPH_SCHEMA_IDS {
        if !supported.contains(schema_id) {
            return Err(format!("supported_schemas() missing {schema_id}"));
        }

        let schema_path = repo_root()
            .join("docs")
            .join("schemas")
            .join(schema_file_name(schema_id));
        let schema = read_json(&schema_path)?;
        let expected_id = format!("https://eidetic-engine/schemas/{schema_id}.json");
        assert_schema_basics(schema_id, &expected_id, &schema)?;

        let snapshot_path = repo_root()
            .join("tests")
            .join("snapshots")
            .join(snapshot_file_name(schema_id));
        if !snapshot_path.exists() {
            return Err(format!(
                "{schema_id} missing snapshot {}",
                snapshot_path.display()
            ));
        }
    }

    Ok(())
}

fn assert_schema_basics(schema_id: &str, expected_id: &str, schema: &Value) -> TestResult {
    ensure_eq(
        schema.get("$schema").and_then(Value::as_str),
        Some("https://json-schema.org/draft/2020-12/schema"),
        schema_id,
        "$schema",
    )?;
    ensure_eq(
        schema.get("$id").and_then(Value::as_str),
        Some(expected_id),
        schema_id,
        "$id",
    )?;
    ensure_eq(
        schema.get("title").and_then(Value::as_str),
        Some(schema_id),
        schema_id,
        "title",
    )?;
    ensure_eq(
        schema.get("additionalProperties").and_then(Value::as_bool),
        Some(false),
        schema_id,
        "additionalProperties",
    )?;

    let presets = schema
        .get("field_presets")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{schema_id} missing field_presets"))?;
    for preset in ["minimal", "summary", "standard", "full"] {
        if !presets
            .get(preset)
            .and_then(Value::as_array)
            .is_some_and(|fields| !fields.is_empty())
        {
            return Err(format!("{schema_id} field_presets.{preset} is empty"));
        }
    }

    Ok(())
}

#[test]
fn context_pack_dna_schema_example_matches_required_shape() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.context.pack_dna.v1.json");
    let schema = read_json(&schema_path)?;
    let example = schema
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|examples| examples.first())
        .ok_or_else(|| "ee.context.pack_dna.v1 must include an example".to_string())?;

    ensure_eq(
        example.get("schema").and_then(Value::as_str),
        Some("ee.context.pack_dna.v1"),
        "ee.context.pack_dna.v1 example",
        "schema",
    )?;
    for required in [
        "snapshotVersion",
        "voronoiDominator",
        "communityOfMass",
        "egoSubgraph",
        "pprNeighbors",
        "degraded",
    ] {
        if example.get(required).is_none() {
            return Err(format!(
                "ee.context.pack_dna.v1 example missing required field {required}"
            ));
        }
    }

    let ppr_neighbors = example
        .get("pprNeighbors")
        .and_then(Value::as_array)
        .ok_or_else(|| "pprNeighbors must be an array".to_string())?;
    if ppr_neighbors.len() > 10 {
        return Err("pprNeighbors example exceeds top-10 contract".to_string());
    }

    let ego_subgraph = example
        .get("egoSubgraph")
        .and_then(Value::as_object)
        .ok_or_else(|| "egoSubgraph must be an object".to_string())?;
    for field in ["nodes", "edges"] {
        if !ego_subgraph
            .get(field)
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
        {
            return Err(format!("egoSubgraph.{field} must be a non-empty array"));
        }
    }

    Ok(())
}

fn ensure_eq<T>(actual: Option<T>, expected: Option<T>, schema_id: &str, field: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{schema_id} {field} mismatch: expected {expected:?}, got {actual:?}"
        ))
    }
}
