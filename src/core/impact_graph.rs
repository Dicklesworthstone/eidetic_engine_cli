//! The third impact tier: a surface co-mentioned by an admitted memory leads
//! to other memories anchored there. Use the canonical anchor projection for
//! every witness, not lexical similarity or a fabricated durable link. One
//! source snapshot covers anchors, scopes, lifecycle, holds, seals and bodies.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlmodel_core::{Row, Value};

use super::{
    DbConnection, DbError, ImpactMatchType, ImpactMemorySummary, ImpactResolvedSurface,
    ImpactResult, MemoryAnchorKind, MemoryAnchorSource, MemoryScopeContext, MemoryScopeStats,
    StoredMemoryAnchor, memory_tags_include_global_scope,
};
use crate::graph::anchor_projection::{
    AnchorProjectionRelation, anchor_surface_key, project_memory_anchor_graph,
};
use crate::models::{MemoryAnchorFreshnessState, MemoryId, validate_attestation_seal_fields};

const SOURCE_LIMIT: usize = 4096;
const PATH_LIMIT: usize = 8192;
const BODY_LIMIT: i64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactGraphPhase {
    pub status: &'static str,
    pub reason: Option<&'static str>,
    pub result_count: usize,
    pub source_rows: usize,
    pub paths_examined: usize,
    pub truncated: bool,
}

impl ImpactGraphPhase {
    fn empty(status: &'static str, reason: Option<&'static str>) -> Self {
        Self { status, reason, result_count: 0, source_rows: 0, paths_examined: 0, truncated: false }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status, "reason": self.reason, "resultCount": self.result_count,
            "sourceRows": self.source_rows, "sourceRowLimit": SOURCE_LIMIT,
            "pathsExamined": self.paths_examined, "pathLimit": PATH_LIMIT,
            "truncated": self.truncated,
        })
    }
}

pub(super) fn read(
    db: &DbConnection,
    workspace: &str,
    scope: &MemoryScopeContext,
    stats: &mut MemoryScopeStats,
    surface: &ImpactResolvedSurface,
    excluded: &BTreeSet<String>,
    limit: usize,
) -> (Vec<ImpactResult>, ImpactGraphPhase) {
    if limit == 0 {
        return (Vec::new(), ImpactGraphPhase::empty("skipped_limit_filled", None));
    }
    if !cfg!(feature = "graph") {
        return (Vec::new(), ImpactGraphPhase::empty("not_available", Some("graph_feature_disabled")));
    }
    let mut graph_stats = scope.stats();
    match collect(db, workspace, scope, &mut graph_stats, surface, excluded, limit) {
        Ok(result) => { stats.merge(&graph_stats); result }
        Err(_) => (Vec::new(), ImpactGraphPhase::empty("not_available", Some("graph_source_unavailable"))),
    }
}

fn unavailable() -> DbError {
    DbError::MalformedRow {
        operation: crate::db::DbOperation::Query,
        message: "Could not verify impact graph sources; no graph advice returned".to_owned(),
    }
}

struct Snapshot<'a> { db: &'a DbConnection, owned: bool }
impl<'a> Snapshot<'a> {
    fn begin(db: &'a DbConnection) -> Result<Self, DbError> {
        let owned = match db.begin_read_snapshot() {
            Ok(()) => true,
            Err(error) if crate::db::db_error_is_nested_transaction(&error) => false,
            Err(_) => return Err(unavailable()),
        };
        Ok(Self { db, owned })
    }
    fn finish(mut self) -> Result<(), DbError> {
        if self.owned { self.db.rollback_read_snapshot()?; self.owned = false; }
        Ok(())
    }
}
impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        if self.owned { let _ = self.db.rollback_read_snapshot(); }
    }
}

#[derive(Clone)]
struct Admitted { workspace: String, summary: ImpactMemorySummary }

struct Reader<'a> {
    db: &'a DbConnection,
    workspace: &'a str,
    scope: &'a MemoryScopeContext,
    stats: &'a mut MemoryScopeStats,
    at: DateTime<Utc>,
    phase: ImpactGraphPhase,
    memories: BTreeMap<String, Option<Admitted>>,
    surfaces: BTreeMap<(MemoryAnchorKind, String), Vec<StoredMemoryAnchor>>,
}

fn text(row: &Row, column: usize) -> Result<&str, DbError> {
    row.get(column).and_then(Value::as_str).ok_or_else(unavailable)
}

fn timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw).ok().map(|at| at.with_timezone(&Utc))
}

fn hash(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn anchor_valid(anchor: &StoredMemoryAnchor) -> bool {
    anchor.memory_id.parse::<MemoryId>().is_ok_and(|id| id.to_string() == anchor.memory_id)
        && hash(&anchor.anchor_value_hash) && hash(&anchor.captured_span_hash)
        && anchor.generation >= 0 && anchor.confidence.is_finite()
        && anchor.confidence > 0.0 && anchor.confidence <= 1.0
}

#[allow(clippy::cast_possible_truncation)]
fn anchor(row: &Row) -> Result<StoredMemoryAnchor, DbError> {
    Ok(StoredMemoryAnchor {
        memory_id: text(row, 0)?.to_owned(),
        anchor_kind: MemoryAnchorKind::parse(text(row, 1)?).ok_or_else(unavailable)?,
        anchor_value_hash: text(row, 2)?.to_owned(),
        redacted_anchor_value: text(row, 3)?.to_owned(),
        confidence: row.get(4).and_then(Value::as_f64).ok_or_else(unavailable)? as f32,
        source: MemoryAnchorSource::parse(text(row, 5)?).ok_or_else(unavailable)?,
        provenance: text(row, 6)?.to_owned(),
        captured_span_hash: text(row, 7)?.to_owned(),
        freshness_state: MemoryAnchorFreshnessState::parse(text(row, 8)?).ok_or_else(unavailable)?,
        generation: row.get(9).and_then(Value::as_i64).ok_or_else(unavailable)?,
        created_at: text(row, 10)?.to_owned(), updated_at: text(row, 11)?.to_owned(),
    })
}

impl Reader<'_> {
    // Both selectors are internal constants. All source values are parameters.
    // Bound row payloads before hydration; malformed/oversized metadata cannot
    // gain graph authority by being truncated into an apparently valid value.
    fn anchors(&mut self, predicate: &str, first: &str, second: &str) -> Result<Vec<StoredMemoryAnchor>, DbError> {
        let remaining = SOURCE_LIMIT - self.phase.source_rows;
        if remaining == 0 { self.phase.truncated = true; return Ok(Vec::new()); }
        let sql = format!(
            "SELECT memory_id, anchor_kind, anchor_value_hash, redacted_anchor_value, confidence, source, provenance, captured_span_hash, freshness_state, generation, created_at, updated_at FROM memory_anchors WHERE {predicate} AND length(CAST(memory_id AS BLOB)) <= 128 AND length(CAST(anchor_kind AS BLOB)) <= 32 AND length(CAST(anchor_value_hash AS BLOB)) <= 71 AND length(CAST(redacted_anchor_value AS BLOB)) <= 256 AND length(CAST(source AS BLOB)) <= 32 AND length(CAST(provenance AS BLOB)) <= 2048 AND length(CAST(captured_span_hash AS BLOB)) <= 71 AND length(CAST(freshness_state AS BLOB)) <= 32 AND length(CAST(created_at AS BLOB)) <= 64 AND length(CAST(updated_at AS BLOB)) <= 64 ORDER BY memory_id ASC, anchor_kind ASC, anchor_value_hash ASC LIMIT ?3"
        );
        let rows = self.db.query(&sql, &[
            Value::Text(first.to_owned()), Value::Text(second.to_owned()),
            Value::BigInt(i64::try_from(remaining + 1).map_err(|_| unavailable())?),
        ])?;
        self.phase.truncated |= rows.len() > remaining;
        self.phase.source_rows += rows.len().min(remaining);
        rows.iter().take(remaining).map(anchor).collect()
    }

    fn surface(&mut self, kind: MemoryAnchorKind, digest: &str) -> Result<Vec<StoredMemoryAnchor>, DbError> {
        let key = (kind, digest.to_owned());
        if let Some(rows) = self.surfaces.get(&key) { return Ok(rows.clone()); }
        let rows = self.anchors("anchor_kind = ?1 AND anchor_value_hash = ?2", kind.as_str(), digest)?;
        self.surfaces.insert(key, rows.clone());
        Ok(rows)
    }

    fn memory(&mut self, id: &str) -> Result<Option<Admitted>, DbError> {
        if let Some(value) = self.memories.get(id) { return Ok(value.clone()); }
        let admitted = self.admit(id)?;
        self.stats.record_candidate_id(admitted.is_some(), Some(id));
        self.memories.insert(id.to_owned(), admitted.clone());
        Ok(admitted)
    }

    fn admit(&self, id: &str) -> Result<Option<Admitted>, DbError> {
        let params = [Value::Text(id.to_owned())];
        let rows = self.db.query(
            "SELECT workspace_id, length(CAST(content AS BLOB)), tombstoned_at, superseded_at FROM memories WHERE id = ?1", &params,
        )?;
        let Some(row) = rows.first() else { return Ok(None); };
        if rows.len() != 1 { return Err(unavailable()); }
        if !matches!(row.get(2), Some(Value::Null))
            || !row.get(1).and_then(Value::as_i64).is_some_and(|bytes| (1..=BODY_LIMIT).contains(&bytes))
            || match row.get(3) {
                Some(Value::Null) => false,
                Some(Value::Text(value)) => timestamp(value).is_none_or(|at| at <= self.at),
                _ => true,
            }
        { return Ok(None); }
        let owner = text(row, 0)?;
        if !crate::core::memory_lifecycle::pending_memory_review_ids(self.db, owner, &[id])?.is_empty() {
            return Ok(None);
        }
        let seals = self.db.query(
            "SELECT content_commitment, sealed_at, revealed_at, reveal_verified FROM memory_seals WHERE memory_id = ?1", &params,
        )?;
        if !seals.is_empty() {
            if seals.len() != 1 { return Err(unavailable()); }
            let seal = &seals[0];
            let revealed = seal.get(2).and_then(Value::as_str);
            if revealed.is_none() || seal.get(3).and_then(Value::as_i64) != Some(1)
                || validate_attestation_seal_fields(text(seal, 0)?, text(seal, 1)?, revealed, Some(true)).is_err()
            { return Ok(None); }
        }
        let Some(memory) = self.db.get_memory(id)? else { return Err(unavailable()); };
        if memory.id != id || memory.workspace_id != owner { return Err(unavailable()); }
        let (Some(created), Some(updated)) = (timestamp(&memory.created_at), timestamp(&memory.updated_at)) else { return Ok(None); };
        if created > self.at || updated > self.at { return Ok(None); }
        let start = memory.valid_from.as_deref().map(timestamp);
        let end = memory.valid_to.as_deref().map(timestamp);
        if matches!(start, Some(None)) || matches!(end, Some(None)) { return Ok(None); }
        let (start, end) = (start.flatten(), end.flatten());
        if start.is_some_and(|at| at > self.at) || end.is_some_and(|at| at < self.at)
            || start.zip(end).is_some_and(|(start, end)| start > end)
        { return Ok(None); }
        let tags = self.db.get_memory_tags(id)?;
        if !(memory.workspace_id == self.workspace || memory_tags_include_global_scope(&tags))
            || !self.scope.memory_in_scope_with_tags(&memory, &tags)
            || memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
        { return Ok(None); }
        let screen = crate::policy::screen_external_text_for_ingestion(&memory.content);
        if screen.redacted || screen.instruction_like || !matches!(screen.instruction_risk, "none" | "low")
            || screen.content != memory.content
        { return Ok(None); }
        let summary = ImpactMemorySummary::from_memory(&memory);
        let rendered = summary.data_json().to_string();
        if crate::policy::redact_secret_like_content(&rendered).content != rendered { return Ok(None); }
        Ok(Some(Admitted { workspace: memory.workspace_id, summary }))
    }
}

fn freshness(anchor: &StoredMemoryAnchor) -> f32 {
    match anchor.freshness_state {
        MemoryAnchorFreshnessState::Current => 1.0,
        MemoryAnchorFreshnessState::Suspect => 0.6,
        MemoryAnchorFreshnessState::Stale => 0.25,
    }
}

fn witness(query: &StoredMemoryAnchor, bridge: &StoredMemoryAnchor, target: &StoredMemoryAnchor) -> Option<(f32, serde_json::Value)> {
    if ![query, bridge, target].into_iter().all(anchor_valid)
        || query.memory_id != bridge.memory_id || query.generation != bridge.generation
        || bridge.memory_id == target.memory_id
        || (query.anchor_kind, &query.anchor_value_hash) == (bridge.anchor_kind, &bridge.anchor_value_hash)
        || (bridge.anchor_kind, &bridge.anchor_value_hash) != (target.anchor_kind, &target.anchor_value_hash)
    { return None; }
    let projection = project_memory_anchor_graph(&[query.clone(), bridge.clone(), target.clone()]);
    let query_key = anchor_surface_key(query);
    let bridge_key = anchor_surface_key(bridge);
    if !projection.edges.iter().any(|edge| {
        edge.relation == AnchorProjectionRelation::AnchorProximity
            && ((edge.source == query_key && edge.target == bridge_key)
                || (edge.target == query_key && edge.source == bridge_key))
    }) { return None; }
    let score = query.confidence.min(bridge.confidence).min(target.confidence)
        * freshness(query) * freshness(bridge) * freshness(target);
    if !score.is_finite() || score <= 0.0 { return None; }
    let evidence: Vec<_> = [query, bridge, target].into_iter().map(|row| serde_json::json!({
        "memoryId": row.memory_id, "kind": row.anchor_kind.as_str(),
        "anchorValueHash": row.anchor_value_hash, "capturedSpanHash": row.captured_span_hash,
        "generation": row.generation, "confidence": row.confidence,
        "freshnessState": row.freshness_state.as_str(), "source": row.source.as_str(),
    })).collect();
    Some((score, serde_json::json!({
        "schema": "ee.impact.anchor_path.v1", "scoreKind": "anchor_path_support",
        "viaMemoryId": query.memory_id, "path": [query_key, bridge_key, target.memory_id],
        "relations": ["anchor_proximity", "mentions_surface"],
        "anchorEvidence": evidence,
    })))
}

fn collect(
    db: &DbConnection, workspace: &str, scope: &MemoryScopeContext, stats: &mut MemoryScopeStats,
    surface: &ImpactResolvedSurface, excluded: &BTreeSet<String>, limit: usize,
) -> Result<(Vec<ImpactResult>, ImpactGraphPhase), DbError> {
    let snapshot = Snapshot::begin(db)?;
    let mut reader = Reader {
        db, workspace, scope, stats, at: Utc::now(),
        phase: ImpactGraphPhase::empty("ok", None), memories: BTreeMap::new(), surfaces: BTreeMap::new(),
    };
    let roots = reader.surface(surface.kind, &surface.anchor_value_hash)?;
    let direct: BTreeSet<_> = roots.iter().map(|row| row.memory_id.as_str()).collect();
    let mut selected = BTreeMap::<String, ImpactResult>::new();
    'roots: for query in &roots {
        if !anchor_valid(query) { continue; }
        let Some(root) = reader.memory(&query.memory_id)? else { continue; };
        let bridges = reader.anchors("memory_id = ?1 AND ?2 = ''", &query.memory_id, "")?;
        for bridge in &bridges {
            if !anchor_valid(bridge) || bridge.generation != query.generation
                || (bridge.anchor_kind, &bridge.anchor_value_hash) == (surface.kind, &surface.anchor_value_hash)
            { continue; }
            for target in reader.surface(bridge.anchor_kind, &bridge.anchor_value_hash)? {
                if reader.phase.paths_examined == PATH_LIMIT { reader.phase.truncated = true; break 'roots; }
                reader.phase.paths_examined += 1;
                if direct.contains(target.memory_id.as_str()) || excluded.contains(&target.memory_id) { continue; }
                let Some((score, path)) = witness(query, bridge, &target) else { continue; };
                let Some(target_memory) = reader.memory(&target.memory_id)? else { continue; };
                // Global visibility is not permission to splice identically
                // spelled repo surfaces from two different workspaces.
                if root.workspace != target_memory.workspace { continue; }
                if selected.get(&target.memory_id).is_some_and(|old| old.score >= score) { continue; }
                selected.insert(target.memory_id.clone(), ImpactResult {
                    rank: 0, memory_id: target.memory_id, match_type: ImpactMatchType::GraphNeighbor,
                    score, memory: target_memory.summary, anchor: None, fallback_hit: None, graph_path: Some(path),
                });
            }
        }
    }
    let mut results: Vec<_> = selected.into_values().collect();
    results.sort_by(|left, right| right.score.total_cmp(&left.score).then_with(|| left.memory_id.cmp(&right.memory_id)));
    results.truncate(limit);
    reader.phase.result_count = results.len();
    if reader.phase.truncated { reader.phase.status = "partial"; reader.phase.reason = Some("graph_source_budget_exhausted"); }
    else if results.is_empty() { reader.phase.status = "not_available"; reader.phase.reason = Some("no_admitted_anchor_proximity"); }
    snapshot.finish()?;
    Ok((results, reader.phase))
}

#[cfg(test)]
#[path = "impact_graph_tests.rs"]
mod tests;
