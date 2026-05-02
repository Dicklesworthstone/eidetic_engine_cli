//! Causal trace contract coverage (EE-451).
//!
//! Verifies that `ee causal trace --json` produces stable, schema-compliant
//! output for tracing causal chains over recorder runs, context packs,
//! preflight closes, tripwire checks, and procedure uses.

use ee::core::causal::{TraceOptions, trace_causal_chains};
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
