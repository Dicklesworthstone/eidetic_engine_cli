use std::path::PathBuf;
use std::time::{Duration, Instant};

use asupersync::{CancelKind, CancelReason, Cx, Outcome};
use ee::config::WorkspaceLocation;
use ee::core::{
    BudgetDimension, CapabilitySet, CliCancelReason, CliOutcomeClass, CliOutcomeSummary,
    CommandCancellation, CommandContext, EXIT_CANCELLED, RequestBudget, outcome_class,
    outcome_exit_code,
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

fn cross_review_context_with_budget(budget: RequestBudget) -> CommandContext {
    CommandContext::new(
        WorkspaceLocation::new(PathBuf::from("/tmp/ee-cross-review-cancellation")),
        budget,
        CapabilitySet::read_only(),
    )
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
fn cross_review_command_context_check_cancellation_covers_cx_paths() -> TestResult {
    let live_cx = Cx::for_testing();
    cross_review_context_with_budget(RequestBudget::unbounded())
        .check_cancellation(&live_cx)
        .map_err(|error| format!("live Cx should pass check_cancellation: {error}"))?;

    let cancelled_cx = Cx::for_testing();
    cancelled_cx.set_cancel_reason(CancelReason::user("cross-review cancellation"));
    let error = cross_review_context_with_budget(RequestBudget::unbounded())
        .check_cancellation(&cancelled_cx)
        .expect_err("cancelled Cx must fail check_cancellation");
    let CommandCancellation::Cancelled(reason) = error else {
        return Err("cancelled Cx must retain a typed cancellation reason".to_owned());
    };
    ensure_equal(&reason.kind, &CancelKind::User, "cancelled Cx reason kind")?;
    ensure_equal(
        &reason.message.as_deref(),
        &Some("cross-review cancellation"),
        "cancelled Cx reason message",
    )?;

    let mut exhausted_budget = RequestBudget::unbounded().with_tokens(0);
    exhausted_budget.record_tokens(1);
    let error = cross_review_context_with_budget(exhausted_budget)
        .check_cancellation(&cancelled_cx)
        .expect_err("exhausted budget must fail check_cancellation before Cx cancellation");
    let CommandCancellation::BudgetExceeded(error) = error else {
        return Err("request budget breach must win an already-cancelled Cx".to_owned());
    };
    ensure_equal(
        &error.dimension,
        &BudgetDimension::Tokens,
        "budget-first dimension",
    )?;
    ensure_equal(&error.limit, &0, "budget-first limit")?;
    ensure_equal(&error.used, &1, "budget-first used")
}

#[test]
fn wall_clock_deadline_math_has_stable_remaining_and_failure_shape() -> TestResult {
    let now = Instant::now();
    let budget = RequestBudget::unbounded_at(now).with_wall_clock(Duration::from_millis(250));

    ensure_equal(
        &budget.remaining_wall_clock_at(now),
        &Some(Duration::from_millis(250)),
        "initial remaining wall-clock budget",
    )?;
    ensure_equal(
        &budget.remaining_wall_clock_at(now + Duration::from_millis(125)),
        &Some(Duration::from_millis(125)),
        "midpoint remaining wall-clock budget",
    )?;
    ensure_equal(
        &budget.remaining_wall_clock_at(now + Duration::from_millis(250)),
        &Some(Duration::ZERO),
        "deadline remaining wall-clock budget",
    )?;
    ensure_equal(
        &budget.check_at(now + Duration::from_millis(250)).is_ok(),
        &true,
        "exact deadline remains within budget",
    )?;

    let err = budget
        .check_at(now + Duration::from_millis(251))
        .err()
        .ok_or("deadline breach must be reported")?;
    ensure_equal(
        &err.dimension,
        &BudgetDimension::WallClock,
        "deadline breach dimension",
    )?;
    ensure_equal(&err.limit, &250, "deadline breach limit")?;
    ensure_equal(&err.used, &251, "deadline breach used")?;
    ensure_equal(
        &format!("{err}"),
        &"request budget exceeded: dimension=wall_clock limit=250 used=251".to_string(),
        "deadline breach diagnostic",
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

        ensure_equal(
            &matches!(
                outcome,
                Outcome::Cancelled(CancelReason {
                    kind: CancelKind::PollQuota | CancelKind::CostBudget | CancelKind::Deadline,
                    ..
                })
            ),
            &true,
            "budget cancel kind",
        )?;
    }

    Ok(())
}
