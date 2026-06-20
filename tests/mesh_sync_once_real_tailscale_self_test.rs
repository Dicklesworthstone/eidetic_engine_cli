//! bd-36bbk.2 — self-test for `scripts/e2e_overhaul/mesh_sync_once_real_tailscale.sh`.
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
//!      with `mesh_tailscale_smoke.sh` / bd-1crtj and
//!      `auto_enroll_real_tailscale.sh` / bd-36bbk.1.11),
//!   2. write exactly one `ee.test_event.v1` skip event to the events file,
//!      and
//!   3. emit a skip reason that names `EE_E2E_REAL_TAILSCALE` so the
//!      operator can see how to opt in.
//!
//! Parallel to bd-1crtj (`scripts/e2e_overhaul/mesh_tailscale_smoke.sh`)
//! and bd-36bbk.1.11 (`scripts/e2e_overhaul/auto_enroll_real_tailscale.sh`).
//! The skip-shape contract is identical across these harnesses; if a future
//! refactor moves the shared logic into a helper this test still catches
//! drift from the agent-visible contract.
//!
//! Verification: this test launches the shell harness as a subprocess. It
//! does NOT depend on bash being installed on Windows — the test is gated
//! to unix targets only via `#[cfg(unix)]` on every test function.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn harness_path() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("e2e_overhaul")
        .join("mesh_sync_once_real_tailscale.sh")
}

/// Set up an isolated event-dir under the system tempdir so concurrent test
/// runs (cargo test multi-threaded, or the same test re-run while a prior
/// run's tempdir is still on disk) do not race on the same path.
fn isolated_event_dir(label: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!(
        "ee-mesh-sync-once-self-test-{}-{}-{}",
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
fn write_executable(path: &Path, body: &str) -> TestResult {
    fs::write(path, body).map_err(|error| format!("write {}: {error}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("stat {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod +x {}: {error}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn path_with_fake_bin(bin_dir: &Path) -> OsString {
    let mut path = OsString::from(bin_dir.as_os_str());
    path.push(":");
    path.push(env::var_os("PATH").unwrap_or_default());
    path
}

#[cfg(unix)]
fn write_fake_tailscale(bin_dir: &Path) -> TestResult {
    write_executable(
        &bin_dir.join("tailscale"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "status" ] && [ "${2:-}" = "--json" ]; then
cat <<'JSON'
{
  "BackendState": "Running",
  "CurrentTailnet": {
    "Name": "secret-tailnet-name",
    "MagicDNSSuffix": "secret-tailnet.ts.net"
  },
  "Self": {
    "Authenticated": true,
    "Online": true,
    "HostName": "self-secret-host",
    "DNSName": "self-secret.tailnet.test.",
    "TailscaleIPs": ["100.64.0.10"]
  },
  "Peer": {
    "nodekey:selectedSECRET": {
      "ID": "selected-id-secret",
      "HostName": "selected-host",
      "DNSName": "selected-host.tailnet.test.",
      "Online": true,
      "Relay": "sfo",
      "CurAddr": "203.0.113.9:41641",
      "TailscaleIPs": ["100.64.0.20"],
      "Tags": ["tag:selected-secret"]
    },
    "nodekey:otherSECRET": {
      "ID": "other-id-secret",
      "HostName": "other-host",
      "DNSName": "other-host.tailnet.test.",
      "Online": true,
      "Relay": "nyc",
      "CurAddr": "198.51.100.7:41641",
      "TailscaleIPs": ["100.64.0.30"],
      "Tags": ["tag:other-secret"]
    }
  }
}
JSON
else
    echo "unexpected tailscale args: $*" >&2
    exit 64
fi
"#,
    )
}

#[cfg(unix)]
fn write_fake_ee(bin_dir: &Path) -> TestResult {
    write_executable(
        &bin_dir.join("ee"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '{"schema":"ee.response.v2","success":true,"data":{"contactedPeers":true},"degraded":[]}\n'
"#,
    )
}

#[cfg(unix)]
fn assert_redacted_tailnet_artifacts(artifact_dir: &Path) -> TestResult {
    let status_path = artifact_dir.join("tailscale_status.json");
    let peer_path = artifact_dir.join("peer.json");
    let status_text = fs::read_to_string(&status_path)
        .map_err(|error| format!("read {}: {error}", status_path.display()))?;
    let peer_text = fs::read_to_string(&peer_path)
        .map_err(|error| format!("read {}: {error}", peer_path.display()))?;
    let retained = format!("{status_text}\n{peer_text}");

    for forbidden in [
        "nodekey:otherSECRET",
        "other-id-secret",
        "other-host",
        "other-host.tailnet.test",
        "100.64.0.30",
        "198.51.100.7",
        "tag:other-secret",
        "secret-tailnet-name",
        "secret-tailnet.ts.net",
    ] {
        if retained.contains(forbidden) {
            return Err(format!(
                "retained tailnet artifacts must redact {forbidden:?}; contents={retained}"
            ));
        }
    }

    let status_json: serde_json::Value = serde_json::from_str(&status_text)
        .map_err(|error| format!("parse {}: {error}", status_path.display()))?;
    let peer_json: serde_json::Value = serde_json::from_str(&peer_text)
        .map_err(|error| format!("parse {}: {error}", peer_path.display()))?;

    if status_json
        .get("Peer")
        .or_else(|| status_json.get("CurrentTailnet"))
        .is_some()
    {
        return Err(format!(
            "retained status must not preserve raw Peer or CurrentTailnet: {status_text}"
        ));
    }
    if status_json
        .pointer("/redacted")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(format!(
            "retained status must be marked redacted: {status_text}"
        ));
    }
    if status_json
        .pointer("/selectedPeer/recordHash")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .is_empty()
    {
        return Err(format!(
            "retained status must include selected peer recordHash: {status_text}"
        ));
    }
    if peer_json
        .pointer("/redacted")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(format!(
            "retained peer must be marked redacted: {peer_text}"
        ));
    }
    if peer_json
        .get("key")
        .or_else(|| peer_json.get("value"))
        .is_some()
    {
        return Err(format!(
            "retained peer artifact must not preserve raw key/value peer entry: {peer_text}"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn assert_sync_once_event_reports_contacted_peer(event_dir: &Path) -> TestResult {
    let event_file = event_dir.join("events.jsonl");
    let contents = fs::read_to_string(&event_file)
        .map_err(|error| format!("read {}: {error}", event_file.display()))?;

    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("parse event JSON: {error}; line={line}"))?;
        if value.get("phase").and_then(serde_json::Value::as_str) != Some("assert") {
            continue;
        }
        let contacted_peers = value
            .pointer("/fields/contactedPeers")
            .and_then(serde_json::Value::as_i64);
        if contacted_peers != Some(1) {
            return Err(format!(
                "assert event must normalize boolean contactedPeers to numeric 1; got {value}"
            ));
        }
        return Ok(());
    }

    Err(format!(
        "expected an assert event in {}; contents={contents:?}",
        event_file.display()
    ))
}

#[cfg(unix)]
#[test]
fn mesh_sync_once_real_tailscale_harness_exists_and_is_executable() -> TestResult {
    let path = harness_path();
    let metadata =
        fs::metadata(&path).map_err(|error| format!("stat {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    // POSIX execute bit on user/group/other. Mirrors how
    // mesh_tailscale_smoke.sh / auto_enroll_real_tailscale.sh are shipped
    // (chmod +x). Without this the operator cannot opt in.
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
fn mesh_sync_once_real_tailscale_refuses_tmp_fallback_without_opt_in() -> TestResult {
    let run_dir = isolated_event_dir("tmp-gate");
    let bin_dir = run_dir.join("bin");
    let event_dir = run_dir.join("events");
    let work_dir = run_dir.join("work");
    fs::create_dir_all(&bin_dir).map_err(|error| format!("create bin dir: {error}"))?;
    fs::create_dir_all(&event_dir).map_err(|error| format!("create event dir: {error}"))?;
    fs::create_dir_all(&work_dir).map_err(|error| format!("create work dir: {error}"))?;
    write_fake_tailscale(&bin_dir)?;
    write_fake_ee(&bin_dir)?;

    let output = Command::new("bash")
        .arg(harness_path())
        .env("PATH", path_with_fake_bin(&bin_dir))
        .env("EE_E2E_REAL_TAILSCALE", "1")
        .env("EE_TEST_EVENT_DIR", &event_dir)
        .env("EE_E2E_TMPDIR", &work_dir)
        .env("EE_BINARY", bin_dir.join("ee"))
        .env_remove("EE_E2E_ARTIFACT_DIR")
        .env_remove("EE_TAILNET_TMP_OK")
        .output()
        .map_err(|error| format!("spawn bash harness: {error}"))?;

    let code = output
        .status
        .code()
        .ok_or_else(|| "harness terminated by signal".to_string())?;
    if code != 78 {
        return Err(format!(
            "expected temp-artifact gate to exit 78; got {code}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("EE_E2E_ARTIFACT_DIR") || !stderr.contains("EE_TAILNET_TMP_OK") {
        return Err(format!(
            "tmp fallback skip must name the explicit artifact-dir and override env vars; stderr={stderr}"
        ));
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn mesh_sync_once_real_tailscale_fake_run_retains_redacted_tailnet_artifacts() -> TestResult {
    let run_dir = isolated_event_dir("fake-redaction");
    let bin_dir = run_dir.join("bin");
    let event_dir = run_dir.join("events");
    let artifact_dir = run_dir.join("artifacts");
    let work_dir = run_dir.join("work");
    fs::create_dir_all(&bin_dir).map_err(|error| format!("create bin dir: {error}"))?;
    fs::create_dir_all(&event_dir).map_err(|error| format!("create event dir: {error}"))?;
    fs::create_dir_all(&artifact_dir).map_err(|error| format!("create artifact dir: {error}"))?;
    fs::create_dir_all(&work_dir).map_err(|error| format!("create work dir: {error}"))?;
    write_fake_tailscale(&bin_dir)?;
    write_fake_ee(&bin_dir)?;

    let output = Command::new("bash")
        .arg(harness_path())
        .env("PATH", path_with_fake_bin(&bin_dir))
        .env("EE_E2E_REAL_TAILSCALE", "1")
        .env("EE_REAL_TAILSCALE_PEER", "selected-host")
        .env("EE_TEST_EVENT_DIR", &event_dir)
        .env("EE_E2E_ARTIFACT_DIR", &artifact_dir)
        .env("EE_E2E_TMPDIR", &work_dir)
        .env("EE_BINARY", bin_dir.join("ee"))
        .env_remove("EE_TAILNET_TMP_OK")
        .output()
        .map_err(|error| format!("spawn bash harness: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "expected fake real-tailnet run to pass; status={:?}; stdout={}; stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    assert_redacted_tailnet_artifacts(&artifact_dir)?;
    assert_sync_once_event_reports_contacted_peer(&event_dir)
}

#[cfg(unix)]
#[test]
fn mesh_sync_once_real_tailscale_skips_cleanly_without_opt_in_env() -> TestResult {
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
    if bead != "bd-36bbk.2" {
        return Err(format!("expected bead bd-36bbk.2; got {bead:?}"));
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

    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if kind != "mesh_sync_once_real_tailscale" {
        return Err(format!(
            "expected event kind mesh_sync_once_real_tailscale; got {kind:?}"
        ));
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn mesh_sync_once_real_tailscale_passes_bash_n_parse_check() -> TestResult {
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
