//! Recover completed topology if the optional PPR tail exhausts the deadline.
//!
//! One bounded, invocation-local checkpoint is sent before PPR begins. It is
//! never a durable snapshot, never authority to rebuild a graph, and never a
//! reason to turn parent cancellation into success. The existing budget-worker
//! limiter continues to own any soft-cancelled blocking tail until it exits.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use super::{
    Cx, GraphError, GraphResult, MemoryGraphProjection, PackDna, PackDnaDegradation, PackDnaInput,
    PackDnaInputSummary, PackDnaPartialResult, PackDnaPprNeighbor, check_cancelled,
    compute_pack_dna_unbudgeted, merge_pack_dna_partial_results, undirected_from_directed,
};
use crate::graph::algorithms::{DEFAULT_FOREGROUND_BUDGET, run_with_budget};
use crate::models::degradation::GRAPH_PACK_DNA_TIMEOUT_CODE;

const PPR_UNAVAILABLE_CODE: &str = "graph_pack_dna_ppr_unavailable";

struct CompletedTopology {
    completed_at: Instant,
    summary: PackDnaInputSummary,
    parts: Vec<PackDnaPartialResult>,
}

pub(super) fn compute<Caps: 'static>(
    cx: &Cx<Caps>,
    projection: &MemoryGraphProjection,
    input: &PackDnaInput,
) -> GraphResult<PackDna> {
    check_cancelled(cx, "pack_dna")?;
    let started = Instant::now();
    let deadline = started + DEFAULT_FOREGROUND_BUDGET;
    let cx_for_worker = cx.clone();
    let directed = projection.graph.clone();
    let snapshot_version = projection.snapshot_version;
    let input = input.clone();
    // Include cloning in the original foreground allowance. The shared runner
    // treats zero as a default budget, so never dispatch an exhausted request.
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(timeout(DEFAULT_FOREGROUND_BUDGET));
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    let result = run_with_budget(cx, "pack_dna", remaining, move || {
        check_cancelled(&cx_for_worker, "pack_dna")?;
        let undirected = undirected_from_directed(&directed)?;
        compute_pack_dna_unbudgeted(
            &cx_for_worker,
            &directed,
            &undirected,
            &input,
            snapshot_version,
            |summary, parts| publish(&sender, summary, parts, deadline),
        )
    })
    .and_then(|result| result);
    recover(cx, result, &receiver, started, deadline, snapshot_version)
}

fn timeout(budget: Duration) -> GraphError {
    GraphError::AlgorithmTimeout {
        algorithm: "pack_dna".to_owned(),
        timeout_ms: u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
    }
}

fn publish(
    sender: &SyncSender<CompletedTopology>,
    summary: PackDnaInputSummary,
    parts: &[PackDnaPartialResult],
    deadline: Instant,
) -> GraphResult<()> {
    let parts = parts.to_vec();
    let completed_at = Instant::now();
    if completed_at > deadline {
        return Err(timeout(DEFAULT_FOREGROUND_BUDGET));
    }
    // There is exactly one production checkpoint. Do not block a timed-out
    // caller, grow a queue, or start another PPR worker after it has left.
    sender
        .try_send(CompletedTopology {
            completed_at,
            summary,
            parts,
        })
        .map_err(|_| timeout(DEFAULT_FOREGROUND_BUDGET))
}

fn recover<Caps>(
    cx: &Cx<Caps>,
    result: GraphResult<PackDna>,
    receiver: &Receiver<CompletedTopology>,
    started: Instant,
    deadline: Instant,
    snapshot_version: u64,
) -> GraphResult<PackDna> {
    check_cancelled(cx, "pack_dna")?;
    let error = match result {
        Ok(dna) => return Ok(dna),
        // Only a deadline may interrupt the optional tail from outside. An
        // engine panic, storage failure or malformed merge is still an error.
        Err(error @ GraphError::AlgorithmTimeout { .. }) => error,
        Err(error) => return Err(error),
    };
    let Ok(topology) = receiver.try_recv() else {
        return Err(error);
    };
    if topology.completed_at < started || topology.completed_at > deadline {
        return Err(error);
    }
    // This strict merger rejects missing/duplicate topology fields and any
    // purported PPR result in a topology checkpoint. No prefix is fabricated.
    let mut dna = merge_pack_dna_partial_results(
        snapshot_version,
        topology.summary,
        topology
            .parts
            .into_iter()
            .chain([PackDnaPartialResult::PprNeighbors(Vec::new())]),
    )?;
    dna.degraded.push(ppr_degradation(true));
    check_cancelled(cx, "pack_dna")?;
    Ok(dna)
}

/// Handle an error at the PPR call itself, not an error from another phase.
/// Cancellation and data/invariant failures remain errors. An unavailable
/// optional engine does not invalidate topology that already completed.
pub(super) fn finish_neighbors(
    result: GraphResult<Vec<PackDnaPprNeighbor>>,
) -> GraphResult<(Vec<PackDnaPprNeighbor>, Option<PackDnaDegradation>)> {
    match result {
        Ok(neighbors) => Ok((neighbors, None)),
        Err(GraphError::AlgorithmTimeout { .. }) => Ok((Vec::new(), Some(ppr_degradation(true)))),
        Err(GraphError::GraphEngine { .. }) => Ok((Vec::new(), Some(ppr_degradation(false)))),
        Err(error) => Err(error),
    }
}

fn ppr_degradation(timed_out: bool) -> PackDnaDegradation {
    let reason = if timed_out {
        "timed out"
    } else {
        "is unavailable"
    };
    PackDnaDegradation {
        code: if timed_out {
            GRAPH_PACK_DNA_TIMEOUT_CODE
        } else {
            PPR_UNAVAILABLE_CODE
        }
        .to_owned(),
        severity: "low".to_owned(),
        // Do not echo a dependency's raw error payload into an agent's pack.
        message: format!(
            "Pack DNA personalized PageRank {reason}; completed dominator, community and ego-neighborhood results are retained. pprNeighbors is unavailable, not evidence of no neighbors. Ordinary context pack items remain usable."
        ),
        repair: "Retry the explanation or use `--no-pack-dna`; no index or graph rebuild is required for this optional signal.".to_owned(),
    }
}

#[cfg(test)]
#[path = "pack_dna_progress_tests.rs"]
mod tests;
