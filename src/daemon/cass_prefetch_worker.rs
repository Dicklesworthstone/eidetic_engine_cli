//! One best-effort idle batch per server; no unbounded queue or result cache.

use super::{Arc, AtomicBool, InflightPool, Instant, Mutex, Ordering, PathBuf, thread};
use crate::core::cass_prefetch::{
    AgentScope, CassPrefetchCandidate, CassPrefetchCoordinator, DEFAULT_PREFETCH_BUDGET,
    PrefetchGeneration, RecencyWeightedFrequencyPredictor,
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
}

#[derive(Clone, Debug)]
pub(super) struct CassPrefetchWorker {
    running: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    foreground: Arc<InflightPool>,
    coordinator: SharedCoordinator,
}

struct RunningBatch(Arc<AtomicBool>);

impl Drop for RunningBatch {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
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
        Some(RunningBatch(Arc::clone(&self.running)))
    }

    fn interrupted(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
            || self
                .foreground
                .inflight
                .try_lock()
                .map_or(true, |active| *active != 0)
    }

    pub(super) fn submit(
        &self,
        index_dir: PathBuf,
        generation: u64,
        candidates: Vec<CassPrefetchCandidate>,
        scope: WarmBatchScope,
    ) {
        if candidates.is_empty() {
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
                if !worker.foreground.wait_until_idle(DEFAULT_PREFETCH_BUDGET)
                    || worker.interrupted()
                {
                    return;
                }
                let WarmBatchScope {
                    agent_scope,
                    workspace,
                    generation: warmed_against,
                    corpus_revision,
                } = scope;
                // Time the warming call HERE. The coordinator reads no clock
                // (bd-kpynd determinism contract), so the budget verdict is
                // made from an elapsed value this side measured.
                let started = Instant::now();
                let outcome = warm_prefetch_lexical(
                    &index_dir,
                    generation,
                    &candidates,
                    DEFAULT_PREFETCH_BUDGET,
                    || worker.interrupted(),
                );
                let spent = started.elapsed();
                let report = outcome.report;
                let warmed_topics = outcome.warmed.len();
                // bd-16pwc.7: stage the topics that were ACTUALLY warmed so a
                // LATER request can settle them. Still no credit awarded here
                // — a hit is a statement about a later request, which warm
                // time cannot know — but the batch is no longer discarded, so
                // hits, misses and budget_exceeded can finally move.
                let degraded = worker
                    .coordinator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .record_warm_batch(
                        agent_scope,
                        workspace,
                        outcome.warmed,
                        warmed_against,
                        &corpus_revision,
                        spent,
                    );
                tracing::debug!(
                    target: "ee::cass_prefetch",
                    completed_queries = report.completed_queries,
                    matching_queries = report.matching_queries,
                    stop = ?report.stop,
                    warmed_topics,
                    spent_ms = spent.as_millis(),
                    degraded = degraded.unwrap_or("none"),
                    "staged speculative lexical warming for later evidence-use credit"
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
    use std::time::Duration;

    fn worker() -> CassPrefetchWorker {
        CassPrefetchWorker::new(
            Arc::new(AtomicBool::new(false)),
            InflightPool::new(4),
            Arc::new(Mutex::new(CassPrefetchCoordinator::new())),
        )
    }

    fn scope() -> WarmBatchScope {
        WarmBatchScope {
            agent_scope: AgentScope::new("agent:test"),
            workspace: "ws".to_owned(),
            generation: PrefetchGeneration::new(0, 7),
            corpus_revision: CorpusRevision::unknown(),
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
        worker.submit(PathBuf::from("absent"), 7, Vec::new(), scope());
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
}
