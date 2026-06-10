//! Tests for the Agent Mail fallback health script.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("swarm_coordination_health.sh")
}

fn write_executable(path: &Path, body: &str) -> TestResult {
    fs::write(path, body).map_err(|error| format!("write {}: {error}", path.display()))?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

#[test]
fn health_script_reports_agent_mail_panic_and_fallback() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fake_bin = tempdir.path().join("bin");
    fs::create_dir_all(&fake_bin).map_err(|error| error.to_string())?;
    write_executable(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
exit 7
"#,
    )?;
    write_executable(
        &fake_bin.join("am"),
        r#"#!/usr/bin/env bash
if [ "$1" = "agents" ] && [ "$2" = "list" ]; then
  printf '{"agents":[]}\n'
  exit 0
fi
if [ "$1" = "mail" ] && [ "$2" = "send" ]; then
  for arg in "$@"; do
    if [ "$arg" = "AgentA,AgentB" ]; then
      printf 'thread main panicked at fsqlite-core: RefCell already borrowed\n' >&2
      exit 101
    fi
  done
  printf '{"sent":true}\n'
  exit 0
fi
exit 2
"#,
    )?;

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(script_path())
        .env("PATH", path)
        .env("AGENT_MAIL_PROJECT", tempdir.path())
        .env("AGENT_MAIL_FROM", "AgentA")
        .env("AGENT_MAIL_SINGLE_TO", "AgentA")
        .env("AGENT_MAIL_MULTI_TO", "AgentA,AgentB")
        .output()
        .map_err(|error| format!("run health script: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "health script should exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout JSON parse failed: {error}"))?;

    ensure(
        value["schema"] == "ee.swarm.coordination_health.v1",
        "schema should identify coordination health event",
    )?;
    ensure(
        value["mcp_http_reachable"] == false,
        "fake curl should report MCP HTTP unreachable",
    )?;
    ensure(
        value["am_agents_list_ok"] == true,
        "fake am agents list should succeed",
    )?;
    ensure(
        value["am_send_single_recipient_ok"] == true,
        "fake single-recipient send should succeed",
    )?;
    ensure(
        value["am_send_multi_recipient_ok"] == false,
        "fake multi-recipient send should fail",
    )?;
    ensure(
        value["observed_panic"] == "RefCell already borrowed",
        "panic excerpt should be captured",
    )?;
    ensure(
        value["fallback_active"] == true,
        "fallback should be active",
    )
}

#[test]
fn health_script_preserves_redacted_semantic_readiness_failure() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fake_bin = tempdir.path().join("bin");
    fs::create_dir_all(&fake_bin).map_err(|error| error.to_string())?;
    write_executable(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
printf '%s\n' '{"health_level":"green","semantic_readiness":{"status":"fail","detail":"open sqlite file /Users/jemanuel/.local/share/mcp_agent_mail_rust/storage.sqlite3: database disk image is malformed: failed to parse B-tree page 283"}}'
"#,
    )?;
    write_executable(
        &fake_bin.join("am"),
        r#"#!/usr/bin/env bash
if [ "$1" = "agents" ] && [ "$2" = "list" ]; then
  printf '{"agents":[]}\n'
  exit 0
fi
if [ "$1" = "mail" ] && [ "$2" = "send" ]; then
  printf '{"sent":true}\n'
  exit 0
fi
exit 2
"#,
    )?;

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(script_path())
        .env("PATH", path)
        .env("AGENT_MAIL_PROJECT", tempdir.path())
        .env("AGENT_MAIL_FROM", "AgentA")
        .env("AGENT_MAIL_SINGLE_TO", "AgentA")
        .env("AGENT_MAIL_MULTI_TO", "AgentA,AgentB")
        .output()
        .map_err(|error| format!("run health script: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "health script should exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout JSON parse failed: {error}; stdout: {stdout}"))?;

    ensure(
        value["mcp_http_reachable"] == true,
        "fake curl should report MCP HTTP reachable",
    )?;
    ensure(
        value["health_level"] == "green",
        "bounded health level should be preserved",
    )?;
    ensure(
        value["semantic_readiness"]["status"] == "fail",
        "semantic readiness status should be preserved",
    )?;
    ensure(
        value["semantic_readiness"]["reason"] == "malformed_sqlite",
        "raw malformed SQLite detail should be classified",
    )?;
    ensure(
        value["fallback_active"] == true,
        "semantic readiness failure should activate fallback posture",
    )?;

    for forbidden in ["/Users/jemanuel", "storage.sqlite3", "B-tree", "page 283"] {
        ensure(
            !stdout.contains(forbidden),
            format!("health snapshot leaked raw semantic-readiness detail: {forbidden}"),
        )?;
    }

    Ok(())
}

#[test]
fn health_script_treats_recovery_corrupt_as_fallback_even_when_green() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fake_bin = tempdir.path().join("bin");
    fs::create_dir_all(&fake_bin).map_err(|error| error.to_string())?;
    write_executable(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
printf '%s\n' '{"health_level":"green","semantic_readiness":{"status":"ok"},"recovery":{"mode":"corrupt","next_action":"Run am doctor repair --yes or restore from /Users/jemanuel/.local/share/mcp_agent_mail_rust/storage.sqlite3 after B-tree page 283 failed","bundle_path":"/Users/jemanuel/.local/share/mcp_agent_mail_rust/doctor/forensics/storage.sqlite3/reconstruct-20260602_030410_115"}}'
"#,
    )?;
    write_executable(
        &fake_bin.join("am"),
        r#"#!/usr/bin/env bash
if [ "$1" = "agents" ] && [ "$2" = "list" ]; then
  printf '{"agents":[]}\n'
  exit 0
fi
if [ "$1" = "mail" ] && [ "$2" = "send" ]; then
  printf '{"sent":true}\n'
  exit 0
fi
exit 2
"#,
    )?;

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(script_path())
        .env("PATH", path)
        .env("AGENT_MAIL_PROJECT", tempdir.path())
        .env("AGENT_MAIL_FROM", "AgentA")
        .env("AGENT_MAIL_SINGLE_TO", "AgentA")
        .env("AGENT_MAIL_MULTI_TO", "AgentA,AgentB")
        .output()
        .map_err(|error| format!("run health script: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "health script should exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout JSON parse failed: {error}; stdout: {stdout}"))?;

    ensure(
        value["mcp_http_reachable"] == true,
        "fake curl should report MCP HTTP reachable",
    )?;
    ensure(
        value["am_agents_list_ok"] == true,
        "fake am agents list should succeed",
    )?;
    ensure(
        value["am_send_single_recipient_ok"] == true,
        "fake single-recipient send should succeed",
    )?;
    ensure(
        value["am_send_multi_recipient_ok"] == true,
        "fake multi-recipient send should succeed",
    )?;
    ensure(
        value["health_level"] == "green",
        "bounded health level should be preserved",
    )?;
    ensure(
        value["semantic_readiness"]["status"] == "pass",
        "semantic readiness ok should be normalized to pass",
    )?;
    ensure(
        value["recovery"]["mode"] == "corrupt",
        "corrupt recovery mode should be preserved as bounded status",
    )?;
    ensure(
        value["recovery"]["reason"] == "archive_corruption",
        "corrupt recovery mode should be classified without raw paths",
    )?;
    ensure(
        value["fallback_active"] == true,
        "corrupt recovery mode should activate fallback posture",
    )?;

    for forbidden in [
        "/Users/jemanuel",
        "storage.sqlite3",
        "B-tree",
        "page 283",
        "reconstruct-20260602_030410_115",
    ] {
        ensure(
            !stdout.contains(forbidden),
            format!("health snapshot leaked raw recovery detail: {forbidden}"),
        )?;
    }

    Ok(())
}

#[test]
fn health_script_parses_degraded_http_health_body_without_path_leaks() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fake_bin = tempdir.path().join("bin");
    fs::create_dir_all(&fake_bin).map_err(|error| error.to_string())?;
    write_executable(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
printf '%s\n' '{"status":"degraded","version":"0.2.51","database_path":"storage.sqlite3","project_count":13,"message_count":9256,"durability_state":"corrupt","detail":"open /Users/jemanuel/.local/share/mcp_agent_mail_rust/storage.sqlite3 failed at B-tree page 283"}'
printf '%s\n' '__EE_HTTP_STATUS__:503'
"#,
    )?;
    write_executable(
        &fake_bin.join("am"),
        r#"#!/usr/bin/env bash
if [ "$1" = "agents" ] && [ "$2" = "list" ]; then
  printf '{"agents":[]}\n'
  exit 0
fi
if [ "$1" = "mail" ] && [ "$2" = "send" ]; then
  printf '{"sent":true}\n'
  exit 0
fi
exit 2
"#,
    )?;

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(script_path())
        .env("PATH", path)
        .env("AGENT_MAIL_PROJECT", tempdir.path())
        .env("AGENT_MAIL_FROM", "AgentA")
        .env("AGENT_MAIL_SINGLE_TO", "AgentA")
        .env("AGENT_MAIL_MULTI_TO", "AgentA,AgentB")
        .output()
        .map_err(|error| format!("run health script: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "health script should exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout JSON parse failed: {error}; stdout: {stdout}"))?;

    ensure(
        value["mcp_http_reachable"] == true,
        "HTTP health body should count as reachable transport",
    )?;
    ensure(
        value["checks"]["mcp_http"]["http_status"] == 503,
        "HTTP status should be preserved as bounded numeric evidence",
    )?;
    ensure(
        value["recovery"]["mode"] == "corrupt",
        "durability_state=corrupt should be treated as recovery evidence",
    )?;
    ensure(
        value["recovery"]["reason"] == "archive_corruption",
        "corrupt durability state should be classified",
    )?;
    ensure(
        value["fallback_active"] == true,
        "degraded health body should activate fallback posture",
    )?;

    for forbidden in ["/Users/jemanuel", "storage.sqlite3", "B-tree", "page 283"] {
        ensure(
            !stdout.contains(forbidden),
            format!("health snapshot leaked raw degraded health detail: {forbidden}"),
        )?;
    }

    Ok(())
}

#[test]
fn health_script_treats_http_error_status_as_fallback_without_recovery_body() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fake_bin = tempdir.path().join("bin");
    fs::create_dir_all(&fake_bin).map_err(|error| error.to_string())?;
    write_executable(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
printf '%s\n' '{"status":"degraded"}'
printf '%s\n' '__EE_HTTP_STATUS__:503'
"#,
    )?;
    write_executable(
        &fake_bin.join("am"),
        r#"#!/usr/bin/env bash
if [ "$1" = "agents" ] && [ "$2" = "list" ]; then
  printf '{"agents":[]}\n'
  exit 0
fi
if [ "$1" = "mail" ] && [ "$2" = "send" ]; then
  printf '{"sent":true}\n'
  exit 0
fi
exit 2
"#,
    )?;

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(script_path())
        .env("PATH", path)
        .env("AGENT_MAIL_PROJECT", tempdir.path())
        .env("AGENT_MAIL_FROM", "AgentA")
        .env("AGENT_MAIL_SINGLE_TO", "AgentA")
        .env("AGENT_MAIL_MULTI_TO", "AgentA,AgentB")
        .output()
        .map_err(|error| format!("run health script: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "health script should exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout JSON parse failed: {error}"))?;

    ensure(
        value["mcp_http_reachable"] == true,
        "HTTP health body should count as reachable transport",
    )?;
    ensure(
        value["checks"]["mcp_http"]["http_status"] == 503,
        "HTTP status should be preserved as bounded numeric evidence",
    )?;
    ensure(
        value.get("recovery").is_none(),
        "generic degraded body should not invent recovery details",
    )?;
    ensure(
        value["fallback_active"] == true,
        "HTTP 5xx health status should activate fallback posture",
    )
}
