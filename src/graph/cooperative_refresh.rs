//! Cooperative centrality refresh execution for memory-link graph snapshots.
//!
//! Performance shape (bd-2wkpg). Two hot paths matter for refresh
//! latency and peak RSS on larger graphs:
//!
//! 1. **Graph fan-out.** Each algorithm's `run_with_budget` inner
//!    closure must be `'static`, so the parent thread cannot borrow
//!    `projection.graph` into the scoped workers. We wrap the graph
//!    in an `Arc<DiGraph>` **once** at entry and hand each worker an
//!    `Arc::clone` — three atomic refcount bumps instead of three full
//!    graph clones. The inner `&*graph` auto-deref keeps every
//!    `fnx_algorithms::*` signature unchanged.
//!
//! 2. **Top-N derivations.** Sorting `scores` four times by four
//!    different metrics used to mean four `scores.clone()` calls
//!    against the full result vector (`O(n)` per clone, `O(n log n)`
//!    per sort). We now sort `scores` once in place by pagerank
//!    (preserving the report's documented `scores`-is-pagerank-sorted
//!    contract), take the small top-10 slice for `top_pagerank`, and
//!    re-use **one** scratch vector for the remaining three metrics.
//!    Total cost: one full clone (the scratch) + four sorts + four
//!    10-element slice copies. When every trailing metric is skipped
//!    (partial-refresh path) the scratch never allocates.
//!
//! Benchmark target: `cargo bench --bench graph_refresh_cooperative`
//! (`benches/graph_refresh_cooperative.rs` exercises scales
//! `[1000, 5000, 25000]`). Functional regression coverage:
//! `cooperative_refresh_preserves_score_order_on_a_moderate_graph`
//! below pins ordering and uniqueness on a 50-node synthetic graph so
//! a future shortcut can't quietly skip the redundant sort pass.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use asupersync::Cx;

use crate::graph::algorithms::{
    DEFAULT_BACKGROUND_BUDGET, PprPolicy, check_cancelled, run_pagerank_with_policy,
    run_with_budget,
};
use crate::graph::{
    CentralityAlgorithmStatus, CentralityRefreshReport, CentralityRefreshStatus, GraphError,
    GraphResult, MemoryCentralityScore, MemoryGraphProjection, betweenness_centrality_directed,
    sort_scores_by_metric_desc_then_memory_id,
};

const COOPERATIVE_ALGORITHM_COUNT: u32 = 3;

struct TimedAlgorithmResult<T> {
    elapsed_ms: f64,
    result: GraphResult<T>,
}

pub fn refresh_centrality_cooperative(
    cx: &Cx,
    projection: &MemoryGraphProjection,
    total_start: Instant,
    budget: Duration,
) -> GraphResult<CentralityRefreshReport> {
    check_cancelled(cx, "cooperative_centrality")?;

    let sub_budget = cooperative_sub_budget(budget);
    refresh_centrality_cooperative_with_budgets(
        cx,
        projection,
        total_start,
        sub_budget,
        sub_budget,
        sub_budget,
    )
}

fn refresh_centrality_cooperative_with_budgets(
    cx: &Cx,
    projection: &MemoryGraphProjection,
    total_start: Instant,
    pagerank_budget: Duration,
    betweenness_budget: Duration,
    hits_budget: Duration,
) -> GraphResult<CentralityRefreshReport> {
    // bd-2wkpg hotspot 1: one DiGraph clone behind an Arc, then cheap
    // refcount bumps for each worker. `run_with_budget` requires
    // `F: FnOnce() -> R + Send + 'static`, so the inner closure must
    // own its graph handle; `Arc<DiGraph>` satisfies that contract
    // without copying the underlying graph buffers.
    let graph_arc = Arc::new(projection.graph.clone());
    let pagerank_graph = Arc::clone(&graph_arc);
    let betweenness_graph = Arc::clone(&graph_arc);
    let hits_graph = Arc::clone(&graph_arc);
    let pagerank_cx = cx.clone();
    let betweenness_cx = cx.clone();
    let hits_cx = cx.clone();

    let (pagerank, betweenness, hits) = thread::scope(|scope| {
        let pagerank_handle = scope.spawn(move || {
            let started = Instant::now();
            let result = run_with_budget(&pagerank_cx, "pagerank", pagerank_budget, move || {
                run_pagerank_with_policy(&pagerank_graph, PprPolicy::default())
            });
            TimedAlgorithmResult {
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                result,
            }
        });

        let betweenness_handle = scope.spawn(move || {
            let started = Instant::now();
            let result = run_with_budget(
                &betweenness_cx,
                "betweenness_centrality",
                betweenness_budget,
                move || betweenness_centrality_directed(&betweenness_graph),
            );
            TimedAlgorithmResult {
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                result,
            }
        });

        let hits_handle = scope.spawn(move || {
            let started = Instant::now();
            let result =
                crate::graph::hits::compute_hits_with_budget(&hits_cx, &hits_graph, hits_budget);
            TimedAlgorithmResult {
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                result,
            }
        });

        (
            join_algorithm("pagerank", pagerank_handle),
            join_algorithm("betweenness_centrality", betweenness_handle),
            join_algorithm("hits", hits_handle),
        )
    });

    check_cancelled(cx, "cooperative_centrality")?;

    let success_count =
        algorithm_success_count(&pagerank.result, &betweenness.result, &hits.result);
    if success_count == 0 {
        return Err(first_algorithm_error(
            pagerank.result,
            betweenness.result,
            hits.result,
        ));
    }
    let pagerank_status = CentralityAlgorithmStatus::from_graph_result(&pagerank.result);
    let betweenness_status = CentralityAlgorithmStatus::from_graph_result(&betweenness.result);
    let hits_status = CentralityAlgorithmStatus::from_graph_result(&hits.result);

    let empty_hits = crate::graph::hits::HitsScores::default();
    let pagerank_scores = pagerank
        .result
        .as_ref()
        .map(|result| result.scores.as_slice())
        .unwrap_or_default();
    let betweenness_scores = betweenness
        .result
        .as_ref()
        .map(|result| result.scores.as_slice())
        .unwrap_or_default();
    let hits_scores = hits.result.as_ref().unwrap_or(&empty_hits);

    let mut scores =
        merge_partial_centrality_scores(pagerank_scores, betweenness_scores, hits_scores);
    sort_scores_by_metric_desc_then_memory_id(&mut scores, |score| score.pagerank);

    // bd-2wkpg hotspot 2: derive every top-N from a single reusable
    // scratch vector. `scores` is sorted in place by pagerank above,
    // so `top_pagerank` is just the (at most) 10-element prefix of
    // `scores` — no allocation beyond the small Vec itself. The three
    // remaining metrics share one scratch clone; resorting in place
    // gives us each ranking without ever cloning the full score list
    // again. When every trailing metric is skipped (e.g. partial
    // refresh where both betweenness and HITS timed out) the scratch
    // is never allocated at all.
    let top_pagerank: Vec<MemoryCentralityScore> =
        if pagerank_status == CentralityAlgorithmStatus::Computed {
            top_n_slice(&scores, 10)
        } else {
            Vec::new()
        };

    let scratch_needed = betweenness_status == CentralityAlgorithmStatus::Computed
        || hits_status == CentralityAlgorithmStatus::Computed;
    let mut scratch: Vec<MemoryCentralityScore> = if scratch_needed {
        scores.clone()
    } else {
        Vec::new()
    };

    let top_betweenness = if betweenness_status == CentralityAlgorithmStatus::Computed {
        sort_scores_by_metric_desc_then_memory_id(&mut scratch, |score| score.betweenness);
        top_n_slice(&scratch, 10)
    } else {
        Vec::new()
    };

    let top_hubs = if hits_status == CentralityAlgorithmStatus::Computed {
        sort_scores_by_metric_desc_then_memory_id(&mut scratch, |score| score.hub);
        top_n_slice(&scratch, 10)
    } else {
        Vec::new()
    };

    let top_authorities = if hits_status == CentralityAlgorithmStatus::Computed {
        sort_scores_by_metric_desc_then_memory_id(&mut scratch, |score| score.authority);
        top_n_slice(&scratch, 10)
    } else {
        Vec::new()
    };
    drop(scratch);

    let failure_count = COOPERATIVE_ALGORITHM_COUNT - success_count;
    let degraded_codes =
        algorithm_degraded_codes(&pagerank.result, &betweenness.result, &hits.result);
    emit_cooperative_centrality_trace(
        pagerank.elapsed_ms,
        betweenness.elapsed_ms,
        hits.elapsed_ms,
        success_count,
        failure_count,
        degraded_codes.as_str(),
    );

    Ok(CentralityRefreshReport {
        version: env!("CARGO_PKG_VERSION"),
        status: CentralityRefreshStatus::Refreshed,
        pagerank_status,
        betweenness_status,
        hits_status,
        dry_run: false,
        node_count: projection.node_count,
        edge_count: projection.edge_count,
        projection_ms: projection.build_ms,
        pagerank_ms: pagerank.elapsed_ms,
        betweenness_ms: betweenness.elapsed_ms,
        hits_ms: hits.elapsed_ms,
        total_ms: total_start.elapsed().as_secs_f64() * 1000.0,
        scores,
        top_pagerank,
        top_betweenness,
        top_hubs,
        top_authorities,
    })
}

/// bd-2wkpg: copy the (already-sorted) leading `cap` rows out of
/// `scores` into a fresh Vec. Replaces the prior `scores.clone() +
/// truncate(10)` pattern, which allocated the full vector before
/// shrinking it. Capped by both `cap` and the live row count so we
/// never address past the end on small graphs (the empty-projection
/// path lands here with `scores.len() == 0`).
fn top_n_slice(scores: &[MemoryCentralityScore], cap: usize) -> Vec<MemoryCentralityScore> {
    let take = scores.len().min(cap);
    scores[..take].to_vec()
}

fn cooperative_sub_budget(budget: Duration) -> Duration {
    if budget.is_zero() {
        return DEFAULT_BACKGROUND_BUDGET;
    }
    let base = budget
        .checked_div(COOPERATIVE_ALGORITHM_COUNT)
        .unwrap_or(DEFAULT_BACKGROUND_BUDGET);
    let slack = budget
        .checked_div(COOPERATIVE_ALGORITHM_COUNT.saturating_mul(10))
        .unwrap_or(Duration::ZERO);
    base.saturating_add(slack).max(Duration::from_millis(1))
}

fn join_algorithm<'scope, T>(
    algorithm: &'static str,
    handle: thread::ScopedJoinHandle<'scope, TimedAlgorithmResult<T>>,
) -> TimedAlgorithmResult<T> {
    handle.join().unwrap_or_else(|panic| TimedAlgorithmResult {
        elapsed_ms: 0.0,
        result: Err(GraphError::GraphEngine {
            operation: "join cooperative centrality worker",
            source: format!("{algorithm}: {}", panic_payload_to_string(panic)),
        }),
    })
}

fn algorithm_success_count<T, U, V>(
    pagerank: &GraphResult<T>,
    betweenness: &GraphResult<U>,
    hits: &GraphResult<V>,
) -> u32 {
    let mut count = 0;
    if pagerank.is_ok() {
        count += 1;
    }
    if betweenness.is_ok() {
        count += 1;
    }
    if hits.is_ok() {
        count += 1;
    }
    count
}

fn first_algorithm_error<T, U, V>(
    pagerank: GraphResult<T>,
    betweenness: GraphResult<U>,
    hits: GraphResult<V>,
) -> GraphError {
    match pagerank {
        Err(error) => return error,
        Ok(_) => {}
    }
    match betweenness {
        Err(error) => return error,
        Ok(_) => {}
    }
    match hits {
        Err(error) => error,
        Ok(_) => GraphError::GraphEngine {
            operation: "cooperative centrality failure aggregation",
            source: "all algorithms succeeded while success_count was zero".to_owned(),
        },
    }
}

fn algorithm_degraded_codes<T, U, V>(
    pagerank: &GraphResult<T>,
    betweenness: &GraphResult<U>,
    hits: &GraphResult<V>,
) -> String {
    let mut codes = BTreeSet::new();
    if let Err(error) = pagerank {
        codes.insert(error.kind_str());
    }
    if let Err(error) = betweenness {
        codes.insert(error.kind_str());
    }
    if let Err(error) = hits {
        codes.insert(error.kind_str());
    }
    codes.into_iter().collect::<Vec<_>>().join(",")
}

fn merge_partial_centrality_scores(
    pagerank_scores: &[fnx_algorithms::CentralityScore],
    betweenness_scores: &[fnx_algorithms::CentralityScore],
    hits: &crate::graph::hits::HitsScores,
) -> Vec<MemoryCentralityScore> {
    let mut nodes = BTreeSet::new();
    let mut pagerank_by_node = BTreeMap::new();
    let mut betweenness_by_node = BTreeMap::new();

    for score in pagerank_scores {
        nodes.insert(score.node.clone());
        pagerank_by_node
            .entry(score.node.clone())
            .or_insert(score.score);
    }
    for score in betweenness_scores {
        nodes.insert(score.node.clone());
        betweenness_by_node
            .entry(score.node.clone())
            .or_insert(score.score);
    }
    for node in hits.hubs.keys().chain(hits.authorities.keys()) {
        nodes.insert(node.clone());
    }

    nodes
        .into_iter()
        .map(|memory_id| MemoryCentralityScore {
            pagerank: pagerank_by_node.get(&memory_id).copied().unwrap_or(0.0),
            betweenness: betweenness_by_node.get(&memory_id).copied().unwrap_or(0.0),
            hub: hits.hubs.get(&memory_id).copied().unwrap_or(0.0),
            authority: hits.authorities.get(&memory_id).copied().unwrap_or(0.0),
            memory_id,
        })
        .collect()
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send + 'static>) -> String {
    let payload = match payload.downcast::<String>() {
        Ok(message) => return *message,
        Err(payload) => payload,
    };
    match payload.downcast::<&'static str>() {
        Ok(message) => (*message).to_owned(),
        Err(_) => "non-string cooperative centrality panic payload".to_owned(),
    }
}

fn emit_cooperative_centrality_trace(
    pagerank_ms: f64,
    betweenness_ms: f64,
    hits_ms: f64,
    success_count: u32,
    failure_count: u32,
    degraded_codes: &str,
) {
    let max_subtask_elapsed_ms = pagerank_ms.max(betweenness_ms).max(hits_ms);
    let max_subtask_elapsed_ms_rounded = trace_millis_to_u64(max_subtask_elapsed_ms);
    tracing::info!(
        target: "ee::graph",
        workspace_id = "",
        request_id = "",
        bead_id = "bd-dre9v",
        surface = "cooperative_centrality",
        phase = "response",
        elapsed_ms = max_subtask_elapsed_ms_rounded,
        degraded_codes = degraded_codes,
        algorithm_count = COOPERATIVE_ALGORITHM_COUNT,
        max_subtask_elapsed_ms = max_subtask_elapsed_ms_rounded,
        supervisor_overhead_ms = 0_u64,
        partial_results_count = if failure_count == 0 { 0 } else { success_count },
        cancelled_algorithms = failure_count,
        "cooperative centrality refresh completed"
    );
}

fn trace_millis_to_u64(value: f64) -> u64 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if !value.is_finite() {
        return u64::MAX;
    }
    let rounded = value.round();
    if rounded >= u64::MAX as f64 {
        u64::MAX
    } else {
        rounded as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    const MEMORY_A: &str = "mem_00000000000000000000000001";
    const MEMORY_B: &str = "mem_00000000000000000000000002";
    const MEMORY_C: &str = "mem_00000000000000000000000003";

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    fn stored_memory_link(id: &str, src: &str, dst: &str) -> crate::db::StoredMemoryLink {
        crate::db::StoredMemoryLink {
            id: id.to_owned(),
            src_memory_id: src.to_owned(),
            dst_memory_id: dst.to_owned(),
            relation: "relates_to".to_owned(),
            weight: 1.0,
            confidence: 1.0,
            directed: true,
            evidence_count: 1,
            last_reinforced_at: None,
            source: "test".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            created_by: None,
            metadata_json: None,
        }
    }

    fn fixture_projection() -> Result<MemoryGraphProjection, String> {
        graph_result(crate::graph::build_memory_graph_from_links(
            &[
                stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa01", MEMORY_A, MEMORY_B),
                stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa02", MEMORY_A, MEMORY_C),
                stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa03", MEMORY_B, MEMORY_C),
            ],
            0,
        ))
    }

    #[test]
    fn trace_millis_to_u64_clamps_edge_values() {
        assert_eq!(trace_millis_to_u64(f64::NAN), 0);
        assert_eq!(trace_millis_to_u64(f64::NEG_INFINITY), 0);
        assert_eq!(trace_millis_to_u64(f64::INFINITY), u64::MAX);
        assert_eq!(trace_millis_to_u64(-1.0), 0);
        assert_eq!(trace_millis_to_u64(1.4), 1);
        assert_eq!(trace_millis_to_u64(1.6), 2);
        assert_eq!(trace_millis_to_u64((u64::MAX as f64) * 2.0), u64::MAX);
    }

    #[test]
    fn cooperative_refresh_handles_empty_snapshot() -> TestResult {
        let projection = graph_result(crate::graph::build_memory_graph_from_links(&[], 0))?;
        let report = graph_result(refresh_centrality_cooperative(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        ))?;

        assert_eq!(report.status, CentralityRefreshStatus::Refreshed);
        assert_eq!(report.node_count, 0);
        assert_eq!(report.edge_count, 0);
        assert!(report.scores.is_empty());
        assert!(report.top_pagerank.is_empty());
        assert!(report.top_authorities.is_empty());
        Ok(())
    }

    #[test]
    fn cooperative_refresh_handles_single_edge_graph() -> TestResult {
        let projection = graph_result(crate::graph::build_memory_graph_from_links(
            &[stored_memory_link(
                "link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa04",
                MEMORY_A,
                MEMORY_B,
            )],
            0,
        ))?;
        let report = graph_result(refresh_centrality_cooperative(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        ))?;

        assert_eq!(report.status, CentralityRefreshStatus::Refreshed);
        assert_eq!(report.node_count, 2);
        assert_eq!(report.edge_count, 1);
        assert_eq!(report.scores.len(), 2);
        assert_eq!(report.top_pagerank.len(), 2);
        assert_eq!(report.top_hubs.len(), 2);
        Ok(())
    }

    #[test]
    fn cooperative_refresh_matches_sequential_scores() -> TestResult {
        let projection = fixture_projection()?;
        let sequential = graph_result(crate::graph::refresh_centrality_from_links(
            &[
                stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa01", MEMORY_A, MEMORY_B),
                stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa02", MEMORY_A, MEMORY_C),
                stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa03", MEMORY_B, MEMORY_C),
            ],
            false,
            &crate::core::graph_memory_budget::MemoryBudgetPolicy::defaults(),
            false,
        ))?;
        let cooperative = graph_result(refresh_centrality_cooperative(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        ))?;

        assert_eq!(cooperative.status, CentralityRefreshStatus::Refreshed);
        assert_eq!(
            score_tuples(&cooperative.scores),
            score_tuples(&sequential.scores)
        );
        assert_eq!(
            memory_ids(&cooperative.top_pagerank),
            memory_ids(&sequential.top_pagerank)
        );
        assert_eq!(
            memory_ids(&cooperative.top_authorities),
            memory_ids(&sequential.top_authorities)
        );
        Ok(())
    }

    #[test]
    fn cooperative_flag_path_matches_direct_cooperative_path() -> TestResult {
        let links = [
            stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa05", MEMORY_A, MEMORY_B),
            stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa06", MEMORY_B, MEMORY_C),
        ];
        let projection = graph_result(crate::graph::build_memory_graph_from_links(&links, 0))?;
        let direct = graph_result(refresh_centrality_cooperative(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        ))?;
        let routed = graph_result(crate::graph::refresh_centrality_from_links(
            &links,
            false,
            &crate::core::graph_memory_budget::MemoryBudgetPolicy::defaults(),
            true,
        ))?;

        assert_eq!(score_tuples(&routed.scores), score_tuples(&direct.scores));
        assert_eq!(memory_ids(&routed.top_hubs), memory_ids(&direct.top_hubs));
        assert_eq!(
            memory_ids(&routed.top_authorities),
            memory_ids(&direct.top_authorities)
        );
        Ok(())
    }

    #[test]
    fn cooperative_refresh_is_deterministic_across_three_runs() -> TestResult {
        let projection = fixture_projection()?;
        let first = graph_result(refresh_centrality_cooperative(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        ))?;
        let second = graph_result(refresh_centrality_cooperative(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        ))?;
        let third = graph_result(refresh_centrality_cooperative(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        ))?;

        assert_eq!(score_tuples(&first.scores), score_tuples(&second.scores));
        assert_eq!(score_tuples(&second.scores), score_tuples(&third.scores));
        Ok(())
    }

    #[test]
    fn cooperative_refresh_preserves_successful_siblings_when_hits_times_out() -> TestResult {
        let links = [
            stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa07", MEMORY_A, MEMORY_B),
            stored_memory_link("link_aaaaaaaaaaaaaaaaaaaaaaaaaaaa08", MEMORY_B, MEMORY_C),
        ];
        let projection = graph_result(crate::graph::build_memory_graph_from_links(&links, 0))?;

        let report = graph_result(refresh_centrality_cooperative_with_budgets(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_millis(1),
        ))?;

        assert_eq!(report.status, CentralityRefreshStatus::Refreshed);
        assert_eq!(
            report.pagerank_status,
            CentralityAlgorithmStatus::Computed,
            "PageRank should be marked computed when it lands in a partial refresh"
        );
        assert_eq!(
            report.betweenness_status,
            CentralityAlgorithmStatus::Computed,
            "betweenness should be marked computed when it lands in a partial refresh"
        );
        assert_eq!(
            report.hits_status,
            CentralityAlgorithmStatus::TimedOut,
            "a 1ms HITS budget must be visible to callers instead of hidden behind zero scores"
        );
        let score_ids = memory_ids(&report.scores);
        assert_eq!(score_ids.len(), 3);
        assert!(score_ids.iter().any(|id| id == MEMORY_A));
        assert!(score_ids.iter().any(|id| id == MEMORY_B));
        assert!(score_ids.iter().any(|id| id == MEMORY_C));
        assert!(
            report.scores.iter().any(|score| score.pagerank > 0.0),
            "PageRank scores should survive a sibling HITS timeout"
        );
        assert!(
            report.scores.iter().any(|score| score.betweenness > 0.0),
            "betweenness scores should survive a sibling HITS timeout"
        );
        assert!(
            report
                .scores
                .iter()
                .all(|score| score.hub == 0.0 && score.authority == 0.0),
            "timed-out HITS should not fabricate hub or authority scores"
        );
        assert_eq!(report.top_pagerank.len(), 3);
        assert_eq!(report.top_betweenness.len(), 3);
        assert!(
            report.top_hubs.is_empty(),
            "timed-out HITS must not emit fake zero-ranked hub leaders"
        );
        assert!(
            report.top_authorities.is_empty(),
            "timed-out HITS must not emit fake zero-ranked authority leaders"
        );
        let data = report.data_json();
        assert_eq!(data["algorithmStatus"]["pagerank"], "computed");
        assert_eq!(data["algorithmStatus"]["betweenness"], "computed");
        assert_eq!(data["algorithmStatus"]["hits"], "timed_out");
        assert_eq!(data["scores"][0]["metricStatus"]["hub"], "timed_out");
        assert_eq!(data["scores"][0]["metricStatus"]["authority"], "timed_out");
        Ok(())
    }

    // bd-2wkpg: a 50-node synthetic graph exercises the
    // single-scratch top-N pipeline on enough rows that the prior
    // clone-per-metric pattern would dominate. The test pins the
    // shape callers rely on (each top-N has exactly 10 rows for a
    // 50-node graph, no duplicates, every top row's metric value is
    // at least the bottom row's) so a future "optimization" that
    // mistakenly skips a sort, reuses a stale scratch state, or
    // truncates the wrong field surface immediately.
    #[test]
    fn cooperative_refresh_preserves_score_order_on_a_moderate_graph() -> TestResult {
        // 50 nodes; chain plus a couple of cross-edges so PageRank /
        // betweenness / HITS each see non-trivial structure.
        let mut links = Vec::new();
        for index in 0..49_u32 {
            let src = format!("mem_{:026x}", index);
            let dst = format!("mem_{:026x}", index + 1);
            links.push(stored_memory_link(
                &format!("link_synth_{:024x}", index),
                &src,
                &dst,
            ));
        }
        // Two long-range edges so betweenness has signal beyond the chain.
        links.push(stored_memory_link(
            "link_synth_long_aaaaaaaaaaaaaa01",
            &format!("mem_{:026x}", 0_u32),
            &format!("mem_{:026x}", 25_u32),
        ));
        links.push(stored_memory_link(
            "link_synth_long_aaaaaaaaaaaaaa02",
            &format!("mem_{:026x}", 10_u32),
            &format!("mem_{:026x}", 49_u32),
        ));

        let projection = graph_result(crate::graph::build_memory_graph_from_links(&links, 0))?;
        let report = graph_result(refresh_centrality_cooperative(
            &Cx::for_testing(),
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        ))?;

        // Cardinality: 50 unique nodes; each top-N caps at 10.
        assert_eq!(report.node_count, 50, "50-node graph round-trip");
        assert_eq!(report.scores.len(), 50, "all rows survive the merge");
        assert_eq!(report.top_pagerank.len(), 10, "top_pagerank capped at 10");
        assert_eq!(
            report.top_betweenness.len(),
            10,
            "top_betweenness capped at 10"
        );
        assert_eq!(report.top_hubs.len(), 10, "top_hubs capped at 10");
        assert_eq!(
            report.top_authorities.len(),
            10,
            "top_authorities capped at 10"
        );

        // Uniqueness within each top-N — guards against a hypothetical
        // future shortcut where scratch sort+truncate ends up handing
        // back duplicates (e.g. mistakenly extending instead of cloning).
        for (label, view) in [
            ("top_pagerank", &report.top_pagerank),
            ("top_betweenness", &report.top_betweenness),
            ("top_hubs", &report.top_hubs),
            ("top_authorities", &report.top_authorities),
        ] {
            let mut ids: Vec<&str> = view.iter().map(|s| s.memory_id.as_str()).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(
                ids.len(),
                view.len(),
                "{label} must contain unique memories"
            );
        }

        // Descending order pin: the FIRST row's metric value must be
        // >= the LAST row's. Catches a missed sort step or a
        // scratch-state contamination across metrics.
        let pagerank_first = report.top_pagerank.first().expect("non-empty").pagerank;
        let pagerank_last = report.top_pagerank.last().expect("non-empty").pagerank;
        assert!(
            pagerank_first >= pagerank_last,
            "top_pagerank must be descending: first={pagerank_first} last={pagerank_last}"
        );
        let betweenness_first = report
            .top_betweenness
            .first()
            .expect("non-empty")
            .betweenness;
        let betweenness_last = report
            .top_betweenness
            .last()
            .expect("non-empty")
            .betweenness;
        assert!(
            betweenness_first >= betweenness_last,
            "top_betweenness descending: first={betweenness_first} last={betweenness_last}"
        );
        let hub_first = report.top_hubs.first().expect("non-empty").hub;
        let hub_last = report.top_hubs.last().expect("non-empty").hub;
        assert!(
            hub_first >= hub_last,
            "top_hubs descending: first={hub_first} last={hub_last}"
        );
        let auth_first = report.top_authorities.first().expect("non-empty").authority;
        let auth_last = report.top_authorities.last().expect("non-empty").authority;
        assert!(
            auth_first >= auth_last,
            "top_authorities descending: first={auth_first} last={auth_last}"
        );

        // Scores is the report's full pagerank-sorted column. The
        // contract is that `scores` stays in pagerank order even after
        // the three trailing top-N derivations resort the scratch.
        let scores_pageranks: Vec<f64> = report.scores.iter().map(|s| s.pagerank).collect();
        let mut sorted_pageranks = scores_pageranks.clone();
        sorted_pageranks
            .sort_by(|left, right| right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(
            scores_pageranks, sorted_pageranks,
            "report.scores must remain pagerank-sorted after top-N derivations \
             (the scratch must not have been aliased over `scores`)"
        );

        Ok(())
    }

    #[test]
    fn cooperative_refresh_honors_pre_cancelled_context() -> TestResult {
        let projection = fixture_projection()?;
        let cx = Cx::for_testing();
        cx.set_cancel_reason(
            asupersync::CancelReason::timeout()
                .with_message("cooperative refresh cancellation test"),
        );

        let error = refresh_centrality_cooperative(
            &cx,
            &projection,
            Instant::now(),
            Duration::from_secs(2),
        )
        .expect_err("pre-cancelled context should stop before spawning workers");

        match error {
            GraphError::AlgorithmCancelled { algorithm, reason } => {
                assert_eq!(algorithm, "cooperative_centrality");
                assert!(reason.contains("cooperative refresh cancellation test"));
            }
            other => return Err(format!("expected cancellation, got {other:?}")),
        }
        Ok(())
    }

    #[test]
    fn cooperative_sub_budget_splits_total_budget_across_algorithms() -> TestResult {
        let sub_budget = cooperative_sub_budget(Duration::from_millis(30));
        assert_eq!(sub_budget, Duration::from_millis(11));
        assert!(sub_budget < DEFAULT_BACKGROUND_BUDGET);
        assert!(sub_budget < Duration::from_millis(30));
        assert_eq!(
            cooperative_sub_budget(Duration::ZERO),
            DEFAULT_BACKGROUND_BUDGET
        );
        Ok(())
    }

    fn score_tuples(
        scores: &[crate::graph::MemoryCentralityScore],
    ) -> Vec<(String, u64, u64, u64, u64)> {
        scores
            .iter()
            .map(|score| {
                (
                    score.memory_id.clone(),
                    score.pagerank.to_bits(),
                    score.betweenness.to_bits(),
                    score.hub.to_bits(),
                    score.authority.to_bits(),
                )
            })
            .collect()
    }

    fn memory_ids(scores: &[crate::graph::MemoryCentralityScore]) -> Vec<String> {
        scores.iter().map(|score| score.memory_id.clone()).collect()
    }
}
