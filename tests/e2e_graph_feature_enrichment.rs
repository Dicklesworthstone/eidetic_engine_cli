//! bd-1dsfd: real-binary pin test for `ee graph feature-enrichment`
//! usage validation.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` and the
//! sibling pin tests. Unlike the centrality family this command has
//! its own dedicated validators (NOT shared with
//! `validate_graph_read_options`) — four distinct validation paths in
//! `handle_graph_feature_enrichment` plus a database-existence guard
//! that no other test exercises end-to-end against the real `ee`
//! binary. This test pins:
//!
//! * `--max-features 0` -> Usage repair
//!   "Omit --max-features to use the default cap."
//! * `--min-combined-score 2.0` -> Usage repair
//!   "Use a value like 0.01 or omit the flag."
//! * `--max-selection-boost -1.0` -> Usage repair
//!   "Use a value like 0.15 or omit the flag."
//! * `--singleflight-burst N` without `--dry-run` -> Usage repair
//!   "Use `ee graph feature-enrichment --dry-run --singleflight-burst
//!   6 --json`." (cross-flag dependency validator)
//! * Missing database (workspace not initialized) -> Storage repair
//!   "ee init --workspace ." surfaced via the database-existence
//!   guard before any algorithm work runs.
//!
//! The pin test does not exercise the full enrichment algorithm — it
//! only locks the documented user-facing validation contracts so
//! future reworks of `handle_graph_feature_enrichment` cannot reword
//! these without a deliberate, reviewed change.

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
        .join("ee-graph-feature-enrichment-pin")
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

fn run_feature_enrichment(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "feature-enrichment",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph feature-enrichment stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_usage_error(parsed: &Value, message_needles: &[&str], repair_needle: &str) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    for needle in message_needles {
        ensure(
            message.contains(needle),
            format!("usage message must contain {needle:?}; got {message}"),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains(repair_needle),
        format!("usage repair must contain {repair_needle:?}; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_feature_enrichment_rejects_zero_max_features_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-max-features")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_feature_enrichment(&workspace_arg, &["--max-features", "0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph feature-enrichment --max-features 0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["--max-features must be greater than zero"],
        "Omit --max-features to use the default cap.",
    )
}

#[test]
fn graph_feature_enrichment_rejects_min_combined_score_out_of_range_with_usage_error() -> TestResult
{
    let workspace = unique_workspace("usage-min-combined")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) =
        run_feature_enrichment(&workspace_arg, &["--min-combined-score", "2.0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph feature-enrichment --min-combined-score 2.0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["--min-combined-score must be a finite value in [0.0, 1.0]"],
        "Use a value like 0.01 or omit the flag.",
    )
}

#[test]
fn graph_feature_enrichment_rejects_negative_max_selection_boost_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-max-boost")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) =
        run_feature_enrichment(&workspace_arg, &["--max-selection-boost", "-1.0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph feature-enrichment --max-selection-boost -1.0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["--max-selection-boost must be a finite non-negative value"],
        "Use a value like 0.15 or omit the flag.",
    )
}

#[test]
fn graph_feature_enrichment_rejects_singleflight_burst_without_dry_run_with_usage_error()
-> TestResult {
    let workspace = unique_workspace("usage-singleflight")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_feature_enrichment(&workspace_arg, &["--singleflight-burst", "6"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph feature-enrichment --singleflight-burst without --dry-run must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["--singleflight-burst requires --dry-run"],
        "ee graph feature-enrichment --dry-run --singleflight-burst 6 --json",
    )
}

#[test]
fn graph_feature_enrichment_surfaces_storage_error_when_database_missing() -> TestResult {
    // Deliberately skip `ee init` so the database-existence guard
    // fires before any validation work. This pins the documented
    // Storage repair pointing the user at `ee init --workspace .`.
    let workspace = unique_workspace("usage-no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_feature_enrichment(&workspace_arg, &[])?;
    ensure(
        !output.status.success(),
        format!(
            "graph feature-enrichment without ee init must fail; stdout: {}",
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
        message.contains("Database not found at"),
        format!("error message must explain the missing database; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee init --workspace ."),
        format!("error repair must point at `ee init --workspace .`; got {repair}"),
    )?;
    Ok(())
}
