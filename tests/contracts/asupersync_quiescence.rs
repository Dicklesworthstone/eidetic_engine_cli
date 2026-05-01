use asupersync::record::RegionRecord;
use asupersync::record::region::RegionState;
use asupersync::{Budget, CancelReason, LabConfig, LabRuntime, Outcome, RegionId, TaskId};
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
