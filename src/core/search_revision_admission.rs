//! Source-backed revision admission before ranking and duplicate suppression.
//!
//! Historical versions remain indexed for `--as-of`. V123 made supersession
//! independent of author expiry, so inclusion flags cannot revive an obsolete
//! revision. Indexed metadata is not authority for this decision.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use super::{DbConnection, SearchDegradation, SearchHit, SearchOptions};
use crate::db::{DbError, DbOperation};
use crate::models::MemoryId;

const PAGE_SIZE: usize = 256;
const FILTERED: &str = "superseded_revision_filtered";
pub(in crate::core::search) const UNAVAILABLE: &str = "revision_visibility_unavailable";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RevisionState {
    Current,
    Superseded(DateTime<Utc>),
    Malformed,
}

impl RevisionState {
    fn visible_at(self, reference: DateTime<Utc>) -> bool {
        match self {
            Self::Current => true,
            Self::Superseded(at) => reference < at,
            Self::Malformed => false,
        }
    }
}

fn is_memory(hit: &SearchHit) -> bool {
    hit.doc_id.parse::<MemoryId>().is_ok()
}

fn states(
    connection: &DbConnection,
    ids: &BTreeSet<&str>,
) -> Result<BTreeMap<String, RevisionState>, DbError> {
    let ids: Vec<_> = ids.iter().copied().collect();
    let mut result = BTreeMap::new();
    for page in ids.chunks(PAGE_SIZE) {
        let placeholders = (1..=page.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT id, superseded_at FROM memories WHERE id IN ({placeholders}) ORDER BY id ASC"
        );
        let parameters = page
            .iter()
            .map(|id| Value::Text((*id).to_owned()))
            .collect::<Vec<_>>();
        for row in connection.query(&sql, &parameters)? {
            let Some(Value::Text(id)) = row.get(0) else {
                return Err(DbError::MalformedRow {
                    operation: DbOperation::Query,
                    message: "Could not read memory revision identity".to_owned(),
                });
            };
            let state = match row.get(1) {
                Some(Value::Null) => RevisionState::Current,
                Some(Value::Text(raw)) => DateTime::parse_from_rfc3339(raw)
                    .map(|at| RevisionState::Superseded(at.with_timezone(&Utc)))
                    .unwrap_or(RevisionState::Malformed),
                _ => RevisionState::Malformed,
            };
            result.insert(id.clone(), state);
        }
    }
    Ok(result)
}

fn load_states(
    options: &SearchOptions,
    ids: &BTreeSet<&str>,
    read_connection: Option<&DbConnection>,
) -> Result<BTreeMap<String, RevisionState>, DbError> {
    if let Some(connection) = read_connection {
        // The caller owns the snapshot. Never begin or release its transaction.
        return states(connection, ids);
    }
    let connection = DbConnection::open_file_read_only(&options.resolve_database_path())?;
    connection.begin_read_snapshot()?;
    let result = states(&connection, ids);
    let released = connection.rollback_read_snapshot();
    result.and_then(|states| released.map(|()| states))
}

fn unavailable() -> SearchDegradation {
    SearchDegradation {
        code: UNAVAILABLE.to_owned(),
        severity: "medium".to_owned(),
        message: "Memory candidates whose revision identity could not be verified were withheld; unrelated entity types remain available.".to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

/// Filter identities belonging to the addressed source snapshot. Missing rows
/// are left to the existing orphan/scope gates: the merged candidate pool can
/// include independently admitted global-store rows. This does not authenticate
/// those rows or replace the separate global-store admission boundary.
pub(in crate::core::search) fn admit_hits(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
) -> Vec<SearchHit> {
    let ids = hits
        .iter()
        .filter(|hit| is_memory(hit))
        .map(|hit| hit.doc_id.as_str())
        .collect::<BTreeSet<_>>();
    if ids.is_empty() {
        return hits;
    }
    let states = match load_states(options, &ids, read_connection) {
        Ok(states) => states,
        Err(_) => {
            // Do not echo SQL, host paths, raw IDs or malformed timestamps.
            degraded.push(unavailable());
            return hits.into_iter().filter(|hit| !is_memory(hit)).collect();
        }
    };
    let reference = options.as_of.unwrap_or_else(Utc::now);
    let mut superseded = 0usize;
    let mut malformed = 0usize;
    let hits = hits
        .into_iter()
        .filter(|hit| match states.get(&hit.doc_id).copied() {
            Some(RevisionState::Malformed) => {
                malformed += 1;
                false
            }
            Some(state) if !state.visible_at(reference) => {
                superseded += 1;
                false
            }
            _ => true,
        })
        .collect();
    if superseded > 0 {
        degraded.push(SearchDegradation {
            code: FILTERED.to_owned(),
            severity: "low".to_owned(),
            message: format!("Excluded {superseded} superseded memory revisions at the requested reference time. Author expiry and inclusion flags do not change revision identity."),
            repair: Some("Use --as-of <RFC3339> before supersession to inspect historical revisions.".to_owned()),
        });
    }
    if malformed > 0 {
        degraded.push(unavailable());
    }
    hits
}

#[cfg(test)]
#[path = "search_revision_admission_tests.rs"]
mod tests;
