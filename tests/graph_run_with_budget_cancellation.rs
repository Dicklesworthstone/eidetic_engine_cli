//! bd-1y395: pin the actual cancel-on-drop contract that
//! `src/graph/algorithms.rs::run_with_budget_observed` relies on when a
//! per-algorithm budget expires.
//!
//! Asupersync documents the semantics in two places:
//!
//!   * `/dp/asupersync/src/runtime/spawn_blocking.rs:203-206`
//!     > "If this future is dropped before completion, the blocking
//!     >  operation continues to run but its result is discarded."
//!
//!   * `/dp/asupersync/src/runtime/blocking_pool.rs:402-404`
//!     > "If the task is still queued, it will be skipped when dequeued.
//!     >  If the task is currently executing, it will run to completion
//!     >  but its result will be discarded."
//!
//! `run_with_budget_observed` on `AlgorithmTimeout` drops the
//! `spawn_blocking` future inside the ephemeral runtime; per the docs,
//! an in-flight worker thread continues past the timeout and runs to
//! natural completion. These tests confirm that documented behaviour is
//! what the production `ee::graph::algorithms::run_with_budget` codepath
//! actually observes, and capture it as a regression fixture so future
//! asupersync upgrades cannot silently flip the contract.
//!
//! `ee`-side mitigation lives in bd-3p05u: `run_with_budget` caps the
//! number of graph budget workers that may remain in flight after a
//! timeout. This file stays as the observability ground truth for the
//! underlying asupersync drop contract the cap is protecting against.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ee::graph::GraphError;
use ee::graph::algorithms::run_with_budget;

type TestResult = Result<(), String>;

/// Shared state the test workload uses to (1) prove it is still running
/// after run_with_budget has returned, (2) accept a kill signal so the
/// test can clean up without leaving an OS-thread leak, and (3) record
/// that the worker eventually exited via its drop guard.
struct WorkerProbe {
    /// Incremented on every iteration of the workload loop. Lets the
    /// test observe whether the worker is making progress AFTER
    /// run_with_budget already returned AlgorithmTimeout.
    ticks: AtomicU64,
    /// Set by the test to ask the worker to exit. Lets us bound test
    /// runtime and avoid leaking a thread into other `cargo test`
    /// invocations.
    kill: AtomicBool,
    /// Set to true by the workload's drop guard when the closure
    /// finally returns. Lets the test confirm clean teardown after
    /// signalling kill.
    finished: AtomicBool,
}

impl WorkerProbe {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ticks: AtomicU64::new(0),
            kill: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        })
    }
}

/// RAII guard: when the closure ends (return or panic), `finished` is
/// flipped. Used so the test can observe natural-completion semantics
/// rather than just guessing from the tick counter.
struct WorkerFinishedGuard {
    probe: Arc<WorkerProbe>,
}

impl Drop for WorkerFinishedGuard {
    fn drop(&mut self) {
        self.probe.finished.store(true, Ordering::Release);
    }
}

/// Build the closure run_with_budget will hand to spawn_blocking. The
/// closure busy-loops at 1 kHz, incrementing `ticks` on every pass,
/// until `kill` is set or 10 seconds have elapsed (hard cap so a
/// runaway never bleeds across tests).
fn workload(probe: Arc<WorkerProbe>) -> impl FnOnce() -> () + Send + 'static {
    move || {
        let _guard = WorkerFinishedGuard {
            probe: probe.clone(),
        };
        let started = Instant::now();
        while !probe.kill.load(Ordering::Acquire) {
            probe.ticks.fetch_add(1, Ordering::Release);
            thread::sleep(Duration::from_millis(1));
            // Hard cap: if for any reason the kill signal never arrives
            // (e.g. the test panicked before raising it), the worker
            // exits on its own after 10s rather than running for the
            // life of the test process.
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
    }
}

/// bd-1y395: positive contract pin. With a 50 ms budget against a
/// closure that loops indefinitely, run_with_budget must:
///   1. return `Err(AlgorithmTimeout)` promptly (well under the
///      worker's natural lifetime),
///   2. NOT have stopped the worker — the worker continues incrementing
///      ticks for at least the documented "run to natural completion"
///      window after the future was dropped.
///
/// This matches the asupersync docs verbatim. If a future asupersync
/// release ever flips the contract to "drop aborts in-flight blocking
/// work", this test will fail loudly and `run_with_budget_observed`
/// will need to be re-examined — possibly to remove a now-redundant
/// `ee`-side mitigation, possibly because the new asupersync semantics
/// changed something else our code depends on.
#[test]
fn run_with_budget_timeout_does_not_abort_in_flight_spawn_blocking_worker() -> TestResult {
    let probe = WorkerProbe::new();
    let probe_for_workload = Arc::clone(&probe);

    let cx = asupersync::Cx::for_testing();
    let started = Instant::now();
    let outcome = run_with_budget(
        &cx,
        "bd_1y395_orphan_probe",
        Duration::from_millis(50),
        workload(probe_for_workload),
    );
    let returned_after = started.elapsed();

    // (1) Timeout must surface promptly. We give a generous 2 s ceiling
    //     so this stays green on a loaded CI host; in practice
    //     `run_with_budget_observed`'s 10 ms cancellation-poll interval
    //     means the actual return is within tens of ms.
    if returned_after >= Duration::from_secs(2) {
        // Best-effort cleanup before erroring.
        probe.kill.store(true, Ordering::Release);
        return Err(format!(
            "run_with_budget did not return promptly after the 50 ms budget; \
             observed wall-clock {returned_after:?} (cap 2 s)"
        ));
    }

    match outcome {
        Err(GraphError::AlgorithmTimeout {
            algorithm,
            timeout_ms,
        }) => {
            if algorithm != "bd_1y395_orphan_probe" {
                probe.kill.store(true, Ordering::Release);
                return Err(format!(
                    "AlgorithmTimeout reported algorithm={algorithm:?}, \
                     expected bd_1y395_orphan_probe"
                ));
            }
            if timeout_ms != 50 {
                probe.kill.store(true, Ordering::Release);
                return Err(format!(
                    "AlgorithmTimeout reported timeout_ms={timeout_ms}, expected 50"
                ));
            }
        }
        Err(other) => {
            probe.kill.store(true, Ordering::Release);
            return Err(format!(
                "expected AlgorithmTimeout, got {other:?} after {returned_after:?}"
            ));
        }
        Ok(()) => {
            probe.kill.store(true, Ordering::Release);
            return Err(format!(
                "run_with_budget returned Ok against an indefinite workload; \
                 budget enforcement must have broken (returned after {returned_after:?})"
            ));
        }
    }

    // Snapshot ticks at the moment run_with_budget returned. The
    // worker should still be ticking AFTER this point per the
    // documented contract.
    let ticks_at_return = probe.ticks.load(Ordering::Acquire);

    // (2) Observe 200 ms of post-return wall-clock. If asupersync
    //     followed the documented contract (worker keeps running), the
    //     tick counter MUST grow during this window. The worker's
    //     1 ms sleep loop guarantees ~200 increments under normal
    //     scheduling; we conservatively require > 5 to absorb any
    //     scheduling jitter on busy CI runners.
    thread::sleep(Duration::from_millis(200));
    let ticks_after_wait = probe.ticks.load(Ordering::Acquire);

    if ticks_after_wait <= ticks_at_return {
        // Workload exited at or before the run_with_budget return.
        // If finished is true that means the closure ran to its
        // natural 10 s cap — but we only slept 200 ms so it should
        // not have. The only way to get here is if asupersync
        // actually DID interrupt the blocking worker on drop, which
        // would contradict the documented contract. Flip the kill
        // anyway and surface the discrepancy.
        probe.kill.store(true, Ordering::Release);
        return Err(format!(
            "asupersync drop contract drift: ticks_at_return={ticks_at_return}, \
             ticks_after_wait={ticks_after_wait}, finished={finished}; \
             docs at /dp/asupersync/src/runtime/spawn_blocking.rs:203-206 \
             and blocking_pool.rs:402-404 promise the worker continues \
             past handle drop. If this test failed, the contract changed \
             — re-read run_with_budget_observed in src/graph/algorithms.rs \
             and decide whether the ee-side mitigation is still required.",
            finished = probe.finished.load(Ordering::Acquire),
        ));
    }

    // (3) Clean teardown: release the worker. We give it a generous
    //     1 s window to observe the kill flag and the drop guard to
    //     fire. Without this step, every run of this test would leak
    //     an "asupersync-blocking" thread into the cargo test process.
    probe.kill.store(true, Ordering::Release);

    let teardown_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < teardown_deadline {
        if probe.finished.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    if !probe.finished.load(Ordering::Acquire) {
        return Err(
            "worker did not exit within 1 s of receiving the kill signal; \
             test would leak an OS thread. Investigate workload's polling \
             frequency or the kill-flag semantics."
                .to_string(),
        );
    }

    Ok(())
}

/// bd-1y395: secondary contract pin. A workload that finishes well
/// within the budget must return Ok(value), the worker must observe
/// natural completion (drop guard fires), and no tick growth happens
/// after the call returns (since the worker exited cleanly, not
/// orphaned).
#[test]
fn run_with_budget_returns_value_when_workload_finishes_within_budget() -> TestResult {
    let probe = WorkerProbe::new();
    let probe_for_workload = Arc::clone(&probe);

    let cx = asupersync::Cx::for_testing();
    let outcome = run_with_budget(
        &cx,
        "bd_1y395_quick_workload",
        Duration::from_secs(2),
        move || {
            let _guard = WorkerFinishedGuard {
                probe: probe_for_workload.clone(),
            };
            for _ in 0..5 {
                probe_for_workload.ticks.fetch_add(1, Ordering::Release);
                thread::sleep(Duration::from_millis(2));
            }
            42_u64
        },
    );

    let value = outcome.map_err(|error| format!("expected Ok, got {error:?}"))?;
    if value != 42 {
        return Err(format!("expected workload to return 42, got {value}"));
    }
    if !probe.finished.load(Ordering::Acquire) {
        return Err(
            "drop guard did not fire by the time run_with_budget returned Ok; \
             completion path is not joining the worker as expected"
                .to_string(),
        );
    }
    let ticks_at_return = probe.ticks.load(Ordering::Acquire);
    if ticks_at_return < 5 {
        return Err(format!(
            "workload should have ticked 5 times before returning; saw {ticks_at_return}"
        ));
    }

    // No further tick growth — the worker is done, so the counter
    // must stay stable.
    thread::sleep(Duration::from_millis(50));
    let ticks_after = probe.ticks.load(Ordering::Acquire);
    if ticks_after != ticks_at_return {
        return Err(format!(
            "tick counter grew after Ok return: {ticks_at_return} -> {ticks_after}; \
             closure was not truly finished"
        ));
    }

    Ok(())
}
