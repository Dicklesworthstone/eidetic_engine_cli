//! Contract checks for the graph-accretion schema governance bead.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ee::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
    MemoryLinkRelation, MemoryLinkSource,
};
use serde_json::Value;

type TestResult = Result<(), String>;

/// Pads or truncates a short identifier to exactly 30 chars while preserving
/// the canonical prefix (e.g. `mem_`, `wsp_`). Required because the database
/// CHECK constraints enforce `length(id) = 30` for memory/workspace ids.
fn pad_id(id: &str, prefix: &str) -> String {
    pad_id_to(id, prefix, 30)
}

/// Mirror of `crate::core::causal::stable_workspace_id` (which is `pub(crate)`
/// and so not reachable from integration tests). Produces the same `wsp_*`
/// id the production CLI computes from a workspace path, so the test fixture
/// can pre-insert workspace+memory rows whose ids match the ones the CLI
/// resolves at runtime via `ensure_workspace`.
fn stable_workspace_id_for_test(path: &std::path::Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    ee::models::WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

/// Pads or truncates a short identifier to exactly `width` chars while
/// preserving the canonical prefix. Used by both memory/workspace ids
/// (length=30) and memory link ids (length=31).
fn pad_id_to(id: &str, prefix: &str, width: usize) -> String {
    debug_assert!(id.starts_with(prefix), "id {id} must start with {prefix}");
    if id.len() == width {
        return id.to_owned();
    }
    if id.len() > width {
        return id.chars().take(width).collect();
    }
    let mut padded = String::with_capacity(width);
    padded.push_str(id);
    while padded.len() < width {
        padded.push('0');
    }
    padded
}

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
    "knowledgeGaps",
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

    // NOTE: Production `ee insights` still emits the legacy `ee.response.v1`
    // outer envelope (see src/cli/insights/mod.rs::render_insights_json which
    // imports RESPONSE_SCHEMA_V1). A prior commit aspirationally updated this
    // assertion to `ee.response.v2` ahead of the production migration; until
    // the insights pipeline is upgraded to v2, the test must reflect what the
    // CLI actually emits. The data-level `ee.insights.v1` schema id is the
    // stable payload contract.
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

#[test]
fn insights_load_bearing_schema_documents_bipartite_items() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.insights.v1.json");
    let schema = read_json(&schema_path)?;
    let item = schema
        .pointer("/$defs/loadBearingItem")
        .and_then(Value::as_object)
        .ok_or_else(|| "ee.insights.v1 missing $defs.loadBearingItem".to_owned())?;
    let required = item
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "$defs.loadBearingItem.required must be an array".to_owned())?;
    for field in [
        "rank",
        "memoryId",
        "loadBearingScore",
        "citingRuleCount",
        "interpretation",
        "evidence",
    ] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            return Err(format!("loadBearingItem.required missing {field}"));
        }
    }

    ensure_eq(
        schema
            .pointer("/$defs/loadBearingItem/properties/loadBearingScore/minimum")
            .and_then(Value::as_u64),
        Some(0),
        "ee.insights.v1 loadBearingItem",
        "loadBearingScore.minimum",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingItem/properties/citingRuleCount/minimum")
            .and_then(Value::as_u64),
        Some(0),
        "ee.insights.v1 loadBearingItem",
        "citingRuleCount.minimum",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingItem/properties/interpretation/const")
            .and_then(Value::as_str),
        Some("load_bearing"),
        "ee.insights.v1 loadBearingItem",
        "interpretation.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingItem/properties/evidence/properties/schema/const")
            .and_then(Value::as_str),
        Some("ee.graph.hits.v1"),
        "ee.insights.v1 loadBearingItem",
        "evidence.schema.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingItem/properties/evidence/properties/algorithm/const")
            .and_then(Value::as_str),
        Some("bipartite_hits"),
        "ee.insights.v1 loadBearingItem",
        "evidence.algorithm.const",
    )?;

    let section_schema = serde_json::to_string(
        schema
            .pointer("/$defs/section")
            .ok_or_else(|| "ee.insights.v1 missing $defs.section".to_owned())?,
    )
    .map_err(|error| format!("serialize section schema: {error}"))?;
    if !section_schema.contains("\"#/$defs/loadBearingItem\"") {
        return Err(
            "loadBearingMemories section items must reference $defs.loadBearingItem".to_owned(),
        );
    }

    let example_sections = schema
        .pointer("/examples/0/sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.insights.v1 example sections must be an array".to_owned())?;
    let load_bearing_section = example_sections
        .iter()
        .find(|section| section.get("name").and_then(Value::as_str) == Some("loadBearingMemories"))
        .ok_or_else(|| "ee.insights.v1 example missing loadBearingMemories section".to_owned())?;
    if !load_bearing_section
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("rule-to-source provenance"))
    {
        return Err(
            "loadBearingMemories example summary must name rule-to-source provenance".to_owned(),
        );
    }

    Ok(())
}

#[test]
fn insights_knowledge_gaps_schema_documents_reflection_recommendations() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.insights.v1.json");
    let schema = read_json(&schema_path)?;
    let item = schema
        .pointer("/$defs/knowledgeGapItem")
        .and_then(Value::as_object)
        .ok_or_else(|| "ee.insights.v1 missing $defs.knowledgeGapItem".to_owned())?;
    let required = item
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "$defs.knowledgeGapItem.required must be an array".to_owned())?;
    for field in [
        "rank",
        "gapId",
        "category",
        "priority",
        "confidence",
        "sourceMemoryIds",
        "metricEvidence",
        "explanation",
        "recommendation",
    ] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            return Err(format!("knowledgeGapItem.required missing {field}"));
        }
    }

    ensure_eq(
        schema
            .pointer("/$defs/knowledgeGapItem/properties/metricEvidence/properties/schema/const")
            .and_then(Value::as_str),
        Some("ee.graph.knowledge_gap.v1"),
        "ee.insights.v1 knowledgeGapItem",
        "metricEvidence.schema.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/knowledgeGapRecommendation/properties/kind/const")
            .and_then(Value::as_str),
        Some("reflect_propose"),
        "ee.insights.v1 knowledgeGapRecommendation",
        "kind.const",
    )?;
    let compact = schema
        .pointer("/$defs/knowledgeGapCompactRecommendation")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            "ee.insights.v1 missing $defs.knowledgeGapCompactRecommendation".to_owned()
        })?;
    let compact_required = compact
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "$defs.knowledgeGapCompactRecommendation.required must be an array".to_owned()
        })?;
    for field in [
        "id",
        "severity",
        "reason",
        "suggested_query",
        "recommendation_kind",
    ] {
        if !compact_required
            .iter()
            .any(|value| value.as_str() == Some(field))
        {
            return Err(format!(
                "knowledgeGapCompactRecommendation.required missing {field}"
            ));
        }
    }
    ensure_eq(
        schema
            .pointer(
                "/$defs/knowledgeGapCompactRecommendation/properties/recommendation_kind/const",
            )
            .and_then(Value::as_str),
        Some("reflect_propose"),
        "ee.insights.v1 knowledgeGapCompactRecommendation",
        "recommendation_kind.const",
    )?;

    let section_schema = serde_json::to_string(
        schema
            .pointer("/$defs/section")
            .ok_or_else(|| "ee.insights.v1 missing $defs.section".to_owned())?,
    )
    .map_err(|error| format!("serialize section schema: {error}"))?;
    if !section_schema.contains("\"#/$defs/knowledgeGapItem\"") {
        return Err("knowledgeGaps section items must reference $defs.knowledgeGapItem".to_owned());
    }
    if !section_schema.contains("\"#/$defs/knowledgeGapCompactRecommendation\"") {
        return Err(
            "knowledgeGaps section recommendations must reference $defs.knowledgeGapCompactRecommendation"
                .to_owned(),
        );
    }

    let example_sections = schema
        .pointer("/examples/0/sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.insights.v1 example sections must be an array".to_owned())?;
    let knowledge_gaps_section = example_sections
        .iter()
        .find(|section| section.get("name").and_then(Value::as_str) == Some("knowledgeGaps"))
        .ok_or_else(|| "ee.insights.v1 example missing knowledgeGaps section".to_owned())?;
    if !knowledge_gaps_section
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("Graph-derived gaps"))
    {
        return Err("knowledgeGaps example summary must name graph-derived gaps".to_owned());
    }
    ensure_eq(
        knowledge_gaps_section
            .get("section")
            .and_then(Value::as_str),
        Some("knowledgeGaps"),
        "ee.insights.v1 knowledgeGaps example",
        "section",
    )?;
    if !knowledge_gaps_section
        .get("recommendations")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        return Err("knowledgeGaps example must include empty recommendations".to_owned());
    }

    Ok(())
}

#[test]
fn insights_knowledge_gaps_cli_freezes_graph_fixture_recommendations() -> TestResult {
    let fixture = KnowledgeGapsFixture::new("knowledge-gaps-contract")?;
    fixture.seed_gap_graph()?;
    let baseline_counts = fixture.storage_counts()?;

    let first = fixture.run_knowledge_gaps()?;
    let second = fixture.run_knowledge_gaps()?;
    ensure_eq(
        first
            .pointer("/data/selectedSection")
            .and_then(Value::as_str),
        Some("knowledgeGaps"),
        "knowledgeGaps fixture",
        "selectedSection",
    )?;
    ensure_eq(
        first
            .pointer("/data/sections/0/section")
            .and_then(Value::as_str),
        Some("knowledgeGaps"),
        "knowledgeGaps fixture",
        "sections[0].section",
    )?;
    if normalize_insights_json_for_determinism(first.clone())
        != normalize_insights_json_for_determinism(second.clone())
    {
        return Err(
            "knowledgeGaps fixture output is not deterministic across repeated CLI runs".to_owned(),
        );
    }

    let section = first
        .pointer("/data/sections/0")
        .ok_or_else(|| "knowledgeGaps response missing first section".to_owned())?;
    let items = section
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "knowledgeGaps items must be an array".to_owned())?;
    let recommendations = section
        .get("recommendations")
        .and_then(Value::as_array)
        .ok_or_else(|| "knowledgeGaps recommendations must be an array".to_owned())?;
    if items.len() != 4 {
        return Err(format!(
            "knowledgeGaps fixture should emit four representative gaps, got {}: {items:?}",
            items.len()
        ));
    }
    if recommendations.len() != items.len() {
        return Err(format!(
            "knowledgeGaps recommendations should match item count: items={}, recommendations={}",
            items.len(),
            recommendations.len()
        ));
    }

    let categories = items
        .iter()
        .map(|item| {
            item.get("category")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("knowledge gap missing category: {item}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if categories
        != vec![
            "thin_evidence_bridge",
            "unresolved_contradiction_cluster",
            "harmful_neighborhood_without_rule",
            "underdetermined_causal_chain",
        ]
    {
        return Err(format!(
            "knowledgeGaps categories/order mismatch: {categories:?}"
        ));
    }

    let rendered_section =
        serde_json::to_string(section).map_err(|error| format!("serialize section: {error}"))?;
    for forbidden in [
        "placeholder",
        "todo",
        "fake",
        "reflect ingest",
        "curate apply",
    ] {
        if rendered_section.to_ascii_lowercase().contains(forbidden) {
            return Err(format!(
                "knowledgeGaps output must not contain forbidden placeholder/action text `{forbidden}`"
            ));
        }
    }

    let hmac_key_path = fixture.write_reflection_key()?;
    for (item, recommendation) in items.iter().zip(recommendations) {
        assert_knowledge_gap_item_contract(item)?;
        assert_knowledge_gap_compact_recommendation_contract(item, recommendation)?;
        let source_ids = item
            .get("sourceMemoryIds")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("sourceMemoryIds must be an array: {item}"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("sourceMemoryIds entry must be a string: {value}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let command = item
            .pointer("/recommendation/command")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("knowledge gap recommendation missing command: {item}"))?;
        let command_sources = reflect_propose_source_memory_args(command)?;
        if command_sources != source_ids {
            return Err(format!(
                "recommendation command source ids must match item source ids exactly: command={command_sources:?}, item={source_ids:?}, command={command}"
            ));
        }
        if !command.contains("--dry-run") || command.contains("reflect ingest") {
            return Err(format!(
                "recommendation command must be reflect propose dry-run only: {command}"
            ));
        }

        let request = fixture.run_reflect_propose_recommendation(command, &hmac_key_path)?;
        ensure_eq(
            request.pointer("/data/schema").and_then(Value::as_str),
            Some("ee.reflect.propose.v1"),
            "knowledgeGaps reflect request",
            "schema",
        )?;
        ensure_eq(
            request
                .pointer("/data/reflectionKind")
                .and_then(Value::as_str),
            Some("gaps"),
            "knowledgeGaps reflect request",
            "reflectionKind",
        )?;
        ensure_eq(
            request.pointer("/data/dryRun").and_then(Value::as_bool),
            Some(true),
            "knowledgeGaps reflect request",
            "dryRun",
        )?;
        ensure_eq(
            request
                .pointer("/data/request/schema")
                .and_then(Value::as_str),
            Some("ee.reflect.request.v1"),
            "knowledgeGaps reflect request artifact",
            "request.schema",
        )?;
        let request_sources = request
            .pointer("/data/request/sourcePackage/sources")
            .and_then(Value::as_array)
            .ok_or_else(|| "reflect request sourcePackage.sources must be an array".to_owned())?
            .iter()
            .map(|source| {
                source
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("reflect request source missing id: {source}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if request_sources != source_ids {
            return Err(format!(
                "reflect request sources must match recommendation sources exactly: request={request_sources:?}, item={source_ids:?}"
            ));
        }
    }

    if fixture.storage_counts()? != baseline_counts {
        return Err("knowledgeGaps insights/reflect-propose dry-run mutated storage".to_owned());
    }

    Ok(())
}

#[test]
fn insights_knowledge_gaps_cli_emits_no_fake_gaps_for_healthy_or_unavailable_graphs() -> TestResult
{
    let healthy = KnowledgeGapsFixture::new("knowledge-gaps-healthy")?;
    healthy.seed_healthy_graph()?;
    let before = healthy.storage_counts()?;
    let healthy_json = healthy.run_knowledge_gaps()?;
    assert_knowledge_gap_section_empty(&healthy_json, "healthy graph")?;
    if healthy.storage_counts()? != before {
        return Err("healthy knowledgeGaps probe mutated storage".to_owned());
    }

    let unavailable = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = unavailable.path().to_string_lossy().into_owned();
    let output = run_ee(&[
        "--workspace",
        &workspace,
        "--json",
        "insights",
        "--section",
        "knowledgeGaps",
    ])?;
    let json = stdout_json(&output, "ee insights knowledgeGaps unavailable workspace")?;
    assert_knowledge_gap_section_empty(&json, "unavailable graph")?;
    ensure_eq(
        json.pointer("/data/degradedSignals/0/code")
            .and_then(Value::as_str),
        Some("graph.workspace_empty"),
        "knowledgeGaps unavailable graph",
        "degradedSignals[0].code",
    )?;
    if serde_json::to_string(&json)
        .map_err(|error| format!("serialize unavailable graph response: {error}"))?
        .contains("insights_section_unavailable")
    {
        return Err(
            "knowledgeGaps unavailable graph must not use placeholder degradation".to_owned(),
        );
    }

    Ok(())
}

#[test]
fn why_schema_documents_load_bearing_graph_block() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.why.v1.json");
    let schema = read_json(&schema_path)?;

    ensure_eq(
        schema
            .pointer("/properties/graph/properties/loadBearing/oneOf/1/$ref")
            .and_then(Value::as_str),
        Some("#/$defs/loadBearingWhy"),
        "ee.why.v1 loadBearing",
        "graph.loadBearing.$ref",
    )?;

    let block = schema
        .pointer("/$defs/loadBearingWhy")
        .and_then(Value::as_object)
        .ok_or_else(|| "ee.why.v1 missing $defs.loadBearingWhy".to_owned())?;
    let required = block
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "$defs.loadBearingWhy.required must be an array".to_owned())?;
    for field in [
        "isLoadBearing",
        "loadBearingScore",
        "authorityRank",
        "citingRuleCount",
        "citingRules",
        "interpretation",
        "evidence",
        "rationale",
    ] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            return Err(format!("loadBearingWhy.required missing {field}"));
        }
    }

    ensure_eq(
        schema
            .pointer("/$defs/loadBearingWhy/properties/loadBearingScore/minimum")
            .and_then(Value::as_u64),
        Some(0),
        "ee.why.v1 loadBearingWhy",
        "loadBearingScore.minimum",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingWhy/properties/authorityRank/minimum")
            .and_then(Value::as_u64),
        Some(1),
        "ee.why.v1 loadBearingWhy",
        "authorityRank.minimum",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingWhy/properties/citingRuleCount/minimum")
            .and_then(Value::as_u64),
        Some(0),
        "ee.why.v1 loadBearingWhy",
        "citingRuleCount.minimum",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingEvidence/properties/schema/const")
            .and_then(Value::as_str),
        Some("ee.graph.hits.v1"),
        "ee.why.v1 loadBearingEvidence",
        "schema.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingEvidence/properties/algorithm/const")
            .and_then(Value::as_str),
        Some("bipartite_hits"),
        "ee.why.v1 loadBearingEvidence",
        "algorithm.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/loadBearingEvidence/properties/projection/const")
            .and_then(Value::as_str),
        Some("rule_provenance_bipartite"),
        "ee.why.v1 loadBearingEvidence",
        "projection.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/ruleReference/properties/relation/const")
            .and_then(Value::as_str),
        Some("cites"),
        "ee.why.v1 ruleReference",
        "relation.const",
    )?;

    let block_schema = serde_json::to_string(
        schema
            .pointer("/$defs/loadBearingWhy")
            .ok_or_else(|| "ee.why.v1 missing $defs.loadBearingWhy".to_owned())?,
    )
    .map_err(|error| format!("serialize loadBearingWhy schema: {error}"))?;
    for forbidden in [
        "\"content\"",
        "\"contentPreview\"",
        "\"filePath\"",
        "\"path\"",
        "\"query\"",
        "\"workspacePath\"",
    ] {
        if block_schema.contains(forbidden) {
            return Err(format!(
                "loadBearingWhy schema should not expose raw-bearing field {forbidden}"
            ));
        }
    }

    let example = schema
        .pointer("/examples/0")
        .ok_or_else(|| "ee.why.v1 must include a loadBearing example".to_owned())?;
    ensure_eq(
        example.get("schema").and_then(Value::as_str),
        Some("ee.why.v1"),
        "ee.why.v1 example",
        "schema",
    )?;
    ensure_eq(
        example.get("memoryId").and_then(Value::as_str),
        Some("mem_load_bearing_release_rule"),
        "ee.why.v1 example",
        "memoryId",
    )?;
    ensure_eq(
        example
            .pointer("/graph/loadBearing/isLoadBearing")
            .and_then(Value::as_bool),
        Some(true),
        "ee.why.v1 example loadBearing",
        "isLoadBearing",
    )?;
    ensure_eq(
        example
            .pointer("/graph/loadBearing/authorityRank")
            .and_then(Value::as_u64),
        Some(1),
        "ee.why.v1 example loadBearing",
        "authorityRank",
    )?;
    ensure_eq(
        example
            .pointer("/graph/loadBearing/citingRuleCount")
            .and_then(Value::as_u64),
        Some(2),
        "ee.why.v1 example loadBearing",
        "citingRuleCount",
    )?;
    ensure_eq(
        example
            .pointer("/graph/loadBearing/interpretation")
            .and_then(Value::as_str),
        Some("load_bearing"),
        "ee.why.v1 example loadBearing",
        "interpretation",
    )?;
    ensure_eq(
        example
            .pointer("/graph/loadBearing/evidence/algorithm")
            .and_then(Value::as_str),
        Some("bipartite_hits"),
        "ee.why.v1 example loadBearing",
        "evidence.algorithm",
    )?;
    let example_rules = example
        .pointer("/graph/loadBearing/citingRules")
        .and_then(Value::as_array)
        .ok_or_else(|| "ee.why.v1 example citingRules must be an array".to_owned())?;
    if example_rules.len() != 2 {
        return Err(format!(
            "ee.why.v1 example should include two redaction-safe rule references, got {}",
            example_rules.len()
        ));
    }
    for rule in example_rules {
        if !rule
            .get("ruleId")
            .and_then(Value::as_str)
            .is_some_and(|id| {
                id.starts_with("rule_")
                    && !id.contains('/')
                    && !id.contains("file")
                    && !id.contains("content")
            })
        {
            return Err(format!(
                "ee.why.v1 example rule reference should expose only a safe rule id: {rule}"
            ));
        }
        ensure_eq(
            rule.get("relation").and_then(Value::as_str),
            Some("cites"),
            "ee.why.v1 example ruleReference",
            "relation",
        )?;
    }

    Ok(())
}

#[test]
fn why_schema_documents_hits_graph_block() -> TestResult {
    let schema_path = repo_root()
        .join("docs")
        .join("schemas")
        .join("ee.why.v1.json");
    let schema = read_json(&schema_path)?;

    ensure_eq(
        schema
            .pointer("/properties/graph/properties/hits/oneOf/1/$ref")
            .and_then(Value::as_str),
        Some("#/$defs/hitsWhy"),
        "ee.why.v1 hits",
        "graph.hits.$ref",
    )?;

    let block = schema
        .pointer("/$defs/hitsWhy")
        .and_then(Value::as_object)
        .ok_or_else(|| "ee.why.v1 missing $defs.hitsWhy".to_owned())?;
    let required = block
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "$defs.hitsWhy.required must be an array".to_owned())?;
    for field in [
        "authorityScore",
        "authorityRank",
        "hubScore",
        "hubRank",
        "dominantRole",
        "profileInfluence",
        "evidence",
        "rationale",
    ] {
        if !required.iter().any(|value| value.as_str() == Some(field)) {
            return Err(format!("hitsWhy.required missing {field}"));
        }
    }

    for (field, minimum) in [
        ("authorityScore", 0),
        ("authorityRank", 1),
        ("hubScore", 0),
        ("hubRank", 1),
    ] {
        ensure_eq(
            schema
                .pointer(&format!("/$defs/hitsWhy/properties/{field}/minimum"))
                .and_then(Value::as_u64),
            Some(minimum),
            "ee.why.v1 hitsWhy",
            field,
        )?;
    }
    ensure_eq(
        schema
            .pointer("/$defs/hitsEvidence/properties/schema/const")
            .and_then(Value::as_str),
        Some("ee.graph.hits.v1"),
        "ee.why.v1 hitsEvidence",
        "schema.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/hitsEvidence/properties/algorithm/const")
            .and_then(Value::as_str),
        Some("hits_centrality_directed"),
        "ee.why.v1 hitsEvidence",
        "algorithm.const",
    )?;
    ensure_eq(
        schema
            .pointer("/$defs/hitsEvidence/properties/graphType/const")
            .and_then(Value::as_str),
        Some("memory_links"),
        "ee.why.v1 hitsEvidence",
        "graphType.const",
    )?;

    let example = schema
        .pointer("/examples/0")
        .ok_or_else(|| "ee.why.v1 must include a HITS example".to_owned())?;
    ensure_eq(
        example
            .pointer("/graph/hits/dominantRole")
            .and_then(Value::as_str),
        Some("authority"),
        "ee.why.v1 example hits",
        "dominantRole",
    )?;
    ensure_eq(
        example
            .pointer("/graph/hits/authorityRank")
            .and_then(Value::as_u64),
        Some(2),
        "ee.why.v1 example hits",
        "authorityRank",
    )?;
    ensure_eq(
        example
            .pointer("/graph/hits/hubRank")
            .and_then(Value::as_u64),
        Some(9),
        "ee.why.v1 example hits",
        "hubRank",
    )?;
    ensure_eq(
        example
            .pointer("/graph/hits/evidence/algorithm")
            .and_then(Value::as_str),
        Some("hits_centrality_directed"),
        "ee.why.v1 example hits",
        "evidence.algorithm",
    )?;
    ensure_eq(
        example
            .pointer("/graph/hits/profileInfluence/groundingBoost")
            .and_then(Value::as_f64)
            .map(|value| value > 0.0),
        Some(true),
        "ee.why.v1 example hits",
        "profileInfluence.groundingBoost",
    )?;
    ensure_eq(
        example
            .pointer("/graph/hits/profileInfluence/orientationBoost")
            .and_then(Value::as_f64)
            .map(|value| value > 0.0),
        Some(true),
        "ee.why.v1 example hits",
        "profileInfluence.orientationBoost",
    )?;

    Ok(())
}

#[test]
fn insights_onboarding_documents_hits_workflow_for_agents() -> TestResult {
    let path = repo_root()
        .join("docs")
        .join("agent-ux")
        .join("insights-onboarding.md");
    let markdown =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;

    for needle in [
        "ee insights --section hubs --workspace . --json",
        "ee insights --section authorities --workspace . --json",
        "ee why mem_authority --workspace . --json",
        "ee context \"ground release evidence\" --profile grounding --workspace . --json",
        "ee context \"map release dependencies\" --profile orientation --workspace . --json",
        "`authorityScore`",
        "`authorityRank`",
        "`hubScore`",
        "`hubRank`",
        "`dominantRole`",
        "`profileInfluence`",
        "`rationale`",
        "`hits_centrality_directed`",
        "`evidence.schema: \"ee.graph.hits.v1\"`",
        "`grounding`",
        "`orientation`",
        "`balanced`",
    ] {
        if !markdown.contains(needle) {
            return Err(format!(
                "{} missing HITS onboarding needle: {needle}",
                path.display()
            ));
        }
    }

    Ok(())
}

#[test]
fn insights_onboarding_documents_load_bearing_workflow_for_agents() -> TestResult {
    let path = repo_root()
        .join("docs")
        .join("agent-ux")
        .join("insights-onboarding.md");
    let markdown =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;

    for needle in [
        "ee insights --section loadBearingMemories --workspace . --json",
        "ee why mem_load_bearing --workspace . --json",
        ".data.graph.loadBearing",
        "`loadBearingScore`",
        "`citingRuleCount`",
        "`interpretation`",
        "`\"load_bearing\"`",
        "`evidence.algorithm`",
        "`\"bipartite_hits\"`",
        "`isLoadBearing`",
        "`authorityRank`",
        "`citingRules`",
        "`evidence.projection`",
        "`rationale`",
        "rule-to-source provenance projection",
        "rule IDs and relations",
    ] {
        if !markdown.contains(needle) {
            return Err(format!(
                "{} missing load-bearing onboarding needle: {needle}",
                path.display()
            ));
        }
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

#[derive(Debug)]
struct KnowledgeGapsFixture {
    _tempdir: tempfile::TempDir,
    workspace: PathBuf,
    database_path: PathBuf,
    workspace_id: String,
}

impl KnowledgeGapsFixture {
    fn new(label: &str) -> Result<Self, String> {
        let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let workspace = tempdir.path().to_path_buf();
        let database_dir = workspace.join(".ee");
        fs::create_dir_all(&database_dir)
            .map_err(|error| format!("create {}: {error}", database_dir.display()))?;
        let database_path = database_dir.join("ee.db");
        // Use the same workspace-id derivation the production CLI uses
        // (`crate::core::causal::stable_workspace_id`) so that when the test
        // pre-seeds memories under this id and then invokes `ee
        // --workspace <tempdir>`, the CLI's `ensure_workspace` lookup-by-path
        // returns the same row. A path-stable id also satisfies the DB CHECK
        // constraint `id GLOB 'wsp_*' AND length(id) = 30`.
        let workspace_id = stable_workspace_id_for_test(&workspace);
        let fixture = Self {
            _tempdir: tempdir,
            workspace,
            database_path,
            workspace_id,
        };
        fixture.initialize_database(label)?;
        Ok(fixture)
    }

    fn initialize_database(&self, label: &str) -> Result<(), String> {
        let connection = self.connection()?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &self.workspace_id,
                &CreateWorkspaceInput {
                    path: self.workspace.display().to_string(),
                    name: Some(label.to_owned()),
                },
            )
            .map_err(|error| error.to_string())
    }

    fn connection(&self) -> Result<DbConnection, String> {
        DbConnection::open_file(&self.database_path).map_err(|error| error.to_string())
    }

    fn seed_gap_graph(&self) -> Result<(), String> {
        let connection = self.connection()?;
        for (id, level, kind, content, confidence) in [
            (
                "mem_kg_bridge_a",
                "semantic",
                "fact",
                "Bridge endpoint A.",
                0.9,
            ),
            (
                "mem_kg_bridge_b",
                "semantic",
                "fact",
                "Bridge articulation B.",
                0.9,
            ),
            (
                "mem_kg_bridge_c",
                "semantic",
                "fact",
                "Bridge endpoint C.",
                0.9,
            ),
            (
                "mem_kg_contradiction_a",
                "semantic",
                "fact",
                "Contradiction exemplar A.",
                0.9,
            ),
            (
                "mem_kg_contradiction_b",
                "semantic",
                "fact",
                "Contradiction exemplar B.",
                0.9,
            ),
            (
                "mem_kg_contradiction_c",
                "semantic",
                "fact",
                "Contradiction exemplar C.",
                0.9,
            ),
            (
                "mem_kg_harmful_outcome",
                "episodic",
                "failure",
                "Harmful deployment outcome without a durable rule.",
                0.8,
            ),
            (
                "mem_kg_harm_neighbor",
                "semantic",
                "fact",
                "Nearby evidence for the harmful incident.",
                0.8,
            ),
            (
                "mem_kg_causal_source",
                "semantic",
                "fact",
                "Low-confidence causal source.",
                0.6,
            ),
            (
                "mem_kg_causal_target",
                "semantic",
                "fact",
                "Low-confidence causal target.",
                0.6,
            ),
        ] {
            self.insert_memory(&connection, id, level, kind, content, confidence)?;
        }

        self.insert_link(
            &connection,
            "link_kg_bridge_1",
            "mem_kg_bridge_a",
            "mem_kg_bridge_b",
            MemoryLinkRelation::Supports,
            1,
            1.0,
        )?;
        self.insert_link(
            &connection,
            "link_kg_bridge_2",
            "mem_kg_bridge_b",
            "mem_kg_bridge_c",
            MemoryLinkRelation::Supports,
            1,
            1.0,
        )?;
        self.insert_link(
            &connection,
            "link_kg_contra_1",
            "mem_kg_contradiction_a",
            "mem_kg_contradiction_b",
            MemoryLinkRelation::Contradicts,
            1,
            1.0,
        )?;
        self.insert_link(
            &connection,
            "link_kg_contra_2",
            "mem_kg_contradiction_b",
            "mem_kg_contradiction_c",
            MemoryLinkRelation::Contradicts,
            1,
            1.0,
        )?;
        self.insert_link(
            &connection,
            "link_kg_contra_3",
            "mem_kg_contradiction_a",
            "mem_kg_contradiction_c",
            MemoryLinkRelation::Contradicts,
            1,
            1.0,
        )?;
        self.insert_link(
            &connection,
            "link_kg_harm_neighbor",
            "mem_kg_harmful_outcome",
            "mem_kg_harm_neighbor",
            MemoryLinkRelation::Supports,
            1,
            1.0,
        )?;
        self.insert_link(
            &connection,
            "link_kg_causal_low_confidence",
            "mem_kg_causal_source",
            "mem_kg_causal_target",
            MemoryLinkRelation::Supports,
            4,
            0.25,
        )
    }

    fn seed_healthy_graph(&self) -> Result<(), String> {
        let connection = self.connection()?;
        for (id, level, kind, content, confidence) in [
            ("mem_kg_healthy_a", "semantic", "fact", "Healthy A.", 0.9),
            ("mem_kg_healthy_b", "semantic", "fact", "Healthy B.", 0.9),
            ("mem_kg_healthy_c", "semantic", "fact", "Healthy C.", 0.9),
            (
                "mem_kg_healthy_rule",
                "procedural",
                "rule",
                "Durable rule with adjacent evidence.",
                0.9,
            ),
        ] {
            self.insert_memory(&connection, id, level, kind, content, confidence)?;
        }
        for (id, source, target) in [
            ("link_kg_healthy_1", "mem_kg_healthy_a", "mem_kg_healthy_b"),
            ("link_kg_healthy_2", "mem_kg_healthy_b", "mem_kg_healthy_c"),
            ("link_kg_healthy_3", "mem_kg_healthy_a", "mem_kg_healthy_c"),
            (
                "link_kg_healthy_4",
                "mem_kg_healthy_b",
                "mem_kg_healthy_rule",
            ),
        ] {
            self.insert_link(
                &connection,
                id,
                source,
                target,
                MemoryLinkRelation::Supports,
                3,
                1.0,
            )?;
        }
        Ok(())
    }

    fn insert_memory(
        &self,
        connection: &DbConnection,
        id: &str,
        level: &str,
        kind: &str,
        content: &str,
        confidence: f32,
    ) -> Result<(), String> {
        let id = pad_id(id, "mem_");
        connection
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace_id.clone(),
                    level: level.to_owned(),
                    kind: kind.to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn insert_link(
        &self,
        connection: &DbConnection,
        id: &str,
        source: &str,
        target: &str,
        relation: MemoryLinkRelation,
        evidence_count: u32,
        confidence: f32,
    ) -> Result<(), String> {
        // memory_link ids have their own CHECK constraint (31 chars), and the
        // src/dst memory ids must round-trip the 30-char `mem_*` shape we used
        // at insertion time.
        let link_id = pad_id_to(id, "link_", 31);
        connection
            .insert_memory_link(
                &link_id,
                &CreateMemoryLinkInput {
                    src_memory_id: pad_id(source, "mem_"),
                    dst_memory_id: pad_id(target, "mem_"),
                    relation,
                    weight: 1.0,
                    confidence,
                    directed: false,
                    evidence_count,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("bd-3bsvv-contract".to_owned()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn run_knowledge_gaps(&self) -> Result<Value, String> {
        let workspace = self.workspace.to_string_lossy().into_owned();
        let output = run_ee(&[
            "--workspace",
            &workspace,
            "--json",
            "insights",
            "--section",
            "knowledgeGaps",
            "--limit",
            "10",
        ])?;
        stdout_json(&output, "ee insights --section knowledgeGaps")
    }

    fn write_reflection_key(&self) -> Result<PathBuf, String> {
        let path = self.workspace.join("reflection_hmac.key");
        fs::write(&path, "bd-3bsvv-knowledge-gaps-contract-key\n")
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        Ok(path)
    }

    fn run_reflect_propose_recommendation(
        &self,
        command: &str,
        hmac_key_path: &Path,
    ) -> Result<Value, String> {
        let args = recommendation_command_args(command)?;
        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .args(args)
            .current_dir(&self.workspace)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY")
            .env("EE_REFLECTION_HMAC_KEY_ID", "bd-3bsvv-contract-key")
            .env("EE_REFLECTION_HMAC_KEY_PATH", hmac_key_path)
            .output()
            .map_err(|error| {
                format!("failed to run recommendation command `{command}`: {error}")
            })?;
        stdout_json(&output, "knowledgeGaps reflect propose recommendation")
    }

    fn storage_counts(&self) -> Result<(usize, usize), String> {
        let connection = self.connection()?;
        let memory_count = connection
            .list_memories(&self.workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .len();
        let link_count = connection
            .list_all_memory_links(None)
            .map_err(|error| error.to_string())?
            .len();
        Ok((memory_count, link_count))
    }
}

fn normalize_insights_json_for_determinism(mut json: Value) -> Value {
    if let Some(data) = json.get_mut("data").and_then(Value::as_object_mut) {
        data.insert(
            "generatedAt".to_owned(),
            Value::String("<normalized>".to_owned()),
        );
        data.insert(
            "runDurationMs".to_owned(),
            Value::Number(serde_json::Number::from(0)),
        );
    }
    json
}

fn assert_knowledge_gap_item_contract(item: &Value) -> TestResult {
    for field in [
        "category",
        "sourceMemoryIds",
        "metricEvidence",
        "explanation",
        "confidence",
        "priority",
        "recommendation",
    ] {
        if item.get(field).is_none() {
            return Err(format!("knowledge gap item missing {field}: {item}"));
        }
    }
    let source_ids = item
        .get("sourceMemoryIds")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("sourceMemoryIds must be an array: {item}"))?;
    if source_ids.is_empty() {
        return Err(format!("knowledge gap item must cite sources: {item}"));
    }
    ensure_eq(
        item.pointer("/metricEvidence/schema")
            .and_then(Value::as_str),
        Some("ee.graph.knowledge_gap.v1"),
        "knowledgeGaps item",
        "metricEvidence.schema",
    )?;
    if item
        .pointer("/metricEvidence/signal")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(format!("knowledge gap item missing metric signal: {item}"));
    }
    ensure_eq(
        item.pointer("/recommendation/kind").and_then(Value::as_str),
        Some("reflect_propose"),
        "knowledgeGaps item",
        "recommendation.kind",
    )
}

fn assert_knowledge_gap_compact_recommendation_contract(
    item: &Value,
    recommendation: &Value,
) -> TestResult {
    ensure_eq(
        recommendation.get("id"),
        item.get("gapId"),
        "knowledgeGaps compact recommendation",
        "id",
    )?;
    ensure_eq(
        recommendation.get("reason"),
        item.get("explanation"),
        "knowledgeGaps compact recommendation",
        "reason",
    )?;
    ensure_eq(
        recommendation
            .get("recommendation_kind")
            .and_then(Value::as_str),
        Some("reflect_propose"),
        "knowledgeGaps compact recommendation",
        "recommendation_kind",
    )?;
    ensure_eq(
        recommendation.get("suggested_query"),
        item.pointer("/recommendation/command"),
        "knowledgeGaps compact recommendation",
        "suggested_query",
    )
}

fn assert_knowledge_gap_section_empty(json: &Value, label: &str) -> TestResult {
    ensure_eq(
        json.pointer("/data/selectedSection")
            .and_then(Value::as_str),
        Some("knowledgeGaps"),
        label,
        "selectedSection",
    )?;
    ensure_eq(
        json.pointer("/data/sections/0/section")
            .and_then(Value::as_str),
        Some("knowledgeGaps"),
        label,
        "section",
    )?;
    let items = json
        .pointer("/data/sections/0/items")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label} knowledgeGaps items must be an array"))?;
    let recommendations = json
        .pointer("/data/sections/0/recommendations")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label} knowledgeGaps recommendations must be an array"))?;
    if !items.is_empty() || !recommendations.is_empty() {
        return Err(format!(
            "{label} should not emit knowledge gaps or recommendations: items={items:?}, recommendations={recommendations:?}"
        ));
    }
    Ok(())
}

fn recommendation_command_args(command: &str) -> Result<Vec<String>, String> {
    let mut parts = command
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.first().map(String::as_str) != Some("ee") {
        return Err(format!(
            "recommendation command must start with `ee`: {command}"
        ));
    }
    parts.remove(0);
    Ok(parts)
}

fn reflect_propose_source_memory_args(command: &str) -> Result<Vec<String>, String> {
    let args = recommendation_command_args(command)?;
    if args.iter().any(|arg| arg == "ingest" || arg == "apply") {
        return Err(format!(
            "recommendation command must not ingest/apply reflection output: {command}"
        ));
    }
    let mut sources = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--source-memory" {
            let source = iter.next().ok_or_else(|| {
                format!("recommendation command has --source-memory without value: {command}")
            })?;
            sources.push(source.clone());
        }
    }
    if sources.is_empty() {
        return Err(format!(
            "recommendation command must include at least one --source-memory: {command}"
        ));
    }
    Ok(sources)
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
