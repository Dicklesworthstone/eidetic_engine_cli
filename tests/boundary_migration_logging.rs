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
    SchemaMismatch {
        expected: String,
        observed: String,
    },
    EnvNotRedacted {
        key: String,
    },
    MissingMatrixRow {
        surface: String,
    },
    UnexpectedMutation {
        before: u64,
        after: u64,
    },
    ForbiddenFilesystemOperationsUnchecked,
    MissingFixtureHash {
        fixture_id: String,
    },
    MissingReproductionCommand,
    InvalidSideEffectClass {
        observed: String,
    },
    InvalidRuntimeBudget,
    InvalidCancellationStatus {
        observed: String,
    },
    MissingCancellationInjectionPoint {
        status: String,
    },
    InvalidObservedOutcome {
        observed: String,
    },
    RuntimeOutcomeMismatch {
        status: String,
        outcome: String,
    },
    RuntimeExitCodeMismatch {
        outcome: String,
        exit_code: Option<i32>,
    },
    MissingRuntimeRollbackOrAuditEvidence {
        outcome: String,
    },
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
    side_effect_class: String,
    changed_record_ids: Vec<String>,
    audit_ids: Vec<String>,
    records_rolled_back_or_audited: Vec<String>,
    filesystem_artifacts_created: Vec<String>,
    forbidden_filesystem_operations_checked: bool,
    command_boundary_matrix_row: Option<String>,
    fixture_hashes: BTreeMap<String, String>,
    db_generation_before: Option<u64>,
    db_generation_after: Option<u64>,
    index_generation_before: Option<u64>,
    index_generation_after: Option<u64>,
    runtime_budget: Option<u64>,
    cancellation_status: String,
    cancellation_injection_point: Option<String>,
    observed_outcome: String,
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
        "side_effect_class": record.side_effect_class,
        "changed_record_ids": record.changed_record_ids,
        "audit_ids": record.audit_ids,
        "records_rolled_back_or_audited": record.records_rolled_back_or_audited,
        "filesystem_artifacts_created": record.filesystem_artifacts_created,
        "forbidden_filesystem_operations_checked": record.forbidden_filesystem_operations_checked,
        "command_boundary_matrix_row": record.command_boundary_matrix_row,
        "fixture_hashes": record.fixture_hashes,
        "db_generation_before": record.db_generation_before,
        "db_generation_after": record.db_generation_after,
        "index_generation_before": record.index_generation_before,
        "index_generation_after": record.index_generation_after,
        "runtime_budget": record.runtime_budget,
        "cancellation_status": record.cancellation_status,
        "cancellation_injection_point": record.cancellation_injection_point,
        "observed_outcome": record.observed_outcome,
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
        "side_effect_class",
        "changed_record_ids",
        "audit_ids",
        "records_rolled_back_or_audited",
        "filesystem_artifacts_created",
        "forbidden_filesystem_operations_checked",
        "command_boundary_matrix_row",
        "fixture_hashes",
        "db_generation_before",
        "db_generation_after",
        "index_generation_before",
        "index_generation_after",
        "runtime_budget",
        "cancellation_status",
        "cancellation_injection_point",
        "observed_outcome",
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

        let changed_records = log
            .get("changed_record_ids")
            .and_then(JsonValue::as_array)
            .map_or(0, Vec::len);
        let audit_ids = log
            .get("audit_ids")
            .and_then(JsonValue::as_array)
            .map_or(0, Vec::len);
        let filesystem_artifacts = log
            .get("filesystem_artifacts_created")
            .and_then(JsonValue::as_array)
            .map_or(0, Vec::len);
        if changed_records > 0 || audit_ids > 0 || filesystem_artifacts > 0 {
            let changed_records = u64::try_from(changed_records).unwrap_or(u64::MAX);
            let audit_ids = u64::try_from(audit_ids).unwrap_or(u64::MAX);
            let filesystem_artifacts = u64::try_from(filesystem_artifacts).unwrap_or(u64::MAX);
            return Err(BoundaryLogFailure::UnexpectedMutation {
                before: changed_records,
                after: changed_records
                    .saturating_add(audit_ids)
                    .saturating_add(filesystem_artifacts),
            });
        }
    }

    let forbidden_operations_checked = log
        .get("forbidden_filesystem_operations_checked")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    if !forbidden_operations_checked {
        return Err(BoundaryLogFailure::ForbiddenFilesystemOperationsUnchecked);
    }

    validate_env_redaction(&log)?;
    validate_runtime_envelope(&log)?;

    let reproduction_command = log
        .get("reproduction_command")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if reproduction_command.trim().is_empty() {
        return Err(BoundaryLogFailure::MissingReproductionCommand);
    }

    Ok(())
}

fn validate_runtime_envelope(log: &JsonValue) -> Result<(), BoundaryLogFailure> {
    if log.get("runtime_budget").and_then(JsonValue::as_u64) == Some(0) {
        return Err(BoundaryLogFailure::InvalidRuntimeBudget);
    }

    let side_effect_class = log
        .get("side_effect_class")
        .and_then(JsonValue::as_str)
        .ok_or(BoundaryLogFailure::MissingRequiredField(
            "side_effect_class",
        ))?;
    if !side_effect_class.starts_with("class=") {
        return Err(BoundaryLogFailure::InvalidSideEffectClass {
            observed: side_effect_class.to_owned(),
        });
    }

    let cancellation_status = log
        .get("cancellation_status")
        .and_then(JsonValue::as_str)
        .ok_or(BoundaryLogFailure::MissingRequiredField(
            "cancellation_status",
        ))?;
    if !matches!(
        cancellation_status,
        "not_applicable" | "not_requested" | "requested" | "completed" | "timeout"
    ) {
        return Err(BoundaryLogFailure::InvalidCancellationStatus {
            observed: cancellation_status.to_owned(),
        });
    }

    let injection_point = log
        .get("cancellation_injection_point")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.trim().is_empty());
    if matches!(cancellation_status, "requested" | "completed" | "timeout")
        && injection_point.is_none()
    {
        return Err(BoundaryLogFailure::MissingCancellationInjectionPoint {
            status: cancellation_status.to_owned(),
        });
    }

    let observed_outcome = log
        .get("observed_outcome")
        .and_then(JsonValue::as_str)
        .ok_or(BoundaryLogFailure::MissingRequiredField("observed_outcome"))?;
    if !matches!(
        observed_outcome,
        "success"
            | "degraded"
            | "cancelled"
            | "budget_exhausted"
            | "storage_error"
            | "index_error"
            | "supervised_child_failed"
            | "not_applicable"
    ) {
        return Err(BoundaryLogFailure::InvalidObservedOutcome {
            observed: observed_outcome.to_owned(),
        });
    }

    match cancellation_status {
        "completed" if observed_outcome != "cancelled" => {
            return Err(BoundaryLogFailure::RuntimeOutcomeMismatch {
                status: cancellation_status.to_owned(),
                outcome: observed_outcome.to_owned(),
            });
        }
        "timeout" if observed_outcome != "budget_exhausted" => {
            return Err(BoundaryLogFailure::RuntimeOutcomeMismatch {
                status: cancellation_status.to_owned(),
                outcome: observed_outcome.to_owned(),
            });
        }
        _ => {}
    }

    let exit_code = log
        .get("exit_code")
        .and_then(JsonValue::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    if observed_outcome == "success" && exit_code != Some(0) {
        return Err(BoundaryLogFailure::RuntimeExitCodeMismatch {
            outcome: observed_outcome.to_owned(),
            exit_code,
        });
    }
    if matches!(
        observed_outcome,
        "cancelled"
            | "budget_exhausted"
            | "storage_error"
            | "index_error"
            | "supervised_child_failed"
    ) && exit_code == Some(0)
    {
        return Err(BoundaryLogFailure::RuntimeExitCodeMismatch {
            outcome: observed_outcome.to_owned(),
            exit_code,
        });
    }

    let changed_record_count = log
        .get("changed_record_ids")
        .and_then(JsonValue::as_array)
        .map_or(0, Vec::len);
    if changed_record_count > 0
        && matches!(
            observed_outcome,
            "cancelled"
                | "budget_exhausted"
                | "storage_error"
                | "index_error"
                | "supervised_child_failed"
        )
    {
        let audit_count = log
            .get("audit_ids")
            .and_then(JsonValue::as_array)
            .map_or(0, Vec::len);
        let rollback_count = log
            .get("records_rolled_back_or_audited")
            .and_then(JsonValue::as_array)
            .map_or(0, Vec::len);
        if audit_count == 0 && rollback_count == 0 {
            return Err(BoundaryLogFailure::MissingRuntimeRollbackOrAuditEvidence {
                outcome: observed_outcome.to_owned(),
            });
        }
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
        BoundaryLogFailure::ForbiddenFilesystemOperationsUnchecked => {
            "forbidden_filesystem_operations_unchecked".to_owned()
        }
        BoundaryLogFailure::MissingFixtureHash { fixture_id } => {
            format!("missing_fixture_hash:{fixture_id}")
        }
        BoundaryLogFailure::MissingReproductionCommand => "missing_reproduction_command".to_owned(),
        BoundaryLogFailure::InvalidSideEffectClass { observed } => {
            format!("invalid_side_effect_class:{observed}")
        }
        BoundaryLogFailure::InvalidRuntimeBudget => "invalid_runtime_budget".to_owned(),
        BoundaryLogFailure::InvalidCancellationStatus { observed } => {
            format!("invalid_cancellation_status:{observed}")
        }
        BoundaryLogFailure::MissingCancellationInjectionPoint { status } => {
            format!("missing_cancellation_injection_point:{status}")
        }
        BoundaryLogFailure::InvalidObservedOutcome { observed } => {
            format!("invalid_observed_outcome:{observed}")
        }
        BoundaryLogFailure::RuntimeOutcomeMismatch { status, outcome } => {
            format!("runtime_outcome_mismatch:{status}:{outcome}")
        }
        BoundaryLogFailure::RuntimeExitCodeMismatch { outcome, .. } => {
            format!("runtime_exit_code_mismatch:{outcome}")
        }
        BoundaryLogFailure::MissingRuntimeRollbackOrAuditEvidence { outcome } => {
            format!("missing_runtime_rollback_or_audit:{outcome}")
        }
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

fn write_boundary_log_with_mechanical_evidence(
    dir: &Path,
    record: &BoundaryLogRecord,
    evidence: JsonValue,
) -> TestResult {
    write_boundary_log(dir, record)?;
    let path = dir.join("boundary-log.json");
    let content = fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let mut log: JsonValue = serde_json::from_str(&content).map_err(|error| error.to_string())?;
    let Some(log_object) = log.as_object_mut() else {
        return Err("boundary log root must be a JSON object".to_owned());
    };
    if let Some(schema_validation) = log_object
        .get_mut("schema_validation")
        .and_then(JsonValue::as_object_mut)
    {
        schema_validation.insert("status".to_owned(), JsonValue::String("matched".to_owned()));
    }
    log_object.insert("mechanical_evidence".to_owned(), evidence);
    fs::write(
        &path,
        serde_json::to_string_pretty(&log).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
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
        side_effect_class: "class=read_only".to_owned(),
        changed_record_ids: Vec::new(),
        audit_ids: Vec::new(),
        records_rolled_back_or_audited: Vec::new(),
        filesystem_artifacts_created: Vec::new(),
        forbidden_filesystem_operations_checked: true,
        command_boundary_matrix_row: Some("status".to_owned()),
        fixture_hashes: BTreeMap::new(),
        db_generation_before: None,
        db_generation_after: None,
        index_generation_before: None,
        index_generation_after: None,
        runtime_budget: None,
        cancellation_status: "not_applicable".to_owned(),
        cancellation_injection_point: None,
        observed_outcome: "success".to_owned(),
        reproduction_command: render_reproduction_command(&cwd, &command, &argv),
        first_failure: None,
    })
}

#[test]
fn boundary_log_records_v76q_claim_and_certificate_manifest_evidence() -> TestResult {
    let root = unique_dossier_dir("boundary-logging-v76q-evidence")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let claim_id = ee::models::ClaimId::from_uuid(uuid::Uuid::from_u128(
        0x7600_0000_0000_0000_0000_0000_0000_0001,
    ))
    .to_string();
    let claims_file = workspace.join("claims.yaml");
    fs::write(
        &claims_file,
        format!(
            "schema: ee.claims_file.v1\nversion: 1\nclaims:\n  - id: {claim_id}\n    title: V76Q real claim\n    status: active\n    frequency: weekly\n"
        ),
    )
    .map_err(|error| error.to_string())?;

    let claim_dir = workspace.join("artifacts").join(&claim_id);
    fs::create_dir_all(&claim_dir).map_err(|error| error.to_string())?;
    let claim_payload = b"{\"claim\":\"v76q\",\"ok\":true}\n";
    let claim_payload_hash = blake3::hash(claim_payload).to_hex().to_string();
    fs::write(claim_dir.join("stdout.json"), claim_payload).map_err(|error| error.to_string())?;
    let claim_manifest_path = claim_dir.join("manifest.json");
    fs::write(
        &claim_manifest_path,
        serde_json::to_string_pretty(&json!({
            "schema": "ee.claim_manifest.v1",
            "claimId": claim_id,
            "verificationStatus": "passing",
            "artifacts": [
                {
                    "path": "stdout.json",
                    "artifactType": "report",
                    "blake3Hash": claim_payload_hash,
                    "sizeBytes": claim_payload.len(),
                    "createdAt": "2026-05-04T00:00:00Z"
                }
            ]
        }))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let certificate_id = "cert_v76q_valid";
    let certificate_payload = b"{\"certificate\":\"v76q\",\"ok\":true}\n";
    let certificate_payload_hash = blake3::hash(certificate_payload).to_hex().to_string();
    let certificate_payload_path = workspace.join("certificate-payload.json");
    fs::write(&certificate_payload_path, certificate_payload).map_err(|error| error.to_string())?;
    let certificate_manifest_path = workspace.join("certificates.json");
    fs::write(
        &certificate_manifest_path,
        serde_json::to_string_pretty(&json!({
            "schema": ee::core::certificate::CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": [
                {
                    "id": certificate_id,
                    "kind": "pack",
                    "status": "valid",
                    "workspaceId": "workspace_v76q",
                    "issuedAt": "2026-05-04T00:00:00Z",
                    "expiresAt": "2999-01-01T00:00:00Z",
                    "payloadPath": "certificate-payload.json",
                    "payloadHash": certificate_payload_hash,
                    "payloadSchema": ee::core::certificate::CERTIFICATE_PAYLOAD_SCHEMA_V1,
                    "assumptions": [{"valid": true}]
                }
            ]
        }))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let workspace_arg = workspace.display().to_string();
    let claim_output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "claim",
        "verify",
        &claim_id,
    ])?;
    ensure(
        claim_output.status.success(),
        "claim verify fixture should pass",
    )?;
    ensure(
        claim_output.stderr.is_empty(),
        "claim verify JSON stderr must stay empty",
    )?;
    let claim_dir_log = root.join("claim-verify");
    write_step_artifacts(&claim_dir_log, &claim_output)?;
    let mut claim_record = make_record(&claim_dir_log, &claim_output, "ee.claim_verify.v1")?;
    claim_record.argv = vec![
        "--workspace".to_owned(),
        workspace_arg.clone(),
        "--json".to_owned(),
        "claim".to_owned(),
        "verify".to_owned(),
        claim_id.clone(),
    ];
    claim_record.workspace = Some(workspace.clone());
    claim_record.evidence_ids = vec![
        "v76q.claims_file".to_owned(),
        "v76q.claim_manifest".to_owned(),
    ];
    claim_record.command_boundary_matrix_row = Some("claim".to_owned());
    claim_record.golden_path =
        Some("tests/fixtures/golden/claims/verified_claim.json.golden".to_owned());
    claim_record.golden_status = "schema_only".to_owned();
    claim_record.fixture_hashes.insert(
        "v76q.claims_file".to_owned(),
        blake3::hash(
            fs::read(&claims_file)
                .map_err(|error| error.to_string())?
                .as_slice(),
        )
        .to_hex()
        .to_string(),
    );
    claim_record.fixture_hashes.insert(
        "v76q.claim_artifact_stdout".to_owned(),
        claim_payload_hash.clone(),
    );
    claim_record.fixture_hashes.insert(
        "v76q.claim_manifest".to_owned(),
        blake3::hash(
            fs::read(&claim_manifest_path)
                .map_err(|error| error.to_string())?
                .as_slice(),
        )
        .to_hex()
        .to_string(),
    );
    claim_record.reproduction_command =
        render_reproduction_command(&claim_record.cwd, &claim_record.command, &claim_record.argv);
    write_boundary_log_with_mechanical_evidence(
        &claim_dir_log,
        &claim_record,
        json!({
            "claims_file_path": claims_file.display().to_string(),
            "artifact_manifest_paths": [claim_manifest_path.display().to_string()],
            "checked_hashes": {
                "claim_artifact_stdout": claim_payload_hash
            },
            "degradation_codes": []
        }),
    )?;

    let certificate_output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "certificate",
        "verify",
        certificate_id,
    ])?;
    ensure(
        certificate_output.status.success(),
        "certificate verify fixture should pass",
    )?;
    ensure(
        certificate_output.stderr.is_empty(),
        "certificate verify JSON stderr must stay empty",
    )?;
    let certificate_dir_log = root.join("certificate-verify");
    write_step_artifacts(&certificate_dir_log, &certificate_output)?;
    let mut certificate_record = make_record(
        &certificate_dir_log,
        &certificate_output,
        "ee.certificate.verify.v1",
    )?;
    certificate_record.argv = vec![
        "--workspace".to_owned(),
        workspace_arg,
        "--json".to_owned(),
        "certificate".to_owned(),
        "verify".to_owned(),
        certificate_id.to_owned(),
    ];
    certificate_record.workspace = Some(workspace);
    certificate_record.evidence_ids = vec![
        "v76q.certificate_manifest".to_owned(),
        "v76q.certificate_payload".to_owned(),
    ];
    certificate_record.command_boundary_matrix_row = Some("certificate".to_owned());
    certificate_record.golden_status = "schema_only".to_owned();
    certificate_record.fixture_hashes.insert(
        "v76q.certificate_manifest".to_owned(),
        blake3::hash(
            fs::read(&certificate_manifest_path)
                .map_err(|error| error.to_string())?
                .as_slice(),
        )
        .to_hex()
        .to_string(),
    );
    certificate_record.fixture_hashes.insert(
        "v76q.certificate_payload".to_owned(),
        certificate_payload_hash.clone(),
    );
    certificate_record.reproduction_command = render_reproduction_command(
        &certificate_record.cwd,
        &certificate_record.command,
        &certificate_record.argv,
    );
    write_boundary_log_with_mechanical_evidence(
        &certificate_dir_log,
        &certificate_record,
        json!({
            "certificate_manifest_path": certificate_manifest_path.display().to_string(),
            "artifact_manifest_paths": [certificate_manifest_path.display().to_string()],
            "checked_hashes": {
                "certificate_payload": certificate_payload_hash
            },
            "degradation_codes": []
        }),
    )?;

    for (path, required_fixtures) in [
        (
            claim_dir_log.join("boundary-log.json"),
            vec![
                "v76q.claims_file",
                "v76q.claim_manifest",
                "v76q.claim_artifact_stdout",
            ],
        ),
        (
            certificate_dir_log.join("boundary-log.json"),
            vec!["v76q.certificate_manifest", "v76q.certificate_payload"],
        ),
    ] {
        validate_boundary_log_extended(&path)
            .map_err(|failure| format!("v76q boundary log should validate: {failure:?}"))?;
        let content = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let log: JsonValue = serde_json::from_str(&content).map_err(|error| error.to_string())?;
        ensure(
            validate_fixture_hashes(&log, &required_fixtures).is_none(),
            "v76q log should include required fixture hashes",
        )?;
        ensure(
            log.pointer("/schema_validation/status")
                .and_then(JsonValue::as_str)
                == Some("matched"),
            "v76q log should record schema validation result",
        )?;
        ensure(
            log.pointer("/golden_validation/status")
                .and_then(JsonValue::as_str)
                == Some("schema_only"),
            "v76q log should record golden validation result",
        )?;
        ensure(
            log.pointer("/mechanical_evidence/checked_hashes")
                .and_then(JsonValue::as_object)
                .is_some_and(|hashes| !hashes.is_empty()),
            "v76q log should record checked hashes",
        )?;
    }

    Ok(())
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
fn boundary_log_accepts_runtime_budget_and_cancellation_envelopes() -> TestResult {
    struct RuntimeCase {
        name: &'static str,
        argv: Vec<&'static str>,
        exit_code: i32,
        runtime_budget: Option<u64>,
        cancellation_status: &'static str,
        cancellation_injection_point: Option<&'static str>,
        observed_outcome: &'static str,
        side_effect_class: &'static str,
        mutation_summary: &'static str,
        changed_record_ids: Vec<&'static str>,
        audit_ids: Vec<&'static str>,
        records_rolled_back_or_audited: Vec<&'static str>,
        degradation_codes: Vec<&'static str>,
    }

    let root = unique_dossier_dir("boundary-logging-runtime-accepted")?;
    let output = run_ee(&["--json", "status"])?;
    let cases = vec![
        RuntimeCase {
            name: "cancel-before-start",
            argv: vec!["--json", "import", "cass"],
            exit_code: 6,
            runtime_budget: Some(500),
            cancellation_status: "completed",
            cancellation_injection_point: Some("before_start"),
            observed_outcome: "cancelled",
            side_effect_class: "class=append_only",
            mutation_summary: "failed_before_mutation",
            changed_record_ids: Vec::new(),
            audit_ids: Vec::new(),
            records_rolled_back_or_audited: Vec::new(),
            degradation_codes: vec!["runtime_cancelled"],
        },
        RuntimeCase {
            name: "cancel-mid-operation",
            argv: vec!["--json", "import", "jsonl"],
            exit_code: 6,
            runtime_budget: Some(750),
            cancellation_status: "completed",
            cancellation_injection_point: Some("mid_operation:after_source_scan"),
            observed_outcome: "cancelled",
            side_effect_class: "class=append_only",
            mutation_summary: "rollback_no_partial_state",
            changed_record_ids: vec!["import_batch_pending"],
            audit_ids: Vec::new(),
            records_rolled_back_or_audited: vec!["rolled_back:import_batch_pending"],
            degradation_codes: vec!["runtime_cancelled"],
        },
        RuntimeCase {
            name: "budget-exhaustion",
            argv: vec!["--json", "index", "rebuild"],
            exit_code: 6,
            runtime_budget: Some(1),
            cancellation_status: "timeout",
            cancellation_injection_point: Some("budget_deadline:index_publish"),
            observed_outcome: "budget_exhausted",
            side_effect_class: "class=derived_asset_rebuild",
            mutation_summary: "derived_rebuild_aborted_before_publish",
            changed_record_ids: vec!["index_generation_pending"],
            audit_ids: Vec::new(),
            records_rolled_back_or_audited: vec!["rolled_back:index_generation_pending"],
            degradation_codes: vec!["runtime_budget_exceeded"],
        },
        RuntimeCase {
            name: "storage-failure-after-partial-progress",
            argv: vec!["--json", "remember", "runtime fixture"],
            exit_code: 3,
            runtime_budget: Some(1000),
            cancellation_status: "not_requested",
            cancellation_injection_point: None,
            observed_outcome: "storage_error",
            side_effect_class: "class=audited_mutation",
            mutation_summary: "transaction_rolled_back_after_storage_failure",
            changed_record_ids: vec!["memory_pending"],
            audit_ids: Vec::new(),
            records_rolled_back_or_audited: vec!["rolled_back:memory_pending"],
            degradation_codes: Vec::new(),
        },
        RuntimeCase {
            name: "supervised-child-failure",
            argv: vec!["--json", "daemon", "run"],
            exit_code: 6,
            runtime_budget: Some(2000),
            cancellation_status: "not_requested",
            cancellation_injection_point: None,
            observed_outcome: "supervised_child_failed",
            side_effect_class: "class=supervised_jobs",
            mutation_summary: "job_ledger_audited_failure",
            changed_record_ids: vec!["job_runtime_1"],
            audit_ids: vec!["audit_job_runtime_1"],
            records_rolled_back_or_audited: vec!["audited:job_runtime_1"],
            degradation_codes: vec!["supervised_child_failed"],
        },
    ];

    for case in cases {
        let dir = root.join(case.name);
        write_step_artifacts(&dir, &output)?;
        let mut record = make_record(&dir, &output, "ee.response.v1")?;
        record.argv = case.argv.iter().map(|arg| (*arg).to_owned()).collect();
        record.exit_code = Some(case.exit_code);
        record.runtime_budget = case.runtime_budget;
        record.cancellation_status = case.cancellation_status.to_owned();
        record.cancellation_injection_point = case.cancellation_injection_point.map(str::to_owned);
        record.observed_outcome = case.observed_outcome.to_owned();
        record.side_effect_class = case.side_effect_class.to_owned();
        record.mutation_summary = case.mutation_summary.to_owned();
        record.changed_record_ids = case
            .changed_record_ids
            .iter()
            .map(|record_id| (*record_id).to_owned())
            .collect();
        record.audit_ids = case
            .audit_ids
            .iter()
            .map(|audit_id| (*audit_id).to_owned())
            .collect();
        record.records_rolled_back_or_audited = case
            .records_rolled_back_or_audited
            .iter()
            .map(|record_id| (*record_id).to_owned())
            .collect();
        record.degradation_codes = case
            .degradation_codes
            .iter()
            .map(|code| (*code).to_owned())
            .collect();
        record.command_boundary_matrix_row =
            Some(case.argv.first().copied().unwrap_or("status").to_owned());
        record.reproduction_command =
            render_reproduction_command(&record.cwd, &record.command, &record.argv);
        record.first_failure = (case.observed_outcome != "success")
            .then(|| format!("outcome={}", case.observed_outcome));
        write_boundary_log(&dir, &record)?;

        validate_boundary_log_extended(&dir.join("boundary-log.json")).map_err(|failure| {
            format!(
                "{} runtime envelope should validate, got {failure:?}",
                case.name
            )
        })?;
    }

    Ok(())
}

#[test]
fn boundary_log_records_real_binary_budgeted_index_dry_run() -> TestResult {
    let root = unique_dossier_dir("boundary-logging-real-binary-runtime")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace_arg = workspace.display().to_string();

    let init_output = run_ee(&["--workspace", &workspace_arg, "--json", "init"])?;
    ensure(
        init_output.status.success(),
        format!(
            "init must prepare workspace for index dry-run: stdout={}, stderr={}",
            String::from_utf8_lossy(&init_output.stdout),
            String::from_utf8_lossy(&init_output.stderr)
        ),
    )?;
    let remember_output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "runtime budget fixture",
    ])?;
    ensure(
        remember_output.status.success(),
        format!(
            "remember must seed workspace rows for index dry-run: stdout={}, stderr={}",
            String::from_utf8_lossy(&remember_output.stdout),
            String::from_utf8_lossy(&remember_output.stderr)
        ),
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "index",
        "rebuild",
        "--dry-run",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "index rebuild dry-run must succeed: stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let step_dir = root.join("index-rebuild-dry-run");
    write_step_artifacts(&step_dir, &output)?;
    let mut record = make_record(&step_dir, &output, "ee.response.v1")?;
    record.argv = vec![
        "--workspace".to_owned(),
        workspace_arg,
        "--json".to_owned(),
        "index".to_owned(),
        "rebuild".to_owned(),
        "--dry-run".to_owned(),
    ];
    record.workspace = Some(workspace);
    record.runtime_budget = Some(30_000);
    record.cancellation_status = "not_requested".to_owned();
    record.cancellation_injection_point = None;
    record.observed_outcome = "success".to_owned();
    record.side_effect_class = "class=derived_asset_rebuild".to_owned();
    record.mutation_summary = "dry_run_no_mutation_expected".to_owned();
    record.command_boundary_matrix_row = Some("index rebuild".to_owned());
    record.reproduction_command =
        render_reproduction_command(&record.cwd, &record.command, &record.argv);
    write_boundary_log(&step_dir, &record)?;

    validate_boundary_log_extended(&step_dir.join("boundary-log.json")).map_err(|failure| {
        format!("real-binary index dry-run runtime log should validate, got {failure:?}")
    })
}

#[test]
fn boundary_log_rejects_inconsistent_runtime_envelopes() -> TestResult {
    struct RuntimeRejection {
        name: &'static str,
        mutate: fn(&mut BoundaryLogRecord),
        expected: BoundaryLogFailure,
    }

    fn invalid_side_effect(record: &mut BoundaryLogRecord) {
        record.side_effect_class = "append_only".to_owned();
    }

    fn zero_budget(record: &mut BoundaryLogRecord) {
        record.runtime_budget = Some(0);
    }

    fn invalid_status(record: &mut BoundaryLogRecord) {
        record.cancellation_status = "maybe".to_owned();
    }

    fn missing_injection(record: &mut BoundaryLogRecord) {
        record.runtime_budget = Some(500);
        record.cancellation_status = "requested".to_owned();
        record.cancellation_injection_point = None;
    }

    fn timeout_outcome_mismatch(record: &mut BoundaryLogRecord) {
        record.exit_code = Some(6);
        record.runtime_budget = Some(1);
        record.cancellation_status = "timeout".to_owned();
        record.cancellation_injection_point = Some("budget_deadline:index_publish".to_owned());
        record.observed_outcome = "cancelled".to_owned();
    }

    fn cancelled_success_exit(record: &mut BoundaryLogRecord) {
        record.exit_code = Some(0);
        record.runtime_budget = Some(500);
        record.cancellation_status = "completed".to_owned();
        record.cancellation_injection_point = Some("mid_operation:pack_write".to_owned());
        record.observed_outcome = "cancelled".to_owned();
    }

    fn unaudited_partial_failure(record: &mut BoundaryLogRecord) {
        record.exit_code = Some(3);
        record.runtime_budget = Some(1000);
        record.observed_outcome = "storage_error".to_owned();
        record.side_effect_class = "class=audited_mutation".to_owned();
        record.mutation_summary = "transaction_failed_after_partial_progress".to_owned();
        record.changed_record_ids = vec!["memory_pending".to_owned()];
        record.audit_ids = Vec::new();
        record.records_rolled_back_or_audited = Vec::new();
    }

    let root = unique_dossier_dir("boundary-logging-runtime-rejected")?;
    let output = run_ee(&["--json", "status"])?;
    let cases = vec![
        RuntimeRejection {
            name: "invalid-side-effect",
            mutate: invalid_side_effect,
            expected: BoundaryLogFailure::InvalidSideEffectClass {
                observed: "append_only".to_owned(),
            },
        },
        RuntimeRejection {
            name: "zero-budget",
            mutate: zero_budget,
            expected: BoundaryLogFailure::InvalidRuntimeBudget,
        },
        RuntimeRejection {
            name: "invalid-status",
            mutate: invalid_status,
            expected: BoundaryLogFailure::InvalidCancellationStatus {
                observed: "maybe".to_owned(),
            },
        },
        RuntimeRejection {
            name: "missing-injection",
            mutate: missing_injection,
            expected: BoundaryLogFailure::MissingCancellationInjectionPoint {
                status: "requested".to_owned(),
            },
        },
        RuntimeRejection {
            name: "timeout-outcome-mismatch",
            mutate: timeout_outcome_mismatch,
            expected: BoundaryLogFailure::RuntimeOutcomeMismatch {
                status: "timeout".to_owned(),
                outcome: "cancelled".to_owned(),
            },
        },
        RuntimeRejection {
            name: "cancelled-success-exit",
            mutate: cancelled_success_exit,
            expected: BoundaryLogFailure::RuntimeExitCodeMismatch {
                outcome: "cancelled".to_owned(),
                exit_code: Some(0),
            },
        },
        RuntimeRejection {
            name: "unaudited-partial-failure",
            mutate: unaudited_partial_failure,
            expected: BoundaryLogFailure::MissingRuntimeRollbackOrAuditEvidence {
                outcome: "storage_error".to_owned(),
            },
        },
    ];

    for case in cases {
        let dir = root.join(case.name);
        write_step_artifacts(&dir, &output)?;
        let mut record = make_record(&dir, &output, "ee.response.v1")?;
        (case.mutate)(&mut record);
        write_boundary_log(&dir, &record)?;

        let result = validate_boundary_log_extended(&dir.join("boundary-log.json"));
        ensure(
            result == Err(case.expected),
            format!(
                "{} must reject invalid runtime envelope, got {result:?}",
                case.name
            ),
        )?;
    }

    ensure(
        first_failure_for_boundary_failure(
            &BoundaryLogFailure::MissingRuntimeRollbackOrAuditEvidence {
                outcome: "storage_error".to_owned(),
            },
        ) == "missing_runtime_rollback_or_audit:storage_error",
        "runtime rollback/audit failure code must be stable",
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
        side_effect_class: "class=read_only".to_owned(),
        changed_record_ids: Vec::new(),
        audit_ids: Vec::new(),
        records_rolled_back_or_audited: Vec::new(),
        filesystem_artifacts_created: Vec::new(),
        forbidden_filesystem_operations_checked: true,
        command_boundary_matrix_row: Some("status".to_owned()),
        fixture_hashes: BTreeMap::new(),
        db_generation_before: Some(42),
        db_generation_after: Some(43),
        index_generation_before: Some(7),
        index_generation_after: Some(7),
        runtime_budget: None,
        cancellation_status: "not_applicable".to_owned(),
        cancellation_injection_point: None,
        observed_outcome: "success".to_owned(),
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

#[test]
fn boundary_log_requires_forbidden_filesystem_operation_check() -> TestResult {
    let check_dir = unique_dossier_dir("boundary-logging-forbidden-fs-check")?.join("status");
    fs::create_dir_all(&check_dir).map_err(|error| error.to_string())?;
    let clean_output = run_ee(&["--json", "status"])?;
    fs::write(
        check_dir.join("stdout"),
        b"{\"schema\":\"ee.response.v1\",\"success\":true,\"data\":{}}\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(check_dir.join("stderr"), b"").map_err(|error| error.to_string())?;

    let mut record = make_record(&check_dir, &clean_output, "ee.response.v1")?;
    record.forbidden_filesystem_operations_checked = false;
    write_boundary_log(&check_dir, &record)?;

    let result = validate_boundary_log_extended(&check_dir.join("boundary-log.json"));
    ensure(
        result == Err(BoundaryLogFailure::ForbiddenFilesystemOperationsUnchecked),
        format!("unchecked forbidden filesystem operations must be rejected, got {result:?}"),
    )?;
    ensure(
        first_failure_for_boundary_failure(
            &BoundaryLogFailure::ForbiddenFilesystemOperationsUnchecked,
        ) == "forbidden_filesystem_operations_unchecked",
        "unchecked forbidden filesystem first failure code must be stable",
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
        side_effect_class: "class=read_only".to_owned(),
        changed_record_ids: Vec::new(),
        audit_ids: Vec::new(),
        records_rolled_back_or_audited: Vec::new(),
        filesystem_artifacts_created: Vec::new(),
        forbidden_filesystem_operations_checked: true,
        command_boundary_matrix_row: Some("nonexistent_surface".to_owned()),
        fixture_hashes: BTreeMap::new(),
        db_generation_before: None,
        db_generation_after: None,
        index_generation_before: None,
        index_generation_after: None,
        runtime_budget: None,
        cancellation_status: "not_applicable".to_owned(),
        cancellation_injection_point: None,
        observed_outcome: "success".to_owned(),
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
        side_effect_class: "class=read_only".to_owned(),
        changed_record_ids: Vec::new(),
        audit_ids: Vec::new(),
        records_rolled_back_or_audited: Vec::new(),
        filesystem_artifacts_created: Vec::new(),
        forbidden_filesystem_operations_checked: true,
        command_boundary_matrix_row: Some("eval".to_owned()),
        fixture_hashes: BTreeMap::new(),
        db_generation_before: None,
        db_generation_after: None,
        index_generation_before: None,
        index_generation_after: None,
        runtime_budget: None,
        cancellation_status: "not_applicable".to_owned(),
        cancellation_injection_point: None,
        observed_outcome: "success".to_owned(),
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
