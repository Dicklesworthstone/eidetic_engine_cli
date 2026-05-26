//! bd-sg5si Phase 1 → bd-1idcb Phase 2: `ee focus suggest` schema +
//! emission contract.
//!
//! Phase 1 pinned the CLI surface and the `ee.focus.suggest.v1`
//! envelope while emitting an empty `recommendations[]` plus the
//! `focus_suggest_unimplemented` honesty sentinel. Phase 2 retired
//! the sentinel: the schema is unchanged, but the surface now
//! populates `recommendations[]` from a real CASS-span + graph
//! centrality scoring path, and `degraded[]` carries only
//! situation-appropriate entries (e.g. `workspace_uninitialized`,
//! `no_recent_evidence`, `graph_unavailable`).
//!
//! This test locks the post-Phase-2 contract so a future accidental
//! schema rename or sentinel resurrection trips a focused failure.
//!
//! Phase 2 acceptance gate (per AGENTS.md honesty-only ↔ implements-
//! surface taxonomy): the `focus_suggest_unimplemented` code must be
//! absent from production emissions even though the historical
//! fixture remains as a tombstone in
//! `tests/fixtures/failure_modes/`.

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
fn focus_suggest_emits_v1_schema_and_no_phase1_sentinel() -> TestResult {
    let workspace = unique_workspace("schema")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    // No `ee init` — focus suggest is documented as read-only and must
    // work even before any workspace state is created.

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
        data["recommendations"].as_array().is_some(),
        format!(
            "recommendations must be an array; got {:?}",
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

    // Phase 2 acceptance gate: the honesty-only sentinel must not
    // reappear in production emissions. The historical fixture lives
    // on as a retired tombstone but the code must never fire again.
    let degraded = parsed["degraded"]
        .as_array()
        .ok_or_else(|| format!("degraded must be an array; got {:?}", parsed["degraded"]))?;
    let sentinel = degraded
        .iter()
        .find(|entry| entry["code"].as_str() == Some("focus_suggest_unimplemented"));
    ensure(
        sentinel.is_none(),
        format!(
            "Phase 1 focus_suggest_unimplemented sentinel must not be emitted; degraded={degraded:?}"
        ),
    )?;

    // Against an uninitialized workspace, Phase 2 should surface a
    // `workspace_uninitialized` degraded entry pointing at `ee init`.
    let init_marker = degraded
        .iter()
        .find(|entry| entry["code"].as_str() == Some("workspace_uninitialized"));
    let init_marker = init_marker.ok_or_else(|| {
        format!("uninitialized workspace must emit workspace_uninitialized; got {degraded:?}")
    })?;
    ensure(
        init_marker["severity"].as_str() == Some("warning"),
        format!(
            "workspace_uninitialized severity must be warning; got {:?}",
            init_marker["severity"]
        ),
    )?;
    ensure(
        init_marker["repair"]
            .as_str()
            .is_some_and(|r| r.contains("ee init")),
        format!(
            "workspace_uninitialized must repair via `ee init`; got {:?}",
            init_marker["repair"]
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

#[test]
fn focus_suggest_task_frame_flag_accepted() -> TestResult {
    // Phase 2 added `--task-frame <id>`. Even against an empty
    // workspace, the flag must be parsed and the command exits zero.
    let workspace = unique_workspace("taskframe")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    let output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "focus",
        "suggest",
        "--task-frame",
        "tf_demo_01",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee focus suggest --task-frame must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("focus suggest stdout must be JSON: {e}"))?;
    ensure(
        parsed["data"]["schema"].as_str() == Some("ee.focus.suggest.v1"),
        format!(
            "task-frame call must still emit the v1 schema; got {:?}",
            parsed["data"]["schema"]
        ),
    )
}
