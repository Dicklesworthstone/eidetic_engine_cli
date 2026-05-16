//! bd-1h8ji.6 — RCH verification control-plane fixture and e2e tests.
//!
//! These tests pin the safety-critical evidence surfaces around RCH-only
//! verification: wrapper classification, summary rendering, environment-failure
//! classes, compile-failure classes, and the local-Cargo tripwire.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Instant;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures_dir() -> PathBuf {
    repo_root()
        .join("tests")
        .join("fixtures")
        .join("rch_verify_control_plane")
}

fn rch_verify_script() -> PathBuf {
    repo_root().join("scripts").join("rch_verify.sh")
}

fn tripwire_script() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("check-local-cargo-tripwire.sh")
}

fn e2e_script() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("e2e_overhaul")
        .join("rch_verify_control_plane.sh")
}

fn trace_control_plane(phase: &'static str, status: &'static str, elapsed_ms: u64) {
    tracing::info!(
        workspace_id = "tests/rch_verify_control_plane",
        request_id = "rch_verify_control_plane_fixture",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-1h8ji.6"),
        surface = "rch_verification_control_plane",
        phase,
        status,
        elapsed_ms,
        command_hash = "",
        worker_id = "",
        degraded_codes = ?Vec::<String>::new(),
        "rch verification control-plane checkpoint"
    );
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn fixture_files() -> Result<Vec<PathBuf>, String> {
    let mut paths = fs::read_dir(fixtures_dir())
        .map_err(|error| format!("read fixtures dir: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn read_fixture(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn string_field<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}: missing string field {pointer}"))
}

fn array_field<'a>(
    value: &'a Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context}: missing array field {pointer}"))
}

fn number_field(value: &Value, pointer: &str, context: &str) -> Result<i32, String> {
    let raw = value
        .pointer(pointer)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{context}: missing integer field {pointer}"))?;
    i32::try_from(raw).map_err(|error| format!("{context}: {pointer} out of range: {error}"))
}

fn command_args(fixture: &Value, context: &str) -> Result<Vec<String>, String> {
    array_field(fixture, "/command", context)?
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context}: command[{index}] must be a string"))
        })
        .collect()
}

fn run_rch_verify_fixture(fixture: &Value, context: &str) -> Result<(ExitStatus, Value), String> {
    let bead_id = string_field(fixture, "/bead_id", context)?;
    let command = command_args(fixture, context)?;
    let started = Instant::now();
    trace_control_plane("setup", "ok", 0);
    let output = Command::new("bash")
        .arg(rch_verify_script())
        .arg("--bead-id")
        .arg(bead_id)
        .arg("--summary")
        .arg("--")
        .args(command)
        .env("RCH_BIN", "rch")
        .env("RCH_VERIFY_NOW", "2026-05-16T06:30:00.000000Z")
        .env(
            "RCH_VERIFY_FAKE_OUTPUT",
            string_field(fixture, "/fake_output", context)?,
        )
        .env(
            "RCH_VERIFY_FAKE_EXIT_CODE",
            number_field(fixture, "/fake_exit_code", context)?.to_string(),
        )
        .env(
            "RCH_VERIFY_FAKE_ELAPSED_MS",
            number_field(fixture, "/fake_elapsed_ms", context)?.to_string(),
        )
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("{context}: run rch_verify.sh: {error}"))?;
    trace_control_plane(
        "action",
        "ok",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        format!(
            "{context}: parse rch_verify output: {error}; stdout={stdout}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    Ok((output.status, report))
}

fn run_tripwire_fixture(fixture: &Value, context: &str) -> Result<(ExitStatus, Value), String> {
    let output = Command::new("sh")
        .arg(tripwire_script())
        .arg("--cmd")
        .arg(string_field(fixture, "/command_text", context)?)
        .arg("--json")
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("{context}: run tripwire: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        format!(
            "{context}: parse tripwire output: {error}; stdout={stdout}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    Ok((output.status, report))
}

fn value_strings<'a>(value: &'a Value, strings: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => strings.push(text),
        Value::Array(items) => {
            for item in items {
                value_strings(item, strings);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                value_strings(item, strings);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn assert_no_secret_or_private_temp_material(context: &str, value: &Value) -> TestResult {
    let mut strings = Vec::new();
    value_strings(value, &mut strings);
    for text in strings {
        ensure(
            !text.contains("/Users/jemanuel"),
            format!("{context}: leaked private user path in `{text}`"),
        )?;
        ensure(
            !text.contains("/private/var/folders"),
            format!("{context}: leaked private temp path in `{text}`"),
        )?;
        ensure(
            !text.to_ascii_lowercase().contains("token="),
            format!("{context}: leaked token-shaped text in `{text}`"),
        )?;
        ensure(
            !text.to_ascii_lowercase().contains("secret="),
            format!("{context}: leaked secret-shaped text in `{text}`"),
        )?;
    }
    Ok(())
}

fn expected_degraded_codes(expected: &Value) -> Result<Vec<String>, String> {
    Ok(expected
        .pointer("/degraded_codes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "expected degraded_codes entries must be strings".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?)
}

fn actual_string_array(value: &Value, pointer: &str) -> Result<Vec<String>, String> {
    Ok(value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing string array {pointer}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{pointer} entries must be strings"))
        })
        .collect::<Result<Vec<_>, _>>()?)
}

fn tripwire_summary(command_text: &str, report: &Value, status_class: &str) -> String {
    format!(
        "Local Cargo tripwire `{command_text}` => `{status_class}`.\n- schema: `{}`\n- allowed: `{}`\n- subcommand: `{}`",
        report["schema"].as_str().unwrap_or("unknown"),
        report["allowed"].as_str().unwrap_or("unknown"),
        report["subcommand"].as_str().unwrap_or("unknown")
    )
}

#[test]
fn fixture_catalog_has_required_schema_and_exactly_one_status_class() -> TestResult {
    let files = fixture_files()?;
    ensure(
        !files.is_empty(),
        "RCH control-plane fixture catalog is empty",
    )?;
    let mut names = BTreeSet::new();
    for path in files {
        let context = path.display().to_string();
        let fixture = read_fixture(&path)?;
        ensure(
            string_field(&fixture, "/schema", &context)?
                == "ee.rch.verify_control_plane_fixture.v1",
            format!("{context}: unexpected schema"),
        )?;
        let name = string_field(&fixture, "/name", &context)?;
        ensure(
            names.insert(name.to_owned()),
            format!("duplicate fixture name {name}"),
        )?;
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        ensure(
            stem == name,
            format!("{context}: filename stem must match name"),
        )?;
        let kind = string_field(&fixture, "/kind", &context)?;
        ensure(
            matches!(kind, "rch_verify" | "tripwire"),
            format!("{context}: unsupported kind {kind}"),
        )?;
        let status_class = string_field(&fixture, "/expected_status_class", &context)?;
        ensure(
            !status_class.is_empty() && !status_class.contains(','),
            format!("{context}: expected_status_class must contain exactly one class"),
        )?;
        ensure(
            fixture
                .pointer("/expected/summary_markdown")
                .and_then(Value::as_str)
                .is_some(),
            format!("{context}: expected summary golden is required"),
        )?;
        assert_no_secret_or_private_temp_material(&context, &fixture)?;
    }
    trace_control_plane("assert", "ok", 0);
    trace_control_plane("cleanup", "ok", 0);
    trace_control_plane("summary", "pass", 0);
    Ok(())
}

#[test]
fn fixture_transcripts_map_to_expected_status_classes() -> TestResult {
    for path in fixture_files()? {
        let context = path.display().to_string();
        let fixture = read_fixture(&path)?;
        let expected = fixture
            .pointer("/expected")
            .ok_or_else(|| format!("{context}: missing expected block"))?;
        let expected_exit = number_field(expected, "/process_exit_code", &context)?;
        match string_field(&fixture, "/kind", &context)? {
            "rch_verify" => {
                let (status, report) = run_rch_verify_fixture(&fixture, &context)?;
                ensure(
                    status.code().unwrap_or(-1) == expected_exit,
                    format!("{context}: process exit mismatch; report={report}"),
                )?;
                ensure(
                    report["schema"] == "ee.rch.verify.v1",
                    format!("{context}: bad schema"),
                )?;
                ensure(
                    report["status"] == expected["report_status"],
                    format!("{context}: status mismatch: {report}"),
                )?;
                ensure(
                    report["command_kind"] == expected["command_kind"],
                    format!("{context}: command kind mismatch: {report}"),
                )?;
                ensure(
                    report["worker_id"] == expected["worker_id"],
                    format!("{context}: worker mismatch: {report}"),
                )?;
                ensure(
                    actual_string_array(&report, "/degraded_codes")?
                        == expected_degraded_codes(expected)?,
                    format!("{context}: degraded code mismatch: {report}"),
                )?;
                if let Some(expected_file) = expected.pointer("/first_error_file") {
                    ensure(
                        report["first_error_file"] == *expected_file,
                        format!("{context}: first_error_file mismatch: {report}"),
                    )?;
                }
                if let Some(expected_line) = expected.pointer("/first_error_line") {
                    ensure(
                        report["first_error_line"] == *expected_line,
                        format!("{context}: first_error_line mismatch: {report}"),
                    )?;
                }
                if expected.pointer("/error_codes").is_some() {
                    ensure(
                        actual_string_array(&report, "/error_codes")?
                            == actual_string_array(expected, "/error_codes")?,
                        format!("{context}: error code mismatch: {report}"),
                    )?;
                }
                assert_no_secret_or_private_temp_material(&context, &report)?;
            }
            "tripwire" => {
                let (status, report) = run_tripwire_fixture(&fixture, &context)?;
                ensure(
                    status.code().unwrap_or(-1) == expected_exit,
                    format!("{context}: process exit mismatch; report={report}"),
                )?;
                ensure(
                    report["schema"] == expected["schema"],
                    format!("{context}: bad schema"),
                )?;
                ensure(
                    report["allowed"] == expected["allowed"],
                    format!("{context}: allowed mismatch: {report}"),
                )?;
                ensure(
                    report["subcommand"] == expected["subcommand"],
                    format!("{context}: subcommand mismatch: {report}"),
                )?;
                let detail = report["detail"].as_str().unwrap_or("");
                ensure(
                    detail.contains(string_field(expected, "/detail_contains", &context)?),
                    format!("{context}: tripwire detail mismatch: {report}"),
                )?;
                assert_no_secret_or_private_temp_material(&context, &report)?;
            }
            other => return Err(format!("{context}: unsupported kind {other}")),
        }
    }
    trace_control_plane("summary", "pass", 0);
    Ok(())
}

#[test]
fn rendered_summaries_match_golden_fixtures() -> TestResult {
    for path in fixture_files()? {
        let context = path.display().to_string();
        let fixture = read_fixture(&path)?;
        let expected_summary = string_field(&fixture, "/expected/summary_markdown", &context)?;
        let actual_summary = match string_field(&fixture, "/kind", &context)? {
            "rch_verify" => {
                let (_status, report) = run_rch_verify_fixture(&fixture, &context)?;
                report["summary_markdown"]
                    .as_str()
                    .ok_or_else(|| format!("{context}: missing summary_markdown"))?
                    .to_owned()
            }
            "tripwire" => {
                let (_status, report) = run_tripwire_fixture(&fixture, &context)?;
                tripwire_summary(
                    string_field(&fixture, "/command_text", &context)?,
                    &report,
                    string_field(&fixture, "/expected_status_class", &context)?,
                )
            }
            other => return Err(format!("{context}: unsupported kind {other}")),
        };
        ensure(
            actual_summary == expected_summary,
            format!(
                "{context}: summary golden mismatch\nexpected:\n{expected_summary}\nactual:\n{actual_summary}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn shell_e2e_driver_emits_phase_logs_and_final_summary() -> TestResult {
    let output = Command::new("bash")
        .arg(e2e_script())
        .env("RCH_BIN", "rch")
        .env("RCH_VERIFY_CONTROL_PLANE_LONG_BENCH", "0")
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run e2e driver: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "e2e driver failed with {:?}\nstdout={}\nstderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut phases = BTreeSet::new();
    let mut saw_summary = false;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value = serde_json::from_str(line)
            .map_err(|error| format!("parse e2e event: {error}; {line}"))?;
        ensure(
            event["schema"] == "ee.test_event.v1",
            format!("unexpected e2e event schema: {event}"),
        )?;
        if let Some(phase) = event["phase"].as_str() {
            phases.insert(phase.to_owned());
        }
        if event["phase"] == "summary" && event["status"] == "pass" {
            saw_summary = true;
        }
        assert_no_secret_or_private_temp_material("e2e event", &event)?;
    }
    for required in ["setup", "action", "assert", "cleanup", "summary"] {
        ensure(
            phases.contains(required),
            format!("e2e driver did not emit phase {required}; stdout={stdout}"),
        )?;
    }
    ensure(
        saw_summary,
        format!("e2e driver did not emit pass summary; stdout={stdout}"),
    )?;
    Ok(())
}
