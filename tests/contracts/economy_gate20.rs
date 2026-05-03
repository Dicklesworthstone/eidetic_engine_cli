//! Gate 20 memory economy contract coverage.
//!
//! Freezes the public degraded JSON shape for economy commands until report,
//! score, simulation, and pruning metrics are backed by persisted workspace data
//! instead of static seed fixtures.

use serde_json::Value as JsonValue;
use std::process::{Command, Output};

type TestResult = Result<(), String>;
const UNSATISFIED_DEGRADED_MODE_EXIT: i32 = 7;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_json_equal(actual: Option<&JsonValue>, expected: JsonValue, context: &str) -> TestResult {
    let actual = actual.ok_or_else(|| format!("{context}: missing JSON field"))?;
    ensure(
        actual == &expected,
        format!("{context}: expected {expected:?}, got {actual:?}"),
    )
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_json_with_exit(args: &[&str], expected_exit: i32) -> Result<JsonValue, String> {
    let output = run_ee(args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

    ensure(
        output.status.code() == Some(expected_exit),
        format!(
            "ee {} returned {:?}, expected {expected_exit}; stderr: {stderr}",
            args.join(" "),
            output.status.code()
        ),
    )?;
    ensure(
        stderr.is_empty(),
        format!(
            "json command must keep diagnostics off stderr for ee {}: {stderr}",
            args.join(" ")
        ),
    )?;

    serde_json::from_str(&stdout).map_err(|error| {
        format!(
            "stdout must be parseable JSON for ee {}: {error}; stdout: {stdout}",
            args.join(" ")
        )
    })
}

fn assert_economy_unavailable(actual: &JsonValue, command: &str) -> TestResult {
    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String("ee.response.v1".to_string()),
        "economy unavailable envelope schema",
    )?;
    ensure_json_equal(
        actual.get("success"),
        JsonValue::Bool(false),
        "economy unavailable success",
    )?;
    ensure_json_equal(
        actual.pointer("/data/command"),
        JsonValue::String(command.to_string()),
        "economy unavailable command",
    )?;
    ensure_json_equal(
        actual.pointer("/data/code"),
        JsonValue::String("economy_metrics_unavailable".to_string()),
        "economy unavailable code",
    )?;
    ensure_json_equal(
        actual.pointer("/data/degraded/0/code"),
        JsonValue::String("economy_metrics_unavailable".to_string()),
        "economy unavailable degraded code",
    )?;
    ensure_json_equal(
        actual.pointer("/data/repair"),
        JsonValue::String("ee status --json".to_string()),
        "economy unavailable repair",
    )?;
    ensure_json_equal(
        actual.pointer("/data/followUpBead"),
        JsonValue::String("eidetic_engine_cli-ve0w".to_string()),
        "economy unavailable follow-up bead",
    )?;
    ensure_json_equal(
        actual.pointer("/data/evidenceIds"),
        JsonValue::Array(Vec::new()),
        "economy unavailable evidence ids",
    )?;
    ensure_json_equal(
        actual.pointer("/data/sourceIds"),
        JsonValue::Array(Vec::new()),
        "economy unavailable source ids",
    )?;
    ensure_json_equal(
        actual.pointer("/data/sideEffectClass"),
        JsonValue::String("read-only, conservative abstention".to_string()),
        "economy unavailable side-effect class",
    )
}

#[test]
fn gate20_economy_report_degrades_until_persisted_metrics_exist() -> TestResult {
    let actual = run_json_with_exit(
        &[
            "--json",
            "economy",
            "report",
            "--include-debt",
            "--include-reserves",
        ],
        UNSATISFIED_DEGRADED_MODE_EXIT,
    )?;

    assert_economy_unavailable(&actual, "economy report")
}

#[test]
fn gate20_economy_score_degrades_until_persisted_metrics_exist() -> TestResult {
    let actual = run_json_with_exit(
        &[
            "--json",
            "economy",
            "score",
            "mem_gate20_release_rule",
            "--artifact-type",
            "memory",
            "--breakdown",
        ],
        UNSATISFIED_DEGRADED_MODE_EXIT,
    )?;

    assert_economy_unavailable(&actual, "economy score")
}

#[test]
fn gate20_economy_simulate_degrades_until_persisted_metrics_exist() -> TestResult {
    let actual = run_json_with_exit(
        &[
            "--json",
            "economy",
            "simulate",
            "--baseline-budget",
            "4000",
            "--budget",
            "2000",
            "--budget",
            "8000",
        ],
        UNSATISFIED_DEGRADED_MODE_EXIT,
    )?;

    assert_economy_unavailable(&actual, "economy simulate")
}

#[test]
fn gate20_economy_prune_plan_degrades_until_persisted_metrics_exist() -> TestResult {
    let actual = run_json_with_exit(
        &[
            "--json",
            "economy",
            "prune-plan",
            "--dry-run",
            "--max-recommendations",
            "3",
        ],
        UNSATISFIED_DEGRADED_MODE_EXIT,
    )?;

    assert_economy_unavailable(&actual, "economy prune-plan")
}
