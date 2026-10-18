//! Bound retained diagnostic payloads independently of the number of jobs.
//!
//! A 256-row limit alone can retain hundreds of MiB when rows approach their
//! 1 MiB wire bound. Count allocated string capacity, not UTF-8 character count
//! or serialized length. Vec slots and the single ordering cutoff are bounded
//! separately by the row limit and the journal's per-record limit.

use std::cmp::Ordering;

use super::DaemonJobRow;

pub(super) const MAX_DETAIL_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn retained_bytes(row: &DaemonJobRow) -> usize {
    let required = [
        &row.schema,
        &row.row_id,
        &row.run_id,
        &row.daemon_job_key,
        &row.runner_job_id,
        &row.job_type,
        &row.status,
        &row.workspace,
        &row.write_owner_id,
        &row.reason,
        &row.recorded_at,
    ];
    let optional = [
        &row.outcome,
        &row.started_at,
        &row.completed_at,
        &row.error,
        &row.recovery_reason,
    ];
    let mut bytes = std::mem::size_of::<DaemonJobRow>();
    for value in required {
        bytes = bytes.saturating_add(value.capacity());
    }
    for value in optional.into_iter().flatten() {
        bytes = bytes.saturating_add(value.capacity());
    }
    bytes
}

fn compare(left: &DaemonJobRow, right: &DaemonJobRow) -> Ordering {
    right
        .recorded_at
        .cmp(&left.recorded_at)
        .then_with(|| left.daemon_job_key.cmp(&right.daemon_job_key))
}

/// Keep the newest contiguous prefix that fits both limits, in any scan order.
/// Once a row is excluded, its rank is a barrier: later, smaller but older rows
/// must not fill the gap and make this cease to be a newest-first prefix.
/// Adding still-newer rows can only make that barrier stricter, never looser.
#[derive(Debug)]
pub(super) struct RecentRows {
    rows: Vec<DaemonJobRow>,
    cutoff: Option<(String, String)>,
    row_limit: usize,
    byte_limit: usize,
}

impl RecentRows {
    pub(super) fn new(row_limit: usize, byte_limit: usize) -> Self {
        Self {
            rows: Vec::new(),
            cutoff: None,
            row_limit,
            byte_limit,
        }
    }

    pub(super) fn push(&mut self, row: DaemonJobRow) {
        if self.row_limit == 0 {
            return;
        }
        if let Some((recorded_at, key)) = &self.cutoff {
            let ordering = recorded_at
                .cmp(&row.recorded_at)
                .then_with(|| row.daemon_job_key.cmp(key));
            if ordering != Ordering::Less {
                return;
            }
        }
        self.rows.push(row);
        self.rows.sort_by(compare);
        let mut used = 0_usize;
        let mut keep = 0;
        for row in self.rows.iter().take(self.row_limit) {
            let bytes = retained_bytes(row);
            if bytes > self.byte_limit.saturating_sub(used) {
                break;
            }
            used += bytes;
            keep += 1;
        }
        if keep < self.rows.len() {
            let excluded = self.rows.remove(keep);
            // Move these strings rather than cloning an unusually large key.
            self.cutoff = Some((excluded.recorded_at, excluded.daemon_job_key));
            self.rows.truncate(keep);
        }
    }

    pub(super) fn into_rows(self) -> Vec<DaemonJobRow> {
        self.rows
    }
}
