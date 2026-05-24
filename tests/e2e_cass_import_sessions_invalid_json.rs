//! bd-12fqm: real-binary pin test for the [`CassImportError::InvalidJson`]
//! surface under `ee import cass` when the fake `cass sessions`
//! subprocess emits malformed or structurally-invalid JSON.
//!
//! `parse_sessions_json` (src/cass/import.rs:1039) emits InvalidJson
//! from two distinct branches that share the same Display prefix
//! (`invalid CASS sessions JSON: {message}`) and repair hint:
//!
//! 1. `serde_json::from_slice` returns an error -> message is the
//!    parser's `error.to_string()` (e.g., "expected value at line 1
//!    column 1").
//! 2. The JSON parses but `value.get("sessions").and_then(as_array)`
//!    returns None *and* `value.get("hits").and_then(as_array)` also
//!    returns None (i.e., no sessions array, no legacy hits fallback)
//!    -> message is the literal "missing sessions array".
//!
//! Both branches surface through `handle_import_cass`
//! (src/cli/mod.rs:15825) -> `cass_import_domain_error` (line 15859)
//! -> `DomainError::Import` (InvalidJson has no
//! `subprocess_diagnostics_json`) -> `write_domain_error` ->
//! `ee.error.v2`.
//!
//! Unit pins for the Display format and repair hint exist
//! (`tests/contracts/cass_import_error_display_contract.rs:74`,
//! `tests/contracts/cass_import_error_repair_hint_contract.rs:53`),
//! but no real-binary E2E proves the CLI actually routes this through
//! the envelope. This pin closes that gap with a hermetic fake-cass
//! shell script.
//!
//! Mirrors the fake-binary harness in
//! `tests/cass_import_concurrency.rs::write_fake_cass_binary` but
//! parameterizes the sessions stdout to drive each InvalidJson branch.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

const CANONICAL_REPAIR_HINT: &str = "run cass api-version --json and cass doctor --json";
const DISPLAY_PREFIX: &str = "invalid CASS sessions JSON:";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_root(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = base
        .join("ee-cass-import-sessions-invalid-json-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    // Ensure parent dir is not world-writable so validate_import_binary's
    // EnvVar-source parent check passes (mode 0o022 must be zero).
    let mut perms = fs::metadata(&dir)
        .map_err(|error| error.to_string())?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&dir, perms).map_err(|error| error.to_string())?;
    Ok(dir)
}

/// Write a fake `cass` shell binary at `path` that emits `sessions_payload`
/// verbatim on stdout for `cass sessions ...` and exits 0. Any other
/// subcommand prints to stderr and exits 64 so failures point at the
/// caller's request shape rather than the fake.
fn write_fake_cass_sessions_binary(path: &Path, sessions_payload: &str) -> TestResult {
    // Escape single quotes by ending the quoted string, inserting an
    // escaped quote, and reopening — the standard sh trick. The payload
    // is small and controlled, so plain replace is safe.
    let escaped = sessions_payload.replace('\'', "'\\''");
    let script = format!(
        r#"#!/bin/sh
set -eu
cmd="${{1:-}}"
case "$cmd" in
  sessions)
    printf '%s' '{escaped}'
    ;;
  *)
    echo "unexpected cass command: $cmd" >&2
    exit 64
    ;;
esac
"#
    );
    fs::write(path, script).map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    // 0o755: owner rwx, group rx, other rx. Satisfies
    // validate_import_binary_metadata's exec bit + no-group/other-writable
    // requirements.
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

fn run_ee(args: &[&str], env: &[(&str, &OsString)]) -> Result<Output, String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ee"));
    cmd.args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

struct InvalidJsonCase {
    branch: &'static str,
    /// What the fake `cass sessions` prints on stdout.
    sessions_payload: &'static str,
    /// Substring that must appear in the InvalidJson message tail (the
    /// part after the Display prefix). For the serde-parse branch this
    /// is a stable fragment of the parser's error string; for the
    /// missing-array branch it is the literal "missing sessions array".
    message_tail: &'static str,
}

const INVALID_JSON_CASES: &[InvalidJsonCase] = &[
    // Branch 1: malformed JSON. `not valid json` is a leading-identifier
    // that serde_json rejects immediately with a deterministic
    // "expected value" prefix. We only assert on "expected value" so a
    // serde_json revision that tweaks the line/column tail still passes.
    InvalidJsonCase {
        branch: "serde_parse_fails",
        sessions_payload: "not valid json",
        message_tail: "expected value",
    },
    // Branch 2: valid JSON, but the top-level object has neither
    // "sessions" nor the legacy "hits" array. parse_sessions_json
    // returns the literal "missing sessions array".
    InvalidJsonCase {
        branch: "missing_sessions_array",
        sessions_payload: r#"{"unrelated":42}"#,
        message_tail: "missing sessions array",
    },
];

#[test]
fn cass_import_sessions_invalid_json_surfaces_invalid_json_envelope() -> TestResult {
    for case in INVALID_JSON_CASES {
        let root = unique_root(case.branch)?;
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let cass_binary = root.join("cass");
        write_fake_cass_sessions_binary(&cass_binary, case.sessions_payload)?;

        let workspace_arg = workspace
            .to_str()
            .ok_or_else(|| "workspace path must be UTF-8".to_string())?
            .to_owned();
        let cass_binary_env: OsString = cass_binary.clone().into_os_string();

        let output = run_ee(
            &[
                "--workspace",
                workspace_arg.as_str(),
                "--json",
                "import",
                "cass",
                "--dry-run",
                "--no-spans",
            ],
            &[("EE_CASS_BINARY", &cass_binary_env)],
        )?;
        ensure(
            !output.status.success(),
            format!(
                "branch {}: ee import cass against fake binary `{}` must exit nonzero; stdout: {}; stderr: {}",
                case.branch,
                cass_binary.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
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
            message.starts_with(DISPLAY_PREFIX),
            format!(
                "branch {}: error.message must begin with `{DISPLAY_PREFIX}`; got `{message}`",
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
