//! Agent status inventory contract coverage.
//!
//! Freezes the deterministic renderer for `ee agent status --json` using the
//! franken-agent-detection fixtures and verifies that `ee status --json`
//! exposes a stable deferred inventory posture instead of machine-specific
//! local paths.

use ee::core::agent_detect::{
    AGENT_STATUS_SCHEMA_V1, AgentInventoryStatus, AgentStatusOptions, fixture_overrides,
    fixtures_path, gather_agent_status,
};
use ee::core::status::StatusReport;
use ee::output::{render_agent_status_json, render_status_json};
use serde_json::Value as JsonValue;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_json_equal(actual: Option<&JsonValue>, expected: JsonValue, context: &str) -> TestResult {
    let actual = actual.ok_or_else(|| format!("{context}: missing JSON field"))?;
    ensure(
        actual == &expected,
        format!("{context}: expected {expected:?}, got {actual:?}"),
    )
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("agent_detect")
        .join(format!("{name}.json.golden"))
}

fn canonical_json(raw: &str) -> Result<String, String> {
    let value: JsonValue =
        serde_json::from_str(raw).map_err(|error| format!("output must be JSON: {error}"))?;
    serde_json::to_string_pretty(&value)
        .map(|json| format!("{json}\n"))
        .map_err(|error| format!("failed to canonicalize JSON: {error}"))
}

fn assert_json_golden(name: &str, actual: &str) -> TestResult {
    let canonical = canonical_json(actual)?;
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("failed to create golden dir {}: {error}", parent.display())
            })?;
        }
        fs::write(&path, &canonical)
            .map_err(|error| format!("failed to write golden {}: {error}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        canonical == expected,
        format!(
            "agent status golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{canonical}"
        ),
    )
}

fn fixture_status_options() -> AgentStatusOptions {
    AgentStatusOptions {
        only_connectors: Some(vec![
            "claude".to_string(),
            "codex".to_string(),
            "cursor".to_string(),
            "gemini".to_string(),
        ]),
        include_undetected: false,
        root_overrides: fixture_overrides(&fixtures_path()),
    }
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[test]
fn agent_status_fixture_json_matches_golden() -> TestResult {
    let report = gather_agent_status(&fixture_status_options())
        .map_err(|error| format!("fixture detection failed: {error}"))?;
    let rendered = render_agent_status_json(&report);
    let value: JsonValue =
        serde_json::from_str(&rendered).map_err(|error| format!("agent status JSON: {error}"))?;

    ensure_json_equal(
        value.get("schema"),
        JsonValue::String("ee.response.v1".to_string()),
        "response schema",
    )?;
    ensure_json_equal(
        value.pointer("/data/inventory/schema"),
        JsonValue::String(AGENT_STATUS_SCHEMA_V1.to_string()),
        "inventory schema",
    )?;
    ensure_json_equal(
        value.pointer("/data/inventory/status"),
        JsonValue::String(AgentInventoryStatus::Ready.as_str().to_string()),
        "inventory status",
    )?;
    ensure_json_equal(
        value.pointer("/data/inventory/summary/detectedCount"),
        JsonValue::from(4),
        "detected count",
    )?;
    ensure(
        value
            .pointer("/data/inventory/installedAgents")
            .and_then(JsonValue::as_array)
            .is_some_and(|agents| agents.len() == 4),
        "fixture inventory should include four detected agents",
    )?;
    ensure(
        !rendered.contains("generatedAt"),
        "agent status contract must not include wall-clock timestamps",
    )?;

    assert_json_golden("agent_status", &rendered)
}

#[test]
fn status_json_embeds_deferred_agent_inventory() -> TestResult {
    let report = StatusReport::gather();
    let rendered = render_status_json(&report);
    let value: JsonValue =
        serde_json::from_str(&rendered).map_err(|error| format!("status JSON: {error}"))?;

    ensure_json_equal(
        value.pointer("/data/capabilities/agentDetection"),
        JsonValue::String("ready".to_string()),
        "agent detection capability",
    )?;
    ensure_json_equal(
        value.pointer("/data/agentInventory/status"),
        JsonValue::String("not_inspected".to_string()),
        "deferred inventory status",
    )?;
    ensure_json_equal(
        value.pointer("/data/agentInventory/inspectionCommand"),
        JsonValue::String("ee agent status --json".to_string()),
        "inspection command",
    )?;
    ensure_json_equal(
        value.pointer("/data/curationHealth/status"),
        JsonValue::String("not_inspected".to_string()),
        "curation health status",
    )?;
    ensure(
        value
            .pointer("/data/agentInventory/installedAgents")
            .is_none(),
        "ee status should not expose machine-specific agent roots",
    )
}

#[test]
fn agent_status_cli_writes_json_stdout_only() -> TestResult {
    let output = run_ee(&[
        "--json",
        "agent",
        "status",
        "--only",
        "codex",
        "--include-undetected",
    ])?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("agent status stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("agent status stderr was not UTF-8: {error}"))?;

    ensure(
        output.status.success(),
        format!("agent status --json should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("agent status --json stderr must be empty, got: {stderr:?}"),
    )?;
    ensure(
        stdout.starts_with('{') && stdout.ends_with('\n'),
        format!("agent status stdout must be newline-terminated JSON, got: {stdout:?}"),
    )?;

    let value: JsonValue =
        serde_json::from_str(&stdout).map_err(|error| format!("agent status JSON: {error}"))?;
    ensure_json_equal(
        value.pointer("/data/command"),
        JsonValue::String("agent status".to_string()),
        "command",
    )
}
