//! bd-ppbue.1: contract coverage for redaction-safe swarm replay inputs and
//! result ledgers.
//!
//! This pins `ee.swarm_workload.v1` and `ee.swarm_replay_result.v1` before a
//! runner exists. The schemas are orchestration metadata plus compact result
//! ledgers; they do not carry raw task/query text, memory bodies, mail bodies,
//! command output, secrets, full file listings, environment dumps, absolute
//! host paths, or wall-clock timestamps.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use ee::core::lab::{
    SWARM_REPLAY_RESULT_SCHEMA_V1, SWARM_WORKLOAD_SCHEMA_V1, SwarmExpectedDegradedPosture,
    SwarmRedactionProbeClass, SwarmRedactionProbeStatus, SwarmReplayAggregate,
    SwarmReplayArtifactRef, SwarmReplayCommandRedactionStatus, SwarmReplayCommandResult,
    SwarmReplayFailure, SwarmReplayRchStatus, SwarmReplayRedactionStatus, SwarmReplayResourceUsage,
    SwarmReplayResult, SwarmReplayStatus, SwarmReplayVerification, SwarmWorkloadCommandShape,
    SwarmWorkloadCommandStep, SwarmWorkloadPathPolicy, SwarmWorkloadProvenance,
    SwarmWorkloadProvenanceKind, SwarmWorkloadRedactionLevel, SwarmWorkloadRedactionProbe,
    SwarmWorkloadResourceProfileHints, SwarmWorkloadTrace, SwarmWorkloadWorkspaceShape,
};
use serde::Serialize;
use serde_json::Value;

type TestResult = Result<(), String>;

const WORKLOAD_SCHEMA_PATH: &str = "docs/schemas/ee.swarm_workload.v1.json";
const RESULT_SCHEMA_PATH: &str = "docs/schemas/ee.swarm_replay_result.v1.json";
const DOC_PATH: &str = "docs/agent-ux/swarm-replay-contracts.md";
const WORKLOAD_SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.swarm_workload.v1.json";
const RESULT_SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.swarm_replay_result.v1.json";

const HASH_64_A: &str = "blake3:1111111111111111111111111111111111111111111111111111111111111111";
const HASH_64_B: &str = "blake3:2222222222222222222222222222222222222222222222222222222222222222";
const HASH_64_C: &str = "blake3:3333333333333333333333333333333333333333333333333333333333333333";
const HASH_64_D: &str = "blake3:4444444444444444444444444444444444444444444444444444444444444444";
const HASH_16_A: &str = "blake3:aaaaaaaaaaaaaaaa";
const HASH_16_B: &str = "blake3:bbbbbbbbbbbbbbbb";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read_json(path: &str) -> Result<Value, String> {
    let full_path = repo_root().join(path);
    let text = fs::read_to_string(&full_path)
        .map_err(|error| format!("read {}: {error}", full_path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", full_path.display()))
}

fn read_text(path: &str) -> Result<String, String> {
    let full_path = repo_root().join(path);
    fs::read_to_string(&full_path).map_err(|error| format!("read {}: {error}", full_path.display()))
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn collect_strings(value: &Value, ctx: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got {value}"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry {entry}"))
        })
        .collect()
}

fn assert_required_fields(schema: &Value, value: &Value, ctx: &str) -> TestResult {
    for field in collect_strings(&schema["required"], &format!("{ctx}.required"))? {
        ensure(
            value.get(&field).is_some(),
            format!("{ctx}: serialized value missing schema-required field `{field}`: {value}"),
        )?;
    }
    Ok(())
}

fn assert_no_raw_payload(rendered: &str) -> TestResult {
    for forbidden in [
        "/Users/",
        "/data/projects/",
        "C:\\",
        "raw task content",
        "raw query text",
        "memory body payload",
        "mail body payload",
        "SECRET_TOKEN",
        "HOME=/",
        "Cargo.lock\n",
    ] {
        ensure(
            !rendered.contains(forbidden),
            format!("swarm contract sample leaked forbidden payload marker `{forbidden}`"),
        )?;
    }
    Ok(())
}

fn minimal_workload() -> SwarmWorkloadTrace {
    SwarmWorkloadTrace {
        schema: SWARM_WORKLOAD_SCHEMA_V1.to_owned(),
        workload_id: "swarmwl_1111111111111111".to_owned(),
        fixture_seed: "healthy_small_seed_001".to_owned(),
        side_effect_free: true,
        redaction_level: SwarmWorkloadRedactionLevel::Strict,
        workspace_shape: SwarmWorkloadWorkspaceShape {
            fixture_profile: "healthy_small_checkout".to_owned(),
            workspace_fingerprint: HASH_64_A.to_owned(),
            path_policy: SwarmWorkloadPathPolicy::NoAbsolutePaths,
            path_tail_hash: None,
            repo_state: "clean_fixture".to_owned(),
        },
        agent_count: 1,
        command_sequence: vec![SwarmWorkloadCommandStep {
            step_id: "step_001".to_owned(),
            agent_slot: 0,
            command: SwarmWorkloadCommandShape {
                verbs: vec!["pack".to_owned()],
                positional_arity: 1,
                flag_names: vec!["--json".to_owned()],
                output_format: Some("json".to_owned()),
                command_hash: HASH_64_B.to_owned(),
            },
            expected_schema: Some("ee.response.v2".to_owned()),
            expected_exit_code: Some(0),
            timeout_ms: None,
            depends_on: Vec::new(),
        }],
        expected_degraded_posture: SwarmExpectedDegradedPosture::NoneExpected,
        redaction_probes: Vec::new(),
        resource_profile_hints: SwarmWorkloadResourceProfileHints {
            profile: "ci_smoke".to_owned(),
            requested_parallel_agents: 1,
            max_parallel_agents: 1,
            memory_budget_mb: None,
            cpu_budget_ms: None,
            rch_required: true,
        },
        provenance: SwarmWorkloadProvenance {
            kind: SwarmWorkloadProvenanceKind::Synthetic,
            source_trace_hashes: Vec::new(),
            derived_from_schemas: vec!["ee.agent_workload_trace.v1".to_owned()],
            fixture_author_hash: None,
        },
    }
}

fn full_workload() -> SwarmWorkloadTrace {
    let mut workload = minimal_workload();
    workload.workload_id = "swarmwl_2222222222222222".to_owned();
    workload.fixture_seed = "rch_blocked_seed_001".to_owned();
    workload.agent_count = 64;
    workload.redaction_level = SwarmWorkloadRedactionLevel::Audit;
    workload.workspace_shape.path_policy = SwarmWorkloadPathPolicy::HashedPathTails;
    workload.workspace_shape.path_tail_hash = Some(HASH_16_A.to_owned());
    workload.workspace_shape.repo_state = "crowded_checkout".to_owned();
    workload.command_sequence.push(SwarmWorkloadCommandStep {
        step_id: "step_002".to_owned(),
        agent_slot: 1,
        command: SwarmWorkloadCommandShape {
            verbs: vec!["search".to_owned()],
            positional_arity: 1,
            flag_names: vec!["--workspace".to_owned(), "--json".to_owned()],
            output_format: Some("json".to_owned()),
            command_hash: HASH_64_C.to_owned(),
        },
        expected_schema: Some("ee.response.v2".to_owned()),
        expected_exit_code: Some(0),
        timeout_ms: Some(3000),
        depends_on: vec!["step_001".to_owned()],
    });
    workload.expected_degraded_posture = SwarmExpectedDegradedPosture::Blocked;
    workload.redaction_probes = vec![
        SwarmWorkloadRedactionProbe {
            probe_id: "probe_001".to_owned(),
            class: SwarmRedactionProbeClass::AbsoluteHostPath,
            value_hash: HASH_64_D.to_owned(),
            expected_status: SwarmRedactionProbeStatus::Blocked,
        },
        SwarmWorkloadRedactionProbe {
            probe_id: "probe_002".to_owned(),
            class: SwarmRedactionProbeClass::RawMemoryBody,
            value_hash: HASH_64_A.to_owned(),
            expected_status: SwarmRedactionProbeStatus::Redacted,
        },
    ];
    workload.resource_profile_hints = SwarmWorkloadResourceProfileHints {
        profile: "swarm_heavy_64_agent".to_owned(),
        requested_parallel_agents: 64,
        max_parallel_agents: 64,
        memory_budget_mb: Some(65_536),
        cpu_budget_ms: Some(120_000),
        rch_required: true,
    };
    workload.provenance = SwarmWorkloadProvenance {
        kind: SwarmWorkloadProvenanceKind::Mixed,
        source_trace_hashes: vec![HASH_64_B.to_owned()],
        derived_from_schemas: vec![
            "ee.agent_workload_trace.v1".to_owned(),
            "ee.swarm_slo.scorecard.v1".to_owned(),
        ],
        fixture_author_hash: Some(HASH_16_B.to_owned()),
    };
    workload
}

fn command_result(
    step_id: &str,
    exit_code: u8,
    elapsed_ms: u64,
    degraded_codes: Vec<&str>,
    redaction_status: SwarmReplayCommandRedactionStatus,
) -> SwarmReplayCommandResult {
    SwarmReplayCommandResult {
        step_id: step_id.to_owned(),
        agent_slot: 0,
        command_hash: HASH_64_B.to_owned(),
        exit_code,
        elapsed_ms,
        stdout_bytes: 512,
        stderr_bytes: if exit_code == 0 { 0 } else { 128 },
        degraded_codes: degraded_codes
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        artifact_paths: vec![SwarmReplayArtifactRef {
            kind: "json".to_owned(),
            path_tail: "artifacts/replay-result.json".to_owned(),
            path_hash: HASH_16_A.to_owned(),
        }],
        redaction_status,
        memory_rss_bytes: Some(73_400_320),
        cpu_ms: Some(73),
    }
}

fn clean_redaction_status(redaction_probes_passed: bool) -> SwarmReplayRedactionStatus {
    SwarmReplayRedactionStatus {
        raw_task_string_present: false,
        raw_query_text_present: false,
        raw_memory_body_present: false,
        raw_mail_body_present: false,
        absolute_host_path_present: false,
        secrets_present: false,
        environment_dump_present: false,
        full_file_listing_present: false,
        redaction_probes_passed,
    }
}

fn result_base(status: SwarmReplayStatus) -> SwarmReplayResult {
    SwarmReplayResult {
        schema: SWARM_REPLAY_RESULT_SCHEMA_V1.to_owned(),
        workload_id: "swarmwl_1111111111111111".to_owned(),
        run_id: "swarmrun_1111111111111111".to_owned(),
        side_effect_free: true,
        status,
        command_results: vec![command_result(
            "step_001",
            0,
            184,
            Vec::new(),
            SwarmReplayCommandRedactionStatus::Clean,
        )],
        aggregate: SwarmReplayAggregate {
            command_count: 1,
            success_count: 1,
            failure_count: 0,
            degraded_count: 0,
            elapsed_ms_total: 184,
            p50_ms: 184,
            p95_ms: 184,
            p99_ms: 184,
        },
        redaction_status: clean_redaction_status(true),
        resource_usage: SwarmReplayResourceUsage {
            peak_rss_bytes: Some(73_400_320),
            max_command_rss_bytes: Some(73_400_320),
            total_cpu_ms: Some(73),
            io_read_bytes: Some(8192),
            io_write_bytes: Some(4096),
        },
        first_failure: None,
        verification: SwarmReplayVerification {
            rch_required: true,
            rch_status: SwarmReplayRchStatus::Passed,
            deterministic: true,
            workload_hash: HASH_64_C.to_owned(),
            replay_hash: HASH_64_D.to_owned(),
            volatile_fields_stripped: vec!["elapsedMs".to_owned()],
        },
        warnings: Vec::new(),
    }
}

fn failure_result() -> SwarmReplayResult {
    let mut result = result_base(SwarmReplayStatus::Fail);
    result.command_results = vec![command_result(
        "step_001",
        7,
        44,
        vec!["rch_topology_blocked"],
        SwarmReplayCommandRedactionStatus::Redacted,
    )];
    result.aggregate = SwarmReplayAggregate {
        command_count: 1,
        success_count: 0,
        failure_count: 1,
        degraded_count: 1,
        elapsed_ms_total: 44,
        p50_ms: 44,
        p95_ms: 44,
        p99_ms: 44,
    };
    result.first_failure = Some(SwarmReplayFailure {
        step_id: "step_001".to_owned(),
        agent_slot: 0,
        code: "rch_topology_blocked".to_owned(),
        severity: "high".to_owned(),
        diagnosis: "RCH blocked before Cargo; no local fallback proof recorded.".to_owned(),
        repair_hint: Some("Fix remote dependency topology before rerunning replay.".to_owned()),
    });
    result.verification.rch_status = SwarmReplayRchStatus::BlockedBeforeCargo;
    result.warnings = vec!["RCH did not reach Cargo.".to_owned()];
    result
}

fn redaction_probe_result() -> SwarmReplayResult {
    let mut result = result_base(SwarmReplayStatus::Blocked);
    result.command_results = vec![command_result(
        "step_001",
        2,
        12,
        vec!["redaction_probe_failed"],
        SwarmReplayCommandRedactionStatus::ProbeFailed,
    )];
    result.redaction_status = clean_redaction_status(false);
    result.first_failure = Some(SwarmReplayFailure {
        step_id: "step_001".to_owned(),
        agent_slot: 0,
        code: "redaction_probe_failed".to_owned(),
        severity: "critical".to_owned(),
        diagnosis: "A redaction probe failed; replay output was withheld.".to_owned(),
        repair_hint: Some("Regenerate the fixture with strict redaction.".to_owned()),
    });
    result.verification.rch_status = SwarmReplayRchStatus::NotRequired;
    result.verification.deterministic = false;
    result
}

#[test]
fn swarm_workload_schema_pins_redacted_orchestration_shape() -> TestResult {
    let schema = read_json(WORKLOAD_SCHEMA_PATH)?;
    ensure(
        schema["$id"] == WORKLOAD_SCHEMA_ID,
        format!(
            "expected workload schema id {WORKLOAD_SCHEMA_ID}; got {}",
            schema["$id"]
        ),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == SWARM_WORKLOAD_SCHEMA_V1,
        "workload schema const must match Rust constant",
    )?;
    ensure(
        schema["properties"]["sideEffectFree"]["const"] == Value::Bool(true),
        "workload schema must be side-effect-free",
    )?;

    for field in [
        "workloadId",
        "fixtureSeed",
        "workspaceShape",
        "agentCount",
        "commandSequence",
        "expectedDegradedPosture",
        "redactionProbes",
        "resourceProfileHints",
        "provenance",
    ] {
        ensure(
            collect_strings(&schema["required"], "workload.required")?
                .iter()
                .any(|entry| entry == field),
            format!("workload schema missing required field `{field}`"),
        )?;
    }

    let command_props = schema["$defs"]["commandShape"]["properties"]
        .as_object()
        .ok_or_else(|| "commandShape.properties must be an object".to_owned())?;
    for forbidden in [
        "argv",
        "args",
        "rawArgs",
        "query",
        "rawQuery",
        "task",
        "rawTask",
        "prompt",
        "memoryBody",
        "mailBody",
    ] {
        ensure(
            !command_props.contains_key(forbidden),
            format!("commandShape must not make `{forbidden}` representable"),
        )?;
    }
    Ok(())
}

#[test]
fn swarm_replay_result_schema_structurally_forbids_raw_content() -> TestResult {
    let schema = read_json(RESULT_SCHEMA_PATH)?;
    ensure(
        schema["$id"] == RESULT_SCHEMA_ID,
        format!(
            "expected result schema id {RESULT_SCHEMA_ID}; got {}",
            schema["$id"]
        ),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == SWARM_REPLAY_RESULT_SCHEMA_V1,
        "result schema const must match Rust constant",
    )?;
    ensure(
        schema["properties"]["sideEffectFree"]["const"] == Value::Bool(true),
        "result schema must be side-effect-free",
    )?;

    let redaction = &schema["$defs"]["redactionStatus"];
    let required = collect_strings(&redaction["required"], "redactionStatus.required")?;
    for field in [
        "rawTaskStringPresent",
        "rawQueryTextPresent",
        "rawMemoryBodyPresent",
        "rawMailBodyPresent",
        "absoluteHostPathPresent",
        "secretsPresent",
        "environmentDumpPresent",
        "fullFileListingPresent",
    ] {
        ensure(
            required.iter().any(|entry| entry == field),
            format!("redactionStatus.required missing `{field}`"),
        )?;
        ensure(
            redaction["properties"][field]["const"] == Value::Bool(false),
            format!("redactionStatus.{field} must be const false"),
        )?;
    }

    ensure(
        schema["$defs"]["artifactRef"]["properties"]["pathTail"]["not"].is_object(),
        "artifact pathTail must structurally reject absolute host paths",
    )
}

#[test]
fn minimal_and_full_workload_traces_serialize_to_schema_shape() -> TestResult {
    let schema = read_json(WORKLOAD_SCHEMA_PATH)?;
    for workload in [minimal_workload(), full_workload()] {
        let value = to_value(&workload)?;
        assert_required_fields(&schema, &value, "workload")?;
        ensure(
            value["schema"] == SWARM_WORKLOAD_SCHEMA_V1,
            "serialized workload schema mismatch",
        )?;
        ensure(
            value["sideEffectFree"] == Value::Bool(true),
            "serialized workload must be sideEffectFree",
        )?;
        assert_no_raw_payload(&workload.to_json())?;
        ensure(
            value.get("recordedAt").is_none()
                && value.get("createdAt").is_none()
                && value.get("runAt").is_none(),
            format!("workload must not carry wall-clock timestamp fields: {value}"),
        )?;
    }
    Ok(())
}

#[test]
fn failure_and_redaction_probe_results_serialize_to_schema_shape() -> TestResult {
    let schema = read_json(RESULT_SCHEMA_PATH)?;
    for result in [failure_result(), redaction_probe_result()] {
        let value = to_value(&result)?;
        assert_required_fields(&schema, &value, "replay result")?;
        ensure(
            value["schema"] == SWARM_REPLAY_RESULT_SCHEMA_V1,
            "serialized replay result schema mismatch",
        )?;
        ensure(
            value["sideEffectFree"] == Value::Bool(true),
            "serialized replay result must be sideEffectFree",
        )?;
        ensure(
            value["redactionStatus"]["rawTaskStringPresent"] == Value::Bool(false)
                && value["redactionStatus"]["rawQueryTextPresent"] == Value::Bool(false)
                && value["redactionStatus"]["rawMemoryBodyPresent"] == Value::Bool(false)
                && value["redactionStatus"]["rawMailBodyPresent"] == Value::Bool(false)
                && value["redactionStatus"]["absoluteHostPathPresent"] == Value::Bool(false)
                && value["redactionStatus"]["secretsPresent"] == Value::Bool(false),
            format!("result redaction posture must keep all raw-content flags false: {value}"),
        )?;
        assert_no_raw_payload(&result.to_json())?;
        ensure(
            value.get("recordedAt").is_none()
                && value.get("createdAt").is_none()
                && value.get("runAt").is_none(),
            format!("result must not carry wall-clock timestamp fields: {value}"),
        )?;
    }
    Ok(())
}

#[test]
fn swarm_replay_contract_docs_pin_security_and_rch_posture() -> TestResult {
    let doc = read_text(DOC_PATH)?;
    for expected in [
        "ee.swarm_workload.v1",
        "ee.swarm_replay_result.v1",
        "absolute host paths",
        "commandHash",
        "RCH was required",
        "local Cargo fallback",
        "do not include timestamps",
    ] {
        ensure(
            doc.contains(expected),
            format!("{DOC_PATH} missing expected contract text: {expected}"),
        )?;
    }
    Ok(())
}
