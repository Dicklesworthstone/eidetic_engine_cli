//! bd-1idcb Phase 2 E2E gate: end-to-end determinism + populated-
//! recommendations contract for `ee focus suggest`.
//!
//! Acceptance points from the bead:
//! 1. Real persisted-data E2E covering happy path (recommendations
//!    returned) and the `--from-cass` toggle.
//! 2. Determinism gate: identical workspace + identical
//!    `--recent-hours` window → identical recommendations ordering
//!    (hash parity).
//! 3. The `focus_suggest_unimplemented` sentinel must NOT appear in
//!    production emissions.
//!
//! The test drives the real `ee` binary against a freshly-initialized
//! workspace + a small set of `ee remember` invocations, then asserts
//! the v1 envelope is non-empty and stable across two runs.

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
        .join("ee-focus-suggest-phase2")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn must_succeed(output: &Output, context: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{context} must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn seed_workspace(workspace_arg: &str) -> TestResult {
    must_succeed(
        &run_ee(&["--workspace", workspace_arg, "init", "--json"])?,
        "ee init",
    )?;
    // Seed a few memories so the surface has real signal. The kind
    // string + content prefix determine the topic-clustering key, so
    // memories with the same `kind` and similar prefixes cluster
    // together. We seed two distinct kinds to verify the surface
    // returns multiple topics deterministically.
    for (kind, content) in [
        ("release", "release readiness gate checklist"),
        ("release", "release readiness audit cadence"),
        ("decision", "adopt asupersync runtime"),
        ("decision", "adopt frankensearch hybrid retrieval"),
    ] {
        must_succeed(
            &run_ee(&[
                "--workspace",
                workspace_arg,
                "remember",
                "--level",
                "episodic",
                "--kind",
                kind,
                "--json",
                content,
            ])?,
            &format!("ee remember kind={kind}"),
        )?;
    }
    Ok(())
}

#[test]
fn focus_suggest_phase2_non_empty_recommendations() -> TestResult {
    let workspace = unique_workspace("happy")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    seed_workspace(&workspace_arg)?;

    let output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "focus",
        "suggest",
        "--limit",
        "10",
        "--recent-hours",
        "24",
    ])?;
    must_succeed(&output, "ee focus suggest")?;

    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("focus suggest stdout must be JSON: {e}"))?;
    let recommendations = parsed["data"]["recommendations"]
        .as_array()
        .ok_or_else(|| {
            format!(
                "recommendations must be an array; got {:?}",
                parsed["data"]["recommendations"]
            )
        })?;
    ensure(
        !recommendations.is_empty(),
        format!(
            "Phase 2 must return at least one recommendation when the workspace has recent memories; got {recommendations:?}"
        ),
    )?;

    // Each recommendation must satisfy the v1 schema's required fields.
    for (idx, rec) in recommendations.iter().enumerate() {
        let topic = rec["topic"]
            .as_str()
            .ok_or_else(|| format!("recommendations[{idx}].topic must be a string; got {rec:?}"))?;
        ensure(
            !topic.is_empty(),
            format!("recommendations[{idx}].topic must be non-empty"),
        )?;
        ensure(
            rec["spanIds"].is_array(),
            format!("recommendations[{idx}].spanIds must be an array; got {rec:?}"),
        )?;
        ensure(
            rec["centralityScore"].is_number(),
            format!("recommendations[{idx}].centralityScore must be a number; got {rec:?}"),
        )?;
        let rationale = rec["rationale"].as_str().ok_or_else(|| {
            format!("recommendations[{idx}].rationale must be a string; got {rec:?}")
        })?;
        ensure(
            !rationale.is_empty(),
            format!("recommendations[{idx}].rationale must be non-empty"),
        )?;
        let query = rec["suggestedQuery"].as_str().ok_or_else(|| {
            format!("recommendations[{idx}].suggestedQuery must be a string; got {rec:?}")
        })?;
        ensure(
            query.contains("ee context") || query.contains("ee search"),
            format!(
                "recommendations[{idx}].suggestedQuery must reference ee context/search; got {query:?}"
            ),
        )?;
    }

    // Phase 2 retired the honesty-only sentinel. It must not appear.
    let degraded = parsed["degraded"]
        .as_array()
        .ok_or_else(|| format!("degraded must be an array; got {:?}", parsed["degraded"]))?;
    let sentinel = degraded
        .iter()
        .find(|entry| entry["code"].as_str() == Some("focus_suggest_unimplemented"));
    ensure(
        sentinel.is_none(),
        format!("Phase 1 focus_suggest_unimplemented sentinel must NOT appear; got {degraded:?}"),
    )
}

#[test]
fn focus_suggest_phase2_deterministic_across_runs() -> TestResult {
    let workspace = unique_workspace("determinism")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    seed_workspace(&workspace_arg)?;

    let args = [
        "--workspace",
        &workspace_arg,
        "--json",
        "focus",
        "suggest",
        "--limit",
        "5",
        "--recent-hours",
        "24",
    ];
    let first = run_ee(&args)?;
    must_succeed(&first, "ee focus suggest (run 1)")?;
    let second = run_ee(&args)?;
    must_succeed(&second, "ee focus suggest (run 2)")?;

    // Parse both and compare the `data.recommendations` arrays
    // structurally. The recency-weighted exp() in the scoring path can
    // emit tiny float drift between runs as the wall-clock advances —
    // determinism is on RECOMMENDATION ORDER + topic identities, not
    // on the exact numeric scores. (The schema is identical either
    // way; the byte-for-byte determinism gate is exercised by the
    // J7 in-process determinism harness.)
    let first_parsed: Value = serde_json::from_slice(&first.stdout)
        .map_err(|e| format!("run 1 stdout must be JSON: {e}"))?;
    let second_parsed: Value = serde_json::from_slice(&second.stdout)
        .map_err(|e| format!("run 2 stdout must be JSON: {e}"))?;

    let first_topics: Vec<&str> = first_parsed["data"]["recommendations"]
        .as_array()
        .map(|recs| {
            recs.iter()
                .filter_map(|r| r["topic"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let second_topics: Vec<&str> = second_parsed["data"]["recommendations"]
        .as_array()
        .map(|recs| {
            recs.iter()
                .filter_map(|r| r["topic"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ensure(
        first_topics == second_topics,
        format!(
            "topic ordering must be deterministic across runs; first={first_topics:?} second={second_topics:?}"
        ),
    )?;

    // Cluster identity is also a determinism property: same workspace
    // must yield the same set of (topic, spanIds) pairs irrespective
    // of run-to-run float drift.
    let first_spans: Vec<&Value> = first_parsed["data"]["recommendations"]
        .as_array()
        .map(|recs| recs.iter().map(|r| &r["spanIds"]).collect::<Vec<_>>())
        .unwrap_or_default();
    let second_spans: Vec<&Value> = second_parsed["data"]["recommendations"]
        .as_array()
        .map(|recs| recs.iter().map(|r| &r["spanIds"]).collect::<Vec<_>>())
        .unwrap_or_default();
    ensure(
        first_spans == second_spans,
        format!(
            "spanIds per recommendation must be deterministic; first={first_spans:?} second={second_spans:?}"
        ),
    )
}

#[test]
fn focus_suggest_from_cass_toggle_changes_pipeline_state() -> TestResult {
    let workspace = unique_workspace("fromcass")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    seed_workspace(&workspace_arg)?;

    let without_cass = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "focus",
        "suggest",
        "--limit",
        "5",
    ])?;
    must_succeed(&without_cass, "ee focus suggest (default)")?;
    let with_cass = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "focus",
        "suggest",
        "--from-cass",
        "--limit",
        "5",
    ])?;
    must_succeed(&with_cass, "ee focus suggest --from-cass")?;

    let parsed_default: Value = serde_json::from_slice(&without_cass.stdout)
        .map_err(|e| format!("default stdout must be JSON: {e}"))?;
    let parsed_cass: Value = serde_json::from_slice(&with_cass.stdout)
        .map_err(|e| format!("--from-cass stdout must be JSON: {e}"))?;

    // The fromCass envelope field must echo the flag in both cases.
    ensure(
        parsed_default["data"]["fromCass"].as_bool() == Some(false),
        format!(
            "default fromCass must be false; got {:?}",
            parsed_default["data"]["fromCass"]
        ),
    )?;
    ensure(
        parsed_cass["data"]["fromCass"].as_bool() == Some(true),
        format!(
            "--from-cass fromCass must be true; got {:?}",
            parsed_cass["data"]["fromCass"]
        ),
    )?;

    // The data.schema is unchanged across the toggle.
    ensure(
        parsed_default["data"]["schema"].as_str() == Some("ee.focus.suggest.v1")
            && parsed_cass["data"]["schema"].as_str() == Some("ee.focus.suggest.v1"),
        "data.schema must be ee.focus.suggest.v1 in both branches".to_owned(),
    )
}
