//! Real source snapshots: no timing sleeps, fake storage, or shortened history.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, MemoryLinkSource};

struct Fixture {
    db: DbConnection,
    root: tempfile::TempDir,
    workspace: String,
}

fn clock() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        std::fs::create_dir(path.join(".ee")).unwrap();
        let db = DbConnection::open_file(path.join(".ee/ee.db")).unwrap();
        db.migrate().unwrap();
        let workspace = stable_workspace_id(&path);
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        Self {
            db,
            root,
            workspace,
        }
    }

    fn scope(&self) -> DecideScope {
        decide_scope(self.root.path(), None, false).unwrap()
    }

    fn reader(&self) -> DbConnection {
        DbConnection::open_file_read_only(self.scope().database_path).unwrap()
    }

    fn seed(&self, number: usize, kind: &str, chosen: &str) -> String {
        let id = format!("mem_{number:026}");
        let body = format!(
            "Topic: Storage backend\nOptions: row store, column store\nChosen: {chosen}\nRationale: Keep durable decisions."
        );
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace.clone(),
                    content: body,
                    level: "semantic".to_owned(),
                    kind: kind.to_owned(),
                    workflow_id: None,
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("manual://decision-read".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
        if kind == "decision" {
            self.fields(&id, chosen);
        }
        id
    }

    fn fields(&self, id: &str, chosen: &str) {
        self.db
            .set_memory_typed_fields_json(
                id,
                Some(
                    &json!({
                        "options": ["row store", "column store"],
                        "chosen": chosen,
                        "rationale": "Keep durable decisions.",
                        "revisit_by": "2030-01-02T00:00:00Z",
                    })
                    .to_string(),
                ),
            )
            .unwrap();
    }

    fn read(&self, history: bool) -> Vec<DecideItem> {
        load(&mut self.scope(), history, clock()).unwrap()
    }
}

fn released(reader: &DbConnection) {
    reader.begin_read_snapshot().unwrap();
    reader.commit_read_snapshot().unwrap();
}

#[test]
fn concurrent_replacement_cannot_mix_an_old_head_with_new_revision_and_lineage() {
    let fixture = Fixture::new();
    let old = fixture.seed(1, "decision", "row store");
    let reader = fixture.reader();
    let mut replacement = None;
    let captured = read_with_observer(&reader, &mut fixture.scope(), false, clock(), |_| {
        fixture
            .db
            .with_transaction(|| {
                let new = fixture.seed(2, "decision", "column store");
                fixture.db.insert_memory_link(
                    "link_00000000000000000000000001",
                    &CreateMemoryLinkInput {
                        src_memory_id: new.clone(),
                        dst_memory_id: old.clone(),
                        relation: MemoryLinkRelation::Supersedes,
                        weight: 1.0,
                        confidence: 1.0,
                        directed: true,
                        evidence_count: 1,
                        last_reinforced_at: None,
                        source: MemoryLinkSource::Human,
                        created_by: None,
                        metadata_json: None,
                    },
                )?;
                fixture
                    .db
                    .expire_memory_valid_to(&old, "2026-01-01T00:00:00Z")?;
                fixture
                    .db
                    .mark_memory_superseded(&old, "2026-01-01T00:00:00Z")?;
                replacement = Some(new);
                Ok(())
            })
            .unwrap();
        Ok(())
    })
    .unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].memory_id, old);
    assert_eq!(captured[0].chosen, "row store");
    assert!(!captured[0].superseded);
    assert!(captured[0].valid_to.is_none());
    assert_eq!(captured[0].chain_depth, 0);
    released(&reader);
    let next = fixture.read(false);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].memory_id, replacement.unwrap());
    assert_eq!(next[0].chosen, "column store");
    assert_eq!(next[0].chain_depth, 1);
    assert_eq!(fixture.read(true).len(), 2);
}

#[test]
fn body_and_exact_typed_fields_are_not_torn_by_a_concurrent_update() {
    let fixture = Fixture::new();
    let id = fixture.seed(1, "decision", "row store");
    let reader = fixture.reader();
    let captured = read_with_observer(&reader, &mut fixture.scope(), false, clock(), |_| {
        fixture.fields(&id, "column store");
        Ok(())
    })
    .unwrap();
    assert_eq!(captured[0].chosen, "row store");
    assert_eq!(fixture.read(false)[0].chosen, "column store");
}

#[test]
fn complete_paging_hydrates_only_decisions_and_keeps_the_last_identity() {
    let fixture = Fixture::new();
    let mut expected = Vec::new();
    fixture
        .db
        .with_transaction(|| {
            for number in 1..=PAGE_SIZE * 2 + 1 {
                fixture.seed(number, "fact", "row store");
                expected.push(fixture.seed(number + 10_000, "decision", "row store"));
            }
            Ok(())
        })
        .unwrap();
    let mut hydrated = Vec::new();
    let mut pages = Vec::new();
    let reader = fixture.reader();
    let result = read_with_observer(&reader, &mut fixture.scope(), false, clock(), |ids| {
        pages.push(ids.len());
        hydrated.extend(ids.iter().map(|id| (*id).to_owned()));
        Ok(())
    })
    .unwrap();
    assert_eq!(pages, [PAGE_SIZE, PAGE_SIZE, 1]);
    assert_eq!(hydrated, expected);
    assert_eq!(result.len(), expected.len());
    assert_eq!(result.last().unwrap().memory_id, *expected.last().unwrap());
    released(&reader);
}

#[test]
fn a_failed_later_page_returns_no_partial_corpus_and_releases_the_snapshot() {
    let fixture = Fixture::new();
    fixture
        .db
        .with_transaction(|| {
            for number in 1..=PAGE_SIZE + 1 {
                fixture.seed(number, "decision", "row store");
            }
            Ok(())
        })
        .unwrap();
    let reader = fixture.reader();
    let mut pages = 0;
    let result = read_with_observer(&reader, &mut fixture.scope(), false, clock(), |_| {
        pages += 1;
        if pages == 2 {
            Err(read_error())
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert_eq!(pages, 2);
    released(&reader);
    assert_eq!(fixture.read(false).len(), PAGE_SIZE + 1);
}

#[test]
fn invalid_stored_typed_fields_are_sanitized_and_do_not_pin_a_read() {
    let fixture = Fixture::new();
    fixture.seed(1, "decision", "row store");
    fixture
        .db
        .execute_raw(
            "UPDATE memories SET typed_fields_json = '{\"chosen\":{\"PRIVATE-DECISION-CANARY\":true}}'",
        )
        .unwrap();
    let reader = fixture.reader();
    let error =
        read_with_observer(&reader, &mut fixture.scope(), false, clock(), |_| Ok(())).unwrap_err();
    assert!(matches!(error, DomainError::Storage { .. }));
    assert!(!format!("{error:?}").contains("PRIVATE-DECISION-CANARY"));
    released(&reader);
}

#[test]
fn failed_nested_begin_preserves_the_callers_snapshot_and_unwind_releases_only_ours() {
    let fixture = Fixture::new();
    fixture.seed(1, "decision", "row store");
    let reader = fixture.reader();
    reader.begin_read_snapshot().unwrap();
    assert!(read_with_observer(&reader, &mut fixture.scope(), false, clock(), |_| Ok(())).is_err());
    reader
        .commit_read_snapshot()
        .expect("the caller still owns its transaction");
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = read_with_observer(&reader, &mut fixture.scope(), false, clock(), |_| {
            panic!("injected read interruption");
        });
    }));
    assert!(unwound.is_err());
    released(&reader);
}

#[test]
fn headship_remains_clock_free_while_history_and_tombstones_keep_their_distinctions() {
    let fixture = Fixture::new();
    let expired = fixture.seed(1, "decision", "row store");
    let replaced = fixture.seed(2, "decision", "row store");
    let deleted = fixture.seed(3, "decision", "row store");
    fixture
        .db
        .expire_memory_valid_to(&expired, "2020-01-01T00:00:00Z")
        .unwrap();
    fixture
        .db
        .mark_memory_superseded(&replaced, "2099-01-01T00:00:00Z")
        .unwrap();
    fixture.db.tombstone_memory(&deleted).unwrap();
    let bytes = std::fs::read(fixture.scope().database_path).unwrap();
    let heads = fixture.read(false);
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].memory_id, expired);
    assert!(!heads[0].superseded && heads[0].valid_to.is_some());
    let history = fixture.read(true);
    assert_eq!(history.len(), 2);
    assert!(
        history
            .iter()
            .any(|item| item.memory_id == replaced && item.superseded)
    );
    assert!(!history.iter().any(|item| item.memory_id == deleted));
    assert_eq!(std::fs::read(fixture.scope().database_path).unwrap(), bytes);
    assert!(!fixture.root.path().join(".ee/index").exists());
}

#[test]
fn public_revisit_uses_the_same_current_decision_snapshot() {
    let fixture = Fixture::new();
    let current = fixture.seed(1, "decision", "row store");
    let old = fixture.seed(2, "decision", "column store");
    fixture
        .db
        .mark_memory_superseded(&old, "2026-01-01T00:00:00Z")
        .unwrap();
    let report = decide_revisit(&DecideRevisitOptions {
        workspace_path: fixture.root.path(),
        database_path: None,
        warning_days: Some(2),
        limit: 10,
        now: Some(clock()),
    })
    .unwrap();
    assert_eq!(report.due_count, 1);
    assert_eq!(report.decisions[0].memory_id, current);
    assert_eq!(report.decisions[0].revisit_status, "near_due");
}
