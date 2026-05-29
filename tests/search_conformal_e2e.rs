//! Mock-free E2E coverage for calibrated search score intervals.
//!
//! Exercises the real `ee` binary against a real temporary workspace, using the
//! actual FrankenSQLite store and search index path. The only fixture data is a
//! deterministic calibration JSONL file written where the search subsystem
//! expects production calibration evidence.

#[path = "support/test_tracing.rs"]
mod test_tracing;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn stdout_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn assert_success(output: &Output, context: &str) -> TestResult {
    ensure(
        output.status.code() == Some(EXIT_SUCCESS),
        format!(
            "{context}: expected exit 0, got {:?}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn assert_stderr_empty(output: &Output, context: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!("{context}: stderr should be empty in JSON mode, got: {stderr}"),
    )
}

fn json_array<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a [Value], String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{context}: {pointer} must be an array"))
}

fn write_perfect_calibration(workspace: &Path) -> TestResult {
    let calibration_dir = workspace.join(".ee").join("search");
    fs::create_dir_all(&calibration_dir)
        .map_err(|error| format!("create calibration dir: {error}"))?;

    let mut rows = String::new();
    for i in 0..24 {
        let score = 0.10_f32 + (i as f32 * 0.03);
        rows.push_str(&format!(
            "{{\"score\":{score:.3},\"groundTruthRelevance\":{score:.3},\"source\":\"review-e2e\"}}\n"
        ));
    }
    fs::write(calibration_dir.join("calibration.jsonl"), rows)
        .map_err(|error| format!("write calibration fixture: {error}"))
}

fn artifact_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("search_conformal_e2e");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn persist_json_artifact(name: &str, value: &Value) {
    let path = artifact_dir().join(format!("{name}.json"));
    let _ = fs::write(
        path,
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    );
}

#[test]
fn search_json_uses_real_calibration_rows_for_score_intervals() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path();
    let workspace_arg = workspace.to_string_lossy().to_string();
    let trace = test_tracing::init_test_tracing(
        "review-2026-05-20",
        "search_json_uses_real_calibration_rows_for_score_intervals",
    );
    trace.setup("search_conformal_e2e", "created real temporary workspace");

    let init = run_ee(&["--workspace", &workspace_arg, "init", "--json"])?;
    assert_success(&init, "init")?;
    assert_stderr_empty(&init, "init")?;

    write_perfect_calibration(workspace)?;
    trace.setup(
        "search_conformal_e2e",
        "wrote production-shaped calibration JSONL rows",
    );

    let memories = [
        "Conformal score interval release rule requires calibrated search evidence.",
        "Search calibration evidence should tighten interval output for relevant memories.",
        "Release readiness searches must report calibrated score uncertainty.",
    ];
    for content in memories {
        let remember = run_ee(&[
            "--workspace",
            &workspace_arg,
            "remember",
            content,
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ])?;
        assert_success(&remember, "remember")?;
        assert_stderr_empty(&remember, "remember")?;
    }

    let rebuild = run_ee(&["--workspace", &workspace_arg, "index", "rebuild", "--json"])?;
    assert_success(&rebuild, "index rebuild")?;
    assert_stderr_empty(&rebuild, "index rebuild")?;
    trace.exercise(
        "search_conformal_e2e",
        "ee search --json",
        "rebuilt real index",
    );

    let search = run_ee(&[
        "--workspace",
        &workspace_arg,
        "search",
        "conformal calibrated search interval release",
        "--source-mode",
        "lexical_only",
        "--relevance-floor",
        "0.0",
        "--limit",
        "3",
        "--json",
    ])?;
    assert_success(&search, "search")?;
    assert_stderr_empty(&search, "search")?;

    let search_json = stdout_json(&search, "search")?;
    persist_json_artifact("search_calibrated_intervals", &search_json);
    ensure(
        search_json.pointer("/schema").and_then(Value::as_str) == Some("ee.response.v2"),
        "search response envelope schema must be ee.response.v2",
    )?;
    ensure(
        search_json.pointer("/success").and_then(Value::as_bool) == Some(true),
        "search response must succeed",
    )?;

    let degraded_codes = json_array(&search_json, "/data/degraded", "search degraded")?
        .iter()
        .filter_map(|entry| entry.get("code").and_then(Value::as_str))
        .collect::<Vec<_>>();
    ensure(
        !degraded_codes.contains(&"conformal_calibration_insufficient"),
        format!(
            "calibrated search must not emit insufficient-calibration degradation: {degraded_codes:?}"
        ),
    )?;

    let results = json_array(&search_json, "/data/results", "search results")?;
    ensure(!results.is_empty(), "search should return calibrated hits")?;

    let first = &results[0];
    let interval = json_array(first, "/scoreInterval", "search result scoreInterval")?;
    ensure(
        interval.len() == 2,
        format!("scoreInterval must have two bounds, got {interval:?}"),
    )?;
    let lower = interval[0]
        .as_f64()
        .ok_or_else(|| format!("lower scoreInterval bound must be numeric: {interval:?}"))?;
    let upper = interval[1]
        .as_f64()
        .ok_or_else(|| format!("upper scoreInterval bound must be numeric: {interval:?}"))?;
    ensure(
        (0.0..=1.0).contains(&lower) && (0.0..=1.0).contains(&upper) && lower <= upper,
        format!("scoreInterval bounds must be ordered unit scores, got [{lower}, {upper}]"),
    )?;
    ensure(
        (upper - lower) <= 0.001,
        format!("perfect calibration should produce a tight interval, got [{lower}, {upper}]"),
    )?;

    ensure(
        first.pointer("/coverageGuarantee").and_then(Value::as_f64) == Some(0.95),
        "coverageGuarantee must surface the configured 95% coverage target",
    )?;
    ensure(
        first
            .pointer("/metadata/scoreCalibration/status")
            .and_then(Value::as_str)
            == Some("calibrated"),
        "result metadata must report calibrated scoreCalibration status",
    )?;
    ensure(
        first
            .pointer("/metadata/scoreCalibration/sampleCount")
            .and_then(Value::as_u64)
            == Some(24),
        "result metadata must report real calibration sample count",
    )?;

    trace.verify(
        "search_conformal_e2e",
        "calibrated",
        "calibrated",
        "real search output used calibration rows for score interval metadata",
    );
    trace.teardown("search_conformal_e2e", "temporary workspace dropped");
    Ok(())
}
