use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value as JsonValue, json};

type TestResult = Result<(), String>;

const BOUNDARY_LOG_SCHEMA: &str = "ee.e2e.boundary_log.v1";

#[derive(Debug, PartialEq)]
enum BoundaryLogFailure {
    InvalidLogSchema,
    MissingRequiredField(&'static str),
    StdoutPollution,
    StdoutJsonInvalid(String),
    SchemaMismatch { expected: String, observed: String },
    EnvNotRedacted { key: String },
    MissingMatrixRow { surface: String },
    UnexpectedMutation { before: u64, after: u64 },
    MissingFixtureHash { fixture_id: String },
    MissingReproductionCommand,
}

struct BoundaryLogRecord {
    command: String,
    argv: Vec<String>,
    cwd: PathBuf,
    workspace: Option<PathBuf>,
    env_sanitized: JsonValue,
    started_at_unix_ms: u128,
    ended_at_unix_ms: u128,
    elapsed_ms: u128,
    exit_code: Option<i32>,
    stdout_artifact_path: PathBuf,
    stderr_artifact_path: PathBuf,
    expected_schema: String,
    golden_path: Option<String>,
    golden_status: String,
    redaction_status: String,
    evidence_ids: Vec<String>,
    degradation_codes: Vec<String>,
    mutation_summary: String,
    command_boundary_matrix_row: Option<String>,
    fixture_hashes: BTreeMap<String, String>,
    db_generation_before: Option<u64>,
    db_generation_after: Option<u64>,
    index_generation_before: Option<u64>,
    index_generation_after: Option<u64>,
    runtime_budget: Option<u64>,
    cancellation_status: String,
    reproduction_command: String,
    first_failure: Option<String>,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_dossier_dir(scenario: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join(scenario)
        .join(format!("{}-{now}", std::process::id())))
}

fn unix_ms_now() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| format!("clock moved backwards: {error}"))
}

fn write_step_artifacts(dir: &Path, output: &Output) -> TestResult {
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    fs::write(dir.join("stdout"), &output.stdout).map_err(|error| error.to_string())?;
    fs::write(dir.join("stderr"), &output.stderr).map_err(|error| error.to_string())
}

fn write_boundary_log(dir: &Path, record: &BoundaryLogRecord) -> TestResult {
    let value = json!({
        "schema": BOUNDARY_LOG_SCHEMA,
        "command": record.command,
        "argv": record.argv,
        "cwd": record.cwd.display().to_string(),
        "workspace": record.workspace.as_ref().map(|path| path.display().to_string()),
        "env_sanitized": record.env_sanitized,
        "started_at_unix_ms": record.started_at_unix_ms,
        "ended_at_unix_ms": record.ended_at_unix_ms,
        "elapsed_ms": record.elapsed_ms,
        "exit_code": record.exit_code,
        "stdout_artifact_path": record.stdout_artifact_path.display().to_string(),
        "stderr_artifact_path": record.stderr_artifact_path.display().to_string(),
        "stdout_json_valid": true,
        "schema_validation": {
            "expected": record.expected_schema,
            "observed": observed_schema_from_stdout(&record.stdout_artifact_path),
            "status": "pending"
        },
        "golden_validation": {
            "path": record.golden_path,
            "status": record.golden_status
        },
        "redaction_status": {
            "status": record.redaction_status,
            "classes": []
        },
        "evidence_ids": record.evidence_ids,
        "degradation_codes": record.degradation_codes,
        "mutation_summary": record.mutation_summary,
        "command_boundary_matrix_row": record.command_boundary_matrix_row,
        "fixture_hashes": record.fixture_hashes,
        "db_generation_before": record.db_generation_before,
        "db_generation_after": record.db_generation_after,
        "index_generation_before": record.index_generation_before,
        "index_generation_after": record.index_generation_after,
        "runtime_budget": record.runtime_budget,
        "cancellation_status": record.cancellation_status,
        "reproduction_command": record.reproduction_command,
        "first_failure": record.first_failure
    });
    let mut content = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    content.push('\n');
    fs::write(dir.join("boundary-log.json"), content).map_err(|error| error.to_string())
}

fn validate_boundary_log(log_path: &Path) -> Result<(), BoundaryLogFailure> {
    let log_content = fs::read_to_string(log_path)
        .map_err(|error| BoundaryLogFailure::StdoutJsonInvalid(error.to_string()))?;
    let log: JsonValue = serde_json::from_str(&log_content)
        .map_err(|error| BoundaryLogFailure::StdoutJsonInvalid(error.to_string()))?;
    if log.get("schema").and_then(JsonValue::as_str) != Some(BOUNDARY_LOG_SCHEMA) {
        return Err(BoundaryLogFailure::InvalidLogSchema);
    }

    for field in [
        "command",
        "argv",
        "cwd",
        "env_sanitized",
        "started_at_unix_ms",
        "ended_at_unix_ms",
        "elapsed_ms",
        "exit_code",
        "stdout_artifact_path",
        "stderr_artifact_path",
        "schema_validation",
        "golden_validation",
        "redaction_status",
        "evidence_ids",
        "degradation_codes",
        "mutation_summary",
        "command_boundary_matrix_row",
        "fixture_hashes",
        "db_generation_before",
        "db_generation_after",
        "index_generation_before",
        "index_generation_after",
        "runtime_budget",
        "cancellation_status",
        "reproduction_command",
        "first_failure",
    ] {
        if log.get(field).is_none() {
            return Err(BoundaryLogFailure::MissingRequiredField(field));
        }
    }

    let expected_schema = log
        .pointer("/schema_validation/expected")
        .and_then(JsonValue::as_str)
        .ok_or(BoundaryLogFailure::MissingRequiredField(
            "schema_validation.expected",
        ))?;
    let stdout_path = log
        .get("stdout_artifact_path")
        .and_then(JsonValue::as_str)
        .ok_or(BoundaryLogFailure::MissingRequiredField(
            "stdout_artifact_path",
        ))?;
    let stdout = fs::read_to_string(stdout_path)
        .map_err(|error| BoundaryLogFailure::StdoutJsonInvalid(error.to_string()))?;

    if !stdout.trim_start().starts_with('{') {
        return Err(BoundaryLogFailure::StdoutPollution);
    }

    let parsed: JsonValue = serde_json::from_str(&stdout)
        .map_err(|error| BoundaryLogFailure::StdoutJsonInvalid(error.to_string()))?;
    let observed_schema = parsed
        .get("schema")
        .and_then(JsonValue::as_str)
        .unwrap_or("<missing>");
    if observed_schema != expected_schema {
        return Err(BoundaryLogFailure::SchemaMismatch {
            expected: expected_schema.to_owned(),
            observed: observed_schema.to_owned(),
        });
    }

    Ok(())
}

fn validate_boundary_log_extended(log_path: &Path) -> Result<(), BoundaryLogFailure> {
    validate_boundary_log(log_path)?;

    let log_content = fs::read_to_string(log_path)
        .map_err(|error| BoundaryLogFailure::StdoutJsonInvalid(error.to_string()))?;
    let log: JsonValue = serde_json::from_str(&log_content)
        .map_err(|error| BoundaryLogFailure::StdoutJsonInvalid(error.to_string()))?;

    let mutation_summary = log
        .get("mutation_summary")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let db_before = log.get("db_generation_before").and_then(JsonValue::as_u64);
    let db_after = log.get("db_generation_after").and_then(JsonValue::as_u64);
    if mutation_summary == "read_only" {
        if let (Some(before), Some(after)) = (db_before, db_after) {
            if before != after {
                return Err(BoundaryLogFailure::UnexpectedMutation { before, after });
            }
        }
    }

    validate_env_redaction(&log)?;

    let reproduction_command = log
        .get("reproduction_command")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if reproduction_command.trim().is_empty() {
        return Err(BoundaryLogFailure::MissingReproductionCommand);
    }

    Ok(())
}

fn validate_env_redaction(log: &JsonValue) -> Result<(), BoundaryLogFailure> {
    let Some(env) = log.get("env_sanitized").and_then(JsonValue::as_object) else {
        return Err(BoundaryLogFailure::MissingRequiredField("env_sanitized"));
    };

    let sensitive_fragments = ["SECRET", "TOKEN", "PASSWORD", "KEY", "CREDENTIAL"];
    for (key, value) in env {
        let upper_key = key.to_ascii_uppercase();
        let sensitive_key = sensitive_fragments
            .iter()
            .any(|fragment| upper_key.contains(fragment));
        if sensitive_key
            && value
                .as_str()
                .is_some_and(|raw| raw != "<redacted>" && raw != "<omitted>")
        {
            return Err(BoundaryLogFailure::EnvNotRedacted { key: key.clone() });
        }
    }

    Ok(())
}

fn first_failure_for_boundary_failure(failure: &BoundaryLogFailure) -> String {
    match failure {
        BoundaryLogFailure::InvalidLogSchema => "schema_mismatch:<boundary-log>".to_owned(),
        BoundaryLogFailure::MissingRequiredField(field) => {
            format!("missing_required_field:{field}")
        }
        BoundaryLogFailure::StdoutPollution => "stdout_pollution".to_owned(),
        BoundaryLogFailure::StdoutJsonInvalid(reason) => {
            format!("stdout_json_invalid:{reason}")
        }
        BoundaryLogFailure::SchemaMismatch { observed, .. } => {
            format!("schema_mismatch:{observed}")
        }
        BoundaryLogFailure::EnvNotRedacted { key } => format!("env_not_redacted:{key}"),
        BoundaryLogFailure::MissingMatrixRow { surface } => {
            format!("missing_matrix_row:{surface}")
        }
        BoundaryLogFailure::UnexpectedMutation { .. } => "unexpected_mutation".to_owned(),
        BoundaryLogFailure::MissingFixtureHash { fixture_id } => {
            format!("missing_fixture_hash:{fixture_id}")
        }
        BoundaryLogFailure::MissingReproductionCommand => "missing_reproduction_command".to_owned(),
    }
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }

    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':' | b'=')
    }) {
        return value.to_owned();
    }

    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

fn render_reproduction_command(cwd: &Path, command: &str, argv: &[String]) -> String {
    let mut parts = vec![
        "cd".to_owned(),
        shell_quote(&cwd.display().to_string()),
        "&&".to_owned(),
        shell_quote(command),
    ];
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn observed_schema_from_stdout(path: &Path) -> String {
    let Ok(stdout) = fs::read_to_string(path) else {
        return "<unreadable>".to_owned();
    };
    serde_json::from_str::<JsonValue>(&stdout)
        .ok()
        .and_then(|value| {
            value
                .get("schema")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "<invalid-json>".to_owned())
}

fn build_boundary_log_summary(log_paths: &[PathBuf]) -> Result<JsonValue, String> {
    let mut steps = Vec::new();
    for path in log_paths {
        let log_content = fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let log: JsonValue = serde_json::from_str(&log_content)
            .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))?;
        let command = log
            .get("command")
            .and_then(JsonValue::as_str)
            .unwrap_or("<missing>")
            .to_owned();
        let argv = log
            .get("argv")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let first_failure = log.get("first_failure").cloned().unwrap_or(JsonValue::Null);
        steps.push(json!({
            "command": command,
            "argv": argv,
            "schema": log.get("schema").and_then(JsonValue::as_str).unwrap_or("<missing>"),
            "exit_code": log.get("exit_code").cloned().unwrap_or(JsonValue::Null),
            "first_failure": first_failure
        }));
    }

    steps.sort_by(|left, right| {
        let left_key = format!(
            "{} {:?}",
            left.get("command")
                .and_then(JsonValue::as_str)
                .unwrap_or(""),
            left.get("argv")
        );
        let right_key = format!(
            "{} {:?}",
            right
                .get("command")
                .and_then(JsonValue::as_str)
                .unwrap_or(""),
            right.get("argv")
        );
        left_key.cmp(&right_key)
    });

    Ok(json!({
        "schema": "ee.e2e.boundary_log.summary.v1",
        "step_count": steps.len(),
        "steps": steps
    }))
}

fn make_record(
    dir: &Path,
    output: &Output,
    expected_schema: &str,
) -> Result<BoundaryLogRecord, String> {
    let started = unix_ms_now()?;
    let ended = unix_ms_now()?;
    let command = "ee".to_owned();
    let argv = vec!["--json".to_owned(), "status".to_owned()];
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    Ok(BoundaryLogRecord {
        command: command.clone(),
        argv: argv.clone(),
        cwd: cwd.clone(),
        workspace: None,
        env_sanitized: json!({
            "overrides": {},
            "sensitive_env_omitted": true
        }),
        started_at_unix_ms: started,
        ended_at_unix_ms: ended,
        elapsed_ms: ended.saturating_sub(started),
        exit_code: output.status.code(),
        stdout_artifact_path: dir.join("stdout"),
        stderr_artifact_path: dir.join("stderr"),
        expected_schema: expected_schema.to_owned(),
        golden_path: None,
        golden_status: "not_applicable".to_owned(),
        redaction_status: "checked".to_owned(),
        evidence_ids: vec!["fixture.boundary_logging.status".to_owned()],
        degradation_codes: Vec::new(),
        mutation_summary: "read_only".to_owned(),
        command_boundary_matrix_row: Some("status".to_owned()),
        fixture_hashes: BTreeMap::new(),
        db_generation_before: None,
        db_generation_after: None,
        index_generation_before: None,
        index_generation_after: None,
        runtime_budget: None,
        cancellation_status: "not_applicable".to_owned(),
        reproduction_command: render_reproduction_command(&cwd, &command, &argv),
        first_failure: None,
    })
}

#[test]
fn boundary_log_accepts_clean_real_binary_json_step() -> TestResult {
    let dir = unique_dossier_dir("boundary-logging-clean")?.join("status");
    let output = run_ee(&["--json", "status"])?;
    write_step_artifacts(&dir, &output)?;
    let record = make_record(&dir, &output, "ee.response.v1")?;
    write_boundary_log(&dir, &record)?;

    validate_boundary_log_extended(&dir.join("boundary-log.json"))
        .map_err(|failure| format!("boundary log should validate clean command: {failure:?}"))?;

    let log_content =
        fs::read_to_string(dir.join("boundary-log.json")).map_err(|error| error.to_string())?;
    let log: JsonValue = serde_json::from_str(&log_content).map_err(|error| error.to_string())?;
    ensure(
        log.get("schema") == Some(&JsonValue::String(BOUNDARY_LOG_SCHEMA.to_owned())),
        "boundary log must use ee.e2e.boundary_log.v1 schema",
    )?;
    ensure(
        observed_schema_from_stdout(&dir.join("stdout")) == "ee.response.v1",
        "observed stdout schema must be extracted for diagnostics",
    )
}

#[test]
fn boundary_log_rejects_missing_required_field_and_unredacted_env() -> TestResult {
    let missing_dir = unique_dossier_dir("boundary-logging-required")?.join("status");
    fs::create_dir_all(&missing_dir).map_err(|error| error.to_string())?;
    fs::write(
        missing_dir.join("stdout"),
        b"{\"schema\":\"ee.response.v1\",\"success\":true,\"data\":{}}\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(missing_dir.join("stderr"), b"").map_err(|error| error.to_string())?;
    let missing_log = json!({
        "schema": BOUNDARY_LOG_SCHEMA,
        "command": "ee"
    });
    let missing_text =
        serde_json::to_string_pretty(&missing_log).map_err(|error| error.to_string())?;
    fs::write(missing_dir.join("boundary-log.json"), missing_text)
        .map_err(|error| error.to_string())?;
    let missing = validate_boundary_log(&missing_dir.join("boundary-log.json"));
    let missing_failure = BoundaryLogFailure::MissingRequiredField("argv");
    ensure(
        missing == Err(missing_failure),
        format!("missing argv must be rejected first, got {missing:?}"),
    )?;
    ensure(
        first_failure_for_boundary_failure(&BoundaryLogFailure::MissingRequiredField("argv"))
            == "missing_required_field:argv",
        "missing-field first failure code must be stable",
    )?;

    let env_dir = unique_dossier_dir("boundary-logging-env")?.join("status");
    let output = run_ee(&["--json", "status"])?;
    write_step_artifacts(&env_dir, &output)?;
    let mut record = make_record(&env_dir, &output, "ee.response.v1")?;
    record.env_sanitized = json!({
        "EE_API_TOKEN": "raw-token",
        "SAFE_FLAG": "1"
    });
    write_boundary_log(&env_dir, &record)?;
    let env_result = validate_boundary_log_extended(&env_dir.join("boundary-log.json"));
    ensure(
        env_result
            == Err(BoundaryLogFailure::EnvNotRedacted {
                key: "EE_API_TOKEN".to_owned(),
            }),
        format!("raw sensitive env value must be rejected, got {env_result:?}"),
    )
}

#[test]
fn boundary_log_rejects_stdout_pollution_and_schema_mismatch() -> TestResult {
    let polluted_dir = unique_dossier_dir("boundary-logging-polluted")?.join("status");
    fs::create_dir_all(&polluted_dir).map_err(|error| error.to_string())?;
    let clean_output = run_ee(&["--json", "status"])?;
    fs::write(
        polluted_dir.join("stdout"),
        b"debug line on stdout\n{\"schema\":\"ee.response.v1\",\"success\":true,\"data\":{}}\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(polluted_dir.join("stderr"), b"").map_err(|error| error.to_string())?;
    let polluted_record = make_record(&polluted_dir, &clean_output, "ee.response.v1")?;
    write_boundary_log(&polluted_dir, &polluted_record)?;
    let pollution = validate_boundary_log(&polluted_dir.join("boundary-log.json"));
    ensure(
        pollution == Err(BoundaryLogFailure::StdoutPollution),
        format!("polluted stdout must be rejected, got {pollution:?}"),
    )?;

    let mismatch_dir = unique_dossier_dir("boundary-logging-schema")?.join("status");
    fs::create_dir_all(&mismatch_dir).map_err(|error| error.to_string())?;
    fs::write(
        mismatch_dir.join("stdout"),
        b"{\"schema\":\"ee.other.v1\",\"success\":true,\"data\":{}}\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(mismatch_dir.join("stderr"), b"").map_err(|error| error.to_string())?;
    let mismatch_record = make_record(&mismatch_dir, &clean_output, "ee.response.v1")?;
    write_boundary_log(&mismatch_dir, &mismatch_record)?;
    let mismatch = validate_boundary_log(&mismatch_dir.join("boundary-log.json"));
    let mismatch_failure = BoundaryLogFailure::SchemaMismatch {
        expected: "ee.response.v1".to_owned(),
        observed: "ee.other.v1".to_owned(),
    };
    ensure(
        mismatch == Err(mismatch_failure),
        format!("schema mismatch must be rejected, got {mismatch:?}"),
    )?;
    ensure(
        first_failure_for_boundary_failure(&BoundaryLogFailure::SchemaMismatch {
            expected: "ee.response.v1".to_owned(),
            observed: "ee.other.v1".to_owned(),
        }) == "schema_mismatch:ee.other.v1",
        "schema mismatch first failure code must be stable",
    )
}

#[test]
fn boundary_log_detects_unexpected_mutation() -> TestResult {
    let mutation_dir = unique_dossier_dir("boundary-logging-mutation")?.join("status");
    fs::create_dir_all(&mutation_dir).map_err(|error| error.to_string())?;
    let clean_output = run_ee(&["--json", "status"])?;
    fs::write(
        mutation_dir.join("stdout"),
        b"{\"schema\":\"ee.response.v1\",\"success\":true,\"data\":{}}\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(mutation_dir.join("stderr"), b"").map_err(|error| error.to_string())?;

    let started = unix_ms_now()?;
    let ended = unix_ms_now()?;
    let mutation_record = BoundaryLogRecord {
        command: "ee".to_owned(),
        argv: vec!["--json".to_owned(), "status".to_owned()],
        cwd: std::env::current_dir().map_err(|error| error.to_string())?,
        workspace: None,
        env_sanitized: json!({ "overrides": {}, "sensitive_env_omitted": true }),
        started_at_unix_ms: started,
        ended_at_unix_ms: ended,
        elapsed_ms: ended.saturating_sub(started),
        exit_code: clean_output.status.code(),
        stdout_artifact_path: mutation_dir.join("stdout"),
        stderr_artifact_path: mutation_dir.join("stderr"),
        expected_schema: "ee.response.v1".to_owned(),
        golden_path: None,
        golden_status: "not_applicable".to_owned(),
        redaction_status: "checked".to_owned(),
        evidence_ids: Vec::new(),
        degradation_codes: Vec::new(),
        mutation_summary: "read_only".to_owned(),
        command_boundary_matrix_row: Some("status".to_owned()),
        fixture_hashes: BTreeMap::new(),
        db_generation_before: Some(42),
        db_generation_after: Some(43),
        index_generation_before: Some(7),
        index_generation_after: Some(7),
        runtime_budget: None,
        cancellation_status: "not_applicable".to_owned(),
        reproduction_command: render_reproduction_command(
            &std::env::current_dir().map_err(|error| error.to_string())?,
            "ee",
            &["--json".to_owned(), "status".to_owned()],
        ),
        first_failure: None,
    };
    write_boundary_log(&mutation_dir, &mutation_record)?;
    let result = validate_boundary_log_extended(&mutation_dir.join("boundary-log.json"));
    ensure(
        result
            == Err(BoundaryLogFailure::UnexpectedMutation {
                before: 42,
                after: 43,
            }),
        format!("unexpected mutation must be rejected, got {result:?}"),
    )
}

fn validate_missing_matrix_row(
    log: &JsonValue,
    matrix_surfaces: &[&str],
) -> Option<BoundaryLogFailure> {
    let matrix_row = log
        .get("command_boundary_matrix_row")
        .and_then(JsonValue::as_str);
    match matrix_row {
        Some(surface) if !matrix_surfaces.contains(&surface) => {
            Some(BoundaryLogFailure::MissingMatrixRow {
                surface: surface.to_owned(),
            })
        }
        None => Some(BoundaryLogFailure::MissingMatrixRow {
            surface: "<null>".to_owned(),
        }),
        Some(_) => None,
    }
}

#[test]
fn boundary_log_detects_missing_matrix_row() -> TestResult {
    let matrix_dir = unique_dossier_dir("boundary-logging-matrix")?.join("status");
    fs::create_dir_all(&matrix_dir).map_err(|error| error.to_string())?;
    let clean_output = run_ee(&["--json", "status"])?;
    fs::write(
        matrix_dir.join("stdout"),
        b"{\"schema\":\"ee.response.v1\",\"success\":true,\"data\":{}}\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(matrix_dir.join("stderr"), b"").map_err(|error| error.to_string())?;

    let started = unix_ms_now()?;
    let ended = unix_ms_now()?;
    let matrix_record = BoundaryLogRecord {
        command: "ee".to_owned(),
        argv: vec!["--json".to_owned(), "fake-command".to_owned()],
        cwd: std::env::current_dir().map_err(|error| error.to_string())?,
        workspace: None,
        env_sanitized: json!({ "overrides": {}, "sensitive_env_omitted": true }),
        started_at_unix_ms: started,
        ended_at_unix_ms: ended,
        elapsed_ms: ended.saturating_sub(started),
        exit_code: clean_output.status.code(),
        stdout_artifact_path: matrix_dir.join("stdout"),
        stderr_artifact_path: matrix_dir.join("stderr"),
        expected_schema: "ee.response.v1".to_owned(),
        golden_path: None,
        golden_status: "not_applicable".to_owned(),
        redaction_status: "checked".to_owned(),
        evidence_ids: Vec::new(),
        degradation_codes: Vec::new(),
        mutation_summary: "read_only".to_owned(),
        command_boundary_matrix_row: Some("nonexistent_surface".to_owned()),
        fixture_hashes: BTreeMap::new(),
        db_generation_before: None,
        db_generation_after: None,
        index_generation_before: None,
        index_generation_after: None,
        runtime_budget: None,
        cancellation_status: "not_applicable".to_owned(),
        reproduction_command: render_reproduction_command(
            &std::env::current_dir().map_err(|error| error.to_string())?,
            "ee",
            &["--json".to_owned(), "fake-command".to_owned()],
        ),
        first_failure: None,
    };
    write_boundary_log(&matrix_dir, &matrix_record)?;

    let log_content = fs::read_to_string(matrix_dir.join("boundary-log.json"))
        .map_err(|error| error.to_string())?;
    let log: JsonValue = serde_json::from_str(&log_content).map_err(|error| error.to_string())?;
    let known_surfaces = ["status", "agent", "memory", "search", "init"];
    let result = validate_missing_matrix_row(&log, &known_surfaces);
    ensure(
        result
            == Some(BoundaryLogFailure::MissingMatrixRow {
                surface: "nonexistent_surface".to_owned(),
            }),
        format!("missing matrix row must be detected, got {result:?}"),
    )
}

fn validate_fixture_hashes(
    log: &JsonValue,
    required_fixtures: &[&str],
) -> Option<BoundaryLogFailure> {
    let fixture_hashes = log.get("fixture_hashes").and_then(JsonValue::as_object);
    for fixture_id in required_fixtures {
        let has_hash = fixture_hashes
            .and_then(|hashes| hashes.get(*fixture_id))
            .and_then(JsonValue::as_str)
            .is_some_and(|hash| !hash.is_empty());
        if !has_hash {
            return Some(BoundaryLogFailure::MissingFixtureHash {
                fixture_id: (*fixture_id).to_owned(),
            });
        }
    }
    None
}

#[test]
fn boundary_log_detects_missing_fixture_hash() -> TestResult {
    let fixture_dir = unique_dossier_dir("boundary-logging-fixture")?.join("status");
    fs::create_dir_all(&fixture_dir).map_err(|error| error.to_string())?;
    let clean_output = run_ee(&["--json", "status"])?;
    fs::write(
        fixture_dir.join("stdout"),
        b"{\"schema\":\"ee.response.v1\",\"success\":true,\"data\":{}}\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(fixture_dir.join("stderr"), b"").map_err(|error| error.to_string())?;

    let started = unix_ms_now()?;
    let ended = unix_ms_now()?;
    let fixture_record = BoundaryLogRecord {
        command: "ee".to_owned(),
        argv: vec!["--json".to_owned(), "eval".to_owned(), "run".to_owned()],
        cwd: std::env::current_dir().map_err(|error| error.to_string())?,
        workspace: None,
        env_sanitized: json!({ "overrides": {}, "sensitive_env_omitted": true }),
        started_at_unix_ms: started,
        ended_at_unix_ms: ended,
        elapsed_ms: ended.saturating_sub(started),
        exit_code: clean_output.status.code(),
        stdout_artifact_path: fixture_dir.join("stdout"),
        stderr_artifact_path: fixture_dir.join("stderr"),
        expected_schema: "ee.response.v1".to_owned(),
        golden_path: None,
        golden_status: "not_applicable".to_owned(),
        redaction_status: "checked".to_owned(),
        evidence_ids: vec!["eval.release_failure".to_owned()],
        degradation_codes: Vec::new(),
        mutation_summary: "read_only".to_owned(),
        command_boundary_matrix_row: Some("eval".to_owned()),
        fixture_hashes: BTreeMap::new(),
        db_generation_before: None,
        db_generation_after: None,
        index_generation_before: None,
        index_generation_after: None,
        runtime_budget: None,
        cancellation_status: "not_applicable".to_owned(),
        reproduction_command: render_reproduction_command(
            &std::env::current_dir().map_err(|error| error.to_string())?,
            "ee",
            &["--json".to_owned(), "eval".to_owned(), "run".to_owned()],
        ),
        first_failure: None,
    };
    write_boundary_log(&fixture_dir, &fixture_record)?;

    let log_content = fs::read_to_string(fixture_dir.join("boundary-log.json"))
        .map_err(|error| error.to_string())?;
    let log: JsonValue = serde_json::from_str(&log_content).map_err(|error| error.to_string())?;
    let required_fixtures = ["eval.release_failure"];
    let result = validate_fixture_hashes(&log, &required_fixtures);
    ensure(
        result
            == Some(BoundaryLogFailure::MissingFixtureHash {
                fixture_id: "eval.release_failure".to_owned(),
            }),
        format!("missing fixture hash must be detected, got {result:?}"),
    )
}

#[test]
fn boundary_log_renders_reproduction_command_and_summary_deterministically() -> TestResult {
    let root = unique_dossier_dir("boundary-logging-summary")?;
    let output = run_ee(&["--json", "status"])?;

    let status_dir = root.join("status");
    write_step_artifacts(&status_dir, &output)?;
    let status_record = make_record(&status_dir, &output, "ee.response.v1")?;
    write_boundary_log(&status_dir, &status_record)?;
    ensure(
        status_record
            .reproduction_command
            .contains("ee --json status"),
        format!(
            "reproduction command should include argv, got {}",
            status_record.reproduction_command
        ),
    )?;

    let agent_dir = root.join("agent-status");
    write_step_artifacts(&agent_dir, &output)?;
    let mut agent_record = make_record(&agent_dir, &output, "ee.response.v1")?;
    agent_record.argv = vec!["--json".to_owned(), "agent".to_owned(), "status".to_owned()];
    agent_record.command_boundary_matrix_row = Some("agent".to_owned());
    agent_record.reproduction_command =
        render_reproduction_command(&agent_record.cwd, &agent_record.command, &agent_record.argv);
    write_boundary_log(&agent_dir, &agent_record)?;

    let summary = build_boundary_log_summary(&[
        status_dir.join("boundary-log.json"),
        agent_dir.join("boundary-log.json"),
    ])?;
    ensure(
        summary.get("schema")
            == Some(&JsonValue::String(
                "ee.e2e.boundary_log.summary.v1".to_owned(),
            )),
        "summary schema must be stable",
    )?;
    ensure(
        summary.get("step_count") == Some(&json!(2)),
        "summary must count both steps",
    )?;
    let first_command = summary
        .pointer("/steps/0/argv/1")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("summary missing first sorted argv: {summary}"))?;
    ensure(
        first_command == "agent",
        format!("summary steps must sort deterministically by argv, got {summary}"),
    )
}
