//! Contract checks for the graph-accretion schema governance bead.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

const INSIGHTS_SECTIONS: &[&str] = &[
    "authorities",
    "bridges",
    "causalBottlenecks",
    "comprehensiveRules",
    "contradictionClusters",
    "hubs",
    "kCore",
    "kTruss",
    "knowledgeSkyline",
    "loadBearingMemories",
    "proximityHotspots",
    "revisionFrontiers",
    "topMemories",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "{context} should succeed; stdout: {stdout}; stderr: {stderr}"
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!("{context} stderr should be empty; got {stderr}"));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{context} stdout should be JSON: {error}; stdout: {stdout}"))
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

#[test]
fn insights_empty_workspace_cli_shape_matches_contract() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();
    let output = run_ee(&["--workspace", &workspace, "--json", "insights"])?;
    let json = stdout_json(&output, "ee insights empty workspace")?;
    let data = json
        .get("data")
        .ok_or_else(|| "insights response missing data".to_owned())?;

    ensure_eq(
        json.get("schema").and_then(Value::as_str),
        Some("ee.response.v1"),
        "ee insights envelope",
        "schema",
    )?;
    ensure_eq(
        json.get("success").and_then(Value::as_bool),
        Some(true),
        "ee insights envelope",
        "success",
    )?;
    ensure_eq(
        data.get("schema").and_then(Value::as_str),
        Some("ee.insights.v1"),
        "ee insights data",
        "schema",
    )?;
    ensure_eq(
        data.get("snapshotVersion").and_then(Value::as_u64),
        Some(0),
        "ee insights data",
        "snapshotVersion",
    )?;
    ensure_eq(
        data.get("runDurationMs").and_then(Value::as_u64),
        Some(0),
        "ee insights data",
        "runDurationMs",
    )?;
    ensure_eq(
        data.pointer("/degradedSignals/0/code")
            .and_then(Value::as_str),
        Some("graph.workspace_empty"),
        "ee insights degraded signal",
        "code",
    )?;
    ensure_eq(
        data.pointer("/degradedSignals/0/severity")
            .and_then(Value::as_str),
        Some("info"),
        "ee insights degraded signal",
        "severity",
    )?;

    let sections = data
        .get("sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee insights sections must be an array".to_owned())?;
    let names = sections
        .iter()
        .map(|section| {
            section
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "section missing name".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names.as_slice() != INSIGHTS_SECTIONS {
        return Err(format!(
            "ee insights section order mismatch: expected {INSIGHTS_SECTIONS:?}, got {names:?}"
        ));
    }

    for section in sections {
        if !section
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            return Err(format!(
                "empty workspace section should have empty items: {section}"
            ));
        }
    }

    Ok(())
}

#[test]
fn insights_schema_example_matches_empty_workspace_contract() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.insights.v1.json");
    let schema = read_json(&schema_path)?;
    let example = schema
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|examples| examples.first())
        .ok_or_else(|| "ee.insights.v1 must include an example".to_owned())?;

    for (field, expected) in [
        ("schema", "ee.insights.v1"),
        ("command", "insights"),
        ("mode", "full_bundle"),
    ] {
        ensure_eq(
            example.get(field).and_then(Value::as_str),
            Some(expected),
            "ee.insights.v1 example",
            field,
        )?;
    }
    ensure_eq(
        example.get("snapshotVersion").and_then(Value::as_u64),
        Some(0),
        "ee.insights.v1 example",
        "snapshotVersion",
    )?;
    ensure_eq(
        example.get("runDurationMs").and_then(Value::as_u64),
        Some(0),
        "ee.insights.v1 example",
        "runDurationMs",
    )?;
    ensure_eq(
        example
            .pointer("/degradedSignals/0/code")
            .and_then(Value::as_str),
        Some("graph.workspace_empty"),
        "ee.insights.v1 example degraded signal",
        "code",
    )?;

    let available_sections = example
        .get("availableSections")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.insights.v1 example availableSections must be an array".to_owned())?
        .iter()
        .map(|section| {
            section
                .as_str()
                .ok_or_else(|| "availableSections entries must be strings".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if available_sections.as_slice() != INSIGHTS_SECTIONS {
        return Err(format!(
            "ee.insights.v1 example availableSections mismatch: expected {INSIGHTS_SECTIONS:?}, got {available_sections:?}"
        ));
    }

    let sections = example
        .get("sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.insights.v1 example sections must be an array".to_owned())?;
    if sections.len() != INSIGHTS_SECTIONS.len() {
        return Err(format!(
            "ee.insights.v1 example should contain {} sections, got {}",
            INSIGHTS_SECTIONS.len(),
            sections.len()
        ));
    }
    for (section, expected_name) in sections.iter().zip(INSIGHTS_SECTIONS) {
        ensure_eq(
            section.get("name").and_then(Value::as_str),
            Some(*expected_name),
            "ee.insights.v1 example section",
            "name",
        )?;
        if !section
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            return Err(format!(
                "ee.insights.v1 example section {expected_name} should have empty items"
            ));
        }
    }

    Ok(())
}

#[test]
fn insights_hubs_and_authorities_schema_documents_hits_items() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.insights.v1.json");
    let schema = read_json(&schema_path)?;

    assert_hits_item_schema(
        &schema,
        "hubItem",
        "hubScore",
        "hub",
        "#/$defs/hubItem",
        "hubs",
    )?;
    assert_hits_item_schema(
        &schema,
        "authorityItem",
        "authorityScore",
        "authority",
        "#/$defs/authorityItem",
        "authorities",
    )?;

    Ok(())
}

#[test]
fn insights_bridges_schema_documents_cluster_disconnection_items() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.insights.v1.json");
    let schema = read_json(&schema_path)?;
    let bridge_item = schema
        .pointer("/$defs/bridgeItem")
        .and_then(Value::as_object)
        .ok_or_else(|| "ee.insights.v1 missing $defs.bridgeItem".to_owned())?;
    let required = bridge_item
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "$defs.bridgeItem.required must be an array".to_owned())?;
    for field in [
        "rank",
        "memoryId",
        "articulationPoint",
        "clusterDisconnectionMagnitude",
        "disconnectedComponents",
        "affectedMemoryCount",
        "largestComponentSize",
        "evidence",
    ] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            return Err(format!("bridgeItem.required missing {field}"));
        }
    }

    ensure_eq(
        schema
            .pointer("/$defs/bridgeItem/properties/evidence/properties/algorithm/const")
            .and_then(Value::as_str),
        Some("articulation_points"),
        "ee.insights.v1 bridgeItem",
        "evidence.algorithm.const",
    )?;

    let section_schema = serde_json::to_string(
        schema
            .pointer("/$defs/section")
            .ok_or_else(|| "ee.insights.v1 missing $defs.section".to_owned())?,
    )
    .map_err(|error| format!("serialize section schema: {error}"))?;
    if !section_schema.contains("\"#/$defs/bridgeItem\"") {
        return Err("bridges section items must reference $defs.bridgeItem".to_owned());
    }

    let example_sections = schema
        .pointer("/examples/0/sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.insights.v1 example sections must be an array".to_owned())?;
    let bridge_section = example_sections
        .iter()
        .find(|section| section.get("name").and_then(Value::as_str) == Some("bridges"))
        .ok_or_else(|| "ee.insights.v1 example missing bridges section".to_owned())?;
    if !bridge_section
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("cluster-disconnection-magnitude"))
    {
        return Err("bridges example summary must name cluster-disconnection-magnitude".to_owned());
    }

    Ok(())
}

#[test]
fn insights_revision_frontiers_schema_documents_dominance_items() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.insights.v1.json");
    let schema = read_json(&schema_path)?;
    let frontier_item = schema
        .pointer("/$defs/revisionFrontierItem")
        .and_then(Value::as_object)
        .ok_or_else(|| "ee.insights.v1 missing $defs.revisionFrontierItem".to_owned())?;
    let required = frontier_item
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "$defs.revisionFrontierItem.required must be an array".to_owned())?;
    for field in [
        "rank",
        "memoryId",
        "logicalId",
        "dominanceFrontierSize",
        "affectedMemoryCount",
        "recentRevisionAt",
        "evidence",
    ] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            return Err(format!("revisionFrontierItem.required missing {field}"));
        }
    }

    ensure_eq(
        schema
            .pointer("/$defs/revisionFrontierItem/properties/evidence/properties/algorithm/const")
            .and_then(Value::as_str),
        Some("dominance_frontiers"),
        "ee.insights.v1 revisionFrontierItem",
        "evidence.algorithm.const",
    )?;

    let section_schema = serde_json::to_string(
        schema
            .pointer("/$defs/section")
            .ok_or_else(|| "ee.insights.v1 missing $defs.section".to_owned())?,
    )
    .map_err(|error| format!("serialize section schema: {error}"))?;
    if !section_schema.contains("\"#/$defs/revisionFrontierItem\"") {
        return Err(
            "revisionFrontiers section items must reference $defs.revisionFrontierItem".to_owned(),
        );
    }

    let example_sections = schema
        .pointer("/examples/0/sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.insights.v1 example sections must be an array".to_owned())?;
    let revision_section = example_sections
        .iter()
        .find(|section| section.get("name").and_then(Value::as_str) == Some("revisionFrontiers"))
        .ok_or_else(|| "ee.insights.v1 example missing revisionFrontiers section".to_owned())?;
    if !revision_section
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("dominance-frontier size"))
    {
        return Err(
            "revisionFrontiers example summary must name dominance-frontier size".to_owned(),
        );
    }

    Ok(())
}

#[test]
fn insights_proximity_hotspots_schema_documents_min_cut_items() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.insights.v1.json");
    let schema = read_json(&schema_path)?;
    let hotspot_item = schema
        .pointer("/$defs/proximityHotspotItem")
        .and_then(Value::as_object)
        .ok_or_else(|| "ee.insights.v1 missing $defs.proximityHotspotItem".to_owned())?;
    let required = hotspot_item
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "$defs.proximityHotspotItem.required must be an array".to_owned())?;
    for field in [
        "rank",
        "memoryA",
        "memoryB",
        "minCut",
        "interpretation",
        "treePath",
        "evidence",
    ] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            return Err(format!("proximityHotspotItem.required missing {field}"));
        }
    }

    ensure_eq(
        schema
            .pointer("/$defs/proximityHotspotItem/properties/evidence/properties/schema/const")
            .and_then(Value::as_str),
        Some("ee.proximity.v1"),
        "ee.insights.v1 proximityHotspotItem",
        "evidence.schema.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/proximityHotspotItem/properties/evidence/properties/algorithm/const")
            .and_then(Value::as_str),
        Some("gomory_hu_tree"),
        "ee.insights.v1 proximityHotspotItem",
        "evidence.algorithm.const",
    )?;

    let section_schema = serde_json::to_string(
        schema
            .pointer("/$defs/section")
            .ok_or_else(|| "ee.insights.v1 missing $defs.section".to_owned())?,
    )
    .map_err(|error| format!("serialize section schema: {error}"))?;
    if !section_schema.contains("\"#/$defs/proximityHotspotItem\"") {
        return Err(
            "proximityHotspots section items must reference $defs.proximityHotspotItem".to_owned(),
        );
    }

    let example_sections = schema
        .pointer("/examples/0/sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.insights.v1 example sections must be an array".to_owned())?;
    let proximity_section = example_sections
        .iter()
        .find(|section| section.get("name").and_then(Value::as_str) == Some("proximityHotspots"))
        .ok_or_else(|| "ee.insights.v1 example missing proximityHotspots section".to_owned())?;
    if !proximity_section
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("min-cut distance"))
    {
        return Err("proximityHotspots example summary must name min-cut distance".to_owned());
    }

    Ok(())
}

fn assert_hits_item_schema(
    schema: &Value,
    item_name: &str,
    score_field: &str,
    interpretation: &str,
    section_ref: &str,
    section_name: &str,
) -> TestResult {
    let item = schema
        .pointer(&format!("/$defs/{item_name}"))
        .and_then(Value::as_object)
        .ok_or_else(|| format!("ee.insights.v1 missing $defs.{item_name}"))?;
    let required = item
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("$defs.{item_name}.required must be an array"))?;
    for field in [
        "rank",
        "memoryId",
        score_field,
        "interpretation",
        "evidence",
    ] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            return Err(format!("{item_name}.required missing {field}"));
        }
    }

    ensure_eq(
        schema
            .pointer(&format!(
                "/$defs/{item_name}/properties/{score_field}/minimum"
            ))
            .and_then(Value::as_u64),
        Some(0),
        "ee.insights.v1 HITS item",
        score_field,
    )?;
    ensure_eq(
        schema
            .pointer(&format!(
                "/$defs/{item_name}/properties/interpretation/const"
            ))
            .and_then(Value::as_str),
        Some(interpretation),
        "ee.insights.v1 HITS item",
        "interpretation.const",
    )?;
    ensure_eq(
        schema
            .pointer(&format!(
                "/$defs/{item_name}/properties/evidence/properties/schema/const"
            ))
            .and_then(Value::as_str),
        Some("ee.graph.hits.v1"),
        "ee.insights.v1 HITS item",
        "evidence.schema.const",
    )?;
    ensure_eq(
        schema
            .pointer(&format!(
                "/$defs/{item_name}/properties/evidence/properties/algorithm/const"
            ))
            .and_then(Value::as_str),
        Some("hits_centrality_directed"),
        "ee.insights.v1 HITS item",
        "evidence.algorithm.const",
    )?;

    let section_schema = serde_json::to_string(
        schema
            .pointer("/$defs/section")
            .ok_or_else(|| "ee.insights.v1 missing $defs.section".to_owned())?,
    )
    .map_err(|error| format!("serialize section schema: {error}"))?;
    if !section_schema.contains(&format!("\"{section_ref}\"")) {
        return Err(format!(
            "{section_name} section items must reference {section_ref}"
        ));
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
