//! Gate 16 prospective preflight and tripwire readiness contracts.
//!
//! These fixtures freeze the exact contract artifacts named by the readiness
//! gate while the public CLI remains covered by `tests/golden.rs` and the
//! logged user scenarios. The reports are deterministic core outputs with
//! dynamic IDs and timestamps normalized.

use ee::core::feedback::{PreflightFeedbackKind, TaskOutcome};
use ee::core::preflight::{
    CloseOptions as PreflightCloseOptions, RunOptions as PreflightRunOptions,
    ShowOptions as PreflightShowOptions, TripwireGenerationConfig, TripwireSource, close_preflight,
    run_preflight, show_preflight,
};
use ee::core::tripwire::{
    CheckOptions as TripwireCheckOptions, ConditionEvaluationResult,
    ListOptions as TripwireListOptions, TripwireEventPayload, check_tripwire,
    evaluate_tripwire_condition, list_tripwires,
};
use ee::models::preflight::TripwireState;
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

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn exact_golden_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(relative)
}

fn pretty_json(value: &JsonValue) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| format!("json render failed: {error}"))
}

fn assert_exact_golden(relative: &str, actual: &str) -> TestResult {
    let path = exact_golden_path(relative);
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing Gate 16 golden {}: {error}", path.display()))?;
    let expected = expected.strip_suffix('\n').unwrap_or(&expected);
    ensure(
        actual == expected,
        format!(
            "Gate 16 golden mismatch for {relative}\n--- expected\n{expected}\n+++ actual\n{actual}"
        ),
    )
}

fn assert_cli_json_stdout_clean(args: &[&str], display: &str, code: &str) -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {display}: {error}"))?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {display}: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {display}: {error}"))?;

    ensure(
        output.status.code() == Some(7),
        format!("ee {display} should degrade, stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("ee {display} must keep diagnostics out of stderr"),
    )?;
    ensure(
        stdout.starts_with('{') && stdout.ends_with('\n'),
        format!("ee {display} stdout must be newline-terminated JSON"),
    )?;
    let value: JsonValue = serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not parseable JSON: {error}"))?;
    ensure_equal(
        &value["schema"],
        &serde_json::json!("ee.response.v1"),
        "public CLI degraded schema",
    )?;
    ensure_equal(
        &value["success"],
        &serde_json::json!(false),
        "public CLI degraded success flag",
    )?;
    ensure_equal(
        &value["data"]["code"],
        &serde_json::json!(code),
        "public CLI degraded code",
    )?;
    Ok(())
}

fn normalized(mut value: JsonValue) -> Result<String, String> {
    normalize_value(&mut value);
    pretty_json(&value)
}

fn normalize_value(value: &mut JsonValue) {
    match value {
        JsonValue::Object(object) => {
            for (key, nested) in object.iter_mut() {
                normalize_field(key, nested);
                normalize_value(nested);
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                normalize_value(item);
            }
        }
        _ => {}
    }
}

fn normalize_field(key: &str, value: &mut JsonValue) {
    const TIMESTAMP_FIELDS: &[&str] = &[
        "started_at",
        "completed_at",
        "closed_at",
        "listed_at",
        "checked_at",
        "created_at",
        "last_checked_at",
        "triggered_at",
        "recorded_at",
    ];

    if TIMESTAMP_FIELDS.contains(&key) && value.is_string() {
        *value = serde_json::json!("TIMESTAMP");
        return;
    }

    if matches!(key, "run_id" | "preflight_run_id")
        && value
            .as_str()
            .is_some_and(|raw| raw.starts_with("pf_") && raw.contains('-'))
    {
        *value = serde_json::json!("pf_DYNAMIC");
        return;
    }

    if key == "risk_brief_id" && value.is_string() {
        *value = serde_json::json!("rb_DYNAMIC");
        return;
    }

    if key == "id"
        && value
            .as_str()
            .is_some_and(|raw| raw.starts_with("tw_") && raw.len() > "tw_004".len())
    {
        *value = serde_json::json!("tw_DYNAMIC");
    }
}

fn release_tripwire_sources() -> Vec<TripwireSource> {
    vec![TripwireSource::dependency_contract(
        "dep_no_tokio",
        "Forbidden async runtime dependency must not appear in release work",
        true,
        ["deploy", "migration", "production"],
    )]
}

#[test]
fn release_task_preflight_matches_exact_gate_golden() -> TestResult {
    let report = run_preflight(&PreflightRunOptions {
        task_input: "deploy production database migration".to_owned(),
        tripwire_sources: release_tripwire_sources(),
        tripwire_generation: TripwireGenerationConfig::default(),
        ..Default::default()
    })
    .map_err(|error| error.message())?;

    ensure_equal(&report.risk_level, &"high".to_owned(), "risk level")?;
    ensure_equal(&report.cleared, &false, "high-risk run is not auto-cleared")?;
    ensure_equal(&report.tripwires_set, &1, "generated tripwire count")?;
    ensure(
        report.evidence_ids.contains(&"dep_no_tokio".to_owned()),
        "preflight report must include evidence ids",
    )?;
    ensure(
        report.next_action == "verify_must_checks_before_proceeding",
        "preflight report must include next action",
    )?;

    let actual = normalized(serde_json::to_value(report).map_err(|error| error.to_string())?)?;
    assert_exact_golden("preflight/release_task.json", &actual)
}

#[test]
fn degraded_evidence_preflight_matches_exact_gate_golden() -> TestResult {
    let report = show_preflight(&PreflightShowOptions {
        run_id: "pf_gate16_contract".to_owned(),
        ..Default::default()
    })
    .map_err(|error| error.message())?;

    ensure(
        report
            .degraded
            .iter()
            .any(|entry| entry.code == "preflight_evidence_stale"),
        "show preflight must expose preflight_evidence_stale",
    )?;

    let actual = normalized(serde_json::to_value(report).map_err(|error| error.to_string())?)?;
    assert_exact_golden("preflight/degraded_evidence.json", &actual)
}

#[test]
fn false_alarm_close_matches_exact_gate_golden() -> TestResult {
    let report = close_preflight(&PreflightCloseOptions {
        run_id: "pf_gate16_contract".to_owned(),
        cleared: true,
        reason: Some("task completed without needing the warning".to_owned()),
        task_outcome: Some(TaskOutcome::Success),
        feedback_kind: Some(PreflightFeedbackKind::FalseAlarm),
        dry_run: true,
        ..Default::default()
    })
    .map_err(|error| error.message())?;

    let feedback = report
        .feedback
        .as_ref()
        .ok_or_else(|| "false-alarm close missing feedback".to_owned())?;
    ensure_equal(
        &feedback.feedback_kind,
        &Some("false_alarm".to_owned()),
        "feedback kind",
    )?;
    ensure_equal(&feedback.durable_mutation, &false, "dry-run mutation")?;
    ensure_equal(&feedback.evidence_preserved, &true, "evidence preserved")?;

    let actual = normalized(serde_json::to_value(report).map_err(|error| error.to_string())?)?;
    assert_exact_golden("preflight/false_alarm_close.json", &actual)
}

#[test]
fn tripwire_list_matches_exact_gate_golden() -> TestResult {
    let report = list_tripwires(&TripwireListOptions {
        state: Some(TripwireState::Triggered),
        include_disarmed: true,
        ..Default::default()
    })
    .map_err(|error| error.message())?;

    ensure_equal(&report.triggered_count, &0, "triggered count")?;
    ensure(
        report.tripwires.is_empty(),
        "unwired tripwire store must return an empty list, not samples",
    )?;

    let actual = normalized(serde_json::to_value(report).map_err(|error| error.to_string())?)?;
    assert_exact_golden("tripwire/list.json", &actual)
}

#[test]
fn tripwire_check_sample_id_reports_not_found_without_feedback() -> TestResult {
    let report = check_tripwire(&TripwireCheckOptions {
        tripwire_id: "tw_004".to_owned(),
        task_outcome: Some(TaskOutcome::Success),
        dry_run: true,
        ..Default::default()
    })
    .map_err(|error| error.message())?;

    ensure_equal(&report.result.as_str(), &"not_found", "sample id rejected")?;
    ensure_equal(&report.should_halt, &false, "no halt without tripwire")?;
    ensure(
        report.feedback.is_none(),
        "not-found checks must not record feedback against sample data",
    )?;
    ensure(
        report
            .degraded
            .iter()
            .any(|entry| entry.code == "tripwire_inputs_incomplete"),
        "sample ID must expose tripwire_inputs_incomplete",
    )?;

    let actual = normalized(serde_json::to_value(report).map_err(|error| error.to_string())?)?;
    assert_exact_golden("tripwire/check_match.json", &actual)
}

#[test]
fn tripwire_check_no_match_matches_exact_gate_golden() -> TestResult {
    let report = check_tripwire(&TripwireCheckOptions {
        tripwire_id: "tw_missing".to_owned(),
        dry_run: true,
        ..Default::default()
    })
    .map_err(|error| error.message())?;

    ensure_equal(&report.result.as_str(), &"not_found", "not-found result")?;
    ensure(
        report
            .degraded
            .iter()
            .any(|entry| entry.code == "tripwire_inputs_incomplete"),
        "missing tripwire must expose tripwire_inputs_incomplete",
    )?;

    let actual = normalized(serde_json::to_value(report).map_err(|error| error.to_string())?)?;
    assert_exact_golden("tripwire/check_no_match.json", &actual)
}

#[test]
fn tripwire_condition_evaluator_uses_explicit_payloads() -> TestResult {
    let payload =
        TripwireEventPayload::default().with_task_input("deploy production database migration");
    let generated_condition = "task_contains_any(\"deploy\", \"migration\")";

    let matched = evaluate_tripwire_condition(generated_condition, &payload);
    let repeated = evaluate_tripwire_condition(generated_condition, &payload);

    ensure_equal(
        &matched.result,
        &ConditionEvaluationResult::Satisfied,
        "generated task-term condition result",
    )?;
    ensure_equal(
        &matched.matched_terms,
        &vec!["deploy".to_owned(), "migration".to_owned()],
        "matched generated terms",
    )?;
    ensure_equal(&matched, &repeated, "deterministic repeated evaluation")?;

    let missing =
        evaluate_tripwire_condition(generated_condition, &TripwireEventPayload::default());
    ensure_equal(
        &missing.result,
        &ConditionEvaluationResult::MissingInput,
        "missing task input result",
    )?;

    let unsupported = evaluate_tripwire_condition(
        "error_count < 3",
        &TripwireEventPayload::default().with_task_input("deploy"),
    );
    ensure_equal(
        &unsupported.result,
        &ConditionEvaluationResult::UnsupportedCondition,
        "unsupported condition result",
    )
}

#[test]
fn public_cli_preflight_and_tripwire_json_keep_stdout_clean() -> TestResult {
    assert_cli_json_stdout_clean(
        &[
            "--json",
            "preflight",
            "run",
            "deploy production database migration",
        ],
        "--json preflight run deploy production database migration",
        "preflight_evidence_unavailable",
    )?;
    assert_cli_json_stdout_clean(
        &["--json", "tripwire", "check", "tw_004", "--dry-run"],
        "--json tripwire check tw_004 --dry-run",
        "tripwire_store_unavailable",
    )
}
