use std::time::{Duration, Instant};

use asupersync::{CancelKind, CancelReason, Outcome};
use ee::core::{
    BudgetDimension, CliCancelReason, CliOutcomeClass, CliOutcomeSummary, EXIT_CANCELLED,
    RequestBudget, outcome_class, outcome_exit_code,
};
use ee::models::DomainError;

type TestResult = Result<(), String>;

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

fn budget_exhausted_outcome() -> Outcome<(), DomainError> {
    Outcome::cancelled(CancelReason::cost_budget().with_message("request budget exhausted"))
}

#[test]
fn budget_exhaustion_maps_to_documented_cli_outcome() -> TestResult {
    let now = Instant::now();
    let mut budget = RequestBudget::unbounded_at(now).with_tokens(10);
    budget.record_tokens(11);

    let err = budget
        .check_at(now)
        .err()
        .ok_or("token budget breach must be reported")?;
    ensure_equal(&err.dimension, &BudgetDimension::Tokens, "breach dimension")?;
    ensure_equal(&err.limit, &10, "breach limit")?;
    ensure_equal(&err.used, &11, "breach used")?;

    let outcome = budget_exhausted_outcome();
    ensure_equal(
        &outcome_exit_code(&outcome),
        &EXIT_CANCELLED,
        "budget exit code",
    )?;
    ensure_equal(
        &outcome_class(&outcome),
        &CliOutcomeClass::Cancelled,
        "budget outcome class",
    )?;

    let summary = CliOutcomeSummary::from_outcome(&outcome);
    ensure_equal(&summary.exit_code, &EXIT_CANCELLED, "summary exit code")?;
    ensure_equal(&summary.class, &CliOutcomeClass::Cancelled, "summary class")?;
    ensure_equal(
        &summary.cancel_reason,
        &Some(CliCancelReason::BudgetExhausted),
        "summary cancel reason",
    )?;
    ensure_equal(
        &summary.message.as_deref(),
        &Some("request budget exhausted"),
        "summary message",
    )?;

    Ok(())
}

#[test]
fn budget_dimensions_report_in_deterministic_order() -> TestResult {
    let now = Instant::now();
    let mut budget = RequestBudget::unbounded_at(now)
        .with_wall_clock(Duration::from_millis(5))
        .with_tokens(1)
        .with_memory_bytes(1)
        .with_io_bytes(1);
    budget.record_tokens(2);
    budget.record_memory_bytes(2);
    budget.record_io_bytes(2);

    let err = budget
        .check_at(now + Duration::from_millis(6))
        .err()
        .ok_or("wall-clock breach must win simultaneous budget breaches")?;
    ensure_equal(
        &err.dimension,
        &BudgetDimension::WallClock,
        "first breach dimension",
    )?;

    let mut budget = RequestBudget::unbounded_at(now)
        .with_tokens(1)
        .with_memory_bytes(1)
        .with_io_bytes(1);
    budget.record_tokens(2);
    budget.record_memory_bytes(2);
    budget.record_io_bytes(2);

    let err = budget
        .check_at(now)
        .err()
        .ok_or("token breach must win non-wall-clock budget breaches")?;
    ensure_equal(
        &err.dimension,
        &BudgetDimension::Tokens,
        "second breach dimension",
    )?;

    Ok(())
}

#[test]
fn asupersync_budget_cancel_kinds_share_cli_budget_class() -> TestResult {
    for reason in [
        CancelReason::poll_quota(),
        CancelReason::cost_budget(),
        CancelReason::deadline(),
    ] {
        let kind = reason.kind;
        let outcome: Outcome<(), DomainError> = Outcome::cancelled(reason);
        let summary = CliOutcomeSummary::from_outcome(&outcome);

        ensure_equal(
            &summary.cancel_reason,
            &Some(CliCancelReason::BudgetExhausted),
            "budget cancel reason class",
        )?;
        ensure_equal(
            &outcome_exit_code(&outcome),
            &EXIT_CANCELLED,
            "budget cancel exit",
        )?;

        match kind {
            CancelKind::PollQuota | CancelKind::CostBudget | CancelKind::Deadline => {}
            _ => return Err("unexpected budget cancel kind".to_string()),
        }
    }

    Ok(())
}
