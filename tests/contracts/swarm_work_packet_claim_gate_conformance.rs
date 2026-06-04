use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative_path: &[&str]) -> Result<Value, String> {
    let mut path = repo_root();
    for component in relative_path {
        path.push(component);
    }
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn payload_schema() -> Result<Value, String> {
    read_json(&[
        "docs",
        "schemas",
        "swarm",
        "ee.swarm.work_packet.claim_gate.v1.json",
    ])
}

fn envelope_schema() -> Result<Value, String> {
    read_json(&["docs", "schemas", "ee.swarm.work_packet.claim_gate.v1.json"])
}

fn string_array_at(value: &Value, pointer: &str, context: &str) -> Result<Vec<String>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context} missing array {pointer}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("{context} has non-string item in {pointer}"))
        })
        .collect()
}

fn string_at<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context} missing string {pointer}"))
}

fn bool_at(value: &Value, pointer: &str, context: &str) -> Result<bool, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{context} missing boolean {pointer}"))
}

fn claim_gate_required_fields() -> Vec<String> {
    [
        "schema",
        "gateId",
        "packetId",
        "workspace",
        "redactionStatus",
        "requestedCandidateId",
        "verdict",
        "safeToClaim",
        "selectedCandidate",
        "recommendedAction",
        "recommendedSafeToClaim",
        "sourceAuthority",
        "unsafeReasons",
        "staleReasons",
        "sourceRefs",
        "degradedCodes",
        "nextCommandActions",
        "claimCommandAction",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn assert_payload_has_required_fields(
    payload: &Value,
    required: &[String],
    context: &str,
) -> TestResult {
    let object = payload
        .as_object()
        .ok_or_else(|| format!("{context} must be an object"))?;
    for field in required {
        if !object.contains_key(field) {
            return Err(format!("{context} missing required field {field}"));
        }
    }
    Ok(())
}

fn assert_no_forbidden_markers(value: &Value, context: &str) -> TestResult {
    let rendered = serde_json::to_string(value)
        .map_err(|error| format!("serialize {context} for redaction scan: {error}"))?;
    for forbidden in [
        "From:",
        "Subject:",
        "Message-ID:",
        "/Users/",
        "/home/",
        "DATABASE_URL=",
        "BEGIN PRIVATE KEY",
        "BEGIN OPENSSH PRIVATE KEY",
        "ghp_",
        "Bearer ",
        "stdout:",
        "stderr:",
        "raw_inbox",
    ] {
        if rendered.contains(forbidden) {
            return Err(format!("{context} leaks forbidden marker {forbidden}"));
        }
    }
    Ok(())
}

fn assert_next_actions_are_read_only(payload: &Value, context: &str) -> TestResult {
    let actions = payload
        .pointer("/nextCommandActions")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context} missing nextCommandActions array"))?;
    for (index, action) in actions.iter().enumerate() {
        if action.pointer("/mutatesState").and_then(Value::as_bool) != Some(false) {
            return Err(format!(
                "{context} nextCommandActions[{index}] must set mutatesState=false"
            ));
        }
    }
    Ok(())
}

#[test]
fn claim_gate_payload_required_keys_match_schema_required_array() -> TestResult {
    let schema = payload_schema()?;
    let required = string_array_at(&schema, "/required", "claim gate payload schema")?;
    let expected = claim_gate_required_fields();
    if required != expected {
        return Err(format!(
            "claim gate required fields drifted\nactual: {required:?}\nexpected: {expected:?}"
        ));
    }

    Ok(())
}

#[test]
fn claim_gate_payload_schema_is_not_marked_shipped_until_bd_1tlcd_1_closes() -> TestResult {
    let schema = payload_schema()?;
    if bool_at(&schema, "/x-ee-status/shipped", "claim gate status")? {
        return Err("claim gate schema must remain unshipped until bd-1tlcd.1 closes".into());
    }
    if bool_at(
        &schema,
        "/x-ee-status/available_in_build",
        "claim gate status",
    )? {
        return Err(
            "claim gate schema must not be marked available before the CLI emits it".into(),
        );
    }
    if string_at(&schema, "/x-ee-status/tracking_bead", "claim gate status")? != "bd-1tlcd.1" {
        return Err("claim gate schema tracking bead must stay bd-1tlcd.1".into());
    }

    Ok(())
}

#[test]
fn claim_gate_response_envelope_pins_inner_schema() -> TestResult {
    let schema = envelope_schema()?;
    if string_at(&schema, "/properties/schema/const", "claim gate envelope")? != "ee.response.v2" {
        return Err("claim gate envelope must pin ee.response.v2".into());
    }
    if string_at(
        &schema,
        "/properties/data/properties/schema/const",
        "claim gate envelope",
    )? != "ee.swarm.work_packet.claim_gate.v1"
    {
        return Err("claim gate envelope must pin data.schema".into());
    }

    let data_required =
        string_array_at(&schema, "/properties/data/required", "claim gate envelope")?;
    for field in [
        "schema",
        "gateId",
        "packetId",
        "verdict",
        "safeToClaim",
        "nextCommandActions",
        "claimCommandAction",
    ] {
        if !data_required.iter().any(|actual| actual == field) {
            return Err(format!("claim gate envelope data.required missing {field}"));
        }
    }

    for preset in ["minimal", "summary", "standard", "full"] {
        let pointer = format!("/field_presets/{preset}/0");
        if string_at(&schema, &pointer, "claim gate envelope field preset")? != "*" {
            return Err(format!(
                "claim gate envelope field preset {preset} must expose wildcard"
            ));
        }
    }

    Ok(())
}

#[test]
fn claim_gate_sample_payloads_are_redacted_and_safe() -> TestResult {
    let schema = payload_schema()?;
    let required = string_array_at(&schema, "/required", "claim gate payload schema")?;
    let safe_sample = schema
        .pointer("/examples/0")
        .cloned()
        .ok_or_else(|| "claim gate payload schema missing first example".to_owned())?;
    let unsafe_sample = json!({
        "schema": "ee.swarm.work_packet.claim_gate.v1",
        "gateId": "swarm_work_packet_claim_gate_333333333333333333333333",
        "packetId": "swarm_work_packet_444444444444444444444444",
        "workspace": "repo:25e38e130474e7f0292de2a3",
        "redactionStatus": "counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content",
        "requestedCandidateId": "bd-owned.1",
        "verdict": "already_owned",
        "safeToClaim": false,
        "selectedCandidate": {
            "id": "bd-owned.1",
            "title": "Peer-owned source implementation",
            "source": "beads_ready",
            "status": "in_progress",
            "priority": 1,
            "assignee": "peer-agent",
            "decision": "already_owned",
            "collisionRisk": "high"
        },
        "recommendedAction": "coordinate_before_claim",
        "recommendedSafeToClaim": false,
        "sourceAuthority": {
            "trackerAuthoritative": true,
            "trackerHealth": "ok",
            "agentMailStatus": "healthy",
            "reservationAuthoritative": true,
            "inboxAuthoritative": true,
            "rchSafeToLaunchCargoVerification": false,
            "sourceCount": 4
        },
        "unsafeReasons": ["active_claim", "reserved_file_overlap"],
        "staleReasons": [],
        "sourceRefs": ["br://bd-owned.1", "reservation://source-file"],
        "degradedCodes": ["rch_remote_required_fallback_prevented"],
        "nextCommandActions": [
            {
                "commandId": "bead_show_candidate",
                "displayCommand": "br show bd-owned.1 --json",
                "argv": ["br", "show", "bd-owned.1", "--json"],
                "shellRequired": false,
                "copySafety": "safe_structured_argv",
                "mutatesState": false,
                "requiredSubstrate": "beads",
                "when": "before_coordination",
                "rationale": "Inspect the peer-owned candidate without changing tracker state."
            }
        ],
        "claimCommandAction": null
    });

    for (context, sample) in [
        ("safe claim-gate sample", safe_sample),
        ("unsafe claim-gate sample", unsafe_sample),
    ] {
        assert_payload_has_required_fields(&sample, &required, context)?;
        assert_no_forbidden_markers(&sample, context)?;
        assert_next_actions_are_read_only(&sample, context)?;
        if sample.pointer("/safeToClaim").and_then(Value::as_bool) == Some(false)
            && !sample
                .pointer("/claimCommandAction")
                .is_some_and(Value::is_null)
        {
            return Err(format!(
                "{context} must set claimCommandAction=null when unsafe"
            ));
        }
    }

    Ok(())
}

#[test]
fn claim_gate_schema_forbids_mutating_inspection_actions() -> TestResult {
    let schema = payload_schema()?;
    if string_at(
        &schema,
        "/properties/nextCommandActions/items/$ref",
        "claim gate payload schema",
    )? != "#/definitions/inspectionCommandAction"
    {
        return Err("nextCommandActions must use inspectionCommandAction".into());
    }
    if schema
        .pointer("/definitions/inspectionCommandAction/properties/mutatesState/const")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("inspectionCommandAction must force mutatesState=false".into());
    }

    let all_of = schema
        .pointer("/allOf")
        .and_then(Value::as_array)
        .ok_or_else(|| "claim gate payload schema missing allOf rules".to_owned())?;
    let unsafe_rule_forces_null_claim = all_of.iter().any(|rule| {
        rule.pointer("/if/properties/safeToClaim/const")
            .and_then(Value::as_bool)
            == Some(false)
            && rule.pointer("/then/properties/claimCommandAction/const") == Some(&Value::Null)
    });
    if !unsafe_rule_forces_null_claim {
        return Err("safeToClaim=false must force claimCommandAction=null".into());
    }

    Ok(())
}
