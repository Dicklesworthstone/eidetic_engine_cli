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

const FEEDBACK_BATCH: usize = 128;

fn unavailable() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify repair feedback; positive guidance withheld".to_owned(),
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

/// Select the existing bounded positive-repair window after excluding exact
/// harmful observations. Filtering after LIMIT would allow a rejected prefix
/// to hide every useful target. Never materialize the whole link history or
/// any source body to make this decision. Both sides use the fingerprint/
/// kind/outcome/target lookup shape; rejected index entries may still need to
/// be scanned, so the output limit is not a constant-total-work guarantee.
///
/// The legacy history selector remains unchanged for diagnostic consumers.
/// Preserve its 128-byte projection and exact aliases, including embedded NUL.
/// An oversized target becomes an empty, inadmissible sentinel, not a prefix.
pub(super) fn pack_targets(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    limit: u32,
) -> Result<Vec<String>> {
    if limit > super::PACK_RECALL_TARGET_LIMIT {
        return Err(unavailable());
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    let rows = connection
        .query(
            "SELECT CASE WHEN length(CAST(h.target_id AS BLOB)) <= 128 THEN h.target_id ELSE '' END FROM error_repair_links AS h WHERE h.workspace_id = ?1 AND h.fingerprint_key = ?2 AND h.link_kind = 'repair' AND h.outcome = 'helpful' AND NOT EXISTS (SELECT 1 FROM error_repair_links AS n WHERE n.workspace_id = ?1 AND n.fingerprint_key = ?2 AND n.link_kind = 'repair' AND n.outcome = 'harmful' AND n.target_id = h.target_id) ORDER BY h.target_id ASC LIMIT ?3",
            &[
                Value::Text(workspace_id.to_owned()),
                Value::Text(fingerprint_key.to_owned()),
                Value::Integer(i64::from(limit)),
            ],
        )
        .map_err(|_| unavailable())?;
    if rows.len() > limit as usize {
        return Err(unavailable());
    }
    rows.into_iter()
        .map(|row| {
            row.get(0)
                .and_then(Value::as_str)
                .filter(|id| id.len() <= 128)
                .map(str::to_owned)
                .ok_or_else(unavailable)
        })
        .collect()
}
