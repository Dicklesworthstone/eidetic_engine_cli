//! Canonical lexical ties before Frankensearch performs rank fusion.
//!
//! Tantivy breaks equal BM25 scores by its physical document address. That is
//! stable within an immutable generation, but insertion order and segment
//! merges can change it after a rebuild or restore. RRF consumes those ranks,
//! so sorting the fused results is too late. Keep the native scores and expand
//! only a tie crossing the requested cutoff before ordering that tie by ID.

use std::borrow::Cow;
use std::sync::Arc;

use frankensearch::core::traits::{LexicalCandidateBatch, LexicalHydrationContext};
use frankensearch::{LexicalRead, ScoredResult, SearchError, SearchFuture, SearchResult};

/// A reader for one leased, immutable lexical generation.
///
/// The caller must hold the same generation lease used for the underlying
/// reader. Each expansion retains the backend's scoring snapshot, and only
/// the final snapshot is handed to hydration. A changed corpus count is a
/// violated generation contract, never evidence that a tie is complete.
pub(crate) struct CanonicalLexicalReader {
    inner: Arc<dyn LexicalRead>,
}

impl CanonicalLexicalReader {
    pub(crate) fn new(inner: Arc<dyn LexicalRead>) -> Self {
        Self { inner }
    }
}

fn checkpoint(cx: &asupersync::Cx) -> SearchResult<()> {
    cx.checkpoint().map_err(|error| SearchError::Cancelled {
        phase: "lexical.canonical_order".to_owned(),
        reason: error.to_string(),
    })
}

fn contract_error(reason: &'static str) -> SearchError {
    SearchError::SubsystemError {
        subsystem: "lexical.canonical_order",
        source: reason.into(),
    }
}

fn candidate_batch(
    results: Vec<ScoredResult>,
    context: Option<LexicalHydrationContext>,
) -> LexicalCandidateBatch {
    match context {
        Some(context) => LexicalCandidateBatch::deferred(results, context),
        None => LexicalCandidateBatch::eager(results),
    }
}

impl LexicalRead for CanonicalLexicalReader {
    fn query_text<'q>(&self, query: &'q str) -> Cow<'q, str> {
        self.inner.query_text(query)
    }

    fn search<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        query: &'a str,
        limit: usize,
    ) -> SearchFuture<'a, Vec<ScoredResult>> {
        Box::pin(async move {
            let (mut results, context) =
                self.search_candidates(cx, query, limit).await?.into_parts();
            self.inner
                .hydrate_candidates(cx, context.as_ref(), &mut results)
                .await?;
            checkpoint(cx)?;
            Ok(results)
        })
    }

    fn search_candidates<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        query: &'a str,
        limit: usize,
    ) -> SearchFuture<'a, LexicalCandidateBatch> {
        Box::pin(async move {
            checkpoint(cx)?;
            if limit == 0 {
                // Preserve a deferred backend's pin even for its empty batch.
                let batch = self.inner.search_candidates(cx, query, 0).await?;
                checkpoint(cx)?;
                if !batch.results().is_empty() {
                    return Err(contract_error(
                        "lexical backend exceeded a zero result limit",
                    ));
                }
                return Ok(batch);
            }

            let document_count = self.inner.doc_count()?;
            let cutoff = limit.min(document_count);
            // One lookahead is enough when no exact-score tie crosses k. All
            // arithmetic stays in the backend's usize domain and saturates
            // before it is bounded by the authoritative generation count.
            let mut requested = limit.saturating_add(1).min(document_count);
            loop {
                checkpoint(cx)?;
                let batch = self.inner.search_candidates(cx, query, requested).await?;
                checkpoint(cx)?;
                if self.inner.doc_count()? != document_count {
                    return Err(contract_error(
                        "lexical generation changed during tie expansion",
                    ));
                }
                let (mut results, context) = batch.into_parts();
                if results.len() > requested {
                    return Err(contract_error(
                        "lexical backend exceeded its requested result limit",
                    ));
                }
                if results.iter().any(|hit| !hit.score.is_finite()) {
                    return Err(contract_error(
                        "lexical backend returned a non-finite score",
                    ));
                }
                if results
                    .windows(2)
                    .any(|pair| pair[0].score.total_cmp(&pair[1].score).is_lt())
                {
                    return Err(contract_error(
                        "lexical backend returned scores out of order",
                    ));
                }

                // Native Tantivy returns the complete top-k, so a short batch
                // exhausts matching documents. Reaching doc_count also proves
                // exhaustion. Otherwise require a strictly lower score after
                // the caller's cutoff; merely sorting a partial tie would keep
                // physical-index order in the selected set.
                let complete = results.len() < requested
                    || requested == document_count
                    || cutoff
                        .checked_sub(1)
                        .and_then(|index| results.get(index))
                        .zip(results.last())
                        .is_some_and(|(boundary, last)| {
                            boundary.score.total_cmp(&last.score).is_gt()
                        });
                if complete {
                    results.sort_by(|left, right| {
                        right
                            .score
                            .total_cmp(&left.score)
                            .then_with(|| left.doc_id.cmp(&right.doc_id))
                    });
                    results.truncate(limit);
                    checkpoint(cx)?;
                    return Ok(candidate_batch(results, context));
                }

                let next = requested.saturating_mul(2).min(document_count);
                if next <= requested {
                    return Err(contract_error("lexical cutoff tie could not be exhausted"));
                }
                // The next request obtains its own complete scored batch and
                // pin. Do not retain or mix candidates from earlier snapshots.
                requested = next;
            }
        })
    }

    fn hydrate_candidates<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        context: Option<&'a LexicalHydrationContext>,
        results: &'a mut [ScoredResult],
    ) -> SearchFuture<'a, ()> {
        self.inner.hydrate_candidates(cx, context, results)
    }

    fn doc_count(&self) -> SearchResult<usize> {
        self.inner.doc_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::search::{IndexableDocument, LexicalWrite, ScoreSource};

    struct FixtureReader {
        rows: Vec<ScoredResult>,
        calls: Mutex<Vec<usize>>,
        count_reads: AtomicUsize,
        document_count: usize,
        change_count: bool,
        cancel_on_call: Option<usize>,
        deferred: bool,
    }

    impl FixtureReader {
        fn new(rows: Vec<ScoredResult>) -> Self {
            Self {
                document_count: rows.len(),
                rows,
                calls: Mutex::new(Vec::new()),
                count_reads: AtomicUsize::new(0),
                change_count: false,
                cancel_on_call: None,
                deferred: true,
            }
        }
    }

    fn hit(id: &str, score: f32) -> ScoredResult {
        ScoredResult {
            doc_id: id.into(),
            score,
            source: ScoreSource::Lexical,
            index: None,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(score),
            rerank_score: None,
            explanation: None,
            metadata: None,
        }
    }

    fn tied_rows(count: usize) -> Vec<ScoredResult> {
        (0..count)
            .rev()
            .map(|index| hit(&format!("doc-{index:03}"), 5.0))
            .collect()
    }

    impl LexicalRead for FixtureReader {
        fn query_text<'q>(&self, query: &'q str) -> Cow<'q, str> {
            Cow::Owned(format!("native:{query}"))
        }

        fn search<'a>(
            &'a self,
            _cx: &'a asupersync::Cx,
            _query: &'a str,
            _limit: usize,
        ) -> SearchFuture<'a, Vec<ScoredResult>> {
            Box::pin(async {
                Err(contract_error(
                    "fixture requires deferred candidate retrieval",
                ))
            })
        }

        fn search_candidates<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            _query: &'a str,
            limit: usize,
        ) -> SearchFuture<'a, LexicalCandidateBatch> {
            Box::pin(async move {
                let call = {
                    let mut calls = self.calls.lock().expect("fixture call log");
                    calls.push(limit);
                    calls.len()
                };
                if self.cancel_on_call == Some(call) {
                    cx.set_cancel_reason(asupersync::CancelReason::user(
                        "stop lexical tie expansion",
                    ));
                }
                let rows = self.rows.iter().take(limit).cloned().collect();
                Ok(if self.deferred {
                    LexicalCandidateBatch::deferred(
                        rows,
                        LexicalHydrationContext::new("lexical-order-fixture", Box::new(call)),
                    )
                } else {
                    LexicalCandidateBatch::eager(rows)
                })
            })
        }

        fn hydrate_candidates<'a>(
            &'a self,
            _cx: &'a asupersync::Cx,
            context: Option<&'a LexicalHydrationContext>,
            results: &'a mut [ScoredResult],
        ) -> SearchFuture<'a, ()> {
            Box::pin(async move {
                if !self.deferred {
                    return if context.is_none() {
                        Ok(())
                    } else {
                        Err(contract_error("eager fixture received a hydration pin"))
                    };
                }
                let call = context
                    .and_then(|pin| pin.downcast_ref::<usize>())
                    .copied()
                    .ok_or_else(|| contract_error("missing fixture snapshot"))?;
                if call != self.calls.lock().expect("fixture call log").len() {
                    return Err(contract_error("hydration received an earlier snapshot"));
                }
                for result in results {
                    result.metadata = Some(Arc::new(serde_json::json!({"snapshot": call})));
                }
                Ok(())
            })
        }

        fn doc_count(&self) -> SearchResult<usize> {
            let reads = self.count_reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.document_count + usize::from(self.change_count && reads > 0))
        }
    }

    fn ids(results: &[ScoredResult]) -> Vec<&str> {
        results.iter().map(|hit| hit.doc_id.as_str()).collect()
    }

    fn run_test<F, Fut>(operation: F) -> Result<(), String>
    where
        F: FnOnce(asupersync::Cx) -> Fut,
        Fut: Future<Output = Result<(), String>>,
    {
        crate::core::run_cli_with_cx(Duration::from_secs(10), operation)
            .map_err(|error| error.to_string())?
    }

    #[test]
    fn cutoff_expansion_stops_after_tie_and_preserves_final_hydration_pin() -> Result<(), String> {
        run_test(|cx| async move {
            let mut rows = tied_rows(11);
            rows.extend((0..100).map(|index| hit(&format!("lower-{index:03}"), 1.0)));
            let inner = Arc::new(FixtureReader::new(rows));
            let reader = CanonicalLexicalReader::new(inner.clone());
            let results = reader
                .search(&cx, "release", 3)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(ids(&results), ["doc-000", "doc-001", "doc-002"]);
            assert_eq!(*inner.calls.lock().expect("fixture call log"), [4, 8, 16]);
            assert!(
                results
                    .iter()
                    .all(|hit| hit.score == 5.0 && hit.lexical_score == Some(5.0))
            );
            assert!(
                results.iter().all(|hit| {
                    hit.metadata.as_ref().expect("hydrated metadata")["snapshot"] == 3
                })
            );
            assert_eq!(reader.query_text("term:literal"), "native:term:literal");
            Ok(())
        })
    }

    #[test]
    fn strict_score_boundary_only_reads_one_lookahead() -> Result<(), String> {
        run_test(|cx| async move {
            let inner = Arc::new(FixtureReader::new(vec![
                hit("z-best", 9.0),
                hit("y-second", 8.0),
                hit("a-lower", 1.0),
                hit("b-lower", 1.0),
            ]));
            let reader = CanonicalLexicalReader::new(inner.clone());
            let results = reader
                .search(&cx, "release", 2)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(ids(&results), ["z-best", "y-second"]);
            assert_eq!(*inner.calls.lock().expect("fixture call log"), [3]);
            assert_eq!(results[0].score, 9.0);
            assert_eq!(results[1].score, 8.0);
            Ok(())
        })
    }

    #[test]
    fn full_corpus_tie_and_unbounded_limit_stop_at_authoritative_count() -> Result<(), String> {
        run_test(|cx| async move {
            let inner = Arc::new(FixtureReader::new(tied_rows(17)));
            let reader = CanonicalLexicalReader::new(inner.clone());
            let batch = reader
                .search_candidates(&cx, "release", 2)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(ids(batch.results()), ["doc-000", "doc-001"]);
            assert_eq!(
                *inner.calls.lock().expect("fixture call log"),
                [3, 6, 12, 17]
            );
            let all = reader
                .search(&cx, "release", usize::MAX)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(all.len(), 17);
            assert_eq!(ids(&all)[0], "doc-000");
            assert_eq!(ids(&all)[16], "doc-016");
            assert_eq!(
                inner.calls.lock().expect("fixture call log").last(),
                Some(&17)
            );
            Ok(())
        })
    }

    #[test]
    fn short_matching_batch_exhausts_tie_without_scanning_unmatched_documents() -> Result<(), String>
    {
        run_test(|cx| async move {
            let mut fixture = FixtureReader::new(tied_rows(7));
            fixture.document_count = 100_000;
            let inner = Arc::new(fixture);
            let reader = CanonicalLexicalReader::new(inner.clone());
            let hits = reader
                .search(&cx, "release", 2)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(ids(&hits), ["doc-000", "doc-001"]);
            assert_eq!(*inner.calls.lock().expect("fixture call log"), [3, 6, 12]);
            Ok(())
        })
    }

    #[test]
    fn eager_batch_keeps_metadata_and_does_not_fabricate_a_snapshot() -> Result<(), String> {
        run_test(|cx| async move {
            let mut rows = tied_rows(5);
            for hit in &mut rows {
                hit.metadata = Some(Arc::new(serde_json::json!({"fixtureId": hit.doc_id})));
            }
            let mut fixture = FixtureReader::new(rows);
            fixture.deferred = false;
            let reader = CanonicalLexicalReader::new(Arc::new(fixture));
            let batch = reader
                .search_candidates(&cx, "release", 2)
                .await
                .map_err(|error| error.to_string())?;
            assert!(!batch.is_deferred());
            assert_eq!(ids(batch.results()), ["doc-000", "doc-001"]);
            let hits = reader
                .search(&cx, "release", 2)
                .await
                .map_err(|error| error.to_string())?;
            for hit in &hits {
                assert_eq!(
                    hit.metadata.as_ref().expect("eager metadata")["fixtureId"],
                    hit.doc_id.as_str()
                );
            }
            Ok(())
        })
    }

    #[test]
    fn zero_limit_and_empty_generation_preserve_empty_backend_batches() -> Result<(), String> {
        run_test(|cx| async move {
            let inner = Arc::new(FixtureReader::new(tied_rows(8)));
            let reader = CanonicalLexicalReader::new(inner.clone());
            let batch = reader
                .search_candidates(&cx, "release", 0)
                .await
                .map_err(|error| error.to_string())?;
            assert!(batch.results().is_empty());
            assert!(batch.is_deferred());
            assert_eq!(*inner.calls.lock().expect("fixture call log"), [0]);
            assert_eq!(inner.count_reads.load(Ordering::SeqCst), 0);
            let empty = Arc::new(FixtureReader::new(Vec::new()));
            assert!(
                CanonicalLexicalReader::new(empty.clone())
                    .search(&cx, "release", 8)
                    .await
                    .map_err(|error| error.to_string())?
                    .is_empty()
            );
            assert_eq!(*empty.calls.lock().expect("fixture call log"), [0]);
            Ok(())
        })
    }

    #[test]
    fn cancellation_stops_cutoff_expansion_without_returning_partial_tie() -> Result<(), String> {
        run_test(|cx| async move {
            let mut fixture = FixtureReader::new(tied_rows(32));
            fixture.cancel_on_call = Some(2);
            let inner = Arc::new(fixture);
            let result = CanonicalLexicalReader::new(inner.clone())
                .search_candidates(&cx, "release", 2)
                .await;
            assert!(matches!(result, Err(SearchError::Cancelled { .. })));
            assert_eq!(*inner.calls.lock().expect("fixture call log"), [3, 6]);
            Ok(())
        })
    }

    #[test]
    fn changed_generation_and_invalid_scores_are_refused() -> Result<(), String> {
        run_test(|cx| async move {
            let mut fixture = FixtureReader::new(tied_rows(8));
            fixture.change_count = true;
            let reader = CanonicalLexicalReader::new(Arc::new(fixture));
            assert!(reader.search_candidates(&cx, "release", 2).await.is_err());
            for rows in [
                vec![hit("nan", f32::NAN)],
                vec![hit("inf", f32::INFINITY)],
                vec![hit("low", 1.0), hit("high", 5.0)],
            ] {
                let reader = CanonicalLexicalReader::new(Arc::new(FixtureReader::new(rows)));
                assert!(reader.search_candidates(&cx, "release", 2).await.is_err());
            }
            Ok(())
        })
    }

    #[test]
    fn native_rebuild_insertion_order_cannot_change_cutoff_selection() -> Result<(), String> {
        run_test(|cx| async move {
            let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
            let documents: Vec<_> = (0..24)
                .map(|index| {
                    let id = format!("evidence-{index:03}");
                    IndexableDocument::new(id.clone(), "cargo release evidence")
                        .with_metadata("fixtureId", id)
                })
                .collect();
            let mut snapshots = Vec::new();
            for reverse in [false, true] {
                let path = scratch
                    .path()
                    .join(if reverse { "restored" } else { "source" });
                let index = crate::search::create_lexical_index(&path)
                    .map_err(|error| error.to_string())?;
                let mut insertion = documents.clone();
                if reverse {
                    insertion.reverse();
                }
                index
                    .index_documents(&cx, &insertion)
                    .await
                    .map_err(|error| error.to_string())?;
                index.commit(&cx).await.map_err(|error| error.to_string())?;
                drop(index);
                let native = crate::search::open_lexical_index_read_only(&path)
                    .map_err(|error| error.to_string())?;
                let all = native
                    .search(&cx, "cargo", 24)
                    .await
                    .map_err(|error| error.to_string())?;
                assert_eq!(all.len(), 24);
                assert!(
                    all.iter()
                        .all(|hit| hit.score.to_bits() == all[0].score.to_bits()),
                    "fixture must cross an exact BM25 tie"
                );
                let reader = CanonicalLexicalReader::new(Arc::new(native));
                let mut runs = Vec::new();
                for limit in [1, 3, 7, 24] {
                    let hits = reader
                        .search(&cx, "cargo", limit)
                        .await
                        .map_err(|error| error.to_string())?;
                    let expected: Vec<_> = (0..limit)
                        .map(|index| format!("evidence-{index:03}"))
                        .collect();
                    assert_eq!(ids(&hits), expected);
                    for hit in &hits {
                        assert_eq!(hit.score.to_bits(), all[0].score.to_bits());
                        assert_eq!(
                            hit.metadata.as_ref().expect("native hydrated metadata")["fixtureId"],
                            hit.doc_id.as_str()
                        );
                    }
                    runs.push(
                        hits.iter()
                            .map(|hit| (hit.doc_id.to_string(), hit.score.to_bits()))
                            .collect::<Vec<_>>(),
                    );
                }
                snapshots.push(runs);
            }
            assert_eq!(snapshots[0], snapshots[1]);
            Ok(())
        })
    }
}
