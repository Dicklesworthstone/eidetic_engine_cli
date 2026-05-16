#![no_main]

//! Fuzz target for `ee insights --section <name>` dispatch.
//!
//! The section name is intentionally user-controlled: agents can call a
//! specific insights section from docs, generated plans, or ad hoc scripts.
//! This target drives arbitrary UTF-8-ish section names through the public
//! CLI JSON path and pins the machine-facing contract:
//!
//! 1. The command must not panic for any bounded input.
//! 2. `--json` must return valid JSON on stdout for both success and usage
//!    errors.
//! 3. Success envelopes must contain `ee.insights.v1` data and at most one
//!    selected section.
//! 4. Error envelopes must carry a structured code/message/repair surface
//!    instead of falling back to prose-only stderr diagnostics.

use std::ffi::OsString;

use ee::models::{ERROR_SCHEMA_V2, RESPONSE_SCHEMA_V1};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 2048;

fn run_json_section(section: &str) -> (String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let args = vec![
        OsString::from("ee"),
        OsString::from("--json"),
        OsString::from("insights"),
        OsString::from("--section"),
        OsString::from(section),
    ];

    let _exit = ee::cli::run(args, &mut stdout, &mut stderr);
    (
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

fn assert_success_envelope(value: &Value) {
    assert_eq!(
        value.get("schema").and_then(Value::as_str),
        Some(RESPONSE_SCHEMA_V1)
    );
    assert_eq!(value.get("success").and_then(Value::as_bool), Some(true));

    let data = value
        .get("data")
        .and_then(Value::as_object)
        .expect("success envelope must contain object data");
    assert_eq!(
        data.get("schema").and_then(Value::as_str),
        Some("ee.insights.v1")
    );
    assert_eq!(
        data.get("command").and_then(Value::as_str),
        Some("insights")
    );
    assert!(
        data.get("sections")
            .and_then(Value::as_array)
            .is_some_and(|sections| sections.len() <= 1),
        "section mode must emit zero or one selected section"
    );
    assert!(
        data.get("availableSections")
            .and_then(Value::as_array)
            .is_some_and(|sections| !sections.is_empty()),
        "insights response must advertise available sections"
    );
}

fn assert_error_envelope(value: &Value) {
    assert_eq!(
        value.get("schema").and_then(Value::as_str),
        Some(ERROR_SCHEMA_V2)
    );
    assert_eq!(value.get("success").and_then(Value::as_bool), Some(false));

    let error = value
        .get("error")
        .and_then(Value::as_object)
        .expect("error envelope must contain object error");
    assert!(
        error
            .get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| !code.is_empty()),
        "error envelope must carry a non-empty code"
    );
    assert!(
        error
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| !message.is_empty()),
        "error envelope must carry a non-empty message"
    );
    assert!(
        error.get("severity").and_then(Value::as_str).is_some(),
        "error envelope must carry severity"
    );
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let section = String::from_utf8_lossy(data);
    let (stdout, stderr) = run_json_section(section.as_ref());
    assert!(
        stderr.is_empty(),
        "JSON insights section dispatch must keep stderr empty, got {stderr:?}"
    );

    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout must be valid JSON: {error}; stdout={stdout:?}"));

    match value.get("schema").and_then(Value::as_str) {
        Some(RESPONSE_SCHEMA_V1) => assert_success_envelope(&value),
        Some(ERROR_SCHEMA_V2) => assert_error_envelope(&value),
        other => panic!("unexpected insights envelope schema {other:?}: {stdout}"),
    }
});
