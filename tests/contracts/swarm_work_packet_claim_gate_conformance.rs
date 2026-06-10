use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use ee::core::beads_integrity::{
    BeadsIntegrityInputs, BeadsIntegrityReport, JsonlParseError, compose_integrity_report,
    compose_integrity_report_from_br_doctor_json,
};
use ee::core::swarm_brief::SwarmBriefReport;
use ee::core::swarm_next_action::{
    SWARM_NEXT_ACTION_REDACTION_STATUS, SWARM_NEXT_ACTION_SCHEMA_V1, SwarmNextActionCandidate,
    SwarmNextActionCheckoutSummary, SwarmNextActionCompileHealthSummary,
    SwarmNextActionCoordinationSummary, SwarmNextActionEnvironmentSummary,
    SwarmNextActionInputSummary, SwarmNextActionSnapshot, SwarmNextActionVerificationSummary,
    SwarmWorkPacket, SwarmWorkPacketClaimGate,
};

type TestResult = Result<(), String>;

const TRACKER_METADATA_DRIFT_CODE: &str = "beads_tracker_metadata_drift";

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

fn resource_admission_sample(decision: &str, recommended_profile: &str) -> Value {
    json!({
        "schema": "ee.resource_admission.v1",
        "policyDomain": "resource_profile_budget_admission",
        "policyId": "candidate.resource_profile_budget_admission",
        "sideEffectFree": true,
        "advisoryOnly": true,
        "canAuthorizeClaim": false,
        "surface": "claim_gate",
        "commandClass": "coordination",
        "decision": decision,
        "requestedProfile": "workstation",
        "effectiveProfile": "workstation",
        "recommendedProfile": recommended_profile,
        "estimatedCostClass": "standard",
        "sourcePosture": {
            "hostCalibration": "fresh",
            "resourceBudget": if decision == "degrade_to_lean" { "recommend_decrease" } else { "within_budget" },
            "rch": if decision == "wait_for_rch" { "blocked" } else { "remote_ready" },
            "localCargo": "refused",
            "lanePressure": if decision == "wait_for_rch" { "verification_pressure" } else { "clear" },
            "workloadPressure": if decision == "degrade_to_lean" { "cache_pressure" } else { "within_budget" },
            "daemon": "not_required",
            "replay": "not_required",
            "redactionPostureVerified": true,
            "sourceCount": 4
        },
        "evidenceFreshness": if decision == "wait_for_rch" { "degraded" } else { "partial" },
        "reasonCodes": if decision == "wait_for_rch" {
            json!(["rch_blocked", "local_cargo_refused", "redaction_posture_verified"])
        } else {
            json!(["budget_delta_recommends_decrease", "cache_pressure", "redaction_posture_verified"])
        },
        "abstentionReasons": [],
        "nextCommands": if decision == "wait_for_rch" {
            json!(["rch status --json"])
        } else {
            json!(["ee pack <task> --resource-profile constrained --json"])
        },
        "nextCommandActions": [
            {
                "commandId": "resource_admission_diag",
                "displayCommand": "ee diag resource-admission --surface claim-gate --command-class coordination --json",
                "argv": ["ee", "diag", "resource-admission", "--surface", "claim-gate", "--command-class", "coordination", "--json"],
                "shellRequired": false,
                "copySafety": "safe_structured_argv",
                "mutatesState": false,
                "requiredSubstrate": "ee",
                "when": "inspect_resource_admission_advice",
                "rationale": "Reproduce the advisory resource admission decision."
            }
        ]
    })
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
        "recoveryActions",
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

fn assert_resource_admission_is_advisory(payload: &Value, context: &str) -> TestResult {
    let Some(admission) = payload.get("resourceAdmission") else {
        return Ok(());
    };
    if admission
        .pointer("/canAuthorizeClaim")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err(format!(
            "{context} resourceAdmission must set canAuthorizeClaim=false"
        ));
    }
    for pointer in ["/sideEffectFree", "/advisoryOnly"] {
        if admission.pointer(pointer).and_then(Value::as_bool) != Some(true) {
            return Err(format!(
                "{context} resourceAdmission must set {pointer}=true"
            ));
        }
    }
    let actions = admission
        .pointer("/nextCommandActions")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context} resourceAdmission missing nextCommandActions"))?;
    for (index, action) in actions.iter().enumerate() {
        if action.pointer("/mutatesState").and_then(Value::as_bool) != Some(false) {
            return Err(format!(
                "{context} resourceAdmission.nextCommandActions[{index}] must be read-only"
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
fn claim_gate_payload_schema_is_marked_shipped_after_bd_1tlcd_1_closes() -> TestResult {
    let schema = payload_schema()?;
    if !bool_at(&schema, "/x-ee-status/shipped", "claim gate status")? {
        return Err("claim gate schema must be marked shipped after bd-1tlcd.1 closes".into());
    }
    if !bool_at(
        &schema,
        "/x-ee-status/available_in_build",
        "claim gate status",
    )? {
        return Err("claim gate schema must be marked available after the CLI emits it".into());
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
    let mut lean_sample = safe_sample.clone();
    lean_sample
        .as_object_mut()
        .ok_or_else(|| "safe claim-gate sample must be object".to_owned())?
        .insert(
            "resourceAdmission".to_owned(),
            resource_admission_sample("degrade_to_lean", "constrained"),
        );
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
            "rchRemoteOnlyRequired": true,
            "rchSafeToLaunchCargoVerification": false,
            "environmentVerdict": "proof_environment_blocked",
            "sourceTestVerdict": "environment_blocked_before_source",
            "remoteVerificationAdmitted": false,
            "localCargoFallbackObserved": null,
            "installFreshnessVerdict": "not_evaluated",
            "installFreshnessAuthoritative": null,
            "installFreshnessRepair": null,
            "sourceCount": 4
        },
        "resourceAdmission": resource_admission_sample("wait_for_rch", "workstation"),
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
        "claimCommandAction": null,
        "recoveryActions": []
    });

    for (context, sample) in [
        ("safe claim-gate sample", safe_sample),
        ("lean-advice claim-gate sample", lean_sample),
        ("unsafe claim-gate sample", unsafe_sample),
    ] {
        assert_payload_has_required_fields(&sample, &required, context)?;
        assert_no_forbidden_markers(&sample, context)?;
        assert_next_actions_are_read_only(&sample, context)?;
        assert_resource_admission_is_advisory(&sample, context)?;
        if sample.pointer("/safeToClaim").and_then(Value::as_bool) == Some(true)
            && sample
                .pointer("/recommendedSafeToClaim")
                .and_then(Value::as_bool)
                != Some(true)
        {
            return Err(format!(
                "{context} must set recommendedSafeToClaim=true when safeToClaim=true"
            ));
        }
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

    let lean_admission = resource_admission_sample("degrade_to_lean", "constrained");
    let lean_decision = string_at(
        &lean_admission,
        "/decision",
        "lean-advice claim-gate resourceAdmission",
    )?;
    if lean_decision != "degrade_to_lean" {
        return Err("claim gate contract sample must cover degrade_to_lean".into());
    }
    let wait_admission = resource_admission_sample("wait_for_rch", "workstation");
    let wait_decision = string_at(
        &wait_admission,
        "/decision",
        "wait-for-rch claim-gate resourceAdmission",
    )?;
    if wait_decision != "wait_for_rch" {
        return Err("claim gate contract sample must cover wait_for_rch".into());
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

    let safe_rule = all_of
        .iter()
        .find(|rule| {
            rule.pointer("/if/properties/safeToClaim/const")
                .and_then(Value::as_bool)
                == Some(true)
        })
        .ok_or_else(|| "claim gate payload schema missing safeToClaim=true rule".to_owned())?;
    if safe_rule
        .pointer("/then/properties/recommendedSafeToClaim/const")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("safeToClaim=true must force recommendedSafeToClaim=true".into());
    }
    if string_at(
        safe_rule,
        "/then/properties/selectedCandidate/properties/decision/const",
        "safeToClaim=true rule",
    )? != "safe_to_claim"
    {
        return Err("safeToClaim=true must force selectedCandidate.decision=safe_to_claim".into());
    }
    if string_at(
        safe_rule,
        "/then/properties/claimCommandAction/type",
        "safeToClaim=true rule",
    )? != "object"
    {
        return Err("safeToClaim=true must force claimCommandAction to be an object".into());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tracker authority states (bd-3w4pv.6)
//
// Fixture-driven gate conformance for the split trackerHealth vocabulary:
// every concrete stale state fails closed, while the doctor
// metadata-message-only contradiction keeps tracker authority true and
// surfaces as a warning-severity degradation instead of
// beads_tracker_not_authoritative.
// ---------------------------------------------------------------------------

fn tracker_authority_fixture(file_name: &str) -> Result<Value, String> {
    read_json(&[
        "tests",
        "fixtures",
        "swarm_work_packet",
        "tracker_authority",
        file_name,
    ])
}

fn u64_at(value: &Value, pointer: &str, context: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{context} missing unsigned integer {pointer}"))
}

fn claim_ready_snapshot(candidate_id: &str, title: &str) -> SwarmNextActionSnapshot {
    SwarmNextActionSnapshot {
        schema: SWARM_NEXT_ACTION_SCHEMA_V1,
        workspace: "/tmp/project".to_owned(),
        redaction_status: SWARM_NEXT_ACTION_REDACTION_STATUS,
        inputs: SwarmNextActionInputSummary {
            source_count: 1,
            ready_bead_count: 1,
            in_progress_bead_count: 0,
            blocked_bead_count: 0,
            bv_top_pick_count: 0,
        },
        candidates: vec![SwarmNextActionCandidate {
            id: candidate_id.to_owned(),
            title: title.to_owned(),
            source: "beads_ready",
            score_milli: None,
            status: "open".to_owned(),
            priority: Some(2),
            issue_type: None,
            assignee: None,
            blocked_by: Vec::new(),
            blocked_by_compile_health: false,
            action_hint: "reserve_files_and_start_smallest_useful_slice".to_owned(),
        }],
        stale_work_proposals: Vec::new(),
        coordination: SwarmNextActionCoordinationSummary {
            active_reservation_count: 0,
            reservation_holders: Vec::new(),
            unread_inbox_count: 0,
            ack_required_count: 0,
        },
        checkout: SwarmNextActionCheckoutSummary {
            dirty_path_count: 0,
            dirty_paths: Vec::new(),
        },
        compile_health: SwarmNextActionCompileHealthSummary {
            safe_to_launch_rch: Some(true),
            blocker_count: 0,
            blockers: Vec::new(),
            recommended_alternative_work: Vec::new(),
        },
        verification: SwarmNextActionVerificationSummary {
            rch_source_enabled: true,
            remote_only_required: true,
            remote_only_safe: Some(true),
            healthy_worker_count: Some(1),
            active_remote_build_count: Some(0),
            queued_remote_build_count: Some(0),
            slots_available: Some(1),
            queue_head_slots_needed: None,
            active_build_max_age_seconds: None,
            queue_status: Some("ready".to_owned()),
            verifier_evidence: Vec::new(),
        },
        environment: SwarmNextActionEnvironmentSummary {
            cargo_target_externalized: true,
            tmpdir_externalized: true,
            external_agent_space_present: true,
            disk_pressure_hint_count: 0,
        },
        degraded: Vec::new(),
    }
}

fn packet_and_gate(
    tracker_integrity: BeadsIntegrityReport,
    snapshot: &SwarmNextActionSnapshot,
    candidate_id: &str,
) -> (SwarmWorkPacket, SwarmWorkPacketClaimGate) {
    let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
    let packet = SwarmWorkPacket::from_brief_and_next_action_with_tracker_integrity(
        &brief,
        snapshot,
        tracker_integrity,
    );
    let gate = packet.claim_gate(Some(candidate_id));
    (packet, gate)
}

fn report_from_matrix_case(case: &Value, context: &str) -> Result<BeadsIntegrityReport, String> {
    let merge_artifact_paths = string_array_at(case, "/mergeArtifactPaths", context)?;
    let jsonl_parse_error = match case.pointer("/jsonlParseError") {
        None | Some(Value::Null) => None,
        Some(error) => Some(JsonlParseError {
            line: u64_at(error, "/line", context)?,
            column: error.pointer("/column").and_then(Value::as_u64),
            excerpt: string_at(error, "/excerpt", context)?.to_owned(),
        }),
    };
    Ok(compose_integrity_report(BeadsIntegrityInputs {
        jsonl_path: ".beads/issues.jsonl",
        db_path: ".beads/beads.db",
        jsonl_record_count: u64_at(case, "/jsonlRecordCount", context)?,
        db_record_count: u64_at(case, "/dbRecordCount", context)?,
        auto_import_enabled: bool_at(case, "/autoImportEnabled", context)?,
        external_changes_pending_import: bool_at(case, "/externalChangesPendingImport", context)?,
        dirty_issue_count: u64_at(case, "/dirtyIssueCount", context)?,
        merge_artifact_paths: &merge_artifact_paths,
        jsonl_parse_error,
    }))
}

fn report_from_doctor_fixture(
    fixture: &Value,
    context: &str,
) -> Result<BeadsIntegrityReport, String> {
    let doctor = fixture
        .get("doctor")
        .ok_or_else(|| format!("{context} missing doctor payload"))?;
    let raw = serde_json::to_string(doctor)
        .map_err(|error| format!("{context} serialize doctor payload: {error}"))?;
    compose_integrity_report_from_br_doctor_json(
        &raw,
        ".beads/issues.jsonl",
        ".beads/beads.db",
        true,
    )
    .map_err(|error| format!("{context} compose integrity report: {error}"))
}

fn gate_contradiction_signal(
    packet: &SwarmWorkPacket,
    gate: &SwarmWorkPacketClaimGate,
) -> (bool, bool) {
    let packet_signal = packet
        .degraded
        .iter()
        .any(|degradation| degradation.code == TRACKER_METADATA_DRIFT_CODE);
    let gate_signal = gate
        .degraded_codes
        .iter()
        .any(|code| code == TRACKER_METADATA_DRIFT_CODE);
    (packet_signal, gate_signal)
}

#[test]
fn claim_gate_tracker_health_matrix_states_are_fail_closed_except_message_only() -> TestResult {
    let fixture = tracker_authority_fixture("claim_gate_tracker_health_matrix.json")?;
    let cases = fixture
        .pointer("/cases")
        .and_then(Value::as_array)
        .ok_or_else(|| "tracker health matrix fixture missing cases[]".to_owned())?;
    if cases.len() != 8 {
        return Err(format!(
            "tracker health matrix must pin all 8 states, found {}",
            cases.len()
        ));
    }

    for case in cases {
        let name = string_at(case, "/name", "tracker health matrix case")?;
        let context = format!("tracker health matrix case {name}");
        let expected_health = string_at(case, "/expected/trackerHealth", &context)?.to_owned();
        let expected_authoritative = bool_at(case, "/expected/trackerAuthoritative", &context)?;
        let expected_contradiction = bool_at(case, "/expected/contradictionSignal", &context)?;

        let report = report_from_matrix_case(case, &context)?;
        let snapshot = claim_ready_snapshot("bd-matrix", "Document tracker authority states");
        let (packet, gate) = packet_and_gate(report, &snapshot, "bd-matrix");

        if gate.source_authority.tracker_health != expected_health {
            return Err(format!(
                "{context}: trackerHealth expected {expected_health}, got {}",
                gate.source_authority.tracker_health
            ));
        }
        if gate.source_authority.tracker_authoritative != expected_authoritative {
            return Err(format!(
                "{context}: trackerAuthoritative expected {expected_authoritative}, got {}",
                gate.source_authority.tracker_authoritative
            ));
        }

        if expected_authoritative {
            if !gate.safe_to_claim || gate.verdict != "safe_to_claim" {
                return Err(format!(
                    "{context}: authoritative tracker must keep the clean candidate claimable, \
                     got verdict {} safeToClaim {}",
                    gate.verdict, gate.safe_to_claim
                ));
            }
            if gate.claim_command_action.is_none() {
                return Err(format!(
                    "{context}: claimable gate must expose claimCommandAction"
                ));
            }
            if gate
                .unsafe_reasons
                .iter()
                .any(|reason| reason.starts_with("beads_tracker_not_authoritative"))
            {
                return Err(format!(
                    "{context}: authoritative tracker must not emit beads_tracker_not_authoritative"
                ));
            }
        } else {
            if gate.safe_to_claim {
                return Err(format!("{context}: stale tracker state must fail closed"));
            }
            if gate.claim_command_action.is_some() {
                return Err(format!(
                    "{context}: stale tracker state must keep claimCommandAction null"
                ));
            }
            let expected_reason = format!("beads_tracker_not_authoritative:{expected_health}");
            if !gate.unsafe_reasons.contains(&expected_reason) {
                return Err(format!(
                    "{context}: missing concrete unsafe reason {expected_reason}; got {:?}",
                    gate.unsafe_reasons
                ));
            }
        }

        let (packet_signal, gate_signal) = gate_contradiction_signal(&packet, &gate);
        if packet_signal != expected_contradiction || gate_signal != expected_contradiction {
            return Err(format!(
                "{context}: contradiction signal expected {expected_contradiction}, \
                 packet {packet_signal}, gate {gate_signal}"
            ));
        }

        let rendered = serde_json::to_value(&gate)
            .map_err(|error| format!("{context}: serialize gate: {error}"))?;
        assert_no_forbidden_markers(&rendered, &context)?;
    }

    Ok(())
}

#[test]
fn claim_gate_doctor_metadata_message_only_keeps_tracker_authoritative() -> TestResult {
    let fixture = tracker_authority_fixture("doctor_metadata_message_only.json")?;
    let context = "doctor metadata-message-only fixture";
    let report = report_from_doctor_fixture(&fixture, context)?;
    let snapshot = claim_ready_snapshot("bd-message-only", "Document tracker authority states");
    let (packet, gate) = packet_and_gate(report, &snapshot, "bd-message-only");

    let expected_health = string_at(&fixture, "/expected/trackerHealth", context)?;
    if gate.source_authority.tracker_health != expected_health {
        return Err(format!(
            "{context}: trackerHealth expected {expected_health}, got {}",
            gate.source_authority.tracker_health
        ));
    }
    if !gate.source_authority.tracker_authoritative {
        return Err(format!(
            "{context}: metadata-only doctor message must keep trackerAuthoritative=true"
        ));
    }
    if !gate.safe_to_claim || gate.claim_command_action.is_none() {
        return Err(format!(
            "{context}: clean concrete evidence must keep the candidate claimable"
        ));
    }
    if gate
        .unsafe_reasons
        .iter()
        .any(|reason| reason.starts_with("beads_tracker_not_authoritative"))
    {
        return Err(format!(
            "{context}: the metadata message must not surface as beads_tracker_not_authoritative"
        ));
    }

    let expected_code = string_at(&fixture, "/expected/contradictionDegradedCode", context)?;
    let drift = packet
        .degraded
        .iter()
        .find(|degradation| degradation.code == expected_code)
        .ok_or_else(|| {
            format!("{context}: contradiction degradation {expected_code} must be present")
        })?;
    if drift.severity != "warning" {
        return Err(format!(
            "{context}: contradiction severity expected warning, got {}",
            drift.severity
        ));
    }
    if !gate.degraded_codes.contains(&expected_code.to_owned()) {
        return Err(format!(
            "{context}: gate degradedCodes must carry {expected_code}"
        ));
    }

    let rendered = serde_json::to_value(&gate)
        .map_err(|error| format!("{context}: serialize gate: {error}"))?;
    assert_no_forbidden_markers(&rendered, context)?;
    assert_next_actions_are_read_only(&rendered, context)?;

    Ok(())
}

#[test]
fn claim_gate_doctor_dirty_issues_fail_closed() -> TestResult {
    let fixture = tracker_authority_fixture("doctor_dirty_issues.json")?;
    let context = "doctor dirty-issues fixture";
    let report = report_from_doctor_fixture(&fixture, context)?;
    let snapshot = claim_ready_snapshot("bd-dirty", "Document tracker authority states");
    let (packet, gate) = packet_and_gate(report, &snapshot, "bd-dirty");

    let expected_health = string_at(&fixture, "/expected/trackerHealth", context)?;
    if gate.source_authority.tracker_health != expected_health {
        return Err(format!(
            "{context}: trackerHealth expected {expected_health}, got {}",
            gate.source_authority.tracker_health
        ));
    }
    if gate.source_authority.tracker_authoritative {
        return Err(format!(
            "{context}: dirty issues must force trackerAuthoritative=false"
        ));
    }
    if gate.safe_to_claim || gate.claim_command_action.is_some() {
        return Err(format!(
            "{context}: dirty issues must fail closed with claimCommandAction=null"
        ));
    }
    let expected_reason = format!("beads_tracker_not_authoritative:{expected_health}");
    if !gate.unsafe_reasons.contains(&expected_reason) {
        return Err(format!(
            "{context}: missing unsafe reason {expected_reason}; got {:?}",
            gate.unsafe_reasons
        ));
    }
    let (packet_signal, gate_signal) = gate_contradiction_signal(&packet, &gate);
    if packet_signal || gate_signal {
        return Err(format!(
            "{context}: concrete dirty evidence must not be reported as a metadata contradiction"
        ));
    }

    Ok(())
}

#[test]
fn claim_gate_dirty_checkout_with_clean_tracker_stays_unsafe_due_to_conflict() -> TestResult {
    let fixture = tracker_authority_fixture("doctor_metadata_message_only.json")?;
    let context = "dirty checkout with clean tracker";
    let report = report_from_doctor_fixture(&fixture, context)?;

    // The candidate title maps to the swarm next-action surfaces, and the
    // checkout has uncommitted work on one of them: dirty-surface conflict
    // evidence must keep the claim gate closed even though the tracker
    // itself is authoritative (metadata-message-only).
    let mut snapshot =
        claim_ready_snapshot("bd-conflict", "Polish swarm next-action conflict surfaces");
    snapshot.checkout.dirty_path_count = 1;
    snapshot.checkout.dirty_paths = vec!["src/core/swarm_next_action.rs".to_owned()];

    let (packet, gate) = packet_and_gate(report, &snapshot, "bd-conflict");

    if !gate.source_authority.tracker_authoritative {
        return Err(format!(
            "{context}: tracker must stay authoritative; unsafety must come from dirty surfaces"
        ));
    }
    if gate.source_authority.tracker_health != "doctor_metadata_message_only" {
        return Err(format!(
            "{context}: trackerHealth expected doctor_metadata_message_only, got {}",
            gate.source_authority.tracker_health
        ));
    }
    let candidate = packet
        .candidates
        .iter()
        .find(|candidate| candidate.id == "bd-conflict")
        .ok_or_else(|| format!("{context}: candidate must remain visible"))?;
    if candidate.decision != "unsafe_due_to_conflict" {
        return Err(format!(
            "{context}: candidate decision expected unsafe_due_to_conflict, got {}",
            candidate.decision
        ));
    }
    if gate.verdict != "unsafe_due_to_conflict" {
        return Err(format!(
            "{context}: gate verdict expected unsafe_due_to_conflict, got {}",
            gate.verdict
        ));
    }
    if gate.safe_to_claim || gate.claim_command_action.is_some() {
        return Err(format!(
            "{context}: dirty checkout must never become claim-safe via tracker authority"
        ));
    }
    if !gate
        .unsafe_reasons
        .iter()
        .any(|reason| reason.starts_with("dirty_path_overlap:"))
    {
        return Err(format!(
            "{context}: unsafe reasons must carry dirty_path_overlap evidence; got {:?}",
            gate.unsafe_reasons
        ));
    }
    if gate
        .unsafe_reasons
        .iter()
        .any(|reason| reason.starts_with("beads_tracker_not_authoritative"))
    {
        return Err(format!(
            "{context}: tracker authority must not be blamed for a dirty-checkout conflict"
        ));
    }

    Ok(())
}
