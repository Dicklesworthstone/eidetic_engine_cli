//! Gate 22 contract and golden tests for causal credit outputs.
//!
//! Covers deterministic trace/estimate/compare/promote-plan payloads and
//! no-mutation guarantees for dry-run-first planning behavior.

use ee::core::causal::{
    CAUSAL_COMPARE_SCHEMA_V1, CAUSAL_ESTIMATE_SCHEMA_V1, CAUSAL_PROMOTE_PLAN_SCHEMA_V1,
    CompareOptions, EstimateOptions, PromotePlanOptions, TraceOptions, compare_causal_evidence,
    estimate_causal_uplift, promote_causal_plan, trace_causal_chains,
};
use ee::models::causal::{CAUSAL_TRACE_SCHEMA_V1, PromotionAction, PromotionPlanStatus};
use serde_json::{Value as JsonValue, json};
use std::env;
use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_field_eq(
    object: &JsonValue,
    field: &str,
    expected: JsonValue,
    context: &str,
) -> TestResult {
    let actual = object
        .get(field)
        .ok_or_else(|| format!("{context}: missing field `{field}`"))?;
    ensure(
        *actual == expected,
        format!("{context}: expected `{field}` = {expected}, got {actual}"),
    )
}

fn repo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path(name: &str) -> PathBuf {
    repo_path()
        .join("tests")
        .join("golden")
        .join("causal")
        .join(format!("{name}.json"))
}

fn normalize_dynamic_timestamps(value: &mut JsonValue) {
    match value {
        JsonValue::Object(map) => {
            for (key, nested) in map.iter_mut() {
                match key.as_str() {
                    "createdAt" | "decidedAt" | "estimatedAt" | "exposedAt" => {
                        *nested = json!("TIMESTAMP");
                    }
                    _ => normalize_dynamic_timestamps(nested),
                }
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                normalize_dynamic_timestamps(item);
            }
        }
        _ => {}
    }
}

fn assert_fixture_json(name: &str, actual: &JsonValue) -> TestResult {
    let mut actual = actual.clone();
    normalize_dynamic_timestamps(&mut actual);

    let update_mode = env::var("UPDATE_GOLDEN").is_ok();
    let path = fixture_path(name);
    let serialized = serde_json::to_string_pretty(&actual)
        .map_err(|error| format!("failed to serialize fixture `{name}`: {error}"))?;

    if update_mode {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        fs::write(&path, serialized + "\n")
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        eprintln!("updated causal fixture: {}", path.display());
        return Ok(());
    }

    let expected_text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "fixture missing: {} ({error}). Run with UPDATE_GOLDEN=1.",
            path.display()
        )
    })?;
    let expected: JsonValue = serde_json::from_str(&expected_text)
        .map_err(|error| format!("fixture {} is invalid JSON: {error}", path.display()))?;
    ensure(
        actual == expected,
        format!("fixture mismatch for `{name}`\nexpected: {expected}\nactual: {actual}"),
    )
}

#[test]
fn causal_trace_json_matches_gate22_fixture() -> TestResult {
    let report = trace_causal_chains(
        &TraceOptions::new()
            .with_run_id("run_fixture_001")
            .with_pack_id("pack_fixture_001")
            .with_preflight_id("pre_fixture_001")
            .with_tripwire_id("trip_fixture_001")
            .with_procedure_id("proc_fixture_001")
            .with_agent_id("agent_fixture_001"),
    );
    ensure(
        !report.chains.is_empty(),
        "trace should include causal chains",
    )?;
    let payload = report.data_json();
    ensure_field_eq(
        &payload,
        "schema",
        json!(CAUSAL_TRACE_SCHEMA_V1),
        "trace schema",
    )?;
    assert_fixture_json("trace", &payload)
}

#[test]
fn causal_estimate_observed_exposure_matches_fixture() -> TestResult {
    let report = estimate_causal_uplift(
        &EstimateOptions::new()
            .with_artifact_id("mem_fixture_001")
            .with_decision_id("decision_fixture_001")
            .with_method("naive")
            .with_assumptions()
            .with_confounders(),
    );
    let payload = report.data_json();
    ensure_field_eq(
        &payload,
        "schema",
        json!(CAUSAL_ESTIMATE_SCHEMA_V1),
        "estimate schema",
    )?;
    assert_fixture_json("estimate_observed_exposure", &payload)
}

#[test]
fn causal_estimate_counterfactual_matches_fixture() -> TestResult {
    let report = estimate_causal_uplift(
        &EstimateOptions::new()
            .with_artifact_id("mem_counterfactual_001")
            .with_chain_id("chain_counterfactual_001")
            .with_method("experiment")
            .with_assumptions()
            .with_confounders(),
    );
    let payload = report.data_json();
    ensure_field_eq(
        &payload,
        "schema",
        json!(CAUSAL_ESTIMATE_SCHEMA_V1),
        "counterfactual estimate schema",
    )?;
    assert_fixture_json("estimate_counterfactual", &payload)
}

#[test]
fn causal_compare_fixture_matches_gate22_snapshot() -> TestResult {
    let report = compare_causal_evidence(
        &CompareOptions::new()
            .with_artifact_id("mem_fixture_001")
            .with_fixture_replay_id("fixture_replay_001")
            .with_shadow_run_id("shadow_run_001")
            .with_counterfactual_episode_id("counterfactual_episode_001")
            .with_experiment_id("experiment_001")
            .with_method("replay"),
    );
    ensure(
        !report.comparisons.is_empty(),
        "compare should produce at least one source comparison",
    )?;
    let payload = report.data_json();
    ensure_field_eq(
        &payload,
        "schema",
        json!(CAUSAL_COMPARE_SCHEMA_V1),
        "compare schema",
    )?;
    assert_fixture_json("compare_fixture", &payload)
}

#[test]
fn causal_promote_plan_dry_run_matches_fixture() -> TestResult {
    let report = promote_causal_plan(
        &PromotePlanOptions::new()
            .with_artifact_id("mem_fixture_001")
            .with_method("replay")
            .with_minimum_uplift(0.05)
            .with_revalidation()
            .with_narrower_routing()
            .with_experiment_proposals()
            .dry_run(),
    );
    ensure(!report.is_empty(), "promote plan should include one plan")?;
    ensure(report.dry_run, "promote plan should run in dry-run mode")?;
    ensure(
        report.plans.iter().all(|plan| plan.dry_run_first),
        "all plans must require dry-run before any mutation",
    )?;
    ensure(
        report
            .plans
            .iter()
            .all(|plan| plan.status == PromotionPlanStatus::DryRunReady),
        "dry-run plans must remain dry_run_ready",
    )?;

    let payload = report.data_json();
    ensure_field_eq(
        &payload,
        "schema",
        json!(CAUSAL_PROMOTE_PLAN_SCHEMA_V1),
        "promote-plan schema",
    )?;
    ensure_field_eq(
        &payload["downstreamEffects"],
        "schema",
        json!("ee.causal.downstream_effects.v1"),
        "downstream schema",
    )?;
    ensure_field_eq(
        &payload["downstreamEffects"]["audit"],
        "rawEvidenceReplaced",
        json!(false),
        "raw evidence replacement guard",
    )?;
    ensure_field_eq(
        &payload["downstreamEffects"]["audit"],
        "silentMutation",
        json!(false),
        "silent mutation guard",
    )?;
    assert_fixture_json("promote_plan_dry_run", &payload)
}

#[test]
fn causal_audit_confounded_matches_fixture_and_no_mutation_policy() -> TestResult {
    let report = promote_causal_plan(
        &PromotePlanOptions::new()
            .with_artifact_id("mem_confounded_001")
            .with_method("naive")
            .with_action(PromotionAction::Demote)
            .with_revalidation(),
    );
    ensure(!report.is_empty(), "expected one confounded plan")?;
    ensure(
        report.plans.iter().all(|plan| plan.dry_run_first),
        "confounded plans must keep dry-run-first enforcement",
    )?;
    ensure(
        report
            .plans
            .iter()
            .all(|plan| plan.status == PromotionPlanStatus::Proposed),
        "non-dry-run report must stay proposed and never auto-apply",
    )?;
    ensure(
        report
            .degradations
            .iter()
            .any(|entry| entry.code == "dry_run_recommended"),
        "confounded report should recommend dry-run boundary",
    )?;
    ensure(
        report
            .plans
            .iter()
            .all(|plan| !plan.blocking_confounder_ids.is_empty()),
        "demotion plan should carry blocking confounder evidence",
    )?;

    let payload = json!({
        "schema": "ee.causal.audit_confounded.v1",
        "command": "causal promote-plan",
        "methodUsed": report.method_used,
        "dryRun": report.dry_run,
        "planStatuses": report
            .plans
            .iter()
            .map(|plan| plan.status.as_str())
            .collect::<Vec<_>>(),
        "dryRunFirst": report
            .plans
            .iter()
            .map(|plan| plan.dry_run_first)
            .collect::<Vec<_>>(),
        "blockingConfounderIds": report
            .plans
            .iter()
            .flat_map(|plan| plan.blocking_confounder_ids.clone())
            .collect::<Vec<_>>(),
        "auditIds": report
            .plans
            .iter()
            .flat_map(|plan| plan.audit_ids.clone())
            .collect::<Vec<_>>(),
        "degradations": report
            .degradations
            .iter()
            .map(|entry| entry.data_json())
            .collect::<Vec<_>>(),
        "recommendations": report.recommendations.data_json(),
        "mutationGuard": "no_direct_apply",
    });
    assert_fixture_json("audit_confounded", &payload)
}
