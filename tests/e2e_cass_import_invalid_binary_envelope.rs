//! bd-eb75o: real-binary pin test for `ee import cass` when
//! EE_CASS_BINARY points at a path that fails validation, surfacing
//! [`CassError::InvalidBinary`] through the `ee.error.v2` envelope.
//!
//! `handle_import_cass` (src/cli/mod.rs:15764) calls
//! `discover_import_binary(None)` after --since validation passes;
//! `discover_import_binary` honours `EE_CASS_BINARY` via
//! `validate_import_binary` (src/cass/client.rs:211), which rejects
//! ill-formed override paths with `CassError::InvalidBinary` *before*
//! any cass subprocess runs. The wrapper then routes the failure
//! through `DomainError::Import` and `write_domain_error`.
//!
//! The InvalidBinary variant has a stable kind_str (`invalid_binary`),
//! Display prefix (`cass binary '{path}' is not allowed: {reason}`),
//! and a repair hint that surfaces the concrete
//! `EE_CASS_BINARY=$(command -v cass)` workaround (issue #11), but
//! `grep -rln "InvalidBinary"
//! tests/` returns zero hits — no real-binary E2E proves the CLI
//! surface. This test pins three deterministic branches that fire
//! regardless of whether a real `cass` binary is installed on the host:
//!
//! 1. relative path (`cass`) -> "must be configured as an absolute path"
//! 2. absolute non-cass filename -> "file name must be `cass`"
//! 3. absolute cass-named nonexistent -> "metadata is unavailable"
//!
//! Mirrors the harness shape of tests/e2e_cass_import_since_invalid_duration.rs
//! (bd-cnsxf) and tests/e2e_context_show_missing_db.rs (bd-15ilq):
//! stdlib only, env!(CARGO_BIN_EXE_ee), table-driven, single #[test] fn.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

// Stable substring the InvalidBinary repair hint must always contain. The
// full wording was enriched (issue #11) to show the concrete
// `EE_CASS_BINARY=$(command -v cass)` workaround, so this pins the actionable
// invariant rather than the exact prose.
const CANONICAL_REPAIR_HINT: &str = "EE_CASS_BINARY=$(command -v cass)";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str], cass_binary: &str) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .env("EE_CASS_BINARY", cass_binary)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_root(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-cass-import-invalid-binary-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

struct InvalidBinaryCase {
    branch: &'static str,
    cass_binary_kind: CassBinaryKind,
    reason_tail: &'static str,
}

enum CassBinaryKind {
    /// A bare `cass` token that is not absolute.
    Relative,
    /// An absolute path whose filename is not `cass`.
    AbsoluteWrongName,
    /// An absolute path named `cass` whose parent directory does not
    /// exist, so `symlink_metadata` returns NotFound.
    AbsoluteMissing,
}

const INVALID_BINARY_CASES: &[InvalidBinaryCase] = &[
    InvalidBinaryCase {
        branch: "relative_path",
        cass_binary_kind: CassBinaryKind::Relative,
        reason_tail: "CASS import binary must be configured as an absolute path",
    },
    InvalidBinaryCase {
        branch: "absolute_wrong_name",
        cass_binary_kind: CassBinaryKind::AbsoluteWrongName,
        reason_tail: "CASS import binary file name must be `cass`",
    },
    InvalidBinaryCase {
        branch: "absolute_missing",
        cass_binary_kind: CassBinaryKind::AbsoluteMissing,
        reason_tail: "CASS import binary metadata is unavailable",
    },
];

fn build_cass_binary_arg(kind: &CassBinaryKind, branch: &str) -> Result<(String, PathBuf), String> {
    let root = unique_root(branch)?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let cass_binary = match kind {
        CassBinaryKind::Relative => "cass".to_owned(),
        CassBinaryKind::AbsoluteWrongName => root
            .join("notcass")
            .to_str()
            .ok_or_else(|| "notcass path must be UTF-8".to_string())?
            .to_owned(),
        CassBinaryKind::AbsoluteMissing => root
            .join("definitely-not-here")
            .join("cass")
            .to_str()
            .ok_or_else(|| "missing path must be UTF-8".to_string())?
            .to_owned(),
    };
    Ok((cass_binary, workspace))
}

#[test]
fn cass_import_invalid_binary_override_surfaces_invalid_binary_envelope() -> TestResult {
    for case in INVALID_BINARY_CASES {
        let (cass_binary, workspace) = build_cass_binary_arg(&case.cass_binary_kind, case.branch)?;
        let workspace_arg = workspace
            .to_str()
            .ok_or_else(|| "workspace path must be UTF-8".to_string())?
            .to_owned();

        let output = run_ee(
            &[
                "--workspace",
                workspace_arg.as_str(),
                "--json",
                "import",
                "cass",
                "--dry-run",
            ],
            cass_binary.as_str(),
        )?;
        ensure(
            !output.status.success(),
            format!(
                "branch {}: ee import cass with EE_CASS_BINARY=`{}` must exit nonzero; stdout: {}",
                case.branch,
                cass_binary,
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
        ensure(
            message.starts_with("cass binary '"),
            format!(
                "branch {}: error.message must begin with `cass binary '`; got `{message}`",
                case.branch,
            ),
        )?;
        ensure(
            message.contains("is not allowed:"),
            format!(
                "branch {}: error.message must contain `is not allowed:` separator; got `{message}`",
                case.branch,
            ),
        )?;
        ensure(
            message.contains(case.reason_tail),
            format!(
                "branch {}: error.message must contain `{}`; got `{message}`",
                case.branch, case.reason_tail,
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
