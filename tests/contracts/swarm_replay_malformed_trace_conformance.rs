//! bd-ppbue.12: malformed `ee.swarm_workload.v1` conformance matrix.
//!
//! This module keeps the negative-input corpus in the contract suite without
//! editing the actively-owned replay runner implementation.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ee::core::lab::{
    MAX_SWARM_WORKLOAD_COMMANDS, SWARM_REPLAY_COMMAND_NOT_ALLOWLISTED_CODE,
    SWARM_REPLAY_LOCAL_CARGO_REFUSED_CODE, SWARM_REPLAY_RESULT_SCHEMA_V1,
    SwarmReplayHostPathPosture, SwarmReplayHostProfileObservation, SwarmReplayOptions,
    SwarmReplayRchStatus, SwarmReplayStatus, SwarmWorkloadFixtureOptions,
    generate_swarm_workload_fixture, replay_swarm_workload_trace,
};
use ee::models::DomainError;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const MAX_LAB_TRACE_BYTES: usize = 16 * 1024 * 1024;
const CONFORMANCE_REPORT_SCHEMA_V1: &str = "ee.swarm_replay_malformed_conformance_report.v1";
const CONFORMANCE_REPORT_GOLDEN: &str =
    include_str!("../fixtures/golden/lab/swarm_replay_malformed_conformance_report.json.golden");
const BLOCKED_RESULT_ENVELOPE_SCHEMA_V1: &str = "ee.swarm_replay_blocked_result_envelope_golden.v1";
const BLOCKED_RESULT_ENVELOPE_GOLDEN: &str =
    include_str!("../fixtures/golden/lab/swarm_replay_blocked_result_envelope.json.golden");

struct MalformedTraceCase {
    requirement_id: &'static str,
    fixture_id: &'static str,
    fixture_path: &'static str,
    expected_outcome: &'static str,
    expected_code: &'static str,
    schema_validation_status: &'static str,
    test_surface: &'static str,
    trace_json: String,
    message_contains: &'static str,
    repair_contains: &'static str,
}

struct RefusalTraceCase {
    requirement_id: &'static str,
    fixture_id: &'static str,
    fixture_path: &'static str,
    expected_outcome: &'static str,
    expected_code: &'static str,
    schema_validation_status: &'static str,
    test_surface: &'static str,
    verbs: &'static [&'static str],
}

struct AdversarialLedgerCase {
    requirement_id: &'static str,
    fixture_id: &'static str,
    fixture_path: &'static str,
    expected_outcome: &'static str,
    expected_code: &'static str,
    schema_validation_status: &'static str,
    test_surface: &'static str,
    mutate: fn(&mut Value) -> Result<(), String>,
}

struct StructureFuzzCase {
    requirement_id: &'static str,
    fixture_id: String,
    expected_outcome: &'static str,
    expected_code: Option<&'static str>,
    schema_validation_status: &'static str,
    trace_json: String,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn workspace(label: &str) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix(&format!("ee-swarm-replay-{label}-"))
        .tempdir()
        .map_err(|error| error.to_string())
}

fn admitted_host_observation() -> SwarmReplayHostProfileObservation {
    SwarmReplayHostProfileObservation {
        logical_cpu_count: Some(8),
        available_memory_mb: Some(8192),
        target_dir_posture: SwarmReplayHostPathPosture::Local,
        tmpdir_posture: SwarmReplayHostPathPosture::Local,
        rch_available: Some(true),
        numa_available: None,
        lexical_ram_tier_available: None,
        path_tail_hashes: Vec::new(),
    }
}

fn replay_options(workspace: &Path, trace_path: PathBuf, dry_run: bool) -> SwarmReplayOptions {
    SwarmReplayOptions {
        workspace: workspace.to_path_buf(),
        trace_path,
        dry_run,
        host_observation: admitted_host_observation(),
        ee_binary_path: None,
        rch_proof_path: None,
    }
}

fn generated_trace_value(seed: &str) -> Result<Value, String> {
    let trace = generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small(seed));
    serde_json::from_str(&trace.to_json()).map_err(|error| error.to_string())
}

fn mutated_trace_json(
    seed: &str,
    mutate: impl FnOnce(&mut Value) -> Result<(), String>,
) -> Result<String, String> {
    let mut value = generated_trace_value(seed)?;
    mutate(&mut value)?;
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}

fn command_sequence_mut(value: &mut Value) -> Result<&mut Vec<Value>, String> {
    value["commandSequence"]
        .as_array_mut()
        .ok_or_else(|| "generated trace missing commandSequence array".to_owned())
}

fn safe_fixture_name(input: &str) -> String {
    input
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn write_trace_fixture(workspace: &Path, fixture_id: &str, text: &str) -> Result<PathBuf, String> {
    let path = workspace.join(format!("{}.json", safe_fixture_name(fixture_id)));
    fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

fn matrix_field_metadata_is_complete(
    requirement_id: &str,
    fixture_id: &str,
    fixture_path: &str,
    expected_outcome: &str,
    expected_code: &str,
    schema_validation_status: &str,
    test_surface: &str,
) -> TestResult {
    ensure(!requirement_id.is_empty(), "requirement id missing")?;
    ensure(!fixture_id.is_empty(), "fixture id missing")?;
    ensure(!fixture_path.is_empty(), "fixture path missing")?;
    ensure(!expected_outcome.is_empty(), "expected outcome missing")?;
    ensure(!expected_code.is_empty(), "expected code missing")?;
    ensure(
        !schema_validation_status.is_empty(),
        "schema validation status missing",
    )?;
    ensure(!test_surface.is_empty(), "test surface missing")
}

fn malformed_trace_cases() -> Result<Vec<MalformedTraceCase>, String> {
    Ok(vec![
        MalformedTraceCase {
            requirement_id: "swarm-trace-json-syntax",
            fixture_id: "malformed-json",
            fixture_path: "generated://swarm-replay/malformed-json",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "json_decode_failed",
            test_surface: "contract",
            trace_json: "{".to_owned(),
            message_contains: "invalid ee.swarm_workload.v1 trace",
            repair_contains: "ee lab generate-workload",
        },
        MalformedTraceCase {
            requirement_id: "swarm-trace-top-level-object",
            fixture_id: "non-object-top-level",
            fixture_path: "generated://swarm-replay/non-object-top-level",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: "[]".to_owned(),
            message_contains: "invalid ee.swarm_workload.v1 trace",
            repair_contains: "ee lab generate-workload",
        },
        MalformedTraceCase {
            requirement_id: "swarm-trace-schema-required",
            fixture_id: "missing-schema",
            fixture_path: "generated://swarm-replay/missing-schema",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_missing_schema_001", |value| {
                value
                    .as_object_mut()
                    .ok_or_else(|| "generated trace was not an object".to_owned())?
                    .remove("schema");
                Ok(())
            })?,
            message_contains: "invalid ee.swarm_workload.v1 trace",
            repair_contains: "ee lab generate-workload",
        },
        MalformedTraceCase {
            requirement_id: "swarm-trace-schema-version",
            fixture_id: "wrong-schema",
            fixture_path: "generated://swarm-replay/wrong-schema",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_wrong_schema_001", |value| {
                value["schema"] = Value::from("ee.swarm_workload.v9");
                Ok(())
            })?,
            message_contains: "expected swarm workload schema ee.swarm_workload.v1",
            repair_contains: "ee.swarm_workload.v1",
        },
        MalformedTraceCase {
            requirement_id: "swarm-generator-evidence-schema",
            fixture_id: "wrong-generator-evidence-schema",
            fixture_path: "generated://swarm-replay/wrong-generator-evidence-schema",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_generator_schema_001", |value| {
                value["generatorEvidence"]["schema"] =
                    Value::from("ee.swarm_workload.generator_evidence.v9");
                Ok(())
            })?,
            message_contains: "expected generator evidence schema",
            repair_contains: "Regenerate",
        },
        MalformedTraceCase {
            requirement_id: "swarm-generator-evidence-schema-id",
            fixture_id: "wrong-generator-schema-id",
            fixture_path: "generated://swarm-replay/wrong-generator-schema-id",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_schema_id_001", |value| {
                value["generatorEvidence"]["schemaId"] =
                    Value::from("https://eidetic-engine/schemas/other.json");
                Ok(())
            })?,
            message_contains: "expected swarm workload schema id",
            repair_contains: "Regenerate",
        },
        MalformedTraceCase {
            requirement_id: "swarm-side-effect-free",
            fixture_id: "side-effect-free-false",
            fixture_path: "generated://swarm-replay/side-effect-free-false",
            expected_outcome: "error",
            expected_code: "policy_denied",
            schema_validation_status: "schema_valid_policy_denied",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_side_effect_001", |value| {
                value["sideEffectFree"] = Value::Bool(false);
                Ok(())
            })?,
            message_contains: "sideEffectFree=true",
            repair_contains: "side-effect-free",
        },
        MalformedTraceCase {
            requirement_id: "swarm-agent-count-positive",
            fixture_id: "zero-agents",
            fixture_path: "generated://swarm-replay/zero-agents",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_zero_agents_001", |value| {
                value["agentCount"] = Value::from(0u64);
                Ok(())
            })?,
            message_contains: "declares zero agents",
            repair_contains: "at least one agent",
        },
        MalformedTraceCase {
            requirement_id: "swarm-command-sequence-nonempty",
            fixture_id: "empty-command-sequence",
            fixture_path: "generated://swarm-replay/empty-command-sequence",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_empty_commands_001", |value| {
                value["commandSequence"] = Value::Array(Vec::new());
                Ok(())
            })?,
            message_contains: "no commandSequence entries",
            repair_contains: "at least one command",
        },
        MalformedTraceCase {
            requirement_id: "swarm-command-sequence-count-cap",
            fixture_id: "excessive-command-sequence",
            fixture_path: "generated://swarm-replay/excessive-command-sequence",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid_command_count_cap",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_excessive_commands_001", |value| {
                let agent_count = value["agentCount"].as_u64().unwrap_or(1).max(1);
                let commands = command_sequence_mut(value)?;
                let template = commands
                    .first()
                    .cloned()
                    .ok_or_else(|| "generated trace missing command template".to_owned())?;
                commands.clear();
                for index in 0..=MAX_SWARM_WORKLOAD_COMMANDS {
                    let mut command = template.clone();
                    command["stepId"] = Value::from(format!("command-cap-{index:04}"));
                    command["agentSlot"] = Value::from((index as u64) % agent_count);
                    command["dependsOn"] = Value::Array(Vec::new());
                    command["command"]["commandHash"] =
                        Value::from(format!("command-cap-hash-{index:04}"));
                    commands.push(command);
                }
                Ok(())
            })?,
            message_contains: "commandSequence entries",
            repair_contains: "smaller traces",
        },
        MalformedTraceCase {
            requirement_id: "swarm-step-id-nonempty",
            fixture_id: "empty-step-id",
            fixture_path: "generated://swarm-replay/empty-step-id",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_empty_step_001", |value| {
                command_sequence_mut(value)?[0]["stepId"] = Value::from("");
                Ok(())
            })?,
            message_contains: "empty stepId",
            repair_contains: "non-empty step IDs",
        },
        MalformedTraceCase {
            requirement_id: "swarm-step-id-unique",
            fixture_id: "duplicate-step-id",
            fixture_path: "generated://swarm-replay/duplicate-step-id",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_duplicate_step_001", |value| {
                let commands = command_sequence_mut(value)?;
                let first_id = commands[0]["stepId"]
                    .as_str()
                    .ok_or_else(|| "generated step missing stepId".to_owned())?
                    .to_owned();
                commands[1]["stepId"] = Value::from(first_id);
                Ok(())
            })?,
            message_contains: "repeats stepId",
            repair_contains: "unique step IDs",
        },
        MalformedTraceCase {
            requirement_id: "swarm-agent-slot-range",
            fixture_id: "agent-slot-out-of-range",
            fixture_path: "generated://swarm-replay/agent-slot-out-of-range",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_agent_slot_001", |value| {
                command_sequence_mut(value)?[0]["agentSlot"] = Value::from(999u64);
                Ok(())
            })?,
            message_contains: "outside declared agentCount",
            repair_contains: "agent slot",
        },
        MalformedTraceCase {
            requirement_id: "swarm-command-verbs-nonempty",
            fixture_id: "empty-command-verbs",
            fixture_path: "generated://swarm-replay/empty-command-verbs",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_empty_verbs_001", |value| {
                command_sequence_mut(value)?[0]["command"]["verbs"] = Value::Array(Vec::new());
                Ok(())
            })?,
            message_contains: "has no command verbs",
            repair_contains: "command shapes",
        },
        MalformedTraceCase {
            requirement_id: "swarm-command-hash-nonempty",
            fixture_id: "empty-command-hash",
            fixture_path: "generated://swarm-replay/empty-command-hash",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_empty_hash_001", |value| {
                command_sequence_mut(value)?[0]["command"]["commandHash"] = Value::from("");
                Ok(())
            })?,
            message_contains: "empty commandHash",
            repair_contains: "command hashes",
        },
        MalformedTraceCase {
            requirement_id: "swarm-step-dependency-resolves",
            fixture_id: "unknown-step-dependency",
            fixture_path: "generated://swarm-replay/unknown-step-dependency",
            expected_outcome: "error",
            expected_code: "usage",
            schema_validation_status: "schema_invalid",
            test_surface: "contract",
            trace_json: mutated_trace_json("bad_unknown_dep_001", |value| {
                command_sequence_mut(value)?[0]["dependsOn"] = json!(["missing_step"]);
                Ok(())
            })?,
            message_contains: "depends on unknown stepId",
            repair_contains: "valid step dependencies",
        },
    ])
}

fn refusal_trace_cases() -> Vec<RefusalTraceCase> {
    vec![
        RefusalTraceCase {
            requirement_id: "swarm-local-cargo-refused",
            fixture_id: "local-cargo-command-shape",
            fixture_path: "generated://swarm-replay/local-cargo-command-shape",
            expected_outcome: "blocked-ledger",
            expected_code: SWARM_REPLAY_LOCAL_CARGO_REFUSED_CODE,
            schema_validation_status: "schema_valid_runner_refused",
            test_surface: "contract",
            verbs: &["cargo", "test"],
        },
        RefusalTraceCase {
            requirement_id: "swarm-command-allowlist",
            fixture_id: "destructive-shell-command-shape",
            fixture_path: "generated://swarm-replay/destructive-shell-command-shape",
            expected_outcome: "blocked-ledger",
            expected_code: SWARM_REPLAY_COMMAND_NOT_ALLOWLISTED_CODE,
            schema_validation_status: "schema_valid_runner_refused",
            test_surface: "contract",
            verbs: &["bash", "rm", "-rf"],
        },
    ]
}

fn adversarial_ledger_cases() -> Vec<AdversarialLedgerCase> {
    vec![
        AdversarialLedgerCase {
            requirement_id: "swarm-redaction-probes-present",
            fixture_id: "empty-redaction-probes",
            fixture_path: "generated://swarm-replay/empty-redaction-probes",
            expected_outcome: "degraded-ledger",
            expected_code: "redaction_probes_passed_false",
            schema_validation_status: "schema_valid_static_check_failed",
            test_surface: "contract",
            mutate: |value| {
                value["redactionProbes"] = Value::Array(Vec::new());
                value["generatorEvidence"]["redactionProbeCount"] = Value::from(0u64);
                Ok(())
            },
        },
        AdversarialLedgerCase {
            requirement_id: "swarm-private-path-metadata-not-rendered",
            fixture_id: "private-path-tail-hash",
            fixture_path: "generated://swarm-replay/private-path-tail-hash",
            expected_outcome: "degraded-ledger",
            expected_code: "private_path_not_rendered",
            schema_validation_status: "schema_valid_support_bundle_redacted",
            test_surface: "contract",
            mutate: |value| {
                value["workspaceShape"]["pathTailHash"] =
                    Value::from("/Users/jemanuel/private-workspace");
                Ok(())
            },
        },
        AdversarialLedgerCase {
            requirement_id: "swarm-redaction-probe-secret-hash-not-rendered",
            fixture_id: "secret-shaped-redaction-probe-hash",
            fixture_path: "generated://swarm-replay/secret-shaped-redaction-probe-hash",
            expected_outcome: "degraded-ledger",
            expected_code: "secret_probe_hash_not_rendered",
            schema_validation_status: "schema_valid_support_bundle_redacted",
            test_surface: "contract",
            mutate: |value| {
                value["redactionProbes"][0]["valueHash"] =
                    Value::from("SECRET_TOKEN=/Users/jemanuel/private-workspace");
                Ok(())
            },
        },
    ]
}

fn expect_domain_error(
    result: Result<ee::core::lab::SwarmReplayResult, DomainError>,
) -> DomainError {
    match result {
        Ok(report) => panic!(
            "expected malformed trace to fail, got replay status {:?}",
            report.status
        ),
        Err(error) => error,
    }
}

fn bounded_structure_fuzz_cases() -> Result<Vec<StructureFuzzCase>, String> {
    let mut cases = Vec::new();

    for index in 0..36 {
        let fixture_id = format!("bounded-structure-fuzz-{index:02}");
        let seed = format!("bounded_structure_fuzz_seed_{index:02}");
        let variant = index % 12;
        let (expected_outcome, expected_code, schema_validation_status, trace_json) = match variant
        {
            0 => (
                "valid-ledger",
                None,
                "schema_valid",
                mutated_trace_json(&seed, |_| Ok(()))?,
            ),
            1 => (
                "error",
                Some("usage"),
                "json_shape_invalid",
                mutated_trace_json(&seed, |value| {
                    value["schema"] = Value::from(index as u64);
                    Ok(())
                })?,
            ),
            2 => (
                "error",
                Some("usage"),
                "schema_invalid",
                mutated_trace_json(&seed, |value| {
                    value["schema"] = Value::from(format!("ee.swarm_workload.v{index}"));
                    Ok(())
                })?,
            ),
            3 => (
                "error",
                Some("usage"),
                "schema_invalid",
                mutated_trace_json(&seed, |value| {
                    value["generatorEvidence"] = Value::String("not-an-object".to_owned());
                    Ok(())
                })?,
            ),
            4 => (
                "error",
                Some("policy_denied"),
                "schema_valid_policy_denied",
                mutated_trace_json(&seed, |value| {
                    value["sideEffectFree"] = Value::Bool(false);
                    Ok(())
                })?,
            ),
            5 => (
                "error",
                Some("usage"),
                "schema_invalid",
                mutated_trace_json(&seed, |value| {
                    value["agentCount"] = Value::from(0u64);
                    Ok(())
                })?,
            ),
            6 => (
                "error",
                Some("usage"),
                "schema_invalid",
                mutated_trace_json(&seed, |value| {
                    value["commandSequence"] = Value::Array(Vec::new());
                    Ok(())
                })?,
            ),
            7 => (
                "error",
                Some("usage"),
                "schema_invalid",
                mutated_trace_json(&seed, |value| {
                    command_sequence_mut(value)?[0]["stepId"] = Value::String("   ".to_owned());
                    Ok(())
                })?,
            ),
            8 => (
                "error",
                Some("usage"),
                "schema_invalid",
                mutated_trace_json(&seed, |value| {
                    command_sequence_mut(value)?[0]["command"]["verbs"] = Value::Array(Vec::new());
                    Ok(())
                })?,
            ),
            9 => (
                "error",
                Some("usage"),
                "schema_invalid",
                mutated_trace_json(&seed, |value| {
                    command_sequence_mut(value)?[0]["command"]["commandHash"] =
                        Value::String(" \t ".to_owned());
                    Ok(())
                })?,
            ),
            10 => (
                "error",
                Some("usage"),
                "schema_invalid",
                mutated_trace_json(&seed, |value| {
                    command_sequence_mut(value)?[0]["dependsOn"] =
                        json!([format!("missing_step_{index}")]);
                    Ok(())
                })?,
            ),
            _ => (
                "blocked-ledger",
                Some(SWARM_REPLAY_COMMAND_NOT_ALLOWLISTED_CODE),
                "schema_valid_runner_refused",
                mutated_trace_json(&seed, |value| {
                    value["resourceProfileHints"]["rchRequired"] = Value::Bool(false);
                    let commands = command_sequence_mut(value)?;
                    commands.truncate(1);
                    commands[0]["command"]["verbs"] = json!(["bash", "rm", "-rf"]);
                    commands[0]["command"]["positionalArity"] = Value::from(0u64);
                    commands[0]["command"]["flagNames"] = Value::Array(Vec::new());
                    Ok(())
                })?,
            ),
        };

        cases.push(StructureFuzzCase {
            requirement_id: "swarm-structure-aware-parser-validator-fuzz",
            fixture_id,
            expected_outcome,
            expected_code,
            schema_validation_status,
            trace_json,
        });
    }

    Ok(cases)
}

fn replay_trace_catching_panic(
    workspace: &Path,
    trace_path: PathBuf,
) -> Result<Result<ee::core::lab::SwarmReplayResult, DomainError>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        replay_swarm_workload_trace(&replay_options(workspace, trace_path, true))
    }))
    .map_err(|_| "swarm replay panicked for fuzz-generated trace".to_owned())
}

fn conformance_entry(
    requirement_id: &str,
    fixture_id: &str,
    fixture_path: &str,
    expected_outcome: &str,
    expected_code: &str,
    schema_validation_status: &str,
    test_surface: &str,
) -> Value {
    json!({
        "requirementId": requirement_id,
        "fixtureId": fixture_id,
        "fixturePath": fixture_path,
        "expectedOutcome": expected_outcome,
        "expectedCode": expected_code,
        "schemaValidationStatus": schema_validation_status,
        "testSurface": test_surface,
    })
}

fn conformance_report_json() -> Result<String, String> {
    let malformed_cases = malformed_trace_cases()?;
    let refusal_cases = refusal_trace_cases();
    let adversarial_ledger_cases = adversarial_ledger_cases();
    let fuzz_cases = bounded_structure_fuzz_cases()?;
    let mut entries = Vec::new();

    for case in &malformed_cases {
        entries.push(conformance_entry(
            case.requirement_id,
            case.fixture_id,
            case.fixture_path,
            case.expected_outcome,
            case.expected_code,
            case.schema_validation_status,
            case.test_surface,
        ));
    }

    for case in &refusal_cases {
        entries.push(conformance_entry(
            case.requirement_id,
            case.fixture_id,
            case.fixture_path,
            case.expected_outcome,
            case.expected_code,
            case.schema_validation_status,
            case.test_surface,
        ));
    }

    for case in &adversarial_ledger_cases {
        entries.push(conformance_entry(
            case.requirement_id,
            case.fixture_id,
            case.fixture_path,
            case.expected_outcome,
            case.expected_code,
            case.schema_validation_status,
            case.test_surface,
        ));
    }

    for case in &fuzz_cases {
        entries.push(conformance_entry(
            case.requirement_id,
            &case.fixture_id,
            &format!("generated://swarm-replay/{}", case.fixture_id),
            case.expected_outcome,
            case.expected_code.unwrap_or("none"),
            case.schema_validation_status,
            "bounded-fuzz-contract",
        ));
    }

    let report = json!({
        "schema": CONFORMANCE_REPORT_SCHEMA_V1,
        "beadId": "bd-ppbue.12",
        "workloadSchema": "ee.swarm_workload.v1",
        "replayResultSchema": SWARM_REPLAY_RESULT_SCHEMA_V1,
        "supportBundleSafety": {
            "rawTraceBodiesIncluded": false,
            "rawCommandStringsIncluded": false,
            "privateAbsolutePathsIncluded": false,
            "dynamicHostFieldsIncluded": false
        },
        "coverage": {
            "malformedCases": malformed_cases.len(),
            "refusalCases": refusal_cases.len(),
            "adversarialLedgerCases": adversarial_ledger_cases.len(),
            "boundedFuzzCases": fuzz_cases.len(),
            "totalEntries": entries.len()
        },
        "guarantees": [
            "no_panic",
            "bounded_trace_bytes",
            "bounded_command_count",
            "deterministic_ordering",
            "stable_error_or_degraded_codes",
            "stdout_machine_data_only",
            "support_bundle_safe",
            "no_local_cargo_fallback"
        ],
        "entries": entries
    });

    let mut rendered = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
    rendered.push('\n');
    Ok(rendered)
}

fn blocked_result_envelope_json() -> Result<String, String> {
    let workspace = workspace("blocked-envelope")?;
    let case = refusal_trace_cases()
        .into_iter()
        .find(|case| case.fixture_id == "destructive-shell-command-shape")
        .ok_or_else(|| "missing blocked replay refusal fixture".to_owned())?;
    let trace_json = mutated_trace_json(case.fixture_id, |value| {
        value["resourceProfileHints"]["rchRequired"] = Value::Bool(false);
        let commands = command_sequence_mut(value)?;
        commands.truncate(1);
        commands[0]["command"]["verbs"] = json!(case.verbs);
        commands[0]["command"]["positionalArity"] = Value::from(0u64);
        commands[0]["command"]["flagNames"] = Value::Array(Vec::new());
        Ok(())
    })?;
    let trace_path = write_trace_fixture(workspace.path(), case.fixture_id, &trace_json)?;
    let report = replay_swarm_workload_trace(&replay_options(workspace.path(), trace_path, true))
        .map_err(|error| error.message())?;
    let rendered_report = report.to_json();
    let report_json: Value =
        serde_json::from_str(&rendered_report).map_err(|error| error.to_string())?;
    let first_failure = report
        .first_failure
        .as_ref()
        .ok_or_else(|| "blocked result missing first failure".to_owned())?;
    let first_result = report
        .command_results
        .first()
        .ok_or_else(|| "blocked result missing command result".to_owned())?;

    ensure(
        report.status == SwarmReplayStatus::Blocked,
        "blocked envelope status",
    )?;
    ensure(
        first_failure.step_id == first_result.step_id,
        "first failure step must match first command result",
    )?;
    ensure(
        first_failure.code == case.expected_code,
        "first failure code mismatch",
    )?;
    ensure(
        !rendered_report.contains("/Users/") && !rendered_report.contains("rm -rf"),
        "full blocked replay result leaked private path or raw destructive command",
    )?;

    let envelope = json!({
        "schema": BLOCKED_RESULT_ENVELOPE_SCHEMA_V1,
        "sourceSchema": report_json["schema"],
        "fixtureId": case.fixture_id,
        "fixturePath": case.fixture_path,
        "expectedOutcome": case.expected_outcome,
        "expectedCode": case.expected_code,
        "status": report_json["status"],
        "sideEffectFree": report_json["sideEffectFree"],
        "aggregate": {
            "commandCount": report_json["aggregate"]["commandCount"],
            "successCount": report_json["aggregate"]["successCount"],
            "failureCount": report_json["aggregate"]["failureCount"],
            "degradedCount": report_json["aggregate"]["degradedCount"]
        },
        "firstFailure": {
            "stepId": "[STEP_ID]",
            "agentSlotMatchesCommandResult": first_failure.agent_slot == first_result.agent_slot,
            "code": report_json["firstFailure"]["code"],
            "severity": report_json["firstFailure"]["severity"],
            "diagnosis": report_json["firstFailure"]["diagnosis"],
            "repairHint": report_json["firstFailure"]["repairHint"]
        },
        "commandResults": [
            {
                "stepId": "[STEP_ID]",
                "commandHash": "[HASH]",
                "exitCode": report_json["commandResults"][0]["exitCode"],
                "degradedCodes": report_json["commandResults"][0]["degradedCodes"],
                "artifactPathCount": first_result.artifact_paths.len(),
                "redactionStatus": report_json["commandResults"][0]["redactionStatus"]
            }
        ],
        "verification": {
            "rchRequired": report_json["verification"]["rchRequired"],
            "rchStatus": report_json["verification"]["rchStatus"],
            "proofLevel": report_json["verification"]["proofCapsule"]["proofLevel"],
            "deterministic": report_json["verification"]["deterministic"],
            "volatileFieldsStripped": report_json["verification"]["volatileFieldsStripped"]
        },
        "supportBundleSafety": {
            "rawCommandStringsIncluded": false,
            "privateAbsolutePathsIncluded": false,
            "rawTraceBodiesIncluded": false,
            "fullResultRedacted": true
        }
    });
    let mut rendered =
        serde_json::to_string_pretty(&envelope).map_err(|error| error.to_string())?;
    rendered.push('\n');
    Ok(rendered)
}

#[test]
fn swarm_replay_malformed_trace_matrix_pins_stable_errors() -> TestResult {
    let workspace = workspace("malformed-matrix")?;
    let mut requirement_ids = BTreeSet::new();

    for case in malformed_trace_cases()? {
        matrix_field_metadata_is_complete(
            case.requirement_id,
            case.fixture_id,
            case.fixture_path,
            case.expected_outcome,
            case.expected_code,
            case.schema_validation_status,
            case.test_surface,
        )?;
        ensure(
            requirement_ids.insert(case.requirement_id),
            format!("duplicate requirement id {}", case.requirement_id),
        )?;
        ensure(
            case.expected_outcome == "error",
            format!("{} must declare error outcome", case.fixture_id),
        )?;

        let trace_path = write_trace_fixture(workspace.path(), case.fixture_id, &case.trace_json)?;
        let error = expect_domain_error(replay_swarm_workload_trace(&replay_options(
            workspace.path(),
            trace_path,
            true,
        )));

        ensure(
            error.code() == case.expected_code,
            format!(
                "{}: expected code {}, got {} ({})",
                case.fixture_id,
                case.expected_code,
                error.code(),
                error.message()
            ),
        )?;
        ensure(
            error.message().contains(case.message_contains),
            format!(
                "{}: message {:?} did not contain {:?}",
                case.fixture_id,
                error.message(),
                case.message_contains
            ),
        )?;
        ensure(
            error
                .repair()
                .is_some_and(|repair| repair.contains(case.repair_contains)),
            format!(
                "{}: repair {:?} did not contain {:?}",
                case.fixture_id,
                error.repair(),
                case.repair_contains
            ),
        )?;
    }

    ensure(
        requirement_ids.len() >= 10,
        format!("matrix too small: {} requirements", requirement_ids.len()),
    )
}

#[test]
fn swarm_replay_refusal_matrix_blocks_risky_command_shapes_without_execution() -> TestResult {
    let workspace = workspace("refusal-matrix")?;

    for case in refusal_trace_cases() {
        matrix_field_metadata_is_complete(
            case.requirement_id,
            case.fixture_id,
            case.fixture_path,
            case.expected_outcome,
            case.expected_code,
            case.schema_validation_status,
            case.test_surface,
        )?;
        ensure(
            case.expected_outcome == "blocked-ledger",
            format!("{} must declare blocked-ledger outcome", case.fixture_id),
        )?;

        let trace_json = mutated_trace_json(case.fixture_id, |value| {
            value["resourceProfileHints"]["rchRequired"] = Value::Bool(false);
            let commands = command_sequence_mut(value)?;
            commands.truncate(1);
            commands[0]["command"]["verbs"] = json!(case.verbs);
            commands[0]["command"]["positionalArity"] = Value::from(0u64);
            commands[0]["command"]["flagNames"] = Value::Array(Vec::new());
            Ok(())
        })?;
        let trace_path = write_trace_fixture(workspace.path(), case.fixture_id, &trace_json)?;
        let report =
            replay_swarm_workload_trace(&replay_options(workspace.path(), trace_path, true))
                .map_err(|error| error.message())?;
        let rendered = report.to_json();

        ensure(report.status == SwarmReplayStatus::Blocked, "status")?;
        ensure(report.aggregate.failure_count == 1, "failure count")?;
        ensure(
            report
                .first_failure
                .as_ref()
                .is_some_and(|failure| failure.code == case.expected_code),
            format!("first failure mismatch for {}", case.fixture_id),
        )?;
        ensure(
            report.command_results[0]
                .degraded_codes
                .iter()
                .any(|code| code == case.expected_code),
            format!("command result missing {}", case.expected_code),
        )?;
        ensure(
            !rendered.contains("rm -rf") && !rendered.contains("/Users/"),
            format!("blocked ledger leaked raw command or private path: {rendered}"),
        )?;
    }

    Ok(())
}

#[test]
fn swarm_replay_adversarial_ledger_cases_remain_support_bundle_safe() -> TestResult {
    let workspace = workspace("adversarial-ledger")?;

    for case in adversarial_ledger_cases() {
        matrix_field_metadata_is_complete(
            case.requirement_id,
            case.fixture_id,
            case.fixture_path,
            case.expected_outcome,
            case.expected_code,
            case.schema_validation_status,
            case.test_surface,
        )?;
        ensure(
            case.expected_outcome == "degraded-ledger",
            format!("{} must declare degraded-ledger outcome", case.fixture_id),
        )?;

        let trace_json = mutated_trace_json(case.fixture_id, case.mutate)?;
        let trace_path = write_trace_fixture(workspace.path(), case.fixture_id, &trace_json)?;
        let first = replay_swarm_workload_trace(&replay_options(
            workspace.path(),
            trace_path.clone(),
            true,
        ))
        .map_err(|error| error.message())?;
        let second =
            replay_swarm_workload_trace(&replay_options(workspace.path(), trace_path, true))
                .map_err(|error| error.message())?;
        let rendered = first.to_json();

        ensure(
            first.status == SwarmReplayStatus::Degraded,
            format!(
                "{} should remain a degraded admission ledger",
                case.fixture_id
            ),
        )?;
        ensure(
            first.first_failure.is_none(),
            format!("{} should not invent a command failure", case.fixture_id),
        )?;
        ensure(
            rendered == second.to_json(),
            format!("{} replay was not deterministic", case.fixture_id),
        )?;
        ensure(
            !rendered.contains("/Users/")
                && !rendered.contains("private-workspace")
                && !rendered.contains("SECRET_TOKEN"),
            format!(
                "{} leaked private or secret-shaped metadata",
                case.fixture_id
            ),
        )?;

        if case.fixture_id == "empty-redaction-probes" {
            ensure(
                !first.redaction_status.redaction_probes_passed,
                "empty redaction probes must fail the redaction probe check",
            )?;
            ensure(
                first
                    .verification
                    .proof_capsule
                    .static_checks
                    .iter()
                    .any(|check| check.name == "redaction_probes" && check.status == "failed"),
                "empty redaction probes must produce a failed static check",
            )?;
        } else {
            ensure(
                first.redaction_status.redaction_probes_passed,
                format!("{} should keep redaction probes passing", case.fixture_id),
            )?;
        }
    }

    Ok(())
}

#[test]
fn swarm_replay_conformance_harness_keeps_valid_generated_trace_admissible() -> TestResult {
    let workspace = workspace("valid-generated")?;
    let trace = generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small(
        "valid_conformance_seed_001",
    ));
    let trace_path =
        write_trace_fixture(workspace.path(), "valid-generated-trace", &trace.to_json())?;

    let first =
        replay_swarm_workload_trace(&replay_options(workspace.path(), trace_path.clone(), true))
            .map_err(|error| error.message())?;
    let second = replay_swarm_workload_trace(&replay_options(workspace.path(), trace_path, true))
        .map_err(|error| error.message())?;

    ensure(first.schema == SWARM_REPLAY_RESULT_SCHEMA_V1, "schema")?;
    ensure(first.status == SwarmReplayStatus::Degraded, "status")?;
    ensure(
        first.command_results.len() == trace.command_sequence.len(),
        "command result count",
    )?;
    ensure(first.aggregate.failure_count == 0, "failure count")?;
    ensure(first.first_failure.is_none(), "first failure")?;
    ensure(
        first.verification.rch_status == SwarmReplayRchStatus::BlockedBeforeCargo,
        "RCH status",
    )?;
    ensure(first.to_json() == second.to_json(), "deterministic replay")
}

#[test]
fn swarm_replay_bounded_structure_fuzz_cases_stay_stable_and_redacted() -> TestResult {
    let workspace = workspace("bounded-fuzz")?;
    let cases = bounded_structure_fuzz_cases()?;
    ensure(cases.len() >= 24, "bounded fuzz corpus too small")?;

    for case in cases {
        matrix_field_metadata_is_complete(
            case.requirement_id,
            &case.fixture_id,
            &format!("generated://swarm-replay/{}", case.fixture_id),
            case.expected_outcome,
            case.expected_code.unwrap_or("none"),
            case.schema_validation_status,
            "bounded-fuzz-contract",
        )?;

        let trace_path = write_trace_fixture(workspace.path(), &case.fixture_id, &case.trace_json)?;
        let first = replay_trace_catching_panic(workspace.path(), trace_path.clone())?;
        let second = replay_trace_catching_panic(workspace.path(), trace_path)?;

        match (case.expected_outcome, first, second) {
            ("valid-ledger", Ok(first_report), Ok(second_report)) => {
                let rendered = first_report.to_json();
                ensure(
                    first_report.status == SwarmReplayStatus::Degraded,
                    format!(
                        "{}: valid fuzz case should dry-run degrade",
                        case.fixture_id
                    ),
                )?;
                ensure(
                    first_report.aggregate.failure_count == 0,
                    format!("{}: valid fuzz case failed commands", case.fixture_id),
                )?;
                ensure(
                    rendered == second_report.to_json(),
                    format!("{}: valid fuzz case was not deterministic", case.fixture_id),
                )?;
                ensure(
                    !rendered.contains("/Users/") && !rendered.contains("rm -rf"),
                    format!("{}: valid ledger leaked forbidden marker", case.fixture_id),
                )?;
            }
            ("blocked-ledger", Ok(first_report), Ok(second_report)) => {
                let expected_code = case
                    .expected_code
                    .ok_or_else(|| format!("{} missing expected code", case.fixture_id))?;
                let rendered = first_report.to_json();
                ensure(
                    first_report.status == SwarmReplayStatus::Blocked,
                    format!("{}: blocked fuzz case status mismatch", case.fixture_id),
                )?;
                ensure(
                    first_report
                        .first_failure
                        .as_ref()
                        .is_some_and(|failure| failure.code == expected_code),
                    format!("{}: blocked fuzz first failure mismatch", case.fixture_id),
                )?;
                ensure(
                    rendered == second_report.to_json(),
                    format!(
                        "{}: blocked fuzz case was not deterministic",
                        case.fixture_id
                    ),
                )?;
                ensure(
                    !rendered.contains("/Users/") && !rendered.contains("rm -rf"),
                    format!(
                        "{}: blocked ledger leaked forbidden marker",
                        case.fixture_id
                    ),
                )?;
            }
            ("error", Err(first_error), Err(second_error)) => {
                let expected_code = case
                    .expected_code
                    .ok_or_else(|| format!("{} missing expected code", case.fixture_id))?;
                ensure(
                    first_error.code() == expected_code,
                    format!(
                        "{}: expected code {}, got {} ({})",
                        case.fixture_id,
                        expected_code,
                        first_error.code(),
                        first_error.message()
                    ),
                )?;
                ensure(
                    first_error.code() == second_error.code()
                        && first_error.message() == second_error.message()
                        && first_error.repair() == second_error.repair(),
                    format!(
                        "{}: error classification was not deterministic",
                        case.fixture_id
                    ),
                )?;
                ensure(
                    !first_error.message().contains("/Users/")
                        && first_error
                            .repair()
                            .is_none_or(|repair| !repair.contains("/Users/")),
                    format!("{}: error leaked private path", case.fixture_id),
                )?;
            }
            (expected, first, second) => {
                return Err(format!(
                    "{}: expected {expected}, got first={:?} second={:?}",
                    case.fixture_id,
                    first.map(|report| report.status),
                    second.map(|report| report.status)
                ));
            }
        }
    }

    Ok(())
}

#[test]
fn swarm_replay_malformed_conformance_report_matches_scrubbed_golden() -> TestResult {
    let rendered = conformance_report_json()?;

    ensure(
        rendered == CONFORMANCE_REPORT_GOLDEN,
        format!(
            "malformed replay conformance report golden drifted\nexpected:\n{CONFORMANCE_REPORT_GOLDEN}\nactual:\n{rendered}"
        ),
    )?;
    ensure(
        !rendered.contains("/Users/") && !rendered.contains("rm -rf"),
        "conformance report leaked private path or raw destructive command",
    )?;
    ensure(
        rendered.contains("\"rawTraceBodiesIncluded\": false"),
        "conformance report must not include raw trace bodies",
    )
}

#[test]
fn swarm_replay_blocked_result_envelope_matches_scrubbed_golden() -> TestResult {
    let rendered = blocked_result_envelope_json()?;

    ensure(
        rendered == BLOCKED_RESULT_ENVELOPE_GOLDEN,
        format!(
            "blocked replay result envelope golden drifted\nexpected:\n{BLOCKED_RESULT_ENVELOPE_GOLDEN}\nactual:\n{rendered}"
        ),
    )?;
    ensure(
        !rendered.contains("/Users/") && !rendered.contains("rm -rf"),
        "blocked replay result envelope leaked private path or raw destructive command",
    )?;
    ensure(
        rendered.contains("\"stepId\": \"[STEP_ID]\"")
            && rendered.contains("\"commandHash\": \"[HASH]\""),
        "blocked replay result envelope must scrub dynamic ids and hashes",
    )
}

#[test]
fn swarm_replay_trace_file_size_cap_refuses_oversized_input_before_parse() -> TestResult {
    let workspace = workspace("oversized")?;
    let oversized = " ".repeat(MAX_LAB_TRACE_BYTES + 1);
    let trace_path = write_trace_fixture(workspace.path(), "oversized-trace", &oversized)?;
    let error = expect_domain_error(replay_swarm_workload_trace(&replay_options(
        workspace.path(),
        trace_path,
        true,
    )));

    ensure(error.code() == "storage", "oversized trace error code")?;
    ensure(
        error.message().contains("read swarm workload trace"),
        format!("unexpected oversized trace context: {}", error.message()),
    )?;
    ensure(
        error.message().contains("exceeds the 16777216 byte cap"),
        format!("missing byte cap in error: {}", error.message()),
    )?;
    ensure(
        error
            .repair()
            .is_some_and(|repair| repair.contains(".ee/lab permissions")),
        format!("missing repair for oversized trace: {:?}", error.repair()),
    )
}
