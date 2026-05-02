use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use asupersync::runtime::yield_now::yield_now;
use asupersync::{
    Budget, CancelKind, CancelReason, Cx, LabConfig, LabRuntime, Outcome, OutcomeError,
    PanicPayload,
};
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

#[derive(Debug, Eq, PartialEq)]
struct LabCancelProbeReport {
    seed: u64,
    steps_delta: u64,
    steps_total: u64,
    trace_fingerprint: u64,
    trace_event_count: u64,
    schedule_hash: u64,
    quiescent: bool,
    invariant_violations: Vec<String>,
    observed_cancel: bool,
    exit_code: u8,
    post_cancel_mutations: u8,
}

fn run_lab_cancel_probe(seed: u64) -> Result<LabCancelProbeReport, String> {
    let mut lab = LabRuntime::new(LabConfig::new(seed).max_steps(64));
    let root = lab.state.create_root_region(Budget::INFINITE);
    let started = Arc::new(AtomicBool::new(false));
    let observed_cancel = Arc::new(AtomicBool::new(false));
    let exit_code = Arc::new(AtomicU8::new(0));
    let post_cancel_mutations = Arc::new(AtomicU8::new(0));

    let task_started = Arc::clone(&started);
    let task_observed_cancel = Arc::clone(&observed_cancel);
    let task_exit_code = Arc::clone(&exit_code);
    let task_post_cancel_mutations = Arc::clone(&post_cancel_mutations);

    let (task_id, _handle) = lab
        .state
        .create_task(root, Budget::INFINITE, async move {
            let Some(cx) = Cx::current() else {
                return Outcome::cancelled(CancelReason::user(
                    "missing current Cx in LabRuntime task",
                ));
            };

            task_started.store(true, Ordering::SeqCst);
            yield_now().await;

            if cx.checkpoint().is_err() {
                let reason = cx
                    .cancel_reason()
                    .unwrap_or_else(CancelReason::parent_cancelled);
                let outcome: Outcome<(), DomainError> = Outcome::cancelled(reason);
                task_exit_code.store(outcome_exit_code(&outcome), Ordering::SeqCst);
                task_observed_cancel.store(true, Ordering::SeqCst);
                return outcome;
            }

            task_post_cancel_mutations.fetch_add(1, Ordering::SeqCst);
            Outcome::Ok(())
        })
        .map_err(|error| format!("failed to create lab cancellation task: {error}"))?;
    lab.scheduler.lock().schedule(task_id, 0);

    lab.step_for_test();
    ensure(
        started.load(Ordering::SeqCst),
        "lab task must reach the first await point before cancellation",
    )?;
    ensure_equal(
        &post_cancel_mutations.load(Ordering::SeqCst),
        &0,
        "pre-cancel mutation count",
    )?;

    let reason = CancelReason::timeout().with_message("EE-208 LabRuntime cancellation");
    let tasks_to_cancel = lab.state.cancel_request(root, &reason, None);
    {
        let mut scheduler = lab.scheduler.lock();
        for (task_id, priority) in tasks_to_cancel {
            scheduler.schedule_cancel(task_id, priority);
        }
    }

    let report = lab.run_until_quiescent_with_report();
    Ok(LabCancelProbeReport {
        seed: report.seed,
        steps_delta: report.steps_delta,
        steps_total: report.steps_total,
        trace_fingerprint: report.trace_fingerprint,
        trace_event_count: report.trace_certificate.event_count,
        schedule_hash: report.trace_certificate.schedule_hash,
        quiescent: report.quiescent,
        invariant_violations: report.invariant_violations,
        observed_cancel: observed_cancel.load(Ordering::SeqCst),
        exit_code: exit_code.load(Ordering::SeqCst),
        post_cancel_mutations: post_cancel_mutations.load(Ordering::SeqCst),
    })
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
fn lab_runtime_region_cancel_hits_checkpoint_without_partial_mutation() -> TestResult {
    let report = run_lab_cancel_probe(0xEE_208)?;

    ensure(report.quiescent, "cancelled lab run must quiesce")?;
    ensure(
        report.invariant_violations.is_empty(),
        format!(
            "cancelled lab run must preserve runtime invariants: {:?}",
            report.invariant_violations
        ),
    )?;
    ensure(
        report.steps_delta > 0,
        "cancelled lab run must execute scheduler steps after cancellation",
    )?;
    ensure(
        report.observed_cancel,
        "task checkpoint must observe region cancellation",
    )?;
    ensure_equal(
        &report.exit_code,
        &EXIT_CANCELLED,
        "checkpoint cancellation maps to CLI exit",
    )?;
    ensure_equal(
        &report.post_cancel_mutations,
        &0,
        "cancelled task must not mutate after checkpoint",
    )
}

#[test]
fn lab_runtime_cancellation_report_is_seed_deterministic() -> TestResult {
    let first = run_lab_cancel_probe(0xEE_208)?;
    let second = run_lab_cancel_probe(0xEE_208)?;

    ensure_equal(&first, &second, "same-seed lab cancellation report")
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
