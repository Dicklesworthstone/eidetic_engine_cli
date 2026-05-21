//! Cooperative centrality refresh execution for memory-link graph snapshots.

use std::thread;
use std::time::{Duration, Instant};

use asupersync::Cx;

use crate::graph::algorithms::{
    DEFAULT_BACKGROUND_BUDGET, PprPolicy, check_cancelled, run_pagerank_with_policy,
    run_with_budget,
};
use crate::graph::{
    CentralityRefreshReport, CentralityRefreshStatus, GraphError, GraphResult,
    MemoryGraphProjection, betweenness_centrality_directed, merge_centrality_scores,
    sort_scores_by_metric_desc_then_memory_id,
};

const COOPERATIVE_ALGORITHM_COUNT: u32 = 3;

struct TimedResult<T> {
    elapsed_ms: f64,
    result: T,
}

pub fn refresh_centrality_cooperative(
    cx: &Cx,
    projection: &MemoryGraphProjection,
    total_start: Instant,
    budget: Duration,
) -> GraphResult<CentralityRefreshReport> {
    check_cancelled(cx, "cooperative_centrality")?;

    let sub_budget = cooperative_sub_budget(budget);
    let pagerank_graph = projection.graph.clone();
    let betweenness_graph = projection.graph.clone();
    let hits_graph = projection.graph.clone();
    let pagerank_cx = cx.clone();
    let betweenness_cx = cx.clone();
    let hits_cx = cx.clone();

    let (pagerank, betweenness, hits) = thread::scope(|scope| {
        let pagerank_handle = scope.spawn(move || {
            let started = Instant::now();
            let result = run_with_budget(&pagerank_cx, "pagerank", sub_budget, move || {
                run_pagerank_with_policy(&pagerank_graph, PprPolicy::default())
            })?;
            Ok(TimedResult {
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                result,
            })
        });

        let betweenness_handle = scope.spawn(move || {
            let started = Instant::now();
            let result = run_with_budget(
                &betweenness_cx,
                "betweenness_centrality",
                sub_budget,
                move || betweenness_centrality_directed(&betweenness_graph),
            )?;
            Ok(TimedResult {
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                result,
            })
        });

        let hits_handle = scope.spawn(move || {
            let started = Instant::now();
            let result = crate::graph::hits::compute_hits_with_cx(&hits_cx, &hits_graph)?;
            Ok(TimedResult {
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                result,
            })
        });

        Ok((
            join_algorithm("pagerank", pagerank_handle)?,
            join_algorithm("betweenness_centrality", betweenness_handle)?,
            join_algorithm("hits", hits_handle)?,
        ))
    })?;

    check_cancelled(cx, "cooperative_centrality")?;

    let mut scores = merge_centrality_scores(
        &pagerank.result.scores,
        &betweenness.result.scores,
        &hits.result,
    );
    sort_scores_by_metric_desc_then_memory_id(&mut scores, |score| score.pagerank);

    let mut top_pagerank = scores.clone();
    top_pagerank.truncate(10);

    let mut top_betweenness = scores.clone();
    sort_scores_by_metric_desc_then_memory_id(&mut top_betweenness, |score| score.betweenness);
    top_betweenness.truncate(10);

    let mut top_hubs = scores.clone();
    sort_scores_by_metric_desc_then_memory_id(&mut top_hubs, |score| score.hub);
    top_hubs.truncate(10);

    let mut top_authorities = scores.clone();
    sort_scores_by_metric_desc_then_memory_id(&mut top_authorities, |score| score.authority);
    top_authorities.truncate(10);

    emit_cooperative_centrality_trace(pagerank.elapsed_ms, betweenness.elapsed_ms, hits.elapsed_ms);

    Ok(CentralityRefreshReport {
        version: env!("CARGO_PKG_VERSION"),
        status: CentralityRefreshStatus::Refreshed,
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
    handle: thread::ScopedJoinHandle<'scope, GraphResult<TimedResult<T>>>,
) -> GraphResult<TimedResult<T>> {
    handle.join().map_err(|panic| GraphError::GraphEngine {
        operation: "join cooperative centrality worker",
        source: format!("{algorithm}: {}", panic_payload_to_string(panic)),
    })?
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

fn emit_cooperative_centrality_trace(pagerank_ms: f64, betweenness_ms: f64, hits_ms: f64) {
    let max_subtask_elapsed_ms = pagerank_ms.max(betweenness_ms).max(hits_ms);
    tracing::info!(
        target: "ee::graph",
        workspace_id = "",
        request_id = "",
        bead_id = "bd-dre9v",
        surface = "cooperative_centrality",
        phase = "response",
        elapsed_ms = max_subtask_elapsed_ms.round() as u64,
        degraded_codes = "",
        algorithm_count = COOPERATIVE_ALGORITHM_COUNT,
        max_subtask_elapsed_ms = max_subtask_elapsed_ms.round() as u64,
        supervisor_overhead_ms = 0_u64,
        partial_results_count = 0_u64,
        cancelled_algorithms = 0_u64,
        "cooperative centrality refresh completed"
    );
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
