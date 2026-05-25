//! bd-sg5si Phase 1: pin the `ee focus suggest` CLI surface and schema.
//!
//! This is the honesty-only contract pin: until the follow-up
//! `implements-surface:focus_suggest` bead lands the centrality and
//! CASS-span scoring, the surface emits an empty `recommendations` array
//! and a documented degraded code. This test locks the schema shape so
//! downstream work has a clear target and any accidental schema rename
//! trips a focused failure.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-focus-suggest-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

#[test]
fn focus_suggest_emits_v1_schema_with_empty_recommendations() -> TestResult {
    let workspace = unique_workspace("schema")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    // No `ee init` — focus suggest is documented as read-only and must
    // work even before any workspace state is created (it will simply
    // have no signals to surface).

    let output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "focus",
        "suggest",
        "--from-cass",
        "--limit",
        "3",
        "--recent-hours",
        "12",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee focus suggest must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("focus suggest stdout must be JSON: {e}"))?;
    ensure(
        parsed["schema"].as_str() == Some("ee.response.v2"),
        format!(
            "outer schema must be ee.response.v2; got {:?}",
            parsed["schema"]
        ),
    )?;
    ensure(
        parsed["success"].as_bool() == Some(true),
        format!("success must be true; got {:?}", parsed["success"]),
    )?;

    let data = &parsed["data"];
    ensure(
        data["schema"].as_str() == Some("ee.focus.suggest.v1"),
        format!(
            "data.schema must be ee.focus.suggest.v1; got {:?}",
            data["schema"]
        ),
    )?;
    ensure(
        data["recommendations"]
            .as_array()
            .is_some_and(|a| a.is_empty()),
        format!(
            "recommendations must be an empty array in Phase 1; got {:?}",
            data["recommendations"]
        ),
    )?;
    ensure(
        data["fromCass"].as_bool() == Some(true),
        format!("fromCass flag must echo back; got {:?}", data["fromCass"]),
    )?;
    ensure(
        data["limit"].as_u64() == Some(3),
        format!("limit must echo back; got {:?}", data["limit"]),
    )?;
    ensure(
        data["recentHours"].as_u64() == Some(12),
        format!("recentHours must echo back; got {:?}", data["recentHours"]),
    )?;

    // The honesty-only degraded code makes the Phase 1 boundary visible.
    let degraded = parsed["degraded"]
        .as_array()
        .ok_or_else(|| format!("degraded must be an array; got {:?}", parsed["degraded"]))?;
    let phase_marker = degraded
        .iter()
        .find(|entry| entry["code"].as_str() == Some("focus_suggest_unimplemented"));
    let phase_marker = phase_marker.ok_or_else(|| {
        format!("degraded must contain focus_suggest_unimplemented marker; got {degraded:?}")
    })?;
    ensure(
        phase_marker["severity"].as_str() == Some("info"),
        format!(
            "focus_suggest_unimplemented severity must be info; got {:?}",
            phase_marker["severity"]
        ),
    )?;
    ensure(
        phase_marker["repair"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        format!(
            "focus_suggest_unimplemented must include a non-empty repair hint; got {:?}",
            phase_marker["repair"]
        ),
    )
}

#[test]
fn focus_suggest_default_recent_window_is_24_hours_and_limit_is_5() -> TestResult {
    // Pin the documented defaults so an agent harness that does not set
    // --recent-hours / --limit knows the surface emits stable values.
    let workspace = unique_workspace("defaults")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    let output = run_ee(&["--workspace", &workspace_arg, "--json", "focus", "suggest"])?;
    ensure(
        output.status.success(),
        format!(
            "ee focus suggest must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("focus suggest stdout must be JSON: {e}"))?;
    let data = &parsed["data"];
    ensure(
        data["recentHours"].as_u64() == Some(24),
        format!(
            "default recentHours must be 24; got {:?}",
            data["recentHours"]
        ),
    )?;
    ensure(
        data["limit"].as_u64() == Some(5),
        format!("default limit must be 5; got {:?}", data["limit"]),
    )?;
    ensure(
        data["fromCass"].as_bool() == Some(false),
        format!("default fromCass must be false; got {:?}", data["fromCass"]),
    )
}
