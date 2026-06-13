use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use std::cell::RefCell;

use ee::core::beads_integrity::{
    BeadsIntegrityInputs, BeadsIntegrityReport, JsonlParseError, compose_integrity_report,
    compose_integrity_report_from_br_doctor_json,
};
use ee::core::memory_drift;
use ee::core::swarm_brief::{
    SwarmBriefBead, SwarmBriefCollectOptions, SwarmBriefCommandError, SwarmBriefCommandOutput,
    SwarmBriefCommandRunner, SwarmBriefDegradation, SwarmBriefMemoryDriftSummary, SwarmBriefReport,
    SwarmBriefSourceFreshness, SwarmBriefSourceKind, SwarmBriefSourceProvenance,
    SwarmBriefSourceSnapshot, SwarmBriefSourceStatus,
};
use ee::core::swarm_next_action::{
    SWARM_NEXT_ACTION_REDACTION_STATUS, SWARM_NEXT_ACTION_SCHEMA_V1, SwarmNextActionCandidate,
    SwarmNextActionCheckoutSummary, SwarmNextActionCompileHealthSummary,
    SwarmNextActionCoordinationSummary, SwarmNextActionEnvironmentSummary,
    SwarmNextActionInputSummary, SwarmNextActionSnapshot, SwarmNextActionVerificationSummary,
    SwarmWorkPacket, SwarmWorkPacketActionableQueueEvidence,
    SwarmWorkPacketActionableQueueExclusionAccounting, SwarmWorkPacketClaimGate,
    actionable_queue_evidence_from_script_stdout, collect_work_packet_actionable_queue_evidence,
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
        "actionableQueue",
        "resourceAdmission",
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
        return Err(format!("{context} missing required resourceAdmission"));
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
        "actionableQueue": {
            "commandId": "beads_actionable_queue",
            "displayCommand": "scripts/br_retry.sh actionable --json",
            "mutatesState": false,
            "collectionMode": "br_retry_script",
            "queueState": "ready",
            "exitClass": "ok",
            "authoritative": true,
            "rowCount": 0,
            "candidateIds": [],
            "truncatedCandidateCount": 0,
            "filterContract": {
                "excludesEpics": true,
                "excludesAssigned": true,
                "excludesBlocked": true,
                "excludesDeferred": true,
                "excludesInProgress": true
            },
            "exclusionAccounting": {
                "rawReadyCount": 1,
                "excludedEpicCount": 0,
                "excludedAssignedCount": 1,
                "excludedBlockedCount": 0,
                "excludedDeferredCount": 0,
                "excludedInProgressCount": 0,
                "excludedOtherCount": 0
            },
            "candidateState": "candidate_absent_from_actionable",
            "bvAdvisoryContradiction": false,
            "trackerAuthorityDegraded": false,
            "contradictionEvidence": []
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

// ---------------------------------------------------------------------------
// Actionable-queue source authority (bd-3w4pv.7)
//
// `scripts/br_retry.sh actionable --json` is the safe claimable-leaf queue;
// raw `br ready` and `bv --robot-next` are broad/advisory. These fixtures pin
// the precedence contract: actionable presence is necessary but never
// sufficient, BV recommendations stay advisory, excluded raw rows are
// accounted instead of promoted, every evaluated failure state fails closed,
// and the collection path is read-only.
// ---------------------------------------------------------------------------

fn actionable_queue_fixture(file_name: &str) -> Result<Value, String> {
    read_json(&[
        "tests",
        "fixtures",
        "swarm_work_packet",
        "actionable_queue",
        file_name,
    ])
}

fn queue_label_at(value: &Value, pointer: &str, context: &str) -> Result<&'static str, String> {
    let raw = string_at(value, pointer, context)?;
    Ok(match raw {
        "not_evaluated" => "not_evaluated",
        "br_retry_script" => "br_retry_script",
        "brief_ready_filter" => "brief_ready_filter",
        "skipped_by_flag" => "skipped_by_flag",
        "ready" => "ready",
        "unavailable" => "unavailable",
        "timed_out" => "timed_out",
        "stale_fallback" => "stale_fallback",
        "ok" => "ok",
        "timeout" => "timeout",
        "spawn_failed" => "spawn_failed",
        "parse_failed" => "parse_failed",
        "unknown" => "unknown",
        other => return Err(format!("{context}: unsupported queue label {other}")),
    })
}

fn exclusion_accounting_from_fixture(
    value: &Value,
    context: &str,
) -> Result<SwarmWorkPacketActionableQueueExclusionAccounting, String> {
    Ok(SwarmWorkPacketActionableQueueExclusionAccounting {
        raw_ready_count: value.pointer("/rawReadyCount").and_then(Value::as_u64),
        excluded_epic_count: u64_at(value, "/excludedEpicCount", context)?,
        excluded_assigned_count: u64_at(value, "/excludedAssignedCount", context)?,
        excluded_blocked_count: u64_at(value, "/excludedBlockedCount", context)?,
        excluded_deferred_count: u64_at(value, "/excludedDeferredCount", context)?,
        excluded_in_progress_count: u64_at(value, "/excludedInProgressCount", context)?,
        excluded_other_count: u64_at(value, "/excludedOtherCount", context)?,
    })
}

fn actionable_queue_evidence_from_fixture(
    value: &Value,
    context: &str,
) -> Result<SwarmWorkPacketActionableQueueEvidence, String> {
    let accounting = value
        .pointer("/exclusionAccounting")
        .ok_or_else(|| format!("{context} missing exclusionAccounting"))?;
    Ok(SwarmWorkPacketActionableQueueEvidence {
        collection_mode: queue_label_at(value, "/collectionMode", context)?,
        queue_state: queue_label_at(value, "/queueState", context)?,
        exit_class: queue_label_at(value, "/exitClass", context)?,
        row_count: value.pointer("/rowCount").and_then(Value::as_u64),
        candidate_ids: string_array_at(value, "/candidateIds", context)?,
        exclusion_accounting: exclusion_accounting_from_fixture(accounting, context)?,
    })
}

fn clean_tracker_report() -> BeadsIntegrityReport {
    let merge_artifact_paths: &[String] = &[];
    compose_integrity_report(BeadsIntegrityInputs {
        jsonl_path: ".beads/issues.jsonl",
        db_path: ".beads/beads.db",
        jsonl_record_count: 5,
        db_record_count: 5,
        auto_import_enabled: true,
        external_changes_pending_import: false,
        dirty_issue_count: 0,
        merge_artifact_paths,
        jsonl_parse_error: None,
    })
}

fn packet_and_gate_with_actionable_queue(
    tracker_integrity: BeadsIntegrityReport,
    snapshot: &SwarmNextActionSnapshot,
    evidence: SwarmWorkPacketActionableQueueEvidence,
    candidate_id: &str,
) -> (SwarmWorkPacket, SwarmWorkPacketClaimGate) {
    let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
    let mut packet = SwarmWorkPacket::from_brief_and_next_action_with_tracker_integrity(
        &brief,
        snapshot,
        tracker_integrity,
    );
    packet.apply_claim_gate_actionable_queue(evidence);
    let gate = packet.claim_gate(Some(candidate_id));
    (packet, gate)
}

fn brief_bead_from_fixture(
    value: &Value,
    source_bucket: &str,
    context: &str,
) -> Result<SwarmBriefBead, String> {
    Ok(SwarmBriefBead {
        id: string_at(value, "/id", context)?.to_owned(),
        title: string_at(value, "/title", context)?.to_owned(),
        status: string_at(value, "/status", context)?.to_owned(),
        priority: Some(2),
        assignee: value
            .pointer("/assignee")
            .and_then(Value::as_str)
            .map(str::to_owned),
        issue_type: value
            .pointer("/issueType")
            .and_then(Value::as_str)
            .map(str::to_owned),
        created_at: None,
        updated_at: None,
        latest_comment_at: None,
        comment_count: 0,
        source_bucket: source_bucket.to_owned(),
    })
}

#[test]
fn claim_gate_actionable_presence_is_necessary_but_not_sufficient() -> TestResult {
    let fixture = actionable_queue_fixture("present_but_gate_refuses.json")?;
    let context = "actionable present-but-refused fixture";
    let candidate_id = string_at(&fixture, "/candidateId", context)?;
    let candidate_title = string_at(&fixture, "/candidateTitle", context)?;
    let tracker = fixture
        .pointer("/tracker")
        .ok_or_else(|| format!("{context} missing tracker inputs"))?;
    let report = report_from_matrix_case(tracker, context)?;
    let evidence = actionable_queue_evidence_from_fixture(
        fixture
            .pointer("/evidence")
            .ok_or_else(|| format!("{context} missing evidence"))?,
        context,
    )?;

    let mut snapshot = claim_ready_snapshot(candidate_id, candidate_title);
    let dirty_paths = string_array_at(&fixture, "/dirtyPaths", context)?;
    snapshot.checkout.dirty_path_count = dirty_paths.len();
    snapshot.checkout.dirty_paths = dirty_paths;

    let (_, gate) =
        packet_and_gate_with_actionable_queue(report, &snapshot, evidence, candidate_id);

    let expected_state = string_at(&fixture, "/expected/candidateState", context)?;
    if gate.actionable_queue.candidate_state != expected_state {
        return Err(format!(
            "{context}: candidateState expected {expected_state}, got {}",
            gate.actionable_queue.candidate_state
        ));
    }
    if !gate.actionable_queue.tracker_authority_degraded {
        return Err(format!(
            "{context}: evaluated queue with a dirty tracker must mark trackerAuthorityDegraded"
        ));
    }
    if gate.actionable_queue.authoritative {
        return Err(format!(
            "{context}: the queue inherits degraded tracker authority and must not stay authoritative"
        ));
    }
    let expected_health = string_at(&fixture, "/expected/trackerHealth", context)?;
    if gate.source_authority.tracker_health != expected_health {
        return Err(format!(
            "{context}: trackerHealth expected {expected_health}, got {}",
            gate.source_authority.tracker_health
        ));
    }
    let expected_verdict = string_at(&fixture, "/expected/verdict", context)?;
    if gate.verdict != expected_verdict {
        return Err(format!(
            "{context}: verdict expected {expected_verdict}, got {}",
            gate.verdict
        ));
    }
    if gate.safe_to_claim || gate.claim_command_action.is_some() {
        return Err(format!(
            "{context}: actionable presence must never be sufficient — claimCommandAction must stay null"
        ));
    }
    let tracker_reason = string_at(&fixture, "/expected/trackerUnsafeReason", context)?;
    if !gate
        .unsafe_reasons
        .iter()
        .any(|reason| reason == tracker_reason)
    {
        return Err(format!(
            "{context}: missing unsafe reason {tracker_reason}; got {:?}",
            gate.unsafe_reasons
        ));
    }
    if !gate
        .unsafe_reasons
        .iter()
        .any(|reason| reason.starts_with("dirty_path_overlap:"))
    {
        return Err(format!(
            "{context}: dirty-surface conflict evidence must stay visible alongside queue presence"
        ));
    }
    let degraded_code = string_at(&fixture, "/expected/degradedCode", context)?;
    if !gate.degraded_codes.iter().any(|code| code == degraded_code) {
        return Err(format!(
            "{context}: degradedCodes must carry {degraded_code}; got {:?}",
            gate.degraded_codes
        ));
    }

    let rendered = serde_json::to_value(&gate)
        .map_err(|error| format!("{context}: serialize gate: {error}"))?;
    assert_no_forbidden_markers(&rendered, context)?;
    assert_next_actions_are_read_only(&rendered, context)?;

    Ok(())
}

#[test]
fn claim_gate_marks_bv_advisory_contradiction_without_claim_command() -> TestResult {
    let fixture = actionable_queue_fixture("bv_advisory_contradiction.json")?;
    let context = "bv advisory contradiction fixture";
    let candidate_id = string_at(&fixture, "/candidateId", context)?;
    let candidate_title = string_at(&fixture, "/candidateTitle", context)?;
    let tracker = fixture
        .pointer("/tracker")
        .ok_or_else(|| format!("{context} missing tracker inputs"))?;
    let report = report_from_matrix_case(tracker, context)?;
    let evidence = actionable_queue_evidence_from_fixture(
        fixture
            .pointer("/evidence")
            .ok_or_else(|| format!("{context} missing evidence"))?,
        context,
    )?;

    let mut snapshot = claim_ready_snapshot(candidate_id, candidate_title);
    snapshot.candidates[0].source = "bv_top_pick";
    snapshot.candidates[0].status = string_at(&fixture, "/candidateStatus", context)?.to_owned();
    snapshot.candidates[0].blocked_by = string_array_at(&fixture, "/candidateBlockedBy", context)?;

    let (_, gate) =
        packet_and_gate_with_actionable_queue(report, &snapshot, evidence, candidate_id);

    let expected_verdict = string_at(&fixture, "/expected/verdict", context)?;
    if gate.verdict != expected_verdict {
        return Err(format!(
            "{context}: verdict expected {expected_verdict}, got {}",
            gate.verdict
        ));
    }
    if gate.safe_to_claim || gate.claim_command_action.is_some() {
        return Err(format!(
            "{context}: a BV-contradicted candidate must never receive a claim command"
        ));
    }
    if !gate.actionable_queue.bv_advisory_contradiction {
        return Err(format!(
            "{context}: bvAdvisoryContradiction must be marked for the selected BV pick"
        ));
    }
    let expected_state = string_at(&fixture, "/expected/candidateState", context)?;
    if gate.actionable_queue.candidate_state != expected_state {
        return Err(format!(
            "{context}: candidateState expected {expected_state}, got {}",
            gate.actionable_queue.candidate_state
        ));
    }
    let expected_evidence = string_array_at(&fixture, "/expected/contradictionEvidence", context)?;
    if gate.actionable_queue.contradiction_evidence != expected_evidence {
        return Err(format!(
            "{context}: contradiction evidence drifted\nactual: {:?}\nexpected: {expected_evidence:?}",
            gate.actionable_queue.contradiction_evidence
        ));
    }
    let unsafe_reason = string_at(&fixture, "/expected/unsafeReason", context)?;
    if !gate
        .unsafe_reasons
        .iter()
        .any(|reason| reason == unsafe_reason)
    {
        return Err(format!(
            "{context}: missing unsafe reason {unsafe_reason}; got {:?}",
            gate.unsafe_reasons
        ));
    }
    let degraded_code = string_at(&fixture, "/expected/degradedCode", context)?;
    if !gate.degraded_codes.iter().any(|code| code == degraded_code) {
        return Err(format!(
            "{context}: degradedCodes must carry {degraded_code}; got {:?}",
            gate.degraded_codes
        ));
    }

    let rendered = serde_json::to_value(&gate)
        .map_err(|error| format!("{context}: serialize gate: {error}"))?;
    assert_no_forbidden_markers(&rendered, context)?;

    Ok(())
}

#[test]
fn claim_gate_actionable_queue_exclusion_accounting_matches_golden() -> TestResult {
    let fixture = actionable_queue_fixture("ready_epic_exclusion.json")?;
    let context = "actionable exclusion accounting fixture";
    let candidate_id = string_at(&fixture, "/candidateId", context)?;

    let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
    for (bucket, pointer) in [
        ("ready", "/brief/ready"),
        ("blocked", "/brief/blocked"),
        ("in_progress", "/brief/inProgress"),
        ("deferred", "/brief/deferred"),
    ] {
        let rows = fixture
            .pointer(pointer)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{context} missing {pointer}"))?;
        for row in rows {
            let bead = brief_bead_from_fixture(row, bucket, context)?;
            match bucket {
                "ready" => brief.beads.ready.push(bead),
                "blocked" => brief.beads.blocked.push(bead),
                "in_progress" => brief.beads.in_progress.push(bead),
                _ => brief.beads.deferred.push(bead),
            }
        }
    }

    let stdout = string_at(&fixture, "/scriptStdout", context)?;
    let evidence = actionable_queue_evidence_from_script_stdout(stdout, &brief);
    if evidence.queue_state != "ready" || evidence.exit_class != "ok" {
        return Err(format!(
            "{context}: parsed script stdout must yield a ready queue, got {} / {}",
            evidence.queue_state, evidence.exit_class
        ));
    }

    let tracker = fixture
        .pointer("/tracker")
        .ok_or_else(|| format!("{context} missing tracker inputs"))?;
    let report = report_from_matrix_case(tracker, context)?;
    let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
    let mut packet = SwarmWorkPacket::from_brief_and_next_action_with_tracker_integrity(
        &brief, &snapshot, report,
    );
    packet.apply_claim_gate_actionable_queue(evidence);

    let gate = packet.claim_gate(Some(candidate_id));
    let rendered = serde_json::to_value(&gate)
        .map_err(|error| format!("{context}: serialize gate: {error}"))?;
    let actual_block = rendered
        .pointer("/actionableQueue")
        .ok_or_else(|| format!("{context}: gate missing actionableQueue block"))?;
    let expected_block = fixture
        .pointer("/expectedActionableQueue")
        .ok_or_else(|| format!("{context} missing expectedActionableQueue golden"))?;
    if actual_block != expected_block {
        return Err(format!(
            "{context}: actionableQueue golden drifted\nactual: {actual_block:#}\nexpected: {expected_block:#}"
        ));
    }

    // The excluded parent epic stays visible as exclusion accounting and a
    // concrete absence reason — it is never promoted into a claim.
    let excluded_id = string_at(&fixture, "/excludedCandidateId", context)?;
    let excluded_gate = packet.claim_gate(Some(excluded_id));
    let expected_excluded_state = string_at(&fixture, "/expectedExcludedCandidateState", context)?;
    if excluded_gate.actionable_queue.candidate_state != expected_excluded_state {
        return Err(format!(
            "{context}: excluded candidateState expected {expected_excluded_state}, got {}",
            excluded_gate.actionable_queue.candidate_state
        ));
    }
    if excluded_gate.safe_to_claim || excluded_gate.claim_command_action.is_some() {
        return Err(format!(
            "{context}: a row the actionable queue excludes must never become claimable"
        ));
    }
    let expected_excluded_reason = string_at(&fixture, "/expectedExcludedUnsafeReason", context)?;
    if !excluded_gate
        .unsafe_reasons
        .iter()
        .any(|reason| reason == expected_excluded_reason)
    {
        return Err(format!(
            "{context}: missing excluded unsafe reason {expected_excluded_reason}; got {:?}",
            excluded_gate.unsafe_reasons
        ));
    }

    assert_no_forbidden_markers(&rendered, context)?;

    Ok(())
}

struct RecordingRunner {
    stdout: String,
    calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl SwarmBriefCommandRunner for RecordingRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        _cwd: &Path,
        _timeout_ms: u64,
    ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
        self.calls.borrow_mut().push((
            program.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        ));
        Ok(SwarmBriefCommandOutput {
            stdout: self.stdout.clone(),
            stderr: String::new(),
        })
    }
}

#[test]
fn actionable_queue_collection_is_read_only_and_records_command_ids() -> TestResult {
    let fixture = actionable_queue_fixture("no_mutation_read_only.json")?;
    let context = "actionable no-mutation fixture";
    let stdout = string_at(&fixture, "/scriptStdout", context)?;

    // The repo root ships scripts/br_retry.sh, so the collector takes the
    // first-class script path; the recording runner proves the only command
    // it ever issues is the read-only queue probe.
    let options = SwarmBriefCollectOptions::for_workspace(repo_root());
    let runner = RecordingRunner {
        stdout: stdout.to_owned(),
        calls: RefCell::new(Vec::new()),
    };
    let brief = SwarmBriefReport::empty(&repo_root());
    let evidence = collect_work_packet_actionable_queue_evidence(&options, &runner, &brief);

    let calls = runner.calls.into_inner();
    if calls.len() != 1 {
        return Err(format!(
            "{context}: collection must issue exactly one command, got {calls:?}"
        ));
    }
    let expected_program = string_at(&fixture, "/expected/commandProgram", context)?;
    let expected_args = string_array_at(&fixture, "/expected/commandArgs", context)?;
    if calls[0].0 != expected_program || calls[0].1 != expected_args {
        return Err(format!(
            "{context}: collection command drifted: {:?} {:?}",
            calls[0].0, calls[0].1
        ));
    }
    let forbidden_markers = string_array_at(&fixture, "/expected/forbiddenArgvMarkers", context)?;
    for (program, args) in &calls {
        for marker in &forbidden_markers {
            if program.contains(marker.as_str())
                || args.iter().any(|arg| arg.contains(marker.as_str()))
            {
                return Err(format!(
                    "{context}: read-only collection must never issue `{marker}` commands"
                ));
            }
        }
    }
    if evidence.queue_state != "ready" || evidence.collection_mode != "br_retry_script" {
        return Err(format!(
            "{context}: script-path collection must report ready/br_retry_script, got {}/{}",
            evidence.queue_state, evidence.collection_mode
        ));
    }

    let snapshot = claim_ready_snapshot("bd-aq-leaf", "Document a small schema improvement");
    let (packet, gate) = packet_and_gate_with_actionable_queue(
        clean_tracker_report(),
        &snapshot,
        evidence,
        "bd-aq-leaf",
    );

    if !packet.mutation_policy.side_effect_free
        || packet.mutation_policy.claims_beads
        || packet.mutation_policy.reserves_files
        || packet.mutation_policy.sends_agent_mail
        || packet.mutation_policy.runs_cargo
        || packet.mutation_policy.stages_git
        || packet.mutation_policy.deletes_files
    {
        return Err(format!(
            "{context}: the work packet mutation policy must stay fully read-only"
        ));
    }

    let expected_command_id = string_at(&fixture, "/expected/commandId", context)?;
    if gate.actionable_queue.command_id != expected_command_id {
        return Err(format!(
            "{context}: evidence block command id expected {expected_command_id}, got {}",
            gate.actionable_queue.command_id
        ));
    }
    let expected_display = string_at(&fixture, "/expected/displayCommand", context)?;
    if gate.actionable_queue.display_command != expected_display {
        return Err(format!(
            "{context}: evidence block display command drifted: {}",
            gate.actionable_queue.display_command
        ));
    }
    if gate.actionable_queue.mutates_state {
        return Err(format!(
            "{context}: the actionable-queue evidence block must record mutatesState=false"
        ));
    }

    // Positive control: with clean tracker, clean checkout, healthy RCH, and
    // the candidate present in the actionable queue, the gate still issues a
    // claim command — the queue integration fails closed, not always-closed.
    if !gate.safe_to_claim || gate.claim_command_action.is_none() {
        return Err(format!(
            "{context}: fully-agreeing sources must keep the candidate claimable, got verdict {}",
            gate.verdict
        ));
    }

    let rendered = serde_json::to_value(&gate)
        .map_err(|error| format!("{context}: serialize gate: {error}"))?;
    assert_no_forbidden_markers(&rendered, context)?;
    assert_next_actions_are_read_only(&rendered, context)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Memory-drift lock contention (bd-koag5)
//
// bd-1xpq9 defines the public contract: lock contention is not stale
// memory evidence because the collector never inspected evidence. bd-14cue
// made the interim collector strategy explicit. These fixtures pin the
// claim-gate/work-packet projection so future agents can distinguish
// `memory_drift_lock_contention` from ordinary
// `memory_drift_source_unverifiable`, and so the degraded path stays
// read-only.
// ---------------------------------------------------------------------------

fn memory_drift_lock_fixture(file_name: &str) -> Result<Value, String> {
    read_json(&[
        "tests",
        "fixtures",
        "swarm_work_packet",
        "memory_drift_lock_contention",
        file_name,
    ])
}

fn source_status_from_fixture(
    value: &Value,
    pointer: &str,
    context: &str,
) -> Result<SwarmBriefSourceStatus, String> {
    Ok(match string_at(value, pointer, context)? {
        "ready" => SwarmBriefSourceStatus::Ready,
        "degraded" => SwarmBriefSourceStatus::Degraded,
        "unavailable" => SwarmBriefSourceStatus::Unavailable,
        other => return Err(format!("{context}: unsupported source status {other}")),
    })
}

fn optional_string_array_at(
    value: &Value,
    pointer: &str,
    context: &str,
) -> Result<Vec<String>, String> {
    if value.pointer(pointer).is_none_or(Value::is_null) {
        Ok(Vec::new())
    } else {
        string_array_at(value, pointer, context)
    }
}

fn u32_at(value: &Value, pointer: &str, context: &str) -> Result<u32, String> {
    let raw = u64_at(value, pointer, context)?;
    u32::try_from(raw).map_err(|_| format!("{context}: {pointer} value {raw} exceeds u32::MAX"))
}

fn memory_drift_summary_from_fixture(
    value: &Value,
    context: &str,
) -> Result<Option<SwarmBriefMemoryDriftSummary>, String> {
    let Some(summary) = value.pointer("/memoryDriftSource/summary") else {
        return Ok(None);
    };
    if summary.is_null() {
        return Ok(None);
    }
    Ok(Some(SwarmBriefMemoryDriftSummary {
        status: string_at(summary, "/status", context)?.to_owned(),
        report_mode: string_at(summary, "/reportMode", context)?.to_owned(),
        total_memories: u32_at(summary, "/totalMemories", context)?,
        current_count: u32_at(summary, "/currentCount", context)?,
        changed_count: u32_at(summary, "/changedCount", context)?,
        missing_source_count: u32_at(summary, "/missingSourceCount", context)?,
        stale_anchor_count: u32_at(summary, "/staleAnchorCount", context)?,
        unverifiable_count: u32_at(summary, "/unverifiableCount", context)?,
        suppressed_count: u32_at(summary, "/suppressedCount", context)?,
        affected_count: u32_at(summary, "/affectedCount", context)?,
        top_affected_memory_ids: optional_string_array_at(
            summary,
            "/topAffectedMemoryIds",
            context,
        )?,
        degraded_codes: optional_string_array_at(summary, "/degradedCodes", context)?,
        source_kind_counts: serde_json::from_value(
            summary
                .get("sourceKindCounts")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .map_err(|error| format!("{context}: sourceKindCounts malformed: {error}"))?,
    }))
}

fn memory_drift_degradation_from_case(
    value: &Value,
    context: &str,
) -> Result<Option<SwarmBriefDegradation>, String> {
    let code = value.pointer("/memoryDriftSource/degradedCode");
    let Some(code) = code.and_then(Value::as_str) else {
        return Ok(None);
    };
    let message = if code == memory_drift::MEMORY_DRIFT_LOCK_CONTENTION_CODE {
        memory_drift::memory_drift_lock_contention_message("swarm_brief")
    } else {
        string_at(value, "/memoryDriftSource/message", context)?.to_owned()
    };
    let repair = if code == memory_drift::MEMORY_DRIFT_LOCK_CONTENTION_CODE {
        Some(memory_drift::MEMORY_DRIFT_LOCK_CONTENTION_REPAIR.to_owned())
    } else {
        value
            .pointer("/memoryDriftSource/repair")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Ok(Some(SwarmBriefDegradation::warning(
        SwarmBriefSourceKind::MemoryDrift,
        code.to_owned(),
        message,
        repair,
    )))
}

fn memory_drift_report_from_case(value: &Value, context: &str) -> Result<SwarmBriefReport, String> {
    let candidate_id = string_at(value, "/candidateId", context)?;
    let candidate_title = string_at(value, "/candidateTitle", context)?;
    let source_status = source_status_from_fixture(value, "/memoryDriftSource/status", context)?;
    let source_degradation = memory_drift_degradation_from_case(value, context)?;
    let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
    brief.beads.ready.push(SwarmBriefBead {
        id: candidate_id.to_owned(),
        title: candidate_title.to_owned(),
        status: "open".to_owned(),
        priority: Some(2),
        assignee: None,
        issue_type: Some("task".to_owned()),
        created_at: None,
        updated_at: None,
        latest_comment_at: None,
        comment_count: 0,
        source_bucket: "ready".to_owned(),
    });

    let degraded = source_degradation.into_iter().collect::<Vec<_>>();
    brief.sources.push(SwarmBriefSourceSnapshot {
        source: SwarmBriefSourceKind::MemoryDrift,
        status: source_status,
        freshness: if source_status == SwarmBriefSourceStatus::Ready {
            SwarmBriefSourceFreshness::current()
        } else {
            SwarmBriefSourceFreshness::unknown()
        },
        provenance: SwarmBriefSourceProvenance::local_probe(),
        item_count: usize::try_from(u64_at(value, "/memoryDriftSource/itemCount", context)?)
            .map_err(|_| format!("{context}: itemCount exceeds usize::MAX"))?,
        degraded: degraded.clone(),
    });
    brief.degraded = degraded;
    brief.memory_drift = memory_drift_summary_from_fixture(value, context)?;
    brief.finalize();
    Ok(brief)
}

fn memory_drift_packet_and_gate_from_case(
    value: &Value,
    context: &str,
) -> Result<(SwarmWorkPacket, SwarmWorkPacketClaimGate), String> {
    let candidate_id = string_at(value, "/candidateId", context)?;
    let brief = memory_drift_report_from_case(value, context)?;
    let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
    let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
    let gate = packet.claim_gate(Some(candidate_id));
    Ok((packet, gate))
}

fn lock_contention_claim_gate_projection(
    packet: &SwarmWorkPacket,
    gate: &SwarmWorkPacketClaimGate,
) -> Value {
    json!({
        "schema": gate.schema,
        "requestedCandidateId": &gate.requested_candidate_id,
        "verdict": gate.verdict,
        "safeToClaim": gate.safe_to_claim,
        "recommendedAction": gate.recommended_action,
        "recommendedSafeToClaim": &gate.recommended_safe_to_claim,
        "selectedCandidate": gate.selected_candidate.as_ref().map(|candidate| json!({
            "id": &candidate.id,
            "decision": candidate.decision,
            "collisionRisk": candidate.collision_risk,
        })),
        "sourceAuthority": {
            "environmentVerdict": gate.source_authority.environment_verdict,
            "sourceTestVerdict": gate.source_authority.source_test_verdict,
            "remoteVerificationAdmitted": &gate.source_authority.remote_verification_admitted,
        },
        "unsafeReasons": &gate.unsafe_reasons,
        "degradedCodes": &gate.degraded_codes,
        "claimCommandAction": &gate.claim_command_action,
        "packetRecommendedAction": {
            "action": packet.recommended_action.action,
            "safeToClaim": &packet.recommended_action.safe_to_claim,
            "reasons": &packet.recommended_action.reasons,
            "proofObligations": &packet.recommended_action.proof_obligations,
        },
        "packetMutationPolicy": &packet.mutation_policy,
        "packetDegraded": &packet.degraded,
    })
}

#[test]
fn memory_drift_lock_contention_conformance_matrix_matches_contract() -> TestResult {
    let fixture = memory_drift_lock_fixture("conformance_matrix.json")?;
    let requirements = fixture
        .pointer("/requirements")
        .and_then(Value::as_array)
        .ok_or_else(|| "memory-drift conformance fixture missing requirements[]".to_owned())?;
    if requirements.len() != 8 {
        return Err(format!(
            "bd-koag5 compliance matrix must pin 8 MUST/SHOULD clauses, found {}",
            requirements.len()
        ));
    }
    let cases = fixture
        .pointer("/cases")
        .and_then(Value::as_array)
        .ok_or_else(|| "memory-drift conformance fixture missing cases[]".to_owned())?;
    if cases.len() != 3 {
        return Err(format!(
            "bd-koag5 fixture must cover lock, no-lock, and inspected-unverifiable cases; found {}",
            cases.len()
        ));
    }

    for case in cases {
        let name = string_at(case, "/name", "memory-drift matrix case")?;
        let context = format!("memory-drift matrix case {name}");
        let (packet, gate) = memory_drift_packet_and_gate_from_case(case, &context)?;
        let rendered_gate = serde_json::to_value(&gate)
            .map_err(|error| format!("{context}: serialize gate: {error}"))?;
        let rendered_packet = serde_json::to_value(&packet)
            .map_err(|error| format!("{context}: serialize packet: {error}"))?;
        assert_no_forbidden_markers(&rendered_gate, &context)?;
        assert_no_forbidden_markers(&rendered_packet, &context)?;

        let forbidden_codes = optional_string_array_at(case, "/expected/forbiddenCodes", &context)?;
        for code in forbidden_codes {
            if gate.degraded_codes.iter().any(|actual| actual == &code)
                || gate.unsafe_reasons.iter().any(|actual| actual == &code)
            {
                return Err(format!(
                    "{context}: forbidden code {code} appeared in gate projection"
                ));
            }
        }

        let expected_codes = optional_string_array_at(case, "/expected/degradedCodes", &context)?;
        for code in expected_codes {
            if !gate.degraded_codes.iter().any(|actual| actual == &code) {
                return Err(format!(
                    "{context}: missing degraded code {code}; got {:?}",
                    gate.degraded_codes
                ));
            }
        }

        let expected_reasons = optional_string_array_at(case, "/expected/unsafeReasons", &context)?;
        for reason in expected_reasons {
            if !gate.unsafe_reasons.iter().any(|actual| actual == &reason) {
                return Err(format!(
                    "{context}: missing unsafe reason {reason}; got {:?}",
                    gate.unsafe_reasons
                ));
            }
        }

        if let Some(expected_safe) = case
            .pointer("/expected/safeToClaim")
            .and_then(Value::as_bool)
        {
            if gate.safe_to_claim != expected_safe {
                return Err(format!(
                    "{context}: safeToClaim expected {expected_safe}, got {}",
                    gate.safe_to_claim
                ));
            }
        }
        if let Some(expected_verdict) = case.pointer("/expected/verdict").and_then(Value::as_str) {
            if gate.verdict != expected_verdict {
                return Err(format!(
                    "{context}: verdict expected {expected_verdict}, got {}",
                    gate.verdict
                ));
            }
        }
        if case
            .pointer("/expected/claimCommandActionNull")
            .and_then(Value::as_bool)
            == Some(true)
            && gate.claim_command_action.is_some()
        {
            return Err(format!(
                "{context}: claimCommandAction must stay null on lock-contention gate"
            ));
        }
        if let Some(expected_action) = case
            .pointer("/expected/recommendedAction")
            .and_then(Value::as_str)
            && packet.recommended_action.action != expected_action
        {
            return Err(format!(
                "{context}: recommendedAction expected {expected_action}, got {}",
                packet.recommended_action.action
            ));
        }
        if let Some(expected_env) = case
            .pointer("/expected/environmentVerdict")
            .and_then(Value::as_str)
            && gate.source_authority.environment_verdict != expected_env
        {
            return Err(format!(
                "{context}: environmentVerdict expected {expected_env}, got {}",
                gate.source_authority.environment_verdict
            ));
        }
    }

    Ok(())
}

#[test]
fn memory_drift_lock_contention_details_and_golden_projection_are_stable() -> TestResult {
    let fixture = memory_drift_lock_fixture("conformance_matrix.json")?;
    let case = fixture
        .pointer("/cases")
        .and_then(Value::as_array)
        .and_then(|cases| {
            cases.iter().find(|case| {
                case.pointer("/name").and_then(Value::as_str)
                    == Some("lock_contention_before_evidence")
            })
        })
        .ok_or_else(|| "lock-contention case missing from fixture".to_owned())?;
    let context = "memory-drift lock-contention golden projection";
    let details = memory_drift::memory_drift_lock_contention_details("swarm_brief");
    let expected_details = case
        .pointer("/memoryDriftSource/expectedDetails")
        .ok_or_else(|| format!("{context}: missing expectedDetails"))?;
    if &details != expected_details {
        return Err(format!(
            "{context}: lock-contention details drifted\nactual: {details:#}\nexpected: {expected_details:#}"
        ));
    }

    let message = memory_drift::memory_drift_lock_contention_message("swarm_brief");
    for needle in string_array_at(
        case,
        "/memoryDriftSource/expectedMessageSubstrings",
        context,
    )? {
        if !message.contains(&needle) {
            return Err(format!(
                "{context}: message missing substring {needle:?}: {message}"
            ));
        }
    }
    assert_no_forbidden_markers(&details, context)?;

    let (packet, gate) = memory_drift_packet_and_gate_from_case(case, context)?;
    let projection = lock_contention_claim_gate_projection(&packet, &gate);
    let golden = fixture
        .pointer("/goldenClaimGateProjection")
        .ok_or_else(|| format!("{context}: missing goldenClaimGateProjection"))?;
    if &projection != golden {
        return Err(format!(
            "{context}: golden projection drifted\nactual: {projection:#}\nexpected: {golden:#}"
        ));
    }
    let first = serde_json::to_string(&projection)
        .map_err(|error| format!("{context}: serialize projection first: {error}"))?;
    let second = serde_json::to_string(&projection)
        .map_err(|error| format!("{context}: serialize projection second: {error}"))?;
    if first != second {
        return Err(format!(
            "{context}: projection serialization is not byte-stable"
        ));
    }
    Ok(())
}

#[test]
fn memory_drift_lock_contention_no_mutation_guard_is_explicit() -> TestResult {
    let fixture = memory_drift_lock_fixture("no_mutation_guard.json")?;
    let tempdir = tempfile::tempdir().map_err(|error| format!("create tempdir: {error}"))?;
    let sentinels = fixture
        .pointer("/sentinels")
        .and_then(Value::as_array)
        .ok_or_else(|| "no-mutation fixture missing sentinels[]".to_owned())?;
    for sentinel in sentinels {
        let file_name = string_at(sentinel, "/fileName", "no-mutation sentinel")?;
        let before = string_at(sentinel, "/before", "no-mutation sentinel")?;
        fs::write(tempdir.path().join(file_name), before)
            .map_err(|error| format!("write sentinel {file_name}: {error}"))?;
    }

    let matrix = memory_drift_lock_fixture("conformance_matrix.json")?;
    let case = matrix
        .pointer("/cases")
        .and_then(Value::as_array)
        .and_then(|cases| {
            cases.iter().find(|case| {
                case.pointer("/name").and_then(Value::as_str)
                    == Some("lock_contention_before_evidence")
            })
        })
        .ok_or_else(|| "lock-contention case missing from fixture".to_owned())?;
    let (packet, gate) = memory_drift_packet_and_gate_from_case(
        case,
        "memory-drift no-mutation lock-contention case",
    )?;

    if !packet.mutation_policy.side_effect_free
        || packet.mutation_policy.claims_beads
        || packet.mutation_policy.reserves_files
        || packet.mutation_policy.sends_agent_mail
        || packet.mutation_policy.runs_cargo
        || packet.mutation_policy.stages_git
        || packet.mutation_policy.deletes_files
    {
        return Err("memory-drift lock-contention packet must remain fully read-only".into());
    }
    if gate.claim_command_action.is_some() {
        return Err(
            "memory-drift lock-contention gate must not emit a mutating claim action".into(),
        );
    }
    for action in &gate.next_command_actions {
        if action.mutates_state {
            return Err(format!(
                "nextCommandActions must be read-only, got mutating action {}",
                action.command_id
            ));
        }
    }

    for sentinel in sentinels {
        let file_name = string_at(sentinel, "/fileName", "no-mutation sentinel")?;
        let expected = string_at(sentinel, "/after", "no-mutation sentinel")?;
        let actual = fs::read_to_string(tempdir.path().join(file_name))
            .map_err(|error| format!("read sentinel {file_name}: {error}"))?;
        if actual != expected {
            return Err(format!(
                "sentinel {file_name} mutated\nexpected: {expected}\nactual: {actual}"
            ));
        }
    }
    Ok(())
}

#[test]
fn memory_drift_lock_contention_composes_with_tracker_and_dirty_blockers() -> TestResult {
    let fixture = memory_drift_lock_fixture("conformance_matrix.json")?;
    let case = fixture
        .pointer("/cases")
        .and_then(Value::as_array)
        .and_then(|cases| {
            cases.iter().find(|case| {
                case.pointer("/name").and_then(Value::as_str)
                    == Some("lock_contention_before_evidence")
            })
        })
        .ok_or_else(|| "lock-contention case missing from fixture".to_owned())?;
    let context = "memory-drift lock-contention multi-blocker composition";
    let candidate_id = string_at(case, "/candidateId", context)?;
    let mut brief = memory_drift_report_from_case(case, context)?;
    brief.beads.ready[0].title = "Polish swarm next-action conflict surfaces".to_owned();

    let mut snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
    snapshot.checkout.dirty_path_count = 1;
    snapshot.checkout.dirty_paths = vec!["src/core/swarm_next_action.rs".to_owned()];

    let merge_artifact_paths: &[String] = &[];
    let tracker = compose_integrity_report(BeadsIntegrityInputs {
        jsonl_path: ".beads/issues.jsonl",
        db_path: ".beads/beads.db",
        jsonl_record_count: 5,
        db_record_count: 5,
        auto_import_enabled: true,
        external_changes_pending_import: false,
        dirty_issue_count: 1,
        merge_artifact_paths,
        jsonl_parse_error: None,
    });
    let packet = SwarmWorkPacket::from_brief_and_next_action_with_tracker_integrity(
        &brief, &snapshot, tracker,
    );
    let gate = packet.claim_gate(Some(candidate_id));

    if gate.safe_to_claim || gate.claim_command_action.is_some() {
        return Err(format!(
            "{context}: multi-blocker lock-contention gate must stay closed"
        ));
    }
    for expected in [
        memory_drift::MEMORY_DRIFT_LOCK_CONTENTION_CODE,
        "beads_tracker_not_authoritative:dirty_issues",
        "dirty_checkout_path_count:1",
        "dirty_path_overlap:src/core/swarm_next_action.rs",
    ] {
        if !gate.unsafe_reasons.iter().any(|reason| reason == expected) {
            return Err(format!(
                "{context}: missing unsafe reason {expected}; got {:?}",
                gate.unsafe_reasons
            ));
        }
    }
    if !gate
        .degraded_codes
        .iter()
        .any(|code| code == memory_drift::MEMORY_DRIFT_LOCK_CONTENTION_CODE)
    {
        return Err(format!(
            "{context}: degradedCodes must retain raw lock code; got {:?}",
            gate.degraded_codes
        ));
    }
    let rendered = serde_json::to_value(&gate)
        .map_err(|error| format!("{context}: serialize gate: {error}"))?;
    assert_no_forbidden_markers(&rendered, context)?;
    Ok(())
}

#[test]
fn claim_gate_actionable_queue_failure_states_fail_closed() -> TestResult {
    let fixture = actionable_queue_fixture("failure_states.json")?;
    let candidate_id = string_at(&fixture, "/candidateId", "actionable failure fixture")?;
    let candidate_title = string_at(&fixture, "/candidateTitle", "actionable failure fixture")?;
    let cases = fixture
        .pointer("/cases")
        .and_then(Value::as_array)
        .ok_or_else(|| "actionable failure fixture missing cases[]".to_owned())?;
    if cases.len() != 4 {
        return Err(format!(
            "actionable failure fixture must pin 4 distinct states, found {}",
            cases.len()
        ));
    }

    for case in cases {
        let name = string_at(case, "/name", "actionable failure case")?;
        let context = format!("actionable failure case {name}");
        let evidence = actionable_queue_evidence_from_fixture(
            case.pointer("/evidence")
                .ok_or_else(|| format!("{context} missing evidence"))?,
            &context,
        )?;
        let snapshot = claim_ready_snapshot(candidate_id, candidate_title);
        let (_, gate) = packet_and_gate_with_actionable_queue(
            clean_tracker_report(),
            &snapshot,
            evidence,
            candidate_id,
        );

        let expected_state = string_at(case, "/expected/candidateState", &context)?;
        if gate.actionable_queue.candidate_state != expected_state {
            return Err(format!(
                "{context}: candidateState expected {expected_state}, got {}",
                gate.actionable_queue.candidate_state
            ));
        }
        // Timeout is not absence: no failure state may ever be reported as a
        // confirmed candidate absence.
        if gate.actionable_queue.candidate_state == "candidate_absent_from_actionable" {
            return Err(format!(
                "{context}: a failed queue read must never be collapsed into candidate absence"
            ));
        }
        let expected_verdict = string_at(case, "/expected/verdict", &context)?;
        if gate.verdict != expected_verdict {
            return Err(format!(
                "{context}: verdict expected {expected_verdict}, got {}",
                gate.verdict
            ));
        }
        if gate.safe_to_claim || gate.claim_command_action.is_some() {
            return Err(format!(
                "{context}: evaluated queue failure states must fail closed"
            ));
        }
        let unsafe_reason = string_at(case, "/expected/unsafeReason", &context)?;
        if !gate
            .unsafe_reasons
            .iter()
            .any(|reason| reason == unsafe_reason)
        {
            return Err(format!(
                "{context}: missing unsafe reason {unsafe_reason}; got {:?}",
                gate.unsafe_reasons
            ));
        }
        let degraded_code = string_at(case, "/expected/degradedCode", &context)?;
        if !gate.degraded_codes.iter().any(|code| code == degraded_code) {
            return Err(format!(
                "{context}: degradedCodes must carry {degraded_code}; got {:?}",
                gate.degraded_codes
            ));
        }

        let rendered = serde_json::to_value(&gate)
            .map_err(|error| format!("{context}: serialize gate: {error}"))?;
        assert_no_forbidden_markers(&rendered, &context)?;
    }

    Ok(())
}
