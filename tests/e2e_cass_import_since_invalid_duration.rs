//! bd-cnsxf: real-binary pin test for `ee import cass --since <bogus>`
//! surfacing the [`CassImportError::InvalidSince`] variant through the
//! `ee.error.v2` envelope.
//!
//! `handle_import_cass` (src/cli/mod.rs:15742) calls
//! `parse_import_since_duration` *before* discovering the CASS binary, so
//! invalid --since values return DomainError::Import early with the
//! per-branch message from `parse_since_duration`
//! (src/cass/import.rs:1884) and the canonical repair hint from
//! `CassImportError::repair_hint` ("use --since with a duration like
//! 90d, 24h, or 7d3h"). The InvalidSince variant has unit coverage in
//! src/cass/import.rs#tests for the parser, but `grep -rn InvalidSince
//! tests/` returns zero hits — no real-binary E2E proves the CLI surface.
//!
//! This pin mirrors tests/e2e_context_show_missing_db.rs (bd-15ilq) — a
//! compact stdlib-only harness that drives the real binary, checks the
//! envelope shape, and pins the human-readable error tail.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

const CANONICAL_REPAIR_HINT: &str = "use --since with a duration like 90d, 24h, or 7d3h";

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

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-cass-import-since-invalid-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

/// One row of the InvalidSince truth-table: a bogus --since value and the
/// per-branch message tail that `parse_since_duration` emits.
struct InvalidSinceCase {
    value: &'static str,
    message_tail: &'static str,
    branch: &'static str,
}

const INVALID_SINCE_CASES: &[InvalidSinceCase] = &[
    InvalidSinceCase {
        value: "",
        message_tail: "duration must not be empty",
        branch: "empty",
    },
    InvalidSinceCase {
        value: "abc",
        message_tail: "expected a positive number",
        branch: "no_leading_digit",
    },
    InvalidSinceCase {
        value: "5",
        message_tail: "missing duration unit",
        branch: "missing_unit",
    },
    InvalidSinceCase {
        value: "5y",
        message_tail: "unsupported duration unit",
        branch: "unsupported_unit",
    },
    InvalidSinceCase {
        value: "0d",
        message_tail: "duration must be greater than zero",
        branch: "zero_total",
    },
];

#[test]
fn cass_import_since_invalid_duration_surfaces_invalid_since_envelope() -> TestResult {
    for case in INVALID_SINCE_CASES {
        let workspace = unique_workspace(case.branch)?;
        let workspace_arg = workspace
            .to_str()
            .ok_or_else(|| "workspace path must be UTF-8".to_string())?
            .to_owned();

        let output = run_ee(&[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "import",
            "cass",
            "--since",
            case.value,
        ])?;
        ensure(
            !output.status.success(),
            format!(
                "branch {}: ee import cass --since `{}` must exit nonzero; stdout: {}",
                case.branch,
                case.value,
                String::from_utf8_lossy(&output.stdout),
            ),
        )?;

        let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            format!(
                "branch {}: stdout must be JSON: {error}; raw: {}",
                case.branch,
                String::from_utf8_lossy(&output.stdout),
            )
        })?;
        ensure(
            parsed["schema"].as_str() == Some("ee.error.v2"),
            format!(
                "branch {}: envelope schema must be ee.error.v2; got {parsed}",
                case.branch,
            ),
        )?;
        ensure(
            parsed["success"].as_bool() == Some(false),
            format!(
                "branch {}: envelope success must be false; got {parsed}",
                case.branch,
            ),
        )?;

        let error = &parsed["error"];
        ensure(
            error.is_object(),
            format!(
                "branch {}: response must include an error object; got {parsed}",
                case.branch,
            ),
        )?;

        let message = error["message"].as_str().unwrap_or_default();
        let expected_prefix = format!("invalid --since value `{}`", case.value);
        ensure(
            message.contains(expected_prefix.as_str()),
            format!(
                "branch {}: error.message must contain `{expected_prefix}`; got `{message}`",
                case.branch,
            ),
        )?;
        ensure(
            message.contains(case.message_tail),
            format!(
                "branch {}: error.message must contain `{}`; got `{message}`",
                case.branch, case.message_tail,
            ),
        )?;

        let repair = error["repair"].as_str().unwrap_or_default();
        ensure(
            repair.contains(CANONICAL_REPAIR_HINT),
            format!(
                "branch {}: error.repair must contain canonical hint `{CANONICAL_REPAIR_HINT}`; got `{repair}`",
                case.branch,
            ),
        )?;
    }
    Ok(())
}
