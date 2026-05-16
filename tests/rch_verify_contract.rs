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

fn target_tmp_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/rch-verify-contract"))
}

fn run_script_with_env(
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(std::process::ExitStatus, String, String), String> {
    let mut command = Command::new("bash");
    command
        .arg(script_path())
        .args(args)
        .env("RCH_VERIFY_NOW", "2026-05-16T04:40:00.000000Z")
        .current_dir(repo_root());
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("run rch verifier wrapper: {error}"))?;
    Ok((
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn run_script(args: &[&str]) -> Result<(std::process::ExitStatus, String, String), String> {
    run_script_with_env(args, &[])
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

fn degraded_contains(report: &Value, expected: &str) -> Result<bool, String> {
    Ok(report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?
        .iter()
        .any(|code| code == expected))
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
    if report["would_offload"] != false {
        return Err("cargo fmt --check should not claim RCH offload".to_owned());
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

#[test]
fn synthetic_remote_transcript_extracts_worker_id() -> TestResult {
    let (status, stdout, stderr) = run_script_with_env(
        &["--", "cargo", "test", "--test", "rch_verify_contract"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=900\nremote test ok\n[RCH] remote trj (12.3s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "123"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "fake transcript run failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse transcript: {error}"))?;
    if report["worker_id"] != "trj" {
        return Err(format!("worker id was not extracted: {report}"));
    }
    if report["elapsed_ms"] != 123 {
        return Err("fake elapsed_ms was not preserved".to_owned());
    }
    if report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?
        .iter()
        .any(|code| code == "rch_verify_remote_marker_missing")
    {
        return Err("remote marker was present but missing-marker degradation emitted".to_owned());
    }
    if report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?
        .iter()
        .any(|code| code == "rch_verify_capacity_or_timeout")
    {
        return Err(
            "successful timeout-text transcript should not be capacity degraded".to_owned(),
        );
    }
    Ok(())
}

#[test]
fn synthetic_local_fallback_refusal_is_not_worker_id() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--test", "rch_verify_contract"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] local (dependency preflight RCH-E327: Path dependency topology policy failed.)\n[RCH] remote required; refusing local fallback (dependency preflight failed)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "42"),
        ],
    )?;
    if status.success() {
        return Err("local fallback refusal should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse fallback: {error}"))?;
    if !report["worker_id"].is_null() {
        return Err(format!(
            "fallback marker was misread as worker id: {report}"
        ));
    }
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "fallback should be an environment failure: {report}"
        ));
    }
    let degraded = report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?;
    for expected in [
        "rch_verify_topology_blocked",
        "rch_verify_local_fallback_refused",
        "rch_verify_remote_marker_missing",
    ] {
        if !degraded.iter().any(|code| code == expected) {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    Ok(())
}

#[test]
fn synthetic_remote_test_failure_with_timeout_env_is_remote_failure() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--",
            "cargo",
            "test",
            "--lib",
            "why_toon_matches_json_contract",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=900\nrunning 1 test\ntest cli::tests::why_toon_matches_json_contract ... FAILED\nError: \"expected Number(12), got Number(12.0)\"\n[RCH] remote trj failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "195544"),
        ],
    )?;
    if status.success() {
        return Err("remote Rust test failure should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse remote failure: {error}"))?;
    if report["worker_id"] != "trj" {
        return Err(format!("remote failure should retain worker id: {report}"));
    }
    if report["status"] != "remote_failure" {
        return Err(format!(
            "remote Rust test failure should not be capacity: {report}"
        ));
    }
    let degraded = report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?;
    if !degraded
        .iter()
        .any(|code| code == "rch_verify_remote_command_failed")
    {
        return Err(format!("missing remote failure degraded code: {report}"));
    }
    if degraded
        .iter()
        .any(|code| code == "rch_verify_capacity_or_timeout")
    {
        return Err(format!("remote test failure was misclassified: {report}"));
    }
    Ok(())
}

#[test]
fn synthetic_pre_cargo_disk_full_extracts_selected_worker() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--lib", "task_frame"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "2026-05-16T12:58:58Z INFO Selected worker: csd at ubuntu@csd (8 slots, speed 50.0)\nrsync: [receiver] mkstemp \"/data/projects/eidetic_engine_cli/.rchignore.XXXXXX\" failed: No space left on device (28)\n[RCH] remote required; refusing local fallback (remote pipeline failed)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "1998"),
            ("RCH_VERIFY_DISABLE_DISK_FULL_RETRY", "1"),
        ],
    )?;
    if status.success() {
        return Err("disk-full transcript should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse disk-full: {error}"))?;
    if report["worker_id"] != "csd" {
        return Err(format!("selected worker was not extracted: {report}"));
    }
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "disk-full local-fallback refusal should be environment failure: {report}"
        ));
    }
    for expected in [
        "rch_verify_remote_command_failed",
        "rch_verify_worker_disk_full",
        "rch_verify_local_fallback_refused",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    if degraded_contains(&report, "rch_verify_remote_marker_missing")? {
        return Err(format!(
            "selected-worker transcript should not be remote-marker missing: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_disk_full_retry_stops_when_quarantine_is_ignored() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--lib", "qos"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "INFO Selected worker: csd at ubuntu@csd (8 slots, speed 50.0)\nrsync: write failed on \"/data/projects/eidetic_engine_cli/.rchignore\": No space left on device (28)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "20"),
            ("RCH_VERIFY_HEALTHY_WORKERS", "css,trj"),
            (
                "RCH_VERIFY_FAKE_RETRY_OUTPUT",
                "INFO Selected worker: csd at ubuntu@csd (8 slots, speed 50.0)\nrsync: write failed on \"/data/projects/eidetic_engine_cli/.rchignore\": No space left on device (28)\n",
            ),
            ("RCH_VERIFY_FAKE_RETRY_EXIT_CODE", "1"),
        ],
    )?;
    if status.success() {
        return Err("ignored quarantine retry should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse retry: {error}"))?;
    if report["worker_id"] != "csd" {
        return Err(format!(
            "retry worker should record ignored quarantine: {report}"
        ));
    }
    for expected in [
        "rch_verify_worker_disk_full",
        "rch_verify_retry_after_worker_disk_full",
        "rch_verify_worker_quarantine_ignored",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout_tail".to_owned())?;
    if !stdout_tail.contains("retrying once with RCH_WORKERS=css,trj") {
        return Err(format!("retry note missing from stdout tail: {report}"));
    }
    Ok(())
}

#[test]
fn synthetic_compile_error_is_not_worker_disk_full() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--lib", "support_bundle"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "error[E0277]: the trait bound `&str: Borrow<String>` is not satisfied\n  --> src/core/support_bundle.rs:1339:44\n[RCH] remote css failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "3000"),
        ],
    )?;
    if status.success() {
        return Err("compile-error transcript should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse compile: {error}"))?;
    if report["status"] != "remote_failure" {
        return Err(format!(
            "compile error should remain remote failure: {report}"
        ));
    }
    if degraded_contains(&report, "rch_verify_worker_disk_full")? {
        return Err(format!(
            "compile error was misclassified as disk full: {report}"
        ));
    }
    if report["first_error_file"] != "src/core/support_bundle.rs"
        || report["first_error_line"] != 1339
    {
        return Err(format!("compile error location not extracted: {report}"));
    }
    Ok(())
}

#[test]
fn synthetic_remote_transcript_writes_ledger_and_summary() -> TestResult {
    let dir = target_tmp_dir().join(format!("rch-verify-ledger-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let ledger = dir.join("runs.jsonl");
    let ledger_arg = ledger.display().to_string();
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--bead-id",
            "bd-test",
            "--ledger",
            &ledger_arg,
            "--summary",
            "--",
            "cargo",
            "test",
            "--test",
            "rch_verify_contract",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "error[E0425]: cannot find value `stderr` in this scope\n  --> tests/rch_verify_contract.rs:42:9\nremote test ok\n[RCH] remote css (1.0s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "1000"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "ledger run failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse report: {error}"))?;
    if report["status"] != "remote_pass" || report["bead_id"] != "bd-test" {
        return Err(format!("unexpected report status/bead: {report}"));
    }
    if report["command_hash"].as_str().map(str::len) != Some(64) {
        return Err(format!("missing sha256 command hash: {report}"));
    }
    if report["first_error_file"] != "tests/rch_verify_contract.rs"
        || report["first_error_line"] != 42
    {
        return Err(format!("first error location was not extracted: {report}"));
    }
    let error_codes = report["error_codes"]
        .as_array()
        .ok_or_else(|| "missing error codes".to_owned())?;
    if !error_codes.iter().any(|code| code == "E0425") {
        return Err(format!("missing rust error code: {report}"));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    if !summary.contains("worker_id: `css`")
        || !summary.contains("bead_id: `bd-test`")
        || !summary.contains("first_error: `tests/rch_verify_contract.rs:42`")
    {
        return Err(format!("summary missing expected fields: {summary}"));
    }

    let ledger_text =
        fs::read_to_string(&ledger).map_err(|error| format!("read ledger: {error}"))?;
    let rows = ledger_text.lines().collect::<Vec<_>>();
    if rows.len() != 1 {
        return Err(format!("expected one ledger row, got {}", rows.len()));
    }
    let row: Value =
        serde_json::from_str(rows[0]).map_err(|error| format!("parse ledger row: {error}"))?;
    if row["schema"] != "ee.rch.verify.ledger.v1"
        || row["status"] != "remote_pass"
        || row["worker_id"] != "css"
        || row["first_error_file"] != "tests/rch_verify_contract.rs"
        || row["first_error_line"] != 42
    {
        return Err(format!("unexpected ledger row: {row}"));
    }
    if row["command_hash"].as_str().map(str::len) != Some(64) {
        return Err(format!("ledger row missing command hash: {row}"));
    }
    Ok(())
}

#[test]
fn ledger_no_write_renders_summary_without_appending() -> TestResult {
    let dir = target_tmp_dir().join(format!("rch-verify-no-write-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let ledger = dir.join("runs.jsonl");
    let ledger_arg = ledger.display().to_string();
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--bead-id",
            "bd-test",
            "--ledger",
            &ledger_arg,
            "--summary",
            "--no-write",
            "--",
            "cargo",
            "test",
            "--test",
            "rch_verify_contract",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] local (dependency preflight RCH-E327: Path dependency topology policy failed.)\n[RCH] remote required; refusing local fallback (dependency preflight failed)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "20"),
        ],
    )?;
    if status.success() {
        return Err("no-write local fallback should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse report: {error}"))?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!("unexpected no-write status: {report}"));
    }
    let degraded = report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?;
    if !degraded
        .iter()
        .any(|code| code == "rch_verify_ledger_write_suppressed")
    {
        return Err(format!("missing no-write degradation: {report}"));
    }
    if ledger.exists() {
        return Err(format!(
            "no-write should not create ledger file; stderr was {stderr}"
        ));
    }
    Ok(())
}
