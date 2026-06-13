//! bd-27n26: real-binary pin test for `ee doctor --robot-triage --json`.
//!
//! Unit coverage pins the formatter. This E2E exercises the compiled binary so
//! Clap routing, JSON stdout discipline, and the agent-facing triage contract
//! stay wired together.

use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

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
        .env("EE_NO_COLOR", "1")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn severity_rank(severity: &str) -> Option<u8> {
    match severity {
        "error" => Some(0),
        "warning" => Some(1),
        "ok" => Some(2),
        _ => None,
    }
}

fn json_array<'a>(
    value: &'a Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context}: {pointer} must be an array; got {value}"))
}

fn json_u64(value: &Value, pointer: &str, context: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{context}: {pointer} must be an unsigned integer; got {value}"))
}

#[test]
fn doctor_robot_triage_json_route_is_agent_readable_response_envelope() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_arg = workspace
        .path()
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "doctor",
        "--robot-triage",
        "--json",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee doctor --robot-triage --json must succeed; stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "ee doctor --robot-triage --json must keep stderr empty; got {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("doctor robot-triage stdout must be JSON: {error}"))?;
    ensure(
        value["schema"].as_str() == Some("ee.response.v2"),
        format!("top-level schema must be ee.response.v2; got {value}"),
    )?;
    ensure(
        value["success"].as_bool() == Some(true),
        format!("robot-triage response must succeed; got {value}"),
    )?;

    let data = &value["data"];
    ensure(
        data["schema"].as_str() == Some("ee.doctor.robot_triage.v1"),
        format!("data schema must be ee.doctor.robot_triage.v1; got {data}"),
    )?;
    ensure(
        data["doctor_version"].as_str() == Some(env!("CARGO_PKG_VERSION")),
        format!("doctor_version must track the crate version; got {data}"),
    )?;
    ensure(
        data["sideEffectFree"].as_bool() == Some(true),
        format!("robot-triage must declare read-only execution; got {data}"),
    )?;
    ensure(
        data["configMutation"].as_str() == Some("never"),
        format!("robot-triage must not mutate config; got {data}"),
    )?;
    ensure(
        [
            "ok",
            "degraded_recoverable",
            "degraded_required",
            "blocked",
            "initializing",
        ]
        .contains(&data["posture"].as_str().unwrap_or_default()),
        format!("posture must use the doctor posture vocabulary; got {data}"),
    )?;
    ensure(
        data["overallHealthy"].as_bool().is_some(),
        format!("overallHealthy must be a boolean; got {data}"),
    )?;

    let entries = json_array(data, "/entries", "robot-triage data")?;
    let top_actionable = json_array(data, "/topActionable", "robot-triage data")?;
    let error_count = json_u64(data, "/counts/error", "robot-triage counts")?;
    let warning_count = json_u64(data, "/counts/warning", "robot-triage counts")?;
    let ok_count = json_u64(data, "/counts/ok", "robot-triage counts")?;
    let total_count = json_u64(data, "/counts/total", "robot-triage counts")?;
    ensure(
        total_count == error_count + warning_count + ok_count,
        format!("counts.total must equal severity subtotals; got {data}"),
    )?;
    ensure(
        total_count as usize == entries.len(),
        format!("counts.total must equal entries length; got {data}"),
    )?;
    ensure(
        !entries.is_empty(),
        "robot-triage must return doctor entries",
    )?;

    let mut prior_rank = 0u8;
    let mut prior_name = "";
    for (index, entry) in entries.iter().enumerate() {
        let name = entry["name"]
            .as_str()
            .ok_or_else(|| format!("entries[{index}].name must be a string; got {entry}"))?;
        ensure(
            !name.is_empty(),
            format!("entries[{index}].name must be non-empty; got {entry}"),
        )?;
        ensure(
            entry["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            format!("entries[{index}].message must be non-empty; got {entry}"),
        )?;
        let severity = entry["severity"]
            .as_str()
            .ok_or_else(|| format!("entries[{index}].severity must be a string; got {entry}"))?;
        let rank = severity_rank(severity)
            .ok_or_else(|| format!("entries[{index}].severity is not recognized: {entry}"))?;
        ensure(
            rank >= prior_rank,
            format!("entries must be severity-sorted; index {index} regressed in {entries:?}"),
        )?;
        if rank == prior_rank {
            ensure(
                prior_name <= name,
                format!("entries with same severity must be name-sorted; got {entries:?}"),
            )?;
        }
        prior_rank = rank;
        prior_name = name;
    }

    ensure(
        top_actionable.len() <= 5,
        format!("topActionable must be capped at five entries; got {top_actionable:?}"),
    )?;
    let expected_top: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["severity"].as_str() != Some("ok"))
        .take(5)
        .collect();
    let actual_top: Vec<&Value> = top_actionable.iter().collect();
    ensure(
        actual_top == expected_top,
        format!("topActionable must be the first non-ok entries; got {top_actionable:?}"),
    )
}
