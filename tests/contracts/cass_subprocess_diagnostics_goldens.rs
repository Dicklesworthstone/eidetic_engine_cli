//! Golden artifacts for `ee.cass.subprocess_diagnostics.v1` (bd-6tl68).
//!
//! Pins the canonical JSON shapes emitted by
//! `CassImportError::subprocess_diagnostics_json()` so a future drift in
//! field ordering, sentinel keys, or nested objects fails the test rather
//! than slipping past the per-field assertions in `src/cass/import.rs`.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::cass::{CassError, CassImportError};
use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("cass")
        .join(format!("subprocess_diagnostics_{name}.json.golden"))
}

fn canonicalize(value: &Value) -> Result<String, String> {
    let sorted = sort_value(value);
    let mut text =
        serde_json::to_string_pretty(&sorted).map_err(|error| format!("serialize: {error}"))?;
    text.push('\n');
    Ok(text)
}

fn sort_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), sort_value(v)))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = serde_json::Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

fn assert_golden(name: &str, error: &CassImportError) -> TestResult {
    let diagnostics = error
        .subprocess_diagnostics_json()
        .ok_or_else(|| format!("expected subprocess diagnostics for {name}"))?;
    let actual = canonicalize(&diagnostics)?;

    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, &actual).map_err(|error| error.to_string())?;
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        actual == expected,
        format!(
            "golden mismatch {}\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ),
    )
}

#[test]
fn cass_subprocess_diagnostics_stdout_line_cap_matches_golden() -> TestResult {
    let error = CassImportError::Cass(CassError::Io {
        message: "cass subprocess stdout line exceeded 1048576 byte limit".to_string(),
    });
    assert_golden("cap_error_stdout_line", &error)
}

#[test]
fn cass_subprocess_diagnostics_invalid_utf8_matches_golden() -> TestResult {
    let error = CassImportError::Cass(CassError::Io {
        message: "cass subprocess stdout line was not valid utf-8".to_string(),
    });
    assert_golden("invalid_utf8", &error)
}

#[test]
fn cass_subprocess_diagnostics_stderr_capture_cap_matches_golden() -> TestResult {
    let error = CassImportError::Cass(CassError::Io {
        message: "cass subprocess stderr exceeded 65536 byte capture limit".to_string(),
    });
    assert_golden("cap_error_stderr_capture", &error)
}

#[test]
fn cass_subprocess_diagnostics_timeout_matches_golden() -> TestResult {
    let error = CassImportError::CassCommand {
        command: "cass view".to_string(),
        exit_code: None,
        stderr: "cass view: deadline exceeded".to_string(),
        timed_out: true,
        stderr_truncated: false,
        stdout_line_count: Some(7),
        peak_stdout_line_bytes: Some(2048),
        peak_stdout_buffer_bytes: Some(8192),
    };
    assert_golden("timeout", &error)
}

#[test]
fn cass_subprocess_diagnostics_command_failure_matches_golden() -> TestResult {
    let error = CassImportError::CassCommand {
        command: "cass view".to_string(),
        exit_code: Some(2),
        stderr: "cass view: missing --workspace flag".to_string(),
        timed_out: false,
        stderr_truncated: true,
        stdout_line_count: Some(3),
        peak_stdout_line_bytes: Some(512),
        peak_stdout_buffer_bytes: Some(1024),
    };
    assert_golden("command_failure", &error)
}
