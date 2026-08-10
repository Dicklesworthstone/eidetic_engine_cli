//! Retrieval-affinity projection (ADR 0066 §2 / bd-3a1op.2).
//!
//! Accumulates decayed co-occurrence weights over persisted pack-ledger rows
//! and `search.returned_mem` audit rows through an append-only consumption
//! cursor, and materializes them as a `retrieval_affinity` graph snapshot
//! through the standard snapshot lifecycle.
//!
//! Privacy: accumulation rows and snapshot edges carry memory ids and
//! counters only — never query text, never content.
//!
//! THE HARD RULE (ADR 0066): this projection NEVER enters live search or
//! pack ranking. Retrieval feeding ranking feeding retrieval would break
//! byte-determinism and self-reinforce popular memories into permanent
//! dominance. Structural enforcement: the family is not registered in the
//! retrieval feature-enrichment path, and
//! `retrieval_affinity_is_not_a_search_scoring_input` pins that the search
//! scoring config cannot reference it. Consumers: `ee graph suggest-links`
//! and diagnostics only.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::db::{CreateGraphSnapshotInput, DbConnection, GraphSnapshotType};

/// Degraded code when the affinity snapshot is absent (cold start).
pub const RETRIEVAL_AFFINITY_COLD_CODE: &str = "retrieval_affinity_cold";

/// Schema tag embedded in the snapshot `metrics_json`.
pub const RETRIEVAL_AFFINITY_SCHEMA_V1: &str = "ee.graph.retrieval_affinity.v1";

/// Decay half-life default (`[graph.affinity] half_life_days = 30`).
pub const AFFINITY_HALF_LIFE_DAYS_DEFAULT: f64 = 30.0;

/// Bounded rows consumed per accumulation run (per source).
pub const ACCUMULATION_BATCH_LIMIT: u32 = 512;

/// Result-set size cap when expanding co-occurrence pairs, bounding the
/// per-set pair expansion at `k·(k−1)/2` for `k ≤ 32` (documented candidate
/// bound: total cost is O(consumed rows · k), never O(n²) over the corpus).
pub const RESULT_SET_PAIR_CAP: usize = 32;

/// Weights below this are dropped at materialization.
const EDGE_EPSILON: f64 = 1e-6;

/// Bounded report from one accumulation run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AffinityAccumulationReport {
    pub pack_records_consumed: u64,
    pub search_rows_consumed: u64,
    pub pairs_updated: u64,
    pub pack_cursor: i64,
    pub search_cursor: i64,
    /// True when a full batch was consumed and more rows may remain.
    pub more_pending: bool,
}

/// Outcome of a materialization attempt.
#[derive(Clone, Debug, PartialEq)]
pub enum AffinityMaterialization {
    /// No accumulated evidence yet; surface [`RETRIEVAL_AFFINITY_COLD_CODE`].
    Cold,
    /// Snapshot persisted.
    Persisted {
        snapshot_id: String,
        snapshot_version: u32,
        node_count: u32,
        edge_count: u32,
        content_hash: String,
    },
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// Expand one ranked result set into canonicalized pair deltas:
/// `w(a,b) += 1 / (1 + |rank_a − rank_b|)`.
fn accumulate_pairs(deltas: &mut BTreeMap<(String, String), f64>, ranked: &[(String, u32)]) -> u64 {
    let bounded = &ranked[..ranked.len().min(RESULT_SET_PAIR_CAP)];
    let mut updated = 0;
    for (left_index, (left_id, left_rank)) in bounded.iter().enumerate() {
        for (right_id, right_rank) in bounded.iter().skip(left_index + 1) {
            if left_id == right_id {
                continue;
            }
            let (memory_a, memory_b) = if left_id < right_id {
                (left_id.clone(), right_id.clone())
            } else {
                (right_id.clone(), left_id.clone())
            };
            let rank_gap = f64::from(left_rank.abs_diff(*right_rank));
            *deltas.entry((memory_a, memory_b)).or_insert(0.0) += 1.0 / (1.0 + rank_gap);
            updated += 1;
        }
    }
    updated
}

/// Consume new pack-ledger and search-audit rows from the stored cursor and
/// fold them into the accumulation table. Idempotent: re-running from the
/// same cursor never double-counts because the cursor and the deltas commit
/// through the same connection before the report returns.
///
/// # Errors
///
/// Returns a human-readable string on storage failure.
pub fn accumulate_retrieval_affinity(
    connection: &DbConnection,
    workspace_id: &str,
    now_rfc3339: &str,
) -> Result<AffinityAccumulationReport, String> {
    let (pack_cursor, search_cursor) = connection
        .retrieval_affinity_cursor(workspace_id)
        .map_err(|error| format!("read affinity cursor: {error}"))?;

    let mut deltas: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut latest_event_at = String::new();
    let mut report = AffinityAccumulationReport {
        pack_cursor,
        search_cursor,
        ..AffinityAccumulationReport::default()
    };

    // ── pack ledger: each pack's items are one ranked result set ──────────
    let packs = connection
        .list_pack_records_after(pack_cursor, ACCUMULATION_BATCH_LIMIT)
        .map_err(|error| format!("list pack records: {error}"))?;
    let packs_len = packs.len();
    for (rowid, pack_id, pack_workspace, created_at) in packs {
        report.pack_cursor = rowid;
        if pack_workspace != workspace_id {
            continue;
        }
        let items = connection
            .get_pack_items(&pack_id)
            .map_err(|error| format!("get pack items: {error}"))?;
        let mut ranked: Vec<(String, u32)> = items
            .iter()
            .map(|item| (item.memory_id.clone(), item.rank))
            .collect();
        ranked.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        report.pairs_updated += accumulate_pairs(&mut deltas, &ranked);
        report.pack_records_consumed += 1;
        if created_at > latest_event_at {
            latest_event_at = created_at;
        }
    }

    // ── search audits: contiguous rows sharing a queryHash are one set ────
    let search_rows = connection
        .list_search_returned_mem_after(search_cursor, ACCUMULATION_BATCH_LIMIT)
        .map_err(|error| format!("list search audit rows: {error}"))?;
    let search_len = search_rows.len();
    let mut current_key: Option<String> = None;
    let mut current_set: Vec<(String, u32)> = Vec::new();
    let flush =
        |set: &mut Vec<(String, u32)>, deltas: &mut BTreeMap<(String, String), f64>| -> u64 {
            let mut updated = 0;
            if set.len() > 1 {
                set.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                updated = accumulate_pairs(deltas, set);
            }
            set.clear();
            updated
        };
    for (rowid, row_workspace, memory_id, details, timestamp) in search_rows {
        report.search_cursor = rowid;
        if row_workspace.as_deref() != Some(workspace_id) {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&details) else {
            continue;
        };
        let Some(query_hash) = parsed.get("queryHash").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let rank = parsed
            .get("rank")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(u32::MAX);
        if current_key.as_deref() != Some(query_hash) {
            report.pairs_updated += flush(&mut current_set, &mut deltas);
            current_key = Some(query_hash.to_owned());
        }
        current_set.push((memory_id, rank));
        report.search_rows_consumed += 1;
        if timestamp > latest_event_at {
            latest_event_at = timestamp;
        }
    }
    report.pairs_updated += flush(&mut current_set, &mut deltas);

    if !deltas.is_empty() {
        let event_at = if latest_event_at.is_empty() {
            now_rfc3339.to_owned()
        } else {
            latest_event_at
        };
        let rows: Vec<(String, String, f64)> = deltas
            .into_iter()
            .map(|((memory_a, memory_b), delta)| (memory_a, memory_b, delta))
            .collect();
        connection
            .apply_retrieval_affinity_deltas(workspace_id, &rows, &event_at)
            .map_err(|error| format!("apply affinity deltas: {error}"))?;
    }
    connection
        .write_retrieval_affinity_cursor(
            workspace_id,
            report.pack_cursor,
            report.search_cursor,
            now_rfc3339,
        )
        .map_err(|error| format!("write affinity cursor: {error}"))?;

    report.more_pending = packs_len == ACCUMULATION_BATCH_LIMIT as usize
        || search_len == ACCUMULATION_BATCH_LIMIT as usize;
    Ok(report)
}

/// Materialize the accumulated weights into a `retrieval_affinity` graph
/// snapshot. Deterministic: decay `w · 2^(−Δt / half_life)` is evaluated at
/// `as_of = max(last_event_at)` over the accumulation rows — never the wall
/// clock — so the same ledger prefix always produces the same
/// `content_hash`.
///
/// # Errors
///
/// Returns a human-readable string on storage failure.
pub fn materialize_retrieval_affinity_snapshot(
    connection: &DbConnection,
    workspace_id: &str,
    source_generation: u32,
    half_life_days: f64,
) -> Result<AffinityMaterialization, String> {
    let edges = connection
        .list_retrieval_affinity_edges(workspace_id)
        .map_err(|error| format!("list affinity edges: {error}"))?;
    if edges.is_empty() {
        return Ok(AffinityMaterialization::Cold);
    }

    let as_of = edges
        .iter()
        .map(|(_, _, _, last_event_at)| last_event_at.as_str())
        .max()
        .unwrap_or_default()
        .to_owned();
    let as_of_parsed = parse_rfc3339(&as_of);

    let half_life = if half_life_days > 0.0 {
        half_life_days
    } else {
        AFFINITY_HALF_LIFE_DAYS_DEFAULT
    };
    let mut nodes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut decayed_edges: Vec<serde_json::Value> = Vec::new();
    for (memory_a, memory_b, weight, last_event_at) in &edges {
        let delta_days = match (as_of_parsed, parse_rfc3339(last_event_at)) {
            (Some(as_of_ts), Some(event_ts)) => {
                (as_of_ts - event_ts).num_seconds().max(0) as f64 / 86_400.0
            }
            _ => 0.0,
        };
        let decayed = weight * 2f64.powf(-delta_days / half_life);
        if decayed < EDGE_EPSILON {
            continue;
        }
        nodes.insert(memory_a.clone());
        nodes.insert(memory_b.clone());
        decayed_edges.push(serde_json::json!({
            "a": memory_a,
            "b": memory_b,
            "weight": (decayed * 1_000_000.0).round() / 1_000_000.0,
        }));
    }
    if decayed_edges.is_empty() {
        return Ok(AffinityMaterialization::Cold);
    }

    let metrics = serde_json::json!({
        "schema": RETRIEVAL_AFFINITY_SCHEMA_V1,
        "asOf": as_of,
        "halfLifeDays": half_life,
        "edges": decayed_edges,
    });
    let metrics_json = metrics.to_string();
    let content_hash = format!("blake3:{}", blake3::hash(metrics_json.as_bytes()).to_hex());

    let snapshot_version = connection
        .get_latest_graph_snapshot(workspace_id, GraphSnapshotType::RetrievalAffinity)
        .map_err(|error| format!("read latest affinity snapshot: {error}"))?
        .map_or(1, |snapshot| snapshot.snapshot_version.saturating_add(1));

    let node_count = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
    let edge_count = u32::try_from(decayed_edges.len()).unwrap_or(u32::MAX);
    let id_payload = blake3::hash(
        format!("{workspace_id}\u{0}{snapshot_version}\u{0}{content_hash}").as_bytes(),
    )
    .to_hex()
    .to_string();
    let snapshot_id = format!("gsnap_{}", &id_payload[..25]);

    connection
        .insert_graph_snapshot(
            &snapshot_id,
            &CreateGraphSnapshotInput {
                workspace_id: workspace_id.to_owned(),
                snapshot_version,
                schema_version: RETRIEVAL_AFFINITY_SCHEMA_V1.to_owned(),
                graph_type: GraphSnapshotType::RetrievalAffinity,
                node_count,
                edge_count,
                metrics_json,
                content_hash: content_hash.clone(),
                source_generation,
                expires_at: None,
            },
        )
        .map_err(|error| format!("insert affinity snapshot: {error}"))?;

    Ok(AffinityMaterialization::Persisted {
        snapshot_id,
        snapshot_version,
        node_count,
        edge_count,
        content_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_connection() -> (tempfile::TempDir, DbConnection, String) {
        let temp = tempfile::tempdir().expect("tempdir");
        let database_path = temp.path().join("ee.db");
        let connection = DbConnection::open_file(&database_path).expect("open");
        connection.migrate().expect("migrate");
        let workspace_id = "wsp_affinity_test_0000000000".to_owned();
        connection
            .execute_raw(&format!(
                "INSERT INTO workspaces (id, path, name, created_at, updated_at) VALUES ('{workspace_id}', '/tmp/affinity', 'affinity', '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z')"
            ))
            .expect("workspace row");
        (temp, connection, workspace_id)
    }

    fn seed_search_set(
        connection: &DbConnection,
        workspace_id: &str,
        query_hash: &str,
        hits: &[(&str, u32)],
        timestamp: &str,
    ) {
        for (index, (memory_id, rank)) in hits.iter().enumerate() {
            let audit_id = format!(
                "audit_{query_hash}{index:02}{:0width$}",
                0,
                width = 26 - query_hash.len().min(24) - 2
            );
            let details = serde_json::json!({
                "queryHash": query_hash,
                "rank": rank,
                "score": 0.5,
                "source": "hybrid",
            })
            .to_string();
            connection
                .execute_raw(&format!(
                    "INSERT INTO audit_log (id, workspace_id, timestamp, action, target_type, target_id, details) VALUES ('{}', '{}', '{}', 'search.returned_mem', 'memory', '{}', '{}')",
                    audit_id.chars().take(32).collect::<String>(),
                    workspace_id,
                    timestamp,
                    memory_id,
                    details.replace('\'', "''"),
                ))
                .expect("audit row");
        }
    }

    #[test]
    fn accumulation_is_cursor_idempotent() {
        let (_temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "qh_alpha",
            &[
                ("mem_a0000000000000000000000001", 1),
                ("mem_a0000000000000000000000002", 2),
            ],
            "2026-08-02T00:00:00Z",
        );

        let first =
            accumulate_retrieval_affinity(&connection, &workspace_id, "2026-08-02T01:00:00Z")
                .expect("first run");
        assert_eq!(first.search_rows_consumed, 2);
        assert_eq!(first.pairs_updated, 1);

        let second =
            accumulate_retrieval_affinity(&connection, &workspace_id, "2026-08-02T02:00:00Z")
                .expect("second run");
        assert_eq!(second.search_rows_consumed, 0, "cursor prevents re-reads");

        let edges = connection
            .list_retrieval_affinity_edges(&workspace_id)
            .expect("edges");
        assert_eq!(edges.len(), 1);
        let (_, _, weight, _) = &edges[0];
        assert!(
            (*weight - 0.5).abs() < 1e-9,
            "adjacent ranks accumulate 1/(1+1) exactly once, got {weight}"
        );
    }

    #[test]
    fn rank_gap_weighting_matches_the_adr_formula() {
        let (_temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "qh_beta0",
            &[
                ("mem_b0000000000000000000000001", 1),
                ("mem_b0000000000000000000000002", 4),
            ],
            "2026-08-02T00:00:00Z",
        );
        accumulate_retrieval_affinity(&connection, &workspace_id, "2026-08-02T01:00:00Z")
            .expect("run");
        let edges = connection
            .list_retrieval_affinity_edges(&workspace_id)
            .expect("edges");
        let (_, _, weight, _) = &edges[0];
        assert!(
            (*weight - 0.25).abs() < 1e-9,
            "rank gap 3 -> 1/(1+3), got {weight}"
        );
    }

    #[test]
    fn materialization_is_deterministic_and_cold_when_empty() {
        let (_temp, connection, workspace_id) = seeded_connection();
        assert_eq!(
            materialize_retrieval_affinity_snapshot(&connection, &workspace_id, 1, 30.0)
                .expect("cold"),
            AffinityMaterialization::Cold
        );

        seed_search_set(
            &connection,
            &workspace_id,
            "qh_gamma",
            &[
                ("mem_c0000000000000000000000001", 1),
                ("mem_c0000000000000000000000002", 2),
                ("mem_c0000000000000000000000003", 3),
            ],
            "2026-08-02T00:00:00Z",
        );
        accumulate_retrieval_affinity(&connection, &workspace_id, "2026-08-02T01:00:00Z")
            .expect("run");

        let first = materialize_retrieval_affinity_snapshot(&connection, &workspace_id, 1, 30.0)
            .expect("first materialization");
        let AffinityMaterialization::Persisted {
            content_hash: first_hash,
            node_count,
            edge_count,
            ..
        } = first
        else {
            panic!("expected persisted snapshot");
        };
        assert_eq!(node_count, 3);
        assert_eq!(edge_count, 3);

        let second = materialize_retrieval_affinity_snapshot(&connection, &workspace_id, 1, 30.0)
            .expect("second materialization");
        let AffinityMaterialization::Persisted {
            content_hash: second_hash,
            snapshot_version,
            ..
        } = second
        else {
            panic!("expected persisted snapshot");
        };
        assert_eq!(
            first_hash, second_hash,
            "same ledger prefix -> same content hash (decay anchored to asOf, not wall clock)"
        );
        assert_eq!(snapshot_version, 2, "versions advance monotonically");
    }

    /// THE HARD RULE, structurally pinned: the search scoring configuration
    /// exposes no lever that can reference the retrieval-affinity family, so
    /// the projection cannot leak into live ranking.
    #[test]
    fn retrieval_affinity_is_not_a_search_scoring_input() {
        for key in crate::core::config_surface::graph_config_keys() {
            assert!(
                !key.contains("retrieval_affinity"),
                "search/graph config must not expose a retrieval_affinity lever: {key}"
            );
        }
        // And the fusion-weight surface itself has exactly the three
        // documented components — no affinity slot to smuggle the
        // projection through.
        let source = include_str!("search.rs");
        assert!(
            !source.contains("retrieval_affinity"),
            "core search must not reference the retrieval_affinity projection"
        );
    }
}
