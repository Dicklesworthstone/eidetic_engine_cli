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
use ee::models::{DomainError, ProcessExitCode};

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

#[derive(Debug, Eq, PartialEq)]
struct LabSupervisedChildFailureReport {
    seed: u64,
    steps_delta: u64,
    steps_total: u64,
    trace_fingerprint: u64,
    trace_event_count: u64,
    schedule_hash: u64,
    quiescent: bool,
    invariant_violations: Vec<String>,
    child_failed: bool,
    child_domain_error: bool,
    child_exit_code: u8,
    supervisor_audited_failure: bool,
    supervisor_domain_error: bool,
    supervisor_exit_code: u8,
    staged_records: u8,
    rolled_back_records: u8,
    audit_records: u8,
    post_failure_mutations: u8,
}

fn storage_failure(message: &str) -> DomainError {
    DomainError::Storage {
        message: message.to_string(),
        repair: Some("ee status --json".to_string()),
    }
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

fn run_lab_supervised_child_failure_probe(
    seed: u64,
) -> Result<LabSupervisedChildFailureReport, String> {
    let mut lab = LabRuntime::new(LabConfig::new(seed).max_steps(64));
    let root = lab.state.create_root_region(Budget::INFINITE);
    let child_failed = Arc::new(AtomicBool::new(false));
    let child_domain_error = Arc::new(AtomicBool::new(false));
    let child_exit_code = Arc::new(AtomicU8::new(0));
    let supervisor_audited_failure = Arc::new(AtomicBool::new(false));
    let supervisor_domain_error = Arc::new(AtomicBool::new(false));
    let supervisor_exit_code = Arc::new(AtomicU8::new(0));
    let staged_records = Arc::new(AtomicU8::new(0));
    let rolled_back_records = Arc::new(AtomicU8::new(0));
    let audit_records = Arc::new(AtomicU8::new(0));
    let post_failure_mutations = Arc::new(AtomicU8::new(0));

    let child_failed_task = Arc::clone(&child_failed);
    let child_domain_error_task = Arc::clone(&child_domain_error);
    let child_exit_code_task = Arc::clone(&child_exit_code);
    let child_staged_records = Arc::clone(&staged_records);

    let (child_id, _handle) = lab
        .state
        .create_task(root, Budget::INFINITE, async move {
            child_staged_records.store(1, Ordering::SeqCst);
            let outcome: Outcome<(), DomainError> =
                Outcome::err(storage_failure("supervised child storage failure"));
            child_exit_code_task.store(outcome_exit_code(&outcome), Ordering::SeqCst);
            child_domain_error_task.store(
                outcome_class(&outcome) == CliOutcomeClass::DomainError,
                Ordering::SeqCst,
            );
            child_failed_task.store(true, Ordering::SeqCst);
            outcome
        })
        .map_err(|error| format!("failed to create supervised child task: {error}"))?;
    lab.scheduler.lock().schedule(child_id, 200);
    lab.step_for_test();

    ensure(
        child_failed.load(Ordering::SeqCst),
        "supervised child must fail before supervisor cleanup",
    )?;

    let supervisor_seen_child_failed = Arc::clone(&child_failed);
    let supervisor_audited = Arc::clone(&supervisor_audited_failure);
    let supervisor_domain_error_task = Arc::clone(&supervisor_domain_error);
    let supervisor_exit_code_task = Arc::clone(&supervisor_exit_code);
    let supervisor_staged_records = Arc::clone(&staged_records);
    let supervisor_rolled_back_records = Arc::clone(&rolled_back_records);
    let supervisor_audit_records = Arc::clone(&audit_records);
    let supervisor_post_failure_mutations = Arc::clone(&post_failure_mutations);

    let (supervisor_id, _handle) = lab
        .state
        .create_task(root, Budget::INFINITE, async move {
            if supervisor_seen_child_failed.load(Ordering::SeqCst) {
                let staged = supervisor_staged_records.load(Ordering::SeqCst);
                supervisor_rolled_back_records.store(staged, Ordering::SeqCst);
                supervisor_audit_records.store(1, Ordering::SeqCst);
                supervisor_audited.store(true, Ordering::SeqCst);

                let outcome: Outcome<(), DomainError> =
                    Outcome::err(storage_failure("supervisor propagated child failure"));
                supervisor_exit_code_task.store(outcome_exit_code(&outcome), Ordering::SeqCst);
                supervisor_domain_error_task.store(
                    outcome_class(&outcome) == CliOutcomeClass::DomainError,
                    Ordering::SeqCst,
                );
                return outcome;
            }

            supervisor_post_failure_mutations.fetch_add(1, Ordering::SeqCst);
            Outcome::ok(())
        })
        .map_err(|error| format!("failed to create supervisor cleanup task: {error}"))?;
    lab.scheduler.lock().schedule(supervisor_id, 100);

    let report = lab.run_until_quiescent_with_report();
    Ok(LabSupervisedChildFailureReport {
        seed: report.seed,
        steps_delta: report.steps_delta,
        steps_total: report.steps_total,
        trace_fingerprint: report.trace_fingerprint,
        trace_event_count: report.trace_certificate.event_count,
        schedule_hash: report.trace_certificate.schedule_hash,
        quiescent: report.quiescent,
        invariant_violations: report.invariant_violations,
        child_failed: child_failed.load(Ordering::SeqCst),
        child_domain_error: child_domain_error.load(Ordering::SeqCst),
        child_exit_code: child_exit_code.load(Ordering::SeqCst),
        supervisor_audited_failure: supervisor_audited_failure.load(Ordering::SeqCst),
        supervisor_domain_error: supervisor_domain_error.load(Ordering::SeqCst),
        supervisor_exit_code: supervisor_exit_code.load(Ordering::SeqCst),
        staged_records: staged_records.load(Ordering::SeqCst),
        rolled_back_records: rolled_back_records.load(Ordering::SeqCst),
        audit_records: audit_records.load(Ordering::SeqCst),
        post_failure_mutations: post_failure_mutations.load(Ordering::SeqCst),
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
fn lab_runtime_supervised_child_failure_is_audited_and_deterministic() -> TestResult {
    let report = run_lab_supervised_child_failure_probe(0xEE_209)?;

    ensure(report.quiescent, "supervised failure run must quiesce")?;
    ensure(
        report.invariant_violations.is_empty(),
        format!(
            "supervised failure run must preserve runtime invariants: {:?}",
            report.invariant_violations
        ),
    )?;
    ensure(
        report.steps_delta > 0,
        "supervised failure run must execute scheduler steps after child failure",
    )?;
    ensure(report.child_failed, "child failure must be observed")?;
    ensure(
        report.child_domain_error,
        "child failure must map to a domain error",
    )?;
    ensure_equal(
        &report.child_exit_code,
        &(ProcessExitCode::Storage as u8),
        "child failure exit",
    )?;
    ensure(
        report.supervisor_audited_failure,
        "supervisor must record failure audit evidence",
    )?;
    ensure(
        report.supervisor_domain_error,
        "supervisor must propagate a domain error",
    )?;
    ensure_equal(
        &report.supervisor_exit_code,
        &(ProcessExitCode::Storage as u8),
        "supervisor failure exit",
    )?;
    ensure_equal(&report.staged_records, &1, "staged record count")?;
    ensure_equal(
        &report.rolled_back_records,
        &report.staged_records,
        "rolled back records",
    )?;
    ensure_equal(&report.audit_records, &1, "audit record count")?;
    ensure_equal(
        &report.post_failure_mutations,
        &0,
        "supervisor must not mutate after auditing child failure",
    )?;

    let repeated = run_lab_supervised_child_failure_probe(0xEE_209)?;
    ensure_equal(&report, &repeated, "same-seed supervised failure report")
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
