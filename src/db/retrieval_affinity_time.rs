//! Per-edge evidence times for the diagnostic retrieval-affinity projection.
//!
//! The core accumulator owns the enclosing write transaction, including source
//! reads and consumption cursors. Never acknowledge a cursor separately from
//! these updates. Historical weights and historical timestamp contamination
//! cannot be reconstructed here; repairing those needs the original evidence.

use chrono::{DateTime, SecondsFormat, Utc};
use sqlmodel_core::Value;

use crate::db::{DbConnection, DbError, DbOperation, Result};

const READ_EVENT_AT: &str = "SELECT CASE WHEN length(CAST(last_event_at AS BLOB)) <= 128 THEN last_event_at ELSE NULL END FROM retrieval_affinity_accumulation WHERE workspace_id = ?1 AND memory_a = ?2 AND memory_b = ?3 LIMIT 2";
const WRITE_EVENT_AT: &str = "UPDATE retrieval_affinity_accumulation SET last_event_at = ?4 WHERE workspace_id = ?1 AND memory_a = ?2 AND memory_b = ?3";

fn invalid_event_time() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not validate retrieval-affinity edge evidence time".to_owned(),
    }
}

impl DbConnection {
    /// Apply each increment with the latest instant belonging to THAT pair.
    ///
    /// Caller must hold the existing writer transaction for this entire batch
    /// and its consumption cursor. Reads are point lookups on the composite
    /// primary key; no scan of unrelated graph edges or raw evidence is needed.
    ///
    /// The existing additive primitive compares timestamp strings. Retained
    /// rows can use offsets or omit fractional seconds, so first compute the
    /// maximum as instants, then normalize after its ordinary weight update.
    /// Both writes roll back if normalization or the later cursor write fails.
    pub(crate) fn apply_retrieval_affinity_timed_deltas(
        &self,
        workspace_id: &str,
        deltas: &[(String, String, f64, DateTime<Utc>)],
    ) -> Result<()> {
        for (memory_a, memory_b, weight, event_at) in deltas {
            if !weight.is_finite() || *weight <= 0.0 {
                return Err(invalid_event_time());
            }
            let mut params = vec![
                Value::Text(workspace_id.to_owned()),
                Value::Text(memory_a.clone()),
                Value::Text(memory_b.clone()),
            ];
            let rows = self.query(READ_EVENT_AT, &params)?;
            let latest = match rows.as_slice() {
                [] => *event_at,
                [row] => {
                    let previous = row
                        .get(0)
                        .and_then(Value::as_str)
                        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                        .ok_or_else(invalid_event_time)?
                        .with_timezone(&Utc);
                    previous.max(*event_at)
                }
                _ => return Err(invalid_event_time()),
            };
            let canonical = latest.to_rfc3339_opts(SecondsFormat::Nanos, true);
            self.apply_retrieval_affinity_deltas(
                workspace_id,
                &[(memory_a.clone(), memory_b.clone(), *weight)],
                &canonical,
            )?;
            params.push(Value::Text(canonical));
            let changed = self.execute_for(DbOperation::Execute, WRITE_EVENT_AT, &params)?;
            if changed != 1 {
                return Err(invalid_event_time());
            }
        }
        Ok(())
    }
}
