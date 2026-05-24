//! bd-291ho: real-binary pin test for `ee graph snapshot refresh` validators.
//!
//! `handle_graph_snapshot_refresh` (src/cli/mod.rs:25313) routes the `--graph`
//! argument through `graph_snapshot_refresh_types` (src/cli/mod.rs:25419),
//! which maps canonical names, accepts long-form aliases
//! (`causal_evidence`, `revision_dag`, `rule_provenance`,
//! `contradiction_subgraph`), and otherwise emits a `DomainError::Usage`
//! whose message starts with `unknown graph refresh target:` and whose repair
//! enumerates the canonical names. The handler also emits a
//! `DomainError::Storage` when the workspace database does not exist, with a
//! repair pointing to `ee init --workspace .`.
//!
//! `tests/graph_determinism.rs:489` already pins the happy `--dry-run` path
//! for the canonical graph names, but the validator surface and the
//! long-form alias mapping have no real-binary assertions. This pin-test
//! mirrors the `tests/e2e_graph_path.rs` harness shape and pins:
//!
//! * `--graph bogus_target` exits non-zero, emits
//!   `unknown graph refresh target: bogus_target`, and surfaces the
//!   documented `Use --graph=memory_links, causal, revision, rules,
//!   contradictions, or all.` repair.
//! * running `graph snapshot refresh` against a workspace without an
//!   initialized database surfaces a `Database not found` Storage error
//!   whose repair is `ee init --workspace .`.
//! * the `causal_evidence` alias is accepted by `--dry-run` and produces a
//!   single-report response whose `graphType=causal_evidence`.
//! * the `revision_dag` alias is accepted by `--dry-run` and produces a
//!   single-report response whose `graphType=revision_dag`.

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
        .join("ee-graph-snapshot-refresh-pin")
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

fn run_snapshot_refresh(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "snapshot",
        "refresh",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph snapshot refresh stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

#[test]
fn graph_snapshot_refresh_rejects_unknown_graph_target_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-bogus-graph")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) =
        run_snapshot_refresh(&workspace_arg, &["--graph", "bogus_target", "--dry-run"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph snapshot refresh --graph bogus_target must fail; stdout: {}",
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
        message.contains("unknown graph refresh target: bogus_target"),
        format!(
            "usage message must pin graph_snapshot_refresh_types unknown-target text; got {message}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("--graph=memory_links")
            && repair.contains("causal")
            && repair.contains("revision")
            && repair.contains("rules")
            && repair.contains("contradictions")
            && repair.contains("or all"),
        format!("usage repair must enumerate the canonical --graph values; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_snapshot_refresh_without_init_surfaces_database_missing_storage_error() -> TestResult {
    let workspace = unique_workspace("no-init")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    // Intentionally skip `ee init` so .ee/ee.db does not exist.

    let (output, parsed) = run_snapshot_refresh(&workspace_arg, &["--dry-run"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph snapshot refresh on uninitialized workspace must fail; stdout: {}",
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
        message.contains("Database not found"),
        format!("storage message must pin the Database not found guard; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee init --workspace ."),
        format!("storage repair must point at `ee init --workspace .`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_snapshot_refresh_accepts_causal_evidence_alias_in_dry_run() -> TestResult {
    let workspace = unique_workspace("alias-causal-evidence")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) =
        run_snapshot_refresh(&workspace_arg, &["--graph", "causal_evidence", "--dry-run"])?;
    ensure(
        output.status.success(),
        format!(
            "--graph causal_evidence alias must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["schema"].as_str() == Some("ee.graph.centrality_refresh.v1"),
        format!(
            "schema must be ee.graph.centrality_refresh.v1; got {schema}",
            schema = parsed["schema"]
        ),
    )?;
    let reports = parsed["data"]["reports"]
        .as_array()
        .ok_or_else(|| format!("reports array must be present; got {parsed}"))?;
    ensure(
        reports.len() == 1,
        format!("causal_evidence alias must produce exactly one report; got {reports:?}"),
    )?;
    let only = &reports[0];
    ensure(
        only["graphType"].as_str() == Some("causal_evidence"),
        format!("graphType must be causal_evidence; got {only}"),
    )?;
    ensure(
        only["dryRun"].as_bool() == Some(true),
        format!("dryRun must be true for --dry-run; got {only}"),
    )?;
    ensure(
        only["status"].as_str() == Some("dry_run"),
        format!("status must be dry_run for --dry-run; got {only}"),
    )?;
    Ok(())
}

#[test]
fn graph_snapshot_refresh_accepts_revision_dag_alias_in_dry_run() -> TestResult {
    let workspace = unique_workspace("alias-revision-dag")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) =
        run_snapshot_refresh(&workspace_arg, &["--graph", "revision_dag", "--dry-run"])?;
    ensure(
        output.status.success(),
        format!(
            "--graph revision_dag alias must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let reports = parsed["data"]["reports"]
        .as_array()
        .ok_or_else(|| format!("reports array must be present; got {parsed}"))?;
    ensure(
        reports.len() == 1,
        format!("revision_dag alias must produce exactly one report; got {reports:?}"),
    )?;
    let only = &reports[0];
    ensure(
        only["graphType"].as_str() == Some("revision_dag"),
        format!("graphType must be revision_dag; got {only}"),
    )?;
    ensure(
        only["dryRun"].as_bool() == Some(true),
        format!("dryRun must be true for --dry-run; got {only}"),
    )?;
    Ok(())
}
