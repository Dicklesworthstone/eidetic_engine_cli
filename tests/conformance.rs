//! CLI envelope conformance harness for `ee.response.v2` and `ee.error.v2`.
//!
//! The older schema matrix validates representative in-memory artifacts. This
//! harness executes the real binary so drift in JSON-emitting subcommands fails
//! through CI instead of only being caught by manual review.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ee::config::workspace::WORKSPACE_ENV_VAR;
use ee::core::workspace::WORKSPACE_REGISTRY_ENV_VAR;
use insta::{assert_json_snapshot, assert_snapshot};
use serde_json::{Map, Value, json};

#[path = "support/command_inventory.rs"]
mod command_inventory;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const RESPONSE_SCHEMA: &str = "ee.response.v2";
const RESPONSE_SCHEMA_FILE: &str = "ee.response.v2.json";
const ERROR_SCHEMA: &str = "ee.error.v2";
const ERROR_SCHEMA_FILE: &str = "ee.error.v2.json";
const STREAMS_STDERR_PROBE: &str =
    "ee diag streams: stderr probe for stream isolation verification\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnvelopeKind {
    Success,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Enforcement {
    Required,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StderrExpectation {
    Clean,
    Exact(&'static str),
}

#[derive(Clone, Debug)]
struct CommandCase {
    id: &'static str,
    surface: &'static str,
    args: Vec<String>,
    expected: EnvelopeKind,
    schema_file: &'static str,
    enforcement: Enforcement,
    require_recovery: bool,
    stderr_expectation: StderrExpectation,
}

#[derive(Clone, Debug)]
struct ObservedCase {
    id: &'static str,
    surface: &'static str,
    expected: EnvelopeKind,
    enforcement: Enforcement,
    exit_code: Option<i32>,
    stdout_json: bool,
    stderr_valid: bool,
    schema: Option<String>,
    schema_file_valid: bool,
    envelope_valid: bool,
    recovery_valid: Option<bool>,
    failure: Option<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn schema_path(file_name: &str) -> PathBuf {
    repo_root().join("docs").join("schemas").join(file_name)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn run_ee(args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove(WORKSPACE_ENV_VAR)
        .env_remove(WORKSPACE_REGISTRY_ENV_VAR)
        // Envelope conformance must not depend on a worker's model cache or
        // trigger a 506 MB network download. The offline fallback exercises
        // the same response envelope while keeping stderr deterministic.
        .env("EE_EMBED_DOWNLOAD", "off")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_value(output: &Output, case_id: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{case_id}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{case_id}: stdout was not JSON: {error}\nstdout:\n{stdout}"))
}

fn string_at<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}: missing string at {pointer}"))
}

fn object_at<'a>(
    value: &'a Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Map<String, Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{context}: missing object at {pointer}"))
}

fn array_at<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a [Value], String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{context}: missing array at {pointer}"))
}

fn success_case(
    id: &'static str,
    surface: &'static str,
    args: Vec<String>,
    enforcement: Enforcement,
) -> CommandCase {
    CommandCase {
        id,
        surface,
        args,
        expected: EnvelopeKind::Success,
        schema_file: RESPONSE_SCHEMA_FILE,
        enforcement,
        require_recovery: false,
        stderr_expectation: StderrExpectation::Clean,
    }
}

fn success_case_with_stderr(
    id: &'static str,
    surface: &'static str,
    args: Vec<String>,
    enforcement: Enforcement,
    stderr_expectation: StderrExpectation,
) -> CommandCase {
    CommandCase {
        stderr_expectation,
        ..success_case(id, surface, args, enforcement)
    }
}

fn error_case(
    id: &'static str,
    surface: &'static str,
    args: Vec<String>,
    require_recovery: bool,
) -> CommandCase {
    CommandCase {
        id,
        surface,
        args,
        expected: EnvelopeKind::Error,
        schema_file: ERROR_SCHEMA_FILE,
        enforcement: Enforcement::Required,
        require_recovery,
        stderr_expectation: StderrExpectation::Clean,
    }
}

fn validate_stderr(case: &CommandCase, stderr: &[u8]) -> TestResult {
    let stderr = std::str::from_utf8(stderr)
        .map_err(|error| format!("{}: stderr was not UTF-8: {error}", case.id))?;
    match case.stderr_expectation {
        StderrExpectation::Clean if stderr.is_empty() => Ok(()),
        StderrExpectation::Clean => Err(format!(
            "{}: stderr must be empty, got {:?}",
            case.id,
            bounded_diagnostic(stderr)
        )),
        StderrExpectation::Exact(expected) if stderr == expected => Ok(()),
        StderrExpectation::Exact(expected) => Err(format!(
            "{}: stderr mismatch: expected {:?}, got {:?}",
            case.id,
            expected,
            bounded_diagnostic(stderr)
        )),
    }
}

fn bounded_diagnostic(text: &str) -> String {
    const MAX_CHARS: usize = 512;
    let mut chars = text.chars();
    let bounded = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

#[test]
fn response_envelope_harness_uses_shared_command_inventory() -> TestResult {
    let inventory = command_inventory::ee_command_inventory_by_path();
    let paths = command_inventory::ee_command_paths();
    for command_path in [
        "init",
        "remember",
        "search",
        "pack",
        "context",
        "why",
        "status",
        "doctor",
        "curate show",
        "reflect propose",
        "reflect request-ledger diagnostics",
        "swarm brief",
        "swarm next-action",
        "swarm work-packet",
        "diag streams",
        "diag plan-cache",
        "diag dependencies",
        "diag host-profile",
        "diag artifacts",
        "diag build-admission",
    ] {
        if !paths.contains(command_path) {
            return Err(format!(
                "response envelope conformance case `{command_path}` is not in the shared clap-derived path set"
            ));
        }
        let entry = inventory.get(command_path).ok_or_else(|| {
            format!(
                "response envelope conformance case `{command_path}` is not in the shared clap-derived inventory"
            )
        })?;
        if !entry.supports_json {
            return Err(format!(
                "response envelope conformance case `{command_path}` is not marked JSON-capable"
            ));
        }
        if entry.declared_response_schema != RESPONSE_SCHEMA {
            return Err(format!(
                "response envelope conformance case `{command_path}` declared schema {}, expected {RESPONSE_SCHEMA}",
                entry.declared_response_schema
            ));
        }
    }
    Ok(())
}

fn assess_case(case: &CommandCase) -> Result<(ObservedCase, Option<Value>), String> {
    let output = run_ee(&case.args)?;
    let stderr_result = validate_stderr(case, &output.stderr);
    let stderr_valid = stderr_result.is_ok();
    let stderr_failure = stderr_result.err();
    let value = match stdout_value(&output, case.id) {
        Ok(value) => value,
        Err(error) => {
            let failure = stderr_failure.map_or(error.clone(), |stderr_error| {
                format!("{error}; {stderr_error}")
            });
            return Ok((
                ObservedCase {
                    id: case.id,
                    surface: case.surface,
                    expected: case.expected,
                    enforcement: case.enforcement,
                    exit_code: output.status.code(),
                    stdout_json: false,
                    stderr_valid,
                    schema: None,
                    schema_file_valid: false,
                    envelope_valid: false,
                    recovery_valid: None,
                    failure: Some(failure),
                },
                None,
            ));
        }
    };

    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let schema_doc = read_json(&schema_path(case.schema_file))?;
    let schema_result = validate_json_schema(&value, &schema_doc, &schema_doc, "$");
    let schema_file_valid = schema_result.is_ok();
    let schema_failure = schema_result
        .err()
        .map(|error| format!("docs schema validation failed: {error}"));
    let envelope_result = validate_envelope(case, &value, output.status.code());
    let recovery_valid = match case.expected {
        EnvelopeKind::Success => None,
        EnvelopeKind::Error if case.require_recovery => Some(
            array_at(&value, "/error/details/recovery", case.id)
                .map(|entries| {
                    !entries.is_empty()
                        && entries.iter().all(|entry| {
                            entry.get("priority").and_then(Value::as_u64).is_some()
                                && entry.get("kind").and_then(Value::as_str).is_some()
                                && entry.get("rationale").and_then(Value::as_str).is_some()
                                && entry.get("riskClass").and_then(Value::as_str).is_some()
                                && entry
                                    .get("requiresHumanApproval")
                                    .and_then(Value::as_bool)
                                    .is_some()
                                && entry
                                    .get("mutatesExternalState")
                                    .and_then(Value::as_bool)
                                    .is_some()
                                && entry
                                    .get("mutatesTrackerState")
                                    .and_then(Value::as_bool)
                                    .is_some()
                                && entry.get("privacyClass").and_then(Value::as_str).is_some()
                        })
                })
                .unwrap_or(false),
        ),
        EnvelopeKind::Error => None,
    };

    let envelope_failure = envelope_result.err();
    let envelope_valid = envelope_failure.is_none() && recovery_valid.unwrap_or(true);
    let mut failures = Vec::new();
    failures.extend(envelope_failure);
    failures.extend(schema_failure);
    failures.extend(stderr_failure);
    if case.require_recovery && recovery_valid != Some(true) {
        failures
            .push("error.details.recovery did not satisfy structured recovery contract".to_owned());
    }
    let failure = (!failures.is_empty()).then(|| failures.join("; "));

    Ok((
        ObservedCase {
            id: case.id,
            surface: case.surface,
            expected: case.expected,
            enforcement: case.enforcement,
            exit_code: output.status.code(),
            stdout_json: true,
            stderr_valid,
            schema,
            schema_file_valid,
            envelope_valid,
            recovery_valid,
            failure,
        },
        Some(value),
    ))
}

fn validate_envelope(case: &CommandCase, value: &Value, exit_code: Option<i32>) -> TestResult {
    match case.expected {
        EnvelopeKind::Success => {
            if exit_code != Some(EXIT_SUCCESS) {
                return Err(format!(
                    "{}: expected success exit 0, got {exit_code:?}",
                    case.id
                ));
            }
            if string_at(value, "/schema", case.id)? != RESPONSE_SCHEMA {
                return Err(format!(
                    "{}: success envelope must use {RESPONSE_SCHEMA}",
                    case.id
                ));
            }
            if value.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(format!(
                    "{}: success envelope must set success=true",
                    case.id
                ));
            }
            object_at(value, "/data", case.id)?;
            if value.get("error").is_some() {
                return Err(format!(
                    "{}: success envelope must not include error",
                    case.id
                ));
            }
            let degraded = value
                .get("degraded")
                .ok_or_else(|| format!("{}: success envelope must include degraded", case.id))?;
            ensure_degraded_array(degraded, case.id)?;
            validate_pack_command_identity(case, value)?;
        }
        EnvelopeKind::Error => {
            if exit_code == Some(EXIT_SUCCESS) {
                return Err(format!("{}: expected non-zero error exit", case.id));
            }
            if string_at(value, "/schema", case.id)? != ERROR_SCHEMA {
                return Err(format!(
                    "{}: error envelope must use {ERROR_SCHEMA}",
                    case.id
                ));
            }
            if value.get("success").is_some() {
                return Err(format!(
                    "{}: error envelope must not include success",
                    case.id
                ));
            }
            let error = object_at(value, "/error", case.id)?;
            for field in ["code", "message", "severity", "repair"] {
                if !error.get(field).is_some_and(Value::is_string) {
                    return Err(format!("{}: error.{field} must be a string", case.id));
                }
            }
            object_at(value, "/error/details", case.id)?;
            if let Some(degraded) = value.get("degraded") {
                ensure_degraded_array(degraded, case.id)?;
            }
        }
    }
    Ok(())
}

fn validate_pack_command_identity(case: &CommandCase, value: &Value) -> TestResult {
    if !matches!(case.id, "ENV-PACK" | "ENV-CONTEXT-ALIAS") {
        return Ok(());
    }
    if string_at(value, "/data/command", case.id)? != "pack" {
        return Err(format!(
            "{}: canonical and alias pack responses must set data.command=pack",
            case.id
        ));
    }

    let root_has_alias = degradation_has_code(value, "/degraded", "deprecated_alias", case.id)?;
    let data_has_alias =
        degradation_has_code(value, "/data/degraded", "deprecated_alias", case.id)?;
    match case.id {
        "ENV-CONTEXT-ALIAS" if root_has_alias && data_has_alias => Ok(()),
        "ENV-CONTEXT-ALIAS" => Err(format!(
            "{}: deprecated context alias must mirror deprecated_alias in data.degraded and top-level degraded",
            case.id
        )),
        "ENV-PACK" if !root_has_alias && !data_has_alias => Ok(()),
        "ENV-PACK" => Err(format!(
            "{}: canonical pack response must not emit deprecated_alias",
            case.id
        )),
        _ => Ok(()),
    }
}

fn degradation_has_code(
    value: &Value,
    pointer: &str,
    code: &str,
    case_id: &str,
) -> Result<bool, String> {
    Ok(array_at(value, pointer, case_id)?
        .iter()
        .any(|entry| entry.get("code").and_then(Value::as_str) == Some(code)))
}

fn ensure_degraded_array(value: &Value, case_id: &str) -> TestResult {
    let entries = value
        .as_array()
        .ok_or_else(|| format!("{case_id}: degraded must be an array"))?;
    for (index, entry) in entries.iter().enumerate() {
        for field in ["code", "severity", "message"] {
            if !entry.get(field).is_some_and(Value::is_string) {
                return Err(format!(
                    "{case_id}: degraded[{index}].{field} must be a string"
                ));
            }
        }
    }
    Ok(())
}

fn enforcement_label(enforcement: Enforcement) -> Value {
    match enforcement {
        Enforcement::Required => json!("required"),
    }
}

fn case_catalog(cases: &[CommandCase]) -> Value {
    Value::Array(
        cases
            .iter()
            .map(|case| {
                json!({
                    "id": case.id,
                    "surface": case.surface,
                    "args": case.args.iter().map(|arg| scrub_arg(arg)).collect::<Vec<_>>(),
                    "expected": match case.expected {
                        EnvelopeKind::Success => "success",
                        EnvelopeKind::Error => "error",
                    },
                    "schemaFile": case.schema_file,
                    "enforcement": enforcement_label(case.enforcement),
                    "requireRecovery": case.require_recovery,
                })
            })
            .collect(),
    )
}

fn scrub_arg(arg: &str) -> String {
    scrub_arg_with_temp_root(arg, &std::env::temp_dir())
}

fn scrub_arg_with_temp_root(arg: &str, temp_root: &Path) -> String {
    let arg_path = Path::new(arg);
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    if arg.starts_with("mem_") {
        "[MEMORY_ID]".to_owned()
    } else if arg_path.starts_with(repo)
        || arg_path.starts_with(temp_root)
        || arg.starts_with("/var/folders/")
        || arg.starts_with("/tmp/")
        || arg.starts_with("/private/tmp/")
    {
        "[WORKSPACE]".to_owned()
    } else {
        arg.to_owned()
    }
}

#[test]
fn scrub_arg_uses_runtime_temp_root_without_hiding_unrelated_paths() {
    let runtime_temp = Path::new("/worker/project/.rch-tmp");
    assert_eq!(
        scrub_arg_with_temp_root(
            "/worker/project/.rch-tmp/.tmp-response-envelope",
            runtime_temp,
        ),
        "[WORKSPACE]"
    );
    assert_eq!(
        scrub_arg_with_temp_root("/opt/ee/fixtures/config.toml", runtime_temp),
        "/opt/ee/fixtures/config.toml"
    );
}

fn expected_conformance_matrix(cases: &[CommandCase]) -> String {
    let mut matrix = String::from(
        "| subcommand | envelope | schema file | recovery required | enforcement |\n\
         | --- | --- | --- | --- | --- |\n",
    );
    for case in cases {
        let envelope = match case.expected {
            EnvelopeKind::Success => "success",
            EnvelopeKind::Error => "error",
        };
        let enforcement = match case.enforcement {
            Enforcement::Required => "required",
        };
        matrix.push_str(&format!(
            "| {} | {envelope} | {} | {} | {enforcement} |\n",
            case.surface,
            case.schema_file,
            bool_cell(case.require_recovery),
        ));
    }
    matrix
}

fn conformance_matrix(rows: &[ObservedCase]) -> String {
    let mut matrix = String::from(
        "| subcommand | envelope | exit code | schema | docs schema | required fields | recovery | status |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for row in rows {
        let envelope = match row.expected {
            EnvelopeKind::Success => "success",
            EnvelopeKind::Error => "error",
        };
        let status = match row.enforcement {
            Enforcement::Required if row.conforms() => "pass".to_owned(),
            Enforcement::Required => "fail".to_owned(),
        };
        matrix.push_str(&format!(
            "| {} | {envelope} | {} | {} | {} | {} | {} | {status} |\n",
            row.surface,
            row.exit_code
                .map_or_else(|| "<signal>".to_owned(), |code| code.to_string()),
            row.schema.as_deref().unwrap_or("<missing>"),
            bool_cell(row.schema_file_valid),
            bool_cell(row.envelope_valid),
            recovery_cell(row.recovery_valid),
        ));
    }
    matrix
}

fn bool_cell(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn recovery_cell(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "n/a",
    }
}

impl ObservedCase {
    fn conforms(&self) -> bool {
        self.stdout_json
            && self.stderr_valid
            && self.schema_file_valid
            && self.envelope_valid
            && self.recovery_valid.unwrap_or(true)
    }
}

#[test]
fn cli_json_envelopes_conform_to_response_v2_and_error_v2() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let missing_workspace = tempdir.path().join("missing-db-workspace");
    fs::create_dir_all(&missing_workspace).map_err(|error| error.to_string())?;
    let missing_workspace = missing_workspace.to_string_lossy().to_string();

    let mut base_cases = vec![
        success_case(
            "ENV-INIT",
            "init",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "init".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-REMEMBER",
            "remember",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "remember".to_owned(),
                "Conformance harness memory used to exercise ee.response.v2.".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
            ],
            Enforcement::Required,
        ),
    ];

    let mut observed = Vec::new();
    let mut memory_id = None;
    for case in base_cases.iter() {
        let (row, value) = assess_case(&case)?;
        if case.id == "ENV-REMEMBER"
            && let Some(ref value) = value
        {
            memory_id = value
                .pointer("/data/memory_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        observed.push(row);
    }
    let memory_id =
        memory_id.ok_or_else(|| "remember response did not include data.memory_id".to_owned())?;

    let followup_cases = vec![
        success_case(
            "ENV-SEARCH",
            "search",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "search".to_owned(),
                "conformance response envelope".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-PACK",
            "pack",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "pack".to_owned(),
                "conformance response envelope".to_owned(),
                "--max-tokens".to_owned(),
                "1200".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-CONTEXT-ALIAS",
            "context",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "context".to_owned(),
                "conformance response envelope".to_owned(),
                "--max-tokens".to_owned(),
                "1200".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-WHY",
            "why",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "why".to_owned(),
                memory_id,
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-STATUS",
            "status",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "status".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-DOCTOR",
            "doctor",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "doctor".to_owned(),
                "--quick".to_owned(),
            ],
            Enforcement::Required,
        ),
        error_case(
            "ENV-PACK-STORAGE-ERROR",
            "pack missing database",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                missing_workspace,
                "pack".to_owned(),
                "missing database recovery contract".to_owned(),
            ],
            true,
        ),
        error_case(
            "ENV-CURATE-SHOW",
            "curate show",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "curate".to_owned(),
                "show".to_owned(),
                "cand_missing_conformance".to_owned(),
            ],
            false,
        ),
        error_case(
            "ENV-REFLECT-PROPOSE",
            "reflect propose",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "reflect".to_owned(),
                "propose".to_owned(),
                "--kind".to_owned(),
                "gaps".to_owned(),
                "--source-memory".to_owned(),
                "mem_missing_conformance".to_owned(),
                "--dry-run".to_owned(),
            ],
            false,
        ),
        success_case(
            "ENV-REFLECT-LEDGER-DIAGNOSTICS",
            "reflect request-ledger diagnostics",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "reflect".to_owned(),
                "request-ledger".to_owned(),
                "diagnostics".to_owned(),
                "--now".to_owned(),
                "2026-05-24T00:00:00Z".to_owned(),
                "--limit".to_owned(),
                "3".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-SWARM-BRIEF",
            "swarm brief",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "swarm".to_owned(),
                "brief".to_owned(),
                "--sources".to_owned(),
                "none".to_owned(),
                "--command-timeout-ms".to_owned(),
                "10".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-SWARM-NEXT-ACTION",
            "swarm next-action",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "swarm".to_owned(),
                "next-action".to_owned(),
                "--sources".to_owned(),
                "none".to_owned(),
                "--command-timeout-ms".to_owned(),
                "10".to_owned(),
            ],
            // bd-iky0b (closed): swarm next-action now wraps in
            // ee.response.v2; the previous fixed_gap is removed.
            Enforcement::Required,
        ),
        success_case(
            "ENV-SWARM-WORK-PACKET",
            "swarm work-packet",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "swarm".to_owned(),
                "work-packet".to_owned(),
                "--sources".to_owned(),
                "none".to_owned(),
                "--command-timeout-ms".to_owned(),
                "10".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case_with_stderr(
            "ENV-DIAG-STREAMS",
            "diag streams",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "diag".to_owned(),
                "streams".to_owned(),
            ],
            Enforcement::Required,
            StderrExpectation::Exact(STREAMS_STDERR_PROBE),
        ),
        success_case(
            "ENV-DIAG-PLAN-CACHE",
            "diag plan-cache",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "diag".to_owned(),
                "plan-cache".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-DIAG-DEPENDENCIES",
            "diag dependencies",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "diag".to_owned(),
                "dependencies".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-DIAG-HOST-PROFILE",
            "diag host-profile",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "diag".to_owned(),
                "host-profile".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-DIAG-ARTIFACTS",
            "diag artifacts",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "diag".to_owned(),
                "artifacts".to_owned(),
            ],
            Enforcement::Required,
        ),
        success_case(
            "ENV-DIAG-BUILD-ADMISSION",
            "diag build-admission",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace,
                "diag".to_owned(),
                "build-admission".to_owned(),
                "--min-free-bytes".to_owned(),
                "0".to_owned(),
                "--artifact-destination".to_owned(),
                "target/sync-down".to_owned(),
            ],
            Enforcement::Required,
        ),
    ];

    base_cases.extend(followup_cases.iter().cloned());
    assert_snapshot!(
        "response_error_envelope_compliance_matrix",
        expected_conformance_matrix(&base_cases)
    );
    assert_json_snapshot!(
        "response_error_envelope_case_catalog",
        case_catalog(&base_cases)
    );

    for case in followup_cases {
        let (row, value) = assess_case(&case)?;
        drop(value);
        observed.push(row);
    }

    let failures = observed
        .iter()
        .filter(|row| row.enforcement == Enforcement::Required && !row.conforms())
        .map(|row| {
            format!(
                "{} ({}) failed: {}",
                row.id,
                row.surface,
                row.failure
                    .as_deref()
                    .unwrap_or("unknown envelope conformance failure")
            )
        })
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "required envelope conformance failures:\n{}",
            failures.join("\n"),
        ) + &format!("\n\nObserved matrix:\n{}", conformance_matrix(&observed)))
    }
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

    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        if options
            .iter()
            .any(|candidate| validate_json_schema(value, candidate, root_schema, path).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path} did not match any oneOf branch"));
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

    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        for candidate in all_of {
            validate_json_schema(value, candidate, root_schema, path)?;
        }
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
            match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    return Err(format!("{path} contains unexpected field {key}"));
                }
                Some(Value::Bool(true)) | None => {}
                Some(Value::Object(property_schema)) => {
                    validate_json_schema(
                        child,
                        &Value::Object(property_schema.clone()),
                        root_schema,
                        &child_path,
                    )?;
                }
                Some(other) => {
                    return Err(format!(
                        "{path} has unsupported additionalProperties schema {other}"
                    ));
                }
            }
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
            && array.len() < min_items as usize
        {
            return Err(format!("{path} has fewer than {min_items} items"));
        }
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64)
            && array.len() > max_items as usize
        {
            return Err(format!("{path} has more than {max_items} items"));
        }
        if let Some(prefix_items) = schema.get("prefixItems").and_then(Value::as_array) {
            for (index, item_schema) in prefix_items.iter().enumerate() {
                if let Some(item) = array.get(index) {
                    validate_json_schema(
                        item,
                        item_schema,
                        root_schema,
                        &format!("{path}[{index}]"),
                    )?;
                }
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_json_schema(item, item_schema, root_schema, &format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn resolve_ref<'a>(root_schema: &'a Value, reference: &str) -> Result<&'a Value, String> {
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
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
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
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
