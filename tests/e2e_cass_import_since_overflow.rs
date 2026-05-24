//! bd-1rpkp: real-binary pin test for the overflow branches of
//! [`CassImportError::InvalidSince`] under `ee import cass --since`.
//!
//! Sister to bd-cnsxf (`tests/e2e_cass_import_since_invalid_duration.rs`,
//! shipped at c892274a) which pinned five of the six InvalidSince
//! message tails: empty, no leading digit, missing unit, unsupported
//! unit, and zero total. This pin closes the sixth — the overflow
//! family that fires from arithmetic guards inside
//! `parse_since_duration` (src/cass/import.rs:1884).
//!
//! Two representative overflow inputs deterministically hit two
//! distinct message tails:
//!
//! 1. 23-digit amount (`99999999999999999999999`) exceeds `u64::MAX` so
//!    the early `amount_text.parse::<u64>()` returns an error mapped to
//!    `"duration number is too large"`.
//! 2. 18-digit amount (`999999999999999999`) parses as u64 (u64::MAX is
//!    20 digits) but `amount.checked_mul(86400)` for the `d` unit
//!    overflows u64, yielding `"duration is too large"`.
//!
//! Both branches surface through the same CLI path as bd-cnsxf:
//! `handle_import_cass` (src/cli/mod.rs:15742) -> early return ->
//! `DomainError::Import` -> `write_domain_error` -> `ee.error.v2`
//! envelope. Validation runs before `discover_import_binary`, so the
//! test is hermetic (no fake cass binary, no DB writes, no network).
//!
//! Mirrors tests/e2e_cass_import_since_invalid_duration.rs harness
//! shape: stdlib only, env!(CARGO_BIN_EXE_ee), table-driven, single
//! #[test] fn.

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
        .join("ee-cass-import-since-overflow-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

struct OverflowCase {
    branch: &'static str,
    value: &'static str,
    message_tail: &'static str,
}

const OVERFLOW_CASES: &[OverflowCase] = &[
    // 23-digit number exceeds u64::MAX (20 digits), so
    // `amount_text.parse::<u64>()` errors before any multiplication
    // happens. Distinct from the next case because the message tail
    // is the parser's own error, not the arithmetic guard.
    OverflowCase {
        branch: "amount_exceeds_u64",
        value: "99999999999999999999999d",
        message_tail: "duration number is too large",
    },
    // 18-digit number parses as u64 (well below u64::MAX) but the
    // `checked_mul(86400)` for the `d` unit overflows u64
    // (1e18 * 86400 ~= 8.6e22 >> 1.8e19). Hits the post-parse
    // arithmetic guard.
    OverflowCase {
        branch: "product_exceeds_u64",
        value: "999999999999999999d",
        message_tail: "duration is too large",
    },
];

#[test]
fn cass_import_since_overflow_surfaces_invalid_since_envelope() -> TestResult {
    for case in OVERFLOW_CASES {
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
