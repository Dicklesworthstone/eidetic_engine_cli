//! EE-USR-005: degraded/offline trust and repair-plan acceptance scenario.

use std::process::Command;

use ee::models::degradation::{ALL_DEGRADATION_CODES, DegradedSubsystem};
use ee::models::error_codes::{AGENT_DETECTOR_UNAVAILABLE, DATABASE_LOCKED};
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_json(output: &std::process::Output, ctx: &str) -> Result<JsonValue, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).map_err(|error| format!("{ctx}: invalid JSON stdout: {error}"))
}

fn assert_stdout_only_machine_data(output: &std::process::Output, ctx: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.is_empty(),
        format!("{ctx}: stderr must be empty: {stderr}"),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.trim_start().starts_with('{'),
        format!("{ctx}: stdout should be JSON object"),
    )
}

fn collect_dependency_degradation_codes(json: &JsonValue) -> Vec<String> {
    json.pointer("/data/entries")
        .and_then(JsonValue::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("degradationCode")
                        .and_then(JsonValue::as_str)
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[test]
fn dependency_surface_reports_unavailable_dependency_codes() -> TestResult {
    let output = run_ee(&["diag", "dependencies", "--json"])?;
    ensure(output.status.success(), "diag dependencies should succeed")?;
    assert_stdout_only_machine_data(&output, "diag dependencies")?;
    let json = stdout_json(&output, "diag dependencies")?;

    ensure(
        json.get("schema")
            .and_then(JsonValue::as_str)
            .is_some_and(|schema| schema == "ee.response.v1"),
        "diag dependencies response schema",
    )?;

    let codes = collect_dependency_degradation_codes(&json);
    ensure(
        codes.iter().any(|code| code == "cass_unavailable"),
        "must include cass_unavailable degradation code",
    )?;
    ensure(
        codes
            .iter()
            .any(|code| code == "agent_detection_unavailable"),
        "must include agent_detection_unavailable degradation code",
    )?;

    ensure(
        json.pointer("/data/entries")
            .and_then(JsonValue::as_array)
            .is_some_and(|entries| !entries.is_empty()),
        "dependencies entries must be present",
    )?;

    // Diagram backend is contract-gated (not always emitted by live runtime
    // dependency scans). Verify the stable contract fixture still carries it.
    let dependency_contract =
        include_str!("fixtures/golden/dependencies/contract_matrix.json.golden");
    ensure(
        dependency_contract.contains("\"degradation_code\": \"diagram_backend_unavailable\""),
        "dependency contract fixture must carry diagram backend degradation code",
    )
}

#[test]
fn eval_and_science_status_return_useful_partial_output_under_degradation() -> TestResult {
    let eval_output = run_ee(&["eval", "run", "--science", "--json"])?;
    ensure(eval_output.status.success(), "eval run should succeed")?;
    assert_stdout_only_machine_data(&eval_output, "eval run")?;
    let eval = stdout_json(&eval_output, "eval run")?;
    ensure(
        eval.pointer("/data/scienceMetrics").is_some(),
        "eval run --science should include science metrics",
    )?;

    let science_output = run_ee(&["analyze", "science-status", "--json"])?;
    ensure(
        science_output.status.success(),
        "analyze science-status should succeed",
    )?;
    assert_stdout_only_machine_data(&science_output, "analyze science-status")?;
    let science = stdout_json(&science_output, "analyze science-status")?;
    let science_status = science
        .pointer("/data/status")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "science status missing".to_owned())?;
    ensure(
        science_status == "not_compiled" || science_status == "backend_unavailable",
        format!("unexpected science status: {science_status}"),
    )?;

    // Semantic-disabled and stale-graph degradations are pinned by scenario fixtures
    // used across eval contracts and should remain stable.
    let metamorphic_scenario = include_str!("fixtures/eval/metamorphic_evaluation/scenario.json");
    ensure(
        metamorphic_scenario.contains("\"semantic_disabled\""),
        "metamorphic scenario must retain semantic_disabled branch",
    )?;
    ensure(
        metamorphic_scenario.contains("\"semantic_disabled_fallback\""),
        "metamorphic scenario must retain semantic-disabled fallback branch",
    )
}

#[test]
fn doctor_fix_plan_is_ordered_and_non_destructive() -> TestResult {
    let output = run_ee(&["doctor", "--fix-plan", "--json"])?;
    ensure(output.status.success(), "doctor --fix-plan should succeed")?;
    assert_stdout_only_machine_data(&output, "doctor --fix-plan")?;
    let json = stdout_json(&output, "doctor --fix-plan")?;

    ensure(
        json.pointer("/data/mode")
            .and_then(JsonValue::as_str)
            .is_some_and(|mode| mode == "fix-plan"),
        "doctor fix-plan mode",
    )?;
    let steps = json
        .pointer("/data/steps")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "doctor fix-plan steps missing".to_owned())?;
    ensure(
        !steps.is_empty(),
        "doctor fix-plan should have at least one step",
    )?;

    for (index, step) in steps.iter().enumerate() {
        let order = step
            .get("order")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| "step.order missing".to_owned())?;
        ensure(
            order == u64::try_from(index + 1).map_err(|error| error.to_string())?,
            format!("step order mismatch at index {index}"),
        )?;
        let command = step
            .get("command")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| "step.command missing".to_owned())?;
        ensure(
            !command.contains("rm -rf")
                && !command.contains("git reset --hard")
                && !command.contains("git clean -fd"),
            format!("non-destructive contract violated by command: {command}"),
        )?;
    }

    ensure(
        json.pointer("/data/cassImportGuidance/suggestedCommands")
            .and_then(JsonValue::as_array)
            .is_some(),
        "doctor fix-plan should include suggested commands",
    )
}

#[test]
fn registries_cover_stale_and_locked_failure_modes() -> TestResult {
    ensure(
        DATABASE_LOCKED.id == "EE-E201",
        "database lock error code id should stay stable",
    )?;
    ensure(
        AGENT_DETECTOR_UNAVAILABLE.id == "EE-E503",
        "agent detector unavailable error code id should stay stable",
    )?;

    let search_stale_present = ALL_DEGRADATION_CODES
        .iter()
        .any(|code| code.subsystem == DegradedSubsystem::Search && code.id == "D003");
    ensure(
        search_stale_present,
        "search stale degradation must be registered",
    )?;

    let graph_stale_present = ALL_DEGRADATION_CODES
        .iter()
        .any(|code| code.subsystem == DegradedSubsystem::Graph && code.id == "D300");
    ensure(
        graph_stale_present,
        "graph stale degradation must be registered",
    )?;

    let science_backend_present = ALL_DEGRADATION_CODES
        .iter()
        .any(|code| code.subsystem == DegradedSubsystem::Science && code.id == "D800");
    ensure(
        science_backend_present,
        "science backend degradation must be registered",
    )
}
