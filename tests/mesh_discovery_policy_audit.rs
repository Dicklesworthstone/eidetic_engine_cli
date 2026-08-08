//! SRR6.46.7 discovery-policy audit contract (bd-tc-epic-qzk7o.2.5, part b
//! of the T1.7 honesty backfill — this file was a newline-only stub).
//!
//! Drives the real binary end-to-end: policy mutations must write
//! `mesh.discovery_policy_changed` audit rows (action, target, and the
//! `ee.mesh.discovery_policy_changed.v1` details schema) that are readable
//! back through `ee audit timeline`, and list mutations must record only the
//! node-key hash — the raw node key must never appear in audit output.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const TEST_NODE_KEY: &str =
    "nodekey:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn workspace_dir() -> PathBuf {
    std::env::temp_dir().join(format!("ee-mesh-dpa-{}", std::process::id()))
}

fn run_ee(workspace: &PathBuf, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env("EE_MESH_ENABLED", "1")
        .env("EE_DATABASE_PATH", workspace.join(".ee").join("ee.db"))
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure_success(output: &Output, label: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label} failed with exit {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn stdout_text(output: &Output, label: &str) -> Result<String, String> {
    String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))
}

#[test]
fn discovery_policy_mutations_write_readable_hash_only_audit_rows() -> TestResult {
    let workspace = workspace_dir();
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create workspace dir: {error}"))?;

    let init = run_ee(&workspace, &["init", "--json"])?;
    ensure_success(&init, "ee init")?;

    let set = run_ee(
        &workspace,
        &[
            "mesh",
            "discovery-policy",
            "set",
            "--discovery-mode",
            "allowlist",
            "--json",
        ],
    )?;
    ensure_success(&set, "discovery-policy set")?;

    let allow = run_ee(
        &workspace,
        &["mesh", "discovery-policy", "allow", TEST_NODE_KEY, "--json"],
    )?;
    ensure_success(&allow, "discovery-policy allow")?;

    let timeline = run_ee(
        &workspace,
        &[
            "audit",
            "timeline",
            "--target",
            "discovery_policy",
            "--limit",
            "10",
            "--json",
        ],
    )?;
    ensure_success(&timeline, "audit timeline")?;
    let timeline_text = stdout_text(&timeline, "audit timeline")?;

    // `ee audit timeline --json` emits the canonical `ee.response.v2`
    // envelope with the `ee.audit.timeline.v1` report (its `entries` +
    // `pagination`) under `data`, so assert both schemas and that both
    // mutation rows are present.
    let document: serde_json::Value = serde_json::from_str(&timeline_text)
        .map_err(|error| format!("audit timeline stdout was not JSON: {error}"))?;
    if document["schema"] != serde_json::Value::String("ee.response.v2".to_owned()) {
        return Err(format!(
            "audit timeline envelope schema was not ee.response.v2: {timeline_text}"
        ));
    }
    if document["data"]["schema"] != serde_json::Value::String("ee.audit.timeline.v1".to_owned()) {
        return Err(format!(
            "audit timeline data schema was not ee.audit.timeline.v1: {timeline_text}"
        ));
    }
    let entry_count = document["data"]["entries"].as_array().map_or(0, Vec::len);
    if entry_count < 2 {
        return Err(format!(
            "expected the set + allow discovery-policy audit rows, saw {entry_count}: {timeline_text}"
        ));
    }

    for needle in [
        "mesh.discovery_policy_changed",
        "ee.mesh.discovery_policy_changed.v1",
        "\"operation\":\"set\"",
        "\"operation\":\"allow\"",
        "allowlist",
        "nodeKeyHash",
    ] {
        if !timeline_text.contains(needle) {
            return Err(format!(
                "audit timeline output missing {needle}\noutput:\n{timeline_text}"
            ));
        }
    }

    if timeline_text.contains(TEST_NODE_KEY) {
        return Err(format!(
            "raw node key leaked into audit output — only nodeKeyHash may appear:\n{timeline_text}"
        ));
    }

    let _ = fs::remove_dir_all(&workspace);
    Ok(())
}
