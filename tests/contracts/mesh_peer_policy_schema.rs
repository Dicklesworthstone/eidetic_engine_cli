//! Contract checks for mesh peer authorization and redaction policy fixtures.

use std::fs;
use std::path::PathBuf;

use ee::models::{
    KNOWN_SCHEMAS, MESH_PEER_POLICY_SCHEMA_V1, MESH_POLICY_DECISION_SCHEMA_V1,
    MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1, MESH_STORAGE_STATUS_SCHEMA_V1,
};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.mesh.peer_policy.v1.json";
const DECISION_SCHEMA_PATH: &str = "docs/schemas/ee.mesh.policy_decision.v1.json";
const FAILURE_SURFACE_SCHEMA_PATH: &str = "docs/schemas/ee.mesh.policy_failure_surface.v1.json";
const STORAGE_STATUS_SCHEMA_PATH: &str = "docs/schemas/ee.mesh.storage_status.v1.json";
const FIXTURES: &[&str] = &[
    "tests/fixtures/mesh/peer_policy_metadata_only.json",
    "tests/fixtures/mesh/peer_policy_body_denied.json",
    "tests/fixtures/mesh/peer_policy_redacted_body_allowed.json",
];
const FAILURE_SURFACE_FIXTURES: &[&str] = &[
    "tests/fixtures/mesh/peer_policy_failure_surface_denied.json",
    "tests/fixtures/mesh/peer_policy_failure_surface_outbound_denied.json",
];
const DECISION_FIXTURES: &[&str] = &[
    "tests/fixtures/mesh/peer_policy_decision_inbound_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_inbound_denied.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_redacted_body_allowed.json",
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

fn ensure(condition: bool, context: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.into())
    }
}

fn ensure_schema_registered(schema_id: &str, supported_name: &str) -> TestResult {
    ensure(
        KNOWN_SCHEMAS.contains(&schema_id),
        format!("KNOWN_SCHEMAS missing {schema_id}"),
    )?;

    let supported = ee::core::supported_schemas()
        .into_iter()
        .map(|schema| (schema.name, schema.schema))
        .collect::<Vec<_>>();
    ensure(
        supported
            .iter()
            .any(|(name, schema)| *name == supported_name && *schema == schema_id),
        format!("supported_schemas missing {supported_name}={schema_id}"),
    )?;

    ensure(
        ee::output::public_schemas()
            .iter()
            .any(|entry| entry.id == schema_id),
        format!("public_schemas missing {schema_id}"),
    )
}

#[test]
fn peer_policy_schema_pins_default_deny_and_trust_boundaries() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;

    ensure_equal(
        &schema.pointer("/$schema").and_then(Value::as_str),
        &Some("https://json-schema.org/draft/2020-12/schema"),
        "json schema draft",
    )?;
    ensure_equal(
        &schema.pointer("/$id").and_then(Value::as_str),
        &Some("https://eidetic-engine/schemas/ee.mesh.peer_policy.v1.json"),
        "schema id",
    )?;
    ensure_equal(
        &schema.pointer("/title").and_then(Value::as_str),
        &Some(MESH_PEER_POLICY_SCHEMA_V1),
        "schema title",
    )?;
    ensure_schema_registered(MESH_PEER_POLICY_SCHEMA_V1, "mesh_peer_policy")?;
    ensure_equal(
        &schema
            .pointer("/properties/defaultAction/const")
            .and_then(Value::as_str),
        &Some("deny"),
        "default deny",
    )?;

    let import_trust = schema
        .pointer("/properties/importTrustClass/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "importTrustClass enum missing".to_string())?;
    ensure(
        !import_trust
            .iter()
            .any(|value| value.as_str() == Some("human_explicit")),
        "peer policy must not allow peer material to import as human_explicit",
    )?;

    let trust_lanes = schema
        .pointer("/$defs/trustLane/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "trustLane enum missing".to_string())?;
    for lane in [
        "localHuman",
        "peerHumanViaPeer",
        "peerAgent",
        "peerDerived",
        "untrusted",
    ] {
        ensure(
            trust_lanes.iter().any(|value| value.as_str() == Some(lane)),
            format!("trust lane {lane} missing"),
        )?;
    }
    Ok(())
}

#[test]
fn peer_policy_fixtures_are_redaction_safe_and_never_human_explicit() -> TestResult {
    for fixture in FIXTURES {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_PEER_POLICY_SCHEMA_V1),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/defaultAction").and_then(Value::as_str),
            &Some("deny"),
            fixture,
        )?;
        ensure(
            value.pointer("/importTrustClass").and_then(Value::as_str) != Some("human_explicit"),
            format!("{fixture} imports peer material as human_explicit"),
        )?;
        for field in ["workspaceId", "peerId", "policyId"] {
            let text = value
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{fixture} missing {field}"))?;
            ensure(
                !text.contains('/') && !text.contains('\\'),
                format!("{fixture} {field} contains raw path separator"),
            )?;
        }
    }
    Ok(())
}

#[test]
fn peer_policy_failure_surface_schema_pins_structured_codes() -> TestResult {
    let schema = read_json(FAILURE_SURFACE_SCHEMA_PATH)?;

    ensure_equal(
        &schema.pointer("/$schema").and_then(Value::as_str),
        &Some("https://json-schema.org/draft/2020-12/schema"),
        "json schema draft",
    )?;
    ensure_equal(
        &schema.pointer("/$id").and_then(Value::as_str),
        &Some("https://eidetic-engine/schemas/ee.mesh.policy_failure_surface.v1.json"),
        "schema id",
    )?;
    ensure_equal(
        &schema.pointer("/title").and_then(Value::as_str),
        &Some(MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1),
        "schema title",
    )?;
    ensure_schema_registered(
        MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1,
        "mesh_policy_failure_surface",
    )?;

    let codes = schema
        .pointer("/properties/code/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "code enum missing".to_string())?;
    for code in [
        "mesh_peer_policy_denied",
        "mesh_peer_policy_quarantined",
        "mesh_peer_policy_rejected",
        "mesh_outbound_policy_denied",
        "mesh_outbound_policy_quarantined",
        "mesh_outbound_policy_rejected",
    ] {
        ensure(
            codes.iter().any(|value| value.as_str() == Some(code)),
            format!("failure code {code} missing"),
        )?;
    }

    let actions = schema
        .pointer("/properties/action/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "action enum missing".to_string())?;
    ensure(
        !actions.iter().any(|value| value.as_str() == Some("allow")),
        "failure surface must not include allow action",
    )
}

#[test]
fn peer_policy_decision_schema_pins_directional_side_effect_fields() -> TestResult {
    let schema = read_json(DECISION_SCHEMA_PATH)?;

    ensure_equal(
        &schema.pointer("/$schema").and_then(Value::as_str),
        &Some("https://json-schema.org/draft/2020-12/schema"),
        "json schema draft",
    )?;
    ensure_equal(
        &schema.pointer("/$id").and_then(Value::as_str),
        &Some("https://eidetic-engine/schemas/ee.mesh.policy_decision.v1.json"),
        "schema id",
    )?;
    ensure_equal(
        &schema.pointer("/title").and_then(Value::as_str),
        &Some(MESH_POLICY_DECISION_SCHEMA_V1),
        "schema title",
    )?;
    ensure_schema_registered(MESH_POLICY_DECISION_SCHEMA_V1, "mesh_policy_decision")?;

    let actions = schema
        .pointer("/properties/action/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "policy decision action enum missing".to_string())?;
    for action in ["allow", "deny", "quarantine", "reject"] {
        ensure(
            actions.iter().any(|value| value.as_str() == Some(action)),
            format!("policy decision action {action} missing"),
        )?;
    }

    let import_trust = schema
        .pointer("/properties/importTrustClass/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "importTrustClass enum missing".to_string())?;
    ensure(
        !import_trust
            .iter()
            .any(|value| value.as_str() == Some("human_explicit")),
        "policy decision must not allow peer material to import as human_explicit",
    )?;

    for field in [
        "bodyFetchAllowed",
        "localTruthSideEffectsAllowed",
        "searchOrGraphSideEffectsAllowed",
        "payloadExportAllowed",
        "rawPayloadExportAllowed",
        "redactedPayloadRequired",
    ] {
        ensure(
            schema.pointer(&format!("/properties/{field}")).is_some(),
            format!("policy decision schema missing {field}"),
        )?;
    }
    Ok(())
}

#[test]
fn peer_policy_failure_surface_fixtures_are_redaction_safe() -> TestResult {
    for fixture in FAILURE_SURFACE_FIXTURES {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1),
            fixture,
        )?;
        ensure(
            value.pointer("/action").and_then(Value::as_str) != Some("allow"),
            format!("{fixture} is not a failure surface"),
        )?;
        for field in ["policyRef", "reason"] {
            let text = value
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{fixture} missing {field}"))?;
            ensure(
                !text.contains('/') && !text.contains('\\'),
                format!("{fixture} {field} contains raw path separator"),
            )?;
        }
    }
    Ok(())
}

#[test]
fn peer_policy_decision_fixtures_are_redaction_safe_and_directional() -> TestResult {
    for fixture in DECISION_FIXTURES {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_POLICY_DECISION_SCHEMA_V1),
            fixture,
        )?;
        for field in ["policyRef", "reason"] {
            let text = value
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{fixture} missing {field}"))?;
            ensure(
                !text.contains('/') && !text.contains('\\'),
                format!("{fixture} {field} contains raw path separator"),
            )?;
        }

        match value.pointer("/direction").and_then(Value::as_str) {
            Some("inbound") => {
                ensure(
                    value.get("importTrustClass").is_some()
                        && value.get("bodyFetchAllowed").is_some()
                        && value.get("localTruthSideEffectsAllowed").is_some()
                        && value.get("searchOrGraphSideEffectsAllowed").is_some(),
                    format!("{fixture} missing inbound side-effect fields"),
                )?;
                ensure(
                    value.get("payloadExportAllowed").is_none()
                        && value.get("rawPayloadExportAllowed").is_none()
                        && value.get("redactedPayloadRequired").is_none(),
                    format!("{fixture} mixes outbound fields into inbound decision"),
                )?;
            }
            Some("outbound") => {
                ensure(
                    value.get("payloadExportAllowed").is_some()
                        && value.get("rawPayloadExportAllowed").is_some()
                        && value.get("redactedPayloadRequired").is_some(),
                    format!("{fixture} missing outbound export fields"),
                )?;
                ensure(
                    value.get("importTrustClass").is_none()
                        && value.get("bodyFetchAllowed").is_none()
                        && value.get("localTruthSideEffectsAllowed").is_none()
                        && value.get("searchOrGraphSideEffectsAllowed").is_none(),
                    format!("{fixture} mixes inbound fields into outbound decision"),
                )?;
            }
            other => {
                return Err(format!(
                    "{fixture} has invalid decision direction {other:?}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn peer_policy_failure_surface_fixtures_pin_inbound_and_outbound_codes() -> TestResult {
    let inbound = read_json("tests/fixtures/mesh/peer_policy_failure_surface_denied.json")?;
    ensure_equal(
        &inbound.pointer("/code").and_then(Value::as_str),
        &Some("mesh_peer_policy_denied"),
        "inbound failure code",
    )?;
    ensure_equal(
        &inbound.pointer("/reason").and_then(Value::as_str),
        &Some("peer_policy_redaction_denied"),
        "inbound reason",
    )?;

    let outbound =
        read_json("tests/fixtures/mesh/peer_policy_failure_surface_outbound_denied.json")?;
    ensure_equal(
        &outbound.pointer("/code").and_then(Value::as_str),
        &Some("mesh_outbound_policy_denied"),
        "outbound failure code",
    )?;
    ensure_equal(
        &outbound.pointer("/reason").and_then(Value::as_str),
        &Some("outbound_payload_requires_redaction"),
        "outbound reason",
    )?;
    ensure_equal(
        &outbound.pointer("/materialLane").and_then(Value::as_str),
        &Some("embedding"),
        "outbound material lane",
    )?;
    ensure_equal(
        &outbound.pointer("/redaction").and_then(Value::as_str),
        &Some("redact"),
        "outbound redaction posture",
    )
}

#[test]
fn peer_policy_decision_fixture_pins_nested_inbound_failure() -> TestResult {
    let value = read_json("tests/fixtures/mesh/peer_policy_decision_inbound_denied.json")?;
    ensure_equal(
        &value.pointer("/schema").and_then(Value::as_str),
        &Some(MESH_POLICY_DECISION_SCHEMA_V1),
        "decision schema",
    )?;
    ensure_equal(
        &value.pointer("/direction").and_then(Value::as_str),
        &Some("inbound"),
        "decision direction",
    )?;
    ensure_equal(
        &value.pointer("/action").and_then(Value::as_str),
        &Some("deny"),
        "decision action",
    )?;
    ensure_equal(
        &value
            .pointer("/localTruthSideEffectsAllowed")
            .and_then(Value::as_bool),
        &Some(false),
        "local truth side effects",
    )?;
    ensure_equal(
        &value
            .pointer("/searchOrGraphSideEffectsAllowed")
            .and_then(Value::as_bool),
        &Some(false),
        "search or graph side effects",
    )?;

    let failure = value
        .pointer("/failure")
        .ok_or_else(|| "denied decision missing nested failure".to_owned())?;
    ensure_equal(
        &failure.pointer("/schema").and_then(Value::as_str),
        &Some(MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1),
        "failure schema",
    )?;
    ensure_equal(
        &failure.pointer("/code").and_then(Value::as_str),
        &Some("mesh_peer_policy_denied"),
        "failure code",
    )?;
    ensure_equal(
        &failure.pointer("/reason").and_then(Value::as_str),
        &value.pointer("/reason").and_then(Value::as_str),
        "failure reason mirrors decision reason",
    )?;
    ensure_equal(
        &failure.pointer("/policyRef").and_then(Value::as_str),
        &value.pointer("/policyRef").and_then(Value::as_str),
        "failure policy ref mirrors decision policy ref",
    )
}

#[test]
fn metadata_only_policy_denies_body_embedding_and_body_fetch() -> TestResult {
    let value = read_json("tests/fixtures/mesh/peer_policy_metadata_only.json")?;

    ensure_equal(
        &value
            .pointer("/allowedLanes/metadata")
            .and_then(Value::as_str),
        &Some("allow"),
        "metadata lane",
    )?;
    ensure_equal(
        &value.pointer("/allowedLanes/body").and_then(Value::as_str),
        &Some("deny"),
        "body lane",
    )?;
    ensure_equal(
        &value
            .pointer("/allowedLanes/embedding")
            .and_then(Value::as_str),
        &Some("deny"),
        "embedding lane",
    )?;
    ensure_equal(
        &value.pointer("/redaction/body").and_then(Value::as_str),
        &Some("deny"),
        "body redaction",
    )?;
    ensure_equal(
        &value
            .pointer("/redaction/embedding")
            .and_then(Value::as_str),
        &Some("deny"),
        "embedding redaction",
    )?;
    ensure_equal(
        &value.pointer("/bodyFetch/allowed").and_then(Value::as_bool),
        &Some(false),
        "body fetch allowed",
    )
}

#[test]
fn body_denied_policy_keeps_peer_agent_below_body_lane() -> TestResult {
    let value = read_json("tests/fixtures/mesh/peer_policy_body_denied.json")?;

    ensure_equal(
        &value.pointer("/trustLane").and_then(Value::as_str),
        &Some("peerAgent"),
        "trust lane",
    )?;
    ensure_equal(
        &value.pointer("/importTrustClass").and_then(Value::as_str),
        &Some("agent_validated"),
        "import trust class",
    )?;
    ensure_equal(
        &value.pointer("/allowedLanes/body").and_then(Value::as_str),
        &Some("deny"),
        "body lane",
    )?;
    ensure_equal(
        &value.pointer("/bodyFetch/allowed").and_then(Value::as_bool),
        &Some(false),
        "body fetch remains denied",
    )
}

#[test]
fn redacted_body_policy_allows_body_only_with_redaction_and_consent() -> TestResult {
    let value = read_json("tests/fixtures/mesh/peer_policy_redacted_body_allowed.json")?;

    ensure_equal(
        &value.pointer("/allowedLanes/body").and_then(Value::as_str),
        &Some("allow"),
        "body lane",
    )?;
    ensure_equal(
        &value.pointer("/redaction/body").and_then(Value::as_str),
        &Some("redact"),
        "body redaction posture",
    )?;
    ensure_equal(
        &value
            .pointer("/allowedLanes/embedding")
            .and_then(Value::as_str),
        &Some("deny"),
        "embedding lane remains denied",
    )?;
    ensure_equal(
        &value
            .pointer("/redaction/embedding")
            .and_then(Value::as_str),
        &Some("deny"),
        "embedding redaction remains denied",
    )?;
    ensure_equal(
        &value.pointer("/bodyFetch/allowed").and_then(Value::as_bool),
        &Some(true),
        "body fetch allowed",
    )?;
    ensure_equal(
        &value
            .pointer("/bodyFetch/requiresConsent")
            .and_then(Value::as_bool),
        &Some(true),
        "body fetch consent",
    )?;
    ensure_equal(
        &value.pointer("/bodyFetch/maxBytes").and_then(Value::as_u64),
        &Some(4096),
        "body fetch max bytes",
    )
}
