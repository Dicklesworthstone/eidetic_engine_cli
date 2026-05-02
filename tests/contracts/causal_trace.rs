//! Causal trace, estimate, and promote-plan contract coverage (EE-451, EE-452, EE-454).
//!
//! Verifies that `ee causal trace --json` and `ee causal estimate --json`
//! produce stable, schema-compliant output for tracing causal chains and
//! estimating uplift with evidence tiers, assumptions, and confounders.

use ee::core::causal::{
    CAUSAL_ESTIMATE_SCHEMA_V1, CAUSAL_PROMOTE_PLAN_SCHEMA_V1, ConfidenceState, EstimateOptions,
    PromotePlanOptions, TraceOptions, estimate_causal_uplift, promote_causal_plan,
    trace_causal_chains,
};
use ee::models::causal::CAUSAL_TRACE_SCHEMA_V1;
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
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
