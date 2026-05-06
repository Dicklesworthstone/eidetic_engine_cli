use std::process::{Command, Output};

use chrono::DateTime;
use ee::obs::{LOG_ENVELOPE_SCHEMA_V1, LogEnvelope};
use serde_json::Value;

type TestResult = Result<(), String>;

const GOLDEN_LOG_ENVELOPE: &str = include_str!("fixtures/golden/obs/log_envelope.json.golden");

#[test]
fn log_envelope_golden_pins_json_line_shape() -> TestResult {
    let line = GOLDEN_LOG_ENVELOPE.trim_end();
    let envelope: LogEnvelope = serde_json::from_str(line)
        .map_err(|error| format!("golden log envelope must parse: {error}"))?;

    assert_eq!(envelope.schema, LOG_ENVELOPE_SCHEMA_V1);
    assert_rfc3339_timestamp(&envelope.ts)?;
    assert_valid_level(&envelope.level)?;
    assert!(!envelope.target.trim().is_empty());
    assert!(envelope.fields.get("command").is_some());

    let reparsed: Value =
        serde_json::from_str(line).map_err(|error| format!("golden must be JSON: {error}"))?;
    assert!(reparsed["fields"].is_object());
    assert_eq!(
        serde_json::to_string(&envelope).map_err(|error| error.to_string())?,
        line
    );
    Ok(())
}

#[test]
fn status_command_emits_real_json_log_envelope_to_stderr_when_enabled() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();
    let init = run_ee(&["--workspace", &workspace, "--json", "init"])?;
    assert_success(&init, "init")?;

    let status = Command::new(env!("CARGO_BIN_EXE_ee"))
        .env("EE_LOG_FORMAT", "json")
        .args(["--workspace", &workspace, "status", "--json"])
        .output()
        .map_err(|error| format!("failed to run ee status with JSON logs: {error}"))?;
    assert_success(&status, "status")?;

    let stdout: Value = serde_json::from_slice(&status.stdout)
        .map_err(|error| format!("status stdout must be JSON: {error}"))?;
    assert_eq!(
        stdout.pointer("/schema").and_then(Value::as_str),
        Some("ee.response.v1")
    );

    let stderr = String::from_utf8(status.stderr)
        .map_err(|error| format!("status stderr was not UTF-8: {error}"))?;
    let mut lines = stderr.lines();
    let row = lines
        .next()
        .ok_or_else(|| "status JSON logging should emit one stderr envelope".to_owned())?;
    if lines.next().is_some() {
        return Err(format!(
            "status JSON logging emitted multiple rows: {stderr:?}"
        ));
    }

    let envelope: LogEnvelope = serde_json::from_str(row)
        .map_err(|error| format!("stderr log envelope must parse: {error}; row={row:?}"))?;
    assert_eq!(envelope.schema, LOG_ENVELOPE_SCHEMA_V1);
    assert_rfc3339_timestamp(&envelope.ts)?;
    assert_eq!(envelope.level, "info");
    assert_eq!(envelope.target, "ee.cli");
    assert_eq!(
        envelope.fields.get("event"),
        Some(&Value::String("command_start".to_owned()))
    );
    assert_eq!(
        envelope.fields.get("command"),
        Some(&Value::String("status".to_owned()))
    );
    Ok(())
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn assert_success(output: &Output, command: &str) -> TestResult {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "ee {command} failed with status {:?}; stdout={}; stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn assert_rfc3339_timestamp(timestamp: &str) -> TestResult {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|_| ())
        .map_err(|error| format!("timestamp must be RFC3339: {error}"))
}

fn assert_valid_level(level: &str) -> TestResult {
    match level {
        "trace" | "debug" | "info" | "warn" | "error" => Ok(()),
        other => Err(format!("invalid log level {other:?}")),
    }
}
