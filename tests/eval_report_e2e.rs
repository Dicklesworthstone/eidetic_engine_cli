//! Real-binary E2E coverage for `ee eval report`.
//!
//! The command is a read-only report over deterministic eval fixtures. This
//! test proves the public surface reuses fixture data, emits stable JSON, and
//! records a replayable JSONL execution log.

use serde_json::{Value as JsonValue, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_run_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-eval-report-e2e")
        .join(format!("{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

fn run_ee(workspace: &Path, args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {:?}: {error}", args))
}

fn parse_stdout_json(output: &Output, context: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context} stdout was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn append_event(
    log_path: &Path,
    step: &str,
    args: &[String],
    output: &Output,
) -> Result<(), String> {
    let event = json!({
        "schema": "ee.eval_report_e2e_event.v1",
        "step": step,
        "args": args,
        "exitCode": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| format!("failed to open {}: {error}", log_path.display()))?;
    serde_json::to_writer(&mut file, &event)
        .map_err(|error| format!("failed to write event JSON: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("failed to write event newline: {error}"))
}

fn ensure_stable_hash(value: &JsonValue, pointer: &str) -> TestResult {
    let hash = value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("missing data hash at {pointer}"))?;
    ensure(
        hash.len() == 16 && hash.chars().all(|ch| ch.is_ascii_hexdigit()),
        format!("{pointer} is not a 16-character hex data hash: {hash}"),
    )
}

fn normalize_eval_report_response(mut value: JsonValue) -> JsonValue {
    value["data"]["fixtureDir"] = JsonValue::String("<FIXTURE_DIR>".to_owned());
    if let Some(data_hashes) = value["data"]["dataHashes"].as_array_mut() {
        for hash in data_hashes {
            hash["dataHash"] = JsonValue::String("[data_hash]".to_owned());
        }
    }
    if value["data"]["firstFailure"].is_object() {
        value["data"]["firstFailure"]["dataHash"] = JsonValue::String("[data_hash]".to_owned());
    }
    if let Some(reports) = value["data"]["reports"].as_array_mut() {
        for report in reports {
            report["duration_ms"] = JsonValue::String("[duration_ms]".to_owned());
            report["data_hash"] = JsonValue::String("[data_hash]".to_owned());
        }
    }
    value
}

#[test]
fn eval_report_summarizes_fixture_hashes_and_first_failure_with_logged_binary_run() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create {}: {error}", workspace.display()))?;
    let events_path = run_dir.join("events.jsonl");
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval");
    let fixture_dir_arg = fixture_dir.to_string_lossy().into_owned();

    let report_args = vec![
        "--json".to_owned(),
        "eval".to_owned(),
        "report".to_owned(),
        "fx.release_failure.v1".to_owned(),
        "--fixture-dir".to_owned(),
        fixture_dir_arg,
    ];
    let report_output = run_ee(&workspace, &report_args)?;
    append_event(&events_path, "eval_report", &report_args, &report_output)?;
    ensure(
        report_output.status.success(),
        format!(
            "eval report failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&report_output.stdout),
            String::from_utf8_lossy(&report_output.stderr)
        ),
    )?;
    ensure(
        report_output.stderr.is_empty(),
        format!(
            "eval report should not write JSON diagnostics to stderr: {}",
            String::from_utf8_lossy(&report_output.stderr)
        ),
    )?;

    let report_json = parse_stdout_json(&report_output, "eval report")?;
    ensure(report_json["schema"] == "ee.response.v1", "response schema")?;
    ensure(report_json["success"] == true, "response success")?;
    ensure(
        report_json["data"]["command"] == "eval report",
        "command field",
    )?;
    ensure(
        report_json["data"]["schema"] == "ee.eval.report_summary.v1",
        "report summary schema",
    )?;
    ensure(report_json["data"]["status"] == "failed", "failed status")?;
    ensure(
        report_json["data"]["fixtureCount"] == 1,
        "single fixture count",
    )?;
    ensure(
        report_json["data"]["firstFailure"]["query"] == "clippy before release",
        "first failing query",
    )?;
    ensure(
        report_json["data"]["firstFailure"]["reasonCodes"]
            .as_array()
            .is_some_and(|codes| codes.iter().any(|code| code == "top_result_not_relevant")),
        "first failure reason codes include top_result_not_relevant",
    )?;
    ensure_stable_hash(&report_json, "/data/dataHashes/0/dataHash")?;
    ensure_stable_hash(&report_json, "/data/firstFailure/dataHash")?;
    ensure_stable_hash(&report_json, "/data/reports/0/data_hash")?;
    ensure(events_path.is_file(), "E2E JSONL log exists")?;

    let normalized = normalize_eval_report_response(report_json);
    let actual = serde_json::to_string_pretty(&normalized)
        .map_err(|error| format!("failed to serialize normalized response: {error}"))?
        + "\n";
    let expected = include_str!("golden/eval-report.snap");
    ensure(
        actual == expected,
        format!("eval report golden mismatch\nexpected:\n{expected}\nactual:\n{actual}"),
    )
}
