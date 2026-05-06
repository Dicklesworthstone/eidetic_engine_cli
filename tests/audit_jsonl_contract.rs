use std::fs;
use std::process::{Command, Output};

use chrono::DateTime;
use ee::obs::{AUDIT_EVENT_SCHEMA_V1, AuditEvent};
use serde_json::Value;

type TestResult = Result<(), String>;

const GOLDEN_AUDIT_EVENT: &str = include_str!("fixtures/golden/obs/audit_event.json.golden");

#[test]
fn audit_event_golden_pins_jsonl_row_shape() -> TestResult {
    let line = GOLDEN_AUDIT_EVENT.trim_end();
    let event: AuditEvent = serde_json::from_str(line)
        .map_err(|error| format!("golden audit event must parse: {error}"))?;

    assert_required_audit_fields(&event)?;
    assert_eq!(
        serde_json::to_string(&event).map_err(|error| error.to_string())?,
        line
    );
    Ok(())
}

#[test]
fn audit_event_append_writes_one_valid_jsonl_row() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let audit_path = tempdir.path().join("audit.jsonl");
    let event: AuditEvent = serde_json::from_str(GOLDEN_AUDIT_EVENT.trim_end())
        .map_err(|error| format!("golden audit event must parse: {error}"))?;

    event
        .append_to_path(&audit_path)
        .map_err(|error| format!("append audit jsonl: {error}"))?;
    let contents = std::fs::read_to_string(&audit_path)
        .map_err(|error| format!("read audit jsonl: {error}"))?;
    let mut lines = contents.lines();
    let row = lines
        .next()
        .ok_or_else(|| "missing audit jsonl row".to_owned())?;
    if lines.next().is_some() {
        return Err("audit jsonl append wrote more than one row".to_owned());
    }

    let parsed: AuditEvent =
        serde_json::from_str(row).map_err(|error| format!("audit row must parse: {error}"))?;
    assert_required_audit_fields(&parsed)
}

#[test]
fn remember_command_appends_real_workspace_audit_jsonl_row() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();
    let init = run_ee(&["--workspace", &workspace, "--json", "init"])?;
    assert_success(&init, "init")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Run cargo fmt --check before release.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--json",
    ])?;
    assert_success(&remember, "remember")?;
    let remembered: Value = serde_json::from_slice(&remember.stdout)
        .map_err(|error| format!("remember stdout must be JSON: {error}"))?;
    let audit_id = remembered
        .pointer("/data/audit_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "remember output missing audit_id".to_owned())?;
    let memory_id = remembered
        .pointer("/data/memory_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "remember output missing memory_id".to_owned())?;

    let audit_path = tempdir.path().join(".ee").join("audit.jsonl");
    let contents = fs::read_to_string(&audit_path)
        .map_err(|error| format!("read {}: {error}", audit_path.display()))?;
    let mut lines = contents.lines();
    let row = lines
        .next()
        .ok_or_else(|| "remember should append one audit JSONL row".to_owned())?;
    if lines.next().is_some() {
        return Err(format!(
            "remember should append exactly one audit JSONL row, got {contents:?}"
        ));
    }

    let event: AuditEvent =
        serde_json::from_str(row).map_err(|error| format!("audit row must parse: {error}"))?;
    assert_required_audit_fields(&event)?;
    assert_eq!(event.actor, "ee remember");
    assert_eq!(event.action, "memory.create");
    assert_eq!(event.subject, format!("memory:{memory_id}"));
    assert_eq!(event.outcome, "success");
    assert_eq!(
        event.fields.get("audit_id"),
        Some(&Value::String(audit_id.to_owned()))
    );
    assert_eq!(
        event.fields.get("memory_id"),
        Some(&Value::String(memory_id.to_owned()))
    );
    assert_eq!(
        event.fields.get("command"),
        Some(&Value::String("ee remember".to_owned()))
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

fn assert_required_audit_fields(event: &AuditEvent) -> TestResult {
    assert_eq!(event.schema, AUDIT_EVENT_SCHEMA_V1);
    assert_rfc3339_timestamp(&event.ts)?;
    assert_non_empty(&event.actor, "actor")?;
    assert_non_empty(&event.action, "action")?;
    assert_non_empty(&event.subject, "subject")?;
    assert_valid_outcome(&event.outcome)?;

    let value = serde_json::to_value(event).map_err(|error| error.to_string())?;
    match value.get("fields") {
        Some(Value::Object(_)) => Ok(()),
        Some(other) => Err(format!("fields must be a JSON object, got {other:?}")),
        None => Err("fields must be present".to_owned()),
    }
}

fn assert_rfc3339_timestamp(timestamp: &str) -> TestResult {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|_| ())
        .map_err(|error| format!("timestamp must be RFC3339: {error}"))
}

fn assert_non_empty(value: &str, field: &str) -> TestResult {
    if value.trim().is_empty() {
        Err(format!("{field} must be non-empty"))
    } else {
        Ok(())
    }
}

fn assert_valid_outcome(outcome: &str) -> TestResult {
    match outcome {
        "success" | "failure" | "cancelled" | "dry_run" | "rollback" => Ok(()),
        other => Err(format!("invalid audit outcome {other:?}")),
    }
}
