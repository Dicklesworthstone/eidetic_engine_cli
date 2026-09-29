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
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use sqlmodel_core::{Row, Value};

use crate::db::{CreateGraphSnapshotInput, DbConnection, GraphSnapshotType};
use crate::models::MemoryId;

/// Degraded code when the affinity snapshot is absent (cold start).
pub const RETRIEVAL_AFFINITY_COLD_CODE: &str = "retrieval_affinity_cold";

/// Schema tag embedded in the snapshot `metrics_json`.
pub const RETRIEVAL_AFFINITY_SCHEMA_V1: &str = "ee.graph.retrieval_affinity.v1";

/// Decay half-life default (`[graph.affinity] half_life_days = 30`).
pub const AFFINITY_HALF_LIFE_DAYS_DEFAULT: f64 = 30.0;

/// Bounded rows consumed per accumulation run (per source).
pub const ACCUMULATION_BATCH_LIMIT: u32 = 512;

/// Result-set size cap when expanding co-occurrence pairs, bounding the
/// per-set pair expansion at `k·(k−1)/2` for `k ≤ 32`. Search observations
/// use original one-based result ranks, including native non-memory slots;
/// a later storage page cannot reset this cap. Total pair work is O(rows · k),
/// never O(n²) over the corpus.
pub const RESULT_SET_PAIR_CAP: usize = 32;

/// Bound retained audit metadata before allocation or JSON decoding.
const SEARCH_DETAILS_MAX_BYTES: i64 = 16 * 1024;

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

/// Each edge owns its evidence time. An unrelated row, even in the same
/// batch or ranked run, cannot rejuvenate it. Instants are compared in UTC;
/// the spelling or offset of an RFC3339 timestamp is not an ordering key.
#[derive(Clone, Debug, PartialEq)]
struct AffinityDelta {
    weight: f64,
    last_event_at: DateTime<Utc>,
}

type AffinityDeltas = BTreeMap<(String, String), AffinityDelta>;

fn accumulate_pair(
    deltas: &mut AffinityDeltas,
    left: (&str, u32),
    right: (&str, u32),
    event_at: DateTime<Utc>,
) {
    let pair = if left.0 < right.0 {
        (left.0.to_owned(), right.0.to_owned())
    } else {
        (right.0.to_owned(), left.0.to_owned())
    };
    let delta = deltas.entry(pair).or_insert(AffinityDelta {
        weight: 0.0,
        last_event_at: event_at,
    });
    delta.weight += 1.0 / (1.0 + f64::from(left.1.abs_diff(right.1)));
    delta.last_event_at = delta.last_event_at.max(event_at);
}

/// Expand one pack's ranked result set using that pack's own timestamp.
fn accumulate_pairs(
    deltas: &mut AffinityDeltas,
    ranked: &[(String, u32)],
    event_at: DateTime<Utc>,
) -> u64 {
    let bounded = &ranked[..ranked.len().min(RESULT_SET_PAIR_CAP)];
    let mut updated = 0;
    for (left_index, (left_id, left_rank)) in bounded.iter().enumerate() {
        for (right_id, right_rank) in bounded.iter().skip(left_index + 1) {
            if left_id == right_id {
                continue;
            }
            accumulate_pair(
                deltas,
                (left_id, *left_rank),
                (right_id, *right_rank),
                event_at,
            );
            updated += 1;
        }
    }
    updated
}

/// Consume new pack-ledger and search-audit rows from the stored cursor and
/// fold them into the accumulation table. Cursor reads, source reads, all
/// edge increments and both cursor writes share one write transaction. A
/// failed attempt therefore cannot leave counted evidence behind a stale
/// cursor, or acknowledge evidence whose increments were not committed.
///
/// Call through the existing write owner. A competing writer may fail with
/// contention; it must retry the entire operation, including the cursor read.
/// This does not repair overcounting left by older non-atomic accumulators.
///
/// # Errors
///
/// Returns a human-readable string on storage failure.
pub fn accumulate_retrieval_affinity(
    connection: &DbConnection,
    workspace_id: &str,
    now_rfc3339: &str,
) -> Result<AffinityAccumulationReport, String> {
    connection
        .with_transaction(|| accumulate_in_transaction(connection, workspace_id, now_rfc3339))
        .map_err(|_| {
            "Retrieval-affinity accumulation did not complete; inspect the retained pack/audit metadata and cursor, then retry the whole refresh through the write owner."
                .to_owned()
        })
}

/// The transaction owner is the public accumulator above. Never expose a
/// successful report before that owner commits. In particular, a failure
/// writing the cursor must roll back every previously applied edge increment.
fn accumulate_in_transaction(
    connection: &DbConnection,
    workspace_id: &str,
    now_rfc3339: &str,
) -> crate::db::Result<AffinityAccumulationReport> {
    let (pack_cursor, search_cursor) = connection.retrieval_affinity_cursor(workspace_id)?;

    let mut deltas = AffinityDeltas::new();
    let mut report = AffinityAccumulationReport {
        pack_cursor,
        search_cursor,
        ..AffinityAccumulationReport::default()
    };

    // ── pack ledger: each pack's items are one ranked result set ──────────
    let packs = connection.list_pack_records_after(pack_cursor, ACCUMULATION_BATCH_LIMIT)?;
    let packs_len = packs.len();
    for (rowid, pack_id, pack_workspace, created_at) in packs {
        report.pack_cursor = rowid;
        if pack_workspace != workspace_id {
            continue;
        }
        let event_at = parse_rfc3339(&created_at).ok_or_else(search_observation_error)?;
        let items = connection.get_pack_items(&pack_id)?;
        let mut ranked: Vec<(String, u32)> = items
            .iter()
            .map(|item| (item.memory_id.clone(), item.rank))
            .collect();
        ranked.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        report.pairs_updated += accumulate_pairs(&mut deltas, &ranked, event_at);
        report.pack_records_consumed += 1;
    }

    let search = accumulate_search_page(
        connection,
        workspace_id,
        search_cursor,
        ACCUMULATION_BATCH_LIMIT,
        &mut deltas,
    )?;
    report.search_cursor = search.cursor;
    report.search_rows_consumed = search.consumed;
    report.pairs_updated += search.pairs;
    if !deltas.is_empty() {
        let rows = deltas
            .into_iter()
            .map(|((memory_a, memory_b), delta)| {
                (memory_a, memory_b, delta.weight, delta.last_event_at)
            })
            .collect::<Vec<_>>();
        connection.apply_retrieval_affinity_timed_deltas(workspace_id, &rows)?;
    }
    connection.write_retrieval_affinity_cursor(
        workspace_id,
        report.pack_cursor,
        report.search_cursor,
        now_rfc3339,
    )?;

    report.more_pending = packs_len == ACCUMULATION_BATCH_LIMIT as usize
        || search.raw_rows == ACCUMULATION_BATCH_LIMIT as usize;
    Ok(report)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchObservation {
    query_hash: String,
    rank: u32,
}

#[derive(Default)]
struct SearchRun {
    query_hash: Option<String>,
    last_rank: u32,
    members: Vec<(String, u32, DateTime<Utc>)>,
}

struct SearchPage {
    cursor: i64,
    raw_rows: usize,
    consumed: u64,
    pairs: u64,
}

fn search_observation_error() -> crate::db::DbError {
    crate::db::DbError::MalformedRow {
        operation: crate::db::DbOperation::Query,
        message: "Could not validate retained retrieval-affinity observations or cursor".to_owned(),
    }
}

fn search_row_cursor(row: &Row) -> crate::db::Result<i64> {
    row.get(0)
        .and_then(Value::as_i64)
        .filter(|cursor| *cursor > 0)
        .ok_or_else(search_observation_error)
}

/// Read actual audit positions, including non-memory targets. Filtering those
/// out in SQL would lose rank boundaries between mixed native result sets.
/// Neither query loads actors, bodies, nor unbounded details. Only the fixed
/// traversal direction is interpolated; all values are bound parameters.
fn search_observation_rows(
    connection: &DbConnection,
    cursor: i64,
    limit: u32,
    preceding: bool,
) -> crate::db::Result<Vec<Row>> {
    let (comparison, order) = if preceding {
        ("<=", "DESC")
    } else {
        (">", "ASC")
    };
    let sql = format!(
        "SELECT rowid, workspace_id, \
            CASE WHEN length(target_type) <= 64 THEN target_type ELSE NULL END, \
            CASE WHEN length(target_id) <= 128 THEN target_id ELSE NULL END, \
            CASE WHEN length(CAST(details AS BLOB)) <= ?3 THEN details ELSE NULL END, \
            CASE WHEN length(timestamp) <= 128 THEN timestamp ELSE NULL END \
         FROM audit_log WHERE rowid {comparison} ?1 AND action = ?4 \
         ORDER BY rowid {order} LIMIT ?2"
    );
    connection.query(
        &sql,
        &[
            Value::BigInt(cursor),
            Value::BigInt(i64::from(limit)),
            Value::BigInt(SEARCH_DETAILS_MAX_BYTES),
            Value::Text(crate::db::audit_actions::SEARCH_RETURNED_MEM.to_owned()),
        ],
    )
}

impl SearchRun {
    /// Retained producers record ordered, one-based ranks, not a stable request
    /// identity. A hash change or rank restart starts another recorded run.
    /// Foreign or undecodable rows break continuity; they never join local runs.
    /// This does not infer request identity for arbitrarily interleaved writers.
    fn observe(
        &mut self,
        row: &Row,
        workspace_id: &str,
        deltas: Option<&mut AffinityDeltas>,
    ) -> crate::db::Result<(bool, u64)> {
        if row.get(1).and_then(Value::as_str) != Some(workspace_id) {
            *self = Self::default();
            return Ok((false, 0));
        }
        let Some(target_type) = row.get(2).and_then(Value::as_str) else {
            *self = Self::default();
            return Ok((false, 0));
        };
        let memory = target_type == "memory";
        let observation = row
            .get(4)
            .and_then(Value::as_str)
            .and_then(|details| serde_json::from_str::<SearchObservation>(details).ok())
            .filter(|value| {
                value.rank > 0
                    && !value.query_hash.trim().is_empty()
                    && value.query_hash.len() <= 256
            });
        let Some(observation) = observation else {
            if memory {
                return Err(search_observation_error());
            }
            *self = Self::default();
            return Ok((false, 0));
        };
        if self.query_hash.as_ref() != Some(&observation.query_hash)
            || observation.rank <= self.last_rank
        {
            self.members.clear();
            self.query_hash = Some(observation.query_hash);
        }
        self.last_rank = observation.rank;
        // Rules and evidence keep their native identities. Their original
        // ranks delimit the run, but they cannot become memory-only edges.
        if !memory {
            return Ok((false, 0));
        }
        let id = row
            .get(3)
            .and_then(Value::as_str)
            .ok_or_else(search_observation_error)?;
        MemoryId::from_str(id).map_err(|_| search_observation_error())?;
        let timestamp = row
            .get(5)
            .and_then(Value::as_str)
            .ok_or_else(search_observation_error)?;
        let event_at = parse_rfc3339(timestamp).ok_or_else(search_observation_error)?;
        // Cap by ORIGINAL result rank, not by page position or by the count
        // left after native targets were removed. A later page never reopens
        // a saturated run. At most 31 earlier ranks can precede an eligible hit.
        if observation.rank as usize > RESULT_SET_PAIR_CAP {
            return Ok((true, 0));
        }
        if self.members.iter().any(|(earlier, _, _)| earlier == id) {
            return Err(search_observation_error());
        }
        let mut pairs = 0;
        if let Some(deltas) = deltas {
            for (earlier, rank, earlier_at) in &self.members {
                accumulate_pair(
                    deltas,
                    (earlier, *rank),
                    (id, observation.rank),
                    event_at.max(*earlier_at),
                );
                pairs += 1;
            }
        }
        self.members
            .push((id.to_owned(), observation.rank, event_at));
        Ok((true, pairs))
    }
}

/// Rehydrate only the bounded preceding rank context in the SAME transaction,
/// then charge each pair exactly when its right-hand audit position is new.
/// No durable carry table or unbounded scan is needed: ranks above 32 never
/// contribute. Replaying context must never replay its already-counted pairs.
fn accumulate_search_page(
    connection: &DbConnection,
    workspace_id: &str,
    cursor: i64,
    limit: u32,
    deltas: &mut AffinityDeltas,
) -> crate::db::Result<SearchPage> {
    if cursor < 0 {
        return Err(search_observation_error());
    }
    let mut run = SearchRun::default();
    if cursor > 0 {
        let mut prefix =
            search_observation_rows(connection, cursor, RESULT_SET_PAIR_CAP as u32, true)?;
        if prefix.first().map(search_row_cursor).transpose()? != Some(cursor) {
            // An absent/pruned cursor cannot silently authorize a new prefix.
            return Err(search_observation_error());
        }
        prefix.reverse();
        let mut previous = 0;
        for row in prefix {
            let position = search_row_cursor(&row)?;
            if position <= previous || position > cursor {
                return Err(search_observation_error());
            }
            run.observe(&row, workspace_id, None)?;
            previous = position;
        }
    }
    let rows = search_observation_rows(
        connection,
        cursor,
        limit.clamp(1, ACCUMULATION_BATCH_LIMIT),
        false,
    )?;
    let mut page = SearchPage {
        cursor,
        raw_rows: rows.len(),
        consumed: 0,
        pairs: 0,
    };
    for row in rows {
        let position = search_row_cursor(&row)?;
        if position <= page.cursor {
            return Err(search_observation_error());
        }
        let (memory, pairs) = run.observe(&row, workspace_id, Some(deltas))?;
        page.cursor = position;
        page.pairs += pairs;
        if memory {
            page.consumed += 1;
        }
    }
    Ok(page)
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

    const INVALID_STATE: &str =
        "Retrieval-affinity snapshot has invalid time or weight state; inspect the retained projection.";
    let mut as_of_parsed: Option<DateTime<Utc>> = None;
    for (_, _, weight, last_event_at) in &edges {
        let event_at = parse_rfc3339(last_event_at).ok_or_else(|| INVALID_STATE.to_owned())?;
        if !weight.is_finite() || *weight < 0.0 {
            return Err(INVALID_STATE.to_owned());
        }
        as_of_parsed = Some(as_of_parsed.map_or(event_at, |previous| previous.max(event_at)));
    }
    let as_of_parsed = as_of_parsed.ok_or_else(|| INVALID_STATE.to_owned())?;
    let as_of = as_of_parsed.to_rfc3339_opts(SecondsFormat::Nanos, true);

    if !half_life_days.is_finite() {
        return Err(INVALID_STATE.to_owned());
    }
    let half_life = if half_life_days > 0.0 {
        half_life_days
    } else {
        AFFINITY_HALF_LIFE_DAYS_DEFAULT
    };
    let mut nodes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut decayed_edges: Vec<serde_json::Value> = Vec::new();
    for (memory_a, memory_b, weight, last_event_at) in &edges {
        let event_at = parse_rfc3339(last_event_at).ok_or_else(|| INVALID_STATE.to_owned())?;
        let delta_days = (as_of_parsed - event_at)
            .to_std()
            .map_err(|_| INVALID_STATE.to_owned())?
            .as_secs_f64()
            / 86_400.0;
        let decayed = weight * 2f64.powf(-delta_days / half_life);
        if decayed < EDGE_EPSILON {
            continue;
        }
        let rounded = (decayed * 1_000_000.0).round() / 1_000_000.0;
        if !rounded.is_finite() {
            return Err(INVALID_STATE.to_owned());
        }
        nodes.insert(memory_a.clone());
        nodes.insert(memory_b.clone());
        decayed_edges.push(serde_json::json!({
            "a": memory_a,
            "b": memory_b,
            "weight": rounded,
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
    #![allow(clippy::unwrap_used)]

    use super::*;

    include!("retrieval_affinity_time_tests.rs");

    fn stream_id(number: u128) -> String {
        MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string()
    }

    fn stream_record(
        connection: &DbConnection,
        workspace: &str,
        target_type: &str,
        id: &str,
        details: String,
    ) -> crate::db::Result<()> {
        connection.insert_audit(
            &crate::db::generate_audit_id(),
            &crate::db::CreateAuditInput {
                workspace_id: Some(workspace.to_owned()),
                actor: None,
                action: crate::db::audit_actions::SEARCH_RETURNED_MEM.to_owned(),
                target_type: Some(target_type.to_owned()),
                target_id: Some(id.to_owned()),
                details: Some(details),
            },
        )?;
        Ok(())
    }

    fn stream_hit(
        connection: &DbConnection,
        workspace: &str,
        target_type: &str,
        id: &str,
        hash: &str,
        rank: u32,
    ) -> crate::db::Result<()> {
        stream_record(
            connection,
            workspace,
            target_type,
            id,
            serde_json::json!({"queryHash": hash, "rank": rank}).to_string(),
        )
    }

    #[test]
    fn pair_crossing_the_production_page_boundary_is_counted_once() {
        let (_temp, connection, workspace) = seeded_connection();
        let first = stream_id(1);
        let second = stream_id(2);
        connection
            .with_transaction(|| {
                for index in 0..ACCUMULATION_BATCH_LIMIT - 1 {
                    stream_hit(
                        &connection,
                        &workspace,
                        "memory",
                        &first,
                        &format!("singleton-{index}"),
                        1,
                    )?;
                }
                stream_hit(&connection, &workspace, "memory", &first, "split", 1)?;
                stream_hit(&connection, &workspace, "memory", &second, "split", 2)
            })
            .expect("retained source rows");
        let left =
            accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("first page");
        assert_eq!((left.search_rows_consumed, left.pairs_updated), (512, 0));
        assert!(left.more_pending);
        let right = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
            .expect("continuation");
        assert_eq!((right.search_rows_consumed, right.pairs_updated), (1, 1));
        assert!(!right.more_pending);
        let edges = connection
            .list_retrieval_affinity_edges(&workspace)
            .expect("complete pair");
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (&edges[0].0, &edges[0].1, edges[0].2),
            (&first, &second, 0.5)
        );
        let replay = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
            .expect("no duplicate replay");
        assert_eq!((replay.search_rows_consumed, replay.pairs_updated), (0, 0));
        assert_eq!(
            connection
                .list_retrieval_affinity_edges(&workspace)
                .unwrap(),
            edges
        );
    }

    #[test]
    fn native_targets_keep_their_ranks_without_becoming_memory_edges() {
        let (_temp, connection, workspace) = seeded_connection();
        let first = stream_id(1);
        let second = stream_id(2);
        connection
            .with_transaction(|| {
                stream_hit(&connection, &workspace, "rule", "rule-native", "mixed", 1)?;
                stream_hit(&connection, &workspace, "memory", &first, "mixed", 2)?;
                stream_hit(
                    &connection,
                    &workspace,
                    "evidence_span",
                    "ev-native",
                    "mixed",
                    3,
                )?;
                stream_hit(&connection, &workspace, "memory", &second, "mixed", 4)?;
                // A misleading memory-shaped ID cannot override the native type.
                stream_hit(&connection, &workspace, "rule", &stream_id(3), "mixed", 5)
            })
            .expect("mixed native observations");
        let report = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
            .expect("native-safe refresh");
        assert_eq!((report.search_rows_consumed, report.pairs_updated), (2, 1));
        let edges = connection
            .list_retrieval_affinity_edges(&workspace)
            .unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!((&edges[0].0, &edges[0].1), (&first, &second));
        assert!(
            (edges[0].2 - 1.0 / 3.0).abs() < 1e-9,
            "do not renumber admitted memories"
        );
        assert!(report.search_cursor > 0);
        assert_eq!(
            accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
                .unwrap()
                .search_rows_consumed,
            0
        );
    }

    #[test]
    fn repeated_query_hashes_restart_at_the_recorded_rank_boundary() {
        let (_temp, connection, workspace) = seeded_connection();
        let ids = (1..=4).map(stream_id).collect::<Vec<_>>();
        connection
            .with_transaction(|| {
                for (index, id) in ids.iter().enumerate() {
                    stream_hit(
                        &connection,
                        &workspace,
                        "memory",
                        id,
                        "same-query",
                        (index % 2 + 1) as u32,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let report = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).unwrap();
        assert_eq!((report.search_rows_consumed, report.pairs_updated), (4, 2));
        let edges = connection
            .list_retrieval_affinity_edges(&workspace)
            .unwrap();
        assert_eq!(
            edges.len(),
            2,
            "different executions must not invent cross-result edges"
        );
        assert_eq!(
            (&edges[0].0, &edges[0].1, edges[0].2),
            (&ids[0], &ids[1], 0.5)
        );
        assert_eq!(
            (&edges[1].0, &edges[1].1, edges[1].2),
            (&ids[2], &ids[3], 0.5)
        );
    }

    #[test]
    fn later_pages_do_not_reset_the_original_result_rank_cap() {
        let (_temp, connection, workspace) = seeded_connection();
        connection
            .with_transaction(|| {
                for rank in 1..=ACCUMULATION_BATCH_LIMIT + 3 {
                    stream_hit(
                        &connection,
                        &workspace,
                        "memory",
                        &stream_id(u128::from(rank)),
                        "large-run",
                        rank,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let first = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).unwrap();
        assert_eq!(first.pairs_updated, 32 * 31 / 2);
        let edges = connection
            .list_retrieval_affinity_edges(&workspace)
            .unwrap();
        assert_eq!(edges.len(), 32 * 31 / 2);
        let second = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).unwrap();
        assert_eq!((second.search_rows_consumed, second.pairs_updated), (3, 0));
        assert_eq!(
            connection
                .list_retrieval_affinity_edges(&workspace)
                .unwrap(),
            edges
        );
    }

    #[test]
    fn every_small_page_size_replays_the_same_retained_pair_weights() {
        let (_temp, connection, workspace) = seeded_connection();
        connection
            .with_transaction(|| {
                for ordinal in 0..75 {
                    let rank = ordinal % 11 + 1;
                    let target = if rank == 3 { "evidence_span" } else { "memory" };
                    stream_hit(
                        &connection,
                        &workspace,
                        target,
                        &stream_id(100 + ordinal as u128),
                        "repeated-query",
                        rank,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let mut expected = BTreeMap::new();
        let whole = accumulate_search_page(&connection, &workspace, 0, 512, &mut expected).unwrap();
        assert!(!expected.is_empty());
        for limit in [1, 2, 7, 31, 32, 33] {
            let mut actual = BTreeMap::new();
            let mut cursor = 0;
            let mut consumed = 0;
            let mut pairs = 0;
            loop {
                let page =
                    accumulate_search_page(&connection, &workspace, cursor, limit, &mut actual)
                        .unwrap();
                cursor = page.cursor;
                consumed += page.consumed;
                pairs += page.pairs;
                if page.raw_rows < limit as usize {
                    break;
                }
            }
            assert_eq!(
                actual, expected,
                "page size {limit} changed the pair weights"
            );
            assert_eq!(
                (cursor, consumed, pairs),
                (whole.cursor, whole.consumed, whole.pairs)
            );
        }
        assert!(
            connection
                .list_retrieval_affinity_edges(&workspace)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            connection.retrieval_affinity_cursor(&workspace).unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn foreign_observations_break_continuity_without_exposing_their_metadata() {
        let (_temp, connection, workspace) = seeded_connection();
        let foreign = "wsp_00000000000000000000000902";
        connection
            .insert_workspace(
                foreign,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/foreign-affinity".to_owned(),
                    name: None,
                },
            )
            .unwrap();
        stream_hit(&connection, &workspace, "memory", &stream_id(1), "same", 1).unwrap();
        stream_record(
            &connection,
            foreign,
            "memory",
            &stream_id(3),
            r#"{"queryHash":"private-foreign-canary","rank":"invalid"}"#.to_owned(),
        )
        .unwrap();
        stream_hit(&connection, &workspace, "memory", &stream_id(2), "same", 3).unwrap();
        let report = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).unwrap();
        assert_eq!((report.search_rows_consumed, report.pairs_updated), (2, 0));
        assert!(
            connection
                .list_retrieval_affinity_edges(&workspace)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_owned_rank_and_oversized_details_hold_the_entire_prefix() {
        for details in [
            r#"{"queryHash":"private-rank-canary","rank":0}"#.to_owned(),
            serde_json::json!({"queryHash": "same", "rank": 3, "private": "x".repeat(SEARCH_DETAILS_MAX_BYTES as usize)}).to_string(),
        ] {
            let (_temp, connection, workspace) = seeded_connection();
            stream_hit(&connection, &workspace, "memory", &stream_id(1), "same", 1).unwrap();
            stream_hit(&connection, &workspace, "memory", &stream_id(2), "same", 2).unwrap();
            stream_record(&connection, &workspace, "memory", &stream_id(3), details).unwrap();
            let error = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect_err("do not acknowledge ambiguous source metadata");
            assert!(!error.contains("private-rank-canary"));
            assert!(connection.list_retrieval_affinity_edges(&workspace).unwrap().is_empty());
            assert_eq!(connection.retrieval_affinity_cursor(&workspace).unwrap(), (0, 0));
        }
    }

    #[test]
    fn repeated_identity_in_one_increasing_rank_run_is_not_double_counted() {
        let (_temp, connection, workspace) = seeded_connection();
        for (number, rank) in [(1, 1), (2, 2), (1, 3)] {
            stream_hit(
                &connection,
                &workspace,
                "memory",
                &stream_id(number),
                "same",
                rank,
            )
            .unwrap();
        }
        assert!(accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).is_err());
        assert!(
            connection
                .list_retrieval_affinity_edges(&workspace)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            connection.retrieval_affinity_cursor(&workspace).unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn an_unverifiable_retained_cursor_is_not_silently_advanced() {
        let (_temp, connection, workspace) = seeded_connection();
        stream_hit(&connection, &workspace, "memory", &stream_id(1), "same", 1).unwrap();
        connection
            .write_retrieval_affinity_cursor(&workspace, 0, 1234, ATOMIC_NOW)
            .unwrap();
        assert!(accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).is_err());
        assert_eq!(
            connection.retrieval_affinity_cursor(&workspace).unwrap(),
            (0, 1234)
        );
        assert!(
            connection
                .list_retrieval_affinity_edges(&workspace)
                .unwrap()
                .is_empty()
        );
    }

    const ATOMIC_NOW: &str = "2026-08-03T00:00:00Z";
    const ATOMIC_HITS: [(&str, u32); 3] = [
        ("mem_00000000000000000000000001", 1),
        ("mem_00000000000000000000000002", 2),
        ("mem_00000000000000000000000003", 3),
    ];

    #[test]
    fn failed_cursor_insert_rolls_back_edges_and_reopen_retries_once() {
        let (temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "atomic_a",
            &ATOMIC_HITS[..2],
            ATOMIC_NOW,
        );
        connection
            .execute_raw(
                "CREATE TRIGGER affinity_fail_cursor BEFORE INSERT ON retrieval_affinity_cursor BEGIN SELECT RAISE(ABORT, 'private-cursor-fixture'); END;",
            )
            .expect("inject cursor failure");
        let error = accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW)
            .expect_err("cursor failure must not acknowledge the batch");
        assert!(!error.contains("private-cursor-fixture"));
        assert!(
            connection
                .list_retrieval_affinity_edges(&workspace_id)
                .expect("edges")
                .is_empty()
        );
        assert_eq!(
            connection
                .retrieval_affinity_cursor(&workspace_id)
                .expect("cursor"),
            (0, 0)
        );
        connection.close().expect("close after failed transaction");

        let connection = DbConnection::open_file(&temp.path().join("ee.db")).expect("reopen");
        assert!(
            connection
                .list_retrieval_affinity_edges(&workspace_id)
                .expect("durable edges")
                .is_empty()
        );
        assert_eq!(
            connection
                .retrieval_affinity_cursor(&workspace_id)
                .expect("durable cursor"),
            (0, 0)
        );
        connection
            .execute_raw("DROP TRIGGER affinity_fail_cursor")
            .expect("remove fixture fault");
        let first =
            accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW).expect("retry");
        assert_eq!((first.search_rows_consumed, first.pairs_updated), (2, 1));
        let edges = connection
            .list_retrieval_affinity_edges(&workspace_id)
            .expect("committed edges");
        assert_eq!(edges.len(), 1);
        assert!((edges[0].2 - 0.5).abs() < 1e-9);
        let second =
            accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW).expect("repeat");
        assert_eq!((second.search_rows_consumed, second.pairs_updated), (0, 0));
        assert_eq!(
            connection
                .list_retrieval_affinity_edges(&workspace_id)
                .expect("unchanged edges"),
            edges
        );
    }

    #[test]
    fn failed_cursor_update_preserves_the_entire_previously_committed_prefix() {
        let (_temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "atomic_b",
            &ATOMIC_HITS[..2],
            ATOMIC_NOW,
        );
        accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW)
            .expect("first prefix");
        let edges = connection
            .list_retrieval_affinity_edges(&workspace_id)
            .expect("first edges");
        let cursor = connection
            .retrieval_affinity_cursor(&workspace_id)
            .expect("first cursor");
        seed_search_set(
            &connection,
            &workspace_id,
            "atomic_c",
            &ATOMIC_HITS,
            "2026-08-04T00:00:00Z",
        );
        connection
            .execute_raw(
                "CREATE TRIGGER affinity_fail_cursor BEFORE UPDATE ON retrieval_affinity_cursor BEGIN SELECT RAISE(ABORT, 'private-update-fixture'); END;",
            )
            .expect("inject existing-cursor failure");
        assert!(accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW).is_err());
        assert_eq!(
            connection
                .list_retrieval_affinity_edges(&workspace_id)
                .expect("rolled back edges"),
            edges
        );
        assert_eq!(
            connection
                .retrieval_affinity_cursor(&workspace_id)
                .expect("rolled back cursor"),
            cursor
        );
        connection
            .execute_raw("DROP TRIGGER affinity_fail_cursor")
            .expect("remove fixture fault");
        let replay = accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW)
            .expect("retry prefix");
        assert_eq!(replay.search_rows_consumed, 3);
        let after = connection
            .list_retrieval_affinity_edges(&workspace_id)
            .expect("complete edges");
        assert_eq!(after.len(), 3);
        assert!(
            (after[0].2 - 1.0).abs() < 1e-9,
            "the old edge receives one increment"
        );
        assert!(replay.search_cursor > cursor.1);
    }

    #[test]
    fn failed_later_edge_rolls_back_the_successfully_written_first_edge() {
        let (_temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "atomic_d",
            &ATOMIC_HITS,
            ATOMIC_NOW,
        );
        connection
            .execute_raw(
                "CREATE TRIGGER affinity_fail_edge BEFORE INSERT ON retrieval_affinity_accumulation WHEN (SELECT COUNT(*) FROM retrieval_affinity_accumulation) > 0 BEGIN SELECT RAISE(ABORT, 'private-edge-fixture'); END;",
            )
            .expect("inject second-edge failure");
        assert!(accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW).is_err());
        assert!(
            connection
                .list_retrieval_affinity_edges(&workspace_id)
                .expect("no partial edges")
                .is_empty()
        );
        assert_eq!(
            connection
                .retrieval_affinity_cursor(&workspace_id)
                .expect("no cursor advance"),
            (0, 0)
        );
        connection
            .execute_raw("DROP TRIGGER affinity_fail_edge")
            .expect("remove fixture fault");
        let replay = accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW)
            .expect("retry all edges");
        assert_eq!(replay.pairs_updated, 3);
        let edges = connection
            .list_retrieval_affinity_edges(&workspace_id)
            .expect("all edges");
        assert_eq!(edges.len(), 3);
        assert!((edges[0].2 - 0.5).abs() < 1e-9);
        assert!((edges[1].2 - 1.0 / 3.0).abs() < 1e-9);
        assert!((edges[2].2 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn other_connections_never_observe_uncommitted_affinity_or_cursor_state() {
        let (temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "atomic_e",
            &ATOMIC_HITS[..2],
            ATOMIC_NOW,
        );
        let observer =
            DbConnection::open_file_read_only(&temp.path().join("ee.db")).expect("observer");
        let report = connection
            .with_transaction(|| {
                let report = accumulate_in_transaction(&connection, &workspace_id, ATOMIC_NOW)?;
                assert_eq!(
                    connection
                        .list_retrieval_affinity_edges(&workspace_id)?
                        .len(),
                    1
                );
                assert_eq!(
                    connection.retrieval_affinity_cursor(&workspace_id)?.1,
                    report.search_cursor
                );
                assert!(
                    observer
                        .list_retrieval_affinity_edges(&workspace_id)?
                        .is_empty()
                );
                assert_eq!(observer.retrieval_affinity_cursor(&workspace_id)?, (0, 0));
                Ok(report)
            })
            .expect("commit complete prefix");
        assert_eq!(
            observer
                .list_retrieval_affinity_edges(&workspace_id)
                .expect("visible edges")
                .len(),
            1
        );
        assert_eq!(
            observer
                .retrieval_affinity_cursor(&workspace_id)
                .expect("visible cursor")
                .1,
            report.search_cursor
        );
    }

    #[test]
    fn competing_accumulators_retry_from_committed_cursors_without_double_counting() {
        let (temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "atomic_f",
            &ATOMIC_HITS[..2],
            ATOMIC_NOW,
        );
        let database = temp.path().join("ee.db");
        let gate = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let handles = (0..2)
                .map(|_| {
                    let database = &database;
                    let workspace_id = &workspace_id;
                    let gate = &gate;
                    scope.spawn(move || {
                        let writer = DbConnection::open_file(database).expect("competing writer");
                        gate.wait();
                        // The write owner may reject contention. Never retry only
                        // the writes with deltas computed from a stale cursor.
                        accumulate_retrieval_affinity(&writer, workspace_id, ATOMIC_NOW)
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                let _outcome = handle.join().expect("writer did not panic");
            }
        });
        accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW)
            .expect("drain any contention retry");
        let edges = connection
            .list_retrieval_affinity_edges(&workspace_id)
            .expect("once-counted edges");
        assert_eq!(edges.len(), 1);
        assert!((edges[0].2 - 0.5).abs() < 1e-9);
        let replay = accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW)
            .expect("already consumed");
        assert_eq!(replay.search_rows_consumed, 0);
    }

    #[test]
    fn read_only_accumulation_cannot_publish_edges_or_a_cursor() {
        let (temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "atomic_g",
            &ATOMIC_HITS[..2],
            ATOMIC_NOW,
        );
        let reader =
            DbConnection::open_file_read_only(&temp.path().join("ee.db")).expect("read only");
        assert!(accumulate_retrieval_affinity(&reader, &workspace_id, ATOMIC_NOW).is_err());
        assert!(
            connection
                .list_retrieval_affinity_edges(&workspace_id)
                .expect("no edges")
                .is_empty()
        );
        assert_eq!(
            connection
                .retrieval_affinity_cursor(&workspace_id)
                .expect("no cursor"),
            (0, 0)
        );
        assert_eq!(
            accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW)
                .expect("writer retry")
                .pairs_updated,
            1
        );
    }

    #[test]
    fn cursor_failure_with_no_pairs_does_not_acknowledge_a_singleton() {
        let (_temp, connection, workspace_id) = seeded_connection();
        seed_search_set(
            &connection,
            &workspace_id,
            "atomic_h",
            &ATOMIC_HITS[..1],
            ATOMIC_NOW,
        );
        connection
            .execute_raw(
                "CREATE TRIGGER affinity_fail_cursor BEFORE INSERT ON retrieval_affinity_cursor BEGIN SELECT RAISE(ABORT, 'cursor fixture'); END;",
            )
            .expect("inject cursor-only failure");
        assert!(accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW).is_err());
        assert_eq!(
            connection
                .retrieval_affinity_cursor(&workspace_id)
                .expect("cursor"),
            (0, 0)
        );
        connection
            .execute_raw("DROP TRIGGER affinity_fail_cursor")
            .expect("remove fixture fault");
        let replay = accumulate_retrieval_affinity(&connection, &workspace_id, ATOMIC_NOW)
            .expect("replay singleton");
        assert_eq!((replay.search_rows_consumed, replay.pairs_updated), (1, 0));
        assert!(replay.search_cursor > 0);
    }

    fn seeded_connection() -> (tempfile::TempDir, DbConnection, String) {
        let temp = tempfile::tempdir().expect("tempdir");
        let database_path = temp.path().join("ee.db");
        let connection = DbConnection::open_file(&database_path).expect("open");
        connection.migrate().expect("migrate");
        let workspace_id = "wsp_00000000000000000000000901".to_owned();
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
                ("mem_00000000000000000000000001", 1),
                ("mem_00000000000000000000000002", 2),
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
                ("mem_00000000000000000000000001", 1),
                ("mem_00000000000000000000000002", 4),
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
                ("mem_00000000000000000000000001", 1),
                ("mem_00000000000000000000000002", 2),
                ("mem_00000000000000000000000003", 3),
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
