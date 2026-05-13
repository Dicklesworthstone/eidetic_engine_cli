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
