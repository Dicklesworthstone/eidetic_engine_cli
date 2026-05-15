use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use asupersync::{CancelReason, Cx};
use ee::graph::algorithms::run_with_budget;
use ee::graph::gomory_hu::build_gomory_hu_tree_with_cx;
use ee::graph::ppr::{PersonalizedPageRankPolicy, compute_personalized_pagerank_with_cx};
use ee::graph::{GraphError, GraphResult};
use ee::models::MemoryId;
use fnx_classes::{Graph, digraph::DiGraph};

type TestResult = Result<(), String>;

fn graph_result<T>(result: GraphResult<T>) -> Result<T, GraphError> {
    result
}

fn assert_cancelled(error: GraphError, algorithm: &str) -> TestResult {
    match error {
        GraphError::AlgorithmCancelled {
            algorithm: actual,
            reason,
        } => {
            if actual != algorithm {
                return Err(format!(
                    "expected cancelled algorithm {algorithm}, got {actual}"
                ));
            }
            if reason.is_empty() {
                return Err("cancellation reason should not be empty".to_owned());
            }
            Ok(())
        }
        other => Err(format!("expected AlgorithmCancelled, got {other:?}")),
    }
}

fn assert_prompt_cancellation_bound(
    result: Result<(), GraphError>,
    algorithm: &str,
    elapsed: Duration,
    max_elapsed: Duration,
) -> TestResult {
    let error = match result {
        Ok(()) => {
            return Err("contract probe completed instead of returning cancellation".to_owned());
        }
        Err(error) => error,
    };
    assert_cancelled(error, algorithm)?;
    if elapsed > max_elapsed {
        return Err(format!(
            "{algorithm} cancellation took too long: elapsed={elapsed:?} max={max_elapsed:?}"
        ));
    }
    Ok(())
}

fn cancel_after(cx: Cx, delay: Duration) {
    thread::spawn(move || {
        thread::sleep(delay);
        cx.set_cancel_reason(
            CancelReason::timeout().with_message("graph cancellation contract timeout"),
        );
    });
}

fn non_polling_wrapper(_cx: &Cx, worker_duration: Duration) {
    thread::sleep(worker_duration);
}

#[test]
fn run_with_budget_exits_promptly_after_ppr_budget_cancellation() -> TestResult {
    let cx = Cx::for_testing();
    cancel_after(cx.clone(), Duration::from_millis(50));
    let worker_cx = cx.clone();
    let started = Instant::now();

    let result = graph_result(run_with_budget(
        &cx,
        "personalized_pagerank",
        Duration::from_millis(250),
        move || {
            while !worker_cx.is_cancel_requested() {
                thread::sleep(Duration::from_millis(5));
            }
            thread::sleep(Duration::from_millis(50));
        },
    ));

    let elapsed = started.elapsed();
    assert_prompt_cancellation_bound(
        result,
        "personalized_pagerank",
        elapsed,
        Duration::from_millis(150),
    )
}

#[test]
fn run_with_budget_exits_promptly_after_gomory_hu_budget_cancellation() -> TestResult {
    let cx = Cx::for_testing();
    cancel_after(cx.clone(), Duration::from_millis(500));
    let worker_cx = cx.clone();
    let started = Instant::now();

    let result = graph_result(run_with_budget(
        &cx,
        "gomory_hu_tree",
        Duration::from_secs(2),
        move || {
            while !worker_cx.is_cancel_requested() {
                thread::sleep(Duration::from_millis(5));
            }
            thread::sleep(Duration::from_millis(50));
        },
    ));

    let elapsed = started.elapsed();
    assert_prompt_cancellation_bound(
        result,
        "gomory_hu_tree",
        elapsed,
        Duration::from_millis(600),
    )
}

#[test]
fn non_polling_wrapper_violates_prompt_cancellation_bound() -> TestResult {
    let cx = Cx::for_testing();
    cancel_after(cx.clone(), Duration::from_millis(50));
    let started = Instant::now();

    non_polling_wrapper(&cx, Duration::from_millis(200));
    let elapsed = started.elapsed();
    let contract_result = assert_prompt_cancellation_bound(
        Ok(()),
        "non_polling_fixture",
        elapsed,
        Duration::from_millis(150),
    );

    if contract_result.is_ok() {
        return Err("non-polling wrapper unexpectedly satisfied cancellation contract".to_owned());
    }
    if elapsed < Duration::from_millis(150) {
        return Err(format!(
            "negative control did not run long enough to prove a violation: {elapsed:?}"
        ));
    }
    Ok(())
}

#[test]
fn personalized_pagerank_wrapper_threads_cancelled_cx() -> TestResult {
    let cx = Cx::for_testing();
    cx.set_cancel_reason(
        CancelReason::timeout().with_message("graph cancellation contract pre-cancel"),
    );
    let graph = DiGraph::strict();
    let seeds = HashMap::<MemoryId, f64>::new();

    let error = match graph_result(compute_personalized_pagerank_with_cx(
        &cx,
        &graph,
        &seeds,
        PersonalizedPageRankPolicy::default(),
    )) {
        Ok(value) => panic!("PPR wrapper should observe the pre-cancelled Cx, got {value:?}"),
        Err(error) => error,
    };

    assert_cancelled(error, "personalized_pagerank")
}

#[test]
fn gomory_hu_wrapper_threads_cancelled_cx() -> TestResult {
    let cx = Cx::for_testing();
    cx.set_cancel_reason(
        CancelReason::timeout().with_message("graph cancellation contract pre-cancel"),
    );
    let graph = Graph::strict();

    let error = match graph_result(build_gomory_hu_tree_with_cx(&cx, &graph)) {
        Ok(value) => {
            panic!("Gomory-Hu wrapper should observe the pre-cancelled Cx, got {value:?}")
        }
        Err(error) => error,
    };

    assert_cancelled(error, "gomory_hu_tree")
}
