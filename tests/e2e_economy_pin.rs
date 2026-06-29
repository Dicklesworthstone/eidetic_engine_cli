//! Real-binary pin test for `ee economy` sub-commands.
//!
//! Covers the four economy sub-commands with a real `ee` binary against temp
//! workspaces. The shell script `e2e_advanced.sh` tests these commands against
//! the agent's live workspace and only checks "output is valid JSON". These
//! tests add:
//!
//! * Stable response-envelope assertions (schema, data.command, data.version)
//! * Artifact count assertions (totalArtifacts tracks seeded memories)
//! * Score field presence on a real scored artifact (overallScore, utilityScore,
//!   costScore, freshnessScore, artifactId echo)
//! * Breakdown object presence when --breakdown is passed
//! * Usage-error guard for `economy simulate --baseline-budget 0`
//! * DegradedMode error when no database exists (no `ee init`)
//! * NotFound exit-code contract for unknown memory IDs

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
        .map_err(|e| format!("failed to spawn ee {}: {e}", args.join(" ")))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("clock error: {e}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-economy-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn ws_str(dir: &PathBuf) -> Result<String, String> {
    dir.to_str()
        .ok_or_else(|| "workspace path not UTF-8".to_owned())
        .map(str::to_owned)
}

fn init_workspace(workspace: &str) -> TestResult {
    let out = run_ee(&["--workspace", workspace, "--json", "init"])?;
    ensure(
        out.status.success(),
        format!(
            "ee init must succeed; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn remember(workspace: &str, content: &str) -> Result<String, String> {
    let out = run_ee(&[
        "--workspace",
        workspace,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !out.status.success() {
        return Err(format!(
            "ee remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("remember stdout not JSON: {e}"))?;
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

fn run_economy_report(workspace: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args = vec!["--workspace", workspace, "--json", "economy", "report"];
    args.extend_from_slice(extra);
    let out = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "economy report stdout not JSON: {e}\nraw={}",
            String::from_utf8_lossy(&out.stdout)
        )
    })?;
    Ok((out, parsed))
}

fn run_economy_score(workspace: &str, id: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args = vec!["--workspace", workspace, "--json", "economy", "score", id];
    args.extend_from_slice(extra);
    let out = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "economy score stdout not JSON: {e}\nraw={}",
            String::from_utf8_lossy(&out.stdout)
        )
    })?;
    Ok((out, parsed))
}

fn assert_success_envelope(parsed: &Value, expected_command: &str) -> TestResult {
    ensure(
        parsed["schema"].as_str() == Some("ee.response.v2"),
        format!("envelope schema must be ee.response.v2; got {parsed}"),
    )?;
    ensure(
        parsed["success"] == Value::Bool(true),
        format!("success must be true; got {parsed}"),
    )?;
    let data = &parsed["data"];
    ensure(
        data.is_object(),
        format!("data must be an object; got {parsed}"),
    )?;
    ensure(
        data["command"].as_str() == Some(expected_command),
        format!(
            "data.command must be {expected_command:?}; got {}",
            data["command"]
        ),
    )?;
    ensure(
        data["version"].is_string(),
        format!("data.version must be a string; got {data}"),
    )?;
    Ok(())
}

fn assert_error_envelope(parsed: &Value) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must contain an error object; got {parsed}"),
    )?;
    ensure(
        error["message"].is_string(),
        format!("error.message must be a string; got {error}"),
    )?;
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn economy_report_on_empty_db_returns_stable_envelope() -> TestResult {
    let ws_dir = unique_workspace("report-empty")?;
    let ws = ws_str(&ws_dir)?;
    init_workspace(&ws)?;

    let (out, parsed) = run_economy_report(&ws, &[])?;
    ensure(
        out.status.success(),
        format!(
            "economy report on empty db must exit 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        ),
    )?;

    assert_success_envelope(&parsed, "economy report")?;

    let report = &parsed["data"]["report"];
    ensure(
        report.is_object(),
        format!("data.report must be an object; got {}", parsed["data"]),
    )?;
    ensure(
        report["totalArtifacts"].as_u64() == Some(0),
        format!(
            "totalArtifacts must be 0 on empty db; got {}",
            report["totalArtifacts"]
        ),
    )?;
    ensure(
        report["status"].is_string(),
        format!("status must be a string; got {report}"),
    )?;
    ensure(
        report["mutationStatus"].is_string(),
        format!("mutationStatus must be a string; got {report}"),
    )?;
    ensure(
        report["workspaceId"].is_string(),
        format!("workspaceId must be a string; got {report}"),
    )?;
    ensure(
        report["artifactBreakdown"].as_array().is_some(),
        format!("artifactBreakdown must be an array; got {report}"),
    )?;
    Ok(())
}

#[test]
fn economy_report_with_seeded_memories_counts_artifacts() -> TestResult {
    let ws_dir = unique_workspace("report-seeded")?;
    let ws = ws_str(&ws_dir)?;
    init_workspace(&ws)?;

    remember(&ws, "Economy-pin: alpha constraint for cost estimation.")?;
    remember(
        &ws,
        "Economy-pin: beta strategy for token budget allocation.",
    )?;
    remember(&ws, "Economy-pin: gamma observation on attention reserves.")?;

    let (out, parsed) = run_economy_report(&ws, &[])?;
    ensure(
        out.status.success(),
        format!(
            "economy report with seeded data must exit 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        ),
    )?;

    assert_success_envelope(&parsed, "economy report")?;

    let report = &parsed["data"]["report"];
    let total = report["totalArtifacts"].as_u64().unwrap_or(0);
    ensure(
        total >= 3,
        format!("totalArtifacts must be >= 3 after seeding 3 memories; got {total}"),
    )?;

    let scored_count = report["scoredArtifactIds"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    ensure(
        scored_count >= 1,
        format!("scoredArtifactIds must be non-empty after seeding; len={scored_count}"),
    )?;

    // artifactBreakdown must contain an entry for "memory" type
    let breakdown = report["artifactBreakdown"]
        .as_array()
        .ok_or_else(|| format!("artifactBreakdown must be an array; got {report}"))?;
    let has_memory_entry = breakdown
        .iter()
        .any(|entry| entry["artifactType"].as_str() == Some("memory"));
    ensure(
        has_memory_entry,
        format!("artifactBreakdown must contain a 'memory' entry after seeding; got {breakdown:?}"),
    )?;

    // Each breakdown entry must carry the required numeric fields
    for entry in breakdown {
        ensure(
            entry["count"].is_number(),
            format!("artifactBreakdown entry must have numeric count; got {entry}"),
        )?;
        ensure(
            entry["avgUtility"].is_number(),
            format!("artifactBreakdown entry must have numeric avgUtility; got {entry}"),
        )?;
    }
    Ok(())
}

#[test]
fn economy_report_without_init_returns_degraded_mode_error() -> TestResult {
    let ws_dir = unique_workspace("report-no-init")?;
    let ws = ws_str(&ws_dir)?;
    // Deliberately skip ee init so no database exists

    let (out, parsed) = run_economy_report(&ws, &[])?;
    ensure(
        !out.status.success(),
        format!("economy report without init must exit non-zero; stdout={parsed}"),
    )?;

    assert_error_envelope(&parsed)?;

    let message = parsed["error"]["message"].as_str().unwrap_or_default();
    // DomainError::UnsatisfiedDegradedMode from economy_paths:
    // "Memory economy metrics are unavailable because no database exists at ..."
    ensure(
        message.contains("unavailable") || message.contains("database") || message.contains("init"),
        format!("error must explain missing database; got {message:?}"),
    )?;
    Ok(())
}

#[test]
fn economy_score_on_unknown_id_returns_not_found() -> TestResult {
    let ws_dir = unique_workspace("score-not-found")?;
    let ws = ws_str(&ws_dir)?;
    init_workspace(&ws)?;

    let unknown_id = "mem_does_not_exist_in_economy_pin_test";
    let (out, parsed) = run_economy_score(&ws, unknown_id, &[])?;
    ensure(
        !out.status.success(),
        format!("economy score on unknown id must exit non-zero; stdout={parsed}"),
    )?;

    assert_error_envelope(&parsed)?;

    // DomainError::NotFound: "memory economy artifact not found: <id>"
    let message = parsed["error"]["message"].as_str().unwrap_or_default();
    ensure(
        message.contains(unknown_id),
        format!("error message must name the unknown artifact id; got {message:?}"),
    )?;

    let repair = parsed["error"]["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee memory list"),
        format!("error repair must point at ee memory list; got {repair:?}"),
    )?;
    Ok(())
}

#[test]
fn economy_score_on_real_memory_returns_stable_envelope() -> TestResult {
    let ws_dir = unique_workspace("score-real")?;
    let ws = ws_str(&ws_dir)?;
    init_workspace(&ws)?;
    let mem_id = remember(&ws, "Economy-pin: real memory for score assertion.")?;

    let (out, parsed) = run_economy_score(&ws, &mem_id, &[])?;
    ensure(
        out.status.success(),
        format!(
            "economy score on real memory must exit 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        ),
    )?;

    assert_success_envelope(&parsed, "economy score")?;

    let score = &parsed["data"]["score"];
    ensure(
        score.is_object(),
        format!("data.score must be an object; got {}", parsed["data"]),
    )?;
    ensure(
        score["artifactId"].as_str() == Some(mem_id.as_str()),
        format!(
            "score.artifactId must echo {mem_id}; got {}",
            score["artifactId"]
        ),
    )?;
    ensure(
        score["artifactType"].is_string(),
        format!("score.artifactType must be a string; got {score}"),
    )?;
    ensure(
        score["overallScore"].is_number(),
        format!("score.overallScore must be a number; got {score}"),
    )?;
    ensure(
        score["utilityScore"].is_number(),
        format!("score.utilityScore must be a number; got {score}"),
    )?;
    ensure(
        score["costScore"].is_number(),
        format!("score.costScore must be a number; got {score}"),
    )?;
    ensure(
        score["freshnessScore"].is_number(),
        format!("score.freshnessScore must be a number; got {score}"),
    )?;
    ensure(
        score["status"].is_string(),
        format!("score.status must be a string; got {score}"),
    )?;
    ensure(
        score["mutationStatus"].as_str() == Some("read_only_no_mutation"),
        format!(
            "score.mutationStatus must be read_only_no_mutation; got {}",
            score["mutationStatus"]
        ),
    )?;
    Ok(())
}

#[test]
fn economy_score_breakdown_flag_adds_breakdown_object() -> TestResult {
    let ws_dir = unique_workspace("score-breakdown")?;
    let ws = ws_str(&ws_dir)?;
    init_workspace(&ws)?;
    let mem_id = remember(&ws, "Economy-pin: breakdown test memory for score detail.")?;

    let (out, parsed) = run_economy_score(&ws, &mem_id, &["--breakdown"])?;
    ensure(
        out.status.success(),
        format!(
            "economy score --breakdown on real memory must exit 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        ),
    )?;

    assert_success_envelope(&parsed, "economy score")?;

    let score = &parsed["data"]["score"];
    let breakdown = &score["breakdown"];
    ensure(
        breakdown.is_object(),
        format!("score.breakdown must be an object when --breakdown is passed; got {score}"),
    )?;
    ensure(
        breakdown["formula"].is_string(),
        format!("breakdown.formula must be a string; got {breakdown}"),
    )?;
    ensure(
        breakdown["retrievalFrequency"].is_number(),
        format!("breakdown.retrievalFrequency must be a number; got {breakdown}"),
    )?;
    ensure(
        breakdown["citationCount"].is_number(),
        format!("breakdown.citationCount must be a number; got {breakdown}"),
    )?;
    Ok(())
}

#[test]
fn economy_simulate_rejects_zero_baseline_budget_with_usage_error() -> TestResult {
    let ws_dir = unique_workspace("simulate-zero-budget")?;
    let ws = ws_str(&ws_dir)?;
    init_workspace(&ws)?;

    let out = run_ee(&[
        "--workspace",
        &ws,
        "--json",
        "economy",
        "simulate",
        "--baseline-budget",
        "0",
    ])?;
    ensure(
        !out.status.success(),
        format!(
            "economy simulate --baseline-budget 0 must exit non-zero; stdout={}",
            String::from_utf8_lossy(&out.stdout)
        ),
    )?;

    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("economy simulate stdout must be JSON: {e}"))?;

    assert_error_envelope(&parsed)?;

    let message = parsed["error"]["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("greater than zero"),
        format!("usage error must mention 'greater than zero'; got {message:?}"),
    )?;

    let repair = parsed["error"]["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee economy simulate"),
        format!("error repair must reference ee economy simulate; got {repair:?}"),
    )?;
    Ok(())
}
