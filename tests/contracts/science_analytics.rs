//! Gate 10 contract coverage for optional science analytics readiness.
//!
//! This suite pins the public CLI payloads for science status/eval surfaces and
//! enforces dependency-tree constraints for the `science-analytics` feature.

use ee::science::ScienceDegradation;
use serde_json::{Value as JsonValue, json};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

const DISALLOWED_SCIENCE_RUNTIME_CRATES: &[&str] = &["fnp-python", "pyo3"];
const SELECTED_SCIENCE_CRATES: &[&str] = &[];

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_json_equal(actual: &JsonValue, expected: &JsonValue, context: &str) -> TestResult {
    ensure(
        actual == expected,
        format!("{context}: expected {}, got {}", expected, actual),
    )
}

fn repo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path(name: &str) -> PathBuf {
    repo_path()
        .join("tests")
        .join("golden")
        .join("science")
        .join(format!("{name}.json"))
}

fn assert_fixture_json(name: &str, actual: &JsonValue) -> TestResult {
    let path = fixture_path(name);
    let update_mode = env::var("UPDATE_GOLDEN").is_ok();
    let serialized = serde_json::to_string_pretty(actual)
        .map_err(|error| format!("failed to serialize fixture `{name}`: {error}"))?;

    if update_mode {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        fs::write(&path, serialized + "\n")
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        eprintln!("Updated science fixture: {}", path.display());
        return Ok(());
    }

    let expected_text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "fixture missing: {} ({error}). Run with UPDATE_GOLDEN=1.",
            path.display()
        )
    })?;
    let expected: JsonValue = serde_json::from_str(&expected_text)
        .map_err(|error| format!("fixture {} is invalid JSON: {error}", path.display()))?;
    ensure_json_equal(actual, &expected, name)
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_json_stdout(output: Output, context: &str) -> Result<JsonValue, String> {
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("{context}: stderr not UTF-8: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "{context}: command failed with exit {:?}; stderr: {stderr}",
            output.status.code()
        ),
    )?;
    ensure(
        stderr.is_empty(),
        format!("{context}: diagnostics leaked to stderr: {stderr:?}"),
    )?;
    ensure(
        stdout.ends_with('\n'),
        format!("{context}: JSON stdout must end with newline"),
    )?;

    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout is invalid JSON: {error}\n{stdout}"))
}

fn fallback_snapshot(eval_science_payload: &JsonValue) -> Result<JsonValue, String> {
    let metrics = eval_science_payload
        .pointer("/data/scienceMetrics")
        .ok_or("missing /data/scienceMetrics")?;
    let degradation = metrics
        .get("degradationCode")
        .cloned()
        .ok_or("missing scienceMetrics.degradationCode")?;
    let fallback = if degradation.is_null() {
        "not_needed"
    } else {
        "simple_metrics"
    };
    Ok(json!({
        "schema": "ee.science.fallback_disabled.v1",
        "command": "eval run --science",
        "status": metrics.get("status").cloned().unwrap_or(JsonValue::Null),
        "available": metrics.get("available").cloned().unwrap_or(JsonValue::Null),
        "degradationCode": degradation,
        "fallback": fallback,
    }))
}

fn run_cargo_tree(extra: &[&str]) -> Result<String, String> {
    let mut args = vec![
        "tree",
        "--manifest-path",
        "Cargo.toml",
        "--edges",
        "normal,build,dev",
        "--prefix",
        "none",
    ];
    args.extend_from_slice(extra);

    let output = Command::new(env!("CARGO"))
        .args(&args)
        .output()
        .map_err(|error| format!("failed to run `cargo {}`: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo tree` failed for {}:\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("cargo tree output not UTF-8: {error}"))
}

fn tree_crate_names(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(ToString::to_string)
        .collect()
}

#[test]
fn science_status_json_matches_fixture() -> TestResult {
    let payload = parse_json_stdout(
        run_ee(&["--json", "analyze", "science-status"])?,
        "ee --json analyze science-status",
    )?;
    assert_fixture_json("status", &payload)
}

#[test]
fn eval_run_simple_json_matches_fixture() -> TestResult {
    let payload = parse_json_stdout(run_ee(&["--json", "eval", "run"])?, "ee --json eval run")?;
    assert_fixture_json("eval_simple", &payload)
}

#[test]
fn eval_run_science_json_matches_fixture() -> TestResult {
    let payload = parse_json_stdout(
        run_ee(&["--json", "eval", "run", "--science"])?,
        "ee --json eval run --science",
    )?;
    assert_fixture_json("eval_science", &payload)
}

#[test]
fn eval_run_science_fallback_snapshot_matches_fixture() -> TestResult {
    let payload = parse_json_stdout(
        run_ee(&["--json", "eval", "run", "--science"])?,
        "ee --json eval run --science",
    )?;
    let snapshot = fallback_snapshot(&payload)?;
    assert_fixture_json("fallback_disabled", &snapshot)
}

#[test]
fn science_input_too_large_contract_matches_fixture() -> TestResult {
    let degradation = ScienceDegradation::input_too_large();
    let payload = json!({
        "schema": "ee.science.input_too_large.v1",
        "code": degradation.code,
        "message": degradation.message,
        "repair": degradation.repair,
    });
    assert_fixture_json("input_too_large", &payload)
}

#[test]
fn default_build_does_not_enable_science_analytics_feature() -> TestResult {
    ensure(
        !cfg!(feature = "science-analytics"),
        "integration test default build unexpectedly enabled science-analytics",
    )
}

#[test]
fn science_feature_tree_excludes_forbidden_and_python_runtime_crates() -> TestResult {
    let tree = run_cargo_tree(&["--features", "science-analytics"])?;
    let names = tree_crate_names(&tree);

    let forbidden_hits: Vec<&str> = FORBIDDEN_CRATES
        .iter()
        .copied()
        .filter(|name| names.contains(*name))
        .collect();
    ensure(
        forbidden_hits.is_empty(),
        format!("science-analytics feature tree includes forbidden crates: {forbidden_hits:?}"),
    )?;

    let python_hits: Vec<&str> = DISALLOWED_SCIENCE_RUNTIME_CRATES
        .iter()
        .copied()
        .filter(|name| names.contains(*name))
        .collect();
    ensure(
        python_hits.is_empty(),
        format!("science runtime must exclude python bridge crates: {python_hits:?}"),
    )?;

    let selected: BTreeSet<&str> = SELECTED_SCIENCE_CRATES.iter().copied().collect();
    let unapproved_science: Vec<String> = names
        .iter()
        .filter(|name| name.starts_with("fnp-") || name.starts_with("fsci-"))
        .filter(|name| !selected.contains(name.as_str()))
        .cloned()
        .collect();
    ensure(
        unapproved_science.is_empty(),
        format!("science feature tree includes unapproved fnp/fsci crates: {unapproved_science:?}"),
    )
}
