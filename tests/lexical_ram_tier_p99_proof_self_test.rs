//! bd-21xbi.3 — self-test for
//! `scripts/e2e_overhaul/lexical_ram_tier_p99_proof.sh`.
//!
//! The opt-in benchmark gate only exercises the real lexical RAM-tier
//! optimization when `EE_HUGE_HOST=1` is set AND the host has >= 256 GiB
//! RAM + 64 cores + Linux. This self-test asserts the always-runnable
//! shape — the skip-clean precondition that CI sees by default — so
//! the gate's default behavior cannot regress without a test failure.
//!
//! Specifically: when `EE_HUGE_HOST` is absent or non-1, the harness
//! must:
//!
//!   1. exit with code 78 (the canonical skipped-precondition exit
//!      shared with the other opt-in harnesses in this directory),
//!   2. write exactly one `ee.test_event.v1` skip event to the events
//!      file, and
//!   3. emit a skip reason that names `EE_HUGE_HOST` so the operator
//!      can see how to opt in.
//!
//! Parallel to bd-36bbk.1.11's
//! `auto_enroll_real_tailscale_self_test.rs` and bd-1crtj's tailnet
//! smoke — these opt-in harnesses share a skip-shape contract, and
//! pinning it in cargo test means the regular test gate catches a
//! regression even when shell-only checks are skipped.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn harness_path() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("e2e_overhaul")
        .join("lexical_ram_tier_p99_proof.sh")
}

fn isolated_event_dir(label: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!(
        "ee-lexical-ram-tier-self-test-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    path
}

#[cfg(unix)]
#[test]
fn lexical_ram_tier_p99_proof_harness_exists_and_is_executable() -> TestResult {
    let path = harness_path();
    let metadata =
        fs::metadata(&path).map_err(|error| format!("stat {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        return Err(format!(
            "{} must have at least one execute bit set (mode is {:o})",
            path.display(),
            mode
        ));
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn lexical_ram_tier_p99_proof_skips_cleanly_without_opt_in_env() -> TestResult {
    let event_dir = isolated_event_dir("skip");
    fs::create_dir_all(&event_dir)
        .map_err(|error| format!("create {}: {error}", event_dir.display()))?;

    let output = Command::new("bash")
        .arg(harness_path())
        // Explicitly clear the opt-in env var so a parent shell that
        // has it set never makes this test attempt a real run.
        .env_remove("EE_HUGE_HOST")
        // Route the harness's events file to our isolated dir so the
        // assertion below reads exactly the events this invocation
        // wrote.
        .env("EE_TEST_EVENT_DIR", &event_dir)
        // Defang any other test-env paths so the EE_HUGE_HOST gate is
        // the FIRST gate the test trips, not a downstream precondition.
        .env_remove("EE_BINARY")
        .env_remove("EE_LEXICAL_RAM_TIER_SEED_LOADER")
        .output()
        .map_err(|error| format!("spawn bash harness: {error}"))?;

    let code = output
        .status
        .code()
        .ok_or_else(|| "harness terminated by signal".to_string())?;
    if code != 78 {
        return Err(format!(
            "expected skip exit 78; got {code}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let event_file = event_dir.join("events.jsonl");
    let contents = fs::read_to_string(&event_file)
        .map_err(|error| format!("read {}: {error}", event_file.display()))?;
    let lines: Vec<&str> = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() != 1 {
        return Err(format!(
            "expected exactly one event line on skip; got {}; contents={contents:?}",
            lines.len()
        ));
    }
    let value: serde_json::Value = serde_json::from_str(lines[0])
        .map_err(|error| format!("parse event JSON: {error}; line={}", lines[0]))?;

    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if schema != "ee.test_event.v1" {
        return Err(format!("expected schema ee.test_event.v1; got {schema:?}"));
    }

    let bead = value
        .get("bead")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if bead != "bd-21xbi.3" {
        return Err(format!("expected bead bd-21xbi.3; got {bead:?}"));
    }

    let status = value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if status != "skipped" {
        return Err(format!("expected status skipped; got {status:?}"));
    }

    let message = value
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !message.contains("EE_HUGE_HOST") {
        return Err(format!(
            "skip message must name the EE_HUGE_HOST opt-in env var; got {message:?}"
        ));
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn lexical_ram_tier_p99_proof_passes_bash_n_parse_check() -> TestResult {
    let output = Command::new("bash")
        .arg("-n")
        .arg(harness_path())
        .output()
        .map_err(|error| format!("spawn bash -n: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "bash -n rejected the harness; status={:?}; stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
