//! Gate 20 memory economy contract coverage.
//!
//! Freezes the public contract for DB-backed memory economy reports, score
//! breakdowns, simulations, and report-only pruning plans.

use ee::core::economy::{
    EconomyPrunePlanOptions, EconomyReportOptions, EconomyScoreOptions, EconomySimulateOptions,
    generate_economy_report, generate_prune_plan, score_artifact, simulate_budgets,
};
use ee::db::{CreateFeedbackEventInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::WorkspaceId;
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), String>;
const UNSATISFIED_DEGRADED_MODE_EXIT: i32 = 7;
const SUCCESS_EXIT: i32 = 0;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_json_equal(actual: Option<&JsonValue>, expected: JsonValue, context: &str) -> TestResult {
    let actual = actual.ok_or_else(|| format!("{context}: missing JSON field"))?;
    ensure(
        actual == &expected,
        format!("{context}: expected {expected:?}, got {actual:?}"),
    )
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

struct EconomyDbFixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    workspace_id: String,
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn economy_fixture() -> Result<EconomyDbFixture, String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database = workspace.join(".ee").join("ee.db");
    let workspace_id = stable_workspace_id(&workspace);
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("workspace".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;

    Ok(EconomyDbFixture {
        _temp: temp,
        workspace,
        database,
        workspace_id,
    })
}

fn add_memory(
    fixture: &EconomyDbFixture,
    id: &str,
    content: &str,
    confidence: f32,
    utility: f32,
    tags: &[&str],
) -> TestResult {
    let connection =
        DbConnection::open_file(&fixture.database).map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            id,
            &CreateMemoryInput {
                workspace_id: fixture.workspace_id.clone(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: content.to_owned(),
                confidence,
                utility,
                importance: 0.5,
                provenance_uri: Some("file://AGENTS.md#L1".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: Some("economy-contract".to_owned()),
                tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())
}

fn add_feedback(fixture: &EconomyDbFixture, id: &str, target_id: &str, signal: &str) -> TestResult {
    let connection =
        DbConnection::open_file(&fixture.database).map_err(|error| error.to_string())?;
    connection
        .insert_feedback_event(
            id,
            &CreateFeedbackEventInput {
                workspace_id: fixture.workspace_id.clone(),
                target_type: "memory".to_owned(),
                target_id: target_id.to_owned(),
                signal: signal.to_owned(),
                weight: 1.0,
                source_type: "outcome_observed".to_owned(),
                source_id: Some(format!("src_{id}")),
                reason: Some("economy contract fixture feedback".to_owned()),
                evidence_json: Some("{\"schema\":\"fixture.economy.feedback.v1\"}".to_owned()),
                session_id: Some("session_economy_contract".to_owned()),
            },
        )
        .map_err(|error| error.to_string())
}

fn run_json_with_exit(args: &[&str], expected_exit: i32) -> Result<JsonValue, String> {
    let output = run_ee(args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

    ensure(
        output.status.code() == Some(expected_exit),
        format!(
            "ee {} returned {:?}, expected {expected_exit}; stderr: {stderr}",
            args.join(" "),
            output.status.code()
        ),
    )?;
    ensure(
        stderr.is_empty(),
        format!(
            "json command must keep diagnostics off stderr for ee {}: {stderr}",
            args.join(" ")
        ),
    )?;

    serde_json::from_str(&stdout).map_err(|error| {
        format!(
            "stdout must be parseable JSON for ee {}: {error}; stdout: {stdout}",
            args.join(" ")
        )
    })
}

fn assert_success_envelope(actual: &JsonValue, command: &str, payload_key: &str) -> TestResult {
    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String("ee.response.v1".to_string()),
        "economy success envelope schema",
    )?;
    ensure_json_equal(
        actual.get("success"),
        JsonValue::Bool(true),
        "economy success flag",
    )?;
    ensure_json_equal(
        actual.pointer("/data/command"),
        JsonValue::String(command.to_string()),
        "economy command",
    )?;
    ensure(
        actual.pointer(&format!("/data/{payload_key}")).is_some(),
        format!("missing economy payload key {payload_key}"),
    )
}

fn json_number(actual: Option<&JsonValue>, context: &str) -> Result<f64, String> {
    actual
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| format!("{context}: expected JSON number, got {actual:?}"))
}

#[test]
fn gate20_core_economy_reads_db_metrics_without_seed_artifacts() -> TestResult {
    let fixture = economy_fixture()?;
    let protected_id = "mem_00000000000000000000002001";
    let noisy_id = "mem_00000000000000000000002002";
    add_memory(
        &fixture,
        protected_id,
        "Protect release verification safeguards even when sparse.",
        0.9,
        0.8,
        &["tail-risk"],
    )?;
    add_memory(
        &fixture,
        noisy_id,
        "Review noisy advice before surfacing it again.",
        0.6,
        0.55,
        &[],
    )?;
    add_feedback(
        &fixture,
        "fb_00000000000000000000002001",
        noisy_id,
        "harmful",
    )?;
    add_feedback(
        &fixture,
        "fb_00000000000000000000002002",
        noisy_id,
        "harmful",
    )?;
    add_feedback(
        &fixture,
        "fb_00000000000000000000002003",
        noisy_id,
        "helpful",
    )?;

    let report = generate_economy_report(&EconomyReportOptions {
        workspace_path: fixture.workspace.clone(),
        database_path: fixture.database.clone(),
        artifact_type: Some("memory".to_owned()),
        min_utility: None,
        include_debt: true,
        include_reserves: true,
    })
    .map_err(|error| error.message())?;
    ensure(
        report.scored_artifact_ids == vec![protected_id.to_owned(), noisy_id.to_owned()],
        format!(
            "unexpected scored artifact ids: {:?}",
            report.scored_artifact_ids
        ),
    )?;
    ensure(
        report
            .formula_components
            .contains(&"false_alarm_rate".to_owned()),
        "formula components must name false_alarm_rate",
    )?;
    ensure(
        report
            .tail_risk_reserves
            .as_ref()
            .is_some_and(|reserves| reserves.critical_memories == 1),
        "tail-risk reserves must count explicit protected memory",
    )?;

    let noisy_score = score_artifact(&EconomyScoreOptions {
        workspace_path: fixture.workspace.clone(),
        database_path: fixture.database.clone(),
        artifact_id: noisy_id.to_owned(),
        artifact_type: "memory".to_owned(),
        breakdown: true,
    })
    .map_err(|error| error.message())?;
    ensure(
        noisy_score.false_alarm_rate > 0.6,
        format!(
            "expected high false-alarm rate, got {}",
            noisy_score.false_alarm_rate
        ),
    )?;
    ensure(
        noisy_score
            .breakdown
            .as_ref()
            .is_some_and(|breakdown| breakdown.retrieval_frequency == 3),
        "score breakdown must count feedback events",
    )?;

    let prune = generate_prune_plan(&EconomyPrunePlanOptions {
        workspace_path: fixture.workspace.clone(),
        database_path: fixture.database.clone(),
        dry_run: true,
        max_recommendations: 10,
    })
    .map_err(|error| error.message())?;
    ensure(prune.read_only, "prune plan must remain read-only")?;
    ensure(
        prune
            .recommendations
            .iter()
            .any(|recommendation| recommendation.id.contains(noisy_id)),
        "noisy DB memory should produce a review recommendation",
    )?;
    ensure(
        prune.recommendations.iter().all(|recommendation| {
            !recommendation.id.contains(protected_id) || recommendation.action == "reserve_review"
        }),
        "tail-risk protected memory must not be retired or demoted",
    )?;

    let simulation = simulate_budgets(&EconomySimulateOptions {
        workspace_path: fixture.workspace.clone(),
        database_path: fixture.database.clone(),
        baseline_budget_tokens: 4_000,
        budget_tokens: vec![2_000, 8_000],
        context_profile: "balanced".to_owned(),
        situation_profile: "standard".to_owned(),
    })
    .map_err(|error| error.message())?;
    ensure(simulation.read_only, "simulation must be read-only")?;
    ensure(
        simulation.ranking_state_unchanged,
        "ranking hash must stay unchanged after simulation",
    )?;
    ensure(
        simulation.scored_artifact_ids == vec![protected_id.to_owned(), noisy_id.to_owned()],
        "simulation must score DB artifact IDs only",
    )?;

    let summary = serde_json::json!({
        "schema": "ee.e2e.economy_gate20_core_log.v1",
        "database": {
            "path": fixture.database.display().to_string(),
            "workspaceId": fixture.workspace_id,
        },
        "scoredArtifactIds": report.scored_artifact_ids,
        "formulaComponents": report.formula_components,
        "mutationPosture": {
            "report": report.mutation_status,
            "prunePlan": prune.mutation_status,
            "simulation": simulation.mutation_status,
        },
        "schemaGoldenStatus": "core_contract_json_shape_asserted",
        "firstFailureDiagnosis": "none",
    });
    ensure(
        summary["scoredArtifactIds"].as_array().is_some_and(|ids| {
            ids.iter().all(|id| {
                id.as_str()
                    .is_some_and(|text| text.starts_with("mem_000000000000000000000020"))
            })
        }),
        "summary must not contain seed artifact IDs",
    )
}

#[test]
fn gate20_cli_economy_reads_db_metrics_without_seed_artifacts() -> TestResult {
    let fixture = economy_fixture()?;
    let protected_id = "mem_00000000000000000000002101";
    let noisy_id = "mem_00000000000000000000002102";
    add_memory(
        &fixture,
        protected_id,
        "Protect release verification safeguards even when sparse.",
        0.9,
        0.8,
        &["tail-risk"],
    )?;
    add_memory(
        &fixture,
        noisy_id,
        "Review noisy advice before surfacing it again.",
        0.6,
        0.55,
        &[],
    )?;
    add_feedback(
        &fixture,
        "fb_00000000000000000000002101",
        noisy_id,
        "harmful",
    )?;
    add_feedback(
        &fixture,
        "fb_00000000000000000000002102",
        noisy_id,
        "harmful",
    )?;
    add_feedback(
        &fixture,
        "fb_00000000000000000000002103",
        noisy_id,
        "helpful",
    )?;

    let workspace = fixture.workspace.display().to_string();
    let report = run_json_with_exit(
        &[
            "--json",
            "--workspace",
            workspace.as_str(),
            "economy",
            "report",
            "--include-debt",
            "--include-reserves",
        ],
        SUCCESS_EXIT,
    )?;
    assert_success_envelope(&report, "economy report", "report")?;
    ensure_json_equal(
        report.pointer("/data/report/scoredArtifactIds"),
        serde_json::json!([protected_id, noisy_id]),
        "CLI report scored artifact IDs",
    )?;
    ensure(
        report
            .pointer("/data/report/formulaComponents")
            .and_then(JsonValue::as_array)
            .is_some_and(|components| {
                components
                    .iter()
                    .any(|component| component == "false_alarm_rate")
            }),
        "CLI report must expose false_alarm_rate formula component",
    )?;
    ensure_json_equal(
        report.pointer("/data/report/tailRiskReserves/criticalMemories"),
        serde_json::json!(1),
        "CLI report tail-risk reserve count",
    )?;

    let score = run_json_with_exit(
        &[
            "--json",
            "--workspace",
            workspace.as_str(),
            "economy",
            "score",
            noisy_id,
            "--artifact-type",
            "memory",
            "--breakdown",
        ],
        SUCCESS_EXIT,
    )?;
    assert_success_envelope(&score, "economy score", "score")?;
    ensure(
        json_number(
            score.pointer("/data/score/falseAlarmRate"),
            "false alarm rate",
        )? > 0.6,
        "CLI score must derive high false-alarm rate from feedback",
    )?;
    ensure_json_equal(
        score.pointer("/data/score/breakdown/retrievalFrequency"),
        serde_json::json!(3),
        "CLI score feedback count",
    )?;

    let prune = run_json_with_exit(
        &[
            "--json",
            "--workspace",
            workspace.as_str(),
            "economy",
            "prune-plan",
            "--dry-run",
            "--max-recommendations",
            "10",
        ],
        SUCCESS_EXIT,
    )?;
    assert_success_envelope(&prune, "economy prune-plan", "prunePlan")?;
    ensure_json_equal(
        prune.pointer("/data/prunePlan/readOnly"),
        JsonValue::Bool(true),
        "CLI prune read-only flag",
    )?;
    ensure(
        prune
            .pointer("/data/prunePlan/recommendations")
            .and_then(JsonValue::as_array)
            .is_some_and(|recommendations| {
                recommendations.iter().any(|recommendation| {
                    recommendation
                        .get("id")
                        .and_then(JsonValue::as_str)
                        .is_some_and(|id| id.contains(noisy_id))
                }) && recommendations.iter().all(|recommendation| {
                    !recommendation
                        .get("id")
                        .and_then(JsonValue::as_str)
                        .is_some_and(|id| id.contains(protected_id))
                        || recommendation.get("action").and_then(JsonValue::as_str)
                            == Some("reserve_review")
                })
            }),
        "CLI prune recommendations must review noisy rows and protect tail-risk rows",
    )?;

    let simulation = run_json_with_exit(
        &[
            "--json",
            "--workspace",
            workspace.as_str(),
            "economy",
            "simulate",
            "--baseline-budget",
            "4000",
            "--budget",
            "2000",
            "--budget",
            "8000",
        ],
        SUCCESS_EXIT,
    )?;
    assert_success_envelope(&simulation, "economy simulate", "simulation")?;
    ensure_json_equal(
        simulation.pointer("/data/simulation/rankingStateUnchanged"),
        JsonValue::Bool(true),
        "CLI simulation ranking state",
    )?;
    ensure_json_equal(
        simulation.pointer("/data/simulation/scoredArtifactIds"),
        serde_json::json!([protected_id, noisy_id]),
        "CLI simulation DB artifact IDs",
    )?;

    let summary = serde_json::json!({
        "schema": "ee.e2e.economy_gate20_cli_log.v1",
        "commands": [
            "economy report",
            "economy score",
            "economy prune-plan",
            "economy simulate"
        ],
        "database": {
            "path": fixture.database.display().to_string(),
            "workspaceId": fixture.workspace_id,
        },
        "scoredArtifactIds": report.pointer("/data/report/scoredArtifactIds"),
        "formulaComponents": report.pointer("/data/report/formulaComponents"),
        "mutationPosture": {
            "report": report.pointer("/data/report/mutationStatus"),
            "prunePlan": prune.pointer("/data/prunePlan/mutationStatus"),
            "simulation": simulation.pointer("/data/simulation/mutationStatus"),
        },
        "schemaGoldenStatus": "cli_contract_json_shape_asserted",
        "firstFailureDiagnosis": "none",
    });
    ensure(
        summary
            .get("scoredArtifactIds")
            .and_then(JsonValue::as_array)
            .is_some_and(|ids| {
                ids.iter().all(|id| {
                    id.as_str()
                        .is_some_and(|text| text.starts_with("mem_000000000000000000000021"))
                })
            }),
        "CLI summary must not contain seed artifact IDs",
    )
}

#[test]
fn gate20_cli_empty_db_abstains_without_seed_artifacts() -> TestResult {
    let fixture = economy_fixture()?;
    let workspace = fixture.workspace.display().to_string();

    let report = run_json_with_exit(
        &[
            "--json",
            "--workspace",
            workspace.as_str(),
            "economy",
            "report",
            "--include-debt",
            "--include-reserves",
        ],
        SUCCESS_EXIT,
    )?;
    assert_success_envelope(&report, "economy report", "report")?;
    ensure_json_equal(
        report.pointer("/data/report/status"),
        JsonValue::String("abstain".to_owned()),
        "empty report status",
    )?;
    ensure_json_equal(
        report.pointer("/data/report/degraded/0/code"),
        JsonValue::String("economy_metrics_empty".to_owned()),
        "empty report degradation",
    )?;
    ensure_json_equal(
        report.pointer("/data/report/scoredArtifactIds"),
        serde_json::json!([]),
        "empty report scored IDs",
    )?;

    let simulation = run_json_with_exit(
        &[
            "--json",
            "--workspace",
            workspace.as_str(),
            "economy",
            "simulate",
            "--baseline-budget",
            "4000",
            "--budget",
            "2000",
        ],
        SUCCESS_EXIT,
    )?;
    assert_success_envelope(&simulation, "economy simulate", "simulation")?;
    ensure_json_equal(
        simulation.pointer("/data/simulation/status"),
        JsonValue::String("abstain".to_owned()),
        "empty simulation status",
    )?;
    ensure_json_equal(
        simulation.pointer("/data/simulation/degraded/0/code"),
        JsonValue::String("economy_metrics_empty".to_owned()),
        "empty simulation degradation",
    )?;

    let prune = run_json_with_exit(
        &[
            "--json",
            "--workspace",
            workspace.as_str(),
            "economy",
            "prune-plan",
            "--dry-run",
            "--max-recommendations",
            "3",
        ],
        SUCCESS_EXIT,
    )?;
    assert_success_envelope(&prune, "economy prune-plan", "prunePlan")?;
    ensure_json_equal(
        prune.pointer("/data/prunePlan/status"),
        JsonValue::String("abstain".to_owned()),
        "empty prune status",
    )?;
    ensure_json_equal(
        prune.pointer("/data/prunePlan/degraded/0/code"),
        JsonValue::String("economy_no_prune_candidates".to_owned()),
        "empty prune degradation",
    )
}

#[test]
fn gate20_cli_missing_database_returns_stable_degraded_error() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let workspace_arg = workspace.display().to_string();

    let actual = run_json_with_exit(
        &[
            "--json",
            "--workspace",
            workspace_arg.as_str(),
            "economy",
            "report",
        ],
        UNSATISFIED_DEGRADED_MODE_EXIT,
    )?;

    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String("ee.error.v1".to_owned()),
        "missing database error schema",
    )?;
    ensure_json_equal(
        actual.pointer("/error/code"),
        JsonValue::String("unsatisfied_degraded_mode".to_owned()),
        "missing database error code",
    )?;
    ensure(
        actual
            .pointer("/error/message")
            .and_then(JsonValue::as_str)
            .is_some_and(|message| message.contains("no database exists")),
        "missing database error message must explain the degraded condition",
    )
}
