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
        // Per AGENTS.md, `ee pack` is the canonical context-pack
        // surface, so Phase 2 of focus_suggest emits `ee pack` for an
        // agent acting on a recommendation. `ee search` is also
        // acceptable for queries that want the raw retrieval rather
        // than a packed surface.
        ensure(
            query.contains("ee pack") || query.contains("ee search"),
            format!(
                "recommendations[{idx}].suggestedQuery must reference `ee pack` or `ee search`; got {query:?}"
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

/// Flag-echo / schema-stability assertion only.
///
/// This test seeds the workspace with `ee remember` but does NOT seed any
/// CASS evidence_spans, so the `--from-cass` branch and the default branch
/// produce structurally identical recommendations. The narrow contract
/// pinned here is just that:
///   - `data.fromCass` echoes back the flag (false vs true), and
///   - `data.schema` stays at `ee.focus.suggest.v1` across the toggle.
///
/// A real pipeline-divergence test (where `--from-cass` populates non-empty
/// `spanIds`) needs evidence_spans seeded via `ee import cass` or an
/// equivalent fixture, and is intentionally out of scope here.
#[test]
fn focus_suggest_from_cass_flag_echoes_to_response() -> TestResult {
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

/// `--task-frame <nonexistent>` must honor the explicit scope.
///
/// The Phase 2 surface is documented as scope-honoring: when the user
/// passes a `--task-frame` id, the recommendations[] must be drawn from
/// the frame's evidence_links neighborhood, not from every recent
/// memory. The original Phase 2 wire-up at src/core/focus_suggest.rs
/// already honors that intent in the `Ok(empty)` arm (frame has no
/// evidence_links) by early-returning empty recommendations. The
/// symmetric `Err` arm (frame id typo'd, task-frame store missing,
/// store corrupt) historically fell back to ALL recent memories,
/// silently broadening the scope the caller asked to narrow.
///
/// This test pins the fix: a `--task-frame` id that cannot be resolved
/// must produce an empty recommendations[] AND a `task_frame_unavailable`
/// degraded entry, even when the workspace has plenty of recent
/// memories that would otherwise satisfy the surface.
#[test]
fn focus_suggest_task_frame_unavailable_honors_scope() -> TestResult {
    let workspace = unique_workspace("taskframe-unavailable")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    seed_workspace(&workspace_arg)?;

    // Sanity check: without `--task-frame`, the seeded workspace MUST
    // return at least one recommendation. This proves the "no
    // recommendations" assertion below comes from the task-frame scope
    // honoring path, not from an unrelated empty-workspace state.
    let unscoped = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "focus",
        "suggest",
        "--limit",
        "10",
    ])?;
    must_succeed(&unscoped, "ee focus suggest (unscoped baseline)")?;
    let unscoped_parsed: Value = serde_json::from_slice(&unscoped.stdout)
        .map_err(|e| format!("unscoped stdout must be JSON: {e}"))?;
    let unscoped_recommendations = unscoped_parsed["data"]["recommendations"]
        .as_array()
        .ok_or_else(|| "unscoped recommendations must be an array".to_string())?;
    ensure(
        !unscoped_recommendations.is_empty(),
        "baseline must populate recommendations[]; seed_workspace contract violated".to_owned(),
    )?;

    // Now drive the surface with a `--task-frame` id that will fail to
    // resolve. The workspace has never run `ee task-frame`, so the
    // task-frame store does not exist; `show_task_frame` errors, and
    // `load_task_frame_evidence` propagates the error to the Err arm.
    let scoped = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "focus",
        "suggest",
        "--task-frame",
        "tf_does_not_exist_12345",
        "--limit",
        "10",
    ])?;
    must_succeed(&scoped, "ee focus suggest --task-frame nonexistent")?;

    let parsed: Value = serde_json::from_slice(&scoped.stdout)
        .map_err(|e| format!("scoped stdout must be JSON: {e}"))?;
    let recommendations = parsed["data"]["recommendations"]
        .as_array()
        .ok_or_else(|| {
            format!(
                "scoped recommendations must be an array; got {:?}",
                parsed["data"]["recommendations"]
            )
        })?;
    ensure(
        recommendations.is_empty(),
        format!(
            "explicit --task-frame scope must yield empty recommendations[] when the frame cannot be resolved; \
             silently broadening to ALL recent memories defeats the scope. Got {recommendations:?}"
        ),
    )?;

    // The unavailable signal must surface as a degraded entry so the
    // caller can distinguish "frame typo / missing store" from "frame
    // exists but had no evidence_links" (task_frame_no_evidence).
    let degraded = parsed["degraded"]
        .as_array()
        .ok_or_else(|| format!("degraded must be an array; got {:?}", parsed["degraded"]))?;
    let unavailable_entry = degraded
        .iter()
        .find(|entry| entry["code"].as_str() == Some("task_frame_unavailable"))
        .ok_or_else(|| {
            format!(
                "task_frame_unavailable degraded entry must appear when --task-frame cannot be resolved; \
                 got degraded={degraded:?}"
            )
        })?;
    ensure(
        unavailable_entry["severity"].as_str() == Some("warning"),
        format!(
            "task_frame_unavailable severity must be warning; got {:?}",
            unavailable_entry["severity"]
        ),
    )?;
    ensure(
        unavailable_entry["repair"]
            .as_str()
            .is_some_and(|r| r.contains("ee task-frame")),
        format!(
            "task_frame_unavailable repair hint must reference `ee task-frame`; got {:?}",
            unavailable_entry["repair"]
        ),
    )?;

    // The `no_recent_evidence` code must NOT also appear: emitting both
    // would tell the caller two contradictory stories about why the
    // recommendations are empty (frame-scope vs empty-workspace).
    let no_recent = degraded
        .iter()
        .find(|entry| entry["code"].as_str() == Some("no_recent_evidence"));
    ensure(
        no_recent.is_none(),
        format!(
            "task_frame_unavailable must short-circuit before the no_recent_evidence check; \
             got both codes in degraded={degraded:?}"
        ),
    )
}

/// `--recent-hours <u32::MAX>` must NOT panic the binary.
///
/// `recent_hours: u32` accepts up to u32::MAX (~489_957 years).
/// Subtracting that duration from `Utc::now()` via the unchecked
/// `now - Duration::hours(...)` path overflows `DateTime<Utc>`'s
/// representable range and panics chrono — turning a benign CLI flag
/// into a denial-of-service for the local CLI. The fix uses
/// `checked_sub_signed` with a `MIN_UTC` fallback and surfaces a
/// `recent_hours_window_clamped` degraded entry.
///
/// This E2E proves the panic is no longer reachable through the real
/// binary surface: the process exits zero, emits valid JSON, and (when
/// the overflow clamp fires) carries the documented degraded code.
/// Note that `DateTime<Utc>` may span the resulting year on some
/// configurations, in which case the overflow clamp does not fire and
/// the degraded entry is simply absent — the panic-absence is the
/// load-bearing assertion either way.
#[test]
fn focus_suggest_recent_hours_u32_max_does_not_panic() -> TestResult {
    let workspace = unique_workspace("recent-hours-overflow")?;
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
        "--recent-hours",
        "4294967295", // u32::MAX
        "--limit",
        "5",
    ])?;
    // Load-bearing assertion: NO panic. A pre-fix `cargo run -- focus
    // suggest --recent-hours 4294967295` produced an abort with
    // `thread 'main' panicked at ...DateTime...`; that path must be
    // unreachable now.
    must_succeed(
        &output,
        "ee focus suggest --recent-hours 4294967295 must not panic",
    )?;

    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("stdout must remain valid JSON even at the overflow boundary: {e}"))?;
    ensure(
        parsed["data"]["schema"].as_str() == Some("ee.focus.suggest.v1"),
        format!(
            "envelope must keep the v1 data schema; got {:?}",
            parsed["data"]["schema"]
        ),
    )
}
