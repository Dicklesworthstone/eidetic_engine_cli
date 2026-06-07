//! bd-1h8ji.2 — RCHVC.2 local-cargo tripwire integration tests.
//!
//! Drives `scripts/check-local-cargo-tripwire.sh` against representative
//! command strings and asserts the JSON report (`ee.rch_local_cargo_tripwire.v1`)
//! classifies each one correctly under the bd-1h8ji.1 verifier contract:
//! direct `cargo build/check/test/bench/clippy` is denied unless wrapped
//! through `scripts/rch_verify.sh` or remote-required `rch exec`,
//! env-prefixed variants without a wrapper are still denied, the
//! `RCH_REQUIRE_REMOTE=1`-without-`rch exec` failure mode is surfaced
//! with a specific detail line, and non-compile cargo subcommands
//! (`metadata`, etc.) are allowed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use serde_json::Value;

type TestResult = Result<(), String>;

/// Emit a tracing checkpoint per the bd-3usjw.58 standard field set so
/// the closure-lint tracing-fields gate sees structured evidence in this
/// FILE SURFACE entry.
fn trace_local_cargo_tripwire(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "tests/rch_local_cargo_tripwire",
        request_id = "rch_local_cargo_tripwire_integration",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-1h8ji.2"),
        surface = "rch_local_cargo_tripwire",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "rch local-cargo tripwire checkpoint"
    );
}

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("check-local-cargo-tripwire.sh")
}

fn classify(command: &str) -> Result<(i32, Value), String> {
    let started = Instant::now();
    trace_local_cargo_tripwire("input", 0, &[]);
    let output = Command::new("sh")
        .arg(script_path())
        .arg("--cmd")
        .arg(command)
        .arg("--json")
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("parse json: {e}; stdout={stdout}"))?;
    let code = output.status.code().unwrap_or(-1);
    trace_local_cargo_tripwire(
        "response",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        &[],
    );
    Ok((code, parsed))
}

fn probe_with_ps_fixture(
    ps_fixture: &str,
    package_cache_pids: &str,
) -> Result<(i32, Value), String> {
    let fixture_path = std::env::temp_dir().join(format!(
        "ee-rch-local-cargo-tripwire-{}-{}.ps",
        std::process::id(),
        ps_fixture.len()
    ));
    fs::write(&fixture_path, ps_fixture).map_err(|e| format!("write ps fixture: {e}"))?;
    let output = Command::new("sh")
        .arg(script_path())
        .arg("--probe-processes")
        .arg("--ps-file")
        .arg(&fixture_path)
        .arg("--package-cache-pids")
        .arg(package_cache_pids)
        .arg("--json")
        .output()
        .map_err(|e| format!("spawn probe: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("parse probe json: {e}; stdout={stdout}"))?;
    Ok((output.status.code().unwrap_or(-1), parsed))
}

#[test]
fn direct_cargo_test_is_denied_with_exit_one() -> TestResult {
    let (code, report) = classify("cargo test --lib happy_path")?;
    if code != 1 {
        return Err(format!(
            "expected exit 1 for direct cargo test, got {code}; report={report}"
        ));
    }
    if report["schema"].as_str() != Some("ee.rch_local_cargo_tripwire.v1") {
        return Err(format!("unexpected schema: {report}"));
    }
    if report["allowed"].as_str() != Some("denied") {
        return Err(format!("expected allowed=denied, got {report}"));
    }
    if report["subcommand"].as_str() != Some("test") {
        return Err(format!("expected subcommand=test, got {report}"));
    }
    Ok(())
}

#[test]
fn direct_cargo_build_is_denied() -> TestResult {
    let (code, report) = classify("cargo build --release")?;
    if code != 1 || report["allowed"].as_str() != Some("denied") {
        return Err(format!("expected denied for cargo build; got {report}"));
    }
    if report["subcommand"].as_str() != Some("build") {
        return Err(format!("expected subcommand=build, got {report}"));
    }
    Ok(())
}

#[test]
fn direct_cargo_check_is_denied() -> TestResult {
    let (code, report) = classify("cargo check --all-targets")?;
    if code != 1 || report["allowed"].as_str() != Some("denied") {
        return Err(format!("expected denied for cargo check; got {report}"));
    }
    if report["subcommand"].as_str() != Some("check") {
        return Err(format!("expected subcommand=check, got {report}"));
    }
    Ok(())
}

#[test]
fn direct_cargo_bench_is_denied() -> TestResult {
    let (code, report) = classify("cargo bench --bench graph_minhash_rank")?;
    if code != 1 || report["allowed"].as_str() != Some("denied") {
        return Err(format!("expected denied for cargo bench; got {report}"));
    }
    if report["subcommand"].as_str() != Some("bench") {
        return Err(format!("expected subcommand=bench, got {report}"));
    }
    Ok(())
}

#[test]
fn direct_cargo_clippy_is_denied() -> TestResult {
    let (code, report) = classify("cargo clippy --all-targets -- -D warnings")?;
    if code != 1 || report["allowed"].as_str() != Some("denied") {
        return Err(format!("expected denied for cargo clippy; got {report}"));
    }
    if report["subcommand"].as_str() != Some("clippy") {
        return Err(format!("expected subcommand=clippy, got {report}"));
    }
    Ok(())
}

#[test]
fn bare_rch_exec_wrapper_is_denied_without_remote_required() -> TestResult {
    let (code, report) = classify("rch exec -- env TMPDIR=/tmp cargo test --lib foo")?;
    if code != 1 {
        return Err(format!(
            "expected exit 1 for bare rch exec wrapper, got {code}; report={report}"
        ));
    }
    if report["allowed"].as_str() != Some("denied") {
        return Err(format!("expected allowed=denied, got {report}"));
    }
    if report["subcommand"].as_str() != Some("rch_exec_without_remote_required") {
        return Err(format!(
            "expected bare-rch-exec subcommand marker, got {report}"
        ));
    }
    Ok(())
}

#[test]
fn remote_required_rch_exec_wrapper_is_allowed() -> TestResult {
    let (code, report) =
        classify("RCH_REQUIRE_REMOTE=1 rch exec -- env TMPDIR=/tmp cargo test --lib foo")?;
    if code != 0 || report["allowed"].as_str() != Some("allowed") {
        return Err(format!(
            "expected allowed for remote-required rch exec wrapper; got {report}"
        ));
    }
    Ok(())
}

#[test]
fn absolute_path_rch_exec_wrapper_is_allowed() -> TestResult {
    let (code, report) = classify(
        "RCH_REQUIRE_REMOTE=1 /Users/jemanuel/.local/bin/rch-manifestfix-20260605-5 exec -- env TMPDIR=/tmp cargo bench --bench foo",
    )?;
    if code != 0 || report["allowed"].as_str() != Some("allowed") {
        return Err(format!(
            "expected allowed for absolute-path remote-required rch wrapper; got {report}"
        ));
    }
    Ok(())
}

#[test]
fn rch_exec_with_json_flag_variant_is_allowed() -> TestResult {
    let (code, report) =
        classify("RCH_REQUIRE_REMOTE=1 rch exec --json -- env TMPDIR=/tmp cargo test --lib foo")?;
    if code != 0 || report["allowed"].as_str() != Some("allowed") {
        return Err(format!(
            "expected allowed for remote-required rch exec --json wrapper; got {report}"
        ));
    }
    Ok(())
}

#[test]
fn rch_diagnose_dry_run_payload_is_allowed() -> TestResult {
    let (code, report) = classify(
        "/Volumes/USBNVME16TB/temp_agent_space/rch-candidate/extracted/rch diagnose --dry-run \"cargo check --lib --quiet\" --json",
    )?;
    if code != 0 || report["allowed"].as_str() != Some("allowed") {
        return Err(format!(
            "expected allowed for read-only rch diagnose payload; got {report}"
        ));
    }
    Ok(())
}

#[test]
fn rch_require_remote_set_without_wrapper_is_denied_with_specific_detail() -> TestResult {
    // The exact bd-1h8ji.2 failure mode: the caller thought
    // RCH_REQUIRE_REMOTE=1 alone was enough to keep cargo remote, but
    // without `rch exec` in the command string, local Darwin cargo
    // started anyway.
    let (code, report) = classify("RCH_REQUIRE_REMOTE=1 cargo bench --bench graph_minhash_rank")?;
    if code != 1 || report["allowed"].as_str() != Some("denied") {
        return Err(format!(
            "expected denied for env-only RCH_REQUIRE_REMOTE; got {report}"
        ));
    }
    let detail = report["detail"].as_str().unwrap_or("");
    if !detail.contains("RCH_REQUIRE_REMOTE=1 was set but rch exec wrapper is absent") {
        return Err(format!(
            "expected specific bd-1h8ji.2 failure-mode detail; got {detail:?}"
        ));
    }
    Ok(())
}

#[test]
fn cargo_metadata_is_not_a_compile_subcommand_and_is_allowed() -> TestResult {
    let (code, report) = classify("cargo metadata --format-version 1")?;
    if code != 0 || report["allowed"].as_str() != Some("allowed") {
        return Err(format!("expected allowed for cargo metadata; got {report}"));
    }
    Ok(())
}

#[test]
fn empty_command_is_allowed() -> TestResult {
    let (code, report) = classify("")?;
    if code != 0 || report["allowed"].as_str() != Some("allowed") {
        return Err(format!("expected allowed for empty command; got {report}"));
    }
    Ok(())
}

#[test]
fn process_probe_reports_manifest_workspace_and_package_cache_lock() -> TestResult {
    let fixture = "\
 41311 1 01:12:03 cargo test --lib import_cursor --manifest-path /Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml
 7193 1 02:33:44 cargo metadata --format-version=1 --manifest-path /Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml
 8 1 00:00:01 cargo metadata --manifest-path /tmp/other/Cargo.toml
";
    let (code, report) = probe_with_ps_fixture(fixture, "7193")?;
    if code != 1 {
        return Err(format!(
            "expected probe exit 1 with local processes; got {code}: {report}"
        ));
    }
    if report["mode"] != "probe_processes" || report["status"] != "bypass_detected" {
        return Err(format!("unexpected probe envelope: {report}"));
    }
    let processes = report["processes"]
        .as_array()
        .ok_or_else(|| format!("missing processes array: {report}"))?;
    if processes.len() != 2 {
        return Err(format!("expected two eidetic processes, got {processes:?}"));
    }
    if processes[0]["pid"] != "7193" || processes[1]["pid"] != "41311" {
        return Err(format!("processes were not sorted by pid: {processes:?}"));
    }
    if processes[0]["manifestPath"] != "/Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml"
        || processes[0]["workspacePath"] != "/Users/jemanuel/projects/eidetic_engine_cli"
    {
        return Err(format!("manifest/workspace path missing: {}", processes[0]));
    }
    if processes[0]["packageCacheLockState"] != "held"
        || processes[0]["packageCacheLockHeld"] != true
        || processes[0]["policyStatus"] != "local_cargo_read_only_lock_holder"
    {
        return Err(format!(
            "package-cache lock holder not reported: {}",
            processes[0]
        ));
    }
    if processes[1]["policyStatus"] != "local_cargo_disallowed" {
        return Err(format!(
            "compile cargo process not classified: {}",
            processes[1]
        ));
    }
    Ok(())
}

#[test]
fn process_probe_ignores_rch_diagnose_dry_run_payloads() -> TestResult {
    let fixture = "\
 55381 613 00:02 /Volumes/USBNVME16TB/temp_agent_space/rch-candidate/extracted/rch diagnose --dry-run cargo check --manifest-path /Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml --json
 55382 613 00:02 cargo test --manifest-path /Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml
";
    let (code, report) = probe_with_ps_fixture(fixture, "")?;
    if code != 1 {
        return Err(format!(
            "expected probe exit 1 from the real local cargo process only; got {code}: {report}"
        ));
    }
    let processes = report["processes"]
        .as_array()
        .ok_or_else(|| format!("missing processes array: {report}"))?;
    if processes.len() != 1 || processes[0]["pid"] != "55382" {
        return Err(format!(
            "expected only direct cargo to be reported, got {processes:?}"
        ));
    }
    Ok(())
}

#[test]
fn process_probe_ignores_stable_rch_verify_shell_without_rch_exec_child() -> TestResult {
    let fixture = "\
 839 69255 00:00 bash -s -- --summary --no-write --known-blocker-override -- cargo test --manifest-path /Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml -- --nocapture
 990 1 00:03 cargo test --manifest-path /Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml
";
    let (code, report) = probe_with_ps_fixture(fixture, "")?;
    if code != 1 {
        return Err(format!(
            "expected probe exit 1 from the real local cargo process only; got {code}: {report}"
        ));
    }
    let processes = report["processes"]
        .as_array()
        .ok_or_else(|| format!("missing processes array: {report}"))?;
    if processes.len() != 1 || processes[0]["pid"] != "990" {
        return Err(format!(
            "expected only direct cargo to be reported, got {processes:?}"
        ));
    }
    Ok(())
}

#[test]
fn process_probe_ignores_ssh_remote_cargo_payload_launcher() -> TestResult {
    let fixture = "\
 90391 1694 00:51 ssh -i /Users/jemanuel/.ssh/contabo_vps_ed25519 -o BatchMode=yes ubuntu@212.90.121.76 cd /data/projects/eidetic_engine_cli_manual_turquoise-20260607T152024Z && mkdir -p .manual-target-data && TMPDIR=/tmp CARGO_TARGET_DIR=/data/projects/eidetic_engine_cli_manual_turquoise-20260607T152024Z/.manual-target-data cargo test --lib global_memory_scope -- --nocapture
 990 1 00:03 cargo test --manifest-path /Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml
";
    let (code, report) = probe_with_ps_fixture(fixture, "")?;
    if code != 1 {
        return Err(format!(
            "expected probe exit 1 from the real local cargo process only; got {code}: {report}"
        ));
    }
    let processes = report["processes"]
        .as_array()
        .ok_or_else(|| format!("missing processes array: {report}"))?;
    if processes.len() != 1 || processes[0]["pid"] != "990" {
        return Err(format!(
            "expected only direct cargo to be reported, got {processes:?}"
        ));
    }
    Ok(())
}

#[test]
fn self_test_subcommand_exits_zero_with_passed_marker() -> TestResult {
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
