//! Speculative reads of an already-published lexical generation.
//!
//! No database, model initialization, index maintenance, result cache, or
//! admission-policy shortcut is involved. The same read-only lexical handle
//! used by ordinary search is warmed; every later request still performs its
//! own snapshot, scope, eligibility and ranking checks.

use super::{Duration, Instant, Path, open_lexical_searcher};
use crate::core::cass_prefetch::{
    CassPrefetchCandidate, DEFAULT_PREFETCH_TOP_K, MAX_PREFETCH_TOPIC_ID_BYTES,
};
use crate::core::index::IndexGenerationLease;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PrefetchStop {
    #[default]
    Complete,
    Interrupted,
    BudgetExceeded,
    Unavailable,
    StaleGeneration,
}

/// These are warming operations, NOT evidence-use/cache-hit metrics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LexicalPrefetchReport {
    pub completed_queries: usize,
    pub matching_queries: usize,
    pub stop: PrefetchStop,
}

/// Execute at most three queries within one soft time budget. A synchronous
/// backend call cannot be forcibly interrupted; check before/after each call
/// and never start another when foreground work, shutdown or the budget wins.
/// Call only with the path resolved from an already-authorized context request.
pub(crate) fn warm_prefetch_lexical(
    index_dir: &Path,
    expected_generation: u64,
    candidates: &[CassPrefetchCandidate],
    budget: Duration,
    should_stop: impl Fn() -> bool,
) -> LexicalPrefetchReport {
    let started = Instant::now();
    let checkpoint = || {
        if should_stop() {
            Some(PrefetchStop::Interrupted)
        } else if started.elapsed() >= budget {
            Some(PrefetchStop::BudgetExceeded)
        } else {
            None
        }
    };
    if let Some(stop) = checkpoint() {
        return LexicalPrefetchReport {
            stop,
            ..Default::default()
        };
    }
    if candidates.is_empty() {
        return LexicalPrefetchReport::default();
    }
    crate::core::run_cli_with_cx(budget, |cx| async move {
        let mut report = LexicalPrefetchReport::default();
        // Nonblocking generation lease: speculative work never queues behind
        // a publisher. Missing/stale/unsafe indexes are not repaired here.
        let _lease =
            match IndexGenerationLease::try_read_for_prefetch(&cx, index_dir, expected_generation)
                .await
            {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    return LexicalPrefetchReport {
                        stop: PrefetchStop::StaleGeneration,
                        ..report
                    };
                }
                Err(_) => {
                    return LexicalPrefetchReport {
                        stop: checkpoint().unwrap_or(PrefetchStop::Unavailable),
                        ..report
                    };
                }
            };
        if let Some(stop) = checkpoint() {
            return LexicalPrefetchReport { stop, ..report };
        }
        let reader = match open_lexical_searcher(index_dir) {
            Ok(Some(reader)) => reader,
            _ => {
                return LexicalPrefetchReport {
                    stop: PrefetchStop::Unavailable,
                    ..report
                };
            }
        };
        for candidate in candidates.iter().take(DEFAULT_PREFETCH_TOP_K) {
            if let Some(stop) = checkpoint() {
                report.stop = stop;
                break;
            }
            let query = candidate.topic_id.as_str();
            if query.trim().is_empty() || query.len() > MAX_PREFETCH_TOPIC_ID_BYTES {
                continue;
            }
            if cx.checkpoint().is_err() {
                report.stop = PrefetchStop::BudgetExceeded;
                break;
            }
            match reader.search(&cx, query, 16).await {
                Ok(matches) => {
                    report.completed_queries += 1;
                    report.matching_queries += usize::from(!matches.is_empty());
                    // Intentionally drop all documents/metadata. Never grant
                    // cached evidence authority or suppress later validation.
                }
                Err(_) => {
                    report.stop = checkpoint().unwrap_or(PrefetchStop::Unavailable);
                    break;
                }
            }
            if let Some(stop) = checkpoint() {
                report.stop = stop;
                break;
            }
        }
        report
    })
    .unwrap_or(LexicalPrefetchReport {
        stop: PrefetchStop::Unavailable,
        ..Default::default()
    })
}

#[cfg(all(test, feature = "lexical-bm25"))]
#[path = "search_prefetch_tests.rs"]
mod tests;
