//! Complete topology is useful evidence, not permission to invent PPR scores.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::graph::pack_dna::{
    compute_pack_dna_partial_results, compute_pack_dna_with_cx, current_or_testing_cx,
};
use crate::graph::{AttrMap, DiGraph};
use crate::models::MemoryId;
use fnx_runtime::CgseValue;

fn mem(value: u128) -> MemoryId {
    MemoryId::from_uuid(uuid::Uuid::from_u128(value))
}

fn fixture() -> (MemoryGraphProjection, PackDnaInput) {
    let mut graph = DiGraph::strict();
    for (a, b) in [(1, 2), (2, 3)] {
        let mut attrs = AttrMap::new();
        attrs.insert("weight".to_owned(), CgseValue::Float(1.0));
        attrs.insert("confidence".to_owned(), CgseValue::Float(1.0));
        attrs.insert(
            "relation".to_owned(),
            CgseValue::String("supports".to_owned()),
        );
        graph
            .add_edge_with_attrs(&mem(a).to_string(), &mem(b).to_string(), attrs)
            .unwrap();
    }
    (
        MemoryGraphProjection {
            node_count: graph.node_count(),
            edge_count: graph.edge_count(),
            graph,
            build_ms: 0.0,
            snapshot_version: 41,
        },
        PackDnaInput::new(vec![mem(2), mem(3)], vec![mem(1)], vec![mem(1)]),
    )
}

fn checkpoint(at: Instant) -> CompletedTopology {
    let (projection, input) = fixture();
    let graph = &projection.graph;
    let undirected = undirected_from_directed(graph).unwrap();
    let mut captured = None;
    // Run the real topology helpers and stop at the actual pre-PPR boundary.
    let result = compute_pack_dna_partial_results(
        &current_or_testing_cx(),
        graph,
        &undirected,
        &input,
        |summary, parts| {
            captured = Some(CompletedTopology {
                completed_at: at,
                summary,
                parts: parts.to_vec(),
            });
            Err(timeout(Duration::from_millis(250)))
        },
    );
    assert!(matches!(result, Err(GraphError::AlgorithmTimeout { .. })));
    captured.expect("real topology reached the pre-PPR boundary")
}

fn queued(topology: CompletedTopology) -> Receiver<CompletedTopology> {
    let (sender, receiver) = mpsc::sync_channel(1);
    assert!(sender.send(topology).is_ok());
    receiver
}

fn recovered() -> PackDna {
    let started = Instant::now();
    recover(
        &current_or_testing_cx(),
        Err(timeout(DEFAULT_FOREGROUND_BUDGET)),
        &queued(checkpoint(started)),
        started,
        started + Duration::from_secs(1),
        41,
    )
    .unwrap()
}

#[test]
fn deadline_retains_real_topology_and_original_snapshot_without_ppr_claims() {
    let dna = recovered();
    let (projection, mut input) = fixture();
    input.ppr_neighbor_limit = 0;
    let reference = compute_pack_dna_with_cx(&current_or_testing_cx(), &projection, &input)
        .expect("topology-only public computation");
    assert_eq!(dna.snapshot_version, 41);
    assert_eq!(dna.pack_memory_count, 2);
    assert_eq!(dna.query_seed_count, 1);
    assert_eq!(dna.trust_anchor_count, 1);
    assert_eq!(dna.dominator, reference.dominator);
    assert_eq!(dna.community_of_mass, reference.community_of_mass);
    assert_eq!(dna.ego_subgraph, reference.ego_subgraph);
    assert!(dna.dominator.is_some());
    assert!(dna.community_of_mass.is_some());
    assert!(dna.ego_subgraph.is_some());
    assert!(dna.ppr_neighbors.is_empty());
    assert_eq!(dna.degraded.len(), 1);
    assert_eq!(dna.degraded[0].code, GRAPH_PACK_DNA_TIMEOUT_CODE);
    assert!(
        dna.degraded[0]
            .message
            .contains("unavailable, not evidence of no neighbors")
    );
    assert_eq!(projection.graph.edge_count(), 2);
}

#[test]
fn no_checkpoint_or_late_checkpoint_cannot_fabricate_an_explanation() {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(250);
    let (_sender, empty) = mpsc::sync_channel(1);
    for receiver in [
        empty,
        queued(checkpoint(started - Duration::from_nanos(1))),
        queued(checkpoint(deadline + Duration::from_nanos(1))),
    ] {
        assert!(matches!(
            recover(
                &current_or_testing_cx(),
                Err(timeout(DEFAULT_FOREGROUND_BUDGET)),
                &receiver,
                started,
                deadline,
                41,
            ),
            Err(GraphError::AlgorithmTimeout { .. })
        ));
    }
}

#[test]
fn complete_result_is_not_replaced_by_an_older_checkpoint() {
    let mut complete = recovered();
    complete.degraded.clear();
    complete.ppr_neighbors.push(PackDnaPprNeighbor {
        memory_id: mem(2).to_string(),
        score: 0.625,
    });
    let started = Instant::now();
    let actual = recover(
        &current_or_testing_cx(),
        Ok(complete.clone()),
        &queued(checkpoint(started)),
        started,
        started + DEFAULT_FOREGROUND_BUDGET,
        41,
    )
    .unwrap();
    assert_eq!(actual, complete);
}

#[test]
fn malformed_checkpoint_fields_are_rejected_by_the_existing_strict_merger() {
    let started = Instant::now();
    for variant in 0..3 {
        let mut topology = checkpoint(started);
        match variant {
            0 => {
                topology.parts.pop();
            }
            1 => topology.parts.push(topology.parts[0].clone()),
            _ => topology
                .parts
                .push(PackDnaPartialResult::PprNeighbors(Vec::new())),
        }
        let error = recover(
            &current_or_testing_cx(),
            Err(timeout(DEFAULT_FOREGROUND_BUDGET)),
            &queued(topology),
            started,
            started + Duration::from_secs(1),
            41,
        )
        .expect_err("incomplete or duplicate results cannot be salvaged");
        assert!(matches!(
            error,
            GraphError::GraphEngine {
                operation: "merge pack DNA partial results",
                ..
            }
        ));
    }
}

#[test]
fn outer_engine_errors_and_cancellation_never_become_partial_success() {
    let started = Instant::now();
    for error in [
        GraphError::GraphEngine {
            operation: "worker panic",
            source: "sentinel".to_owned(),
        },
        GraphError::AlgorithmCancelled {
            algorithm: "pack_dna".to_owned(),
            reason: "stop".to_owned(),
        },
        GraphError::InvalidSnapshotMetrics {
            reason: "bad topology".to_owned(),
        },
    ] {
        let expected = error.to_string();
        let actual = recover(
            &current_or_testing_cx(),
            Err(error),
            &queued(checkpoint(started)),
            started,
            started + Duration::from_secs(1),
            41,
        )
        .expect_err("non-timeout outer failure remains an error");
        assert_eq!(actual.to_string(), expected);
    }
}

#[test]
fn only_optional_ppr_availability_errors_are_degraded_without_raw_error_text() {
    for (error, code) in [
        (
            timeout(DEFAULT_FOREGROUND_BUDGET),
            GRAPH_PACK_DNA_TIMEOUT_CODE,
        ),
        (
            GraphError::GraphEngine {
                operation: "PPR",
                source: "PRIVATE_ERROR_SENTINEL".to_owned(),
            },
            PPR_UNAVAILABLE_CODE,
        ),
    ] {
        let (neighbors, warning) = finish_neighbors(Err(error)).unwrap();
        assert!(neighbors.is_empty());
        let warning = warning.unwrap();
        assert_eq!(warning.code, code);
        assert_eq!(warning.severity, "low");
        assert!(!format!("{warning:?}").contains("PRIVATE_ERROR_SENTINEL"));
        assert!(!warning.repair.contains("centrality-refresh"));
    }
    for error in [
        GraphError::AlgorithmCancelled {
            algorithm: "PPR".to_owned(),
            reason: "parent".to_owned(),
        },
        GraphError::InvalidSnapshotMetrics {
            reason: "corrupt".to_owned(),
        },
        GraphError::NumericOverflow {
            field: "node_count",
            value: "overflow".to_owned(),
        },
    ] {
        assert!(finish_neighbors(Err(error)).is_err());
    }
    let expected = vec![PackDnaPprNeighbor {
        memory_id: mem(2).to_string(),
        score: 0.25,
    }];
    assert_eq!(
        finish_neighbors(Ok(expected.clone())).unwrap(),
        (expected, None)
    );
    assert_eq!(
        finish_neighbors(Ok(Vec::new())).unwrap(),
        (Vec::new(), None)
    );
}

#[test]
fn disabled_or_empty_ppr_does_not_publish_a_recovery_checkpoint() {
    for (limit, clear_seeds) in [(0, false), (10, true)] {
        let (projection, mut input) = fixture();
        input.ppr_neighbor_limit = limit;
        if clear_seeds {
            input.query_seed_weights.clear();
        }
        let undirected = undirected_from_directed(&projection.graph).unwrap();
        let mut called = false;
        let (_, parts) = compute_pack_dna_partial_results(
            &current_or_testing_cx(),
            &projection.graph,
            &undirected,
            &input,
            |_, _| {
                called = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(!called);
        assert!(parts.iter().any(|part| matches!(
            part,
            PackDnaPartialResult::PprNeighbors(neighbors) if neighbors.is_empty()
        )));
        assert!(parts.iter().any(|part| matches!(
            part,
            PackDnaPartialResult::Degraded(entries) if entries.is_empty()
        )));
    }
}

#[test]
fn checkpoint_channel_is_bounded_and_a_departed_caller_stops_the_ppr_launch() {
    let topology = checkpoint(Instant::now());
    let (sender, receiver) = mpsc::sync_channel(1);
    let deadline = Instant::now() + Duration::from_secs(1);
    publish(&sender, topology.summary, &topology.parts, deadline).unwrap();
    assert!(publish(&sender, topology.summary, &topology.parts, deadline).is_err());
    let first = receiver.try_recv().unwrap();
    assert_eq!(first.parts, topology.parts);
    drop(receiver);
    assert!(publish(&sender, topology.summary, &topology.parts, deadline).is_err());
}

#[test]
fn partial_json_retains_topology_and_aggregates_its_specific_degradation() {
    let value = serde_json::to_value(recovered()).unwrap();
    assert_eq!(value["snapshotVersion"], 41);
    assert!(value["voronoiDominator"].is_object());
    assert!(value["communityOfMass"].is_object());
    assert!(value["egoSubgraph"].is_object());
    assert_eq!(value["pprNeighbors"], serde_json::json!([]));
    assert_eq!(value["degraded"][0]["code"], GRAPH_PACK_DNA_TIMEOUT_CODE);
    assert_eq!(
        value["degraded"][0]["sources"],
        serde_json::json!(["pack_dna"])
    );
    assert!(value.get("packMemoryCount").is_none());
}

#[test]
fn an_installed_cancelled_request_refuses_both_public_compute_and_recovery() -> Result<(), String> {
    crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        let (projection, input) = fixture();
        let started = Instant::now();
        let receiver = queued(checkpoint(started));
        cx.set_cancel_reason(asupersync::CancelReason::user("stop pack explanation"));
        assert!(matches!(
            compute_pack_dna_with_cx(&cx, &projection, &input),
            Err(GraphError::AlgorithmCancelled { .. })
        ));
        assert!(matches!(
            recover(
                &cx,
                Err(timeout(DEFAULT_FOREGROUND_BUDGET)),
                &receiver,
                started,
                started + Duration::from_secs(1),
                41,
            ),
            Err(GraphError::AlgorithmCancelled { .. })
        ));
        assert!(cx.checkpoint().is_err());
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
