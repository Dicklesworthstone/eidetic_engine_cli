//! Speculative reads of an already-published lexical generation.
//!
//! No database, model initialization, index maintenance, result cache, or
//! admission-policy shortcut is involved. The same read-only lexical handle
//! used by ordinary search is warmed; every later request still performs its
//! own snapshot, scope, eligibility and ranking checks.

use super::{Duration, Instant, Path, open_lexical_searcher};
use crate::core::cass_prefetch::{
    CassPrefetchCandidate, MAX_PREFETCH_HISTORY, MAX_PREFETCH_TOPIC_ID_BYTES, TopicId,
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

/// The counts plus WHICH topics were actually warmed (bd-16pwc.7).
///
/// `completed_queries` alone cannot be mapped back to topics: the warming
/// loop `continue`s over empty and oversized topic ids, so "the first N
/// candidates" is wrong whenever a candidate is skipped. Evidence-use credit
/// must be awarded against topics whose query actually completed, never
/// against the submitted candidate list, so the warmed ids are reported
/// explicitly.
///
/// [`LexicalPrefetchReport`] stays `Copy` and unchanged; this wraps it so
/// existing report constructions and assertions keep working.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LexicalPrefetchOutcome {
    pub report: LexicalPrefetchReport,
    /// Topic ids whose warming query ran to completion, in warm order. A
    /// topic appears here ONLY if its backend call returned; a topic that
    /// was predicted, submitted, then cut short by the foreground-inflight
    /// interrupt is absent, which is the common case rather than the edge.
    pub warmed: Vec<TopicId>,
}

impl LexicalPrefetchOutcome {
    /// Outcome for a run that stopped before warming anything.
    fn stopped(stop: PrefetchStop) -> Self {
        Self {
            report: LexicalPrefetchReport {
                stop,
                ..Default::default()
            },
            warmed: Vec::new(),
        }
    }
}

/// Execute the configured number of queries within one soft time budget,
/// bounded by [`MAX_PREFETCH_HISTORY`]. A synchronous
/// backend call cannot be forcibly interrupted; check before/after each call
/// and never start another when foreground work, shutdown or the budget wins.
/// Call only with the path resolved from an already-authorized context request.
pub(crate) fn warm_prefetch_lexical(
    index_dir: &Path,
    expected_generation: u64,
    candidates: &[CassPrefetchCandidate],
    top_k: usize,
    budget: Duration,
    should_stop: impl Fn() -> bool,
) -> LexicalPrefetchOutcome {
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
        return LexicalPrefetchOutcome::stopped(stop);
    }
    if candidates.is_empty() || top_k == 0 {
        return LexicalPrefetchOutcome::default();
    }
    crate::core::run_cli_with_cx(budget, |cx| async move {
        let mut outcome = LexicalPrefetchOutcome::default();
        // Nonblocking generation lease: speculative work never queues behind
        // a publisher. Missing/stale/unsafe indexes are not repaired here.
        let _lease =
            match IndexGenerationLease::try_read_for_prefetch(&cx, index_dir, expected_generation)
                .await
            {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    outcome.report.stop = PrefetchStop::StaleGeneration;
                    return outcome;
                }
                Err(_) => {
                    outcome.report.stop = checkpoint().unwrap_or(PrefetchStop::Unavailable);
                    return outcome;
                }
            };
        if let Some(stop) = checkpoint() {
            outcome.report.stop = stop;
            return outcome;
        }
        let reader = match open_lexical_searcher(index_dir) {
            Ok(Some(reader)) => reader,
            _ => {
                outcome.report.stop = PrefetchStop::Unavailable;
                return outcome;
            }
        };
        for candidate in candidates.iter().take(top_k.min(MAX_PREFETCH_HISTORY)) {
            if let Some(stop) = checkpoint() {
                outcome.report.stop = stop;
                break;
            }
            let query = candidate.topic_id.as_str();
            if query.trim().is_empty() || query.len() > MAX_PREFETCH_TOPIC_ID_BYTES {
                continue;
            }
            if cx.checkpoint().is_err() {
                outcome.report.stop = PrefetchStop::BudgetExceeded;
                break;
            }
            match reader.search(&cx, query, 16).await {
                Ok(matches) => {
                    outcome.report.completed_queries += 1;
                    outcome.report.matching_queries += usize::from(!matches.is_empty());
                    // bd-16pwc.7: the query returned, so THIS topic is staged
                    // and may later earn evidence-use credit. Recorded here,
                    // at the only point that knows a candidate was neither
                    // skipped nor interrupted.
                    outcome.warmed.push(candidate.topic_id.clone());
                    // Intentionally drop all documents/metadata. Never grant
                    // cached evidence authority or suppress later validation.
                }
                Err(_) => {
                    outcome.report.stop = checkpoint().unwrap_or(PrefetchStop::Unavailable);
                    break;
                }
            }
            if let Some(stop) = checkpoint() {
                outcome.report.stop = stop;
                break;
            }
        }
        outcome
    })
    .unwrap_or_else(|_| LexicalPrefetchOutcome::stopped(PrefetchStop::Unavailable))
}

#[cfg(all(test, feature = "lexical-bm25"))]
#[path = "search_prefetch_tests.rs"]
mod tests;
