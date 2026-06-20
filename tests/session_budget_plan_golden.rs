//! bd-1clqr.4: golden and property coverage for `ee session-budget plan`.
//!
//! These tests keep the planner contract independent of CLI wall-clock time by
//! calling the pure planner with fixed inputs and fixture-backed ledgers.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use ee::core::session_budget::{BudgetPlan, BudgetPlannerInput, plan_cheapest_next_command};
use serde_json::Value;

type TestResult = Result<(), String>;

const FIXTURES_REL: &str = "tests/fixtures/session_budget";
const GOLDEN_BASELINE_REL: &str = "tests/fixtures/session_budget/plan_baseline.golden.json";
const GOLDEN_AFTER_PACK_REL: &str =
    "tests/fixtures/session_budget/plan_after_costly_pack.golden.json";
const GOLDEN_RCH_BLOCKED_REL: &str =
    "tests/fixtures/session_budget/plan_rch_blocked_cargo_refusal.golden.json";
const STABLE_PREFIX: [&str; 4] = ["primer", "recall", "ask", "proof-skip"];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn load_json(relative: &str) -> Result<Value, String> {
    let path = repo_path(relative);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn fixture_row(name: &str) -> Result<Value, String> {
    load_json(&format!("{FIXTURES_REL}/{name}.json"))
}

fn write_ledger(path: &Path, row_names: &[&str]) -> TestResult {
    let mut encoded = String::new();
    for name in row_names {
        let row = fixture_row(name)?;
        let line =
            serde_json::to_string(&row).map_err(|error| format!("{name}: serialize: {error}"))?;
        encoded.push_str(&line);
        encoded.push('\n');
    }
    fs::write(path, encoded).map_err(|error| format!("write {}: {error}", path.display()))
}

fn temp_ledger(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ee-session-budget-plan-{name}-{}.jsonl",
        std::process::id()
    ))
}

fn fixed_input(ledger_path: Option<PathBuf>) -> BudgetPlannerInput {
    BudgetPlannerInput {
        ledger_path,
        degraded_sources: Vec::new(),
        rch_healthy: false,
        task_hint: Some("prepare release".to_owned()),
        workspace_fingerprint: "d37f1e828e51".to_owned(),
        generated_at: Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap(),
    }
}

fn plan_json(plan: &BudgetPlan) -> Result<Value, String> {
    serde_json::to_value(plan).map_err(|error| format!("serialize plan: {error}"))
}

fn plan_surfaces(plan: &BudgetPlan) -> Vec<&str> {
    std::iter::once(&plan.recommendation)
        .chain(plan.fallbacks.iter())
        .map(|entry| entry.surface.as_str())
        .collect()
}

#[test]
fn baseline_plan_matches_golden_fixture() -> TestResult {
    let ledger = temp_ledger("baseline");
    write_ledger(&ledger, &["cheap_recall"])?;
    let plan = plan_cheapest_next_command(&fixed_input(Some(ledger)));

    assert_eq!(plan_json(&plan)?, load_json(GOLDEN_BASELINE_REL)?);
    assert_eq!(plan.ledger_summary.row_count, 1);
    assert_eq!(plan.ledger_summary.total_wall_clock_ms, 42);
    assert_eq!(
        plan.ledger_summary.most_recent_surface.as_deref(),
        Some("recall")
    );
    Ok(())
}

#[test]
fn costly_pack_row_updates_ledger_summary_without_changing_prefix() -> TestResult {
    let ledger = temp_ledger("after-pack");
    write_ledger(&ledger, &["cheap_recall", "large_pack"])?;
    let plan = plan_cheapest_next_command(&fixed_input(Some(ledger)));

    assert_eq!(plan_json(&plan)?, load_json(GOLDEN_AFTER_PACK_REL)?);
    assert_eq!(plan.ledger_summary.row_count, 2);
    assert_eq!(plan.ledger_summary.total_wall_clock_ms, 360);
    assert_eq!(
        plan.ledger_summary.most_recent_surface.as_deref(),
        Some("pack")
    );
    assert_eq!(plan_surfaces(&plan), STABLE_PREFIX);
    Ok(())
}

#[test]
fn rch_blocked_cargo_hint_refuses_local_cargo_and_matches_golden() -> TestResult {
    let ledger = temp_ledger("rch-blocked");
    write_ledger(
        &ledger,
        &["cheap_recall", "large_pack", "rch_blocked_proof"],
    )?;
    let mut input = fixed_input(Some(ledger));
    input.degraded_sources = vec!["rch".to_owned()];
    input.task_hint = Some("cargo test --test session_budget_plan_golden".to_owned());
    let plan = plan_cheapest_next_command(&input);

    assert_eq!(plan_json(&plan)?, load_json(GOLDEN_RCH_BLOCKED_REL)?);
    assert_eq!(plan.ledger_summary.row_count, 3);
    assert_eq!(plan.ledger_summary.degraded_event_count, 2);
    assert_eq!(
        plan.refusals
            .first()
            .and_then(|refusal| refusal.alternative.as_deref()),
        Some(
            "scripts/rch_verify.sh --summary --no-write -- cargo test --test session_budget_plan_golden"
        )
    );
    Ok(())
}

#[test]
fn lower_output_budgets_take_stable_plan_prefixes() -> TestResult {
    let ledger = temp_ledger("prefix");
    write_ledger(&ledger, &["cheap_recall", "large_pack"])?;
    let plan = plan_cheapest_next_command(&fixed_input(Some(ledger)));
    let surfaces = plan_surfaces(&plan);

    for budget in 1..=surfaces.len() {
        assert_eq!(
            &surfaces[..budget],
            &STABLE_PREFIX[..budget],
            "prefix changed at output budget {budget}"
        );
    }
    Ok(())
}
