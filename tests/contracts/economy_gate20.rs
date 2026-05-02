//! Gate 20 memory economy contract coverage.
//!
//! Freezes the public JSON shape for economy report, score, and dry-run prune
//! plan outputs. The tests exercise the real CLI binary, assert stdout/stderr
//! isolation, and normalize the prune-plan wall-clock timestamp before golden
//! comparison.

use chrono::DateTime;
use serde_json::Value as JsonValue;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

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

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("economy")
        .join(format!("{name}.json.golden"))
}

fn read_golden(name: &str) -> Result<JsonValue, String> {
    let path = golden_path(name);
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("golden {} must be JSON: {error}", path.display()))
}

fn pretty_json(value: &JsonValue) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| format!("json render failed: {error}"))
}

fn assert_json_golden(name: &str, actual: &JsonValue) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("failed to create golden dir {}: {error}", parent.display())
            })?;
        }
        fs::write(&path, pretty_json(actual)?)
            .map_err(|error| format!("failed to write golden {}: {error}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = read_golden(name)?;
    ensure(
        actual == &expected,
        format!(
            "economy golden mismatch for {name}\n--- expected\n{}\n+++ actual\n{}",
            pretty_json(&expected)?,
            pretty_json(actual)?
        ),
    )
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_json(args: &[&str]) -> Result<JsonValue, String> {
    let output = run_ee(args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

    ensure(
        output.status.success(),
        format!(
            "ee {} failed with {:?}; stderr: {stderr}",
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

fn normalize_prune_plan_timestamp(value: &mut JsonValue) -> TestResult {
    let generated_at = value
        .pointer("/data/generatedAt")
        .and_then(JsonValue::as_str)
        .ok_or("prune-plan generatedAt missing")?;
    DateTime::parse_from_rfc3339(generated_at)
        .map_err(|error| format!("generatedAt must be RFC 3339: {error}"))?;
    let slot = value
        .pointer_mut("/data/generatedAt")
        .ok_or("prune-plan generatedAt slot missing")?;
    *slot = JsonValue::String("1970-01-01T00:00:00+00:00".to_string());
    Ok(())
}

#[test]
fn gate20_economy_report_json_matches_golden() -> TestResult {
    let actual = run_json(&[
        "--json",
        "economy",
        "report",
        "--include-debt",
        "--include-reserves",
    ])?;

    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String("ee.response.v1".to_string()),
        "report envelope schema",
    )?;
    ensure_json_equal(
        actual.get("success"),
        JsonValue::Bool(true),
        "report success",
    )?;
    ensure_json_equal(
        actual.pointer("/data/total_artifacts"),
        serde_json::json!(67),
        "report total artifact count",
    )?;
    ensure_json_equal(
        actual.pointer("/data/overall_utility_score"),
        serde_json::json!(0.75),
        "report overall utility score",
    )?;
    ensure_json_equal(
        actual.pointer("/data/attention_budget_used"),
        serde_json::json!(2100.0),
        "report attention budget used",
    )?;
    ensure_json_equal(
        actual.pointer("/data/attention_budget_total"),
        serde_json::json!(4000.0),
        "report attention budget total",
    )?;
    ensure(
        actual.pointer("/data/maintenance_debt").is_some(),
        "report must include maintenance debt when requested",
    )?;
    ensure(
        actual.pointer("/data/tail_risk_reserves").is_some(),
        "report must include tail-risk reserves when requested",
    )?;
    ensure_json_equal(
        actual.pointer("/data/tail_risk_reserves/degradation_coverage"),
        serde_json::json!(0.85),
        "report tail-risk reserve coverage",
    )?;

    assert_json_golden("report_with_debt_and_reserves", &actual)
}

#[test]
fn gate20_economy_score_json_matches_golden() -> TestResult {
    let actual = run_json(&[
        "--json",
        "economy",
        "score",
        "mem_gate20_release_rule",
        "--artifact-type",
        "memory",
        "--breakdown",
    ])?;

    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String("ee.response.v1".to_string()),
        "score envelope schema",
    )?;
    ensure_json_equal(
        actual.get("success"),
        JsonValue::Bool(true),
        "score success",
    )?;
    ensure_json_equal(
        actual.pointer("/data/artifact_id"),
        JsonValue::String("mem_gate20_release_rule".to_string()),
        "score artifact id",
    )?;
    ensure_json_equal(
        actual.pointer("/data/utility_score"),
        serde_json::json!(0.82),
        "score utility score",
    )?;
    ensure_json_equal(
        actual.pointer("/data/cost_score"),
        serde_json::json!(0.65),
        "score cost score",
    )?;
    ensure_json_equal(
        actual.pointer("/data/confidence_score"),
        serde_json::json!(0.75),
        "score confidence score",
    )?;
    ensure(
        actual.pointer("/data/breakdown/retrieval_frequency") == Some(&serde_json::json!(12)),
        "score breakdown must include retrieval frequency",
    )?;

    assert_json_golden("score_with_breakdown", &actual)
}

#[test]
fn gate20_economy_prune_plan_dry_run_matches_golden() -> TestResult {
    let mut actual = run_json(&[
        "--json",
        "economy",
        "prune-plan",
        "--dry-run",
        "--max-recommendations",
        "3",
    ])?;
    normalize_prune_plan_timestamp(&mut actual)?;

    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String("ee.response.v1".to_string()),
        "prune-plan envelope schema",
    )?;
    ensure_json_equal(
        actual.pointer("/data/schema"),
        JsonValue::String("ee.economy.prune_plan.v1".to_string()),
        "prune-plan data schema",
    )?;
    ensure_json_equal(
        actual.pointer("/data/dryRun"),
        JsonValue::Bool(true),
        "prune-plan dry-run",
    )?;
    ensure_json_equal(
        actual.pointer("/data/mutationStatus"),
        JsonValue::String("not_applied".to_string()),
        "prune-plan mutation status",
    )?;
    ensure_json_equal(
        actual.pointer("/data/status"),
        JsonValue::String("planned".to_string()),
        "prune-plan status",
    )?;
    ensure_json_equal(
        actual.pointer("/data/summary/actions"),
        serde_json::json!(["revalidate", "retire", "compact"]),
        "prune-plan action ordering",
    )?;
    ensure(
        actual
            .pointer("/data/recommendations")
            .and_then(JsonValue::as_array)
            .is_some_and(|recommendations| {
                recommendations.iter().all(|entry| {
                    entry.pointer("/dryRunCommand")
                        == Some(&JsonValue::String(
                            "ee economy prune-plan --dry-run --json".to_string(),
                        ))
                })
            }),
        "prune-plan recommendations must include dry-run commands",
    )?;

    assert_json_golden("prune_plan_dry_run_top3", &actual)
}
