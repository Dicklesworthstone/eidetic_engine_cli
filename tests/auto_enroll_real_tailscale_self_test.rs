//! bd-36bbk.1.11 — self-test for `scripts/e2e_overhaul/auto_enroll_real_tailscale.sh`.
//!
//! The opt-in harness itself only exercises real Tailscale code paths when
//! `EE_E2E_REAL_TAILSCALE=1` is set. This self-test asserts the
//! always-runnable shape — the skip-clean precondition — so the harness's
//! default behavior (which is what CI sees) cannot regress without a test
//! failure.
//!
//! Specifically: when `EE_E2E_REAL_TAILSCALE` is absent or non-1, the
//! harness must:
//!
//!   1. exit with code 78 (the canonical skipped-precondition exit shared
//!      with `mesh_tailscale_smoke.sh` / bd-1crtj),
//!   2. write exactly one `ee.test_event.v1` skip event to the events file,
//!      and
//!   3. emit a skip reason that names `EE_E2E_REAL_TAILSCALE` so the
//!      operator can see how to opt in.
//!
//! Parallel to bd-1crtj (`scripts/e2e_overhaul/mesh_tailscale_smoke.sh`)
//! which carries the same skip-shape contract. Keeping the contract pinned
//! in a Rust self-test (rather than only in a shell-only check) means the
//! cargo test gate catches a regression even when the shell-test gate is
//! skipped.
//!
//! Verification: this test launches the shell harness as a subprocess. It
//! does NOT depend on bash being installed on Windows — the test is gated
//! to unix targets only via `#[cfg(unix)]` on every test function.

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
        .join("auto_enroll_real_tailscale.sh")
}

/// Set up an isolated event-dir under the system tempdir so concurrent test
/// runs (cargo test multi-threaded, or the same test re-run while a prior
/// run's tempdir is still on disk) do not race on the same path.
fn isolated_event_dir(label: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!(
        "ee-auto-enroll-self-test-{}-{}-{}",
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
fn auto_enroll_real_tailscale_harness_exists_and_is_executable() -> TestResult {
    let path = harness_path();
    let metadata =
        fs::metadata(&path).map_err(|error| format!("stat {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    // POSIX execute bit on user/group/other. Mirrors how mesh_tailscale_smoke.sh
    // is shipped (chmod +x). Without this the operator cannot opt in.
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
fn auto_enroll_real_tailscale_skips_cleanly_without_opt_in_env() -> TestResult {
    let event_dir = isolated_event_dir("skip");
    fs::create_dir_all(&event_dir)
        .map_err(|error| format!("create {}: {error}", event_dir.display()))?;

    let output = Command::new("bash")
        .arg(harness_path())
        // Explicitly clear the opt-in env var so a parent shell that has it
        // set never makes this test attempt a real Tailscale run.
        .env_remove("EE_E2E_REAL_TAILSCALE")
        // Route the harness's events file to our isolated dir so the
        // assertion below reads exactly the events this invocation wrote.
        .env("EE_TEST_EVENT_DIR", &event_dir)
        // Defang any other test-env paths the operator might have set so
        // the harness's precondition is the FIRST gate the test trips.
        .env_remove("EE_REAL_TAILSCALE_PEER")
        .env_remove("EE_BINARY")
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

    // The harness prints the event-file path to stdout on the skip path so
    // an outer test runner can read it back. Read the file directly via the
    // env-dir we just set.
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
    if bead != "bd-36bbk.1.11" {
        return Err(format!("expected bead bd-36bbk.1.11; got {bead:?}"));
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
    if !message.contains("EE_E2E_REAL_TAILSCALE") {
        return Err(format!(
            "skip message must name the EE_E2E_REAL_TAILSCALE opt-in env var; got {message:?}"
        ));
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn auto_enroll_real_tailscale_passes_bash_n_parse_check() -> TestResult {
    // Independent of the runtime skip-shape test above: confirms `bash -n`
    // accepts the script. Catches syntax regressions that the runtime test
    // would also catch but only after a fork+exec; this fails earlier and
    // with a clearer error message.
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
