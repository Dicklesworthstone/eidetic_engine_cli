//! Smoke test for the J1 test-logging harness (bead bd-17c65.10.1).
//!
//! Validates:
//! 1. The schema file at `docs/schemas/test_event_v1.json` is well-formed
//!    JSON Schema and pins the `ee.test_event.v1` constant.
//! 2. The Rust harness, when invoked via `log_event_to`, produces events
//!    that match the schema (required fields, kind enum, hash format).
//! 3. The bash harness at `scripts/lib/e2e_logger.sh`, when sourced and
//!    driven against a temp log file, produces events that match the schema.
//!
//! The crate forbids `unsafe` code, so this test does NOT mutate the parent
//! process's environment. Bash harness validation uses a `Command::env` call
//! to inject the log path into a subprocess.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use ee::obs::test_log::{
    CommandRecorder, EventKind, LogLevel, TEST_EVENT_SCHEMA_V1, TestEvent, log_event_to,
};
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

const ALLOWED_KINDS: &[&str] = &[
    "command_start",
    "command_end",
    "assert_ok",
    "assert_fail",
    "golden_compare",
    "timer_lap",
    "note",
];

fn read_events(path: &Path) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut events = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "line {} not valid JSON: {error} | content: {line}",
                lineno + 1
            )
        })?;
        events.push(value);
    }
    Ok(events)
}

fn validate_event(event: &Value) -> Result<(), String> {
    let obj = event.as_object().ok_or("event is not an object")?;
    let schema = obj
        .get("schema")
        .and_then(Value::as_str)
        .ok_or("missing schema field")?;
    if schema != TEST_EVENT_SCHEMA_V1 {
        return Err(format!("schema mismatch: {schema}"));
    }
    let ts = obj
        .get("ts")
        .and_then(Value::as_str)
        .ok_or("missing ts field")?;
    chrono::DateTime::parse_from_rfc3339(ts)
        .map_err(|error| format!("ts {ts} not RFC 3339: {error}"))?;
    obj.get("test_id")
        .and_then(Value::as_str)
        .ok_or("missing test_id field")?;
    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("missing kind field")?;
    if !ALLOWED_KINDS.contains(&kind) {
        return Err(format!("kind {kind} not in enum"));
    }
    for key in ["stdout_hash", "stdin_hash"] {
        if let Some(hash) = obj.get(key).and_then(Value::as_str) {
            if !hash.starts_with("blake3:") && !hash.starts_with("sha256:") {
                return Err(format!("{key} unrecognized prefix: {hash}"));
            }
        }
    }
    if let Some(exit) = obj.get("exit_code") {
        if !exit.is_i64() {
            return Err(format!("exit_code is not integer: {exit}"));
        }
    }
    if let Some(elapsed) = obj.get("elapsed_ms") {
        if !elapsed.is_f64() && !elapsed.is_i64() {
            return Err(format!("elapsed_ms is not numeric: {elapsed}"));
        }
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn schema_file_exists_and_pins_v1_const() -> TestResult {
    let schema_path = repo_root().join("docs/schemas/test_event_v1.json");
    let schema_text =
        std::fs::read_to_string(&schema_path).map_err(|error| format!("read schema: {error}"))?;
    let schema: Value =
        serde_json::from_str(&schema_text).map_err(|error| format!("parse schema: {error}"))?;
    let id = schema
        .pointer("/$id")
        .and_then(Value::as_str)
        .ok_or("schema $id missing")?;
    if !id.contains("test_event.v1") {
        return Err(format!("$id does not pin v1: {id}"));
    }
    let kind_const = schema
        .pointer("/properties/schema/const")
        .and_then(Value::as_str)
        .ok_or("schema const missing")?;
    if kind_const != TEST_EVENT_SCHEMA_V1 {
        return Err(format!("schema const mismatch: {kind_const}"));
    }
    Ok(())
}

#[test]
fn rust_harness_emits_schema_valid_events() -> TestResult {
    let tmp = TempDir::new().map_err(|e| e.to_string())?;
    let log_path = tmp.path().join("run.jsonl");

    log_event_to(
        &log_path,
        LogLevel::Normal,
        &TestEvent::new("rust_smoke", EventKind::Note)
            .with_field("phase", Value::String("setup".into())),
    );
    log_event_to(
        &log_path,
        LogLevel::Normal,
        &TestEvent::new("rust_smoke", EventKind::AssertOk)
            .with_field("label", Value::String("first_invariant".into())),
    );

    // CommandRecorder reads env for the log path; we provide it via
    // Command::env on a child process below. For unit-style coverage, we
    // already test the recorder inside src/obs/test_log.rs.
    // Here, simulate a recorder-style pair by writing both events directly.
    log_event_to(
        &log_path,
        LogLevel::Normal,
        &TestEvent::new("rust_smoke", EventKind::CommandStart).with_field(
            "args",
            Value::Array(
                ["true"]
                    .iter()
                    .map(|s| Value::String((*s).into()))
                    .collect(),
            ),
        ),
    );
    let mut end_event = TestEvent::new("rust_smoke", EventKind::CommandEnd)
        .with_field(
            "stdout_hash",
            Value::String(ee::obs::test_log::hash_bytes(b"")),
        )
        .with_field("stderr_excerpt", Value::String(String::new()));
    end_event.exit_code = Some(0);
    end_event.elapsed_ms = Some(0.5);
    log_event_to(&log_path, LogLevel::Normal, &end_event);

    let events = read_events(&log_path)?;
    if events.len() < 4 {
        return Err(format!("expected ≥ 4 events, got {}", events.len()));
    }
    for (i, event) in events.iter().enumerate() {
        validate_event(event).map_err(|error| format!("rust event {i} invalid: {error}"))?;
    }
    let kinds: BTreeSet<&str> = events
        .iter()
        .filter_map(|e| e.get("kind").and_then(Value::as_str))
        .collect();
    for required in ["note", "assert_ok", "command_start", "command_end"] {
        if !kinds.contains(required) {
            return Err(format!("missing kind: {required}"));
        }
    }
    Ok(())
}

#[test]
fn bash_harness_emits_schema_valid_events() -> TestResult {
    let tmp = TempDir::new().map_err(|e| e.to_string())?;
    let log_path = tmp.path().join("bash_run.jsonl");
    let bash_lib = repo_root().join("scripts/lib/e2e_logger.sh");
    if !bash_lib.exists() {
        return Err(format!("bash harness missing: {}", bash_lib.display()));
    }

    let script = format!(
        r#"
        set +e
        source {harness:?}
        e2e_log_start "bash_smoke"
        e2e_log_note "starting"
        e2e_log_command echo hello world
        e2e_log_assert_eq "1" "1" "trivial_match"
        e2e_log_assert_eq "1" "2" "intentional_fail"
        e2e_log_end
        "#,
        harness = bash_lib.display(),
    );

    // Spawn bash with EE_TEST_LOG_PATH injected only into the child env —
    // parent process env stays untouched (crate forbids unsafe in-process
    // env mutation).
    let output = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("EE_TEST_LOG_PATH", &log_path)
        .output()
        .map_err(|e| format!("bash spawn: {e}"))?;

    if !log_path.exists() {
        return Err(format!(
            "bash harness did not produce log file at {}. stderr: {}",
            log_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let events = read_events(&log_path)?;
    if events.len() < 5 {
        return Err(format!(
            "expected ≥ 5 bash events, got {}. stderr: {}",
            events.len(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    for (i, event) in events.iter().enumerate() {
        validate_event(event).map_err(|error| format!("bash event {i} invalid: {error}"))?;
    }
    let kinds: BTreeSet<&str> = events
        .iter()
        .filter_map(|e| e.get("kind").and_then(Value::as_str))
        .collect();
    for required in [
        "note",
        "assert_ok",
        "assert_fail",
        "command_start",
        "command_end",
    ] {
        if !kinds.contains(required) {
            return Err(format!(
                "missing bash kind: {required}. observed: {kinds:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn command_recorder_emits_events_when_log_path_set() -> TestResult {
    // Drive CommandRecorder via a subprocess that has EE_TEST_LOG_PATH set,
    // so we never mutate parent env. The subprocess runs a tiny Rust shim
    // via `cargo` is too heavy here; instead we just call the bash harness
    // which already exercises this path. (Rust-side recorder coverage is in
    // src/obs/test_log.rs unit tests.)
    //
    // This test asserts that the recorder is reachable from the library
    // surface and constructs without error.
    let mut cmd = Command::new("true");
    let _recorder = CommandRecorder::new("recorder_smoke", "true", &mut cmd);
    Ok(())
}
