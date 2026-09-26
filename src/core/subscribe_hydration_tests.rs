//! Real-store regressions for bounded subscription tag hydration.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use super::*;
use crate::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput};

fn audit_head(db: &DbConnection) -> Result<u64, DomainError> {
    let rows = db
        .query("SELECT COALESCE(MAX(rowid), 0) FROM audit_log", &[])
        .map_err(storage_error)?;
    rows.first()
        .and_then(|row| row.get(0).and_then(|value| value.as_i64()))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| malformed_row("fixture cursor"))
}

const LOCAL: &str = "wsp_00000000000000000000000081";
const FOREIGN: &str = "wsp_00000000000000000000000082";
const FIRST: &str = "mem_00000000000000000000000081";
const SECOND: &str = "mem_00000000000000000000000082";

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    writer: DbConnection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary store");
        let workspace = root.path().canonicalize().expect("physical workspace");
        fs::create_dir(workspace.join(".ee")).expect("marker directory");
        let database = workspace.join(".ee/ee.db");
        let writer = DbConnection::open_file(&database).expect("open real store");
        writer.migrate().expect("migrate real schema");
        for (id, path) in [(LOCAL, workspace.clone()), (FOREIGN, workspace.join("other"))] {
            writer
                .insert_workspace(
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
            writer,
        }
    }

    fn memory(&self, id: &str, workspace: &str, tag: &str) {
        self.writer
            .insert_memory(
                id,
                &CreateMemoryInput {
                    workspace_id: workspace.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run the release checks before publishing.".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("manual://subscribe-fixture".to_owned()),
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: None,
                    tags: vec![tag.to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("memory with canonical tags");
    }

    fn audit(
        &self,
        workspace: Option<&str>,
        target: &str,
        target_type: Option<&str>,
        action: &str,
    ) -> u64 {
        self.writer
            .insert_audit(
                &crate::db::generate_audit_id(),
                &CreateAuditInput {
                    workspace_id: workspace.map(str::to_owned),
                    actor: Some("subscription-test".to_owned()),
                    action: action.to_owned(),
                    target_type: target_type.map(str::to_owned),
                    target_id: Some(target.to_owned()),
                    details: None,
                },
            )
            .expect("durable audit");
        audit_head(&self.writer).expect("inserted cursor")
    }

    fn options(&self, cursor: u64, limit: u32, filter: Option<&str>) -> SubscribePollOptions<'_> {
        SubscribePollOptions {
            workspace_path: &self.workspace,
            database_path: Some(&self.database),
            cursor,
            filter: super::super::parse_subscribe_filter(filter).expect("filter"),
            limit,
        }
    }
}

#[test]
fn repeated_memory_events_keep_all_canonical_tags_and_distinct_cursors() {
    let f = Fixture::new();
    f.memory(FIRST, LOCAL, "zulu");
    f.writer
        .execute_raw(&format!(
            "INSERT INTO memory_tags (memory_id, tag) VALUES ('{FIRST}', 'alpha')"
        ))
        .expect("second tag");
    f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_CREATE);
    f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_UPDATE);
    let report = poll_memory_deltas(&f.options(0, 100, Some("TAG=alpha+zulu"))).unwrap();
    assert_eq!(report.delta_count, 2);
    assert!(report.deltas.iter().all(|d| d.tags == ["alpha", "zulu"]));
    assert!(report.deltas[0].cursor < report.deltas[1].cursor);
    assert!(!report.has_more);
}

#[test]
fn hydration_crosses_the_bulk_parameter_batch_boundary_without_losing_a_memory() {
    let f = Fixture::new();
    for index in 0..257 {
        let id = format!("mem_{:026}", 1_000 + index);
        f.memory(&id, LOCAL, "release");
        f.audit(Some(LOCAL), &id, Some("memory"), audit_actions::MEMORY_CREATE);
    }
    let report = poll_memory_deltas(&f.options(0, 300, Some("TAG=release"))).unwrap();
    assert_eq!(report.delta_count, 257);
    assert!(report.deltas.iter().all(|d| d.tags == ["release"]));
    assert_eq!(report.next_cursor, report.high_watermark);
    assert!(!report.has_more);
}

// A real schema fault makes an unintended tag lookup observable without
// mocking the database or counting calls to a fake bulk-read implementation.
fn hide_tags(f: &Fixture) {
    f.writer
        .execute_raw("ALTER TABLE memory_tags RENAME TO subscription_hidden_tags")
        .expect("temporarily hide the tag table");
}

fn restore_tags(f: &Fixture) {
    f.writer
        .execute_raw("ALTER TABLE subscription_hidden_tags RENAME TO memory_tags")
        .expect("restore the tag table");
}

#[test]
fn metadata_excluded_pages_do_not_query_tags_and_still_acknowledge_scanned_rows() {
    let f = Fixture::new();
    f.memory(FIRST, LOCAL, "release");
    let cursor = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_CREATE);
    hide_tags(&f);
    let report = poll_memory_deltas(&f.options(0, 100, Some("LEVEL=episodic")))
        .expect("excluded metadata never requires tag storage");
    assert!(report.deltas.is_empty());
    assert_eq!(report.next_cursor, cursor);
    assert!(!report.has_more);
}

#[test]
fn a_failed_bulk_read_withholds_the_page_and_allows_retry_from_the_previous_cursor() {
    let f = Fixture::new();
    f.memory(FIRST, LOCAL, "release");
    let cursor = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_CREATE);
    hide_tags(&f);
    let options = f.options(0, 100, Some("TAG=release"));
    let error = poll_memory_deltas(&options).expect_err("tag lookup must fail closed");
    assert!(!format!("{error:?}").contains("subscription_hidden_tags"));
    assert!(!format!("{error:?}").contains(FIRST));
    restore_tags(&f);
    let report = poll_memory_deltas(&options).expect("the failed poll did not consume the cursor");
    assert_eq!(report.delta_count, 1);
    assert_eq!(report.next_cursor, cursor);
    assert_eq!(report.deltas[0].tags, ["release"]);
}

#[test]
fn metadata_prefilter_preserves_empty_page_has_more_and_does_not_skip_the_next_match() {
    let f = Fixture::new();
    f.memory(FIRST, LOCAL, "release");
    f.memory(SECOND, LOCAL, "release");
    f.writer
        .execute_raw(&format!("UPDATE memories SET level = 'semantic' WHERE id = '{FIRST}'"))
        .expect("first memory outside the requested level");
    let first = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_CREATE);
    let second = f.audit(Some(LOCAL), SECOND, Some("memory"), audit_actions::MEMORY_CREATE);
    let report = poll_memory_deltas(&f.options(0, 1, Some("LEVEL=procedural,TAG=release")))
        .unwrap();
    assert!(report.deltas.is_empty());
    assert!(report.has_more);
    assert_eq!(report.next_cursor, first);
    let report = poll_memory_deltas(&f.options(first, 1, Some("LEVEL=procedural,TAG=release")))
        .unwrap();
    assert_eq!(report.delta_count, 1);
    assert_eq!(report.deltas[0].memory_id, SECOND);
    assert_eq!(report.deltas[0].tags, ["release"]);
    assert_eq!(report.next_cursor, second);
    assert!(!report.has_more);
}

#[test]
fn bulk_tags_observe_the_same_snapshot_as_audit_metadata_during_a_real_writer_commit() {
    let f = Fixture::new();
    f.memory(FIRST, LOCAL, "release");
    let old = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_CREATE);
    let reader = DbConnection::open(DatabaseConfig::read_only_file(f.database.clone())).unwrap();
    let snapshot = SubscriptionSnapshot::begin(&reader).unwrap();
    assert_eq!(snapshot.high_watermark(LOCAL).unwrap(), old);
    f.writer
        .execute_raw(&format!(
            "UPDATE memory_tags SET tag = 'changed' WHERE memory_id = '{FIRST}'"
        ))
        .expect("concurrent tag commit");
    let new = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_TAG_SET);
    let filter = super::super::parse_subscribe_filter(Some("TAG=release")).unwrap();
    let page = snapshot.page(LOCAL, 0, 100, &filter, None).unwrap();
    assert_eq!(page.deltas.len(), 1);
    assert_eq!(page.deltas[0].tags, ["release"]);
    assert_eq!(page.next_cursor, old);
    assert_eq!(page.high_watermark, old);
    snapshot.finish().unwrap();
    let fresh = poll_memory_deltas(&f.options(old, 100, Some("TAG=changed"))).unwrap();
    assert_eq!(fresh.delta_count, 1);
    assert_eq!(fresh.deltas[0].tags, ["changed"]);
    assert_eq!(fresh.next_cursor, new);
}

#[test]
fn invalid_memory_id_and_lookahead_are_never_hydrated_or_acknowledged_as_a_match() {
    let f = Fixture::new();
    let invalid = f.audit(
        Some(LOCAL),
        "rule_00000000000000000000000081",
        Some("memory"),
        audit_actions::MEMORY_UPDATE,
    );
    f.memory(FIRST, LOCAL, "release");
    f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_CREATE);
    hide_tags(&f);
    let page = poll_memory_deltas(&f.options(0, 1, None))
        .expect("invalid identity and lookahead must not trigger a tag read");
    assert!(page.deltas.is_empty());
    assert!(page.has_more);
    assert_eq!(page.next_cursor, invalid);
    restore_tags(&f);
    let next = poll_memory_deltas(&f.options(page.next_cursor, 1, Some("TAG=release"))).unwrap();
    assert_eq!(next.delta_count, 1);
    assert_eq!(next.deltas[0].memory_id, FIRST);
    assert!(!next.has_more);
}

#[test]
fn metadata_exits_emit_identity_only_invalidations_without_tag_storage() {
    let f = Fixture::new();
    f.memory(FIRST, LOCAL, "private-tag");
    let cursor = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_UPDATE);
    hide_tags(&f);
    let page = poll_memory_deltas(&f.options(0, 100, Some("LEVEL=episodic,TAG=release")))
        .expect("metadata exit needs no tags");
    assert!(page.deltas.is_empty());
    assert_eq!(page.invalidations.len(), 1);
    assert_eq!(page.invalidations[0].cursor, cursor);
    assert_eq!(page.invalidations[0].memory_id, FIRST);
    assert_eq!(page.invalidations[0].affected_filters, ["levels", "tags"]);
    assert_eq!(page.next_cursor, cursor);
    let public = serde_json::to_value(&page.invalidations[0]).expect("notice JSON");
    for field in ["tags", "levels", "kinds", "trustClass", "agentName", "content"] {
        assert!(public.get(field).is_none(), "private field {field}");
    }
    assert!(!public.to_string().contains("private-tag"));
    assert!(!public.to_string().contains("subscription-test"));
}

#[test]
fn tag_and_metadata_exits_keep_audit_order_across_hydration_phases() {
    let f = Fixture::new();
    f.memory(FIRST, LOCAL, "not-release");
    f.memory(SECOND, LOCAL, "release");
    f.writer
        .execute_raw(&format!("UPDATE memories SET level = 'semantic' WHERE id = '{SECOND}'"))
        .expect("metadata exit");
    let first = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_TAG_SET);
    let second = f.audit(Some(LOCAL), SECOND, Some("memory"), audit_actions::MEMORY_UPDATE);
    let third = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_UPDATE);
    let filter = "LEVEL=procedural,TAG=release";
    let page = poll_memory_deltas(&f.options(0, 2, Some(filter))).expect("mixed exits");
    assert!(page.deltas.is_empty());
    assert_eq!(page.invalidations.iter().map(|n| n.cursor).collect::<Vec<_>>(), [first, second]);
    assert_eq!(page.next_cursor, second);
    assert!(page.has_more);
    let next = poll_memory_deltas(&f.options(second, 2, Some(filter))).expect("next page");
    assert!(next.deltas.is_empty());
    assert_eq!(next.invalidations.len(), 1);
    assert_eq!(next.invalidations[0].cursor, third);
    assert_eq!(next.next_cursor, third);
    assert!(!next.has_more);
}

#[test]
fn metadata_invalidation_is_not_acknowledged_when_a_later_tag_batch_fails() {
    let f = Fixture::new();
    f.memory(FIRST, LOCAL, "release");
    f.memory(SECOND, LOCAL, "release");
    f.writer
        .execute_raw(&format!("UPDATE memories SET level = 'semantic' WHERE id = '{FIRST}'"))
        .expect("metadata exit");
    let exit = f.audit(Some(LOCAL), FIRST, Some("memory"), audit_actions::MEMORY_UPDATE);
    let matched = f.audit(Some(LOCAL), SECOND, Some("memory"), audit_actions::MEMORY_UPDATE);
    hide_tags(&f);
    let options = f.options(0, 100, Some("LEVEL=procedural,TAG=release"));
    assert!(poll_memory_deltas(&options).is_err(), "no partial page acknowledgement");
    restore_tags(&f);
    let page = poll_memory_deltas(&options).expect("retry includes both events");
    assert_eq!(page.invalidations.len(), 1);
    assert_eq!(page.invalidations[0].cursor, exit);
    assert_eq!(page.deltas.len(), 1);
    assert_eq!(page.deltas[0].cursor, matched);
    assert_eq!(page.next_cursor, matched);
}
