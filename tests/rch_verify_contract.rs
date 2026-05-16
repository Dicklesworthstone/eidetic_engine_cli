use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root().join("scripts/rch_verify.sh")
}

fn run_script(args: &[&str]) -> Result<(std::process::ExitStatus, String, String), String> {
    let output = Command::new("bash")
        .arg(script_path())
        .args(args)
        .env("RCH_VERIFY_NOW", "2026-05-16T04:40:00.000000Z")
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run rch verifier wrapper: {error}"))?;
    Ok((
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn run_json(args: &[&str]) -> Result<Value, String> {
    let (status, stdout, stderr) = run_script(args)?;
    if !status.success() {
        return Err(format!(
            "script failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    serde_json::from_str(&stdout).map_err(|error| format!("parse wrapper JSON: {error}"))
}

#[test]
fn script_is_syntax_valid_and_uses_explicit_rch_exec() -> TestResult {
    let output = Command::new("bash")
        .arg("-n")
        .arg(script_path())
        .output()
        .map_err(|error| format!("bash -n failed to start: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }

    let text =
        fs::read_to_string(script_path()).map_err(|error| format!("read wrapper: {error}"))?;
    if !text.contains("\"$RCH_BIN\" \"exec\" \"--\"") {
        return Err("wrapper must use explicit rch exec".to_owned());
    }
    if text.contains("CARGO_TARGET_DIR=/Volumes/USBNVME16TB") {
        return Err("remote command must not embed the Mac USB target path".to_owned());
    }
    Ok(())
}

#[test]
fn dry_run_accepts_focused_cargo_test_and_builds_remote_env() -> TestResult {
    let report = run_json(&[
        "--dry-run",
        "--",
        "cargo",
        "test",
        "--lib",
        "output::streaming",
        "--",
        "--nocapture",
    ])?;

    if report["schema"] != "ee.rch.verify.v1" {
        return Err("unexpected schema".to_owned());
    }
    if report["success"] != true {
        return Err("dry-run cargo test should succeed".to_owned());
    }
    if report["command_kind"] != "cargo_test" {
        return Err(format!("wrong command kind: {report}"));
    }
    if report["remote_required"] != true || report["would_offload"] != true {
        return Err("dry-run did not declare remote-only offload".to_owned());
    }
    let invocation = report["rch_invocation"]
        .as_array()
        .ok_or_else(|| "missing rch invocation".to_owned())?;
    let invocation_text = invocation
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if !invocation_text.contains("rch exec -- env TMPDIR=/tmp") {
        return Err(format!("unexpected invocation: {invocation_text}"));
    }
    if invocation_text.contains("/Volumes/USBNVME16TB") {
        return Err("dry-run remote invocation leaked Mac-only USB path".to_owned());
    }
    Ok(())
}

#[test]
fn dry_run_accepts_cargo_fmt_only_when_checking() -> TestResult {
    let report = run_json(&["--dry-run", "--", "cargo", "fmt", "--check"])?;
    if report["command_kind"] != "cargo_fmt_check" {
        return Err(format!(
            "cargo fmt --check classified incorrectly: {report}"
        ));
    }

    let (status, stdout, _stderr) = run_script(&["--dry-run", "--", "cargo", "fmt"])?;
    if status.success() {
        return Err("cargo fmt without --check should be refused".to_owned());
    }
    let rejected: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse rejection: {error}"))?;
    if rejected["degraded_codes"][0] != "rch_verify_refused_unknown_command" {
        return Err(format!("unexpected rejection: {rejected}"));
    }
    Ok(())
}

#[test]
fn dry_run_rejects_unknown_and_forbidden_commands_by_default() -> TestResult {
    let (status, stdout, _stderr) = run_script(&["--dry-run", "--", "echo", "hello"])?;
    if status.success() {
        return Err("unknown command should be refused without --allow-raw".to_owned());
    }
    let rejected: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse rejection: {error}"))?;
    if rejected["degraded_codes"][0] != "rch_verify_refused_unknown_command" {
        return Err(format!("unexpected unknown-command rejection: {rejected}"));
    }

    let (status, stdout, _stderr) = run_script(&["--dry-run", "--", "cargo", "test", "rm -rf"])?;
    if status.success() {
        return Err("forbidden command text should be refused".to_owned());
    }
    let forbidden: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse forbidden: {error}"))?;
    if forbidden["degraded_codes"][0] != "rch_verify_refused_forbidden_command" {
        return Err(format!(
            "unexpected forbidden-command rejection: {forbidden}"
        ));
    }
    Ok(())
}

#[test]
fn dry_run_json_is_deterministic_for_same_input() -> TestResult {
    let args = [
        "--dry-run",
        "--",
        "cargo",
        "clippy",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ];
    let first = run_json(&args)?;
    let second = run_json(&args)?;
    if first != second {
        return Err(format!(
            "dry-run proof is not deterministic:\n{first}\n{second}"
        ));
    }
    if first["command_kind"] != "cargo_clippy" {
        return Err("cargo clippy classified incorrectly".to_owned());
    }
    Ok(())
}
