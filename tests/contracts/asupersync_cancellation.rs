use asupersync::{CancelKind, CancelReason, Outcome, OutcomeError, PanicPayload};
use ee::core::{
    CliCancelReason, CliOutcomeClass, CliOutcomeSummary, EXIT_CANCELLED, EXIT_PANICKED,
    outcome_class, outcome_exit_code, run_cli_future,
};
use ee::models::DomainError;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

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

async fn repository_layer() -> Outcome<&'static str, DomainError> {
    Outcome::cancelled(CancelReason::timeout().with_message("contract cancellation"))
}

async fn service_layer() -> Outcome<&'static str, DomainError> {
    match repository_layer().await {
        Outcome::Cancelled(reason) => Outcome::cancelled(reason),
        other => other,
    }
}

async fn command_layer() -> Outcome<&'static str, DomainError> {
    match service_layer().await {
        Outcome::Cancelled(reason) => Outcome::cancelled(reason),
        other => other,
    }
}

#[test]
fn outcome_cancelled_survives_service_layers_and_maps_to_cli_exit() -> TestResult {
    let outcome = run_cli_future(command_layer())
        .map_err(|error| format!("asupersync runtime failed: {error}"))?;

    match &outcome {
        Outcome::Cancelled(reason) => {
            ensure_equal(
                &reason.kind,
                &CancelKind::Timeout,
                "cancel reason survives service layers",
            )?;
            ensure_equal(
                &reason.message.as_deref(),
                &Some("contract cancellation"),
                "cancel message survives service layers",
            )?;
        }
        other => return Err(format!("expected cancelled outcome, got {other:?}")),
    }

    ensure_equal(
        &outcome_exit_code(&outcome),
        &EXIT_CANCELLED,
        "cancelled exit",
    )?;
    ensure_equal(
        &outcome_class(&outcome),
        &CliOutcomeClass::Cancelled,
        "cancelled class",
    )?;

    let summary = CliOutcomeSummary::from_outcome(&outcome);
    ensure_equal(&summary.exit_code, &EXIT_CANCELLED, "summary exit")?;
    ensure_equal(
        &summary.cancel_reason,
        &Some(CliCancelReason::Timeout),
        "summary cancel reason",
    )?;

    Ok(())
}

#[test]
fn panicked_outcome_is_not_retryable_domain_failure() -> TestResult {
    let outcome: Outcome<(), DomainError> = Outcome::panicked(PanicPayload::new("contract panic"));

    ensure_equal(
        &outcome_exit_code(&outcome),
        &EXIT_PANICKED,
        "panicked exit",
    )?;
    ensure_equal(
        &outcome_class(&outcome),
        &CliOutcomeClass::Panicked,
        "panicked class",
    )?;

    let summary = CliOutcomeSummary::from_outcome(&outcome);
    ensure(
        !summary.is_success(),
        "panicked summary must not be success",
    )?;
    ensure_equal(&summary.exit_code, &EXIT_PANICKED, "summary exit")?;
    ensure_equal(&summary.class, &CliOutcomeClass::Panicked, "summary class")?;
    ensure_equal(&summary.cancel_reason, &None, "panicked cancel reason")?;

    match outcome.into_result() {
        Err(OutcomeError::Panicked(payload)) => {
            ensure_equal(&payload.message(), &"contract panic", "panic payload")?;
        }
        other => {
            return Err(format!(
                "panicked outcome must remain OutcomeError::Panicked, got {other:?}"
            ));
        }
    }

    Ok(())
}
