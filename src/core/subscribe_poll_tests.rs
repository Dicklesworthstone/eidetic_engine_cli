use super::*;
use crate::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput};
use std::path::PathBuf;

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    db: DbConnection,
    own: String,
    other: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let physical = root.path().canonicalize().expect("physical root");
        let workspace = physical.join("one");
        let other_path = physical.join("two");
        std::fs::create_dir_all(workspace.join(".ee")).expect("workspace");
        std::fs::create_dir_all(&other_path).expect("second workspace");
        let database = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&database).expect("open");
        db.migrate().expect("migrate fixture");
        let own = stable_workspace_id(&workspace);
        let other = stable_workspace_id(&other_path);
        for (id, path) in [(&own, &workspace), (&other, &other_path)] {
            db.insert_workspace(
                id,
                &CreateWorkspaceInput {
                    path: path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .expect("workspace binding");
        }
        Self {
            _root: root,
            workspace,
            database,
            db,
            own,
            other,
        }
    }

    fn memory(&self, workspace: &str, number: u128, tag: &str) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: workspace.to_owned(),
                    content: "Run the release checks before publication.".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    provenance_uri: Some("manual://subscribe-fixture".to_owned()),
                    tags: vec![tag.to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("memory");
        id
    }

    fn audit(
        &self,
        workspace: Option<&str>,
        target: Option<&str>,
        kind: Option<&str>,
        action: &str,
    ) -> u64 {
        self.db
            .insert_audit(
                &crate::db::generate_audit_id(),
                &CreateAuditInput {
                    workspace_id: workspace.map(str::to_owned),
                    actor: Some("subscription-test".to_owned()),
                    action: action.to_owned(),
                    target_type: kind.map(str::to_owned),
                    target_id: target.map(str::to_owned),
                    details: None,
                },
            )
            .expect("audit");
        let rows = self
            .db
            .query("SELECT MAX(rowid) FROM audit_log", &[])
            .expect("cursor");
        rows[0]
            .get(0)
            .and_then(|value| value.as_i64())
            .expect("rowid") as u64
    }

    fn poll(&self, cursor: u64, limit: u32, filter: SubscribeFilter) -> SubscribePollReport {
        poll_memory_deltas(&SubscribePollOptions {
            workspace_path: &self.workspace,
            database_path: None,
            cursor,
            filter,
            limit,
        })
        .expect("poll")
    }
}

#[test]
fn subscription_is_workspace_bound_even_in_a_shared_database() {
    let f = Fixture::new();
    let local = f.memory(&f.own, 1, "release");
    let foreign = f.memory(&f.other, 2, "private-project");
    let local_cursor = f.audit(
        Some(&f.own),
        Some(&local),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    f.audit(
        Some(&f.other),
        Some(&foreign),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    // Neither side of a mismatched audit/source join grants workspace authority.
    f.audit(
        Some(&f.own),
        Some(&foreign),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    f.audit(
        Some(&f.other),
        Some(&local),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    f.audit(
        Some(&f.own),
        Some(&local),
        Some("rule"),
        audit_actions::TRUST_CLASS_TRANSITION,
    );
    let local_tail = f.audit(Some(&f.own), None, None, "index.rebuild");
    f.audit(Some(&f.other), None, None, "index.rebuild");
    let report = f.poll(0, 100, SubscribeFilter::default());
    assert_eq!(report.deltas.len(), 1);
    assert_eq!(report.deltas[0].memory_id, local);
    assert_eq!(report.deltas[0].cursor, local_cursor);
    assert_eq!(
        report.deltas[0].workspace_id.as_deref(),
        Some(f.own.as_str())
    );
    assert_eq!(report.high_watermark, local_tail);
    assert_eq!(report.next_cursor, local_tail);
    assert!(!report.has_more);
    assert!(!report.data_json().to_string().contains("private-project"));

    let mut filter = SubscribeFilter::default();
    filter.workspace_ids.insert(f.other.clone());
    let narrowed = f.poll(0, 100, filter);
    assert!(
        narrowed.deltas.is_empty(),
        "filters cannot broaden workspace scope"
    );
}

#[test]
fn legacy_audits_need_a_local_source_and_nonmemory_identities_are_not_emitted() {
    let f = Fixture::new();
    let local = f.memory(&f.own, 1, "release");
    let foreign = f.memory(&f.other, 2, "other");
    f.audit(None, Some(&local), None, audit_actions::MEMORY_CREATE);
    f.audit(None, Some(&foreign), None, audit_actions::MEMORY_CREATE);
    f.audit(Some(&f.own), Some(&f.own), Some("memory"), "memory.scope");
    let report = f.poll(0, 100, SubscribeFilter::default());
    assert_eq!(report.delta_count, 1);
    assert_eq!(report.deltas[0].memory_id, local);
    assert_eq!(
        report.deltas[0].workspace_id.as_deref(),
        Some(f.own.as_str())
    );
}

#[test]
fn absent_memory_does_not_hide_an_owned_tombstone_invalidation() {
    let f = Fixture::new();
    let missing = MemoryId::from_uuid(uuid::Uuid::from_u128(123)).to_string();
    f.audit(
        Some(&f.own),
        Some(&missing),
        Some("memory"),
        audit_actions::MEMORY_TOMBSTONE,
    );
    let report = f.poll(0, 100, SubscribeFilter::default());
    assert_eq!(report.delta_count, 1);
    assert_eq!(report.deltas[0].memory_id, missing);
    assert_eq!(report.deltas[0].kind, "tombstoned");
    assert!(report.deltas[0].tags.is_empty());
}

#[test]
fn filtered_empty_pages_keep_the_lookahead_and_advance_through_nonmemory_tail() {
    let f = Fixture::new();
    let mut cursors = Vec::new();
    let mut ids = Vec::new();
    for number in 1..=5 {
        let id = f.memory(
            &f.own,
            number,
            if number <= 2 { "other" } else { "release" },
        );
        cursors.push(f.audit(
            Some(&f.own),
            Some(&id),
            Some("memory"),
            audit_actions::MEMORY_CREATE,
        ));
        ids.push(id);
    }
    let tail = f.audit(Some(&f.own), None, None, "index.rebuild");
    let filter = super::super::parse_subscribe_filter(Some("TAG=release")).expect("filter");
    let first = f.poll(0, 2, filter.clone());
    assert!(first.deltas.is_empty());
    assert!(first.has_more, "empty filtered page is not end-of-stream");
    assert_eq!(first.next_cursor, cursors[1]);
    assert_eq!(first.data_json()["hasMore"], true);
    let second = f.poll(first.next_cursor, 2, filter.clone());
    assert!(second.has_more);
    assert_eq!(
        second
            .deltas
            .iter()
            .map(|d| &d.memory_id)
            .collect::<Vec<_>>(),
        vec![&ids[2], &ids[3]]
    );
    assert_eq!(second.next_cursor, cursors[3]);
    let third = f.poll(second.next_cursor, 2, filter.clone());
    assert!(!third.has_more);
    assert_eq!(third.deltas[0].memory_id, ids[4]);
    assert_eq!(third.next_cursor, tail);
    let empty = f.poll(third.next_cursor, 2, filter);
    assert!(empty.deltas.is_empty());
    assert_eq!(empty.next_cursor, tail);
}

#[test]
fn exact_full_page_and_irrelevant_only_page_reach_the_observed_watermark() {
    let f = Fixture::new();
    let tail = f.audit(Some(&f.own), None, None, "index.rebuild");
    let empty = f.poll(0, 2, SubscribeFilter::default());
    assert_eq!(empty.next_cursor, tail);
    assert!(!empty.has_more);
    for number in 1..=2 {
        let id = f.memory(&f.own, number, "release");
        f.audit(
            Some(&f.own),
            Some(&id),
            Some("memory"),
            audit_actions::MEMORY_CREATE,
        );
    }
    let full = f.poll(tail, 2, SubscribeFilter::default());
    assert_eq!(full.delta_count, 2);
    assert!(!full.has_more);
    assert_eq!(full.next_cursor, full.high_watermark);
    let id = f.memory(&f.own, 3, "release");
    f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    let next = f.poll(full.next_cursor, 2, SubscribeFilter::default());
    assert_eq!(next.delta_count, 1);
    assert_eq!(next.deltas[0].memory_id, id);
}

#[test]
fn subscription_snapshot_pins_watermark_rows_and_tags_across_real_writes() {
    let f = Fixture::new();
    let id = f.memory(&f.own, 1, "release");
    let before = f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    let reader =
        DbConnection::open(DatabaseConfig::read_only_file(f.database.clone())).expect("reader");
    let snapshot = SubscriptionSnapshot::begin(&reader).expect("snapshot");
    assert_eq!(snapshot.high_watermark(&f.own).expect("pin"), before);
    f.db.execute_raw(&format!(
        "UPDATE memories SET kind = 'decision' WHERE id = '{id}'"
    ))
    .expect("change kind");
    f.db.execute_raw(&format!(
        "UPDATE memory_tags SET tag = 'other' WHERE memory_id = '{id}'"
    ))
    .expect("change tag");
    f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_UPDATE,
    );
    let page = snapshot
        .page(&f.own, 0, 100, &SubscribeFilter::default(), None)
        .expect("pinned page");
    assert_eq!(page.high_watermark, before);
    assert_eq!(page.deltas.len(), 1);
    assert_eq!(page.deltas[0].kinds, vec!["rule".to_owned()]);
    assert_eq!(page.deltas[0].tags, vec!["release".to_owned()]);
    snapshot.finish().expect("release");
    let fresh = f.poll(before, 100, SubscribeFilter::default());
    assert_eq!(fresh.delta_count, 1);
    assert_eq!(fresh.deltas[0].kinds, vec!["decision".to_owned()]);
    assert_eq!(fresh.deltas[0].tags, vec!["other".to_owned()]);
}

#[test]
fn dropped_subscription_snapshot_releases_only_its_own_transaction() {
    let f = Fixture::new();
    let outer = SubscriptionSnapshot::begin(&f.db).expect("outer snapshot");
    assert!(SubscriptionSnapshot::begin(&f.db).is_err());
    outer
        .finish()
        .expect("nested failure did not roll back outer");
    {
        let _early_return = SubscriptionSnapshot::begin(&f.db).expect("snapshot");
    }
    f.db.begin_read_snapshot().expect("released on drop");
    f.db.rollback_read_snapshot()
        .expect("release test snapshot");
}

#[test]
fn polling_never_creates_or_migrates_a_store() {
    let f = Fixture::new();
    let missing = f.workspace.join("missing.db");
    let options = SubscribePollOptions {
        workspace_path: &f.workspace,
        database_path: Some(&missing),
        cursor: 0,
        filter: SubscribeFilter::default(),
        limit: 100,
    };
    assert!(poll_memory_deltas(&options).is_err());
    assert!(!missing.exists());
    let blank = f.workspace.join("unmigrated.db");
    let db = DbConnection::open_file(&blank).expect("blank database");
    let before = db.list_user_tables().expect("before tables");
    let options = SubscribePollOptions {
        database_path: Some(&blank),
        ..options
    };
    assert!(poll_memory_deltas(&options).is_err());
    assert_eq!(db.list_user_tables().expect("after tables"), before);
}

#[test]
fn stale_unsigned_cursor_has_explicit_resynchronization_without_overflow() {
    let f = Fixture::new();
    let id = f.memory(&f.own, 1, "release");
    let cursor = f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    let report = f.poll(u64::MAX, u32::MAX, SubscribeFilter::default());
    assert!(report.deltas.is_empty());
    assert!(!report.has_more);
    assert_eq!(report.next_cursor, cursor);
    assert_eq!(report.degraded.len(), 1);
    assert_eq!(report.degraded[0].code, SUBSCRIBE_CURSOR_STALE);
    assert!(report.degraded[0].repair.contains("Resynchronize"));
}

#[test]
fn filtered_tag_removal_invalidates_the_previously_visible_identity() {
    let f = Fixture::new();
    let id = f.memory(&f.own, 1, "release");
    f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    let filter = super::super::parse_subscribe_filter(Some("TAG=release")).expect("filter");
    let first = f.poll(0, 100, filter.clone());
    assert_eq!(first.delta_count, 1);
    assert!(first.invalidations.is_empty());
    f.db.execute_raw(&format!(
        "UPDATE memory_tags SET tag = 'other' WHERE memory_id = '{id}'"
    ))
    .expect("remove membership");
    f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_TAG_REMOVE,
    );
    let next = f.poll(first.next_cursor, 100, filter.clone());
    assert!(next.deltas.is_empty(), "new state is not a filter match");
    assert_eq!(next.invalidations.len(), 1);
    assert_eq!(next.invalidations[0].memory_id, id);
    assert_eq!(next.data_json()["invalidationCount"], 1);
    let again = f.poll(next.next_cursor, 100, filter);
    assert!(again.deltas.is_empty());
    assert!(
        again.invalidations.is_empty(),
        "invalidation was acknowledged exactly once"
    );
}

#[test]
fn filtered_trust_downgrade_never_leaves_a_silent_stale_trusted_entry() {
    let f = Fixture::new();
    let id = f.memory(&f.own, 1, "release");
    f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    let filter =
        super::super::parse_subscribe_filter(Some("TRUST_CLASS=human_explicit")).expect("filter");
    let first = f.poll(0, 100, filter.clone());
    assert_eq!(first.delta_count, 1);
    f.db.execute_raw(&format!(
        "UPDATE memories SET trust_class = 'legacy_import' WHERE id = '{id}'"
    ))
    .expect("downgrade");
    f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::TRUST_CLASS_TRANSITION,
    );
    let next = f.poll(first.next_cursor, 100, filter);
    assert!(next.deltas.is_empty());
    assert_eq!(next.invalidations.len(), 1);
    assert_eq!(next.invalidations[0].memory_id, id);
    assert_eq!(next.invalidations[0].affected_filters, vec!["trustClass"]);
}

#[test]
fn mixed_delta_and_invalidation_pages_share_one_lossless_cursor() {
    let f = Fixture::new();
    let missing = MemoryId::from_uuid(uuid::Uuid::from_u128(1)).to_string();
    let eviction = f.audit(
        Some(&f.own),
        Some(&missing),
        Some("memory"),
        audit_actions::MEMORY_TOMBSTONE,
    );
    let matching = f.memory(&f.own, 2, "release");
    let created = f.audit(
        Some(&f.own),
        Some(&matching),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    let filter = super::super::parse_subscribe_filter(Some("TAG=release")).expect("filter");
    let first = f.poll(0, 1, filter.clone());
    assert_eq!(first.next_cursor, eviction);
    assert!(first.has_more);
    assert!(first.deltas.is_empty());
    assert_eq!(first.invalidations[0].memory_id, missing);
    let second = f.poll(first.next_cursor, 1, filter);
    assert_eq!(second.next_cursor, created);
    assert!(!second.has_more);
    assert_eq!(second.deltas[0].memory_id, matching);
    assert!(second.invalidations.is_empty());
}

#[test]
fn invalid_time_filters_fail_instead_of_disabling_the_cutoff() {
    let now = DateTime::parse_from_rfc3339("2026-09-26T00:00:00Z")
        .expect("now")
        .with_timezone(&Utc);
    for milliseconds in [-1, i64::MAX] {
        let filter = SubscribeFilter {
            since_ms: Some(milliseconds),
            ..SubscribeFilter::default()
        };
        assert!(matches!(
            subscription_cutoff(&filter, now),
            Err(DomainError::UsageCodeWithDetails {
                code: super::super::SUBSCRIBE_FILTER_INVALID,
                ..
            })
        ));
    }
    let filter = SubscribeFilter {
        since_ms: Some(1_000),
        ..SubscribeFilter::default()
    };
    assert_eq!(
        subscription_cutoff(&filter, now).expect("cutoff"),
        Some(now - TimeDelta::seconds(1))
    );
    assert_eq!(
        subscription_cutoff(&SubscribeFilter::default(), now).expect("no cutoff"),
        None
    );
}

#[test]
fn fetching_a_notified_memory_does_not_start_a_subscription_feedback_loop() {
    use crate::core::memory::{GetMemoryOptions, get_memory_details};

    let f = Fixture::new();
    let id = f.memory(&f.own, 1, "release");
    let created = f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_CREATE,
    );
    let first = f.poll(0, 1, SubscribeFilter::default());
    assert_eq!(first.delta_count, 1);
    assert_eq!(first.next_cursor, created);
    let original = f.db.get_memory(&id).expect("source").expect("present");
    let mut cursor = first.next_cursor;
    for expected_reads in 1..=3 {
        // Exercise the actual public read producer, not a fabricated show row.
        let shown = get_memory_details(&GetMemoryOptions {
            database_path: &f.database,
            memory_id: &id,
            include_tombstoned: false,
        });
        assert!(shown.found && shown.error.is_none(), "{shown:?}");
        let reads =
            f.db.query(
                "SELECT COUNT(*), MAX(rowid) FROM audit_log WHERE action = ?1 AND target_id = ?2",
                &[
                    SqlValue::Text(audit_actions::MEMORY_SHOW.to_owned()),
                    SqlValue::Text(id.clone()),
                ],
            )
            .expect("actual read audits");
        assert_eq!(
            reads[0].get(0).and_then(SqlValue::as_i64),
            Some(expected_reads)
        );
        let read_cursor = u64::try_from(
            reads[0]
                .get(1)
                .and_then(SqlValue::as_i64)
                .expect("read cursor"),
        )
        .expect("positive cursor");
        assert!(
            read_cursor > cursor,
            "the producer must really have emitted its audit"
        );
        let next = f.poll(cursor, 1, SubscribeFilter::default());
        assert!(next.deltas.is_empty() && next.invalidations.is_empty());
        assert!(!next.has_more);
        assert_eq!(next.next_cursor, read_cursor);
        assert_eq!(next.high_watermark, read_cursor);
        cursor = next.next_cursor;
    }
    assert_eq!(
        f.db.get_memory(&id)
            .expect("source after reads")
            .expect("present")
            .content,
        original.content
    );
    let changed = f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_TAG_SET,
    );
    let next = f.poll(cursor, 1, SubscribeFilter::default());
    assert_eq!(next.delta_count, 1, "real changes still notify");
    assert_eq!(next.deltas[0].cursor, changed);
    assert_eq!(next.deltas[0].changed_fields, ["tags"]);
}

#[test]
fn observation_prefixes_and_tails_do_not_consume_change_page_slots() {
    let f = Fixture::new();
    let id = f.memory(&f.own, 1, "release");
    for _ in 0..5 {
        for action in OBSERVATION_ONLY_ACTIONS {
            f.audit(Some(&f.own), Some(&id), Some("memory"), action);
        }
    }
    let first = f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_TAG_ADD,
    );
    f.audit(None, Some(&id), None, audit_actions::MEMORY_SHOW);
    let second = f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_UPDATE,
    );
    let tail = f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        "memory.reveal_failed",
    );
    let page = f.poll(0, 1, SubscribeFilter::default());
    assert_eq!(page.delta_count, 1);
    assert_eq!(page.deltas[0].cursor, first);
    assert_eq!(
        page.next_cursor, first,
        "never acknowledge the second change as lookahead"
    );
    assert!(page.has_more);
    assert_eq!(page.high_watermark, tail);
    let next = f.poll(first, 1, SubscribeFilter::default());
    assert_eq!(next.delta_count, 1);
    assert_eq!(next.deltas[0].cursor, second);
    assert_eq!(next.next_cursor, tail);
    assert!(!next.has_more, "read-only tail is not more change work");
    let replay = f.poll(tail, 1, SubscribeFilter::default());
    assert!(replay.deltas.is_empty() && replay.invalidations.is_empty());
    assert_eq!(replay.next_cursor, tail);
}

#[test]
fn observations_neither_invalidate_filtered_membership_nor_require_tag_storage() {
    let f = Fixture::new();
    let id = f.memory(&f.own, 1, "private-tag");
    let mut tail = 0;
    for action in OBSERVATION_ONLY_ACTIONS {
        tail = f.audit(Some(&f.own), Some(&id), Some("memory"), action);
    }
    f.db.execute_raw("ALTER TABLE memory_tags RENAME TO subscription_observation_tags")
        .expect("make an unintended hydration observable");
    for expression in [
        None,
        Some("TAG=release"),
        Some("LEVEL=episodic,TAG=release"),
    ] {
        let filter = super::super::parse_subscribe_filter(expression).expect("filter");
        let page = f.poll(0, 1, filter);
        assert!(page.deltas.is_empty() && page.invalidations.is_empty());
        assert_eq!(page.next_cursor, tail);
        assert!(!page.has_more);
    }
    let rows =
        f.db.query("SELECT COUNT(*) FROM audit_log", &[])
            .expect("history remains");
    assert_eq!(
        rows[0].get(0).and_then(SqlValue::as_i64),
        Some(OBSERVATION_ONLY_ACTIONS.len() as i64)
    );
}

#[test]
fn observation_exclusion_is_exact_and_unknown_mutations_remain_visible() {
    let f = Fixture::new();
    let id = f.memory(&f.own, 1, "release");
    f.audit(
        Some(&f.own),
        Some(&id),
        Some("memory"),
        audit_actions::MEMORY_SHOW,
    );
    let mut expected = Vec::new();
    for action in [
        "memory.show.changed",
        "memory.reveal_failed_recovered",
        "future.reclassify",
        audit_actions::MEMORY_UPDATE,
    ] {
        expected.push(f.audit(Some(&f.own), Some(&id), Some("memory"), action));
    }
    let page = f.poll(0, 100, SubscribeFilter::default());
    assert_eq!(
        page.deltas
            .iter()
            .map(|delta| delta.cursor)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(!page.has_more);
    assert_eq!(page.next_cursor, page.high_watermark);
}
