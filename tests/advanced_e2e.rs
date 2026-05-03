//! EE-TST-005: Advanced Subsystem End-to-End Tests
//!
//! Tests for recorder, preflight, procedure, economy, learning, and causal subsystems.
//! Each test validates JSON output contracts, dry-run behavior, and stdout/stderr isolation.

use std::fmt::Debug;
use std::fs;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const UNSATISFIED_DEGRADED_MODE_EXIT: i32 = 7;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn stdout_is_json(output: &Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout).is_ok()
}

fn stdout_has_schema(output: &Output, expected: &str) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
        json.get("schema")
            .is_some_and(|s| s.as_str() == Some(expected))
    } else {
        false
    }
}

fn stdout_contains(output: &Output, needle: &str) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains(needle)
}

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout).map_err(|error| format!("stdout was not JSON: {error}"))
}

fn stdout_is_clean(output: &Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[INFO]")
            || trimmed.starts_with("[WARN]")
            || trimmed.starts_with("[ERROR]")
            || trimmed.starts_with("warning:")
            || trimmed.starts_with("error:")
        {
            return false;
        }
    }
    true
}

// ============================================================================
// Recorder Tests (EE-401)
// ============================================================================

#[test]
fn recorder_start_dry_run_returns_valid_json() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "start",
        "--agent-id",
        "test-agent",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_has_schema(&output, "ee.recorder.start.v1"),
        "must have recorder start schema",
    )?;
    ensure(stdout_contains(&output, "runId"), "must contain runId")?;
    ensure(stdout_contains(&output, "dryRun"), "must contain dryRun")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn recorder_start_creates_run_id() -> TestResult {
    let output = run_ee(&["recorder", "start", "--agent-id", "test-agent", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "run_"),
        "runId must start with run_",
    )
}

#[test]
fn recorder_event_requires_run_id() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "event",
        "run_test_123",
        "--event-type",
        "tool_call",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_has_schema(&output, "ee.recorder.event_response.v1"),
        "must have recorder event schema",
    )?;
    ensure(stdout_contains(&output, "eventId"), "must contain eventId")?;
    ensure(
        stdout_contains(&output, "sequence"),
        "must contain sequence",
    )
}

#[test]
fn recorder_event_supports_redaction() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "event",
        "run_test_123",
        "--event-type",
        "user_message",
        "--payload",
        "password marker token marker",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "redactionStatus"),
        "must contain redactionStatus",
    )?;
    let value = stdout_json(&output)?;
    ensure_equal(
        &value["redactionStatus"],
        &serde_json::json!("full"),
        "redaction status",
    )?;
    ensure_equal(
        &value["redactionClasses"],
        &serde_json::json!(["password", "token"]),
        "redaction classes",
    )?;
    ensure_equal(
        &value["redactedBytes"],
        &serde_json::json!(28),
        "redacted bytes",
    )?;
    ensure(
        !String::from_utf8_lossy(&output.stdout).contains("password marker token marker"),
        "stdout must not echo raw payload",
    )
}

#[test]
fn recorder_event_returns_hash_chain_fields() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "event",
        "run_test_123",
        "--event-type",
        "tool_result",
        "--payload",
        "ok",
        "--previous-event-hash",
        "blake3:previous",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let value = stdout_json(&output)?;
    ensure_equal(
        &value["previousEventHash"],
        &serde_json::json!("blake3:previous"),
        "previous hash",
    )?;
    ensure_equal(
        &value["chainStatus"],
        &serde_json::json!("linked"),
        "chain status",
    )?;
    ensure(
        value["eventHash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("blake3:")),
        "event hash must be BLAKE3-prefixed",
    )
}

#[test]
fn recorder_event_rejects_oversized_payload_with_stable_code() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "event",
        "run_test_123",
        "--event-type",
        "tool_call",
        "--payload",
        "0123456789",
        "--max-payload-bytes",
        "4",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(1), "exit code")?;
    let value = stdout_json(&output)?;
    ensure_equal(
        &value["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        &value["error"]["code"],
        &serde_json::json!("recorder_payload_too_large"),
        "error code",
    )?;
    ensure_equal(
        &value["error"]["details"]["payloadBytes"],
        &serde_json::json!(10),
        "payload bytes",
    )?;
    ensure(
        output.stderr.is_empty(),
        "json rejection diagnostics must not use stderr",
    )
}

#[test]
fn recorder_event_validates_event_type() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "event",
        "run_test_123",
        "--event-type",
        "invalid_type",
        "--json",
    ])?;
    ensure_equal(
        &output.status.code(),
        &Some(1),
        "exit code for invalid event type",
    )
}

#[test]
fn recorder_import_dry_run_maps_cass_view_without_mutation() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let input_path = tempdir.path().join("cass-view.json");
    fs::write(
        &input_path,
        r#"{"lines":[{"line":3,"content":"{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"format release\"}}"},{"line":4,"content":"{\"type\":\"tool_use\",\"name\":\"shell\"}"}]}"#,
    )
    .map_err(|error| error.to_string())?;
    let input = input_path.to_string_lossy().into_owned();

    let output = run_ee(&[
        "recorder",
        "import",
        "--source-type",
        "cass",
        "--source-id",
        "/sessions/cass-a.jsonl",
        "--input",
        input.as_str(),
        "--agent-id",
        "codex",
        "--session-id",
        "cass-a",
        "--dry-run",
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(output.stderr.is_empty(), "json dry-run stderr empty")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    let value = stdout_json(&output)?;
    ensure_equal(
        &value["schema"],
        &serde_json::json!("ee.response.v1"),
        "response envelope",
    )?;
    ensure_equal(
        &value["data"]["schema"],
        &serde_json::json!("ee.recorder.import_plan.v1"),
        "import plan schema",
    )?;
    ensure_equal(
        &value["data"]["dryRun"],
        &serde_json::json!(true),
        "dry run",
    )?;
    ensure_equal(
        &value["data"]["summary"]["eventsMapped"],
        &serde_json::json!(2),
        "mapped count",
    )?;
    ensure_equal(
        &value["data"]["events"][0]["eventType"],
        &serde_json::json!("user_message"),
        "first event type",
    )?;
    ensure_equal(
        &value["data"]["events"][1]["eventType"],
        &serde_json::json!("tool_call"),
        "second event type",
    )?;
    ensure_equal(
        &value["data"]["mutations"][0]["action"],
        &serde_json::json!("would_create_run"),
        "run mutation is planned only",
    )?;
    ensure(
        !String::from_utf8_lossy(&output.stdout).contains("format release"),
        "raw CASS payload must not be echoed",
    )
}

#[test]
fn recorder_import_requires_dry_run_with_stable_error_code() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "import",
        "--source-type",
        "cass",
        "--source-id",
        "/sessions/cass-a.jsonl",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(1), "exit code")?;
    ensure(output.stderr.is_empty(), "json error stderr empty")?;
    let value = stdout_json(&output)?;
    ensure_equal(
        &value["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        &value["error"]["code"],
        &serde_json::json!("recorder_import_dry_run_required"),
        "stable error code",
    )
}

#[test]
fn recorder_finish_dry_run_returns_valid_json() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "finish",
        "run_test_123",
        "--status",
        "completed",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_has_schema(&output, "ee.recorder.finish.v1"),
        "must have recorder finish schema",
    )?;
    ensure(stdout_contains(&output, "dryRun"), "must contain dryRun")
}

#[test]
fn recorder_finish_validates_status() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "finish",
        "run_test_123",
        "--status",
        "invalid_status",
        "--json",
    ])?;
    ensure_equal(
        &output.status.code(),
        &Some(1),
        "exit code for invalid status",
    )
}

#[test]
fn recorder_tail_returns_valid_json() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "tail",
        "run_test_123",
        "--limit",
        "10",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_has_schema(&output, "ee.recorder.tail.v1"),
        "must have recorder tail schema",
    )?;
    ensure(stdout_contains(&output, "events"), "must contain events")
}

// ============================================================================
// Procedure Tests (EE-411 - Stub tests for when implemented)
// ============================================================================

#[test]
fn procedure_list_returns_valid_json() -> TestResult {
    let output = run_ee(&["procedure", "list", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn procedure_show_handles_not_found() -> TestResult {
    let output = run_ee(&["procedure", "show", "proc_nonexistent", "--json"])?;
    // May return NotFound (10) or Success with empty result
    ensure(
        output.status.code() == Some(0) || output.status.code() == Some(10),
        "exit code must be 0 or 10 (not found)",
    )?;
    if output.status.code() == Some(0) {
        ensure(stdout_is_json(&output), "stdout must be valid JSON")
    } else {
        Ok(())
    }
}

// ============================================================================
// Economy Tests (EE-431 - Stub tests for when implemented)
// ============================================================================

#[test]
fn economy_report_returns_valid_json() -> TestResult {
    let output = run_ee(&["economy", "report", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn economy_score_handles_not_found() -> TestResult {
    let output = run_ee(&["economy", "score", "mem_nonexistent", "--json"])?;
    // May return NotFound (10) or Success with empty result
    ensure(
        output.status.code() == Some(0) || output.status.code() == Some(10),
        "exit code must be 0 or 10 (not found)",
    )?;
    if output.status.code() == Some(0) {
        ensure(stdout_is_json(&output), "stdout must be valid JSON")
    } else {
        Ok(())
    }
}

#[test]
fn economy_simulate_compares_budgets_without_mutation() -> TestResult {
    let output = run_ee(&[
        "economy",
        "simulate",
        "--baseline-budget",
        "4000",
        "--budget",
        "2000",
        "--budget",
        "8000",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(
        output.stderr.is_empty(),
        "json command must keep diagnostics off stderr",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        json.get("schema").unwrap_or(&serde_json::Value::Null),
        &serde_json::json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(
        json.pointer("/data/schema")
            .unwrap_or(&serde_json::Value::Null),
        &serde_json::json!("ee.economy.simulation.v1"),
        "simulation schema",
    )?;
    ensure_equal(
        json.pointer("/data/mutationStatus")
            .unwrap_or(&serde_json::Value::Null),
        &serde_json::json!("not_applied"),
        "mutation status",
    )?;
    ensure_equal(
        json.pointer("/data/rankingStateUnchanged")
            .unwrap_or(&serde_json::Value::Null),
        &serde_json::json!(true),
        "ranking state unchanged",
    )?;
    let hash_before = json
        .pointer("/data/rankingStateHashBefore")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "rankingStateHashBefore must be a string".to_string())?;
    let hash_after = json
        .pointer("/data/rankingStateHashAfter")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "rankingStateHashAfter must be a string".to_string())?;
    ensure_equal(&hash_before, &hash_after, "ranking state hash")?;

    let scenarios = json
        .pointer("/data/scenarios")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "scenarios must be an array".to_string())?;
    let budgets = scenarios
        .iter()
        .filter_map(|scenario| {
            scenario
                .pointer("/budgetTokens")
                .and_then(serde_json::Value::as_u64)
        })
        .collect::<Vec<_>>();
    ensure_equal(&budgets, &vec![2000, 4000, 8000], "scenario budgets")
}

#[test]
fn economy_simulate_rejects_zero_budget() -> TestResult {
    let output = run_ee(&["economy", "simulate", "--budget", "0", "--json"])?;
    ensure_equal(&output.status.code(), &Some(1), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        output.stderr.is_empty(),
        "json rejection diagnostics must not use stderr",
    )?;
    let json = stdout_json(&output)?;
    ensure_equal(
        json.get("schema").unwrap_or(&serde_json::Value::Null),
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        json.pointer("/error/code")
            .unwrap_or(&serde_json::Value::Null),
        &serde_json::json!("usage"),
        "error code",
    )
}

#[test]
fn economy_prune_plan_dry_run_returns_recommendations() -> TestResult {
    let output = run_ee(&[
        "economy",
        "prune-plan",
        "--dry-run",
        "--max-recommendations",
        "5",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(
        &json["data"]["schema"],
        &serde_json::json!("ee.economy.prune_plan.v1"),
        "prune plan schema",
    )?;
    ensure_equal(
        &json["data"]["dryRun"],
        &serde_json::json!(true),
        "dry run flag",
    )?;
    ensure_equal(
        &json["data"]["mutationStatus"],
        &serde_json::json!("not_applied"),
        "mutation status",
    )?;
    let actions = json["data"]["summary"]["actions"]
        .as_array()
        .ok_or_else(|| "actions must be an array".to_string())?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    ensure_equal(
        &actions,
        &vec!["revalidate", "retire", "compact", "merge", "demote"],
        "prune actions",
    )
}

#[test]
fn economy_prune_plan_requires_dry_run() -> TestResult {
    let output = run_ee(&["economy", "prune-plan", "--json"])?;
    ensure_equal(&output.status.code(), &Some(8), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        output.stderr.is_empty(),
        "stderr must stay clean for JSON errors",
    )
}

// ============================================================================
// Learning Experiment Tests (EE-444)
// ============================================================================

#[test]
fn learn_experiment_run_dry_run_returns_plan_shape() -> TestResult {
    let output = run_ee(&[
        "learn",
        "experiment",
        "run",
        "--id",
        "exp_database_contract_fixture",
        "--max-attention-tokens",
        "600",
        "--max-runtime-seconds",
        "90",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(output.stderr.is_empty(), "stderr must be empty")?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.learn.experiment_run.v1"),
        "run schema",
    )?;
    ensure_equal(
        &json["dryRun"],
        &serde_json::json!(true),
        "run dry-run flag",
    )?;
    ensure_equal(&json["status"], &serde_json::json!("dry_run"), "run status")?;
    ensure_equal(
        &json["experimentKind"],
        &serde_json::json!("procedure_revalidation"),
        "experiment kind",
    )?;
    ensure_equal(
        &json["budget"]["plannedAttentionTokens"],
        &serde_json::json!(600),
        "planned attention tokens",
    )?;
    ensure_equal(
        &json["budget"]["plannedRuntimeSeconds"],
        &serde_json::json!(90),
        "planned runtime seconds",
    )?;
    let steps = json["steps"]
        .as_array()
        .ok_or_else(|| "steps must be an array".to_string())?;
    ensure(!steps.is_empty(), "steps must not be empty")?;
    ensure(
        steps
            .iter()
            .all(|step| step["writesStorage"] == serde_json::json!(false)),
        "dry-run steps must not write storage",
    )?;
    ensure_equal(
        &json["observations"][0]["signal"],
        &serde_json::json!("positive"),
        "observation signal",
    )?;
    ensure_equal(
        &json["outcomePreview"]["status"],
        &serde_json::json!("confirmed"),
        "outcome preview status",
    )
}

#[test]
fn learn_experiment_run_rejects_non_dry_run() -> TestResult {
    let output = run_ee(&[
        "learn",
        "experiment",
        "run",
        "--id",
        "exp_database_contract_fixture",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(8), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        output.stderr.is_empty(),
        "stderr must stay clean for JSON errors",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!("policy_denied"),
        "error code",
    )
}

#[test]
fn learn_experiment_run_supports_shadow_budget_template() -> TestResult {
    let output = run_ee(&[
        "learn",
        "experiment",
        "run",
        "--id",
        "exp_shadow_budget_probe",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["experimentKind"],
        &serde_json::json!("shadow_budget"),
        "experiment kind",
    )?;
    ensure(
        json["budget"]["shadowBudgetDeltaTokens"]
            .as_i64()
            .is_some_and(|value| value < 0),
        "shadow budget delta must be negative",
    )
}

#[test]
fn learn_observe_dry_run_records_evidence_shape() -> TestResult {
    let output = run_ee(&[
        "learn",
        "observe",
        "exp_fixture_001",
        "--measurement-name",
        "fixture_replay",
        "--measurement-value",
        "1.0",
        "--signal",
        "positive",
        "--evidence-id",
        "ev_b",
        "--evidence-id",
        "ev_a",
        "--evidence-id",
        "ev_a",
        "--note",
        "Replay matched expected output.",
        "--redaction-status",
        "redacted",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(output.stderr.is_empty(), "stderr must be empty")?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.learn.observe.v1"),
        "observe schema",
    )?;
    ensure_equal(
        &json["dryRun"],
        &serde_json::json!(true),
        "observe dry-run flag",
    )?;
    ensure_equal(
        &json["status"],
        &serde_json::json!("dry_run"),
        "observe status",
    )?;
    ensure_equal(
        &json["observation"]["signal"],
        &serde_json::json!("positive"),
        "observation signal",
    )?;
    ensure_equal(
        &json["observation"]["measurementValue"],
        &serde_json::json!(1.0),
        "measurement value",
    )?;
    ensure_equal(
        &json["observation"]["evidenceIds"],
        &serde_json::json!(["ev_a", "ev_b"]),
        "deduplicated evidence ids",
    )
}

#[test]
fn learn_close_dry_run_records_outcome_shape() -> TestResult {
    let output = run_ee(&[
        "learn",
        "close",
        "exp_fixture_001",
        "--status",
        "confirmed",
        "--decision-impact",
        "Promote fixture-backed rule",
        "--confidence-delta",
        "0.25",
        "--priority-delta",
        "3",
        "--promote-artifact",
        "mem_rule_001",
        "--promote-artifact",
        "proc_release_001",
        "--promote-artifact",
        "sit_release_001",
        "--demote-artifact",
        "mem_old_001",
        "--demote-artifact",
        "tw_noisy_001",
        "--safety-note",
        "Dry-run only.",
        "--audit-id",
        "audit_001",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(output.stderr.is_empty(), "stderr must be empty")?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.learn.close.v1"),
        "close schema",
    )?;
    ensure_equal(
        &json["dryRun"],
        &serde_json::json!(true),
        "close dry-run flag",
    )?;
    ensure_equal(
        &json["status"],
        &serde_json::json!("dry_run"),
        "close status",
    )?;
    ensure_equal(
        &json["outcome"]["status"],
        &serde_json::json!("confirmed"),
        "outcome status",
    )?;
    ensure_equal(
        &json["outcome"]["confidenceDelta"],
        &serde_json::json!(0.25),
        "confidence delta",
    )?;
    ensure_equal(
        &json["outcome"]["promotedArtifactIds"],
        &serde_json::json!(["mem_rule_001", "proc_release_001", "sit_release_001"]),
        "promoted artifacts",
    )?;
    ensure_equal(
        &json["outcome"]["demotedArtifactIds"],
        &serde_json::json!(["mem_old_001", "tw_noisy_001"]),
        "demoted artifacts",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["schema"],
        &serde_json::json!("ee.learn.downstream_effects.v1"),
        "downstream effects schema",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["mutationMode"],
        &serde_json::json!("dry_run_projection"),
        "downstream mutation mode",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["economyScore"]["priorityDelta"],
        &serde_json::json!(3),
        "economy priority delta",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["procedureDrift"]["procedureArtifactIds"],
        &serde_json::json!(["proc_release_001"]),
        "procedure artifacts",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["procedureDrift"]["driftSignal"],
        &serde_json::json!("validated_by_experiment"),
        "procedure drift signal",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["tripwireFalseAlarm"]["falseAlarmCostDelta"],
        &serde_json::json!(0),
        "tripwire false alarm delta",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["situationConfidence"]["situationArtifactIds"],
        &serde_json::json!(["sit_release_001"]),
        "situation artifacts",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["situationConfidence"]["confidenceDelta"],
        &serde_json::json!(0.25),
        "situation confidence delta",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["audit"]["durableFeedbackRecorded"],
        &serde_json::json!(false),
        "dry-run durable feedback",
    )?;
    ensure_equal(
        &json["downstreamEffects"]["audit"]["silentMutation"],
        &serde_json::json!(false),
        "silent mutation guard",
    )
}

#[test]
fn learn_close_rejects_invalid_confidence_delta() -> TestResult {
    let output = run_ee(&[
        "learn",
        "close",
        "exp_fixture_001",
        "--status",
        "confirmed",
        "--decision-impact",
        "Promote fixture-backed rule",
        "--confidence-delta",
        "2.0",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(1), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        output.stderr.is_empty(),
        "stderr must stay clean for JSON errors",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!("usage"),
        "error code",
    )
}

// ============================================================================
// Rule Tests (EE-086)
// ============================================================================

#[test]
fn rule_add_dry_run_returns_response_envelope() -> TestResult {
    let output = run_ee(&[
        "rule",
        "add",
        "Run cargo fmt --check before release.",
        "--tag",
        "Rust,CI",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(output.stderr.is_empty(), "stderr must be empty")?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(
        &json["data"]["schema"],
        &serde_json::json!("ee.rule.add.v1"),
        "rule add schema",
    )?;
    ensure_equal(
        &json["data"]["dryRun"],
        &serde_json::json!(true),
        "dry-run flag",
    )?;
    ensure_equal(
        &json["data"]["persisted"],
        &serde_json::json!(false),
        "persisted flag",
    )?;
    ensure_equal(
        &json["data"]["tags"],
        &serde_json::json!(["ci", "rust"]),
        "canonical tags",
    )?;
    ensure_equal(
        &json["data"]["evidence"]["status"],
        &serde_json::json!("missing"),
        "evidence status",
    )?;
    ensure_equal(
        &json["data"]["indexStatus"],
        &serde_json::json!("dry_run_not_queued"),
        "dry-run index status",
    )
}

#[test]
fn rule_add_rejects_validated_rule_without_evidence() -> TestResult {
    let output = run_ee(&[
        "rule",
        "add",
        "Validated rules need evidence.",
        "--maturity",
        "validated",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(1), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        output.stderr.is_empty(),
        "stderr must stay clean for JSON errors",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!("usage"),
        "error code",
    )
}

#[test]
fn rule_show_rejects_invalid_rule_id_as_json_error() -> TestResult {
    let output = run_ee(&["rule", "show", "not-a-rule-id", "--json"])?;
    ensure_equal(&output.status.code(), &Some(1), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        output.stderr.is_empty(),
        "stderr must stay clean for JSON errors",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!("usage"),
        "error code",
    )
}

#[test]
fn rule_add_persists_rule_with_source_memory() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;
    ensure(init.stderr.is_empty(), "init stderr must be empty")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Release evidence: cargo fmt caught a formatting regression.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(0), "remember exit")?;
    ensure(remember.stderr.is_empty(), "remember stderr must be empty")?;
    let remember_json = stdout_json(&remember)?;
    let source_memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember memory_id must be a string".to_string())?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "rule",
        "add",
        "Run cargo fmt --check before release.",
        "--maturity",
        "validated",
        "--confidence",
        "0.86",
        "--source-memory",
        source_memory_id,
        "--tag",
        "release",
        "--json",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure_equal(&output.status.code(), &Some(0), "rule add exit")?;
    ensure(
        output.stderr.is_empty(),
        format!("rule add stderr must be empty: {stderr}"),
    )?;
    let json = stdout_json(&output)?;
    ensure_equal(
        &json["data"]["persisted"],
        &serde_json::json!(true),
        "persisted flag",
    )?;
    ensure_equal(
        &json["data"]["maturity"],
        &serde_json::json!("validated"),
        "maturity",
    )?;
    ensure_equal(
        &json["data"]["sourceMemoryIds"],
        &serde_json::json!([source_memory_id]),
        "source memory IDs",
    )?;
    ensure_equal(
        &json["data"]["evidence"]["status"],
        &serde_json::json!("verified"),
        "evidence status",
    )?;
    ensure(
        json["data"]["auditId"]
            .as_str()
            .is_some_and(|id| id.starts_with("audit_")),
        "audit ID must be present",
    )?;
    ensure(
        json["data"]["indexJobId"]
            .as_str()
            .is_some_and(|id| id.starts_with("sidx_")),
        "index job ID must be present",
    )?;
    let rule_id = json["data"]["ruleId"]
        .as_str()
        .ok_or_else(|| "ruleId must be a string".to_string())?;

    let list = run_ee(&[
        "--workspace",
        &workspace,
        "rule",
        "list",
        "--maturity",
        "validated",
        "--tag",
        "release",
        "--json",
    ])?;
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    ensure_equal(&list.status.code(), &Some(0), "rule list exit")?;
    ensure(
        list.stderr.is_empty(),
        format!("rule list stderr must be empty: {list_stderr}"),
    )?;
    ensure(stdout_is_json(&list), "rule list stdout must be valid JSON")?;
    ensure(stdout_is_clean(&list), "rule list stdout must be clean")?;
    let list_json = stdout_json(&list)?;
    ensure_equal(
        &list_json["data"]["schema"],
        &serde_json::json!("ee.rule.list.v1"),
        "rule list schema",
    )?;
    ensure_equal(
        &list_json["data"]["totalCount"],
        &serde_json::json!(1),
        "rule list total count",
    )?;
    ensure_equal(
        &list_json["data"]["rules"][0]["id"],
        &serde_json::json!(rule_id),
        "rule list id",
    )?;
    ensure_equal(
        &list_json["data"]["rules"][0]["evidence"]["sourceMemoryCount"],
        &serde_json::json!(1),
        "rule list evidence count",
    )?;

    let show = run_ee(&["--workspace", &workspace, "rule", "show", rule_id, "--json"])?;
    let show_stderr = String::from_utf8_lossy(&show.stderr);
    ensure_equal(&show.status.code(), &Some(0), "rule show exit")?;
    ensure(
        show.stderr.is_empty(),
        format!("rule show stderr must be empty: {show_stderr}"),
    )?;
    ensure(stdout_is_json(&show), "rule show stdout must be valid JSON")?;
    ensure(stdout_is_clean(&show), "rule show stdout must be clean")?;
    let show_json = stdout_json(&show)?;
    ensure_equal(
        &show_json["data"]["schema"],
        &serde_json::json!("ee.rule.show.v1"),
        "rule show schema",
    )?;
    ensure_equal(
        &show_json["data"]["rule"]["id"],
        &serde_json::json!(rule_id),
        "rule show id",
    )?;
    ensure_equal(
        &show_json["data"]["rule"]["sourceMemoryIds"],
        &serde_json::json!([source_memory_id]),
        "rule show source memory IDs",
    )
}

#[test]
fn release_brief_search_context_why_and_doctor_fix_plan_are_machine_clean() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    let init_stderr = String::from_utf8_lossy(&init.stderr);
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;
    ensure(
        init.stderr.is_empty(),
        format!("init stderr must be empty: {init_stderr}"),
    )?;
    ensure(stdout_is_json(&init), "init stdout must be valid JSON")?;
    ensure(stdout_is_clean(&init), "init stdout must be clean")?;

    let rule = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Before release, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`; verify GitHub release assets before pushing to main.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--tags",
        "release,cargo,verification",
        "--source",
        "file://tests/fixtures/eval/release_failure/source_memory.json#L1",
        "--json",
    ])?;
    let rule_stderr = String::from_utf8_lossy(&rule.stderr);
    ensure_equal(&rule.status.code(), &Some(0), "rule remember exit")?;
    ensure(
        rule.stderr.is_empty(),
        format!("rule remember stderr must be empty: {rule_stderr}"),
    )?;
    ensure(
        stdout_is_json(&rule),
        "rule remember stdout must be valid JSON",
    )?;
    ensure(stdout_is_clean(&rule), "rule remember stdout must be clean")?;
    let rule_json = stdout_json(&rule)?;
    let rule_id = rule_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "rule memory_id must be a string".to_string())?;

    let failure = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "A previous release attempt failed because clippy was skipped after a formatting-only change; the release workflow rejected `cargo clippy --all-targets -- -D warnings` with an unused import before artifacts could be published.",
        "--level",
        "episodic",
        "--kind",
        "failure",
        "--tags",
        "release,clippy,workflow",
        "--source",
        "file://tests/fixtures/eval/release_failure/source_memory.json#L10",
        "--json",
    ])?;
    let failure_stderr = String::from_utf8_lossy(&failure.stderr);
    ensure_equal(&failure.status.code(), &Some(0), "failure remember exit")?;
    ensure(
        failure.stderr.is_empty(),
        format!("failure remember stderr must be empty: {failure_stderr}"),
    )?;
    ensure(
        stdout_is_json(&failure),
        "failure remember stdout must be valid JSON",
    )?;
    ensure(
        stdout_is_clean(&failure),
        "failure remember stdout must be clean",
    )?;
    let failure_json = stdout_json(&failure)?;
    let failure_id = failure_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "failure memory_id must be a string".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    let rebuild_stderr = String::from_utf8_lossy(&rebuild.stderr);
    ensure_equal(&rebuild.status.code(), &Some(0), "index rebuild exit")?;
    ensure(
        rebuild.stderr.is_empty(),
        format!("index rebuild stderr must be empty: {rebuild_stderr}"),
    )?;
    ensure(
        stdout_is_json(&rebuild),
        "index rebuild stdout must be valid JSON",
    )?;
    ensure(
        stdout_is_clean(&rebuild),
        "index rebuild stdout must be clean",
    )?;
    let rebuild_json = stdout_json(&rebuild)?;
    ensure_equal(
        &rebuild_json["data"]["memories_indexed"],
        &serde_json::json!(2),
        "indexed memory count",
    )?;

    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "clippy release",
        "--json",
    ])?;
    let search_stderr = String::from_utf8_lossy(&search.stderr);
    ensure_equal(&search.status.code(), &Some(0), "search exit")?;
    ensure(
        search.stderr.is_empty(),
        format!("search stderr must be empty: {search_stderr}"),
    )?;
    ensure(stdout_is_json(&search), "search stdout must be valid JSON")?;
    ensure(stdout_is_clean(&search), "search stdout must be clean")?;
    let search_json = stdout_json(&search)?;
    let search_results = search_json["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results must be an array".to_string())?;
    ensure(
        search_results
            .iter()
            .any(|hit| hit["doc_id"].as_str() == Some(rule_id)),
        "search results must include the release rule memory",
    )?;
    ensure(
        search_results
            .iter()
            .any(|hit| hit["doc_id"].as_str() == Some(failure_id)),
        "search results must include the release failure memory",
    )?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "prepare release",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    let context_stderr = String::from_utf8_lossy(&context.stderr);
    ensure_equal(&context.status.code(), &Some(0), "context exit")?;
    ensure(
        context.stderr.is_empty(),
        format!("context stderr must be empty: {context_stderr}"),
    )?;
    ensure(
        stdout_is_json(&context),
        "context stdout must be valid JSON",
    )?;
    ensure(stdout_is_clean(&context), "context stdout must be clean")?;
    let context_json = stdout_json(&context)?;
    ensure_equal(
        &context_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "context response schema",
    )?;
    let pack_items = context_json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "context pack items must be an array".to_string())?;
    ensure(
        pack_items
            .iter()
            .any(|item| item["memoryId"].as_str() == Some(rule_id)),
        "context pack must include the release rule memory",
    )?;
    ensure(
        pack_items
            .iter()
            .any(|item| item["memoryId"].as_str() == Some(failure_id)),
        "context pack must include the release failure memory",
    )?;
    ensure(
        context_json["data"]["pack"]["hash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("blake3:")),
        "context pack must persist a blake3 hash",
    )?;

    let why = run_ee(&["--workspace", &workspace, "why", rule_id, "--json"])?;
    let why_stderr = String::from_utf8_lossy(&why.stderr);
    ensure_equal(&why.status.code(), &Some(0), "why exit")?;
    ensure(
        why.stderr.is_empty(),
        format!("why stderr must be empty: {why_stderr}"),
    )?;
    ensure(stdout_is_json(&why), "why stdout must be valid JSON")?;
    ensure(stdout_is_clean(&why), "why stdout must be clean")?;
    let why_json = stdout_json(&why)?;
    ensure_equal(
        &why_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "why response schema",
    )?;
    ensure_equal(
        &why_json["data"]["selection"]["latestPackSelection"]["query"],
        &serde_json::json!("prepare release"),
        "why latest pack query",
    )?;
    ensure(
        why_json["data"]["selection"]["latestPackSelection"]["packId"]
            .as_str()
            .is_some_and(|pack_id| pack_id.starts_with("pack_")),
        "why latest pack selection must include a persisted pack id",
    )?;

    let doctor = run_ee(&["--workspace", &workspace, "doctor", "--fix-plan", "--json"])?;
    let doctor_stderr = String::from_utf8_lossy(&doctor.stderr);
    ensure_equal(&doctor.status.code(), &Some(0), "doctor exit")?;
    ensure(
        doctor.stderr.is_empty(),
        format!("doctor stderr must be empty: {doctor_stderr}"),
    )?;
    ensure(stdout_is_json(&doctor), "doctor stdout must be valid JSON")?;
    ensure(stdout_is_clean(&doctor), "doctor stdout must be clean")?;
    let doctor_json = stdout_json(&doctor)?;
    ensure_equal(
        &doctor_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "doctor response schema",
    )?;
    ensure_equal(
        &doctor_json["data"]["mode"],
        &serde_json::json!("fix-plan"),
        "doctor mode",
    )?;
    ensure(
        doctor_json["data"]["steps"].is_array(),
        "doctor fix-plan must include steps",
    )?;
    ensure(
        doctor_json["data"]["suggestedCommands"].is_array(),
        "doctor fix-plan must include suggested commands",
    )
}

// ============================================================================
// Preflight Tests (EE-391)
// ============================================================================

#[test]
fn preflight_run_blocks_high_risk_deploy_task() -> TestResult {
    let output = run_ee(&[
        "preflight",
        "run",
        "deploy production database migration",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(
        output.stderr.is_empty(),
        "json command must keep diagnostics off stderr",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "preflight run response schema",
    )?;
    ensure_equal(
        &json["data"]["status"],
        &serde_json::json!("completed"),
        "preflight run status",
    )?;
    ensure_equal(
        &json["data"]["risk_level"],
        &serde_json::json!("high"),
        "preflight run risk level",
    )?;
    ensure_equal(
        &json["data"]["cleared"],
        &serde_json::json!(false),
        "preflight run clearance",
    )?;
    ensure(
        json["data"]["block_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("exceeds auto-clear threshold")),
        "preflight run must include block reason",
    )
}

#[test]
fn preflight_show_returns_stubbed_storage_details() -> TestResult {
    let output = run_ee(&["preflight", "show", "pf_gate16_contract", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(
        output.stderr.is_empty(),
        "json command must keep diagnostics off stderr",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "preflight show response schema",
    )?;
    ensure_equal(
        &json["data"]["run"]["id"],
        &serde_json::json!("pf_gate16_contract"),
        "preflight show run id",
    )?;
    ensure_equal(
        &json["data"]["run"]["block_reason"],
        &serde_json::json!("Storage not yet wired"),
        "preflight show degraded storage reason",
    )
}

#[test]
fn preflight_close_dry_run_records_feedback_shape() -> TestResult {
    let output = run_ee(&[
        "preflight",
        "close",
        "pf_gate16_contract",
        "--cleared",
        "--reason",
        "advanced e2e preflight closure",
        "--task-outcome",
        "success",
        "--feedback",
        "helped",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(
        output.stderr.is_empty(),
        "json command must keep diagnostics off stderr",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "preflight close response schema",
    )?;
    ensure_equal(
        &json["data"]["dry_run"],
        &serde_json::json!(true),
        "preflight close dry-run",
    )?;
    ensure_equal(
        &json["data"]["new_status"],
        &serde_json::json!("completed"),
        "preflight close status",
    )?;
    ensure_equal(
        &json["data"]["feedback"]["signal"],
        &serde_json::json!("helpful"),
        "preflight close feedback signal",
    )
}

#[test]
fn preflight_close_without_cleared_infers_false_alarm_for_success() -> TestResult {
    let output = run_ee(&[
        "preflight",
        "close",
        "pf_gate16_contract",
        "--reason",
        "advanced e2e inferred feedback check",
        "--task-outcome",
        "success",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(
        output.stderr.is_empty(),
        "json command must keep diagnostics off stderr",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "preflight close response schema",
    )?;
    ensure_equal(
        &json["data"]["new_status"],
        &serde_json::json!("cancelled"),
        "uncleared preflight close status",
    )?;
    ensure_equal(
        &json["data"]["feedback"]["feedback_kind"],
        &serde_json::json!("false_alarm"),
        "inferred feedback kind",
    )?;
    ensure_equal(
        &json["data"]["feedback"]["signal"],
        &serde_json::json!("inaccurate"),
        "inferred feedback signal",
    )
}

#[test]
fn preflight_show_rejects_invalid_run_id_with_usage_error() -> TestResult {
    let output = run_ee(&["preflight", "show", "invalid", "--json"])?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")?;
    ensure(
        output.stderr.is_empty(),
        "json errors must keep diagnostics off stderr",
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!("usage"),
        "error code",
    )?;
    ensure(
        output.status.code() == Some(0) || output.status.code() == Some(1),
        "json usage errors must return a process exit code",
    )?;
    ensure(
        json["error"]["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("pf_<uuid>")),
        "usage error should include repair guidance",
    )
}

// ============================================================================
// Output Contract Tests
// ============================================================================

#[test]
fn all_recorder_commands_produce_stdout_only_data() -> TestResult {
    let commands = [
        vec![
            "recorder",
            "start",
            "--agent-id",
            "test",
            "--dry-run",
            "--json",
        ],
        vec![
            "recorder",
            "event",
            "run_1",
            "--event-type",
            "tool_call",
            "--json",
        ],
        vec!["recorder", "finish", "run_1", "--dry-run", "--json"],
        vec!["recorder", "tail", "run_1", "--json"],
        vec![
            "recorder",
            "import",
            "--source-id",
            "cass://empty",
            "--dry-run",
            "--json",
        ],
    ];

    for args in &commands {
        let output = run_ee(args)?;
        if output.status.code() == Some(0) {
            ensure(
                stdout_is_clean(&output),
                format!("ee {} must have clean stdout", args.join(" ")),
            )?;
        }
    }
    Ok(())
}

#[test]
fn recorder_commands_support_human_output() -> TestResult {
    let output = run_ee(&["recorder", "start", "--agent-id", "test", "--dry-run"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.contains("Recording Session"),
        "human output must contain header",
    )
}

// ============================================================================
// Causal Trace Tests (EE-451)
// ============================================================================

#[test]
fn causal_trace_dry_run_returns_valid_json() -> TestResult {
    let output = run_ee(&[
        "causal",
        "trace",
        "--run-id",
        "run-test-001",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "ee.causal.trace"),
        "must have causal trace schema",
    )?;
    ensure(stdout_contains(&output, "dryRun"), "must contain dryRun")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn causal_trace_shows_filters_applied() -> TestResult {
    let output = run_ee(&[
        "causal",
        "trace",
        "--memory-id",
        "mem-001",
        "--agent-id",
        "agent-test",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "filtersApplied"),
        "must contain filtersApplied",
    )?;
    ensure(
        stdout_contains(&output, "memory_id"),
        "must show memory_id filter",
    )
}

#[test]
fn causal_trace_returns_chains_and_summary() -> TestResult {
    let output = run_ee(&["causal", "trace", "--procedure-id", "proc-001", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let json = stdout_json(&output)?;
    let data = json.get("data").unwrap_or(&json);
    ensure(data.get("chains").is_some(), "must contain chains")?;
    ensure(data.get("summary").is_some(), "must contain summary")
}

#[test]
fn causal_trace_no_filters_returns_degradation() -> TestResult {
    let output = run_ee(&["causal", "trace", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let json = stdout_json(&output)?;
    let data = json.get("data").unwrap_or(&json);
    let degradations = data.get("degradations").and_then(|v| v.as_array());
    ensure(
        degradations.is_some_and(|d| !d.is_empty()),
        "should have degradation when no filters",
    )
}

// ============================================================================
// Causal Estimate Tests (EE-452)
// ============================================================================

#[test]
fn causal_estimate_dry_run_returns_valid_json() -> TestResult {
    let output = run_ee(&[
        "causal",
        "estimate",
        "--artifact-id",
        "art-001",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "ee.causal.estimate"),
        "must have causal estimate schema",
    )?;
    ensure(stdout_contains(&output, "dryRun"), "must contain dryRun")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn causal_estimate_naive_method_returns_insufficient_confidence() -> TestResult {
    let output = run_ee(&[
        "causal",
        "estimate",
        "--artifact-id",
        "art-001",
        "--method",
        "naive",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "\"insufficient\""),
        "naive method should have insufficient confidence",
    )
}

#[test]
fn causal_estimate_replay_method_returns_medium_confidence() -> TestResult {
    let output = run_ee(&[
        "causal",
        "estimate",
        "--artifact-id",
        "art-001",
        "--method",
        "replay",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "\"medium\""),
        "replay method should have medium confidence",
    )
}

#[test]
fn causal_estimate_experiment_method_returns_high_confidence() -> TestResult {
    let output = run_ee(&[
        "causal",
        "estimate",
        "--artifact-id",
        "art-001",
        "--method",
        "experiment",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "\"high\""),
        "experiment method should have high confidence",
    )
}

#[test]
fn causal_estimate_includes_assumptions_when_requested() -> TestResult {
    let output = run_ee(&[
        "causal",
        "estimate",
        "--artifact-id",
        "art-001",
        "--include-assumptions",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let json = stdout_json(&output)?;
    let data = json.get("data").unwrap_or(&json);
    let assumptions = data.get("assumptions").and_then(|v| v.as_array());
    ensure(
        assumptions.is_some_and(|a| !a.is_empty()),
        "should include assumptions when requested",
    )
}

#[test]
fn causal_estimate_includes_confounders_when_requested() -> TestResult {
    let output = run_ee(&[
        "causal",
        "estimate",
        "--artifact-id",
        "art-001",
        "--include-confounders",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let json = stdout_json(&output)?;
    let data = json.get("data").unwrap_or(&json);
    let confounders = data.get("confounders").and_then(|v| v.as_array());
    ensure(
        confounders.is_some_and(|c| !c.is_empty()),
        "should include confounders when requested",
    )
}

#[test]
fn causal_estimate_summary_contains_method_used() -> TestResult {
    let output = run_ee(&[
        "causal",
        "estimate",
        "--artifact-id",
        "art-001",
        "--method",
        "matching",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let json = stdout_json(&output)?;
    let data = json.get("data").unwrap_or(&json);
    let method = data
        .get("summary")
        .and_then(|s| s.get("methodUsed"))
        .and_then(|m| m.as_str());
    ensure(
        method == Some("matching"),
        "summary must contain methodUsed",
    )
}

// ============================================================================
// Causal Compare Tests (EE-453)
// ============================================================================

#[test]
fn causal_compare_dry_run_returns_valid_json() -> TestResult {
    let output = run_ee(&[
        "causal",
        "compare",
        "--fixture-replay-id",
        "fixture-001",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "ee.causal.compare.v1"),
        "must have causal compare schema",
    )?;
    ensure(stdout_contains(&output, "dryRun"), "must contain dryRun")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn causal_compare_with_multiple_sources_returns_comparisons() -> TestResult {
    let output = run_ee(&[
        "causal",
        "compare",
        "--fixture-replay-id",
        "fixture-001",
        "--shadow-run-id",
        "shadow-001",
        "--counterfactual-episode-id",
        "counterfactual-001",
        "--experiment-id",
        "exp-001",
        "--method",
        "experiment",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let json = stdout_json(&output)?;
    let data = json.get("data").unwrap_or(&json);
    let comparisons = data.get("comparisons").and_then(|value| value.as_array());
    ensure(
        comparisons.is_some_and(|items| items.len() == 4),
        "compare should return four source comparisons",
    )?;
    ensure(
        stdout_contains(&output, "\"methodUsed\":\"experiment\""),
        "summary should include methodUsed",
    )
}

#[test]
fn causal_compare_without_source_ids_returns_degradation() -> TestResult {
    let output = run_ee(&["causal", "compare", "--artifact-id", "art-001", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(
        stdout_contains(&output, "\"code\":\"no_sources\""),
        "missing source IDs should produce no_sources degradation",
    )
}

// ============================================================================
// Causal Promote Plan Tests (EE-454)
// ============================================================================

#[test]
fn causal_promote_plan_dry_run_returns_valid_json() -> TestResult {
    let output = run_ee(&[
        "causal",
        "promote-plan",
        "--artifact-id",
        "art-001",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "ee.causal.promote_plan.v1"),
        "must have causal promote-plan schema",
    )?;
    ensure(stdout_contains(&output, "plans"), "must include plans")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn causal_promote_plan_supports_explicit_action_override() -> TestResult {
    let output = run_ee(&[
        "causal",
        "promote-plan",
        "--artifact-id",
        "art-001",
        "--action",
        "demote",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "\"action\":\"demote\""),
        "explicit action must be reflected in plan output",
    )
}

#[test]
fn causal_promote_plan_projects_cross_surface_effects() -> TestResult {
    let output = run_ee(&[
        "causal",
        "promote-plan",
        "--artifact-id",
        "art-001",
        "--method",
        "experiment",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let json = stdout_json(&output)?;
    let downstream = json
        .get("downstreamEffects")
        .or_else(|| {
            json.get("data")
                .and_then(|data| data.get("downstreamEffects"))
        })
        .ok_or_else(|| "missing downstreamEffects payload".to_string())?;
    ensure_equal(
        &downstream["schema"],
        &serde_json::json!("ee.causal.downstream_effects.v1"),
        "downstream effects schema",
    )?;
    ensure(
        downstream["economyScore"].is_object(),
        "economy score projection",
    )?;
    ensure(
        downstream["learningAgenda"].is_object(),
        "learning agenda projection",
    )?;
    ensure(
        downstream["preflightRouting"].is_object(),
        "preflight routing projection",
    )?;
    ensure(
        downstream["procedureVerification"].is_object(),
        "procedure verification projection",
    )?;
    ensure_equal(
        &downstream["audit"]["mutationMode"],
        &serde_json::json!("dry_run_projection"),
        "downstream mutation mode",
    )?;
    ensure_equal(
        &downstream["audit"]["rawEvidenceReplaced"],
        &serde_json::json!(false),
        "raw evidence remains immutable",
    )?;
    ensure_equal(
        &downstream["audit"]["silentMutation"],
        &serde_json::json!(false),
        "silent mutation disabled",
    )?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn causal_promote_plan_underpowered_evidence_routes_to_review() -> TestResult {
    let output = run_ee(&[
        "causal",
        "promote-plan",
        "--artifact-id",
        "mem-underpowered-001",
        "--method",
        "matching",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(output.stderr.is_empty(), "json mode stderr must be empty")?;
    let json = stdout_json(&output)?;
    let data = json.get("data").unwrap_or(&json);
    ensure_equal(
        &data["plans"][0]["action"],
        &serde_json::json!("hold"),
        "underpowered evidence must not promote",
    )?;
    ensure_equal(
        &data["plans"][0]["evidenceStrength"],
        &serde_json::json!("correlational"),
        "matching method evidence strength",
    )?;
    ensure(
        data["recommendations"]["reviewRecommendations"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "underpowered evidence must route to review",
    )?;
    ensure(
        data["recommendations"]["experimentProposals"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "underpowered evidence must propose a learning experiment",
    )?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn causal_promote_plan_preserves_safety_guards_and_stderr_isolation() -> TestResult {
    let output = run_ee(&[
        "causal",
        "promote-plan",
        "--artifact-id",
        "mem-safety-critical-001",
        "--method",
        "experiment",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(output.stderr.is_empty(), "json mode stderr must be empty")?;
    let json = stdout_json(&output)?;
    let data = json.get("data").unwrap_or(&json);
    ensure(
        data["recommendations"]["safetyGuards"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.as_str()
                        .is_some_and(|text| text.contains("never randomized away"))
                })
            }),
        "safety-critical guard must remain in causal promote-plan output",
    )?;
    ensure(
        data["plans"]
            .as_array()
            .is_some_and(|plans| plans.iter().all(|plan| plan["dryRunFirst"] == true)),
        "all causal plans must remain dry-run-first",
    )?;
    ensure_equal(
        &data["downstreamEffects"]["audit"]["silentMutation"],
        &serde_json::json!(false),
        "silent mutation guard",
    )?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn all_causal_commands_produce_stdout_only_data() -> TestResult {
    let commands = [
        vec!["causal", "trace", "--run-id", "test", "--dry-run", "--json"],
        vec![
            "causal",
            "estimate",
            "--artifact-id",
            "art-001",
            "--dry-run",
            "--json",
        ],
        vec![
            "causal",
            "compare",
            "--fixture-replay-id",
            "fixture-001",
            "--dry-run",
            "--json",
        ],
        vec![
            "causal",
            "promote-plan",
            "--artifact-id",
            "art-001",
            "--dry-run",
            "--json",
        ],
    ];

    for args in &commands {
        let output = run_ee(args)?;
        if output.status.code() == Some(0) {
            ensure(
                stdout_is_clean(&output),
                format!("ee {} must have clean stdout", args.join(" ")),
            )?;
        }
    }
    Ok(())
}

#[test]
fn causal_commands_support_human_output() -> TestResult {
    let output = run_ee(&["causal", "trace", "--run-id", "test", "--dry-run"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.contains("Causal Trace"),
        "human output must contain header",
    )
}

// ============================================================================
// Post-Task Outcome Scenario Tests (EE-USR-004)
// ============================================================================

#[test]
fn post_task_outcome_scenario_commands_emit_machine_data() -> TestResult {
    let commands = [
        vec![
            "outcome",
            "mem_test_001",
            "--signal",
            "helpful",
            "--dry-run",
            "--json",
        ],
        vec!["review", "session", "--propose", "--limit", "5", "--json"],
        vec!["curate", "candidates", "--all", "--limit", "5", "--json"],
        vec![
            "procedure",
            "propose",
            "--title",
            "Release checklist candidate",
            "--source-run",
            "run_test_001",
            "--dry-run",
            "--json",
        ],
        vec![
            "procedure",
            "verify",
            "proc_test_001",
            "--dry-run",
            "--allow-failure",
            "--json",
        ],
        vec!["learn", "agenda", "--json"],
    ];

    let mut successful_commands = 0_u32;
    for args in &commands {
        let output = run_ee(args)?;
        ensure(
            stdout_is_json(&output),
            format!("ee {} must emit JSON to stdout", args.join(" ")),
        )?;
        let json = stdout_json(&output)?;
        let schema = json.get("schema").and_then(|value| value.as_str());
        ensure(
            schema.is_some_and(|value| !value.trim().is_empty()),
            format!("ee {} must include a schema in JSON mode", args.join(" ")),
        )?;

        if output.status.code() == Some(0) {
            ensure(
                schema != Some("ee.error.v1"),
                format!(
                    "ee {} should not emit ee.error.v1 on success",
                    args.join(" ")
                ),
            )?;
            ensure(
                String::from_utf8_lossy(&output.stderr).trim().is_empty(),
                format!(
                    "ee {} must keep diagnostics off stderr in JSON mode",
                    args.join(" ")
                ),
            )?;
            successful_commands += 1;
        }
    }

    ensure(
        successful_commands >= 3,
        "at least three post-task scenario commands should succeed",
    )
}

// ============================================================================
// Rehearse Tests (EE-REHEARSE-001)
// ============================================================================

#[test]
fn rehearse_plan_returns_valid_json() -> TestResult {
    let output = run_ee(&["rehearse", "plan", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "ee.rehearse.plan.v1"),
        "must have rehearse plan schema",
    )?;
    ensure(
        stdout_contains(&output, "can_proceed"),
        "must contain can_proceed",
    )?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn rehearse_plan_reports_non_rehearsable_command() -> TestResult {
    let command_spec = r#"[{
      "id":"cmd_non_rehearsable",
      "command":"serve",
      "args":["--json"],
      "expected_effect":"external_io",
      "stop_on_failure":true,
      "idempotency_key":null
    }]"#;
    let output = run_ee(&[
        "rehearse",
        "plan",
        "--commands-json",
        command_spec,
        "--profile",
        "full",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let value = stdout_json(&output)?;
    ensure(
        value["non_rehearsable"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()),
        "must include non_rehearsable entries",
    )?;
    ensure_equal(
        &value["non_rehearsable"][0]["reason_code"],
        &serde_json::json!("external_io"),
        "reason code",
    )?;
    ensure_equal(
        &value["can_proceed"],
        &serde_json::json!(false),
        "can proceed",
    )
}

#[test]
fn rehearse_run_degrades_until_real_sandbox_exists() -> TestResult {
    let command_spec = r#"[{
      "id":"cmd_status",
      "command":"status",
      "args":["--json"],
      "expected_effect":"read_only",
      "stop_on_failure":false,
      "idempotency_key":"idem-status-001"
    }]"#;
    let output = run_ee(&[
        "rehearse",
        "run",
        "--commands-json",
        command_spec,
        "--profile",
        "quick",
        "--json",
    ])?;
    ensure_equal(
        &output.status.code(),
        &Some(UNSATISFIED_DEGRADED_MODE_EXIT),
        "exit code",
    )?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    let value = stdout_json(&output)?;
    ensure_equal(
        &value["schema"],
        &serde_json::json!("ee.response.v1"),
        "schema",
    )?;
    ensure_equal(&value["success"], &serde_json::json!(false), "success")?;
    ensure_equal(
        &value["data"]["code"],
        &serde_json::json!("rehearsal_unavailable"),
        "degraded code",
    )?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn rehearse_inspect_and_promote_plan_degrade_until_artifacts_exist() -> TestResult {
    let inspect_output = run_ee(&["rehearse", "inspect", "rrun_fixture_001", "--json"])?;
    ensure_equal(
        &inspect_output.status.code(),
        &Some(UNSATISFIED_DEGRADED_MODE_EXIT),
        "inspect exit code",
    )?;
    ensure(
        stdout_is_json(&inspect_output),
        "inspect stdout must be valid JSON",
    )?;
    let inspect_json = stdout_json(&inspect_output)?;
    ensure_equal(
        &inspect_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "inspect schema",
    )?;
    ensure_equal(
        &inspect_json["data"]["code"],
        &serde_json::json!("rehearsal_unavailable"),
        "inspect degraded code",
    )?;

    let promote_output = run_ee(&["rehearse", "promote-plan", "rrun_fixture_001", "--json"])?;
    ensure_equal(
        &promote_output.status.code(),
        &Some(UNSATISFIED_DEGRADED_MODE_EXIT),
        "promote-plan exit code",
    )?;
    ensure(
        stdout_is_json(&promote_output),
        "promote stdout must be valid JSON",
    )?;
    let promote_json = stdout_json(&promote_output)?;
    ensure_equal(
        &promote_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "promote schema",
    )?;
    ensure_equal(
        &promote_json["data"]["code"],
        &serde_json::json!("rehearsal_unavailable"),
        "promote degraded code",
    )?;
    ensure(
        stdout_is_clean(&promote_output),
        "promote stdout must be clean",
    )
}

#[test]
fn all_rehearse_commands_produce_stdout_only_data() -> TestResult {
    let commands = [
        vec!["rehearse", "plan", "--json"],
        vec!["rehearse", "run", "--profile", "quick", "--json"],
        vec!["rehearse", "inspect", "rrun_fixture_001", "--json"],
        vec!["rehearse", "promote-plan", "rrun_fixture_001", "--json"],
    ];

    for args in &commands {
        let output = run_ee(args)?;
        if matches!(
            output.status.code(),
            Some(0) | Some(UNSATISFIED_DEGRADED_MODE_EXIT)
        ) {
            ensure(
                stdout_is_clean(&output),
                format!("ee {} must have clean stdout", args.join(" ")),
            )?;
        }
    }
    Ok(())
}

// ============================================================================
// Agent-Native UX Epic (cat-agent-native-ux)
// ============================================================================

#[test]
fn agent_native_ux_surfaces_emit_machine_clean_json() -> TestResult {
    let commands = [
        vec!["capabilities", "--json"],
        vec!["schema", "list", "--json"],
        vec!["introspect", "--json"],
        vec!["agent-docs", "commands", "--json"],
        vec!["diag", "streams", "--json"],
        vec!["analyze", "science-status", "--json"],
    ];

    for args in &commands {
        let output = run_ee(args)?;
        ensure_equal(
            &output.status.code(),
            &Some(0),
            &format!("ee {} exit code", args.join(" ")),
        )?;
        ensure(
            stdout_is_json(&output),
            format!("ee {} must emit JSON to stdout", args.join(" ")),
        )?;
        ensure(
            stdout_is_clean(&output),
            format!("ee {} must keep stdout machine clean", args.join(" ")),
        )?;
        if args.first() == Some(&"diag") {
            ensure(
                stdout_contains(&output, "\"stderrReceivedProbe\":true"),
                "diag streams must report stderr probe capture",
            )?;
            ensure(
                String::from_utf8_lossy(&output.stderr)
                    .contains("stderr probe for stream isolation verification"),
                "diag streams must emit stderr probe",
            )?;
        } else {
            ensure(
                String::from_utf8_lossy(&output.stderr).trim().is_empty(),
                format!(
                    "ee {} must keep diagnostics off stderr in JSON mode",
                    args.join(" ")
                ),
            )?;
        }
    }

    Ok(())
}
