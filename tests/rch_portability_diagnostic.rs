//! bd-1h8ji.4 — RCHVC.4 remote-portability diagnostic harness.
//!
//! Drives `scripts/check-rch-portability.sh` against synthetic transcripts
//! and asserts the JSON report (`ee.rch_remote_portability.v1`) flags every
//! documented portability anomaly: Mac-only Rust target triples, `/Volumes/`
//! USB scratch paths, `/var/folders/` TMPDIR leakage, AppleDouble `._*`
//! files in compile output, and `.DS_Store` metadata. Clean transcripts
//! must produce `status=ok` with `count=0`. The diagnostic is read-only;
//! these tests exercise that contract end-to-end through the actual script.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::Value;

type TestResult = Result<(), String>;

/// Emit a tracing checkpoint per the bd-3usjw.58 standard field set so
/// the closure-lint tracing-fields gate sees structured evidence in this
/// FILE SURFACE entry. Mirrors the `trace_*` helpers other Part II beads
/// use under tests/.
fn trace_rch_portability(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "tests/rch_portability_diagnostic",
        request_id = "rch_portability_diagnostic_integration",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-1h8ji.4"),
        surface = "rch_remote_portability",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "rch portability diagnostic checkpoint"
    );
}

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("check-rch-portability.sh")
}

/// Pipe a transcript into the script via stdin and parse the JSON report
/// from stdout. Returns `(exit_code, parsed_report)`.
fn run_script_with_stdin(transcript: &str) -> Result<(i32, Value), String> {
    let started = Instant::now();
    trace_rch_portability("input", 0, &[]);
    let mut child = Command::new("sh")
        .arg(script_path())
        .arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "no stdin handle".to_owned())?;
        stdin
            .write_all(transcript.as_bytes())
            .map_err(|e| format!("write stdin: {e}"))?;
    }
    let output = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value =
        serde_json::from_str(stdout.trim()).map_err(|e| format!("parse json: {e}: {stdout}"))?;
    let code = output.status.code().unwrap_or(-1);
    trace_rch_portability(
        "response",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        &[],
    );
    Ok((code, parsed))
}

fn anomaly_codes(report: &Value) -> Vec<String> {
    report
        .get("anomalies")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.get("code").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn clean_transcript_reports_ok_and_zero_anomalies() -> TestResult {
    let transcript = "remote command: cargo test --lib happy_path\nFinished test [unoptimized + debuginfo]\nrunning 1 test\ntest happy_path ... ok\ntest result: ok. 1 passed; 0 failed\n";
    let (code, report) = run_script_with_stdin(transcript)?;
    if code != 0 {
        return Err(format!(
            "expected exit 0 for clean transcript, got {code}; report={report}"
        ));
    }
    if report["schema"].as_str() != Some("ee.rch_remote_portability.v1") {
        return Err(format!("unexpected schema: {report}"));
    }
    if report["status"].as_str() != Some("ok") {
        return Err(format!("expected status=ok, got {report}"));
    }
    if report["count"].as_u64() != Some(0) {
        return Err(format!("expected count=0, got {report}"));
    }
    Ok(())
}

#[test]
fn darwin_target_triple_is_flagged() -> TestResult {
    let transcript = "remote command: cargo build --target arm64-apple-darwin22.0\n";
    let (code, report) = run_script_with_stdin(transcript)?;
    if code == 0 {
        return Err(format!(
            "expected non-zero exit for darwin target, got 0; report={report}"
        ));
    }
    let codes = anomaly_codes(&report);
    if !codes.iter().any(|c| c == "rch_portability_darwin_target") {
        return Err(format!("missing darwin_target code; got {codes:?}"));
    }
    Ok(())
}

#[test]
fn usb_volume_path_is_flagged() -> TestResult {
    let transcript = "syncing /Volumes/USBNVME16TB/temp_agent_space/cargo-target -> /data/projects/eidetic_engine_cli\n";
    let (code, report) = run_script_with_stdin(transcript)?;
    if code == 0 {
        return Err(format!(
            "expected non-zero exit for USB volume path, got 0; report={report}"
        ));
    }
    let codes = anomaly_codes(&report);
    if !codes.iter().any(|c| c == "rch_portability_usb_volume") {
        return Err(format!("missing usb_volume code; got {codes:?}"));
    }
    Ok(())
}

#[test]
fn appledouble_metadata_in_compile_output_is_flagged() -> TestResult {
    let transcript = "warning: vendor/zstd-sys/c/._zstd.c is an AppleDouble file the C compiler tried to parse\n";
    let (code, report) = run_script_with_stdin(transcript)?;
    if code == 0 {
        return Err(format!(
            "expected non-zero exit for AppleDouble metadata, got 0; report={report}"
        ));
    }
    let codes = anomaly_codes(&report);
    if !codes
        .iter()
        .any(|c| c == "rch_portability_appledouble_compile")
    {
        return Err(format!("missing appledouble_compile code; got {codes:?}"));
    }
    Ok(())
}

#[test]
fn var_folders_tmpdir_leak_is_flagged() -> TestResult {
    let transcript =
        "TMPDIR=/var/folders/abc/xyz123/T/cargo-build-xxxx\nremote exec: cargo test --lib\n";
    let (code, report) = run_script_with_stdin(transcript)?;
    if code == 0 {
        return Err(format!(
            "expected non-zero exit for var/folders TMPDIR, got 0; report={report}"
        ));
    }
    let codes = anomaly_codes(&report);
    if !codes.iter().any(|c| c == "rch_portability_var_folders_tmp") {
        return Err(format!("missing var_folders_tmp code; got {codes:?}"));
    }
    Ok(())
}

#[test]
fn ds_store_file_is_flagged() -> TestResult {
    let transcript = "rsync transfer: tests/fixtures/.DS_Store -> /data/projects/eidetic_engine_cli/tests/fixtures/.DS_Store\n";
    let (code, report) = run_script_with_stdin(transcript)?;
    if code == 0 {
        return Err(format!(
            "expected non-zero exit for .DS_Store, got 0; report={report}"
        ));
    }
    let codes = anomaly_codes(&report);
    if !codes.iter().any(|c| c == "rch_portability_ds_store") {
        return Err(format!("missing ds_store code; got {codes:?}"));
    }
    Ok(())
}

#[test]
fn combined_anomalies_aggregate_in_one_report() -> TestResult {
    let transcript = "cargo build --target arm64-apple-darwin\n\
                      syncing /Volumes/USBNVME16TB/temp\n\
                      warning: vendor/c/._bad.c\n\
                      TMPDIR=/var/folders/aa/bb\n\
                      transfer: foo/.DS_Store\n";
    let (code, report) = run_script_with_stdin(transcript)?;
    if code == 0 {
        return Err(format!(
            "expected non-zero exit for combined anomalies, got 0; report={report}"
        ));
    }
    let codes = anomaly_codes(&report);
    let expected = [
        "rch_portability_darwin_target",
        "rch_portability_usb_volume",
        "rch_portability_appledouble_compile",
        "rch_portability_var_folders_tmp",
        "rch_portability_ds_store",
    ];
    for needle in expected {
        if !codes.iter().any(|c| c == needle) {
            return Err(format!(
                "combined transcript missing code {needle}; got {codes:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn self_test_subcommand_exits_zero_with_expected_count() -> TestResult {
    let output = Command::new("sh")
        .arg(script_path())
        .arg("--self-test")
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "--self-test should exit 0; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("self-test PASSED") {
        return Err(format!("expected PASSED marker; stdout={stdout}"));
    }
    Ok(())
}
