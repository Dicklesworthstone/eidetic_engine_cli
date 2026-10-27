//! One best-effort idle batch per server; no unbounded queue or result cache.

use super::{
    Arc, AtomicBool, Condvar, Duration, InflightPool, Instant, Mutex, Ordering, PathBuf, thread,
};
use crate::core::cass_prefetch::{
    AgentScope, CassPrefetchCandidate, CassPrefetchCoordinator, CassPrefetchPolicyTicket,
    DEFAULT_PREFETCH_BUDGET, PrefetchGeneration, RecencyWeightedFrequencyPredictor, TopicId,
};
use crate::core::search::warm_prefetch_lexical;
use crate::models::CorpusRevision;

type SharedCoordinator = Arc<Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>>;

/// Identity a completed batch is settled against (bd-16pwc.7).
///
/// Carried as one value rather than four more `submit` parameters: the
/// coordinator needs all four together or none of them, and splitting them
/// across a long argument list is how a workspace gets credited to the wrong
/// agent.
#[derive(Clone, Debug)]
pub(super) struct WarmBatchScope {
    pub agent_scope: AgentScope,
    pub workspace: String,
    /// The generation the batch is warmed AGAINST, not the live one.
    pub generation: PrefetchGeneration,
    /// The corpus revision the batch is warmed AGAINST.
    pub corpus_revision: CorpusRevision,
    /// Revoked when the workspace disables or changes adaptive prefetch.
    pub ticket: CassPrefetchPolicyTicket,
}

#[derive(Clone, Debug)]
pub(super) struct CassPrefetchWorker {
    running: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    foreground: Arc<InflightPool>,
    coordinator: SharedCoordinator,
    /// Count of batches that have left the single-flight slot, with the condvar
    /// that announces each one (bd-16pwc.4).
    ///
    /// WHY A COUNTER AND NOT A FLAG. This worker is deliberately
    /// fire-and-forget, so for 28 comments bd-16pwc.4's last clause -- "a second
    /// context call reports a prefetch hit" -- could only be written as a race:
    /// whether the second call saw a staged batch depended on whether the
    /// background warm had finished, which is machine load. The fix is an await
    /// point, and a MONOTONIC COUNTER is the one shape that does not reintroduce
    /// the race on the waiter's side. A waiter reads the count BEFORE triggering
    /// work and then waits for it to pass that mark, so a batch that completes
    /// between the trigger and the wait is already counted -- there is no lost
    /// wakeup and no "it finished too fast" failure mode. A boolean "is a batch
    /// running" cannot express that: by the time the waiter looks, the answer
    /// may be false for either of two opposite reasons.
    ///
    /// The pair mirrors `InflightPool`'s `inflight`/`idle`, including recovering
    /// from a poisoned lock rather than propagating, because this side only ever
    /// increments a count it also holds the lock for.
    settled: Arc<(Mutex<u64>, Condvar)>,
}

struct RunningBatch {
    running: Arc<AtomicBool>,
    settled: Arc<(Mutex<u64>, Condvar)>,
}

impl Drop for RunningBatch {
    fn drop(&mut self) {
        // ORDER MATTERS. Release the single-flight slot BEFORE announcing the
        // settle, so a waiter that wakes on the count and immediately submits
        // again finds the slot free instead of racing this drop and silently
        // getting nothing (`submit` returns early when `reserve` fails).
        self.running.store(false, Ordering::Release);
        let (count, signal) = &*self.settled;
        {
            let mut settled = count
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *settled += 1;
        }
        // Notify with the guard dropped, or every woken waiter blocks straight
        // back on the mutex this thread still holds.
        signal.notify_all();
    }
}

impl CassPrefetchWorker {
    pub(super) fn new(
        shutdown: Arc<AtomicBool>,
        foreground: Arc<InflightPool>,
        coordinator: SharedCoordinator,
    ) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            shutdown,
            foreground,
            coordinator,
            settled: Arc::new((Mutex::new(0), Condvar::new())),
        }
    }

    fn reserve(&self) -> Option<RunningBatch> {
        if self.shutdown.load(Ordering::Acquire)
            || self
                .running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        Some(RunningBatch {
            running: Arc::clone(&self.running),
            settled: Arc::clone(&self.settled),
        })
    }

    /// Batches that have left the single-flight slot since this worker was
    /// built. Read this BEFORE triggering work; see `settled`.
    ///
    /// CFG-GATED, and the visibility is the reason rather than squeamishness
    /// about test hooks. `pub(super)` can only be reached from inside
    /// `src/daemon`, so the only callers this could ever have are in-crate, and
    /// the production path has no reason to wait on speculative work -- that is
    /// the design. Left ungated it would be an unused method under
    /// `clippy --all-targets -- -D warnings`, i.e. born red.
    ///
    /// An out-of-crate integration test cannot use this at any visibility; that
    /// route needs the count exposed through the server handle, because
    /// `start_server` constructs the worker internally and never hands it out.
    #[cfg(test)]
    pub(super) fn batches_settled(&self) -> u64 {
        let (count, _signal) = &*self.settled;
        *count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Block until more than `mark` batches have settled, or `timeout` elapses.
    /// Returns whether the count passed `mark`.
    ///
    /// This counts a batch that was ABANDONED as settled -- shutdown, a
    /// foreground request interrupting the idle wait, or an unwind all drop
    /// `RunningBatch` -- which is deliberate. The question a waiter asks is "is
    /// the speculative work over", not "did it succeed"; conflating the two
    /// would hang a waiter on exactly the paths this worker exists to take.
    /// Whether anything was staged is then a question for the coordinator's
    /// metrics, which is where the answer actually lives.
    ///
    /// Cfg-gated for the same reason as `batches_settled`.
    #[cfg(test)]
    pub(super) fn wait_for_batch_settle(&self, mark: u64, timeout: std::time::Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (count, signal) = &*self.settled;
        let mut current = count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *current <= mark {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next, result) = signal
                .wait_timeout(current, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            current = next;
            if result.timed_out() && *current <= mark {
                return false;
            }
        }
        true
    }

    fn interrupted(&self) -> bool {
        self.shutdown.load(Ordering::Acquire) || !self.foreground.foreground_is_idle()
    }

    fn policy_is_current(&self, ticket: CassPrefetchPolicyTicket) -> bool {
        // Speculation yields to foreground policy changes without waiting for
        // their lock, just as it yields to a foreground search request.
        self.coordinator
            .try_lock()
            .is_ok_and(|coordinator| coordinator.policy_ticket_is_current(ticket))
    }

    fn stage_warm_batch(
        &self,
        scope: WarmBatchScope,
        warmed_topics: Vec<TopicId>,
        spent: Duration,
    ) -> Option<&'static str> {
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !coordinator.policy_ticket_is_current(scope.ticket) {
            return None;
        }
        coordinator.record_warm_batch(
            scope.agent_scope,
            scope.workspace,
            warmed_topics,
            scope.generation,
            &scope.corpus_revision,
            spent,
        )
    }

    pub(super) fn submit(
        &self,
        index_dir: PathBuf,
        generation: u64,
        candidates: Vec<CassPrefetchCandidate>,
        scope: WarmBatchScope,
    ) {
        if candidates.is_empty() || !self.policy_is_current(scope.ticket) {
            return;
        }
        let Some(batch) = self.reserve() else {
            return;
        };
        let worker = self.clone();
        // The originating response still owns its foreground permit. Wait only
        // here, never in dispatch, for that response to drain. There is at most
        // one speculative thread, including this bounded idle wait. A failed
        // spawn drops the captured guard and releases the single-flight slot.
        let spawned = thread::Builder::new()
            .name("ee-cass-prefetch".to_owned())
            .spawn(move || {
                let _batch = batch;
                let ticket = scope.ticket;
                // A custom warm budget never extends the existing maximum
                // idle-queue wait or makes shutdown wait for speculation.
                if !worker
                    .foreground
                    .wait_until_idle(ticket.budget.min(DEFAULT_PREFETCH_BUDGET))
                    || worker.interrupted()
                    || !worker.policy_is_current(ticket)
                {
                    return;
                }
                // Time the warming call HERE. The coordinator reads no clock
                // (bd-kpynd determinism contract), so the budget verdict is
                // made from an elapsed value this side measured.
                let started = Instant::now();
                let outcome = warm_prefetch_lexical(
                    &index_dir,
                    generation,
                    &candidates,
                    ticket.top_k,
                    ticket.budget,
                    || worker.interrupted() || !worker.policy_is_current(ticket),
                );
                let spent = started.elapsed();
                let report = outcome.report;
                let warmed_topics = outcome.warmed.len();
                // bd-16pwc.7: stage the topics that were ACTUALLY warmed so a
                // LATER request can settle them. Still no credit awarded here
                // — a hit is a statement about a later request, which warm
                // time cannot know — but the batch is no longer discarded, so
                // hits, misses and budget_exceeded can finally move.
                let degraded = worker.stage_warm_batch(scope, outcome.warmed, spent);
                tracing::debug!(
                    target: "ee::cass_prefetch",
                    completed_queries = report.completed_queries,
                    matching_queries = report.matching_queries,
                    stop = ?report.stop,
                    warmed_topics,
                    spent_ms = spent.as_millis(),
                    degraded = degraded.unwrap_or("none"),
                    "finished speculative lexical warming"
                );
            });
        if spawned.is_err() {
            tracing::debug!(target: "ee::cass_prefetch", "speculative worker unavailable; foreground unchanged");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_prefetch::{CASS_PREFETCH_BUDGET_EXCEEDED_CODE, CassPrefetchPolicy};

    fn worker() -> CassPrefetchWorker {
        CassPrefetchWorker::new(
            Arc::new(AtomicBool::new(false)),
            InflightPool::new(4),
            Arc::new(Mutex::new(CassPrefetchCoordinator::new())),
        )
    }

    fn scope(worker: &CassPrefetchWorker) -> WarmBatchScope {
        let ticket = worker
            .coordinator
            .lock()
            .unwrap()
            .configure_runtime_policy(CassPrefetchPolicy::from_config(
                &crate::config::SwarmAdaptiveConfig {
                    enabled: Some(true),
                    ..Default::default()
                },
            ))
            .unwrap();
        WarmBatchScope {
            agent_scope: AgentScope::new("agent:test"),
            workspace: "ws".to_owned(),
            generation: PrefetchGeneration::new(0, 7),
            corpus_revision: CorpusRevision::unknown(),
            ticket,
        }
    }

    #[test]
    fn only_one_batch_can_reside_and_drop_releases_it() {
        let worker = worker();
        let batch = worker.reserve().unwrap();
        assert!(worker.reserve().is_none());
        assert!(worker.clone().reserve().is_none());
        drop(batch);
        assert!(worker.reserve().is_some());
    }

    #[test]
    fn shutdown_refuses_new_work_and_interrupts_running_work() {
        let worker = worker();
        let batch = worker.reserve().unwrap();
        assert!(!worker.interrupted());
        worker.shutdown.store(true, Ordering::Release);
        assert!(worker.interrupted());
        drop(batch);
        assert!(worker.reserve().is_none());
    }

    #[test]
    fn foreground_requests_interrupt_warming_without_waiting_for_it() {
        let worker = worker();
        let _batch = worker.reserve().unwrap();
        let request = worker.foreground.try_acquire().unwrap();
        assert!(worker.interrupted());
        drop(request);
        assert!(!worker.interrupted());
    }

    #[test]
    fn separate_servers_do_not_share_singleflight_state() {
        let first = worker();
        let second = worker();
        let _batch = first.reserve().unwrap();
        assert!(second.reserve().is_some());
        first.shutdown.store(true, Ordering::Release);
        assert!(!second.interrupted());
    }

    /// The await point bd-16pwc.4's last clause needs. Four arms, because the
    /// first three alone would all pass against a `wait_for_batch_settle` that
    /// simply returned `true`.
    #[test]
    fn batch_settles_are_countable_and_waitable_bd_16pwc_4() {
        let worker = worker();
        assert_eq!(
            worker.batches_settled(),
            0,
            "a fresh worker has settled nothing"
        );

        // 1. A settle advances the count and releases the waiter.
        let mark = worker.batches_settled();
        let batch = worker.reserve().unwrap();
        drop(batch);
        assert_eq!(worker.batches_settled(), mark + 1);
        assert!(
            worker.wait_for_batch_settle(mark, Duration::from_secs(5)),
            "a waiter marked before the batch must observe its settle"
        );

        // 2. THE RACE THIS TYPE EXISTS TO CLOSE: the batch finished long before
        // anyone waited. A flag would read "not running" and be unable to say
        // whether that meant done or not-yet-started; the mark makes it decidable
        // and the wait returns immediately rather than blocking for the timeout.
        let before = worker.batches_settled();
        assert!(
            worker.wait_for_batch_settle(before.saturating_sub(1), Duration::ZERO),
            "an already-passed mark must satisfy the wait without blocking"
        );

        // 3. PLANTED NEGATIVE. Nothing settles, so the wait must TIME OUT. An
        // implementation that returned true unconditionally would pass arms 1
        // and 2 and fail only here.
        let current = worker.batches_settled();
        assert!(
            !worker.wait_for_batch_settle(current, Duration::from_millis(50)),
            "waiting past the last settle with no work in flight must time out"
        );

        // 4. An ABANDONED batch still settles. Shutdown is the common case --
        // the thread returns before warming anything -- and a waiter that hung
        // on it would hang on exactly the path this worker takes most.
        let mark = worker.batches_settled();
        let batch = worker.reserve().unwrap();
        worker.shutdown.store(true, Ordering::Release);
        drop(batch);
        assert!(
            worker.wait_for_batch_settle(mark, Duration::from_secs(5)),
            "a batch abandoned for shutdown must still announce its settle"
        );
    }

    /// A settle announced from the worker's own thread, which is the shape the
    /// production path takes: `submit` spawns, the waiter is elsewhere.
    #[test]
    fn a_settle_from_another_thread_wakes_the_waiter_bd_16pwc_4() {
        let worker = worker();
        let mark = worker.batches_settled();
        let batch = worker.reserve().unwrap();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            drop(batch);
        });
        assert!(
            worker.wait_for_batch_settle(mark, Duration::from_secs(10)),
            "the condvar must deliver a settle raised on the worker thread"
        );
        handle.join().expect("settling thread joins");
        assert_eq!(worker.batches_settled(), mark + 1);
    }

    #[test]
    fn unwinding_releases_the_slot_and_settles_the_batch_bd_16pwc_4() {
        let worker = worker();
        let mark = worker.batches_settled();
        let result = std::panic::catch_unwind(|| {
            let _batch = worker.reserve().unwrap();
            panic!("simulated backend panic");
        });
        assert!(result.is_err());
        assert!(
            worker.wait_for_batch_settle(mark, Duration::from_secs(5)),
            "an unwind must settle the batch, not leave a waiter hanging"
        );
    }

    #[test]
    fn unwinding_releases_the_slot() {
        let worker = worker();
        let result = std::panic::catch_unwind(|| {
            let _batch = worker.reserve().unwrap();
            panic!("simulated backend panic");
        });
        assert!(result.is_err());
        assert!(worker.reserve().is_some());
    }

    #[test]
    fn no_candidates_do_not_spawn_or_reserve() {
        let worker = worker();
        worker.submit(PathBuf::from("absent"), 7, Vec::new(), scope(&worker));
        assert!(!worker.running.load(Ordering::Acquire));
        assert!(worker.foreground.wait_until_idle(Duration::ZERO));
        // Nothing was warmed, so nothing may be staged for credit.
        assert_eq!(
            worker
                .coordinator
                .lock()
                .unwrap()
                .pending_warm_topics(&AgentScope::new("agent:test"), "ws"),
            0
        );
    }

    #[test]
    fn revoked_policy_refuses_submitted_and_completed_batches() {
        let worker = worker();
        let revoked = scope(&worker);
        let topics = vec![TopicId::new("cargo")];
        worker.stage_warm_batch(revoked.clone(), topics.clone(), Duration::from_millis(1));
        assert_eq!(
            worker
                .coordinator
                .lock()
                .unwrap()
                .pending_warm_topics(&revoked.agent_scope, "ws"),
            1
        );
        worker
            .coordinator
            .lock()
            .unwrap()
            .configure_runtime_policy(CassPrefetchPolicy::default());
        assert!(!worker.policy_is_current(revoked.ticket));
        worker.submit(
            PathBuf::from("absent"),
            7,
            vec![CassPrefetchCandidate::new("cargo", 1.0, "test")],
            revoked.clone(),
        );
        assert!(!worker.running.load(Ordering::Acquire));
        assert_eq!(worker.batches_settled(), 0);

        // A batch that was already executing when the setting changed must
        // not restore pending topics, even if the same policy is re-enabled.
        let current = scope(&worker);
        assert_ne!(current.ticket, revoked.ticket);
        let before = worker
            .coordinator
            .lock()
            .unwrap()
            .metrics_for("ws")
            .cloned();
        worker.stage_warm_batch(revoked, topics.clone(), Duration::from_millis(1));
        {
            let coordinator = worker.coordinator.lock().unwrap();
            assert_eq!(
                coordinator.pending_warm_topics(&current.agent_scope, "ws"),
                0
            );
            assert_eq!(coordinator.metrics_for("ws").cloned(), before);
        }
        worker.stage_warm_batch(current.clone(), topics, Duration::from_millis(1));
        assert_eq!(
            worker
                .coordinator
                .lock()
                .unwrap()
                .pending_warm_topics(&current.agent_scope, "ws"),
            1
        );
    }

    #[test]
    fn changed_budget_revokes_old_batch_and_settles_with_configured_budget() {
        let worker = worker();
        let old_scope = scope(&worker);
        let ticket = worker
            .coordinator
            .lock()
            .unwrap()
            .configure_runtime_policy(CassPrefetchPolicy::from_config(
                &crate::config::SwarmAdaptiveConfig {
                    enabled: Some(true),
                    prefetch_budget_ms: Some(5),
                    ..Default::default()
                },
            ))
            .unwrap();
        let current = WarmBatchScope {
            ticket,
            ..old_scope.clone()
        };
        let topics = vec![TopicId::new("cargo")];
        worker.stage_warm_batch(old_scope, topics.clone(), Duration::from_millis(6));
        assert!(
            worker
                .coordinator
                .lock()
                .unwrap()
                .metrics_for("ws")
                .is_none()
        );
        assert_eq!(
            worker.stage_warm_batch(current.clone(), topics.clone(), Duration::from_millis(6)),
            Some(CASS_PREFETCH_BUDGET_EXCEEDED_CODE)
        );
        assert_eq!(
            worker
                .coordinator
                .lock()
                .unwrap()
                .pending_warm_topics(&current.agent_scope, "ws"),
            0
        );
        worker.stage_warm_batch(current.clone(), topics, Duration::from_millis(4));
        let coordinator = worker.coordinator.lock().unwrap();
        assert_eq!(
            coordinator.pending_warm_topics(&current.agent_scope, "ws"),
            1
        );
        assert_eq!(coordinator.metrics_for("ws").unwrap().budget_exceeded, 1);
    }
}
