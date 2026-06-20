//! bd-3usjw.73 contract: `br` read commands recover from a transient
//! `.beads/issues.jsonl` partial-write parse race without hiding permanent
//! command failures.

#![allow(clippy::expect_used)]

use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

const BEAD_ID: &str = "bd-3usjw.73";
const DEGRADED_CODE: &str = "beads_jsonl_partial_write_transient";
const REQUEST_ID: &str = "bd-3usjw.73-contract";
const WORKSPACE_ID: &str = "br-jsonl-race-fixture";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn retained_artifact_root(test_id: &str) -> Result<PathBuf, String> {
    let target_root = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from))
        .unwrap_or_else(|| repo_root().join("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let root = target_root
        .join("br-concurrent-read-race")
        .join(format!("{test_id}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&root).map_err(|error| format!("create {}: {error}", root.display()))?;
    Ok(root)
}

fn write_file(path: &Path, content: &str) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create parent {}: {error}", parent.display()))?;
    }
    fs::write(path, content).map_err(|error| format!("write {}: {error}", path.display()))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> TestResult {
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod executable {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> TestResult {
    Ok(())
}

fn fake_br_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail
state_file="${EE_FAKE_BR_STATE:?EE_FAKE_BR_STATE required}"
mode="${EE_FAKE_BR_MODE:-transient}"
count=0
if [ -f "$state_file" ]; then
    count="$(cat "$state_file")"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$state_file"

case "$mode" in
    transient)
        if [ "$count" -eq 1 ]; then
            printf 'Configuration error: Invalid JSON at line 2318: invalid type: integer `7`, expected struct Issue\n' >&2
            exit 2
        fi
        printf '{"schema":"br.ready.v1","workspace_id":"%s","request_id":"%s","issues":[],"attempt":%s}\n' "${EE_FAKE_BR_WORKSPACE_ID:-br-jsonl-race-fixture}" "${EE_FAKE_BR_REQUEST_ID:-bd-3usjw.73-contract}" "$count"
        ;;
    actionable)
        if [ "$#" -ne 4 ] || [ "$1" != "ready" ] || [ "$2" != "--limit" ] || [ "$3" != "0" ] || [ "$4" != "--json" ]; then
            printf 'expected br ready --limit 0 --json, got:' >&2
            printf ' [%s]' "$@" >&2
            printf '\n' >&2
            exit 66
        fi
        cat <<'JSON'
{"schema":"br.ready.v1","issues":[{"id":"bd-actionable","status":"open","assignee":null,"issue_type":"bug"},{"id":"bd-assigned","status":"open","assignee":"AmberSparrow","issue_type":"bug"},{"id":"bd-in-progress","status":"in_progress","assignee":null,"issue_type":"bug"},{"id":"bd-epic","status":"open","assignee":null,"issue_type":"epic"}]}
JSON
        ;;
    permanent)
        printf 'Usage error: unknown br subcommand\n' >&2
        exit 64
        ;;
    *)
        printf 'unknown fake br mode: %s\n' "$mode" >&2
        exit 65
        ;;
esac
"#
}

fn install_fake_br(root: &Path) -> Result<PathBuf, String> {
    let bin_dir = root.join("bin");
    let fake_br = bin_dir.join("br");
    write_file(&fake_br, fake_br_script())?;
    make_executable(&fake_br)?;
    Ok(bin_dir)
}

fn prepend_path(bin_dir: &Path) -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut path = OsString::from(bin_dir.as_os_str());
    path.push(":");
    path.push(current);
    path
}

fn run_wrapper_with_args(root: &Path, mode: &str, args: &[&str]) -> Result<(Output, u128), String> {
    let bin_dir = install_fake_br(root)?;
    let state_file = root.join(format!("fake-br-state-{mode}"));
    let start = Instant::now();
    let output = Command::new(repo_root().join("scripts").join("br_retry.sh"))
        .args(args)
        .current_dir(repo_root())
        .env("PATH", prepend_path(&bin_dir))
        .env("EE_FAKE_BR_MODE", mode)
        .env("EE_FAKE_BR_STATE", &state_file)
        .env("EE_FAKE_BR_WORKSPACE_ID", WORKSPACE_ID)
        .env("EE_FAKE_BR_REQUEST_ID", REQUEST_ID)
        .output()
        .map_err(|error| format!("spawn scripts/br_retry.sh: {error}"))?;
    Ok((output, start.elapsed().as_millis()))
}

fn run_wrapper(root: &Path, mode: &str) -> Result<(Output, u128), String> {
    run_wrapper_with_args(root, mode, &["ready", "--json"])
}

fn parse_json_lines(stderr: &str) -> Result<Vec<Value>, String> {
    stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("stderr line is not JSON: {line:?}: {error}"))
        })
        .collect()
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_json_eq(json: &Value, pointer: &str, expected: &Value) -> TestResult {
    let actual = json
        .pointer(pointer)
        .ok_or_else(|| format!("{pointer} missing in {json:#}"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{pointer}: expected {expected:#}, got {actual:#}"))
    }
}

fn write_test_event(
    root: &Path,
    output: &Value,
    stderr_events: &[Value],
    elapsed_ms: u128,
) -> TestResult {
    let retry_attempts = stderr_events
        .iter()
        .rev()
        .find_map(|event| event.get("attempts").and_then(Value::as_u64))
        .unwrap_or(1);
    let event = json!({
        "schema": "ee.test_event.v1",
        "test": "br_concurrent_read_race_recovers_transient_partial_jsonl_parse",
        "workspace_id": WORKSPACE_ID,
        "request_id": REQUEST_ID,
        "bead_id": BEAD_ID,
        "surface": "scripts/br_retry.sh",
        "phase": "br_ready_json_read",
        "elapsed_ms": elapsed_ms,
        "retry_attempts": retry_attempts,
        "degraded_codes": [DEGRADED_CODE],
        "output_attempt": output.get("attempt").and_then(Value::as_u64).unwrap_or(0),
    });
    let line =
        serde_json::to_string(&event).map_err(|error| format!("serialize test event: {error}"))?;
    write_file(
        &root.join("br_concurrent_read_race_events.jsonl"),
        &(line + "\n"),
    )
}

#[test]
fn br_concurrent_read_race_recovers_transient_partial_jsonl_parse() -> TestResult {
    let root = retained_artifact_root("transient-recovery")?;
    let (output, elapsed_ms) = run_wrapper(&root, "transient")?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    ensure(
        output.status.success(),
        format!(
            "br_retry should recover from transient JSONL parse race\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ),
    )?;
    ensure(
        stdout.ends_with('\n'),
        "br_retry must preserve the successful br stdout newline",
    )?;
    let body: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse stdout JSON: {error}"))?;
    ensure_json_eq(&body, "/schema", &json!("br.ready.v1"))?;
    ensure_json_eq(&body, "/workspace_id", &json!(WORKSPACE_ID))?;
    ensure_json_eq(&body, "/request_id", &json!(REQUEST_ID))?;
    ensure_json_eq(&body, "/attempt", &json!(2))?;

    let stderr_events = parse_json_lines(&stderr)?;
    ensure(
        stderr_events.len() >= 2,
        format!("expected initial and recovery diagnostics, got {stderr_events:#?}"),
    )?;
    ensure_json_eq(&stderr_events[0], "/schema", &json!("ee.beads_retry.v1"))?;
    ensure_json_eq(&stderr_events[0], "/subcommand", &json!("ready"))?;
    ensure_json_eq(&stderr_events[0], "/attempts", &json!(1))?;
    ensure_json_eq(&stderr_events[0], "/succeeded", &json!(false))?;
    ensure_json_eq(
        &stderr_events[0],
        "/degraded_codes/0",
        &json!(DEGRADED_CODE),
    )?;

    let recovery = stderr_events
        .iter()
        .find(|event| event.pointer("/succeeded") == Some(&json!(true)))
        .ok_or_else(|| format!("missing recovery diagnostic: {stderr_events:#?}"))?;
    ensure_json_eq(recovery, "/attempts", &json!(2))?;
    ensure_json_eq(recovery, "/last_error_class", &json!("invalid_json_line"))?;
    ensure_json_eq(recovery, "/degraded_codes/0", &json!(DEGRADED_CODE))?;

    write_test_event(&root, &body, &stderr_events, elapsed_ms)?;
    Ok(())
}

#[test]
fn br_retry_does_not_retry_permanent_br_errors() -> TestResult {
    let root = retained_artifact_root("permanent-error")?;
    let (output, _) = run_wrapper(&root, "permanent")?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    ensure(
        !output.status.success(),
        "permanent fake br failure must be returned to the caller",
    )?;
    ensure(stdout.is_empty(), format!("unexpected stdout: {stdout:?}"))?;
    ensure(
        stderr.contains("Usage error: unknown br subcommand"),
        format!("expected original permanent stderr, got {stderr:?}"),
    )?;
    ensure(
        !stderr.contains("ee.beads_retry.v1"),
        format!("permanent errors must not emit retry diagnostics: {stderr:?}"),
    )?;
    Ok(())
}

#[test]
fn br_retry_actionable_scans_full_ready_queue_before_filtering() -> TestResult {
    let root = retained_artifact_root("actionable-full-scan")?;
    let (output, _) = run_wrapper_with_args(&root, "actionable", &["actionable", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    ensure(
        output.status.success(),
        format!(
            "br_retry actionable should scan full ready queue\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ),
    )?;
    ensure(stderr.is_empty(), format!("unexpected stderr: {stderr}"))?;
    let body: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse stdout JSON: {error}"))?;
    let rows = body
        .as_array()
        .ok_or_else(|| format!("actionable output must be a JSON array: {body:#}"))?;
    ensure(
        rows.len() == 1,
        format!("expected exactly one actionable row, got {rows:#?}"),
    )?;
    ensure_json_eq(&body, "/0/id", &json!("bd-actionable"))?;
    ensure_json_eq(&body, "/0/status", &json!("open"))?;
    Ok(())
}
