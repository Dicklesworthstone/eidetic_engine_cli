//! Real-binary pin test for the default concise `ee doctor --json` contract.
//!
//! Unit coverage already pins the formatter. This E2E exercises the compiled
//! binary so Clap routing, stdout/stderr discipline, and the concise/full JSON
//! split stay wired together.

use std::process::{Command, Output};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn log_event(kind: &str, label: &str, fields: Value) {
    eprintln!(
        "{}",
        json!({
            "schema": "ee.test_event.v1",
            "suite": "e2e_doctor_concise_default",
            "kind": kind,
            "label": label,
            "fields": fields,
        })
    );
}

fn ensure(condition: bool, label: &str, details: Value) -> TestResult {
    let details_text = details.to_string();
    log_event(
        "assertion",
        label,
        json!({
            "passed": condition,
            "details": details,
        }),
    );
    if condition {
        Ok(())
    } else {
        Err(format!("{label}: {details_text}"))
    }
}

fn preview(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(400).collect()
}

fn run_ee(label: &str, args: &[&str]) -> Result<Output, String> {
    log_event(
        "command_start",
        label,
        json!({
            "command": "ee",
            "argv": args,
        }),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env("EE_NO_COLOR", "1")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_DATABASE_PATH")
        .env_remove("EE_INDEX_DIR")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    log_event(
        "command_end",
        label,
        json!({
            "exitCode": output.status.code(),
            "success": output.status.success(),
            "stdoutBytes": output.stdout.len(),
            "stderrBytes": output.stderr.len(),
            "stdoutPreview": preview(&output.stdout),
            "stderrPreview": preview(&output.stderr),
        }),
    );
    Ok(output)
}

fn parse_json(label: &str, output: &Output) -> Result<Value, String> {
    let value = serde_json::from_slice::<Value>(&output.stdout)
        .map_err(|error| format!("{label} stdout must be JSON: {error}"))?;
    log_event(
        "json_parse",
        label,
        json!({
            "schema": value.get("schema"),
            "success": value.get("success"),
            "fields": value.get("fields"),
        }),
    );
    Ok(value)
}

fn json_array_len(value: &Value, pointer: &str, label: &str) -> Result<usize, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| format!("{label}: {pointer} must be an array; got {value}"))
}

#[test]
fn doctor_default_json_is_concise_and_full_json_keeps_diagnostics() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_arg = workspace
        .path()
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;
    log_event(
        "workspace",
        "temp_workspace",
        json!({
            "workspace": workspace_arg,
        }),
    );

    let init_output = run_ee("init", &["--workspace", workspace_arg, "init", "--json"])?;
    ensure(
        init_output.status.success(),
        "init succeeds before doctor pin test",
        json!({
            "stdout": preview(&init_output.stdout),
            "stderr": preview(&init_output.stderr),
        }),
    )?;
    ensure(
        init_output.stderr.is_empty(),
        "init keeps stderr empty",
        json!({
            "stderr": preview(&init_output.stderr),
        }),
    )?;
    let init_json = parse_json("init", &init_output)?;
    ensure(
        init_json["schema"].as_str() == Some("ee.response.v2")
            && init_json["success"].as_bool() == Some(true),
        "init returns successful response envelope",
        json!({
            "schema": init_json.get("schema"),
            "success": init_json.get("success"),
        }),
    )?;

    let concise_output = run_ee(
        "doctor_concise",
        &["--workspace", workspace_arg, "doctor", "--json"],
    )?;
    ensure(
        concise_output.status.success(),
        "default doctor json succeeds",
        json!({
            "stdout": preview(&concise_output.stdout),
            "stderr": preview(&concise_output.stderr),
        }),
    )?;
    ensure(
        concise_output.stderr.is_empty(),
        "default doctor json keeps stderr empty",
        json!({
            "stderr": preview(&concise_output.stderr),
        }),
    )?;
    let concise = parse_json("doctor_concise", &concise_output)?;
    ensure(
        concise["schema"].as_str() == Some("ee.response.v2")
            && concise["success"].as_bool() == Some(true),
        "default doctor uses successful response envelope",
        json!({
            "schema": concise.get("schema"),
            "success": concise.get("success"),
        }),
    )?;
    ensure(
        concise["fields"].as_str() == Some("doctor_concise"),
        "default doctor fields are concise",
        json!({
            "fields": concise.get("fields"),
        }),
    )?;
    ensure(
        concise["data"]["mode"].as_str() == Some("concise")
            && concise["data"]["fullCommand"].as_str() == Some("ee doctor --full --json"),
        "default doctor points agents at full diagnostics",
        json!({
            "mode": concise.pointer("/data/mode"),
            "fullCommand": concise.pointer("/data/fullCommand"),
        }),
    )?;

    let core_len = json_array_len(&concise, "/data/coreChecks", "concise doctor")?;
    let actionable_len = json_array_len(&concise, "/data/actionable", "concise doctor")?;
    ensure(
        core_len > 0,
        "default doctor includes core checks",
        json!({
            "coreCheckCount": core_len,
        }),
    )?;
    ensure(
        concise["data"]["coreChecks"]
            .as_array()
            .is_some_and(|checks| {
                checks
                    .iter()
                    .all(|check| check["tier"].as_str() == Some("core"))
            }),
        "default doctor coreChecks are core-tier only",
        json!({
            "coreChecks": concise.pointer("/data/coreChecks"),
        }),
    )?;
    ensure(
        concise["data"]["advisorySummary"].is_object()
            && concise["data"]["advisorySummary"]["summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("ee doctor --full --json"))
            && concise["data"]["advisorySummary"]["fullCommand"].as_str()
                == Some("ee doctor --full --json"),
        "default doctor summarizes advisory diagnostics",
        json!({
            "actionableCount": actionable_len,
            "advisorySummary": concise.pointer("/data/advisorySummary"),
        }),
    )?;
    for omitted_key in [
        "checks",
        "advisories",
        "singleFlight",
        "flightRecorder",
        "qos",
        "rchWorkerPressure",
        "verificationPosture",
        "verificationLedger",
        "hostCalibration",
        "meshAutoEnrollment",
    ] {
        ensure(
            concise["data"].get(omitted_key).is_none(),
            "default doctor omits full diagnostic firehose",
            json!({
                "omittedKey": omitted_key,
            }),
        )?;
    }

    let full_output = run_ee(
        "doctor_full",
        &["--workspace", workspace_arg, "doctor", "--full", "--json"],
    )?;
    ensure(
        full_output.status.success(),
        "full doctor json succeeds",
        json!({
            "stdout": preview(&full_output.stdout),
            "stderr": preview(&full_output.stderr),
        }),
    )?;
    ensure(
        full_output.stderr.is_empty(),
        "full doctor json keeps stderr empty",
        json!({
            "stderr": preview(&full_output.stderr),
        }),
    )?;
    let full = parse_json("doctor_full", &full_output)?;
    ensure(
        full["schema"].as_str() == Some("ee.response.v2")
            && full["success"].as_bool() == Some(true)
            && full["fields"].as_str() == Some("full"),
        "full doctor uses exhaustive response envelope",
        json!({
            "schema": full.get("schema"),
            "success": full.get("success"),
            "fields": full.get("fields"),
        }),
    )?;
    ensure(
        json_array_len(&full, "/data/checks", "full doctor")? >= core_len,
        "full doctor includes exhaustive checks",
        json!({
            "conciseCoreCheckCount": core_len,
            "fullCheckCount": json_array_len(&full, "/data/checks", "full doctor")?,
        }),
    )?;
    ensure(
        full["data"]["meshAutoEnrollment"]["schema"].as_str()
            == Some("ee.doctor.mesh_auto_enrollment.v1")
            && json_array_len(&full, "/data/meshAutoEnrollment/checks", "full doctor mesh")? == 15,
        "full doctor includes mesh auto-enrollment diagnostics",
        json!({
            "meshSchema": full.pointer("/data/meshAutoEnrollment/schema"),
            "meshCheckCount": json_array_len(&full, "/data/meshAutoEnrollment/checks", "full doctor mesh")?,
        }),
    )?;
    ensure(
        full["data"]["rchWorkerPressure"]["schema"].as_str() == Some("ee.rch.worker_pressure.v1")
            && full["data"]["hostCalibration"]["schema"].as_str()
                == Some("ee.host_calibration.posture.v1")
            && full["data"]["verificationPosture"].is_object()
            && full["data"]["verificationLedger"].is_object(),
        "full doctor keeps advisory subsystem diagnostics",
        json!({
            "rchWorkerPressureSchema": full.pointer("/data/rchWorkerPressure/schema"),
            "hostCalibrationSchema": full.pointer("/data/hostCalibration/schema"),
            "hasVerificationPosture": full.pointer("/data/verificationPosture").is_some(),
            "hasVerificationLedger": full.pointer("/data/verificationLedger").is_some(),
        }),
    )?;
    ensure(
        concise_output.stdout.len() < full_output.stdout.len(),
        "default concise doctor output is smaller than full output",
        json!({
            "conciseBytes": concise_output.stdout.len(),
            "fullBytes": full_output.stdout.len(),
        }),
    )
}
