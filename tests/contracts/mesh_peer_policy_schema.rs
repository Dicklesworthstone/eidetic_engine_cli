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
    "tests/fixtures/mesh/peer_policy_failure_surface_quarantined.json",
    "tests/fixtures/mesh/peer_policy_failure_surface_rejected.json",
    "tests/fixtures/mesh/peer_policy_failure_surface_outbound_denied.json",
    "tests/fixtures/mesh/peer_policy_failure_surface_outbound_quarantined.json",
    "tests/fixtures/mesh/peer_policy_failure_surface_outbound_rejected.json",
];
const DECISION_FIXTURES: &[&str] = &[
    "tests/fixtures/mesh/peer_policy_decision_inbound_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_inbound_redacted_body_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_inbound_denied.json",
    "tests/fixtures/mesh/peer_policy_decision_inbound_quarantined.json",
    "tests/fixtures/mesh/peer_policy_decision_inbound_rejected.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_metadata_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_revision_notice_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_redacted_body_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_redacted_embedding_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_shared_body_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_shared_embedding_allowed.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_denied.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_quarantined.json",
    "tests/fixtures/mesh/peer_policy_decision_outbound_rejected.json",
];
const DISALLOWED_PEER_IMPORT_TRUST_CLASSES: &[&str] =
    &["human_explicit", "cass_evidence", "legacy_import"];

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

fn ensure_not_disallowed_peer_import_trust_class(
    actual: Option<&str>,
    context: &str,
) -> TestResult {
    for disallowed_class in DISALLOWED_PEER_IMPORT_TRUST_CLASSES {
        ensure(
            actual != Some(*disallowed_class),
            format!("{context} imports peer material as {disallowed_class}"),
        )?;
    }
    Ok(())
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
    for disallowed_class in DISALLOWED_PEER_IMPORT_TRUST_CLASSES {
        ensure(
            !import_trust
                .iter()
                .any(|value| value.as_str() == Some(*disallowed_class)),
            format!("peer policy must not allow peer material to import as {disallowed_class}"),
        )?;
    }

    let trust_lanes = schema
        .pointer("/$defs/trustLane/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "trustLane enum missing".to_string())?;
    for lane in ["peerHumanViaPeer", "peerAgent", "peerDerived", "untrusted"] {
        ensure(
            trust_lanes.iter().any(|value| value.as_str() == Some(lane)),
            format!("trust lane {lane} missing"),
        )?;
    }
    ensure(
        !trust_lanes
            .iter()
            .any(|value| value.as_str() == Some("localHuman")),
        "peer policy schema must reject localHuman trustLane assignments",
    )?;
    Ok(())
}

#[test]
fn peer_policy_fixtures_are_redaction_safe_and_peer_import_safe() -> TestResult {
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
        ensure_not_disallowed_peer_import_trust_class(
            value.pointer("/importTrustClass").and_then(Value::as_str),
            fixture,
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
    )?;

    let code_action_invariants = schema
        .pointer("/allOf")
        .and_then(Value::as_array)
        .ok_or_else(|| "failure surface code/action invariants missing".to_string())?;
    let cases = [
        (
            0,
            ["mesh_peer_policy_denied", "mesh_outbound_policy_denied"],
            "deny",
        ),
        (
            1,
            [
                "mesh_peer_policy_quarantined",
                "mesh_outbound_policy_quarantined",
            ],
            "quarantine",
        ),
        (
            2,
            ["mesh_peer_policy_rejected", "mesh_outbound_policy_rejected"],
            "reject",
        ),
    ];
    for (index, expected_codes, expected_action) in cases {
        let invariant = code_action_invariants
            .get(index)
            .ok_or_else(|| format!("failure surface invariant {index} missing"))?;
        let invariant_codes = invariant
            .pointer("/if/properties/code/enum")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("failure surface invariant {index} code enum missing"))?;
        for code in expected_codes {
            ensure(
                invariant_codes
                    .iter()
                    .any(|value| value.as_str() == Some(code)),
                format!("failure surface invariant {index} missing code {code}"),
            )?;
        }
        ensure_equal(
            &invariant
                .pointer("/then/properties/action/const")
                .and_then(Value::as_str),
            &Some(expected_action),
            &format!("failure surface invariant {index} action"),
        )?;
    }

    Ok(())
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
    for allowed_class in ["agent_validated", "agent_assertion"] {
        ensure(
            import_trust
                .iter()
                .any(|value| value.as_str() == Some(allowed_class)),
            format!("policy decision importTrustClass missing {allowed_class}"),
        )?;
    }
    ensure(
        import_trust.iter().any(Value::is_null),
        "policy decision importTrustClass must permit null when no policy applies",
    )?;
    for disallowed_class in DISALLOWED_PEER_IMPORT_TRUST_CLASSES {
        ensure(
            !import_trust
                .iter()
                .any(|value| value.as_str() == Some(*disallowed_class)),
            format!("policy decision must not allow peer material to import as {disallowed_class}"),
        )?;
    }

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

    ensure_equal(
        &schema
            .pointer("/allOf/2/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("inbound"),
        "inbound body allow invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/2/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "inbound body allow invariant action",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/2/if/properties/materialLane/const")
            .and_then(Value::as_str),
        &Some("body"),
        "inbound body allow invariant lane",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/2/then/properties/bodyFetchAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "inbound allowed body decisions must permit body fetch",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/2/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "inbound allowed body decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/3/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound redacted body invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/3/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "outbound redacted body invariant action",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/3/if/properties/materialLane/const")
            .and_then(Value::as_str),
        &Some("body"),
        "outbound redacted body invariant lane",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/3/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("redact"),
        "outbound redacted body invariant redaction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/3/then/properties/payloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound redacted body decisions may export only redacted payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/3/then/properties/rawPayloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(false),
        "outbound redacted body decisions must not export raw payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/3/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound redacted body decisions must require redacted payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/3/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "outbound redacted body allow decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/4/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound redacted embedding invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/4/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "outbound redacted embedding invariant action",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/4/if/properties/materialLane/const")
            .and_then(Value::as_str),
        &Some("embedding"),
        "outbound redacted embedding invariant lane",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/4/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("redact"),
        "outbound redacted embedding invariant redaction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/4/then/properties/payloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound redacted embedding decisions may export only redacted payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/4/then/properties/rawPayloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(false),
        "outbound redacted embedding decisions must not export raw payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/4/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound redacted embedding decisions must require redacted payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/4/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "outbound redacted embedding allow decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/5/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound shared body invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/5/if/properties/materialLane/const")
            .and_then(Value::as_str),
        &Some("body"),
        "outbound shared body invariant lane",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/5/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("share"),
        "outbound shared body invariant redaction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/5/then/properties/payloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound shared body decisions may export payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/5/then/properties/rawPayloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound shared body decisions explicitly permit raw payload export",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/5/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(false),
        "outbound shared body decisions must not claim redaction is required",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/5/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "outbound shared body allow decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/6/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound shared embedding invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/6/if/properties/materialLane/const")
            .and_then(Value::as_str),
        &Some("embedding"),
        "outbound shared embedding invariant lane",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/6/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("share"),
        "outbound shared embedding invariant redaction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/6/then/properties/payloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound shared embedding decisions may export payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/6/then/properties/rawPayloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound shared embedding decisions explicitly permit raw payload export",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/6/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(false),
        "outbound shared embedding decisions must not claim redaction is required",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/6/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "outbound shared embedding allow decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/7/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound shared metadata invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/7/if/properties/materialLane/const")
            .and_then(Value::as_str),
        &Some("metadata"),
        "outbound shared metadata invariant lane",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/7/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("share"),
        "outbound shared metadata invariant redaction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/7/then/properties/payloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound shared metadata decisions may export payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/7/then/properties/rawPayloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound shared metadata decisions explicitly permit raw payload export",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/7/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(false),
        "outbound shared metadata decisions must not claim redaction is required",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/7/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "outbound shared metadata allow decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/8/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound shared revision notice invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/8/if/properties/materialLane/const")
            .and_then(Value::as_str),
        &Some("revisionNotice"),
        "outbound shared revision notice invariant lane",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/8/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("share"),
        "outbound shared revision notice invariant redaction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/8/then/properties/payloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound shared revision notice decisions may export payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/8/then/properties/rawPayloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound shared revision notice decisions explicitly permit raw payload export",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/8/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(false),
        "outbound shared revision notice decisions must not claim redaction is required",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/8/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "outbound shared revision notice allow decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/9/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "allow decision invariant action",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/9/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "allowed decisions must not carry failure",
    )?;
    let non_allow_actions = schema
        .pointer("/allOf/10/if/properties/action/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "non-allow action invariant enum missing".to_string())?;
    for action in ["deny", "quarantine", "reject"] {
        ensure(
            non_allow_actions
                .iter()
                .any(|value| value.as_str() == Some(action)),
            format!("non-allow action invariant missing {action}"),
        )?;
    }
    ensure_equal(
        &schema
            .pointer("/allOf/10/then/properties/failure/$ref")
            .and_then(Value::as_str),
        &Some("ee.mesh.policy_failure_surface.v1.json"),
        "non-allow decisions must carry structured failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/11/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("inbound"),
        "inbound non-allow side-effect invariant direction",
    )?;
    let inbound_non_allow_actions = schema
        .pointer("/allOf/11/if/properties/action/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "inbound non-allow action invariant enum missing".to_string())?;
    for action in ["deny", "quarantine", "reject"] {
        ensure(
            inbound_non_allow_actions
                .iter()
                .any(|value| value.as_str() == Some(action)),
            format!("inbound non-allow side-effect invariant missing {action}"),
        )?;
    }
    for field in [
        "bodyFetchAllowed",
        "localTruthSideEffectsAllowed",
        "searchOrGraphSideEffectsAllowed",
    ] {
        ensure_equal(
            &schema
                .pointer(&format!("/allOf/11/then/properties/{field}/const"))
                .and_then(Value::as_bool),
            &Some(false),
            &format!("inbound non-allow decisions must set {field}=false"),
        )?;
    }
    ensure_equal(
        &schema
            .pointer("/allOf/12/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound non-allow export invariant direction",
    )?;
    let outbound_non_allow_actions = schema
        .pointer("/allOf/12/if/properties/action/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "outbound non-allow action invariant enum missing".to_string())?;
    for action in ["deny", "quarantine", "reject"] {
        ensure(
            outbound_non_allow_actions
                .iter()
                .any(|value| value.as_str() == Some(action)),
            format!("outbound non-allow export invariant missing {action}"),
        )?;
    }
    for field in ["payloadExportAllowed", "rawPayloadExportAllowed"] {
        ensure_equal(
            &schema
                .pointer(&format!("/allOf/12/then/properties/{field}/const"))
                .and_then(Value::as_bool),
            &Some(false),
            &format!("outbound non-allow decisions must set {field}=false"),
        )?;
    }
    ensure_equal(
        &schema
            .pointer("/allOf/13/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("inbound"),
        "inbound allow import trust invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/13/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "inbound allow import trust invariant action",
    )?;
    let inbound_allow_import_trust = schema
        .pointer("/allOf/13/then/properties/importTrustClass/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "inbound allow import trust invariant enum missing".to_string())?;
    for allowed_class in ["agent_validated", "agent_assertion"] {
        ensure(
            inbound_allow_import_trust
                .iter()
                .any(|value| value.as_str() == Some(allowed_class)),
            format!("inbound allow import trust invariant missing {allowed_class}"),
        )?;
    }
    ensure(
        !inbound_allow_import_trust.iter().any(Value::is_null),
        "inbound allow decisions must use a concrete importTrustClass",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/14/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "allow trust lane invariant action",
    )?;
    let allow_trust_lanes = schema
        .pointer("/allOf/14/then/properties/trustLane/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "allow trust lane invariant enum missing".to_string())?;
    for trust_lane in ["peerHumanViaPeer", "peerAgent", "peerDerived", "untrusted"] {
        ensure(
            allow_trust_lanes
                .iter()
                .any(|value| value.as_str() == Some(trust_lane)),
            format!("allow trust lane invariant missing {trust_lane}"),
        )?;
    }
    ensure(
        !allow_trust_lanes
            .iter()
            .any(|value| value.as_str() == Some("localHuman")),
        "allowed decisions must not use localHuman trustLane",
    )?;
    ensure(
        !allow_trust_lanes.iter().any(Value::is_null),
        "allowed decisions must use a concrete trustLane",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/15/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("inbound"),
        "inbound allow non-body fetch invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/15/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "inbound allow non-body fetch invariant action",
    )?;
    let non_body_lanes = schema
        .pointer("/allOf/15/if/properties/materialLane/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "inbound allow non-body lane enum missing".to_string())?;
    for material_lane in [
        "metadata",
        "embedding",
        "graphLink",
        "revisionNotice",
        "curationSignal",
    ] {
        ensure(
            non_body_lanes
                .iter()
                .any(|value| value.as_str() == Some(material_lane)),
            format!("inbound allow non-body invariant missing {material_lane}"),
        )?;
    }
    ensure(
        !non_body_lanes
            .iter()
            .any(|value| value.as_str() == Some("body")),
        "inbound allow non-body invariant must not include body",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/15/then/properties/bodyFetchAllowed/const")
            .and_then(Value::as_bool),
        &Some(false),
        "inbound allowed non-body decisions must not permit body fetch",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/16/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "allow policy reference invariant action",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/16/then/properties/policyRef/not/const")
            .and_then(Value::as_str),
        &Some("missing"),
        "allowed decisions must not use the missing policy reference sentinel",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/17/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "allow redaction posture invariant action",
    )?;
    let allow_redactions = schema
        .pointer("/allOf/17/then/properties/redaction/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "allow redaction posture invariant enum missing".to_string())?;
    for redaction in ["share", "redact"] {
        ensure(
            allow_redactions
                .iter()
                .any(|value| value.as_str() == Some(redaction)),
            format!("allow redaction posture invariant missing {redaction}"),
        )?;
    }
    ensure(
        !allow_redactions
            .iter()
            .any(|value| value.as_str() == Some("deny")),
        "allowed decisions must not use the deny redaction posture",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/18/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("inbound"),
        "inbound allow side-effect invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/18/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "inbound allow side-effect invariant action",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/18/then/properties/localTruthSideEffectsAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "inbound allowed decisions must permit local-truth side effects",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/18/then/properties/searchOrGraphSideEffectsAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "inbound allowed decisions must permit search or graph side effects",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/19/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound allowed share export invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/19/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "outbound allowed share export invariant action",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/19/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("share"),
        "outbound allowed share export invariant redaction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/19/then/properties/payloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound allowed share decisions must permit payload export",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/19/then/properties/rawPayloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound allowed share decisions must permit raw payload export",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/19/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(false),
        "outbound allowed share decisions must not require redacted payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/19/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "outbound allowed share decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/20/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound allowed redact export invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/20/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "outbound allowed redact export invariant action",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/20/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("redact"),
        "outbound allowed redact export invariant redaction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/20/then/properties/payloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound allowed redact decisions must permit payload export",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/20/then/properties/rawPayloadExportAllowed/const")
            .and_then(Value::as_bool),
        &Some(false),
        "outbound allowed redact decisions must not permit raw payload export",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/20/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound allowed redact decisions must require redacted payloads",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/20/then/properties/failure/type")
            .and_then(Value::as_str),
        &Some("null"),
        "outbound allowed redact decisions must not carry failure",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/21/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("outbound"),
        "outbound redaction-required invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/21/if/properties/redaction/const")
            .and_then(Value::as_str),
        &Some("redact"),
        "outbound redaction-required invariant posture",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/21/then/properties/redactedPayloadRequired/const")
            .and_then(Value::as_bool),
        &Some(true),
        "outbound redacted decisions must report redacted payload requirement",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/22/if/properties/direction/const")
            .and_then(Value::as_str),
        &Some("inbound"),
        "inbound allow peer-safe trust lane invariant direction",
    )?;
    ensure_equal(
        &schema
            .pointer("/allOf/22/if/properties/action/const")
            .and_then(Value::as_str),
        &Some("allow"),
        "inbound allow peer-safe trust lane invariant action",
    )?;
    let inbound_allow_trust_lanes = schema
        .pointer("/allOf/22/then/properties/trustLane/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "inbound allow peer-safe trust lane enum missing".to_string())?;
    for trust_lane in ["peerHumanViaPeer", "peerAgent", "peerDerived", "untrusted"] {
        ensure(
            inbound_allow_trust_lanes
                .iter()
                .any(|value| value.as_str() == Some(trust_lane)),
            format!("inbound allow trust lane invariant missing {trust_lane}"),
        )?;
    }
    ensure(
        !inbound_allow_trust_lanes
            .iter()
            .any(|value| value.as_str() == Some("localHuman")),
        "inbound allowed peer decisions must not use localHuman trustLane",
    )?;
    ensure(
        !inbound_allow_trust_lanes.iter().any(Value::is_null),
        "inbound allowed peer decisions must use a concrete trustLane",
    )?;
    Ok(())
}

#[test]
fn mesh_storage_status_schema_pins_policy_decision_counts() -> TestResult {
    let schema = read_json(STORAGE_STATUS_SCHEMA_PATH)?;

    ensure_equal(
        &schema.pointer("/$schema").and_then(Value::as_str),
        &Some("https://json-schema.org/draft/2020-12/schema"),
        "json schema draft",
    )?;
    ensure_equal(
        &schema.pointer("/$id").and_then(Value::as_str),
        &Some("https://eidetic-engine/schemas/ee.mesh.storage_status.v1.json"),
        "schema id",
    )?;
    ensure_equal(
        &schema.pointer("/title").and_then(Value::as_str),
        &Some(MESH_STORAGE_STATUS_SCHEMA_V1),
        "schema title",
    )?;
    ensure_schema_registered(MESH_STORAGE_STATUS_SCHEMA_V1, "mesh_storage_status")?;

    let required = schema
        .pointer("/required")
        .and_then(Value::as_array)
        .ok_or_else(|| "storage status required fields missing".to_string())?;
    ensure(
        required
            .iter()
            .any(|value| value.as_str() == Some("policyDecisionEventCount")),
        "storage status schema must require policyDecisionEventCount",
    )?;
    ensure(
        required
            .iter()
            .any(|value| value.as_str() == Some("policyFailureEventCount")),
        "storage status schema must require policyFailureEventCount",
    )?;
    ensure(
        schema
            .pointer("/properties/policyDecisionEventCount/minimum")
            .and_then(Value::as_u64)
            == Some(0),
        "policyDecisionEventCount must be a non-negative counter",
    )?;
    ensure(
        schema
            .pointer("/properties/policyFailureEventCount/minimum")
            .and_then(Value::as_u64)
            == Some(0),
        "policyFailureEventCount must be a non-negative counter",
    )
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
    let cases = [
        (
            "tests/fixtures/mesh/peer_policy_failure_surface_denied.json",
            "mesh_peer_policy_denied",
            "deny",
            "peer_policy_redaction_denied",
            "body",
            "deny",
        ),
        (
            "tests/fixtures/mesh/peer_policy_failure_surface_quarantined.json",
            "mesh_peer_policy_quarantined",
            "quarantine",
            "peer_policy_lane_quarantined",
            "curationSignal",
            "share",
        ),
        (
            "tests/fixtures/mesh/peer_policy_failure_surface_rejected.json",
            "mesh_peer_policy_rejected",
            "reject",
            "peer_import_local_human_trust_lane",
            "metadata",
            "deny",
        ),
        (
            "tests/fixtures/mesh/peer_policy_failure_surface_outbound_denied.json",
            "mesh_outbound_policy_denied",
            "deny",
            "outbound_payload_requires_redaction",
            "embedding",
            "redact",
        ),
        (
            "tests/fixtures/mesh/peer_policy_failure_surface_outbound_quarantined.json",
            "mesh_outbound_policy_quarantined",
            "quarantine",
            "outbound_lane_quarantined",
            "curationSignal",
            "share",
        ),
        (
            "tests/fixtures/mesh/peer_policy_failure_surface_outbound_rejected.json",
            "mesh_outbound_policy_rejected",
            "reject",
            "non_deny_default_action",
            "metadata",
            "deny",
        ),
    ];

    for (fixture, code, action, reason, material_lane, redaction) in cases {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/code").and_then(Value::as_str),
            &Some(code),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/action").and_then(Value::as_str),
            &Some(action),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/reason").and_then(Value::as_str),
            &Some(reason),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/materialLane").and_then(Value::as_str),
            &Some(material_lane),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/redaction").and_then(Value::as_str),
            &Some(redaction),
            fixture,
        )?;
    }

    Ok(())
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
fn peer_policy_decision_fixtures_pin_inbound_non_allow_failures() -> TestResult {
    let cases = [
        (
            "tests/fixtures/mesh/peer_policy_decision_inbound_denied.json",
            "deny",
            "mesh_peer_policy_denied",
            "peer_policy_redaction_denied",
            "body",
            "deny",
            "peerAgent",
            "agent_validated",
        ),
        (
            "tests/fixtures/mesh/peer_policy_decision_inbound_quarantined.json",
            "quarantine",
            "mesh_peer_policy_quarantined",
            "peer_policy_lane_quarantined",
            "curationSignal",
            "share",
            "peerHumanViaPeer",
            "agent_validated",
        ),
        (
            "tests/fixtures/mesh/peer_policy_decision_inbound_rejected.json",
            "reject",
            "mesh_peer_policy_rejected",
            "peer_import_local_human_trust_lane",
            "metadata",
            "deny",
            "localHuman",
            "agent_validated",
        ),
    ];

    for (fixture, action, code, reason, material_lane, redaction, trust_lane, import_trust_class) in
        cases
    {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_POLICY_DECISION_SCHEMA_V1),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/direction").and_then(Value::as_str),
            &Some("inbound"),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/action").and_then(Value::as_str),
            &Some(action),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/reason").and_then(Value::as_str),
            &Some(reason),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/materialLane").and_then(Value::as_str),
            &Some(material_lane),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/redaction").and_then(Value::as_str),
            &Some(redaction),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/trustLane").and_then(Value::as_str),
            &Some(trust_lane),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/importTrustClass").and_then(Value::as_str),
            &Some(import_trust_class),
            fixture,
        )?;
        ensure_not_disallowed_peer_import_trust_class(
            value.pointer("/importTrustClass").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/bodyFetchAllowed").and_then(Value::as_bool),
            &Some(false),
            fixture,
        )?;
        ensure_equal(
            &value
                .pointer("/localTruthSideEffectsAllowed")
                .and_then(Value::as_bool),
            &Some(false),
            fixture,
        )?;
        ensure_equal(
            &value
                .pointer("/searchOrGraphSideEffectsAllowed")
                .and_then(Value::as_bool),
            &Some(false),
            fixture,
        )?;

        let failure = value
            .pointer("/failure")
            .ok_or_else(|| format!("{fixture} missing failure"))?;
        ensure_equal(
            &failure.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/code").and_then(Value::as_str),
            &Some(code),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/action").and_then(Value::as_str),
            &Some(action),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/reason").and_then(Value::as_str),
            &value.pointer("/reason").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/policyRef").and_then(Value::as_str),
            &value.pointer("/policyRef").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/materialLane").and_then(Value::as_str),
            &value.pointer("/materialLane").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/redaction").and_then(Value::as_str),
            &value.pointer("/redaction").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/trustLane").and_then(Value::as_str),
            &value.pointer("/trustLane").and_then(Value::as_str),
            fixture,
        )?;
    }

    Ok(())
}

#[test]
fn peer_policy_decision_fixture_pins_inbound_redacted_body_allow() -> TestResult {
    let value =
        read_json("tests/fixtures/mesh/peer_policy_decision_inbound_redacted_body_allowed.json")?;
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
        &Some("allow"),
        "decision action",
    )?;
    ensure_equal(
        &value.pointer("/materialLane").and_then(Value::as_str),
        &Some("body"),
        "material lane",
    )?;
    ensure_equal(
        &value.pointer("/redaction").and_then(Value::as_str),
        &Some("redact"),
        "redaction posture",
    )?;
    ensure_equal(
        &value.pointer("/importTrustClass").and_then(Value::as_str),
        &Some("agent_validated"),
        "import trust class",
    )?;
    ensure_not_disallowed_peer_import_trust_class(
        value.pointer("/importTrustClass").and_then(Value::as_str),
        "peer-imported redacted body",
    )?;
    ensure_equal(
        &value.pointer("/bodyFetchAllowed").and_then(Value::as_bool),
        &Some(true),
        "body fetch allowed",
    )?;
    ensure_equal(
        &value.pointer("/failure"),
        &Some(&Value::Null),
        "allowed redacted body decision must not include failure",
    )
}

#[test]
fn peer_policy_decision_fixture_pins_nested_outbound_failure() -> TestResult {
    let value = read_json("tests/fixtures/mesh/peer_policy_decision_outbound_denied.json")?;
    ensure_equal(
        &value.pointer("/schema").and_then(Value::as_str),
        &Some(MESH_POLICY_DECISION_SCHEMA_V1),
        "decision schema",
    )?;
    ensure_equal(
        &value.pointer("/direction").and_then(Value::as_str),
        &Some("outbound"),
        "decision direction",
    )?;
    ensure_equal(
        &value.pointer("/action").and_then(Value::as_str),
        &Some("deny"),
        "decision action",
    )?;
    ensure_equal(
        &value
            .pointer("/payloadExportAllowed")
            .and_then(Value::as_bool),
        &Some(false),
        "payload export allowed",
    )?;
    ensure_equal(
        &value
            .pointer("/rawPayloadExportAllowed")
            .and_then(Value::as_bool),
        &Some(false),
        "raw payload export allowed",
    )?;
    ensure_equal(
        &value
            .pointer("/redactedPayloadRequired")
            .and_then(Value::as_bool),
        &Some(true),
        "redacted payload required",
    )?;

    let failure = value
        .pointer("/failure")
        .ok_or_else(|| "denied outbound decision missing nested failure".to_owned())?;
    ensure_equal(
        &failure.pointer("/schema").and_then(Value::as_str),
        &Some(MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1),
        "failure schema",
    )?;
    ensure_equal(
        &failure.pointer("/code").and_then(Value::as_str),
        &Some("mesh_outbound_policy_denied"),
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
fn peer_policy_decision_fixtures_pin_outbound_non_allow_failures() -> TestResult {
    let cases = [
        (
            "tests/fixtures/mesh/peer_policy_decision_outbound_denied.json",
            "deny",
            "mesh_outbound_policy_denied",
            "outbound_payload_requires_redaction",
            "embedding",
            "redact",
        ),
        (
            "tests/fixtures/mesh/peer_policy_decision_outbound_quarantined.json",
            "quarantine",
            "mesh_outbound_policy_quarantined",
            "outbound_lane_quarantined",
            "curationSignal",
            "share",
        ),
        (
            "tests/fixtures/mesh/peer_policy_decision_outbound_rejected.json",
            "reject",
            "mesh_outbound_policy_rejected",
            "non_deny_default_action",
            "metadata",
            "deny",
        ),
    ];

    for (fixture, action, code, reason, material_lane, redaction) in cases {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_POLICY_DECISION_SCHEMA_V1),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/direction").and_then(Value::as_str),
            &Some("outbound"),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/action").and_then(Value::as_str),
            &Some(action),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/reason").and_then(Value::as_str),
            &Some(reason),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/materialLane").and_then(Value::as_str),
            &Some(material_lane),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/redaction").and_then(Value::as_str),
            &Some(redaction),
            fixture,
        )?;
        ensure_equal(
            &value
                .pointer("/payloadExportAllowed")
                .and_then(Value::as_bool),
            &Some(false),
            fixture,
        )?;
        ensure_equal(
            &value
                .pointer("/rawPayloadExportAllowed")
                .and_then(Value::as_bool),
            &Some(false),
            fixture,
        )?;

        let failure = value
            .pointer("/failure")
            .ok_or_else(|| format!("{fixture} missing failure"))?;
        ensure_equal(
            &failure.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/code").and_then(Value::as_str),
            &Some(code),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/action").and_then(Value::as_str),
            &Some(action),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/reason").and_then(Value::as_str),
            &value.pointer("/reason").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/policyRef").and_then(Value::as_str),
            &value.pointer("/policyRef").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/materialLane").and_then(Value::as_str),
            &value.pointer("/materialLane").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/redaction").and_then(Value::as_str),
            &value.pointer("/redaction").and_then(Value::as_str),
            fixture,
        )?;
        ensure_equal(
            &failure.pointer("/trustLane").and_then(Value::as_str),
            &value.pointer("/trustLane").and_then(Value::as_str),
            fixture,
        )?;
    }

    Ok(())
}

#[test]
fn peer_policy_decision_fixture_pins_outbound_redacted_body_allow() -> TestResult {
    let value =
        read_json("tests/fixtures/mesh/peer_policy_decision_outbound_redacted_body_allowed.json")?;
    ensure_equal(
        &value.pointer("/schema").and_then(Value::as_str),
        &Some(MESH_POLICY_DECISION_SCHEMA_V1),
        "decision schema",
    )?;
    ensure_equal(
        &value.pointer("/direction").and_then(Value::as_str),
        &Some("outbound"),
        "decision direction",
    )?;
    ensure_equal(
        &value.pointer("/action").and_then(Value::as_str),
        &Some("allow"),
        "decision action",
    )?;
    ensure_equal(
        &value.pointer("/materialLane").and_then(Value::as_str),
        &Some("body"),
        "material lane",
    )?;
    ensure_equal(
        &value.pointer("/redaction").and_then(Value::as_str),
        &Some("redact"),
        "redaction posture",
    )?;
    ensure_equal(
        &value
            .pointer("/payloadExportAllowed")
            .and_then(Value::as_bool),
        &Some(true),
        "payload export allowed",
    )?;
    ensure_equal(
        &value
            .pointer("/rawPayloadExportAllowed")
            .and_then(Value::as_bool),
        &Some(false),
        "raw payload export allowed",
    )?;
    ensure_equal(
        &value
            .pointer("/redactedPayloadRequired")
            .and_then(Value::as_bool),
        &Some(true),
        "redacted payload required",
    )?;
    ensure_equal(
        &value.pointer("/failure"),
        &Some(&Value::Null),
        "allowed outbound redacted body decision must not include failure",
    )
}

#[test]
fn peer_policy_decision_fixture_pins_outbound_redacted_embedding_allow() -> TestResult {
    let value = read_json(
        "tests/fixtures/mesh/peer_policy_decision_outbound_redacted_embedding_allowed.json",
    )?;
    ensure_equal(
        &value.pointer("/schema").and_then(Value::as_str),
        &Some(MESH_POLICY_DECISION_SCHEMA_V1),
        "decision schema",
    )?;
    ensure_equal(
        &value.pointer("/direction").and_then(Value::as_str),
        &Some("outbound"),
        "decision direction",
    )?;
    ensure_equal(
        &value.pointer("/action").and_then(Value::as_str),
        &Some("allow"),
        "decision action",
    )?;
    ensure_equal(
        &value.pointer("/materialLane").and_then(Value::as_str),
        &Some("embedding"),
        "material lane",
    )?;
    ensure_equal(
        &value.pointer("/redaction").and_then(Value::as_str),
        &Some("redact"),
        "redaction posture",
    )?;
    ensure_equal(
        &value
            .pointer("/payloadExportAllowed")
            .and_then(Value::as_bool),
        &Some(true),
        "payload export allowed",
    )?;
    ensure_equal(
        &value
            .pointer("/rawPayloadExportAllowed")
            .and_then(Value::as_bool),
        &Some(false),
        "raw payload export allowed",
    )?;
    ensure_equal(
        &value
            .pointer("/redactedPayloadRequired")
            .and_then(Value::as_bool),
        &Some(true),
        "redacted payload required",
    )?;
    ensure_equal(
        &value.pointer("/failure"),
        &Some(&Value::Null),
        "allowed outbound redacted embedding decision must not include failure",
    )
}

#[test]
fn peer_policy_decision_fixtures_pin_outbound_shared_payload_allow() -> TestResult {
    let cases = [
        (
            "tests/fixtures/mesh/peer_policy_decision_outbound_metadata_allowed.json",
            "metadata",
        ),
        (
            "tests/fixtures/mesh/peer_policy_decision_outbound_revision_notice_allowed.json",
            "revisionNotice",
        ),
        (
            "tests/fixtures/mesh/peer_policy_decision_outbound_shared_body_allowed.json",
            "body",
        ),
        (
            "tests/fixtures/mesh/peer_policy_decision_outbound_shared_embedding_allowed.json",
            "embedding",
        ),
    ];

    for (fixture, material_lane) in cases {
        let value = read_json(fixture)?;
        ensure_equal(
            &value.pointer("/schema").and_then(Value::as_str),
            &Some(MESH_POLICY_DECISION_SCHEMA_V1),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/direction").and_then(Value::as_str),
            &Some("outbound"),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/action").and_then(Value::as_str),
            &Some("allow"),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/materialLane").and_then(Value::as_str),
            &Some(material_lane),
            fixture,
        )?;
        ensure_equal(
            &value.pointer("/redaction").and_then(Value::as_str),
            &Some("share"),
            fixture,
        )?;
        ensure_equal(
            &value
                .pointer("/payloadExportAllowed")
                .and_then(Value::as_bool),
            &Some(true),
            fixture,
        )?;
        ensure_equal(
            &value
                .pointer("/rawPayloadExportAllowed")
                .and_then(Value::as_bool),
            &Some(true),
            fixture,
        )?;
        ensure_equal(
            &value
                .pointer("/redactedPayloadRequired")
                .and_then(Value::as_bool),
            &Some(false),
            fixture,
        )?;
        ensure_equal(&value.pointer("/failure"), &Some(&Value::Null), fixture)?;
    }

    Ok(())
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
