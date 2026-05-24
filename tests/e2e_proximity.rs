//! bd-243rl: real-binary pin test for `ee proximity`.
//!
//! `handle_proximity` (src/cli/mod.rs:24584) shares the
//! `validate_graph_memory_pair` and `validate_graph_threshold` validators with
//! the graph subcommands but emits its own `ee.proximity.v1` schema and a
//! distinct interpretation vocabulary (`self`, `missing_memory`,
//! `unreachable`, `weak`, `moderate`, `strong`) plus a degraded array carrying
//! `graph_proximity_unreachable` when the queried pair sits in different
//! Gomory-Hu components.
//!
//! The existing `tests/graph_neighborhood_smoke.rs:1368` covers only the
//! happy-path linked pair. Validator errors via the proximity entrypoint, the
//! self-pair branch, the missing-memory branch, and the unreachable branch
//! are all unpinned against the real `ee` binary.
//!
//! This pin-test mirrors `tests/e2e_graph_explain_link.rs`'s harness shape and
//! pins:
//!
//! * empty src/dst surfaces `validate_graph_memory_pair`'s usage error +
//!   repair through the proximity command path
//! * `--min-weight` outside `[0.0, 1.0]` surfaces
//!   `validate_graph_threshold`'s usage error through proximity
//! * a self-pair (memory_a == memory_b for a memory that exists in the
//!   link graph) surfaces `interpretation="self"`, `minCut=0.0`,
//!   `treePath=[memory]`, and an empty `degraded` array
//! * a memory_b that does not appear in any link surfaces
//!   `interpretation="missing_memory"`, `minCut=null`, `treePath=null`,
//!   `degraded=[]`, and `memoryA`/`memoryB` echo
//! * a disconnected pair (two link components with no bridge) surfaces
//!   `interpretation="unreachable"`, `minCut=null`, `treePath=null`, and a
//!   `degraded` entry whose `code="graph_proximity_unreachable"` and whose
//!   `severity="info"`
//! * the envelope pins `schema="ee.proximity.v1"` across all branches

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use ee::db::{CreateMemoryLinkInput, DbConnection, MemoryLinkRelation, MemoryLinkSource};
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
        .join("ee-proximity-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn init_workspace(workspace_arg: &str) -> TestResult {
    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )
}

fn remember(workspace_arg: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["public_id"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .or_else(|| parsed["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "remember response missing memory id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn insert_link(database_path: &std::path::Path, link_id: &str, src: &str, dst: &str) -> TestResult {
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            link_id,
            &CreateMemoryLinkInput {
                src_memory_id: src.to_owned(),
                dst_memory_id: dst.to_owned(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.9,
                confidence: 0.8,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-01T00:00:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("e2e-proximity-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_proximity(
    workspace_arg: &str,
    memory_a: &str,
    memory_b: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "proximity",
        memory_a,
        memory_b,
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("proximity stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_proximity_envelope(parsed: &Value) -> TestResult {
    ensure(
        parsed["schema"].as_str() == Some("ee.proximity.v1"),
        format!("schema must be ee.proximity.v1; got {parsed}"),
    )?;
    ensure(
        parsed["memoryA"].is_string(),
        format!("memoryA must echo as string; got {parsed}"),
    )?;
    ensure(
        parsed["memoryB"].is_string(),
        format!("memoryB must echo as string; got {parsed}"),
    )?;
    ensure(
        parsed["interpretation"].is_string(),
        format!("interpretation must be a string; got {parsed}"),
    )?;
    ensure(
        parsed["degraded"].is_array(),
        format!("degraded must be an array; got {parsed}"),
    )?;
    Ok(())
}

#[test]
fn proximity_rejects_empty_memory_pair_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_proximity(&workspace_arg, "", "mem_b", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "proximity with empty memory_a must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("source and destination memory IDs must be non-empty"),
        format!("usage message must pin validate_graph_memory_pair text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee graph path mem_a mem_b"),
        format!("usage repair must reference the documented example; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn proximity_rejects_min_weight_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-weight")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) =
        run_proximity(&workspace_arg, "mem_a", "mem_b", &["--min-weight", "2.0"])?;
    ensure(
        !output.status.success(),
        format!(
            "proximity with --min-weight 2.0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("--min-weight") && message.contains("finite value in [0.0, 1.0]"),
        format!("usage message must pin validate_graph_threshold text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("--min-weight"),
        format!("usage repair must reference --min-weight; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn proximity_self_pair_returns_self_interpretation_with_zero_min_cut() -> TestResult {
    let workspace = unique_workspace("self-pair")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let a = remember(&workspace_arg, "Pin-test proximity self-pair a.")?;
    let b = remember(&workspace_arg, "Pin-test proximity self-pair b.")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(&database_path, "link_00000000000000000000pinpx01", &a, &b)?;

    let (output, parsed) = run_proximity(&workspace_arg, &a, &a, &[])?;
    ensure(
        output.status.success(),
        format!(
            "proximity self-pair must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    assert_proximity_envelope(&parsed)?;
    ensure(
        parsed["memoryA"].as_str() == Some(a.as_str()),
        format!("memoryA must echo the self id; got {parsed}"),
    )?;
    ensure(
        parsed["memoryB"].as_str() == Some(a.as_str()),
        format!("memoryB must echo the self id; got {parsed}"),
    )?;
    ensure(
        parsed["interpretation"].as_str() == Some("self"),
        format!("interpretation must be 'self' for matched ids; got {parsed}"),
    )?;
    let min_cut = parsed["minCut"]
        .as_f64()
        .ok_or_else(|| format!("minCut must be numeric for self-pair; got {parsed}"))?;
    ensure(
        min_cut == 0.0,
        format!("self-pair minCut must be 0.0; got {min_cut}"),
    )?;
    let tree_path = parsed["treePath"]
        .as_array()
        .ok_or_else(|| format!("treePath must be an array for self-pair; got {parsed}"))?;
    ensure(
        tree_path.len() == 1 && tree_path[0].as_str() == Some(a.as_str()),
        format!("self-pair treePath must be [self_id]; got {tree_path:?}"),
    )?;
    ensure(
        parsed["degraded"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        format!("self-pair must not be degraded; got {parsed}"),
    )?;
    Ok(())
}

#[test]
fn proximity_missing_memory_returns_missing_interpretation() -> TestResult {
    let workspace = unique_workspace("missing-memory")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let a = remember(&workspace_arg, "Pin-test proximity missing src.")?;
    let b = remember(&workspace_arg, "Pin-test proximity missing dst.")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(&database_path, "link_00000000000000000000pinpx02", &a, &b)?;

    let phantom = "mem_phantom_not_in_link_graph_xyz";
    let (output, parsed) = run_proximity(&workspace_arg, &a, phantom, &[])?;
    ensure(
        output.status.success(),
        format!(
            "proximity with one absent memory must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    assert_proximity_envelope(&parsed)?;
    ensure(
        parsed["memoryA"].as_str() == Some(a.as_str()),
        format!("memoryA must echo the present id; got {parsed}"),
    )?;
    ensure(
        parsed["memoryB"].as_str() == Some(phantom),
        format!("memoryB must echo the phantom id; got {parsed}"),
    )?;
    ensure(
        parsed["interpretation"].as_str() == Some("missing_memory"),
        format!(
            "interpretation must be 'missing_memory' when one id is absent from the link graph; got {parsed}"
        ),
    )?;
    ensure(
        parsed["minCut"].is_null(),
        format!("minCut must be null when missing_memory; got {parsed}"),
    )?;
    ensure(
        parsed["treePath"].is_null(),
        format!("treePath must be null when missing_memory; got {parsed}"),
    )?;
    ensure(
        parsed["degraded"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        format!("missing_memory branch must not emit a degraded entry; got {parsed}"),
    )?;
    Ok(())
}

#[test]
fn proximity_disconnected_pair_returns_unreachable_with_degraded_entry() -> TestResult {
    let workspace = unique_workspace("unreachable")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    // Two distinct components: (a <-> b) and (c <-> d). No cross-edge.
    let a = remember(&workspace_arg, "Pin-test proximity component-1 a.")?;
    let b = remember(&workspace_arg, "Pin-test proximity component-1 b.")?;
    let c = remember(&workspace_arg, "Pin-test proximity component-2 c.")?;
    let d = remember(&workspace_arg, "Pin-test proximity component-2 d.")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(&database_path, "link_00000000000000000000pinpx03", &a, &b)?;
    insert_link(&database_path, "link_00000000000000000000pinpx04", &c, &d)?;

    let (output, parsed) = run_proximity(&workspace_arg, &a, &c, &[])?;
    ensure(
        output.status.success(),
        format!(
            "proximity across components must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    assert_proximity_envelope(&parsed)?;
    ensure(
        parsed["memoryA"].as_str() == Some(a.as_str()),
        format!("memoryA must echo the left id; got {parsed}"),
    )?;
    ensure(
        parsed["memoryB"].as_str() == Some(c.as_str()),
        format!("memoryB must echo the right id; got {parsed}"),
    )?;
    ensure(
        parsed["interpretation"].as_str() == Some("unreachable"),
        format!("interpretation must be 'unreachable' across components; got {parsed}"),
    )?;
    ensure(
        parsed["minCut"].is_null(),
        format!("minCut must be null when unreachable; got {parsed}"),
    )?;
    ensure(
        parsed["treePath"].is_null(),
        format!("treePath must be null when unreachable; got {parsed}"),
    )?;
    let degraded = parsed["degraded"]
        .as_array()
        .ok_or_else(|| format!("degraded must be an array; got {parsed}"))?;
    ensure(
        degraded.len() == 1,
        format!("unreachable branch must surface exactly one degraded entry; got {degraded:?}"),
    )?;
    let only = &degraded[0];
    ensure(
        only["code"].as_str() == Some("graph_proximity_unreachable"),
        format!("degraded[0].code must be graph_proximity_unreachable; got {only}"),
    )?;
    ensure(
        only["severity"].as_str() == Some("info"),
        format!("degraded[0].severity must be 'info'; got {only}"),
    )?;
    let message = only["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("unreachable") && message.contains("different components"),
        format!("degraded[0].message must explain the unreachable cause; got {message}"),
    )?;
    Ok(())
}
