//! bd-20453.3: golden fixture matrix for environment attestation authority skew.

use std::path::Path;

use chrono::{TimeZone, Utc};
use ee::core::environment_attestation::{
    EnvironmentAttestationInputs, EnvironmentAttestationLocalCargoScanOrigin,
    environment_attestation_from_swarm_brief, environment_attestation_from_swarm_brief_with_inputs,
};
use ee::core::swarm_brief::{
    SwarmBriefDegradation, SwarmBriefDirtyFile, SwarmBriefFileReservation, SwarmBriefReport,
    SwarmBriefSourceFreshness, SwarmBriefSourceKind, SwarmBriefSourceProvenance,
    SwarmBriefSourceSnapshot, SwarmBriefSourceStatus,
};
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, String>;

const SCHEMA_TEXT: &str = include_str!("../docs/schemas/ee.environment_attestation.v1.json");
const GOLDEN_MATRIX: &str =
    include_str!("fixtures/golden/environment_attestation/skew_authority_matrix.json.golden");
const SCRUBBED_ATTESTATION_ID: &str = "environment_attestation_000000000000000000000000";
const DENIED_SUBSTRINGS: &[&str] = &[
    "body_md",
    "raw secret body",
    "ghp_",
    "Bearer ",
    "DATABASE_URL=",
    "/Users/",
    "/Volumes/",
    "/data/",
    "/tmp/",
    "/private/tmp",
    "/var/folders",
    "stdout:",
    "stderr:",
];

#[test]
fn environment_attestation_skew_authority_matrix_matches_golden() -> TestResult {
    let schema: Value = serde_json::from_str(SCHEMA_TEXT).map_err(|error| error.to_string())?;
    let mut cases = Vec::new();

    for (name, attestation) in attestation_cases()? {
        let mut value = serde_json::to_value(&attestation).map_err(|error| error.to_string())?;
        validate_json_schema(&value, &schema, &schema, "$")?;
        assert_attestation_id_pattern(&value)?;
        assert_no_denied_substrings(name, &value)?;
        value["attestationId"] = json!(SCRUBBED_ATTESTATION_ID);
        cases.push(compact_case(name, &value)?);
    }

    let matrix = json!({
        "schema": "ee.environment_attestation.fixture_matrix.v1",
        "cases": cases,
    });
    let rendered = serde_json::to_string_pretty(&matrix).map_err(|error| error.to_string())? + "\n";
    if rendered != GOLDEN_MATRIX {
        return Err(format!(
            "environment attestation golden drifted\n--- expected\n{}--- actual\n{}",
            GOLDEN_MATRIX, rendered
        ));
    }

    Ok(())
}

fn attestation_cases() -> Result<
    Vec<(
        &'static str,
        ee::core::environment_attestation::EnvironmentAttestationReport,
    )>,
    String,
> {
    let mut cases = Vec::new();

    cases.push((
        "clean_remote_ready",
        environment_attestation_from_swarm_brief(
            &report_with_sources(vec![
                ready_source(SwarmBriefSourceKind::Git),
                ready_source(SwarmBriefSourceKind::Beads),
                ready_source(SwarmBriefSourceKind::Bv),
                ready_source(SwarmBriefSourceKind::AgentMail),
                ready_source(SwarmBriefSourceKind::Rch),
            ]),
            fixed_time(),
        ),
    ));

    cases.push((
        "stale_binary_suspected",
        environment_attestation_from_swarm_brief(
            &report_with_sources(vec![degraded_source(
                SwarmBriefSourceKind::Git,
                "stale_binary_suspected",
                "installed ee at /private/tmp/stale-ee lacks source flags",
                Some("ee --version"),
            )]),
            fixed_time(),
        ),
    ));

    cases.push((
        "tracker_stale_and_bv_blocked",
        environment_attestation_from_swarm_brief(
            &report_with_sources(vec![
                degraded_source(
                    SwarmBriefSourceKind::Beads,
                    "beads_tracker_stale",
                    "Beads JSONL is newer than the DB",
                    Some("br sync --import-only"),
                ),
                degraded_source(
                    SwarmBriefSourceKind::Bv,
                    "bv_recommendation_stale",
                    "BV recommended bd-37ugy but br show reports blocked",
                    Some("br show bd-37ugy --json"),
                ),
            ]),
            fixed_time(),
        ),
    ));

    cases.push((
        "agent_mail_probe_unavailable",
        environment_attestation_from_swarm_brief(
            &report_with_sources(vec![unavailable_source(
                SwarmBriefSourceKind::AgentMail,
                "agent_mail_unavailable",
                "MCP Agent Mail is reachable but the CLI probe lacks a redacted snapshot",
            )]),
            fixed_time(),
        ),
    ));

    cases.push((
        "rch_topology_blocked_before_cargo",
        environment_attestation_from_swarm_brief(
            &report_with_sources(vec![degraded_source(
                SwarmBriefSourceKind::Rch,
                "rch_worker_topology_blocked",
                "RCH-E327 blocked before Cargo; remote required refused local fallback",
                Some("rch status --json"),
            )]),
            fixed_time(),
        ),
    ));

    let local_cargo_scan = json!({
        "schema": "ee.rch_local_cargo_tripwire.v1",
        "mode": "probe_processes",
        "status": "bypass_detected",
        "count": 1,
        "detectedLocalBuilds": [{"kind": "cargo"}],
        "evidence": [{"kind": "active_process_scan", "result": "bypass_detected"}]
    });
    cases.push((
        "local_cargo_bypass_detected",
        environment_attestation_from_swarm_brief_with_inputs(
            &report_with_sources(vec![ready_source(SwarmBriefSourceKind::Git)]),
            EnvironmentAttestationInputs {
                generated_at: fixed_time(),
                local_cargo_process_scan: Some(&local_cargo_scan),
                local_cargo_process_scan_origin:
                    EnvironmentAttestationLocalCargoScanOrigin::LiveProbe,
                ci_proof_lane_snapshot: None,
            },
        ),
    ));

    let ci_stale_snapshot = ci_proof_lane_fixture("artifact_stale")?;
    cases.push((
        "ci_proof_lane_stale_artifact",
        environment_attestation_from_swarm_brief_with_inputs(
            &report_with_sources(vec![ready_source(SwarmBriefSourceKind::Git)]),
            EnvironmentAttestationInputs {
                generated_at: fixed_time(),
                local_cargo_process_scan: None,
                local_cargo_process_scan_origin:
                    EnvironmentAttestationLocalCargoScanOrigin::LiveProbe,
                ci_proof_lane_snapshot: Some(&ci_stale_snapshot),
            },
        ),
    ));

    let mut reservation_report =
        report_with_sources(vec![ready_source(SwarmBriefSourceKind::AgentMail)]);
    reservation_report
        .file_reservations
        .push(SwarmBriefFileReservation {
            path_pattern: "tests/**".to_owned(),
            holder: "BlueFortress".to_owned(),
            exclusive: true,
            expires_at: Some("2026-06-05T01:11:14Z".to_owned()),
        });
    reservation_report.finalize();
    cases.push((
        "reservation_conflict",
        environment_attestation_from_swarm_brief(&reservation_report, fixed_time()),
    ));

    let mut dirty_report = report_with_sources(vec![ready_source(SwarmBriefSourceKind::Git)]);
    dirty_report.dirty_files.push(SwarmBriefDirtyFile {
        path: "tests/**".to_owned(),
        status: "M".to_owned(),
    });
    dirty_report.finalize();
    cases.push((
        "dirty_source_checkout",
        environment_attestation_from_swarm_brief(&dirty_report, fixed_time()),
    ));

    cases.push((
        "source_authority_ambiguous_empty",
        environment_attestation_from_swarm_brief(
            &SwarmBriefReport::empty(Path::new(".")),
            fixed_time(),
        ),
    ));

    Ok(cases)
}

fn ci_proof_lane_fixture(name: &str) -> Result<Value, String> {
    let text = match name {
        "artifact_stale" => include_str!("fixtures/ci_proof_lane/artifact_stale.json"),
        _ => return Err(format!("unknown CI proof lane fixture {name}")),
    };
    serde_json::from_str(text).map_err(|error| format!("{name} fixture must parse: {error}"))
}

fn fixed_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 4, 20, 0, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

fn report_with_sources(sources: Vec<SwarmBriefSourceSnapshot>) -> SwarmBriefReport {
    let mut report = SwarmBriefReport::empty(Path::new("."));
    report.sources = sources;
    report.finalize();
    report
}

fn ready_source(source: SwarmBriefSourceKind) -> SwarmBriefSourceSnapshot {
    SwarmBriefSourceSnapshot::ready(source, SwarmBriefSourceProvenance::local_probe(), 1)
}

fn degraded_source(
    source: SwarmBriefSourceKind,
    code: &str,
    message: &str,
    repair: Option<&str>,
) -> SwarmBriefSourceSnapshot {
    SwarmBriefSourceSnapshot {
        source,
        status: SwarmBriefSourceStatus::Degraded,
        freshness: SwarmBriefSourceFreshness::current(),
        provenance: SwarmBriefSourceProvenance::local_probe(),
        item_count: 0,
        degraded: vec![SwarmBriefDegradation::warning(
            source,
            code,
            message,
            repair.map(ToOwned::to_owned),
        )],
    }
}

fn unavailable_source(
    source: SwarmBriefSourceKind,
    code: &str,
    message: &str,
) -> SwarmBriefSourceSnapshot {
    SwarmBriefSourceSnapshot::unavailable(
        source,
        SwarmBriefSourceProvenance::local_probe(),
        SwarmBriefDegradation::warning(source, code, message, None),
    )
}

fn compact_case(name: &str, attestation: &Value) -> TestResult<Value> {
    Ok(json!({
        "case": name,
        "summary": attestation
            .get("summary")
            .ok_or_else(|| format!("{name}: missing summary"))?,
        "verdict": attestation
            .get("verdict")
            .ok_or_else(|| format!("{name}: missing verdict"))?,
        "sourceAuthority": compact_sources(
            attestation
                .get("sourceAuthority")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{name}: missing sourceAuthority"))?
        ),
        "degradedCodes": attestation
            .get("degraded")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{name}: missing degraded"))?
            .iter()
            .map(|entry| entry.get("code").cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>(),
        "recoveryActions": compact_recovery_actions(
            attestation
                .get("recoveryActions")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{name}: missing recoveryActions"))?
        ),
    }))
}

fn compact_sources(sources: &[Value]) -> Vec<Value> {
    sources
        .iter()
        .map(|source| {
            json!({
                "source": source["source"].clone(),
                "authority": source["authority"].clone(),
                "status": source["status"].clone(),
                "freshness": source["freshness"].clone(),
                "degradedCodes": source["degradedCodes"].clone(),
                "recoveryActions": compact_recovery_actions(
                    source
                        .get("recoveryActions")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                ),
            })
        })
        .collect()
}

fn compact_recovery_actions(actions: &[Value]) -> Vec<Value> {
    actions
        .iter()
        .map(|action| {
            json!({
                "kind": action["kind"].clone(),
                "command": action
                    .pointer("/command/displayCommand")
                    .cloned()
                    .unwrap_or(Value::Null),
                "mutatesState": action["mutatesState"].clone(),
                "requiredSubstrate": action["requiredSubstrate"].clone(),
            })
        })
        .collect()
}

fn assert_attestation_id_pattern(value: &Value) -> TestResult {
    let id = value
        .get("attestationId")
        .and_then(Value::as_str)
        .ok_or_else(|| "attestationId missing".to_owned())?;
    let suffix = id
        .strip_prefix("environment_attestation_")
        .ok_or_else(|| format!("attestationId has wrong prefix: {id}"))?;
    if suffix.len() != 24 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("attestationId has wrong hash suffix: {id}"));
    }
    Ok(())
}

fn assert_no_denied_substrings(name: &str, value: &Value) -> TestResult {
    let rendered = serde_json::to_string(value).map_err(|error| error.to_string())?;
    for denied in DENIED_SUBSTRINGS {
        if rendered.contains(denied) {
            return Err(format!(
                "{name}: attestation leaked denied substring {denied}"
            ));
        }
    }
    Ok(())
}

fn validate_json_schema(
    value: &Value,
    schema: &Value,
    root_schema: &Value,
    path: &str,
) -> TestResult {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = resolve_ref(root_schema, reference)?;
        return validate_json_schema(value, target, root_schema, path);
    }

    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        if options
            .iter()
            .any(|candidate| validate_json_schema(value, candidate, root_schema, path).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path} did not match any anyOf branch"));
    }

    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err(format!("{path} expected const {expected}, got {value}"));
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        return Err(format!(
            "{path} value {value} is not in enum {enum_values:?}"
        ));
    }

    if let Some(expected_types) = schema_types(schema)
        && !expected_types
            .iter()
            .any(|expected_type| json_type_matches(value, expected_type))
    {
        return Err(format!(
            "{path} expected type {:?}, got {}",
            expected_types,
            json_type_name(value)
        ));
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required {
                let field = field
                    .as_str()
                    .ok_or_else(|| format!("{path} schema required entry is not a string"))?;
                if !object.contains_key(field) {
                    return Err(format!("{path} missing required field {field}"));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            let child_path = format!("{path}.{key}");
            if let Some(property_schema) = properties.and_then(|props| props.get(key)) {
                validate_json_schema(child, property_schema, root_schema, &child_path)?;
                continue;
            }
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                return Err(format!("{path} contains unexpected field {key}"));
            }
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
            && array.len() < min_items as usize
        {
            return Err(format!("{path} has fewer than {min_items} items"));
        }
        if let Some(items_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_json_schema(item, items_schema, root_schema, &format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn resolve_ref<'a>(root_schema: &'a Value, reference: &str) -> TestResult<&'a Value> {
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| format!("unsupported non-local $ref {reference}"))?;
    root_schema
        .pointer(pointer)
        .ok_or_else(|| format!("unresolved $ref {reference}"))
}

fn schema_types(schema: &Value) -> Option<Vec<&str>> {
    match schema.get("type")? {
        Value::String(single) => Some(vec![single.as_str()]),
        Value::Array(values) => Some(values.iter().filter_map(Value::as_str).collect()),
        _ => None,
    }
}

fn json_type_matches(value: &Value, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
