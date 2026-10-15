//! Source-backed negative feedback admission for positive repair recall.
//!
//! Helpful and harmful observations are independent historical rows. A later
//! helpful upsert cannot erase contrary evidence, and a cached report cannot
//! authorize a repair that has since received a harmful observation. This is
//! a read-time hold on unqualified positive guidance, not rule deprecation or
//! a mutation of feedback history. Scope is the exact workspace/error class.

use std::collections::BTreeSet;

use sqlmodel_core::Value;

use crate::db::{DbConnection, DbError, DbOperation, Result};

#[path = "error_recall_revalidate.rs"]
mod revalidate;

pub(super) use revalidate::Gate;

const FEEDBACK_BATCH: usize = 128;

fn unavailable() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify repair feedback or source proof; positive guidance withheld".to_owned(),
    }
}

/// Inspect only one already selected batch, inside the caller's source
/// snapshot. Target identity is exact: never trim it or truncate an alias.
/// Return identifiers only; feedback reasons and source text stay private.
pub(super) fn harmful_targets(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    targets: &[&str],
) -> Result<BTreeSet<String>> {
    if targets.len() > FEEDBACK_BATCH || targets.iter().any(|id| id.len() > 128) {
        return Err(unavailable());
    }
    if targets.is_empty() {
        return Ok(BTreeSet::new());
    }
    let selected: BTreeSet<_> = targets.iter().copied().collect();
    let placeholders = (3..3 + selected.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT DISTINCT target_id FROM error_repair_links WHERE workspace_id = ?1 AND fingerprint_key = ?2 AND link_kind = 'repair' AND outcome = 'harmful' AND target_id IN ({placeholders}) ORDER BY target_id ASC"
    );
    let mut parameters = vec![
        Value::Text(workspace_id.to_owned()),
        Value::Text(fingerprint_key.to_owned()),
    ];
    parameters.extend(selected.iter().map(|id| Value::Text((*id).to_owned())));
    let rows = connection
        .query(&sql, &parameters)
        .map_err(|_| unavailable())?;
    let mut harmful = BTreeSet::new();
    for row in rows {
        let Some(id) = row.get(0).and_then(Value::as_str) else {
            return Err(unavailable());
        };
        if !selected.contains(id) || !harmful.insert(id.to_owned()) {
            return Err(unavailable());
        }
    }
    Ok(harmful)
}

/// Select up to the existing target budget after feedback and live proof checks.
/// Page metadata in canonical order, refilling only slots refused by the gate.
/// The server retains the raw target ordering key; only a bounded link ID crosses
/// the cursor boundary, so an oversized target never has to be hydrated to page.
/// One gate reuses bounded per-session verdicts throughout this source snapshot.
///
/// Diagnostic history and the legacy 128-byte target projection stay unchanged.
/// Empty sentinels and exact aliases retain their old sampling semantics. Total
/// index scans and source interpretation are not constant work or a latency SLO.
pub(super) fn pack_targets(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    limit: u32,
) -> Result<Vec<String>> {
    let mut gate = Gate::new(connection, workspace_id, fingerprint_key);
    pack_targets_with(connection, workspace_id, fingerprint_key, limit, |ids| gate.withheld(ids))
}

fn pack_targets_with(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    limit: u32,
    mut withheld: impl FnMut(&[&str]) -> Result<BTreeSet<String>>,
) -> Result<Vec<String>> {
    if limit > super::PACK_RECALL_TARGET_LIMIT {
        return Err(unavailable());
    }
    let mut output = Vec::new();
    let mut after = String::new();
    while output.len() < limit as usize {
        let remaining = limit as usize - output.len();
        let rows = connection.query(
            "SELECT CASE WHEN length(CAST(h.target_id AS BLOB)) <= 128 THEN h.target_id ELSE '' END, CASE WHEN length(CAST(h.link_id AS BLOB)) <= 128 THEN h.link_id ELSE '' END FROM error_repair_links AS h WHERE h.workspace_id = ?1 AND h.fingerprint_key = ?2 AND h.link_kind = 'repair' AND h.outcome = 'helpful' AND (?4 = '' OR h.target_id > (SELECT c.target_id FROM error_repair_links AS c WHERE c.workspace_id = ?1 AND c.fingerprint_key = ?2 AND c.link_id = ?4) OR (h.target_id = (SELECT c.target_id FROM error_repair_links AS c WHERE c.workspace_id = ?1 AND c.fingerprint_key = ?2 AND c.link_id = ?4) AND h.link_id > ?4)) AND NOT EXISTS (SELECT 1 FROM error_repair_links AS n WHERE n.workspace_id = ?1 AND n.fingerprint_key = ?2 AND n.link_kind = 'repair' AND n.outcome = 'harmful' AND n.target_id = h.target_id) ORDER BY h.target_id ASC, h.link_id ASC LIMIT ?3",
            &[
                Value::Text(workspace_id.to_owned()),
                Value::Text(fingerprint_key.to_owned()),
                Value::Integer(i64::try_from(remaining).map_err(|_| unavailable())?),
                Value::Text(after.clone()),
            ],
        ).map_err(|_| unavailable())?;
        if rows.is_empty() {
            break;
        }
        if rows.len() > remaining {
            return Err(unavailable());
        }
        let page_complete = rows.len() == remaining;
        let mut targets = Vec::with_capacity(rows.len());
        for row in &rows {
            let target = row.get(0).and_then(Value::as_str)
                .filter(|id| id.len() <= 128).ok_or_else(unavailable)?;
            let cursor = row.get(1).and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 128).ok_or_else(unavailable)?;
            if cursor == after {
                return Err(unavailable());
            }
            targets.push(target.to_owned());
            after = cursor.to_owned();
        }
        let refs: Vec<_> = targets.iter().map(String::as_str).collect();
        let rejected = withheld(&refs)?;
        if rejected.iter().any(|id| !refs.contains(&id.as_str())) {
            return Err(unavailable());
        }
        output.extend(targets.into_iter().filter(|id| !rejected.contains(id)));
        if !page_complete {
            break;
        }
    }
    Ok(output)
}
