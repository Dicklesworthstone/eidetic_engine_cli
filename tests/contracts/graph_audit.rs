//! Contract checks for canonical graph audit actions.

use std::collections::BTreeSet;

use ee::core::graph_audit::{
    ALL_GRAPH_AUDIT_ACTIONS, AlgorithmDegradedInputs, ResultCachedInputs, ResultEvictedInputs,
    ResultEvictedReason, SNAPSHOT_TARGET_TYPE, SnapshotArchivedInputs, SnapshotArchivedReason,
    SnapshotRefreshedInputs, WITNESS_TARGET_TYPE, build_algorithm_degraded_payload,
    build_result_cached_payload, build_result_evicted_payload, build_snapshot_archived_payload,
    build_snapshot_refreshed_payload, graph_algorithm_result_audit_target_id,
    insert_graph_audit_payload,
};
use ee::db::{CreateWorkspaceInput, DbConnection};

type TestResult<T = ()> = Result<T, String>;

const WORKSPACE_ID: &str = "wsp_graph_audit_contract_0000000001";

#[test]
fn graph_audit_actions_round_trip_through_audit_log() -> TestResult {
    let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;
    conn.migrate().map_err(|error| error.to_string())?;
    conn.insert_workspace(
        WORKSPACE_ID,
        &CreateWorkspaceInput {
            path: "/workspace/graph-audit-contract".to_owned(),
            name: Some("graph-audit-contract".to_owned()),
        },
    )
    .map_err(|error| error.to_string())?;

    let result_target =
        graph_algorithm_result_audit_target_id("gsnap_contract", "pagerank", "blake3:cafe");
    let payloads = vec![
        build_snapshot_refreshed_payload(SnapshotRefreshedInputs {
            snapshot_id: "gsnap_contract",
            graph_type: "memory_links",
            snapshot_version: 7,
            content_hash: "blake3:contract",
            build_time_ms: 12,
            node_count: 3,
            edge_count: 2,
        }),
        build_snapshot_archived_payload(SnapshotArchivedInputs {
            snapshot_id: "gsnap_old_contract",
            archived_at: "2026-05-21T09:00:00Z",
            reason: SnapshotArchivedReason::NewerSnapshot,
        }),
        build_result_cached_payload(ResultCachedInputs {
            witness_id: result_target.as_str(),
            algorithm: "pagerank",
            params_hash: "blake3:cafe",
            elapsed_ms: 4,
            cache_size_after: 1,
        }),
        build_result_evicted_payload(ResultEvictedInputs {
            witness_id: result_target.as_str(),
            reason: ResultEvictedReason::TtlExpired,
        }),
        build_algorithm_degraded_payload(AlgorithmDegradedInputs {
            algorithm: "pagerank",
            code: "graph_algorithm_timeout",
            severity: "medium",
            repair: "Run the graph operation in background mode or reduce the graph input size.",
            snapshot_version: Some(7),
        }),
    ];

    for payload in payloads {
        insert_graph_audit_payload(&conn, WORKSPACE_ID, "graph", payload)
            .map_err(|error| error.to_string())?;
    }

    let entries = conn
        .list_audit_entries(None, Some(100))
        .map_err(|error| error.to_string())?;
    let graph_actions = entries
        .iter()
        .filter(|entry| entry.action.starts_with("graph."))
        .map(|entry| entry.action.as_str())
        .collect::<BTreeSet<_>>();
    for expected in ALL_GRAPH_AUDIT_ACTIONS {
        if !graph_actions.contains(expected) {
            return Err(format!(
                "graph audit action {expected} missing from audit_log"
            ));
        }
    }

    let snapshot_rows = entries
        .iter()
        .filter(|entry| entry.target_type.as_deref() == Some(SNAPSHOT_TARGET_TYPE))
        .count();
    let result_rows = entries
        .iter()
        .filter(|entry| entry.target_type.as_deref() == Some(WITNESS_TARGET_TYPE))
        .count();
    if snapshot_rows != 2 {
        return Err(format!(
            "expected 2 graph snapshot audit rows, got {snapshot_rows}"
        ));
    }
    if result_rows != 2 {
        return Err(format!(
            "expected 2 result-cache audit rows, got {result_rows}"
        ));
    }
    for entry in entries
        .iter()
        .filter(|entry| entry.action.starts_with("graph."))
    {
        if entry.details.as_deref().is_none_or(str::is_empty) {
            return Err(format!(
                "{} audit details should be populated",
                entry.action
            ));
        }
    }

    conn.close().map_err(|error| error.to_string())
}
