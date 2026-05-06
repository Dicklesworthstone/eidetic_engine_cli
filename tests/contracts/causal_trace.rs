//! Causal trace, estimate, and promote-plan contract coverage (EE-451, EE-452, EE-454).
//!
//! Verifies that `ee causal trace --json` and `ee causal estimate --json`
//! produce stable, schema-compliant output for tracing causal chains and
//! estimating uplift with evidence tiers, assumptions, and confounders.

use ee::core::causal::{
    CAUSAL_COMPARE_SCHEMA_V1, CAUSAL_ESTIMATE_SCHEMA_V1, CAUSAL_PROMOTE_PLAN_SCHEMA_V1,
    CompareOptions, EstimateOptions, PromotePlanOptions, TraceOptions, compare_causal_evidence,
    estimate_causal_uplift, promote_causal_plan, trace_causal_chains,
};
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::causal::CAUSAL_TRACE_SCHEMA_V1;
use ee::models::{
    MemoryId, RATIONALE_TRACE_SCHEMA_V1, RationaleTrace, RationaleTraceKind, RationaleTracePosture,
    RationaleTraceValidationErrorKind, WorkspaceId, validate_rationale_summary,
};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), String>;

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

fn run_ee_owned(args: &[String]) -> Result<Output, String> {
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

fn cli_json_owned_with_exit(
    args: &[String],
    expected_exit: i32,
    context: &str,
) -> Result<JsonValue, String> {
    let output = run_ee_owned(args)?;
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

fn assert_success_response(json: &JsonValue, command: &str, data_schema: &str) -> TestResult {
    assert_schema_field(json, "ee.response.v1", command)?;
    ensure(
        json.get("success").and_then(JsonValue::as_bool) == Some(true),
        format!("{command}: success must be true"),
    )?;
    ensure(
        json.pointer("/data/schema").and_then(JsonValue::as_str) == Some(data_schema),
        format!("{command}: data schema must be {data_schema}"),
    )?;
    ensure(
        json.pointer("/data/command").and_then(JsonValue::as_str) == Some(command),
        format!("{command}: command field must be {command}"),
    )
}

fn seeded_causal_cli_fixture() -> Result<CausalCliFixture, String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join(".ee"))
        .map_err(|error| format!("failed to create causal fixture workspace: {error}"))?;
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| format!("failed to canonicalize causal fixture: {error}"))?;
    let workspace_id = stable_test_workspace_id(&canonical_workspace);
    let database_path = canonical_workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: canonical_workspace.display().to_string(),
                name: Some("causal contract fixture".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;

    let failure = MemoryId::from_uuid(uuid::Uuid::from_u128(11_001)).to_string();
    let root_a = MemoryId::from_uuid(uuid::Uuid::from_u128(11_002)).to_string();
    let root_b = MemoryId::from_uuid(uuid::Uuid::from_u128(11_003)).to_string();
    for (id, kind, content) in [
        (
            &failure,
            "failure",
            "release failed after unvalidated causal inference",
        ),
        (
            &root_a,
            "root-cause",
            "manual review caught missing evidence",
        ),
        (
            &root_b,
            "root-cause",
            "evidence ledger changed plan selection",
        ),
    ] {
        connection
            .insert_memory(id, &causal_fixture_memory(&workspace_id, kind, content))
            .map_err(|error| error.to_string())?;
    }
    insert_causal_fixture_edge(
        &connection,
        "cev_contract_a",
        &workspace_id,
        &failure,
        &root_a,
        0.4,
    )?;
    insert_causal_fixture_edge(
        &connection,
        "cev_contract_b",
        &workspace_id,
        &failure,
        &root_b,
        0.9,
    )?;

    Ok(CausalCliFixture {
        _temp: temp,
        workspace: canonical_workspace,
        failure,
    })
}

fn stable_test_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn causal_fixture_memory(workspace_id: &str, kind: &str, content: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace_id.to_owned(),
        level: "episodic".to_owned(),
        kind: kind.to_owned(),
        content: content.to_owned(),
        workflow_id: None,
        confidence: 0.9,
        utility: 0.8,
        importance: 0.7,
        provenance_uri: Some("agent-mail://causal-contract".to_owned()),
        trust_class: "agent_validated".to_owned(),
        trust_subclass: None,
        tags: Vec::new(),
        valid_from: None,
        valid_to: None,
    }
}

fn insert_causal_fixture_edge(
    connection: &DbConnection,
    edge_id: &str,
    workspace_id: &str,
    failure_id: &str,
    candidate_cause_id: &str,
    score: f64,
) -> Result<(), String> {
    connection
        .execute_raw(&format!(
            "INSERT INTO causal_evidence (id, workspace_id, failure_id, candidate_cause_id, contribution_score, evidence_uris_json, computed_at, method)
             VALUES ('{edge_id}', '{workspace_id}', '{failure_id}', '{candidate_cause_id}', {score}, '[\"agent-mail://causal-contract/{edge_id}\"]', '2026-05-06T00:00:00Z', 'manual')"
        ))
        .map_err(|error| error.to_string())
}

struct CausalCliFixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    failure: String,
}

impl CausalCliFixture {
    fn args(&self, parts: &[&str]) -> Vec<String> {
        let mut args = vec![
            "--workspace".to_owned(),
            self.workspace.display().to_string(),
            "--json".to_owned(),
        ];
        args.extend(parts.iter().map(|part| (*part).to_owned()));
        args
    }
}

fn first_two_chain_ids(trace: &JsonValue) -> Result<(String, String), String> {
    let chains = trace
        .pointer("/data/chains")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "trace response missing chains array".to_owned())?;
    ensure(
        chains.len() >= 2,
        format!("trace response must include two seeded chains, got {chains:?}"),
    )?;
    let left = chains[0]
        .get("chainId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "first chain missing chainId".to_owned())?
        .to_owned();
    let right = chains[1]
        .get("chainId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "second chain missing chainId".to_owned())?
        .to_owned();
    Ok((left, right))
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

fn assert_degradation_code(json: &JsonValue, expected_code: &str, context: &str) -> TestResult {
    let degradations = json
        .get("degradations")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{context}: missing degradations"))?;
    ensure(
        degradations.iter().any(|item| {
            item.get("code")
                .and_then(JsonValue::as_str)
                .is_some_and(|code| code == expected_code)
        }),
        format!("{context}: missing degradation code {expected_code}"),
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
fn causal_cli_json_subcommands_use_persisted_evidence_ledgers() -> TestResult {
    let fixture = seeded_causal_cli_fixture()?;
    let trace = cli_json_owned_with_exit(
        &fixture.args(&["causal", "trace", &fixture.failure, "--depth", "2"]),
        0,
        "causal trace",
    )?;
    assert_success_response(&trace, "causal trace", CAUSAL_TRACE_SCHEMA_V1)?;
    ensure(
        trace
            .pointer("/data/chains/0/evidenceUris/0")
            .and_then(JsonValue::as_str)
            .is_some_and(|uri| uri.starts_with("agent-mail://causal-contract/")),
        "trace must expose persisted evidence URIs",
    )?;
    let (chain_a, chain_b) = first_two_chain_ids(&trace)?;

    let estimate = cli_json_owned_with_exit(
        &fixture.args(&["causal", "estimate", &chain_b, "--method", "replay"]),
        0,
        "causal estimate",
    )?;
    assert_success_response(&estimate, "causal estimate", CAUSAL_ESTIMATE_SCHEMA_V1)?;
    ensure(
        estimate
            .pointer("/data/estimates/0/chainId")
            .and_then(JsonValue::as_str)
            == Some(chain_b.as_str()),
        "estimate must be tied to the requested chain",
    )?;

    let compare = cli_json_owned_with_exit(
        &fixture.args(&[
            "causal", "compare", &chain_a, &chain_b, "--method", "replay",
        ]),
        0,
        "causal compare",
    )?;
    assert_success_response(&compare, "causal compare", CAUSAL_COMPARE_SCHEMA_V1)?;
    ensure(
        compare
            .pointer("/data/comparisons/0/verdict")
            .and_then(JsonValue::as_str)
            .is_some(),
        "compare must emit a deterministic verdict",
    )?;

    let promote = cli_json_owned_with_exit(
        &fixture.args(&[
            "causal",
            "promote-plan",
            &chain_b,
            "--minimum-uplift",
            "0.05",
            "--include-revalidation",
        ]),
        0,
        "causal promote-plan",
    )?;
    assert_success_response(
        &promote,
        "causal promote-plan",
        CAUSAL_PROMOTE_PLAN_SCHEMA_V1,
    )?;
    ensure(
        promote
            .pointer("/data/curationCandidateIds/0")
            .and_then(JsonValue::as_str)
            .is_some_and(|id| id.starts_with("curate_")),
        "promote-plan must create a curation candidate",
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
fn trace_with_procedure_filter_abstains_without_evidence_ledger() -> TestResult {
    let options = TraceOptions::new().with_procedure_id("proc-001");
    let report = trace_causal_chains(&options);
    let json = report.data_json();

    ensure(
        report.chains.is_empty(),
        "trace must not synthesize causal chains from a procedure ID",
    )?;
    assert_degradation_code(
        &json,
        "causal_evidence_unavailable",
        "procedure-filter trace",
    )
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
fn estimate_with_artifact_abstains_without_outcome_ledger() -> TestResult {
    let options = EstimateOptions::new()
        .with_artifact_id("memory-001")
        .with_decision_id("decision-001");
    let report = estimate_causal_uplift(&options);
    let json = report.data_json();

    ensure(
        report.is_empty(),
        "estimate must not synthesize uplift without persisted outcomes",
    )?;
    assert_degradation_code(&json, "causal_sample_underpowered", "artifact estimate")
}

#[test]
fn estimate_methods_all_abstain_without_evidence_ledgers() -> TestResult {
    for method in ["naive", "replay", "experiment"] {
        let options = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_method(method);
        let report = estimate_causal_uplift(&options);
        let json = report.data_json();

        ensure(
            report.estimates.is_empty(),
            format!("{method} method must not synthesize an estimate"),
        )?;
        ensure(
            report.method_used == method,
            format!("{method} method should be tracked in the report"),
        )?;
        assert_degradation_code(
            &json,
            "causal_sample_underpowered",
            &format!("{method} estimate"),
        )?;
    }
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
    let json = report.data_json();

    ensure(
        !report.has_confounders(),
        "confounders must come from persisted evidence, not generated placeholders",
    )?;
    assert_degradation_code(
        &json,
        "causal_confounders_unavailable",
        "confounder estimate",
    )
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
fn compare_with_sources_abstains_without_comparison_ledgers() -> TestResult {
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

    ensure(
        comparisons.is_empty(),
        "compare must not synthesize verdicts from source IDs",
    )?;
    ensure(
        json.get("summary")
            .and_then(|summary| summary.get("totalComparisons"))
            .and_then(JsonValue::as_u64)
            == Some(0),
        "summary should report zero comparisons",
    )?;
    assert_degradation_code(
        &json,
        "causal_comparison_evidence_unavailable",
        "source comparison",
    )
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
