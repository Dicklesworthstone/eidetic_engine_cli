//! Contract checks for workspace-scoped mesh peer-group bindings.

use std::fs;
use std::path::PathBuf;

use ee::config::{ConfigFile, MeshLane, MeshLaneDecision};
use ee::models::{KNOWN_SCHEMAS, MESH_PEER_GROUP_BINDING_SCHEMA_V1};
use serde_json::Value;

type TestResult = Result<(), String>;

const FIXTURES: &[&str] = &[
    "tests/fixtures/mesh/peer_group_binding_valid.json",
    "tests/fixtures/mesh/peer_group_binding_deny_body.json",
    "tests/fixtures/mesh/peer_group_binding_quarantine_curation.json",
    "tests/fixtures/mesh/peer_group_binding_missing_lane_default_deny.json",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn peer_group_binding_schema_is_documented_and_registered() -> TestResult {
    let schema = read_json("docs/schemas/ee.mesh.peer_group_binding.v1.json")?;

    ensure_equal(
        &schema.pointer("/$schema").and_then(Value::as_str),
        &Some("https://json-schema.org/draft/2020-12/schema"),
        "json schema draft",
    )?;
    ensure_equal(
        &schema.pointer("/$id").and_then(Value::as_str),
        &Some("https://eidetic-engine/schemas/ee.mesh.peer_group_binding.v1.json"),
        "schema id",
    )?;
    ensure_equal(
        &schema.pointer("/title").and_then(Value::as_str),
        &Some(MESH_PEER_GROUP_BINDING_SCHEMA_V1),
        "schema title",
    )?;
    ensure_equal(
        &schema
            .pointer("/properties/defaultAction/const")
            .and_then(Value::as_str),
        &Some("deny"),
        "default action const",
    )?;
    ensure_equal(
        &KNOWN_SCHEMAS.contains(&MESH_PEER_GROUP_BINDING_SCHEMA_V1),
        &true,
        "known schema registry",
    )?;

    let supported = ee::core::supported_schemas()
        .into_iter()
        .map(|schema| schema.schema)
        .collect::<Vec<_>>();
    ensure_equal(
        &supported.contains(&MESH_PEER_GROUP_BINDING_SCHEMA_V1),
        &true,
        "supported schema registry",
    )
}

#[test]
fn peer_group_binding_fixtures_are_redaction_safe_and_use_known_lanes() -> TestResult {
    for fixture in FIXTURES {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_PEER_GROUP_BINDING_SCHEMA_V1),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/defaultAction").and_then(Value::as_str),
            &Some("deny"),
            fixture,
        )?;
        for field in ["workspaceId", "peerGroupId"] {
            let text = value
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{fixture} missing {field}"))?;
            if text.contains('/') || text.contains('\\') {
                return Err(format!("{fixture} {field} contains raw path separator"));
            }
        }
        let lanes = value
            .get("lanes")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{fixture} missing lanes object"))?;
        for (lane, decision) in lanes {
            if ![
                "metadata",
                "body",
                "embedding",
                "graphLink",
                "revisionNotice",
                "curationSignal",
            ]
            .contains(&lane.as_str())
            {
                return Err(format!("{fixture} contains unknown lane {lane}"));
            }
            if !["allow", "quarantine", "deny"].contains(&decision.as_str().unwrap_or("")) {
                return Err(format!("{fixture} contains invalid decision for {lane}"));
            }
        }
    }
    Ok(())
}

#[test]
fn config_peer_group_binding_defaults_to_deny_across_workspace_and_lanes() -> TestResult {
    let config = ConfigFile::parse(
        r#"
[[mesh.peer_group_bindings]]
workspace_id = "wsp_workspace_a_001"
workspace_alias = "workspace-a"
peer_group_id = "pg_team_alpha_001"
peer_ids = ["peer_agent_001"]
origin_workspace_ids = ["wsp_origin_001"]
default_action = "deny"

[mesh.peer_group_bindings.lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
"#,
    )
    .map_err(|error| format!("config should parse: {error}"))?;
    let binding = config
        .mesh
        .peer_group_bindings
        .as_ref()
        .and_then(|bindings| bindings.first())
        .ok_or_else(|| "expected peer-group binding".to_string())?;

    ensure_equal(
        &binding.decision_for(
            "wsp_workspace_a_001",
            "peer_agent_001",
            "wsp_origin_001",
            MeshLane::Metadata,
        ),
        &MeshLaneDecision::Allow,
        "explicit metadata grant",
    )?;
    ensure_equal(
        &binding.decision_for(
            "wsp_workspace_b_001",
            "peer_agent_001",
            "wsp_origin_001",
            MeshLane::Metadata,
        ),
        &MeshLaneDecision::Deny,
        "same peer denied in workspace B",
    )?;
    ensure_equal(
        &binding.decision_for(
            "wsp_workspace_a_001",
            "peer_agent_001",
            "wsp_unknown_origin_001",
            MeshLane::Metadata,
        ),
        &MeshLaneDecision::Deny,
        "unknown origin workspace denied",
    )?;
    ensure_equal(
        &binding.decision_for(
            "wsp_workspace_a_001",
            "peer_agent_001",
            "wsp_origin_001",
            MeshLane::CurationSignal,
        ),
        &MeshLaneDecision::Deny,
        "missing lane denied",
    )
}
