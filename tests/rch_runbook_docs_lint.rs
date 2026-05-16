//! bd-1h8ji.8 — RCH runbook docs-lint and copy-paste smoke coverage.
//!
//! The real lint/extractor lives in `scripts/check-rch-doc-examples.py`; the
//! shell e2e driver executes the exact dry-run command extracted from docs. The
//! tests keep both surfaces pinned without running local Cargo themselves.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn doc_lint_script() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("check-rch-doc-examples.py")
}

fn e2e_script() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("e2e_overhaul")
        .join("rch_runbook_docs_smoke.sh")
}

fn trace_rch_docs(phase: &'static str, status: &'static str, elapsed_ms: u64) {
    tracing::info!(
        workspace_id = "tests/rch_runbook_docs_lint",
        request_id = "rch_runbook_docs_lint",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-1h8ji.8"),
        surface = "rch_doc_examples",
        phase,
        status,
        elapsed_ms,
        command_hash = "",
        source_file = "",
        degraded_codes = ?Vec::<String>::new(),
        "rch docs lint checkpoint"
    );
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_doc_lint(args: &[&str]) -> Result<(std::process::ExitStatus, String, String), String> {
    let output = Command::new("python3")
        .arg(doc_lint_script())
        .args(args)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run doc lint: {error}"))?;
    Ok((
        output.status,
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn parse_json(stdout: &str, context: &str) -> Result<Value, String> {
    serde_json::from_str(stdout.trim())
        .map_err(|error| format!("{context}: parse JSON: {error}; stdout={stdout}"))
}

#[test]
fn docs_lint_report_covers_required_files_and_has_no_denials() -> TestResult {
    let started = Instant::now();
    trace_rch_docs("input", "start", 0);
    let (status, stdout, stderr) = run_doc_lint(&["--json"])?;
    trace_rch_docs(
        "response",
        "done",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    ensure(
        status.success(),
        format!("doc lint failed: stdout={stdout}; stderr={stderr}"),
    )?;
    let report = parse_json(&stdout, "lint report")?;
    ensure(
        report["schema"] == "ee.rch_doc_examples.v1",
        format!("unexpected schema: {report}"),
    )?;
    ensure(
        report["status"] == "ok",
        format!("lint did not pass: {report}"),
    )?;
    ensure(
        report["denied_count"].as_u64() == Some(0),
        format!("expected zero denials: {report}"),
    )?;
    ensure(
        report["command_count"].as_u64().unwrap_or(0) > 0,
        format!("expected lint to inspect commands: {report}"),
    )?;
    let paths = report["checked_files"]
        .as_array()
        .ok_or_else(|| format!("checked_files missing: {report}"))?
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect::<BTreeSet<_>>();
    for required in [
        "docs/rch_runbook.md",
        "docs/rch_verification.md",
        "AGENTS.md",
        "README.md",
    ] {
        ensure(
            paths.contains(required),
            format!("missing checked file {required}: {report}"),
        )?;
    }
    Ok(())
}

#[test]
fn self_test_covers_allowed_denied_skipped_and_smoke_shapes() -> TestResult {
    let (status, stdout, stderr) = run_doc_lint(&["--self-test"])?;
    ensure(
        status.success(),
        format!("self-test failed: stdout={stdout}; stderr={stderr}"),
    )?;
    ensure(
        stdout.contains("self-test passed"),
        format!("unexpected self-test stdout: {stdout}"),
    )?;
    Ok(())
}

#[test]
fn smoke_command_is_extracted_from_real_docs() -> TestResult {
    let (status, stdout, stderr) = run_doc_lint(&["--extract-smoke-command", "--json"])?;
    ensure(
        status.success(),
        format!("extract smoke command failed: stdout={stdout}; stderr={stderr}"),
    )?;
    let payload = parse_json(&stdout, "smoke command")?;
    ensure(
        payload["schema"] == "ee.rch_doc_examples.smoke_command.v1",
        format!("unexpected smoke schema: {payload}"),
    )?;
    let command = payload["command"]
        .as_str()
        .ok_or_else(|| format!("missing command: {payload}"))?;
    ensure(
        command.contains("scripts/rch_verify.sh --dry-run -- cargo"),
        format!("smoke command is not the dry-run wrapper: {payload}"),
    )?;
    ensure(
        payload["source_file"].as_str() == Some("docs/rch_verification.md"),
        format!("unexpected smoke source file: {payload}"),
    )?;
    ensure(
        payload["command_hash"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        format!("missing command hash: {payload}"),
    )?;
    Ok(())
}

#[test]
fn shell_e2e_driver_emits_phase_logs_and_final_summary() -> TestResult {
    let output = Command::new("bash")
        .arg(e2e_script())
        .env("RCH_BIN", "rch")
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run e2e driver: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "e2e driver failed with {:?}\nstdout={}\nstderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut phases = BTreeSet::new();
    let mut saw_summary = false;
    let mut saw_dry_run_assert = false;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value = serde_json::from_str(line)
            .map_err(|error| format!("parse e2e event: {error}; {line}"))?;
        ensure(
            event["schema"] == "ee.test_event.v1",
            format!("unexpected event schema: {event}"),
        )?;
        ensure(
            event["surface"] == "rch_doc_examples",
            format!("unexpected event surface: {event}"),
        )?;
        if let Some(phase) = event["phase"].as_str() {
            phases.insert(phase.to_owned());
        }
        if event["phase"] == "assert" && event["status"] == "dry_run_proof_validated" {
            saw_dry_run_assert = true;
            ensure(
                event["normalized_command"]
                    .as_str()
                    .is_some_and(|command| command.contains("scripts/rch_verify.sh --dry-run")),
                format!("assert event did not record the extracted command: {event}"),
            )?;
        }
        if event["phase"] == "summary" && event["status"] == "pass" {
            saw_summary = true;
        }
    }
    for required in ["setup", "action", "assert", "cleanup", "summary"] {
        ensure(
            phases.contains(required),
            format!("e2e driver did not emit phase {required}; stdout={stdout}"),
        )?;
    }
    ensure(
        saw_dry_run_assert,
        format!("missing dry-run assert event; stdout={stdout}"),
    )?;
    ensure(
        saw_summary,
        format!("missing pass summary event; stdout={stdout}"),
    )?;
    Ok(())
}
