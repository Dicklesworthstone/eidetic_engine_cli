//! bd-ppbue.14: selector/admission evidence conformance for replay capsules.
//!
//! The focused contract here is intentionally test-only: parse compact
//! `ee.rch.verify.v1` proofs, attach them to a generated replay trace, and pin
//! the support-bundle-safe selector summary without editing active runner files.

use std::fs;
use std::path::{Path, PathBuf};

use ee::core::lab::{
    SwarmReplayHostPathPosture, SwarmReplayHostProfileObservation, SwarmReplayOptions,
    SwarmReplayRchStatus, SwarmReplayStatus, SwarmReplayVerificationProofLevel,
    SwarmWorkloadFixtureOptions, generate_swarm_workload_fixture, replay_swarm_workload_trace,
};
use ee::models::verification_evidence_record_from_rch_verify;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const RESULT_SCHEMA_PATH: &str = "docs/schemas/ee.swarm_replay_result.v1.json";

struct SelectorAdmissionCase {
    requirement_id: &'static str,
    fixture_id: &'static str,
    expected_status: SwarmReplayStatus,
    expected_rch_status: SwarmReplayRchStatus,
    expected_proof_level: SwarmReplayVerificationProofLevel,
    expected_selector_status: &'static str,
    expected_selected_worker: Option<&'static str>,
    expected_failure_reason: Option<&'static str>,
    expected_cargo_started: Option<bool>,
    expected_remote_marker: bool,
    expected_local_fallback_refused: bool,
    proof: Value,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_eq<T>(actual: T, expected: T, label: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label}: expected {expected:?}, got {actual:?}"))
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(path: &str) -> Result<Value, String> {
    let full_path = repo_root().join(path);
    let text = fs::read_to_string(&full_path)
        .map_err(|error| format!("read {}: {error}", full_path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", full_path.display()))
}

fn admitted_host_observation() -> SwarmReplayHostProfileObservation {
    SwarmReplayHostProfileObservation {
        logical_cpu_count: Some(16),
        available_memory_mb: Some(32_768),
        target_dir_posture: SwarmReplayHostPathPosture::External,
        tmpdir_posture: SwarmReplayHostPathPosture::External,
        rch_available: Some(true),
        numa_available: Some(false),
        lexical_ram_tier_available: Some(true),
        path_tail_hashes: vec!["blake3:aaaaaaaaaaaaaaaa".to_owned()],
    }
}

fn write_generated_trace(workspace: &Path, seed: &str) -> Result<PathBuf, String> {
    let trace = generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small(seed));
    let trace_path = workspace.join(format!("{seed}-swarm-workload.json"));
    fs::write(&trace_path, trace.to_json())
        .map_err(|error| format!("write {}: {error}", trace_path.display()))?;
    Ok(trace_path)
}

fn write_proof(workspace: &Path, fixture_id: &str, proof: &Value) -> Result<PathBuf, String> {
    let proof_path = workspace.join(format!("{fixture_id}-rch-proof.json"));
    fs::write(&proof_path, proof.to_string())
        .map_err(|error| format!("write {}: {error}", proof_path.display()))?;
    Ok(proof_path)
}

fn replay_options(
    workspace: &Path,
    trace_path: PathBuf,
    proof_path: PathBuf,
) -> SwarmReplayOptions {
    SwarmReplayOptions {
        workspace: workspace.to_path_buf(),
        trace_path,
        dry_run: true,
        host_observation: admitted_host_observation(),
        ee_binary_path: None,
        rch_proof_path: Some(proof_path),
    }
}

fn remote_pass_proof() -> Value {
    json!({
        "schema": "ee.rch.verify.v1",
        "success": true,
        "status": "remote_pass",
        "generated_at": "2026-06-03T11:20:00Z",
        "started_at": "2026-06-03T11:18:00Z",
        "completed_at": "2026-06-03T11:20:00Z",
        "command": ["cargo", "test", "--test", "contracts", "selector_admission"],
        "command_text": "cargo test --test contracts selector_admission",
        "command_kind": "cargo_test",
        "command_hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "remote_required": true,
        "would_offload": true,
        "worker_id": "vmi1227854",
        "exit_code": 0,
        "elapsed_ms": 120000,
        "degraded_codes": [],
        "selector_admission_probe": {
            "schema": "ee.rch.selector_admission_probe.v1",
            "status": "selected",
            "required_runtime": "Rust",
            "workers_reported": ["vmi1227854", "vmi1264463"],
            "daemon_workers_reported": ["vmi1227854"],
            "workers_reported_count": 2,
            "daemon_workers_reported_count": 1,
            "selected_worker": "vmi1227854",
            "selection_failure_reason": null,
            "workers_vs_selection_contradiction": false,
            "path_normalization_warning": null,
            "remote_required": true,
            "local_fallback_refused": false,
            "admission_blocker": null
        },
        "local_cargo_processes": {
            "schema": "ee.rch_local_cargo_tripwire.v1",
            "status": "checked",
            "count": 0,
            "processes": []
        }
    })
}

fn precargo_selector_failure_proof() -> Value {
    json!({
        "schema": "ee.rch.verify.v1",
        "success": true,
        "status": "rch_environment_failure",
        "generated_at": "2026-06-03T11:20:00Z",
        "command": ["cargo", "check", "--lib"],
        "command_text": "cargo check --lib",
        "command_kind": "cargo_check",
        "command_hash": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "remote_required": true,
        "would_offload": true,
        "worker_id": null,
        "exit_code": 1,
        "elapsed_ms": 1839,
        "degraded_codes": [
            "rch_verify_remote_command_failed",
            "rch_verify_local_fallback_refused",
            "rch_verify_remote_marker_missing"
        ],
        "selector_admission_probe": {
            "schema": "ee.rch.selector_admission_probe.v1",
            "status": "selection_failed",
            "required_runtime": "Rust",
            "workers_reported": ["vmi1227854"],
            "daemon_workers_reported": ["vmi1227854"],
            "workers_reported_count": 1,
            "daemon_workers_reported_count": 1,
            "selected_worker": null,
            "selection_failure_reason": "no_workers_with_rust_installed",
            "workers_vs_selection_contradiction": true,
            "path_normalization_warning": "RCH_TOPOLOGY_ERR_ALIAS_NOT_SYMLINK:path=/Users/alice/projects",
            "remote_required": true,
            "local_fallback_refused": true,
            "admission_blocker": null
        },
        "known_blocker": {
            "schema": "ee.rch.known_blocker.v1",
            "blocker_fingerprint": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "blocker_kind": "selector_admission_failed",
            "remediation_bead": "bd-17c65.10.17",
            "retry_after": "2026-06-03T16:05:25.180106Z"
        },
        "local_cargo_processes": {
            "schema": "ee.rch_local_cargo_tripwire.v1",
            "status": "checked",
            "count": 0,
            "processes": []
        }
    })
}

fn local_fallback_contamination_proof() -> Value {
    json!({
        "schema": "ee.rch.verify.v1",
        "success": true,
        "status": "local_fallback",
        "generated_at": "2026-06-03T11:20:00Z",
        "command": ["cargo", "test", "--lib"],
        "command_text": "cargo test --lib",
        "command_kind": "cargo_test",
        "command_hash": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "remote_required": true,
        "would_offload": false,
        "worker_id": null,
        "exit_code": 0,
        "elapsed_ms": 4213,
        "degraded_codes": ["rch_verify_local_fallback_detected"],
        "fallback_reason": "remote marker missing",
        "selector_admission_probe": {
            "schema": "ee.rch.selector_admission_probe.v1",
            "status": "selection_failed",
            "required_runtime": "Rust",
            "workers_reported": [],
            "daemon_workers_reported": [],
            "workers_reported_count": 0,
            "daemon_workers_reported_count": 0,
            "selected_worker": null,
            "selection_failure_reason": "no_workers_with_rust_installed",
            "workers_vs_selection_contradiction": false,
            "path_normalization_warning": null,
            "remote_required": true,
            "local_fallback_refused": false,
            "admission_blocker": null
        },
        "local_cargo_processes": {
            "schema": "ee.rch_local_cargo_tripwire.v1",
            "status": "checked",
            "count": 2,
            "processes": []
        }
    })
}

fn selector_admission_cases() -> Vec<SelectorAdmissionCase> {
    vec![
        SelectorAdmissionCase {
            requirement_id: "selector-admission-selected-worker",
            fixture_id: "remote-pass-selected-worker",
            expected_status: SwarmReplayStatus::Degraded,
            expected_rch_status: SwarmReplayRchStatus::Passed,
            expected_proof_level: SwarmReplayVerificationProofLevel::RemoteVerified,
            expected_selector_status: "selected",
            expected_selected_worker: Some("vmi1227854"),
            expected_failure_reason: None,
            expected_cargo_started: Some(true),
            expected_remote_marker: true,
            expected_local_fallback_refused: false,
            proof: remote_pass_proof(),
        },
        SelectorAdmissionCase {
            requirement_id: "selector-admission-precargo-blocker",
            fixture_id: "precargo-selector-failure",
            expected_status: SwarmReplayStatus::Blocked,
            expected_rch_status: SwarmReplayRchStatus::BlockedBeforeCargo,
            expected_proof_level: SwarmReplayVerificationProofLevel::RchBlocked,
            expected_selector_status: "selection_failed",
            expected_selected_worker: None,
            expected_failure_reason: Some("no_workers_with_rust_installed"),
            expected_cargo_started: Some(false),
            expected_remote_marker: false,
            expected_local_fallback_refused: true,
            proof: precargo_selector_failure_proof(),
        },
        SelectorAdmissionCase {
            requirement_id: "selector-admission-local-fallback-contamination",
            fixture_id: "local-fallback-contamination",
            expected_status: SwarmReplayStatus::Fail,
            expected_rch_status: SwarmReplayRchStatus::Failed,
            expected_proof_level: SwarmReplayVerificationProofLevel::LocalCargoContaminated,
            expected_selector_status: "selection_failed",
            expected_selected_worker: None,
            expected_failure_reason: Some("no_workers_with_rust_installed"),
            expected_cargo_started: Some(false),
            expected_remote_marker: false,
            expected_local_fallback_refused: false,
            proof: local_fallback_contamination_proof(),
        },
    ]
}

#[test]
fn swarm_replay_selector_admission_matrix_classifies_proof_posture() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;

    for case in selector_admission_cases() {
        ensure(!case.requirement_id.is_empty(), "requirement id missing")?;
        ensure(!case.fixture_id.is_empty(), "fixture id missing")?;

        let record = verification_evidence_record_from_rch_verify(&case.proof)
            .map_err(|error| format!("parse {}: {error}", case.fixture_id))?;
        let parsed_selector = record
            .selector_admission
            .as_ref()
            .ok_or_else(|| format!("{}: selector admission missing", case.fixture_id))?;
        ensure(
            !parsed_selector.workers_reported.is_empty()
                || case.fixture_id == "local-fallback-contamination",
            format!("{}: worker-list summary missing upstream", case.fixture_id),
        )?;

        let trace_path = write_generated_trace(workspace.path(), case.fixture_id)?;
        let proof_path = write_proof(workspace.path(), case.fixture_id, &case.proof)?;
        let report =
            replay_swarm_workload_trace(&replay_options(workspace.path(), trace_path, proof_path))
                .map_err(|error| error.message())?;
        let rch = report
            .verification
            .proof_capsule
            .rch
            .as_ref()
            .ok_or_else(|| format!("{}: RCH proof summary missing", case.fixture_id))?;
        let selector = rch
            .selector_admission
            .as_ref()
            .ok_or_else(|| format!("{}: selector summary missing", case.fixture_id))?;

        ensure_eq(report.status, case.expected_status, case.requirement_id)?;
        ensure_eq(
            report.verification.rch_status,
            case.expected_rch_status,
            case.requirement_id,
        )?;
        ensure_eq(
            report.verification.proof_capsule.proof_level,
            case.expected_proof_level,
            case.requirement_id,
        )?;
        ensure_eq(
            selector.status.as_deref(),
            Some(case.expected_selector_status),
            case.requirement_id,
        )?;
        ensure_eq(
            selector.selected_worker.as_deref(),
            case.expected_selected_worker,
            case.requirement_id,
        )?;
        ensure_eq(
            selector.selection_failure_reason.as_deref(),
            case.expected_failure_reason,
            case.requirement_id,
        )?;
        ensure_eq(
            rch.cargo_started,
            case.expected_cargo_started,
            case.requirement_id,
        )?;
        ensure_eq(
            rch.remote_marker_present,
            case.expected_remote_marker,
            case.requirement_id,
        )?;
        ensure_eq(
            rch.local_fallback_refused,
            case.expected_local_fallback_refused,
            case.requirement_id,
        )?;
        ensure(
            !rch.raw_output_included,
            "raw output must stay out of capsule",
        )?;
        ensure(
            rch.local_paths_redacted,
            "local paths must be marked redacted",
        )?;
    }

    Ok(())
}

#[test]
fn swarm_replay_selector_admission_redacts_private_path_warning() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let trace_path = write_generated_trace(workspace.path(), "selector-redaction")?;
    let proof = precargo_selector_failure_proof();
    let proof_path = write_proof(workspace.path(), "selector-redaction", &proof)?;
    let report =
        replay_swarm_workload_trace(&replay_options(workspace.path(), trace_path, proof_path))
            .map_err(|error| error.message())?;
    let rendered = report.to_json();
    let selector = report
        .verification
        .proof_capsule
        .rch
        .as_ref()
        .and_then(|rch| rch.selector_admission.as_ref())
        .ok_or_else(|| "selector summary missing".to_owned())?;
    let warning = selector
        .path_normalization_warning
        .as_deref()
        .ok_or_else(|| "path normalization warning missing".to_owned())?;

    ensure(
        warning.contains("/Users/<redacted>"),
        format!("private path warning was not redacted: {warning}"),
    )?;
    for forbidden in [
        "/Users/alice",
        "/Users/jemanuel",
        "/data/projects/eidetic_engine_cli",
        "stdout_tail",
        "stderr_tail",
        "remote_project_root",
    ] {
        ensure(
            !rendered.contains(forbidden),
            format!("selector capsule leaked forbidden marker `{forbidden}`: {rendered}"),
        )?;
    }
    Ok(())
}

#[test]
fn swarm_replay_selector_admission_schema_pins_safe_summary_shape() -> TestResult {
    let schema = read_json(RESULT_SCHEMA_PATH)?;
    let selector = &schema["$defs"]["rchSelectorAdmission"];
    let required = selector["required"]
        .as_array()
        .ok_or_else(|| "selector required must be an array".to_owned())?;
    for field in [
        "status",
        "requiredRuntime",
        "selectedWorker",
        "selectionFailureReason",
        "workersVsSelectionContradiction",
        "pathNormalizationWarning",
        "remoteRequired",
        "localFallbackRefused",
    ] {
        ensure(
            required.iter().any(|entry| entry.as_str() == Some(field)),
            format!("selector schema must require `{field}`"),
        )?;
    }
    let rch_proof = &schema["$defs"]["rchProofSummary"];
    ensure(
        rch_proof["required"].as_array().is_some_and(|required| {
            required
                .iter()
                .any(|entry| entry.as_str() == Some("selectorAdmission"))
        }),
        "RCH proof schema must require selectorAdmission",
    )?;
    for forbidden_field in [
        "stdoutTail",
        "stderrTail",
        "remoteProjectRoot",
        "remoteTargetDir",
        "rawOutput",
    ] {
        ensure(
            rch_proof["properties"].get(forbidden_field).is_none(),
            format!("RCH proof summary must not expose `{forbidden_field}`"),
        )?;
    }
    Ok(())
}
