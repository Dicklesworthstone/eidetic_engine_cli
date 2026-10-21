//! Resumable JSONL delivery of complete subscription audit units.
//!
//! A cursor is acknowledged only after its entire frame has been written and
//! flushed. A link or revision may expand one audit into multiple notices; those
//! travel together rather than exposing a resumable cursor on a partial unit.

use std::io::Write;
use std::time::{Duration, Instant};

use asupersync::Cx;
use serde::Serialize;

use crate::core::subscribe::{
    MemoryDelta, MemoryInvalidation, SubscribePollOptions, SubscribePollReport,
};
use crate::models::DomainError;

pub const CHANGE_SCHEMA_V1: &str = "ee.subscribe.change.v1";
pub const CHECKPOINT_SCHEMA_V1: &str = "ee.subscribe.checkpoint.v1";

#[derive(Clone, Debug)]
pub struct StreamOptions<'a> {
    pub poll: SubscribePollOptions<'a>,
    pub interval: Duration,
    /// Complete audit-change frames, not expanded endpoint notices. Checkpoint
    /// frames do not count. Zero performs no poll or output operation.
    pub max_events: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StreamProgress {
    pub next_cursor: u64,
    pub events_emitted: u64,
}

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
enum Notice<'a> {
    Invalidation(&'a MemoryInvalidation),
    Delta(&'a MemoryDelta),
}

impl<'a> Notice<'a> {
    fn cursor(self) -> u64 {
        match self {
            Self::Invalidation(value) => value.cursor,
            Self::Delta(value) => value.cursor,
        }
    }

    fn identity(self) -> &'a str {
        match self {
            Self::Invalidation(value) => &value.memory_id,
            Self::Delta(value) => &value.memory_id,
        }
    }

    fn workspace(self) -> Option<&'a str> {
        match self {
            Self::Invalidation(value) => value.workspace_id.as_deref(),
            Self::Delta(value) => value.workspace_id.as_deref(),
        }
    }

    fn audit(self) -> &'a str {
        match self {
            Self::Invalidation(value) => &value.audit_id,
            Self::Delta(value) => &value.audit_id,
        }
    }

    fn order(self) -> u8 {
        match self {
            Self::Invalidation(_) => 0,
            Self::Delta(_) => 1,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Change<'a> {
    schema: &'static str,
    cursor: u64,
    workspace_id: &'a str,
    events: &'a [Notice<'a>],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Checkpoint<'a> {
    schema: &'static str,
    cursor: u64,
    workspace_id: &'a str,
}

fn delivery_error() -> DomainError {
    DomainError::Storage {
        message: "Subscription delivery stopped before acknowledging the next complete frame."
            .to_owned(),
        repair: Some(
            "Discard an incomplete JSON line. Resume from the last complete frame cursor with the same database, workspace and filter. Replayed complete frames must be applied idempotently."
                .to_owned(),
        ),
    }
}

fn checkpoint(cx: Option<&Cx>) -> Result<(), DomainError> {
    if let Some(cx) = cx {
        cx.checkpoint().map_err(|_| delivery_error())?;
    }
    Ok(())
}

fn write_frame(
    cx: Option<&Cx>,
    output: &mut impl Write,
    frame: &impl Serialize,
) -> Result<(), DomainError> {
    let mut bytes = serde_json::to_vec(frame).map_err(|_| delivery_error())?;
    bytes.push(b'\n');
    checkpoint(cx)?;
    output.write_all(&bytes).map_err(|_| delivery_error())?;
    output.flush().map_err(|_| delivery_error())
}

/// Validate the entire source page before writing even its first frame. Do not
/// silently reset a stale cursor or emit an accepted prefix of an invalid page.
fn ordered_notices(
    page: &SubscribePollReport,
    cursor: u64,
) -> Result<Vec<Notice<'_>>, DomainError> {
    if page.cursor != cursor
        || page.high_watermark < cursor
        || page.next_cursor < cursor
        || page.next_cursor > page.high_watermark
        || page.delta_count != page.deltas.len()
        || (page.has_more
            && (page.next_cursor == cursor || page.next_cursor >= page.high_watermark))
        || !page.degraded.is_empty()
    {
        return Err(DomainError::UsageCodeWithDetails {
            code: "subscribe_stream_resync_required",
            message: "Subscription source progress is inconsistent or degraded; the stream was not advanced."
                .to_owned(),
            repair: Some(
                "Reconcile authoritative workspace state before choosing a new cursor. Inspect ee subscribe poll --json with the same workspace, database and filter."
                    .to_owned(),
            ),
            details_json: serde_json::json!({
                "cursor": cursor,
                "highWatermark": page.high_watermark,
            })
            .to_string(),
        });
    }
    let mut notices: Vec<_> = page
        .invalidations
        .iter()
        .map(Notice::Invalidation)
        .chain(page.deltas.iter().map(Notice::Delta))
        .collect();
    if notices.iter().any(|notice| {
        notice.cursor() <= cursor
            || notice.cursor() > page.next_cursor
            || notice.workspace() != Some(page.workspace_id.as_str())
    }) {
        return Err(delivery_error());
    }
    notices.sort_by(|left, right| {
        left.cursor()
            .cmp(&right.cursor())
            .then_with(|| left.order().cmp(&right.order()))
            .then_with(|| left.identity().cmp(right.identity()))
    });
    if notices
        .windows(2)
        .any(|pair| pair[0].cursor() == pair[1].cursor() && pair[0].audit() != pair[1].audit())
    {
        return Err(delivery_error());
    }
    Ok(notices)
}

fn deliver(
    cx: Option<&Cx>,
    output: &mut impl Write,
    page: &SubscribePollReport,
    progress: &mut StreamProgress,
    maximum: Option<u64>,
) -> Result<bool, DomainError> {
    checkpoint(cx)?;
    let notices = ordered_notices(page, progress.next_cursor)?;
    let mut offset = 0;
    while offset < notices.len() {
        let cursor = notices[offset].cursor();
        let count = notices[offset..]
            .iter()
            .take_while(|notice| notice.cursor() == cursor)
            .count();
        let group = &notices[offset..offset + count];
        if count == 1 {
            write_frame(cx, output, &group[0])?;
        } else {
            write_frame(
                cx,
                output,
                &Change {
                    schema: CHANGE_SCHEMA_V1,
                    cursor,
                    workspace_id: &page.workspace_id,
                    events: group,
                },
            )?;
        }
        progress.next_cursor = cursor;
        progress.events_emitted = progress.events_emitted.saturating_add(1);
        if maximum.is_some_and(|maximum| progress.events_emitted >= maximum) {
            return Ok(true);
        }
        offset += count;
    }
    if page.next_cursor > progress.next_cursor {
        write_frame(
            cx,
            output,
            &Checkpoint {
                schema: CHECKPOINT_SCHEMA_V1,
                cursor: page.next_cursor,
                workspace_id: &page.workspace_id,
            },
        )?;
        progress.next_cursor = page.next_cursor;
    }
    Ok(false)
}

fn wait(cx: Option<&Cx>, interval: Duration) -> Result<(), DomainError> {
    let started = Instant::now();
    loop {
        checkpoint(cx)?;
        let remaining = interval.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

/// Stream admitted source changes without retaining a database transaction
/// while waiting or writing. Output is at-least-once across interrupted writes;
/// consumers resume only from a complete JSON line, never a nested event.
pub fn run(
    cx: Option<&Cx>,
    options: &StreamOptions<'_>,
    output: &mut impl Write,
) -> Result<StreamProgress, DomainError> {
    run_with(
        cx,
        options,
        output,
        crate::core::subscribe::poll_memory_deltas,
        wait,
    )
}

fn run_with(
    cx: Option<&Cx>,
    options: &StreamOptions<'_>,
    output: &mut impl Write,
    mut poll: impl FnMut(&SubscribePollOptions<'_>) -> Result<SubscribePollReport, DomainError>,
    mut idle: impl FnMut(Option<&Cx>, Duration) -> Result<(), DomainError>,
) -> Result<StreamProgress, DomainError> {
    let mut progress = StreamProgress {
        next_cursor: options.poll.cursor,
        events_emitted: 0,
    };
    if options.max_events == Some(0) {
        return Ok(progress);
    }
    let mut binding = None;
    loop {
        checkpoint(cx)?;
        let mut request = options.poll.clone();
        request.cursor = progress.next_cursor;
        let page = poll(&request)?;
        let scope = (page.workspace_id.clone(), page.database_path.clone());
        if binding.as_ref().is_some_and(|prior| prior != &scope) {
            return Err(delivery_error());
        }
        binding = Some(scope);
        if deliver(cx, output, &page, &mut progress, options.max_events)? {
            return Ok(progress);
        }
        if !page.has_more {
            idle(cx, options.interval.max(Duration::from_millis(1)))?;
        }
    }
}

#[cfg(test)]
#[path = "subscribe_stream_tests.rs"]
mod tests;
