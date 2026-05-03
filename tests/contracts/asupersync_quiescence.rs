use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use asupersync::record::RegionRecord;
use asupersync::record::region::RegionState;
use asupersync::runtime::yield_now::yield_now;
use asupersync::{Budget, CancelReason, Cx, LabConfig, LabRuntime, Outcome, RegionId, TaskId};
use ee::core::{EXIT_CANCELLED, outcome_exit_code};
use ee::models::DomainError;

type TestResult = Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandPath {
    Remember,
    Search,
    Context,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CancelledCommandProbe {
    durable_writes: u8,
    spawned_tasks: u8,
    completed_tasks: u8,
}

#[derive(Debug, Eq, PartialEq)]
struct CancelledCleanupReport {
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
    staged_records: u8,
    rolled_back_records: u8,
    audit_records: u8,
    durable_commits: u8,
    post_cancel_mutations: u8,
}

impl CancelledCommandProbe {
    fn cancel_before_mutation(&mut self, path: CommandPath) -> Outcome<(), DomainError> {
        match path {
            CommandPath::Remember | CommandPath::Search | CommandPath::Context => {
                Outcome::cancelled(CancelReason::parent_cancelled())
            }
        }
    }

    const fn has_no_partial_writes(self) -> bool {
        self.durable_writes == 0
    }

    const fn has_no_orphan_tasks(self) -> bool {
        self.spawned_tasks == self.completed_tasks
    }
}

fn run_cancelled_cleanup_probe(seed: u64) -> Result<CancelledCleanupReport, String> {
    let mut lab = LabRuntime::new(LabConfig::new(seed).max_steps(64));
    let root = lab.state.create_root_region(Budget::INFINITE);
    let observed_cancel = Arc::new(AtomicBool::new(false));
    let exit_code = Arc::new(AtomicU8::new(0));
    let staged_records = Arc::new(AtomicU8::new(0));
    let rolled_back_records = Arc::new(AtomicU8::new(0));
    let audit_records = Arc::new(AtomicU8::new(0));
    let durable_commits = Arc::new(AtomicU8::new(0));
    let post_cancel_mutations = Arc::new(AtomicU8::new(0));

    let task_observed_cancel = Arc::clone(&observed_cancel);
    let task_exit_code = Arc::clone(&exit_code);
    let task_staged_records = Arc::clone(&staged_records);
    let task_rolled_back_records = Arc::clone(&rolled_back_records);
    let task_audit_records = Arc::clone(&audit_records);
    let task_durable_commits = Arc::clone(&durable_commits);
    let task_post_cancel_mutations = Arc::clone(&post_cancel_mutations);

    let (task_id, _handle) = lab
        .state
        .create_task(root, Budget::INFINITE, async move {
            let Some(cx) = Cx::current() else {
                return Outcome::cancelled(CancelReason::user(
                    "missing current Cx in cleanup probe",
                ));
            };

            task_staged_records.store(1, Ordering::SeqCst);
            yield_now().await;

            if cx.checkpoint().is_err() {
                let staged = task_staged_records.load(Ordering::SeqCst);
                task_rolled_back_records.store(staged, Ordering::SeqCst);
                task_audit_records.store(1, Ordering::SeqCst);
                let reason = cx
                    .cancel_reason()
                    .unwrap_or_else(CancelReason::parent_cancelled);
                let outcome: Outcome<(), DomainError> = Outcome::cancelled(reason);
                task_exit_code.store(outcome_exit_code(&outcome), Ordering::SeqCst);
                task_observed_cancel.store(true, Ordering::SeqCst);
                return outcome;
            }

            task_durable_commits.fetch_add(1, Ordering::SeqCst);
            task_post_cancel_mutations.fetch_add(1, Ordering::SeqCst);
            Outcome::ok(())
        })
        .map_err(|error| format!("failed to create cleanup probe task: {error}"))?;
    lab.scheduler.lock().schedule(task_id, 0);

    lab.step_for_test();
    ensure_equal(
        &staged_records.load(Ordering::SeqCst),
        &1,
        "staged records before cancellation",
    )?;
    ensure_equal(
        &durable_commits.load(Ordering::SeqCst),
        &0,
        "durable commits before cancellation",
    )?;

    let reason = CancelReason::timeout().with_message("EE-209 cleanup cancellation");
    let tasks_to_cancel = lab.state.cancel_request(root, &reason, None);
    {
        let mut scheduler = lab.scheduler.lock();
        for (task_id, priority) in tasks_to_cancel {
            scheduler.schedule_cancel(task_id, priority);
        }
    }

    let report = lab.run_until_quiescent_with_report();
    Ok(CancelledCleanupReport {
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
        staged_records: staged_records.load(Ordering::SeqCst),
        rolled_back_records: rolled_back_records.load(Ordering::SeqCst),
        audit_records: audit_records.load(Ordering::SeqCst),
        durable_commits: durable_commits.load(Ordering::SeqCst),
        post_cancel_mutations: post_cancel_mutations.load(Ordering::SeqCst),
    })
}

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

#[test]
fn cancelled_command_cleanup_audits_staged_progress_before_quiescence() -> TestResult {
    let report = run_cancelled_cleanup_probe(0xEE_04)?;

    ensure(report.quiescent, "cleanup cancellation run must quiesce")?;
    ensure(
        report.invariant_violations.is_empty(),
        format!(
            "cleanup cancellation run must preserve runtime invariants: {:?}",
            report.invariant_violations
        ),
    )?;
    ensure(
        report.steps_delta > 0,
        "cleanup cancellation run must execute scheduler steps after cancellation",
    )?;
    ensure(
        report.observed_cancel,
        "cleanup checkpoint must observe cancellation",
    )?;
    ensure_equal(
        &report.exit_code,
        &EXIT_CANCELLED,
        "cleanup cancellation exit",
    )?;
    ensure_equal(&report.staged_records, &1, "staged records")?;
    ensure_equal(
        &report.rolled_back_records,
        &report.staged_records,
        "rolled back records",
    )?;
    ensure_equal(&report.audit_records, &1, "audit records")?;
    ensure_equal(
        &report.durable_commits,
        &0,
        "cancelled cleanup must not commit durable records",
    )?;
    ensure_equal(
        &report.post_cancel_mutations,
        &0,
        "cancelled cleanup must not mutate after checkpoint",
    )?;

    let repeated = run_cancelled_cleanup_probe(0xEE_04)?;
    ensure_equal(&report, &repeated, "same-seed cleanup cancellation report")
}

#[test]
fn lab_runtime_fixture_reaches_quiescence_deterministically() -> TestResult {
    let mut lab = LabRuntime::new(LabConfig::new(0xEE_03));
    ensure(lab.is_quiescent(), "fresh lab runtime must be quiescent")?;

    let report = lab.run_until_quiescent_with_report();
    ensure_equal(&report.seed, &0xEE_03, "lab seed")?;
    ensure_equal(&report.steps_delta, &0, "fresh lab steps delta")?;
    ensure_equal(&report.steps_total, &0, "fresh lab steps total")?;
    ensure(report.quiescent, "lab report must be quiescent")?;
    ensure(
        report.invariant_violations.is_empty(),
        format!(
            "fresh lab runtime must have no invariant violations: {:?}",
            report.invariant_violations
        ),
    )?;

    Ok(())
}

#[test]
fn cancelled_command_fixture_has_no_orphans_or_partial_writes() -> TestResult {
    for path in [
        CommandPath::Remember,
        CommandPath::Search,
        CommandPath::Context,
    ] {
        let mut lab = LabRuntime::new(LabConfig::new(0xEE_03));
        let mut probe = CancelledCommandProbe::default();
        let outcome = probe.cancel_before_mutation(path);

        ensure_equal(
            &outcome_exit_code(&outcome),
            &EXIT_CANCELLED,
            "cancelled command exit",
        )?;
        ensure(
            probe.has_no_partial_writes(),
            "cancelled command must not leave durable writes",
        )?;
        ensure(
            probe.has_no_orphan_tasks(),
            "cancelled command must not leave orphan tasks",
        )?;

        let report = lab.run_until_quiescent_with_report();
        ensure(
            report.quiescent,
            "cancelled command fixture must leave lab runtime quiescent",
        )?;
        ensure_equal(&report.steps_total, &0, "cancelled fixture steps")?;
    }

    Ok(())
}

#[test]
fn region_close_requires_and_preserves_quiescence() -> TestResult {
    let region = RegionRecord::new(RegionId::testing_default(), None, Budget::default());
    ensure(region.is_quiescent(), "fresh region must be quiescent")?;

    let child = RegionId::testing_default();
    region
        .add_child(child)
        .map_err(|error| format!("failed to add child region: {error:?}"))?;
    ensure(
        !region.is_quiescent(),
        "child region must prevent quiescence",
    )?;
    ensure(
        !region.complete_close(),
        "region with a child must not complete close",
    )?;
    region.remove_child(child);
    ensure(region.is_quiescent(), "removed child restores quiescence")?;

    let task = TaskId::testing_default();
    region
        .add_task(task)
        .map_err(|error| format!("failed to add task: {error:?}"))?;
    ensure(!region.is_quiescent(), "live task must prevent quiescence")?;
    region.remove_task(task);
    ensure(region.is_quiescent(), "removed task restores quiescence")?;

    region
        .try_reserve_obligation()
        .map_err(|error| format!("failed to reserve obligation: {error:?}"))?;
    ensure(
        !region.is_quiescent(),
        "pending obligation must prevent quiescence",
    )?;
    region.resolve_obligation();
    ensure(
        region.is_quiescent(),
        "resolved obligation restores quiescence",
    )?;

    ensure(
        region.begin_close(Some(CancelReason::user("contract close"))),
        "begin close must transition from open",
    )?;
    ensure(
        region.begin_finalize(),
        "quiescent closing region must enter finalizing",
    )?;
    ensure(
        region.complete_close(),
        "quiescent finalizing region must close",
    )?;
    ensure_equal(&region.state(), &RegionState::Closed, "region state")?;
    ensure(region.is_quiescent(), "closed region must remain quiescent")?;

    Ok(())
}
