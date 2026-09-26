//! Read-only, workspace-bound pages of durable memory invalidations.
//!
//! The audit cursor orders events; memory metadata is the current projection
//! within the same pinned snapshot, not a reconstruction of past memory bodies.
//! A filter narrows the addressed workspace and never grants another scope.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;
use std::time::Instant;

use chrono::{DateTime, TimeDelta, Utc};
use sqlmodel_core::Value as SqlValue;

use super::{
    MAX_LIMIT, MemoryDelta, SUBSCRIBE_CURSOR_STALE, SUBSCRIBE_POLL_SCHEMA_V1, SubscribeDegradation,
    SubscribeFilter, SubscribePollOptions, SubscribePollReport, cursor_sql_value, malformed_row,
    materialize_delta, raw_delta_from_row, resolve_workspace_path, subscribe_filter_domain_error,
};
use crate::core::workspace::{bound_workspace_id_or_hash, stable_workspace_id};
use crate::db::{DatabaseConfig, DbConnection, audit_actions};
use crate::models::{DomainError, MemoryId};

#[path = "subscribe_invalidation.rs"]
mod invalidation;
pub use invalidation::{MEMORY_INVALIDATION_SCHEMA_V1, MemoryInvalidation};

fn storage_error(_: impl std::fmt::Display) -> DomainError {
    DomainError::Storage {
        message: "Could not read a consistent subscription page; no cursor was acknowledged"
            .to_owned(),
        repair: Some(
            "Run ee doctor --json. Apply any required schema migration explicitly with ee migrate run, then retry with the previous cursor."
                .to_owned(),
        ),
    }
}

pub(super) fn poll_memory_deltas(
    options: &SubscribePollOptions<'_>,
) -> Result<SubscribePollReport, DomainError> {
    let started = Instant::now();
    let since_cutoff = subscription_cutoff(&options.filter, Utc::now())?;
    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    let requested = stable_workspace_id(&workspace_path);
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    if !database_path.try_exists().map_err(storage_error)? {
        return Err(crate::core::storeless_workspace_error(&database_path));
    }
    // Polling must not initialize a vanished store, run migrations, enqueue
    // jobs, or contend for the write owner merely to inspect committed work.
    let connection = DbConnection::open(DatabaseConfig::read_only_file(database_path.clone()))
        .map_err(storage_error)?;
    let snapshot = SubscriptionSnapshot::begin(&connection)?;
    let workspace_id = bound_workspace_id_or_hash(
        &connection,
        &requested,
        &[workspace_path.as_path(), options.workspace_path],
    )?;
    if connection
        .get_workspace(&workspace_id)
        .map_err(storage_error)?
        .is_none()
    {
        return Err(DomainError::Configuration {
            message: "The subscription database does not contain the requested workspace binding"
                .to_owned(),
            repair: Some(
                "Select the workspace's own database or an explicitly bound alternate store."
                    .to_owned(),
            ),
        });
    }
    let page = snapshot.page(
        &workspace_id,
        options.cursor,
        options.limit.clamp(1, MAX_LIMIT),
        &options.filter,
        since_cutoff,
    )?;
    snapshot.finish()?;

    let mut degraded = Vec::new();
    if options.cursor > page.high_watermark {
        degraded.push(SubscribeDegradation {
            code: SUBSCRIBE_CURSOR_STALE.to_owned(),
            severity: "warning".to_owned(),
            message: format!(
                "Requested cursor {} is ahead of the workspace audit high watermark {}.",
                options.cursor, page.high_watermark
            ),
            repair: "Resynchronize from authoritative workspace state before accepting nextCursor, or poll from cursor 0 to replay retained history. Reuse cursors only with the same store, workspace, and filter.".to_owned(),
        });
    }
    let degraded_codes: Vec<&str> = degraded.iter().map(|entry| entry.code.as_str()).collect();
    tracing::info!(
        target: "ee::subscribe",
        surface = "subscribe",
        bead_id = "bd-ub249",
        workspace_id,
        request_id = "subscribe_poll",
        cursor = options.cursor,
        deltas_emitted = page.deltas.len(),
        invalidations_emitted = page.invalidations.len(),
        has_more = page.has_more,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        degraded_codes = ?degraded_codes,
        "subscribe poll complete"
    );
    Ok(SubscribePollReport {
        schema: SUBSCRIBE_POLL_SCHEMA_V1,
        command: "subscribe poll",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id,
        database_path,
        cursor: options.cursor,
        next_cursor: page.next_cursor,
        high_watermark: page.high_watermark,
        has_more: page.has_more,
        delta_count: page.deltas.len(),
        deltas: page.deltas,
        invalidations: page.invalidations,
        degraded,
    })
}

struct DeltaPage {
    high_watermark: u64,
    next_cursor: u64,
    has_more: bool,
    deltas: Vec<MemoryDelta>,
    invalidations: Vec<MemoryInvalidation>,
}

fn subscription_cutoff(
    filter: &SubscribeFilter,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, DomainError> {
    let Some(milliseconds) = filter.since_ms else {
        return Ok(None);
    };
    let invalid = || {
        subscribe_filter_domain_error(
            "Subscription sinceMs is outside the supported non-negative time range.".to_owned(),
            "Choose a smaller non-negative sinceMs value; the time filter was not disabled.",
        )
    };
    if milliseconds < 0 {
        return Err(invalid());
    }
    // Overflow must be an error, not None (which means an unfiltered feed).
    now.checked_sub_signed(TimeDelta::milliseconds(milliseconds))
        .map(Some)
        .ok_or_else(invalid)
}

struct SubscriptionSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl<'a> SubscriptionSnapshot<'a> {
    fn begin(connection: &'a DbConnection) -> Result<Self, DomainError> {
        connection.begin_read_snapshot().map_err(storage_error)?;
        Ok(Self {
            connection,
            active: true,
        })
    }

    fn high_watermark(&self, workspace_id: &str) -> Result<u64, DomainError> {
        let rows = self
            .connection
            .query(
                "SELECT COALESCE(MAX(a.rowid), 0) FROM audit_log a \
             LEFT JOIN memories m ON m.id = a.target_id \
             WHERE a.workspace_id = ?1 \
                OR (a.workspace_id IS NULL AND m.workspace_id = ?1)",
                &[SqlValue::Text(workspace_id.to_owned())],
            )
            .map_err(storage_error)?;
        rows.first()
            .and_then(|row| row.get(0).and_then(|value| value.as_i64()))
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| malformed_row("workspace audit high watermark"))
    }

    fn page(
        &self,
        workspace_id: &str,
        cursor: u64,
        limit: u32,
        filter: &SubscribeFilter,
        since_cutoff: Option<DateTime<Utc>>,
    ) -> Result<DeltaPage, DomainError> {
        let high_watermark = self.high_watermark(workspace_id)?;
        let limit = limit.clamp(1, MAX_LIMIT) as usize;
        let rows = self
            .connection
            .query(
                "SELECT a.rowid, a.id, COALESCE(a.workspace_id, m.workspace_id), \
                    a.timestamp, a.actor, a.action, a.target_id, m.level, m.kind, m.trust_class \
             FROM audit_log a LEFT JOIN memories m ON m.id = a.target_id \
             WHERE a.rowid > ?1 AND a.rowid <= ?4 \
               AND (a.target_type = 'memory' OR (a.target_type IS NULL \
                    AND (a.action LIKE 'memory.%' OR a.action = ?3))) \
               AND a.target_id IS NOT NULL \
               AND COALESCE(a.workspace_id, m.workspace_id) = ?5 \
               AND (m.id IS NULL OR m.workspace_id = ?5) \
             ORDER BY a.rowid ASC LIMIT ?2",
                &[
                    cursor_sql_value(cursor),
                    SqlValue::BigInt((limit + 1) as i64),
                    SqlValue::Text(audit_actions::TRUST_CLASS_TRANSITION.to_owned()),
                    cursor_sql_value(high_watermark),
                    SqlValue::Text(workspace_id.to_owned()),
                ],
            )
            .map_err(storage_error)?;
        // One lookahead row proves whether the raw page is exhausted. Never
        // acknowledge that row: the next request still has to inspect it.
        let has_more = rows.len() > limit;
        let mut next_cursor = cursor.min(high_watermark);
        let mut deltas = Vec::new();
        let mut invalidations = Vec::new();
        // Metadata-excluded events can still invalidate prior membership.
        // Only candidates eligible for a full delta need current tag reads.
        let mut metadata_filter = filter.clone();
        metadata_filter.tags.clear();
        for row in rows.into_iter().take(limit) {
            let raw = raw_delta_from_row(&row)?;
            next_cursor = raw.cursor;
            // A batch/scope audit or a foreign native entity is not a memory
            // event merely because its action shares the memory prefix.
            if MemoryId::from_str(&raw.memory_id).is_err() {
                continue;
            }
            let delta = materialize_delta(raw, Vec::new());
            if metadata_filter.matches_delta(&delta, since_cutoff) {
                deltas.push(delta);
            } else if let Some(notice) =
                invalidation::filtered_invalidation(filter, &delta, since_cutoff)
            {
                invalidations.push(notice);
            }
        }
        self.hydrate_tags(&mut deltas)?;
        deltas.retain(|delta| {
            if filter.matches_delta(delta, since_cutoff) {
                true
            } else {
                if let Some(notice) =
                    invalidation::filtered_invalidation(filter, delta, since_cutoff)
                {
                    invalidations.push(notice);
                }
                false
            }
        });
        // The metadata and tag phases can discover exits in opposite order.
        // Both arrays must retain the durable audit order within this page.
        invalidations.sort_by_key(|notice| notice.cursor);
        if !has_more {
            // Skip only the proven-empty tail of this pinned snapshot. New
            // commits are above this watermark and remain visible next time.
            next_cursor = high_watermark;
        }
        Ok(DeltaPage {
            high_watermark,
            next_cursor,
            has_more,
            deltas,
            invalidations,
        })
    }

    /// Hydrate each distinct candidate once in this same read snapshot.
    /// Repeated events retain their own cursors and complete canonical tags;
    /// lookahead and metadata-only invalidations never enter a batch.
    fn hydrate_tags(&self, deltas: &mut [MemoryDelta]) -> Result<(), DomainError> {
        let ids: BTreeSet<&str> = deltas.iter().map(|delta| delta.memory_id.as_str()).collect();
        let ids: Vec<&str> = ids.into_iter().collect();
        let mut tags = BTreeMap::new();
        // At most 40 bulk-reader calls for the 10,000-event page limit.
        // Bound SQL parameters independently of repeated audit identities.
        for batch in ids.chunks(256) {
            tags.extend(
                self.connection
                    .get_memory_tags_batch(batch)
                    .map_err(storage_error)?,
            );
        }
        for delta in deltas {
            delta.tags = tags.get(&delta.memory_id).cloned().unwrap_or_default();
            delta.tags.sort();
            delta.tags.dedup();
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(), DomainError> {
        self.connection
            .commit_read_snapshot()
            .map_err(storage_error)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for SubscriptionSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            tracing::error!(target: "ee::subscribe", "failed to release subscription snapshot");
        }
    }
}

#[cfg(test)]
#[path = "subscribe_poll_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "subscribe_hydration_tests.rs"]
mod hydration_tests;
