//! bd-d85wp: real-binary pin test for `ee schema export <unknown>`
//! schema_not_found envelope.
//!
//! `render_single_schema_export` (src/output/mod.rs:9165) emits an
//! `ee.error.v2` envelope when the requested schema id does not match any
//! `public_schemas()` entry:
//!
//!   {
//!     "schema": "ee.error.v2",
//!     "error": {
//!       "code": "schema_not_found",
//!       "message": "Schema '<id>' not found",
//!       "severity": "low",
//!       "repair": "ee schema list",
//!       "details": { "schemaId": "<id>" }
//!     }
//!   }
//!
//! The dispatch in src/cli/mod.rs:10616 just writes the envelope to stdout
//! and exits zero (success exit code). No real-binary test pins this
//! envelope today; failure_mode_catalog_coverage.rs:169 only references
//! the code string in an enum check.
//!
//! This pin-test mirrors the
//! `tests/e2e_context_show_missing_db.rs` harness shape.

#![cfg(unix)]

use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[test]
fn schema_export_unknown_id_emits_schema_not_found_envelope_with_zero_exit() -> TestResult {
    let phantom = "bogus_schema_id_not_in_public_schemas";
    let output = run_ee(&["--json", "schema", "export", phantom])?;
    ensure(
        output.status.success(),
        format!(
            "schema export <unknown> must exit zero (the renderer writes the error envelope without setting a non-zero exit code); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("schema export stdout must be JSON: {error}"))?;
    ensure(
        parsed["schema"].as_str() == Some("ee.error.v2"),
        format!("envelope schema must be ee.error.v2; got {parsed}"),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    ensure(
        error["code"].as_str() == Some("schema_not_found"),
        format!("error.code must be schema_not_found; got {error}"),
    )?;
    ensure(
        error["severity"].as_str() == Some("low"),
        format!("error.severity must be low; got {error}"),
    )?;
    ensure(
        error["repair"].as_str() == Some("ee schema list"),
        format!("error.repair must be `ee schema list`; got {error}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains(phantom),
        format!("error.message must echo the requested phantom id; got {message}"),
    )?;
    ensure(
        error["details"]["schemaId"].as_str() == Some(phantom),
        format!("error.details.schemaId must echo the requested phantom id; got {error}"),
    )?;
    Ok(())
}
