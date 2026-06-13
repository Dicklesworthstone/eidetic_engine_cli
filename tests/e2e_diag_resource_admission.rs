//! bd-19bq8.2: real-binary pin test for `ee diag resource-admission`.
//!
//! `src/cli/mod.rs` has parser and in-process JSON shape coverage for this
//! route. This test invokes the compiled `ee` binary and pins the
//! safety-critical contract that resource-admission advice is advisory only and
//! cannot authorize claims or destructive follow-up commands.

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
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_stdout(output: &Output) -> Result<Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| format!("stdout must be JSON: {error}"))
}

#[test]
fn diag_resource_admission_wait_for_rch_is_advisory_only() -> TestResult {
    let output = run_ee(&[
        "--json",
        "diag",
        "resource-admission",
        "--surface",
        "verification",
        "--command-class",
        "verification",
        "--requested-profile",
        "swarm",
        "--effective-profile",
        "swarm",
        "--estimated-cost-class",
        "swarm-heavy",
        "--rch",
        "active-project-exclusion",
        "--local-cargo",
        "refused",
        "--lane-pressure",
        "verification-pressure",
        "--daemon",
        "not-required",
        "--replay",
        "not-required",
    ])?;

    ensure(
        output.status.success(),
        format!(
            "diag resource-admission must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "machine-readable diag route must keep stderr empty; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let parsed = parse_stdout(&output)?;
    ensure(
        parsed.pointer("/schema") == Some(&Value::String("ee.response.v2".to_owned())),
        format!("unexpected response envelope: {parsed}"),
    )?;
    ensure(
        parsed.pointer("/success") == Some(&Value::Bool(true)),
        format!("resource admission response must succeed: {parsed}"),
    )?;

    let data = parsed
        .pointer("/data")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("response data must be an object: {parsed}"))?;
    for (pointer, expected) in [
        ("/data/schema", "ee.resource_admission.v1"),
        ("/data/mutationPolicy", "never_mutates_state"),
        ("/data/decision", "wait_for_rch"),
        ("/data/subject/surface", "verification"),
        ("/data/subject/commandClass", "verification"),
        ("/data/subject/requestedProfile", "swarm"),
        ("/data/subject/effectiveProfile", "swarm"),
        ("/data/subject/estimatedCostClass", "swarm_heavy"),
        ("/data/sourcePosture/rch", "active_project_exclusion"),
        ("/data/sourcePosture/localCargo", "refused"),
        ("/data/sourcePosture/lanePressure", "verification_pressure"),
    ] {
        ensure(
            parsed.pointer(pointer) == Some(&Value::String(expected.to_owned())),
            format!("{pointer} must be {expected}; got {parsed}"),
        )?;
    }
    for (pointer, expected) in [
        ("/data/sideEffectFree", true),
        ("/data/advisoryOnly", true),
        ("/data/redactionPosture/rawMemoryBodyPresent", false),
        ("/data/redactionPosture/rawMailBodyPresent", false),
        ("/data/redactionPosture/rawCommandOutputPresent", false),
        ("/data/redactionPosture/absoluteHostPathPresent", false),
        ("/data/redactionPosture/fullEnvironmentPresent", false),
        ("/data/redactionPosture/secretsPresent", false),
    ] {
        ensure(
            parsed.pointer(pointer) == Some(&Value::Bool(expected)),
            format!("{pointer} must be {expected}; got {parsed}"),
        )?;
    }

    let reason_codes = data
        .get("reasonCodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "reasonCodes must be an array".to_owned())?;
    for required in [
        "rch_active_project_exclusion",
        "local_cargo_refused",
        "lane_pressure_verification",
        "redaction_posture_verified",
    ] {
        ensure(
            reason_codes.iter().any(|value| value == required),
            format!("reasonCodes must include {required}: {reason_codes:?}"),
        )?;
    }

    let next_commands = data
        .get("nextCommands")
        .and_then(Value::as_array)
        .ok_or_else(|| "nextCommands must be an array".to_owned())?;
    ensure(
        next_commands
            .iter()
            .all(|command| command.pointer("/destructive") == Some(&Value::Bool(false))),
        format!("resource-admission next commands must be non-destructive: {next_commands:?}"),
    )?;
    ensure(
        next_commands.iter().any(|command| {
            command.pointer("/command") == Some(&Value::String("rch status --json".to_owned()))
        }),
        format!("wait_for_rch advice must include bounded RCH status refresh: {next_commands:?}"),
    )?;

    if let Some(queue_pressure) = data.get("queuePressure") {
        ensure(
            queue_pressure.pointer("/canAuthorizeClaim") == Some(&Value::Bool(false)),
            format!("queuePressure must never authorize claims: {queue_pressure}"),
        )?;
    }

    Ok(())
}
