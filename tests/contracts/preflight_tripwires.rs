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
    CheckOptions as TripwireCheckOptions, CheckResult, ConditionEvaluationResult,
    ListOptions as TripwireListOptions, TripwireEventPayload, check_tripwire,
    evaluate_tripwire_condition, list_tripwires,
};
use ee::db::{CreateTripwireInput, CreateWorkspaceInput, DbConnection};
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
        output.status.code() == Some(6),
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

fn assert_cli_tripwire_not_found_stdout_clean(args: &[&str], display: &str) -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {display}: {error}"))?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {display}: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {display}: {error}"))?;

    ensure(
        output.status.success(),
        format!("ee {display} should succeed, stderr: {stderr}"),
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
        &serde_json::json!("ee.tripwire.check.v1"),
        "public CLI tripwire schema",
    )?;
    ensure_equal(
        &value["result"],
        &serde_json::json!("not_found"),
        "public CLI tripwire result",
    )?;
    ensure(
        value["degraded"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["code"] == serde_json::json!("tripwire_inputs_incomplete"))
        }),
        "public CLI tripwire not_found must include tripwire_inputs_incomplete",
    )
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

    ensure_equal(&report.risk_level, &"critical".to_owned(), "risk level")?;
    ensure_equal(&report.cleared, &false, "high-risk run is not auto-cleared")?;
    ensure_equal(&report.tripwires_set, &1, "generated tripwire count")?;
    ensure_equal(
        &report.tripwires[0].source_kind,
        &Some("dependency_contract".to_owned()),
        "generated tripwire source kind",
    )?;
    ensure_equal(
        &report.tripwires[0].source_id,
        &Some("dep_no_tokio".to_owned()),
        "generated tripwire source id",
    )?;
    ensure_equal(
        &report.tripwires[0].source_score,
        &Some(1.0),
        "generated tripwire source score",
    )?;
    ensure_equal(
        &report.tripwires[0].trigger_terms,
        &vec![
            "deploy".to_owned(),
            "migration".to_owned(),
            "production".to_owned(),
        ],
        "generated tripwire trigger terms",
    )?;
    ensure(
        report.tripwires[0]
            .provenance
            .contains(&"source_score=1.000".to_owned()),
        "generated tripwire provenance must include source score",
    )?;
    ensure(
        report.evidence_ids.contains(&"dep_no_tokio".to_owned()),
        "preflight report must include evidence ids",
    )?;
    ensure(
        report.ask_now_prompts.is_empty(),
        "preflight core must not emit ask-now prompts",
    )?;
    ensure(
        report
            .must_verify_checks
            .iter()
            .any(|check| check.contains("dependency_contract:dep_no_tokio")),
        "preflight checks must cite explicit evidence sources",
    )?;
    ensure(
        report.next_action == "review_evidence_matches_before_proceeding",
        "preflight report must include next action",
    )?;

    let actual = normalized(serde_json::to_value(report).map_err(|error| error.to_string())?)?;
    assert_exact_golden("preflight/release_task.json", &actual)
}

#[test]
fn show_preflight_returns_not_found_without_persisted_run() -> TestResult {
    let Err(error) = show_preflight(&PreflightShowOptions {
        run_id: "pf_gate16_contract".to_owned(),
        ..Default::default()
    }) else {
        return Err("show must not fabricate a stored preflight run".to_owned());
    };

    ensure_equal(&error.code(), &"not_found", "show not found code")
}

#[test]
fn close_preflight_returns_not_found_without_feedback() -> TestResult {
    let Err(error) = close_preflight(&PreflightCloseOptions {
        run_id: "pf_gate16_contract".to_owned(),
        cleared: true,
        reason: Some("task completed without needing the warning".to_owned()),
        task_outcome: Some(TaskOutcome::Success),
        feedback_kind: Some(PreflightFeedbackKind::FalseAlarm),
        dry_run: true,
        ..Default::default()
    }) else {
        return Err("close must not record feedback for a missing preflight run".to_owned());
    };

    ensure_equal(&error.code(), &"not_found", "close not found code")
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
fn tripwire_store_check_uses_persisted_rows_and_logs_event() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().join("workspace");
    let ee_dir = workspace.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| format!("mkdir .ee: {error}"))?;
    let database_path = ee_dir.join("ee.db");
    let workspace_path = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?;
    let workspace_path_string = workspace_path.to_string_lossy().into_owned();

    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open tripwire test db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate tripwire test db: {error}"))?;
    connection
        .insert_workspace(
            "wsp_01234567890123456789012345",
            &CreateWorkspaceInput {
                path: workspace_path_string,
                name: Some("workspace".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;
    connection
        .insert_tripwire(
            "tw_gate16_store_001",
            &CreateTripwireInput {
                workspace_id: "wsp_01234567890123456789012345".to_owned(),
                preflight_run_id: "pf_gate16_store".to_owned(),
                tripwire_type: "custom".to_owned(),
                condition: "task_contains_any(\"deploy\")".to_owned(),
                action: "halt".to_owned(),
                state: "armed".to_owned(),
                message: Some("deployment risk".to_owned()),
                created_at: "2026-05-03T20:00:00Z".to_owned(),
                last_checked_at: None,
                triggered_at: None,
            },
        )
        .map_err(|error| format!("insert tripwire: {error}"))?;
    connection
        .close()
        .map_err(|error| format!("close seeded tripwire db: {error}"))?;

    let listed = list_tripwires(&TripwireListOptions {
        workspace: workspace_path.clone(),
        database_path: Some(database_path.clone()),
        preflight_run_id: Some("pf_gate16_store".to_owned()),
        ..Default::default()
    })
    .map_err(|error| error.message())?;
    ensure_equal(&listed.total_count, &1, "stored list count")?;
    ensure_equal(
        &listed.tripwires[0].id,
        &"tw_gate16_store_001".to_owned(),
        "stored list id",
    )?;

    let checked = check_tripwire(&TripwireCheckOptions {
        workspace: workspace_path,
        database_path: Some(database_path.clone()),
        tripwire_id: "tw_gate16_store_001".to_owned(),
        event_payload: TripwireEventPayload::default().with_task_input("deploy release"),
        update_timestamp: true,
        dry_run: false,
        ..Default::default()
    })
    .map_err(|error| error.message())?;
    ensure_equal(&checked.result, &CheckResult::Triggered, "check result")?;
    ensure_equal(&checked.should_halt, &true, "halt decision")?;
    ensure_equal(&checked.durable_mutation, &true, "durable mutation")?;
    ensure(
        checked
            .event_payload_hash
            .as_deref()
            .is_some_and(|hash| hash.starts_with("blake3:")),
        "event payload hash",
    )?;

    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("reopen tripwire test db: {error}"))?;
    let stored = connection
        .get_tripwire("tw_gate16_store_001")
        .map_err(|error| format!("read updated tripwire: {error}"))?
        .ok_or_else(|| "updated tripwire missing".to_owned())?;
    ensure_equal(&stored.state, &"triggered".to_owned(), "persisted state")?;
    let events = connection
        .list_tripwire_check_events("tw_gate16_store_001")
        .map_err(|error| format!("list check events: {error}"))?;
    ensure_equal(&events.len(), &1, "logged check events")?;
    ensure_equal(
        &events[0].check_result,
        &"triggered".to_owned(),
        "logged result",
    )?;
    ensure_equal(&events[0].should_halt, &true, "logged halt decision")?;
    ensure_equal(
        &events[0].mutation_posture,
        &"state_update_and_check_event_persisted".to_owned(),
        "logged mutation posture",
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
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let database_path = tempdir.path().join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open public CLI tripwire db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate public CLI tripwire db: {error}"))?;
    connection
        .close()
        .map_err(|error| format!("close public CLI tripwire db: {error}"))?;
    let database_path = database_path.to_string_lossy().into_owned();
    assert_cli_tripwire_not_found_stdout_clean(
        &[
            "--json",
            "tripwire",
            "check",
            "tw_004",
            "--database",
            &database_path,
            "--dry-run",
        ],
        "--json tripwire check tw_004 --database <path> --dry-run",
    )
}
