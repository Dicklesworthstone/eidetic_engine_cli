use super::*;
use crate::core::subscribe::{
    MEMORY_DELTA_SCHEMA_V1, MEMORY_INVALIDATION_SCHEMA_V1, SUBSCRIBE_POLL_SCHEMA_V1,
    SubscribeDegradation,
};
use serde_json::Value;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

fn delta(cursor: u64, name: &str) -> MemoryDelta {
    MemoryDelta {
        schema: MEMORY_DELTA_SCHEMA_V1,
        cursor,
        kind: "updated".to_owned(),
        memory_id: name.to_owned(),
        levels: vec!["procedural".to_owned()],
        kinds: vec!["rule".to_owned()],
        tags: vec!["release".to_owned()],
        workspace_id: Some("workspace".to_owned()),
        trust_class: Some("human_explicit".to_owned()),
        agent_name: None,
        changed_fields: vec!["tags".to_owned()],
        audit_id: format!("audit-{cursor}"),
        occurred_at: "2026-10-21T00:00:00Z".to_owned(),
    }
}

fn invalidation(cursor: u64, name: &str) -> MemoryInvalidation {
    MemoryInvalidation {
        schema: MEMORY_INVALIDATION_SCHEMA_V1,
        cursor,
        memory_id: name.to_owned(),
        workspace_id: Some("workspace".to_owned()),
        audit_id: format!("audit-{cursor}"),
        occurred_at: "2026-10-21T00:00:00Z".to_owned(),
        changed_fields: vec!["links".to_owned()],
        affected_filters: Vec::new(),
        reason: "memory_link_changed",
    }
}

fn page(
    cursor: u64,
    next: u64,
    deltas: Vec<MemoryDelta>,
    invalidations: Vec<MemoryInvalidation>,
) -> SubscribePollReport {
    SubscribePollReport {
        schema: SUBSCRIBE_POLL_SCHEMA_V1,
        command: "subscribe poll",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: "workspace".to_owned(),
        database_path: PathBuf::from("/tmp/subscription.db"),
        cursor,
        next_cursor: next,
        high_watermark: next,
        has_more: false,
        delta_count: deltas.len(),
        deltas,
        invalidations,
        degraded: Vec::new(),
    }
}

fn options(maximum: Option<u64>) -> StreamOptions<'static> {
    StreamOptions {
        poll: SubscribePollOptions::new(Path::new("/tmp/subscription")),
        interval: Duration::from_secs(3600),
        max_events: maximum,
    }
}

fn frames(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .expect("UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("complete frame"))
        .collect()
}

#[test]
fn replay_merges_both_lanes_and_preserves_atomic_cursor_groups() {
    let report = page(
        0,
        9,
        vec![delta(7, "new"), delta(2, "single")],
        vec![
            invalidation(7, "old"),
            invalidation(4, "right"),
            invalidation(4, "left"),
        ],
    );
    let before = report.data_json();
    let mut output = Vec::new();
    let mut progress = StreamProgress::default();
    assert!(!deliver(None, &mut output, &report, &mut progress, None).unwrap());
    let frames = frames(&output);
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame["cursor"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [2, 4, 7, 9]
    );
    assert_eq!(frames[0], serde_json::to_value(&report.deltas[1]).unwrap());
    assert_eq!(frames[1]["schema"], CHANGE_SCHEMA_V1);
    assert_eq!(frames[1]["events"][0]["memoryId"], "left");
    assert_eq!(frames[1]["events"][1]["memoryId"], "right");
    assert_eq!(
        frames[2]["events"][0]["schema"],
        MEMORY_INVALIDATION_SCHEMA_V1
    );
    assert_eq!(frames[2]["events"][1]["schema"], MEMORY_DELTA_SCHEMA_V1);
    assert_eq!(frames[3]["schema"], CHECKPOINT_SCHEMA_V1);
    assert_eq!(
        progress,
        StreamProgress {
            next_cursor: 9,
            events_emitted: 3
        }
    );
    assert_eq!(
        before,
        report.data_json(),
        "delivery cannot mutate source projections"
    );
}

#[test]
fn stopping_at_one_event_never_splits_a_link_or_acknowledges_its_later_tail() {
    let report = page(
        0,
        11,
        vec![delta(9, "later")],
        vec![invalidation(3, "right"), invalidation(3, "left")],
    );
    let mut progress = StreamProgress::default();
    let mut output = Vec::new();
    assert!(deliver(None, &mut output, &report, &mut progress, Some(1)).unwrap());
    let first = frames(&output);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["events"].as_array().unwrap().len(), 2);
    assert_eq!(
        progress,
        StreamProgress {
            next_cursor: 3,
            events_emitted: 1
        }
    );
    let resumed = page(
        progress.next_cursor,
        11,
        vec![delta(9, "later")],
        Vec::new(),
    );
    assert!(!deliver(None, &mut output, &resumed, &mut progress, None).unwrap());
    let all = frames(&output);
    assert_eq!(
        all.iter()
            .map(|frame| frame["cursor"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [3, 9, 11]
    );
    assert_eq!(all[1]["memoryId"], "later");
}

struct FailingWriter {
    bytes: Vec<u8>,
    remaining: usize,
    fail_flush: bool,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Err(std::io::Error::other("PRIVATE_STREAM_SINK"));
        }
        let count = bytes.len().min(self.remaining);
        self.bytes.extend_from_slice(&bytes[..count]);
        self.remaining -= count;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.fail_flush {
            Err(std::io::Error::other("PRIVATE_FLUSH"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn partial_writes_and_flush_failure_never_advance_the_resume_cursor() {
    let report = page(
        5,
        9,
        Vec::new(),
        vec![invalidation(8, "a"), invalidation(8, "b")],
    );
    let initial = StreamProgress {
        next_cursor: 5,
        events_emitted: 0,
    };
    for (remaining, fail_flush) in [(0, false), (13, false), (usize::MAX, true)] {
        let mut progress = initial;
        let mut output = FailingWriter {
            bytes: Vec::new(),
            remaining,
            fail_flush,
        };
        let error = deliver(None, &mut output, &report, &mut progress, None).unwrap_err();
        assert_eq!(progress, initial);
        assert!(!error.to_string().contains("PRIVATE"));
        let mut retry = Vec::new();
        assert!(!deliver(None, &mut retry, &report, &mut progress, None).unwrap());
        let retried = frames(&retry);
        assert_eq!(retried[0]["events"].as_array().unwrap().len(), 2);
        assert_eq!(progress.next_cursor, 9);
    }
}

#[test]
fn inconsistent_or_stale_pages_are_rejected_before_any_frame_is_written() {
    let good = page(5, 9, vec![delta(6, "first"), delta(8, "later")], Vec::new());
    for case in 0..8 {
        let mut report = good.clone();
        match case {
            0 => report.cursor = 0,
            1 => report.high_watermark = 4,
            2 => report.next_cursor = 4,
            3 => report.deltas[1].cursor = 10,
            4 => report.deltas[1].workspace_id = Some("PRIVATE_FOREIGN".to_owned()),
            5 => {
                report.deltas[1].cursor = 6;
                report.deltas[1].audit_id = "other-audit".to_owned();
            }
            6 => report.has_more = true,
            _ => report.degraded.push(SubscribeDegradation {
                code: "PRIVATE_DEGRADATION".to_owned(),
                severity: "warning".to_owned(),
                message: "PRIVATE_MESSAGE".to_owned(),
                repair: "PRIVATE_REPAIR".to_owned(),
            }),
        }
        let mut progress = StreamProgress {
            next_cursor: 5,
            events_emitted: 0,
        };
        let mut output = Vec::new();
        let error = deliver(None, &mut output, &report, &mut progress, None).unwrap_err();
        assert!(output.is_empty(), "case {case}");
        assert_eq!(progress.next_cursor, 5);
        assert!(!error.to_string().contains("PRIVATE"));
    }
}

#[test]
fn filtered_backlog_drains_without_sleep_and_idle_pages_do_not_spin() {
    let mut prefix = page(0, 2, Vec::new(), Vec::new());
    prefix.high_watermark = 9;
    prefix.has_more = true;
    let second = page(
        2,
        9,
        Vec::new(),
        vec![invalidation(5, "a"), invalidation(5, "b")],
    );
    let mut pages = VecDeque::from([prefix, second]);
    let mut output = Vec::new();
    let mut requested = Vec::new();
    let progress = run_with(
        None,
        &options(Some(1)),
        &mut output,
        |request| {
            requested.push(request.cursor);
            Ok(pages.pop_front().expect("bounded page count"))
        },
        |_, _| panic!("known backlog must not sleep"),
    )
    .unwrap();
    assert_eq!(requested, [0, 2]);
    assert_eq!(progress.next_cursor, 5);
    let emitted = frames(&output);
    assert_eq!(emitted.len(), 2);
    assert_eq!(emitted[0]["schema"], CHECKPOINT_SCHEMA_V1);
    let mut idle_count = 0;
    let result = run_with(
        None,
        &options(None),
        &mut Vec::new(),
        |_| Ok(page(0, 0, Vec::new(), Vec::new())),
        |_, interval| {
            idle_count += 1;
            assert_eq!(interval, Duration::from_secs(3600));
            Err(delivery_error())
        },
    );
    assert!(result.is_err());
    assert_eq!(idle_count, 1);
}

#[test]
fn zero_budget_and_changed_store_binding_do_not_read_or_cross_scopes() {
    let mut output = Vec::new();
    let result = run_with(
        None,
        &options(Some(0)),
        &mut output,
        |_| panic!("zero budget must not read storage"),
        |_, _| panic!("zero budget must not wait"),
    )
    .unwrap();
    assert_eq!(result, StreamProgress::default());
    assert!(output.is_empty());
    let mut first = page(0, 2, Vec::new(), Vec::new());
    first.has_more = true;
    first.high_watermark = 8;
    let mut second = page(2, 8, vec![delta(5, "wrong-store")], Vec::new());
    second.database_path = PathBuf::from("/private/changed-store.db");
    let mut pages = VecDeque::from([first, second]);
    assert!(
        run_with(
            None,
            &options(Some(1)),
            &mut output,
            |_| Ok(pages.pop_front().unwrap()),
            |_, _| panic!("no wait"),
        )
        .is_err()
    );
    assert_eq!(frames(&output).len(), 1);
    assert_eq!(frames(&output)[0]["cursor"], 2);
    assert!(!String::from_utf8(output).unwrap().contains("wrong-store"));
}

#[test]
fn explicit_cancellation_stops_before_poll_and_during_idle_wait() {
    let result = crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        cx.cancel_with(asupersync::CancelKind::User, Some("stop subscription"));
        let mut bytes = Vec::new();
        assert!(
            run_with(
                Some(&cx),
                &options(None),
                &mut bytes,
                |_| panic!("cancelled stream must not poll"),
                |_, _| panic!("cancelled stream must not wait"),
            )
            .is_err()
        );
        assert!(wait(Some(&cx), Duration::from_secs(3600)).is_err());
        assert!(bytes.is_empty());
    });
    result.expect("runtime");
}

struct Store {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    db: crate::db::DbConnection,
    id: String,
}

impl Store {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        std::fs::create_dir(workspace.join(".ee")).unwrap();
        let database = workspace.join(".ee/ee.db");
        let db = crate::db::DbConnection::open_file(&database).unwrap();
        db.migrate().unwrap();
        let id = crate::core::workspace::stable_workspace_id(&workspace);
        db.insert_workspace(
            &id,
            &crate::db::CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        Self {
            _root: root,
            workspace,
            database,
            db,
            id,
        }
    }

    fn memory(&self, number: u128) -> String {
        let id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &crate::db::CreateMemoryInput {
                    workspace_id: self.id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("PRIVATE_STREAM_BODY_{number}"),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("manual://stream-test".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["release".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
        id
    }

    fn head(&self) -> u64 {
        let rows = self
            .db
            .query("SELECT COALESCE(MAX(rowid), 0) FROM audit_log", &[])
            .unwrap();
        u64::try_from(
            rows[0]
                .get(0)
                .and_then(sqlmodel_core::Value::as_i64)
                .unwrap(),
        )
        .unwrap()
    }

    fn audit(&self, target: &str, kind: &str, action: &str) -> u64 {
        self.db
            .insert_audit(
                &crate::db::generate_audit_id(),
                &crate::db::CreateAuditInput {
                    workspace_id: Some(self.id.clone()),
                    actor: Some("PRIVATE_STREAM_ACTOR".to_owned()),
                    action: action.to_owned(),
                    target_type: Some(kind.to_owned()),
                    target_id: Some(target.to_owned()),
                    details: None,
                },
            )
            .unwrap();
        self.head()
    }

    fn stream(
        &self,
        cursor: u64,
        filter: Option<&str>,
        maximum: u64,
        output: &mut impl Write,
    ) -> Result<StreamProgress, DomainError> {
        let options = self.options(cursor, filter, maximum);
        crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
            run(Some(&cx), &options, output)
        })
        .expect("bounded stream runtime")
    }

    fn options(&self, cursor: u64, filter: Option<&str>, maximum: u64) -> StreamOptions<'_> {
        StreamOptions {
            poll: SubscribePollOptions {
                workspace_path: &self.workspace,
                database_path: Some(&self.database),
                cursor,
                filter: crate::core::subscribe::parse_subscribe_filter(filter).unwrap(),
                limit: 100,
            },
            interval: Duration::from_millis(1),
            max_events: Some(maximum),
        }
    }
}

#[test]
fn real_link_producer_replays_both_endpoints_as_one_resumable_frame() {
    use crate::core::memory::{MemoryLinkMode, MemoryLinkOptions, update_memory_link};
    use crate::db::{MemoryLinkRelation, MemoryLinkSource};
    let store = Store::new();
    let left = store.memory(1);
    let right = store.memory(2);
    let start = store.head();
    let source = store.db.get_memory(&left).unwrap();
    let linked = update_memory_link(&MemoryLinkOptions {
        workspace_path: &store.workspace,
        database_path: &store.database,
        memory_id: &right,
        mode: MemoryLinkMode::Create {
            target_memory_id: left.clone(),
            relation: MemoryLinkRelation::Supports,
            weight: 1.0,
            confidence: 0.9,
            directed: true,
            evidence_count: 1,
            source: MemoryLinkSource::Human,
            metadata_json: Some("{\"annotation\":\"PRIVATE_STREAM_LINK\"}".to_owned()),
        },
        actor: Some("PRIVATE_STREAM_ACTOR"),
        dry_run: false,
        include_tombstoned: false,
    })
    .unwrap();
    assert!(linked.persisted);
    let mut output = Vec::new();
    let progress = store
        .stream(start, Some("CHANGED_FIELDS=links"), 1, &mut output)
        .unwrap();
    let values = frames(&output);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["schema"], CHANGE_SCHEMA_V1);
    assert_eq!(values[0]["events"].as_array().unwrap().len(), 2);
    assert_eq!(values[0]["events"][0]["memoryId"], left);
    assert_eq!(values[0]["events"][1]["memoryId"], right);
    assert_eq!(progress.next_cursor, store.head());
    assert_eq!(progress.events_emitted, 1);
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("PRIVATE_STREAM")
    );
    assert_eq!(store.db.get_memory(&left).unwrap(), source);
    let again = crate::core::subscribe::poll_memory_deltas(
        &store.options(progress.next_cursor, None, 1).poll,
    )
    .unwrap();
    assert!(again.deltas.is_empty() && again.invalidations.is_empty());
}

#[test]
fn real_filter_exits_are_delivered_and_unavailable_source_pages_emit_no_prefix() {
    let store = Store::new();
    let id = store.memory(1);
    let start = store.head();
    store
        .db
        .execute_raw(&format!(
            "UPDATE memories SET level = 'semantic' WHERE id = '{id}'"
        ))
        .unwrap();
    let cursor = store.audit(
        &id,
        "memory",
        crate::db::audit_actions::MEMORY_LEVEL_TRANSITION,
    );
    let mut bytes = Vec::new();
    let progress = store
        .stream(start, Some("LEVEL=procedural"), 1, &mut bytes)
        .unwrap();
    let notices = frames(&bytes);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["schema"], MEMORY_INVALIDATION_SCHEMA_V1);
    assert_eq!(notices[0]["memoryId"], id);
    assert_eq!(progress.next_cursor, cursor);
    let missing = crate::models::MemoryLinkId::from_uuid(uuid::Uuid::from_u128(900)).to_string();
    store.audit(
        &missing,
        "memory_link",
        crate::db::audit_actions::MEMORY_LINK_CREATE,
    );
    let mut rejected = Vec::new();
    assert!(store.stream(start, None, 2, &mut rejected).is_err());
    assert!(
        rejected.is_empty(),
        "an incomplete source page must not yield a successful prefix"
    );
    let mut stale = Vec::new();
    let error = store
        .stream(store.head() + 1, None, 1, &mut stale)
        .unwrap_err();
    assert!(stale.is_empty());
    assert!(matches!(
        error,
        DomainError::UsageCodeWithDetails {
            code: "subscribe_stream_resync_required",
            ..
        }
    ));
}

#[test]
fn a_write_during_delivery_is_observed_by_the_next_fresh_poll() {
    struct ConcurrentWriter<'a> {
        store: &'a Store,
        memory: &'a str,
        bytes: Vec<u8>,
        new_cursor: Option<u64>,
    }
    impl Write for ConcurrentWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.new_cursor.is_none() {
                self.new_cursor = Some(self.store.audit(
                    self.memory,
                    "memory",
                    crate::db::audit_actions::MEMORY_TAG_SET,
                ));
            }
            Ok(())
        }
    }
    let store = Store::new();
    let id = store.memory(1);
    let before = store.head();
    let first = store.audit(&id, "memory", crate::db::audit_actions::MEMORY_CREATE);
    let mut writer = ConcurrentWriter {
        store: &store,
        memory: &id,
        bytes: Vec::new(),
        new_cursor: None,
    };
    let progress = store.stream(before, None, 2, &mut writer).unwrap();
    let second = writer.new_cursor.unwrap();
    assert!(second > first);
    let events = frames(&writer.bytes);
    assert_eq!(
        events
            .iter()
            .map(|event| event["cursor"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [first, second]
    );
    assert_eq!(progress.next_cursor, second);
    assert_eq!(progress.events_emitted, 2);
}
