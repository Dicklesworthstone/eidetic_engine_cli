//! Causal trace, estimate, and promote-plan contract coverage (EE-451, EE-452, EE-454).
//!
//! Verifies that `ee causal trace --json` and `ee causal estimate --json`
//! produce stable, schema-compliant output for tracing causal chains and
//! estimating uplift with evidence tiers, assumptions, and confounders.

use ee::core::causal::{
    CAUSAL_COMPARE_SCHEMA_V1, CAUSAL_ESTIMATE_SCHEMA_V1, CAUSAL_PROMOTE_PLAN_SCHEMA_V1,
    CompareOptions, ConfidenceState, EstimateOptions, PromotePlanOptions, TraceOptions,
    compare_causal_evidence, estimate_causal_uplift, promote_causal_plan, trace_causal_chains,
};
use ee::models::causal::CAUSAL_TRACE_SCHEMA_V1;
use ee::models::{
    RATIONALE_TRACE_SCHEMA_V1, RationaleTrace, RationaleTraceKind, RationaleTracePosture,
    RationaleTraceValidationErrorKind, validate_rationale_summary,
};
use serde_json::Value as JsonValue;
use std::process::{Command, Output};

type TestResult = Result<(), String>;
const UNSATISFIED_DEGRADED_MODE_EXIT: i32 = 7;

const CAUSAL_COMPARE_ALL_SOURCES_GOLDEN: &str =
    // EE-453: deterministic snapshot across replay, shadow, counterfactual, and experiment inputs.
    include_str!("../fixtures/golden/causal/compare_all_sources.json.golden");

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

fn assert_clean_machine_stdout(stdout: &str, context: &str) -> TestResult {
    ensure(
        stdout.starts_with('{'),
        format!("{context}: JSON stdout must start with an object, got {stdout:?}"),
    )?;
    ensure(
        stdout.ends_with('\n'),
        format!("{context}: JSON stdout must end with a newline, got {stdout:?}"),
    )?;
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        ensure(
            !matches!(
                trimmed.split_once(':').map(|(prefix, _)| prefix),
                Some("warning" | "error")
            ) && !trimmed.starts_with("[INFO]")
                && !trimmed.starts_with("[WARN]")
                && !trimmed.starts_with("[ERROR]"),
            format!("{context}: diagnostics leaked to stdout line {line:?}"),
        )?;
    }
    Ok(())
}

fn cli_json_with_exit(
    args: &[&str],
    expected_exit: i32,
    context: &str,
) -> Result<JsonValue, String> {
    let output = run_ee(args)?;
    ensure(
        output.status.code() == Some(expected_exit),
        format!(
            "{context}: command returned {:?}, expected {expected_exit}; stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "{context}: JSON command stderr must stay empty, got {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{context}: stdout was not UTF-8: {error}"))?;
    assert_clean_machine_stdout(&stdout, context)?;
    let json: JsonValue = serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout was not JSON: {error}; stdout={stdout}"))?;
    Ok(json)
}

fn assert_causal_unavailable(json: &JsonValue, command: &str, context: &str) -> TestResult {
    assert_schema_field(json, "ee.response.v1", context)?;
    ensure(
        json.get("success").and_then(JsonValue::as_bool) == Some(false),
        format!("{context}: success must be false"),
    )?;
    ensure(
        json.pointer("/data/command").and_then(JsonValue::as_str) == Some(command),
        format!("{context}: command field must be {command}"),
    )?;
    ensure(
        json.pointer("/data/code").and_then(JsonValue::as_str)
            == Some("causal_evidence_unavailable"),
        format!("{context}: missing causal_evidence_unavailable code"),
    )?;
    ensure(
        json.pointer("/data/repair").and_then(JsonValue::as_str) == Some("ee status --json"),
        format!("{context}: repair must point to status"),
    )?;
    ensure(
        json.pointer("/data/followUpBead")
            .and_then(JsonValue::as_str)
            == Some("eidetic_engine_cli-dz00"),
        format!("{context}: follow-up bead missing"),
    )?;
    ensure(
        json.pointer("/data/evidenceIds")
            .and_then(JsonValue::as_array)
            .is_some_and(Vec::is_empty),
        format!("{context}: evidenceIds must be empty"),
    )?;
    ensure(
        json.pointer("/data/sourceIds")
            .and_then(JsonValue::as_array)
            .is_some_and(Vec::is_empty),
        format!("{context}: sourceIds must be empty"),
    )
}

fn assert_schema_field(json: &JsonValue, expected_schema: &str, context: &str) -> TestResult {
    let schema = json
        .get("schema")
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("{context}: missing schema field"))?;
    ensure(
        schema == expected_schema,
        format!("{context}: expected schema {expected_schema}, got {schema}"),
    )
}

fn assert_has_field(json: &JsonValue, field: &str, context: &str) -> TestResult {
    ensure(
        json.get(field).is_some(),
        format!("{context}: missing required field '{field}'"),
    )
}

fn normalize_compare_numeric_fields(value: &mut JsonValue) {
    let Some(comparisons) = value
        .get_mut("comparisons")
        .and_then(JsonValue::as_array_mut)
    else {
        return;
    };
    for comparison in comparisons {
        for field in ["baselineUplift", "candidateUplift", "upliftDelta"] {
            let Some(number) = comparison
                .get_mut(field)
                .and_then(|value| value.as_f64())
                .map(|raw| (raw * 1_000_000.0).round() / 1_000_000.0)
            else {
                continue;
            };
            comparison[field] = serde_json::json!(number);
        }
    }
}

// ============================================================================
// Gate 22 Public CLI Contract Tests
// ============================================================================

#[test]
fn causal_cli_json_subcommands_degrade_until_evidence_ledgers_exist() -> TestResult {
    let cases: [(&str, &[&str]); 4] = [
        (
            "causal trace",
            &[
                "--json",
                "causal",
                "trace",
                "--run-id",
                "run_fixture_001",
                "--pack-id",
                "pack_fixture_001",
                "--preflight-id",
                "pre_fixture_001",
                "--tripwire-id",
                "trip_fixture_001",
                "--procedure-id",
                "proc_fixture_001",
                "--agent-id",
                "agent_fixture_001",
            ],
        ),
        (
            "causal estimate",
            &[
                "--json",
                "causal",
                "estimate",
                "--artifact-id",
                "mem_fixture_001",
                "--decision-id",
                "decision_fixture_001",
                "--method",
                "naive",
                "--include-assumptions",
                "--include-confounders",
            ],
        ),
        (
            "causal compare",
            &[
                "--json",
                "causal",
                "compare",
                "--artifact-id",
                "mem_fixture_001",
                "--fixture-replay-id",
                "fixture_replay_001",
                "--shadow-run-id",
                "shadow_run_001",
                "--counterfactual-episode-id",
                "counterfactual_episode_001",
                "--experiment-id",
                "experiment_001",
                "--method",
                "replay",
            ],
        ),
        (
            "causal promote-plan",
            &[
                "--json",
                "causal",
                "promote-plan",
                "--artifact-id",
                "mem_fixture_001",
                "--method",
                "replay",
                "--minimum-uplift",
                "0.05",
                "--include-revalidation",
                "--include-narrower-routing",
                "--include-experiment-proposals",
                "--dry-run",
            ],
        ),
    ];

    for (context, args) in cases {
        let json = cli_json_with_exit(args, UNSATISFIED_DEGRADED_MODE_EXIT, context)?;
        assert_causal_unavailable(&json, context, context)?;
    }
    Ok(())
}

#[test]
fn causal_cli_promote_plan_dry_run_degrades_before_mutation() -> TestResult {
    let json = cli_json_with_exit(
        &[
            "--json",
            "causal",
            "promote-plan",
            "--artifact-id",
            "mem_fixture_001",
            "--method",
            "replay",
            "--minimum-uplift",
            "0.05",
            "--include-revalidation",
            "--include-narrower-routing",
            "--include-experiment-proposals",
            "--dry-run",
        ],
        UNSATISFIED_DEGRADED_MODE_EXIT,
        "causal promote-plan dry-run mutation guard",
    )?;
    assert_causal_unavailable(
        &json,
        "causal promote-plan",
        "causal promote-plan dry-run mutation guard",
    )
}

#[test]
fn causal_cli_confounded_demote_degrades_before_review_claims() -> TestResult {
    let json = cli_json_with_exit(
        &[
            "--json",
            "causal",
            "promote-plan",
            "--artifact-id",
            "mem_confounded_001",
            "--method",
            "naive",
            "--action",
            "demote",
            "--include-revalidation",
        ],
        UNSATISFIED_DEGRADED_MODE_EXIT,
        "causal promote-plan confounded demotion",
    )?;
    assert_causal_unavailable(
        &json,
        "causal promote-plan",
        "causal promote-plan confounded demotion",
    )
}

// ============================================================================
// Schema Contract Tests
// ============================================================================

#[test]
fn trace_report_has_correct_schema() -> TestResult {
    let options = TraceOptions::new().with_run_id("test-run-001");
    let report = trace_causal_chains(&options);
    let json = report.data_json();

    assert_schema_field(&json, CAUSAL_TRACE_SCHEMA_V1, "trace report")?;
    assert_has_field(&json, "command", "trace report")?;
    assert_has_field(&json, "chains", "trace report")?;
    assert_has_field(&json, "summary", "trace report")?;
    assert_has_field(&json, "filtersApplied", "trace report")?;
    assert_has_field(&json, "degradations", "trace report")?;
    assert_has_field(&json, "dryRun", "trace report")?;

    Ok(())
}

#[test]
fn trace_report_summary_has_required_fields() -> TestResult {
    let options = TraceOptions::new().with_pack_id("pack-001");
    let report = trace_causal_chains(&options);
    let json = report.data_json();

    let summary = json.get("summary").ok_or("missing summary field")?;

    assert_has_field(summary, "totalChains", "summary")?;
    assert_has_field(summary, "totalExposures", "summary")?;
    assert_has_field(summary, "totalDecisions", "summary")?;

    Ok(())
}

#[test]
fn trace_chain_has_required_fields() -> TestResult {
    let options = TraceOptions::new().with_procedure_id("proc-001");
    let report = trace_causal_chains(&options);

    ensure(
        !report.chains.is_empty(),
        "expected at least one chain with procedure filter",
    )?;

    let chain = &report.chains[0];
    let json = chain.data_json();

    assert_has_field(&json, "chainId", "chain")?;
    assert_has_field(&json, "decisionTrace", "chain")?;
    assert_has_field(&json, "exposures", "chain")?;
    assert_has_field(&json, "recorderRunIds", "chain")?;
    assert_has_field(&json, "contextPackIds", "chain")?;
    assert_has_field(&json, "preflightIds", "chain")?;
    assert_has_field(&json, "tripwireIds", "chain")?;
    assert_has_field(&json, "procedureIds", "chain")?;

    Ok(())
}

#[test]
fn rationale_trace_schema_links_to_causal_trace_without_private_reasoning() -> TestResult {
    let trace = RationaleTrace::new(
        "rat_causal_001",
        RationaleTraceKind::Hypothesis,
        "ProudBasin",
        "Replay evidence supports a recorder-link explanation for the outcome.",
        "2026-05-03T18:30:00Z",
    )
    .map_err(|error| error.to_string())?
    .with_posture(RationaleTracePosture::Supported)
    .with_causal_trace_id("causal_trace_001")
    .with_evidence_uri("agent-mail://eidetic_engine_cli-kz1.2/1477");

    ensure(
        trace.schema == RATIONALE_TRACE_SCHEMA_V1,
        "rationale trace schema must be stable",
    )?;
    ensure(
        trace
            .linked_causal_trace_ids
            .iter()
            .any(|id| id == "causal_trace_001"),
        "rationale trace must link to causal trace IDs",
    )?;
    ensure(
        trace.posture == RationaleTracePosture::Supported,
        "rationale trace must carry support posture",
    )?;

    let private = validate_rationale_summary("private chain-of-thought says this would work");
    ensure(
        private.map_err(|error| error.kind)
            == Err(RationaleTraceValidationErrorKind::PrivateReasoningMaterial),
        "private chain-of-thought markers must be rejected",
    )
}

#[test]
fn dry_run_returns_empty_chains_but_applies_filters() -> TestResult {
    let options = TraceOptions::new()
        .with_memory_id("mem-001")
        .with_agent_id("test-agent")
        .dry_run();

    let report = trace_causal_chains(&options);

    ensure(report.dry_run, "dry_run flag should be true")?;
    ensure(
        report.chains.is_empty(),
        "dry run should return empty chains",
    )?;
    ensure(
        report
            .filters_applied
            .iter()
            .any(|filter| filter.contains("memory_id")),
        "memory_id filter should be applied",
    )?;
    ensure(
        report
            .filters_applied
            .iter()
            .any(|filter| filter.contains("agent_id")),
        "agent_id filter should be applied",
    )?;

    Ok(())
}

#[test]
fn no_filters_returns_degradation() -> TestResult {
    let options = TraceOptions::new();
    let report = trace_causal_chains(&options);

    ensure(
        !report.degradations.is_empty(),
        "should have degradation when no filters",
    )?;
    ensure(
        report.degradations[0].code == "no_filters",
        "degradation code should be 'no_filters'",
    )?;

    Ok(())
}

// ============================================================================
// Filter Application Tests
// ============================================================================

#[test]
fn all_filter_options_are_tracked() -> TestResult {
    let options = TraceOptions::new()
        .with_memory_id("mem-001")
        .with_run_id("run-001")
        .with_pack_id("pack-001")
        .with_preflight_id("pre-001")
        .with_tripwire_id("tw-001")
        .with_procedure_id("proc-001")
        .with_agent_id("agent-001")
        .with_workspace_id("ws-001")
        .with_limit(100);

    let report = trace_causal_chains(&options);

    let filters = &report.filters_applied;
    ensure(
        filters.iter().any(|f| f.contains("memory_id")),
        "memory_id filter",
    )?;
    ensure(
        filters.iter().any(|f| f.contains("run_id")),
        "run_id filter",
    )?;
    ensure(
        filters.iter().any(|f| f.contains("pack_id")),
        "pack_id filter",
    )?;
    ensure(
        filters.iter().any(|f| f.contains("preflight_id")),
        "preflight_id filter",
    )?;
    ensure(
        filters.iter().any(|f| f.contains("tripwire_id")),
        "tripwire_id filter",
    )?;
    ensure(
        filters.iter().any(|f| f.contains("procedure_id")),
        "procedure_id filter",
    )?;
    ensure(
        filters.iter().any(|f| f.contains("agent_id")),
        "agent_id filter",
    )?;
    ensure(
        filters.iter().any(|f| f.contains("workspace_id")),
        "workspace_id filter",
    )?;
    ensure(filters.iter().any(|f| f.contains("limit")), "limit filter")?;

    Ok(())
}

// ============================================================================
// Human Output Tests
// ============================================================================

#[test]
fn human_summary_contains_key_information() -> TestResult {
    let options = TraceOptions::new().with_run_id("run-test");
    let report = trace_causal_chains(&options);
    let summary = report.human_summary();

    ensure(summary.contains("Causal Trace"), "should contain title")?;
    ensure(summary.contains("Chains found:"), "should show chain count")?;
    ensure(
        summary.contains("Total exposures:"),
        "should show exposure count",
    )?;
    ensure(
        summary.contains("Total decisions:"),
        "should show decision count",
    )?;

    Ok(())
}

#[test]
fn dry_run_human_summary_indicates_mode() -> TestResult {
    let options = TraceOptions::new().with_memory_id("mem-001").dry_run();
    let report = trace_causal_chains(&options);
    let summary = report.human_summary();

    ensure(
        summary.contains("[DRY RUN]"),
        "should indicate dry run mode",
    )?;

    Ok(())
}

// ============================================================================
// EE-452: Causal Estimate Contract Tests
// ============================================================================

#[test]
fn estimate_report_has_correct_schema() -> TestResult {
    let options = EstimateOptions::new()
        .with_artifact_id("art-001")
        .with_decision_id("dec-001");
    let report = estimate_causal_uplift(&options);
    let json = report.data_json();

    assert_schema_field(&json, CAUSAL_ESTIMATE_SCHEMA_V1, "estimate report")?;
    assert_has_field(&json, "command", "estimate report")?;
    assert_has_field(&json, "estimates", "estimate report")?;
    assert_has_field(&json, "assumptions", "estimate report")?;
    assert_has_field(&json, "confounders", "estimate report")?;
    assert_has_field(&json, "summary", "estimate report")?;
    assert_has_field(&json, "filtersApplied", "estimate report")?;
    assert_has_field(&json, "dryRun", "estimate report")?;

    Ok(())
}

#[test]
fn estimate_summary_has_required_fields() -> TestResult {
    let options = EstimateOptions::new().with_artifact_id("art-001");
    let report = estimate_causal_uplift(&options);
    let json = report.data_json();

    let summary = json.get("summary").ok_or("missing summary field")?;

    assert_has_field(summary, "totalEstimates", "summary")?;
    assert_has_field(summary, "totalAssumptions", "summary")?;
    assert_has_field(summary, "totalConfounders", "summary")?;
    assert_has_field(summary, "methodUsed", "summary")?;

    Ok(())
}

#[test]
fn estimate_with_artifact_produces_result() -> TestResult {
    let options = EstimateOptions::new()
        .with_artifact_id("memory-001")
        .with_decision_id("decision-001");
    let report = estimate_causal_uplift(&options);

    ensure(!report.is_empty(), "should produce estimate with filters")?;
    ensure(
        report.estimates[0].artifact_id == "memory-001",
        "artifact_id should match",
    )?;

    Ok(())
}

#[test]
fn estimate_evidence_tiers_are_conservative() -> TestResult {
    let naive = EstimateOptions::new()
        .with_artifact_id("art-001")
        .with_method("naive");
    let replay = EstimateOptions::new()
        .with_artifact_id("art-001")
        .with_method("replay");
    let experiment = EstimateOptions::new()
        .with_artifact_id("art-001")
        .with_method("experiment");

    let naive_report = estimate_causal_uplift(&naive);
    let replay_report = estimate_causal_uplift(&replay);
    let exp_report = estimate_causal_uplift(&experiment);

    ensure(
        naive_report.estimates[0].confidence_state == ConfidenceState::Insufficient,
        "naive method should have insufficient confidence",
    )?;
    ensure(
        replay_report.estimates[0].confidence_state == ConfidenceState::Medium,
        "replay method should have medium confidence",
    )?;
    ensure(
        exp_report.estimates[0].confidence_state == ConfidenceState::High,
        "experiment method should have high confidence",
    )?;

    Ok(())
}

#[test]
fn estimate_includes_assumptions_when_requested() -> TestResult {
    let options = EstimateOptions::new()
        .with_artifact_id("art-001")
        .with_assumptions();
    let report = estimate_causal_uplift(&options);

    ensure(
        !report.assumptions.is_empty(),
        "should include assumptions when requested",
    )?;
    ensure(
        report.assumptions.iter().any(|a| a.code == "stable_unit"),
        "should include stable_unit assumption",
    )?;

    Ok(())
}

#[test]
fn estimate_includes_confounders_when_requested() -> TestResult {
    let options = EstimateOptions::new()
        .with_artifact_id("art-001")
        .with_confounders();
    let report = estimate_causal_uplift(&options);

    ensure(
        report.has_confounders(),
        "should include confounders when requested",
    )?;

    Ok(())
}

#[test]
fn estimate_dry_run_returns_empty_estimates() -> TestResult {
    let options = EstimateOptions::new().with_artifact_id("art-001").dry_run();
    let report = estimate_causal_uplift(&options);

    ensure(report.is_empty(), "dry run should return empty estimates")?;
    ensure(report.dry_run, "dry_run flag should be true")?;

    Ok(())
}

#[test]
fn estimate_human_summary_shows_key_info() -> TestResult {
    let options = EstimateOptions::new()
        .with_artifact_id("art-001")
        .with_method("replay");
    let report = estimate_causal_uplift(&options);
    let summary = report.human_summary();

    ensure(
        summary.contains("Causal Estimate Report"),
        "should contain title",
    )?;
    ensure(summary.contains("Method:"), "should show method")?;
    ensure(
        summary.contains("Estimates found:"),
        "should show estimate count",
    )?;

    Ok(())
}

// ============================================================================
// EE-453: Causal Compare Contract Tests
// ============================================================================

#[test]
fn compare_report_has_correct_schema() -> TestResult {
    let options = CompareOptions::new()
        .with_fixture_replay_id("fixture-001")
        .with_shadow_run_id("shadow-001")
        .with_counterfactual_episode_id("counterfactual-001")
        .with_experiment_id("exp-001")
        .with_method("replay");
    let report = compare_causal_evidence(&options);
    let json = report.data_json();

    assert_schema_field(&json, CAUSAL_COMPARE_SCHEMA_V1, "compare report")?;
    assert_has_field(&json, "command", "compare report")?;
    assert_has_field(&json, "comparisons", "compare report")?;
    assert_has_field(&json, "summary", "compare report")?;
    assert_has_field(&json, "filtersApplied", "compare report")?;
    assert_has_field(&json, "degradations", "compare report")?;
    assert_has_field(&json, "dryRun", "compare report")?;
    Ok(())
}

#[test]
fn compare_without_sources_reports_degradation() -> TestResult {
    let options = CompareOptions::new().with_artifact_id("mem-001");
    let report = compare_causal_evidence(&options);
    let json = report.data_json();
    let degradations = json
        .get("degradations")
        .and_then(JsonValue::as_array)
        .ok_or("missing degradations")?;

    ensure(
        degradations.iter().any(|item| {
            item.get("code")
                .and_then(JsonValue::as_str)
                .is_some_and(|code| code == "no_sources")
        }),
        "should contain no_sources degradation",
    )?;
    Ok(())
}

#[test]
fn compare_with_sources_records_verdicts() -> TestResult {
    let options = CompareOptions::new()
        .with_fixture_replay_id("fixture-001")
        .with_shadow_run_id("shadow-001")
        .with_counterfactual_episode_id("counterfactual-001")
        .with_experiment_id("exp-001")
        .with_method("experiment");
    let report = compare_causal_evidence(&options);
    let json = report.data_json();
    let comparisons = json
        .get("comparisons")
        .and_then(JsonValue::as_array)
        .ok_or("missing comparisons")?;

    ensure(!comparisons.is_empty(), "comparisons should not be empty")?;
    ensure(
        comparisons.iter().all(|item| item.get("verdict").is_some()),
        "each comparison should include verdict",
    )?;
    ensure(
        json.get("summary")
            .and_then(|summary| summary.get("totalComparisons"))
            .and_then(JsonValue::as_u64)
            == Some(4),
        "summary should report four comparisons",
    )?;
    Ok(())
}

#[test]
fn compare_all_sources_matches_golden_fixture() -> TestResult {
    let mut expected: JsonValue = serde_json::from_str(CAUSAL_COMPARE_ALL_SOURCES_GOLDEN)
        .map_err(|error| format!("compare golden must be valid json: {error}"))?;
    let report = compare_causal_evidence(
        &CompareOptions::new()
            .with_fixture_replay_id("fixture-001")
            .with_shadow_run_id("shadow-001")
            .with_counterfactual_episode_id("counterfactual-001")
            .with_experiment_id("exp-001")
            .with_method("experiment"),
    );
    let mut actual = report.data_json();
    normalize_compare_numeric_fields(&mut expected);
    normalize_compare_numeric_fields(&mut actual);

    ensure(
        actual == expected,
        format!(
            "compare report drifted from golden\nactual: {}\nexpected: {}",
            actual, expected
        ),
    )
}

#[test]
fn compare_dry_run_has_empty_comparison_list() -> TestResult {
    let options = CompareOptions::new()
        .with_fixture_replay_id("fixture-001")
        .with_method("matching")
        .dry_run();
    let report = compare_causal_evidence(&options);
    let json = report.data_json();
    let comparisons = json
        .get("comparisons")
        .and_then(JsonValue::as_array)
        .ok_or("missing comparisons")?;

    ensure(
        comparisons.is_empty(),
        "dry run should not emit comparisons",
    )?;
    ensure(
        json.get("dryRun").and_then(JsonValue::as_bool) == Some(true),
        "dry run flag should be true",
    )?;
    Ok(())
}

// ============================================================================
// EE-454: Causal Promote Plan Contract Tests
// ============================================================================

#[test]
fn promote_plan_report_has_correct_schema() -> TestResult {
    let options = PromotePlanOptions::new()
        .with_artifact_id("mem-001")
        .with_method("replay")
        .dry_run();
    let report = promote_causal_plan(&options);
    let json = report.data_json();

    assert_schema_field(&json, CAUSAL_PROMOTE_PLAN_SCHEMA_V1, "promote-plan report")?;
    assert_has_field(&json, "command", "promote-plan report")?;
    assert_has_field(&json, "plans", "promote-plan report")?;
    assert_has_field(&json, "recommendations", "promote-plan report")?;
    assert_has_field(&json, "summary", "promote-plan report")?;
    assert_has_field(&json, "filtersApplied", "promote-plan report")?;
    assert_has_field(&json, "degradations", "promote-plan report")?;
    assert_has_field(&json, "dryRun", "promote-plan report")?;
    Ok(())
}

#[test]
fn promote_plan_dry_run_produces_plan_with_action() -> TestResult {
    let options = PromotePlanOptions::new()
        .with_artifact_id("mem-001")
        .with_method("experiment")
        .dry_run();
    let report = promote_causal_plan(&options);
    ensure(
        !report.is_empty(),
        "promote-plan should produce at least one plan",
    )?;

    let plans = report.data_json().get("plans").cloned().unwrap_or_default();
    ensure(
        plans.as_array().is_some_and(|entries| {
            entries
                .first()
                .and_then(|entry| entry.get("action"))
                .is_some()
        }),
        "first plan should include action field",
    )?;
    Ok(())
}

#[test]
fn promote_plan_unknown_method_reports_degradation() -> TestResult {
    let options = PromotePlanOptions::new()
        .with_artifact_id("mem-001")
        .with_method("mystery")
        .dry_run();
    let report = promote_causal_plan(&options);
    let json = report.data_json();
    let degradations = json
        .get("degradations")
        .and_then(JsonValue::as_array)
        .ok_or("missing degradations")?;

    ensure(
        degradations.iter().any(|item| {
            item.get("code")
                .and_then(JsonValue::as_str)
                .is_some_and(|code| code == "unknown_method")
        }),
        "should contain unknown_method degradation",
    )?;
    Ok(())
}

#[test]
fn promote_plan_includes_experiment_proposal_when_requested() -> TestResult {
    let options = PromotePlanOptions::new()
        .with_artifact_id("mem-001")
        .with_experiment_proposals()
        .dry_run();
    let report = promote_causal_plan(&options);
    let json = report.data_json();
    let proposals = json
        .get("recommendations")
        .and_then(|value| value.get("experimentProposals"))
        .and_then(JsonValue::as_array)
        .ok_or("missing experiment proposals")?;

    ensure(
        !proposals.is_empty(),
        "experiment proposals should be present",
    )?;
    Ok(())
}
