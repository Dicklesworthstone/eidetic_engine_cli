use std::process::{Command, Output};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

const EXPECTED_FIXTURE_IDS: &[&str] = &[
    "fx.async_migration.v1",
    "fx.dangerous_cleanup.v1",
    "fx.data_size_tiers.v1",
    "fx.memory_poisoning.v1",
    "fx.metamorphic_evaluation.v1",
    "fx.release_failure.v1",
    "fx.semantic_model_admissibility.v1",
];

const GOLDEN_REPORTS: &[(&str, &str)] = &[
    (
        "fx.async_migration.v1",
        include_str!("fixtures/golden/eval/fx.async_migration.v1/report.json.golden"),
    ),
    (
        "fx.dangerous_cleanup.v1",
        include_str!("fixtures/golden/eval/fx.dangerous_cleanup.v1/report.json.golden"),
    ),
    (
        "fx.release_failure.v1",
        include_str!("fixtures/golden/eval/fx.release_failure.v1/report.json.golden"),
    ),
];

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn command_json(args: &[&str]) -> Result<Value, String> {
    let output = run_ee(args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(format!(
            "ee {} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            stdout,
            stderr
        ));
    }

    if !stderr.trim().is_empty() {
        return Err(format!(
            "ee {} wrote stderr during JSON success:\n{}",
            args.join(" "),
            stderr
        ));
    }

    serde_json::from_str(&stdout)
        .map_err(|error| format!("failed to parse JSON from ee {}: {error}", args.join(" ")))
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn string_field<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field at {pointer}"))
}

fn fixture_ids(value: &Value) -> Result<Vec<&str>, String> {
    let fixtures = value
        .pointer("/data/fixtures")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing /data/fixtures array".to_string())?;

    fixtures
        .iter()
        .map(|fixture| {
            fixture
                .get("fixture_id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("fixture entry missing fixture_id: {fixture:?}"))
        })
        .collect()
}

fn report_value(value: &Value) -> Result<Value, String> {
    value
        .pointer("/data/report")
        .cloned()
        .ok_or_else(|| "missing /data/report object".to_string())
}

fn normalized_response_for_stability(mut value: Value) -> Result<String, String> {
    let report = value
        .pointer_mut("/data/report")
        .ok_or_else(|| "missing /data/report object".to_string())?;
    report["duration_ms"] = json!("[duration_ms]");
    serde_json::to_string(&value).map_err(|error| format!("serialize stability JSON: {error}"))
}

fn normalized_report_for_golden(value: &Value) -> Result<String, String> {
    let mut report = report_value(value)?;

    ensure_equal(
        &string_field(&report, "/schema")?,
        &"ee.eval.report.v1",
        "report schema",
    )?;

    report["duration_ms"] = json!("[duration_ms]");
    report["data_hash"] = json!("[data_hash]");

    let per_query = report
        .pointer_mut("/metrics/per_query")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "missing /metrics/per_query array".to_string())?;
    per_query.sort_by(|left, right| {
        let left_query = left
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let right_query = right
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left_query.cmp(right_query)
    });

    let mut output = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("serialize report golden JSON: {error}"))?;
    output.push('\n');
    Ok(output)
}

fn stable_data_hash(value: &Value) -> Result<&str, String> {
    let data_hash = string_field(value, "/data/report/data_hash")?;
    let is_hex = data_hash.len() == 16 && data_hash.chars().all(|c| c.is_ascii_hexdigit());
    if is_hex {
        Ok(data_hash)
    } else {
        Err(format!(
            "data_hash is not a 16-character hex string: {data_hash}"
        ))
    }
}

#[test]
fn eval_list_json_enumerates_all_fixture_directories() -> TestResult {
    let value = command_json(&["--json", "eval", "list"])?;

    ensure_equal(
        &string_field(&value, "/schema")?,
        &"ee.response.v1",
        "response schema",
    )?;
    ensure_equal(
        &value.pointer("/success").and_then(Value::as_bool),
        &Some(true),
        "success",
    )?;
    ensure_equal(
        &string_field(&value, "/data/command")?,
        &"eval list",
        "command",
    )?;
    ensure_equal(
        &value.pointer("/data/fixtureCount").and_then(Value::as_u64),
        &Some(EXPECTED_FIXTURE_IDS.len() as u64),
        "fixtureCount",
    )?;

    let actual_ids = fixture_ids(&value)?;
    let expected_ids = EXPECTED_FIXTURE_IDS.to_vec();
    ensure_equal(&actual_ids, &expected_ids, "fixture IDs")
}

#[test]
fn eval_run_reports_are_stable_and_match_golden_snapshots() -> TestResult {
    for &(fixture_id, golden) in GOLDEN_REPORTS {
        let first = command_json(&["--json", "eval", "run", fixture_id])?;
        let second = command_json(&["--json", "eval", "run", fixture_id])?;

        ensure_equal(
            &stable_data_hash(&first)?,
            &stable_data_hash(&second)?,
            &format!("{fixture_id} data_hash stability"),
        )?;
        ensure_equal(
            &normalized_response_for_stability(first.clone())?,
            &normalized_response_for_stability(second)?,
            &format!("{fixture_id} JSON stability"),
        )?;

        let actual = normalized_report_for_golden(&first)?;
        ensure_equal(
            &actual.as_str(),
            &golden,
            &format!("{fixture_id} golden report"),
        )?;
    }

    Ok(())
}
