//! One best-effort idle batch per server; no unbounded queue or result cache.

use super::{Arc, AtomicBool, InflightPool, Ordering, PathBuf, thread};
use crate::core::cass_prefetch::{CassPrefetchCandidate, DEFAULT_PREFETCH_BUDGET};
use crate::core::search::warm_prefetch_lexical;

#[derive(Clone, Debug)]
pub(super) struct CassPrefetchWorker {
    running: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    foreground: Arc<InflightPool>,
}

struct RunningBatch(Arc<AtomicBool>);

impl Drop for RunningBatch {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl CassPrefetchWorker {
    pub(super) fn new(shutdown: Arc<AtomicBool>, foreground: Arc<InflightPool>) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            shutdown,
            foreground,
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
                let report = warm_prefetch_lexical(
                    &index_dir,
                    generation,
                    &candidates,
                    DEFAULT_PREFETCH_BUDGET,
                    || worker.interrupted(),
                );
                tracing::debug!(
                    target: "ee::cass_prefetch",
                    completed_queries = report.completed_queries,
                    matching_queries = report.matching_queries,
                    stop = ?report.stop,
                    "finished speculative lexical warming; no evidence-use credit recorded"
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
        CassPrefetchWorker::new(Arc::new(AtomicBool::new(false)), InflightPool::new(4))
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
        worker.submit(PathBuf::from("absent"), 7, Vec::new());
        assert!(!worker.running.load(Ordering::Acquire));
        assert!(worker.foreground.wait_until_idle(Duration::ZERO));
    }
}
