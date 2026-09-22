//! Real FrankenSQLite interleavings for the production resume loader.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use crate::models::MemoryId;

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    memory_id: String,
    reader: DbConnection,
    writer: DbConnection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".ee")).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let database = workspace.join(".ee/ee.db");
        let writer = DbConnection::open_file(&database).unwrap();
        writer.migrate().unwrap();
        let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
        writer
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("coherent resume".to_owned()),
                },
            )
            .unwrap();
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d45)).to_string();
        writer
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id,
                    level: "episodic".to_owned(),
                    kind: "decision".to_owned(),
                    content: "Topic: Old session\nChosen: old choice".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some(format!("ee://memory/{memory_id}")),
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: vec![
                        "session-old".to_owned(),
                        "next".to_owned(),
                        "resume".to_owned(),
                    ],
                    // Match the deterministic August row clock below. A missing
                    // author-validity bound defaults to the real insertion time,
                    // which would legitimately exclude this row from August reads.
                    valid_from: Some("2026-08-09T10:00:00Z".to_owned()),
                    valid_to: None,
                },
            )
            .unwrap();
        writer
            .set_memory_typed_fields_json(&memory_id, Some(&decision_fields("old choice")))
            .unwrap();
        writer.execute_raw(&format!(
            "UPDATE memories SET created_at = '2026-08-09T10:00:00Z', updated_at = '2026-08-09T10:00:00Z' WHERE id = '{memory_id}'"
        )).unwrap();
        // Both real connections are ready before the reader pins a snapshot.
        let reader = DbConnection::open_file_read_only(&database).unwrap();
        Self {
            _root: root,
            workspace,
            database,
            memory_id,
            reader,
            writer,
        }
    }

    fn options(&self) -> ResumeOptions<'_> {
        ResumeOptions {
            workspace_path: &self.workspace,
            database_path: &self.database,
            sessions: 3,
        }
    }

    fn load(&self) -> Result<ResumeState, DomainError> {
        load(
            &self.reader,
            &self.options(),
            &self.workspace,
            reference_time(),
        )
    }

    fn rewrite(&self) {
        self.writer.with_transaction(|| {
            self.writer.execute_raw(&format!(
                "UPDATE memories SET content = 'Topic: New session\nChosen: new choice' WHERE id = '{}'", self.memory_id
            ))?;
            self.writer.execute_raw(&format!(
                "UPDATE memory_tags SET tag = 'session-new' WHERE memory_id = '{}' AND tag = 'session-old'", self.memory_id
            ))?;
            self.writer.set_memory_typed_fields_json(&self.memory_id, Some(&decision_fields("new choice")))?;
            Ok(())
        }).expect("writer commits independently of the pinned read-only snapshot");
    }
}

fn reference_time() -> DateTime<Utc> {
    super::super::parse_ts("2030-01-01T00:00:00Z").unwrap()
}

fn decision_fields(chosen: &str) -> String {
    serde_json::json!({
        "options": ["old choice", "new choice"],
        "chosen": chosen,
        "rationale": "Preserve coherent session state",
        "revisit_by": "2026-08-11T00:00:00Z"
    })
    .to_string()
}

fn assert_released(connection: &DbConnection) {
    connection
        .begin_read_snapshot()
        .expect("no leaked resume transaction");
    connection.commit_read_snapshot().unwrap();
}

#[test]
fn memory_body_tags_and_decision_fields_share_one_real_snapshot() {
    let fixture = Fixture::new();
    let before = load_with_boundary(
        &fixture.reader,
        &fixture.options(),
        &fixture.workspace,
        reference_time(),
        || {
            fixture.rewrite();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(before.all_live.len(), 1);
    assert!(before.all_live[0].content.contains("Old session"));
    assert!(before.tags[&fixture.memory_id].contains(&"session-old".to_owned()));
    assert!(!before.tags[&fixture.memory_id].contains(&"session-new".to_owned()));
    let (decisions, total, truncated) = super::super::collect_revisit_decisions(
        &before.all_live,
        &before.typed_decision_fields,
        reference_time(),
    )
    .unwrap();
    assert_eq!(total, 1);
    assert!(!truncated);
    assert_eq!(decisions[0].chosen, "old choice");
    assert_released(&fixture.reader);

    // A fresh call must not remain pinned to the old state.
    let after = fixture.load().unwrap();
    assert!(after.all_live[0].content.contains("New session"));
    assert!(after.tags[&fixture.memory_id].contains(&"session-new".to_owned()));
    let (decisions, _, _) = super::super::collect_revisit_decisions(
        &after.all_live,
        &after.typed_decision_fields,
        reference_time(),
    )
    .unwrap();
    assert_eq!(decisions[0].chosen, "new choice");
    let public = super::super::build_resume_report(&fixture.options()).unwrap();
    assert_eq!(public.sessions[0].label, "session-new");
    assert_eq!(public.open_loops.revisit_decisions[0].chosen, "new choice");
}

#[test]
fn concurrent_retirement_belongs_only_to_the_next_resume() {
    let fixture = Fixture::new();
    let before = load_with_boundary(
        &fixture.reader,
        &fixture.options(),
        &fixture.workspace,
        reference_time(),
        || {
            fixture
                .writer
                .execute_raw(&format!(
                    "UPDATE memories SET tombstoned_at = '2026-08-10T00:00:00Z' WHERE id = '{}'",
                    fixture.memory_id
                ))
                .unwrap();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(before.all_live.len(), 1);
    assert_eq!(before.typed_decision_fields.len(), 1);
    let after = fixture.load().unwrap();
    assert!(after.all_live.is_empty());
    assert!(after.tags.is_empty());
    assert!(after.typed_decision_fields.is_empty());
}

#[test]
fn failed_dependent_read_withholds_the_bundle_and_releases_the_snapshot() {
    let fixture = Fixture::new();
    let failed = load_with_boundary(
        &fixture.reader,
        &fixture.options(),
        &fixture.workspace,
        reference_time(),
        || {
            Err(DomainError::Storage {
                message: "controlled read failure".to_owned(),
                repair: None,
            })
        },
    );
    assert!(matches!(failed, Err(DomainError::Storage { .. })));
    assert_released(&fixture.reader);
    assert_eq!(fixture.load().unwrap().all_live.len(), 1);
}

#[test]
fn unwinding_releases_the_owned_snapshot() {
    let fixture = Fixture::new();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = load_with_boundary(
            &fixture.reader,
            &fixture.options(),
            &fixture.workspace,
            reference_time(),
            || panic!("controlled resume boundary panic"),
        );
    }));
    assert!(failed.is_err());
    assert_released(&fixture.reader);
    assert!(fixture.load().is_ok());
}

#[test]
fn a_failed_nested_begin_preserves_the_callers_transaction() {
    let fixture = Fixture::new();
    fixture.reader.begin_read_snapshot().unwrap();
    fixture.reader.get_memory_tags(&fixture.memory_id).unwrap();
    assert!(fixture.load().is_err());
    fixture
        .reader
        .commit_read_snapshot()
        .expect("resume must not roll back the caller");
    assert_released(&fixture.reader);
}

#[test]
fn incomplete_schema_releases_the_snapshot_without_returning_a_partial_bundle() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("unmigrated.db");
    let connection = DbConnection::open_file(&database).unwrap();
    let options = ResumeOptions {
        workspace_path: root.path(),
        database_path: &database,
        sessions: 3,
    };
    assert!(matches!(
        load(&connection, &options, root.path(), reference_time()),
        Err(DomainError::MigrationRequired { .. })
    ));
    assert_released(&connection);
}

#[test]
fn fresh_fractional_rows_are_visible_without_waiting_for_next_second() {
    let fixture = Fixture::new();
    fixture.writer.execute_raw(&format!(
        "UPDATE memories SET created_at = '2026-08-09T10:00:00.500+00:00', updated_at = '2026-08-09T10:00:00.500+00:00' WHERE id = '{}'", fixture.memory_id
    )).unwrap();
    // Differential control: the old whole-second caller hides this row,
    // even though the authored validity is already active.
    let workspace_id = crate::core::workspace::stable_workspace_id(&fixture.workspace);
    assert!(
        fixture
            .reader
            .list_recent_current_memories_for_retrieval(&workspace_id, "2026-08-09T10:00:00Z", 8,)
            .unwrap()
            .is_empty()
    );
    let now = super::super::parse_ts("2026-08-09T10:00:00.750Z").unwrap();
    let state = load(&fixture.reader, &fixture.options(), &fixture.workspace, now).unwrap();
    assert_eq!(
        state.all_live.len(),
        1,
        "never truncate the row clock to the start of this second"
    );
    assert_eq!(state.all_live[0].id, fixture.memory_id);
    assert!(state.tags[&fixture.memory_id].contains(&"session-old".to_owned()));
    assert!(state.typed_decision_fields.contains_key(&fixture.memory_id));
    assert_released(&fixture.reader);
}

#[test]
fn precise_row_clock_still_excludes_future_creation_and_updates() {
    let fixture = Fixture::new();
    let workspace_id = crate::core::workspace::stable_workspace_id(&fixture.workspace);
    for (created, updated, expected) in [
        ("2026-08-09T10:00:00.500Z", "2026-08-09T10:00:00.500Z", 1),
        ("2026-08-09T10:00:00.750Z", "2026-08-09T10:00:00.750Z", 1),
        ("2026-08-09T10:00:00.900Z", "2026-08-09T10:00:00.900Z", 0),
        ("2026-08-09T10:00:00.500Z", "2026-08-09T10:00:00.900Z", 0),
    ] {
        fixture.writer.execute_raw(&format!(
            "UPDATE memories SET created_at = '{created}', updated_at = '{updated}' WHERE id = '{}'", fixture.memory_id
        )).unwrap();
        let rows = fixture
            .reader
            .list_recent_current_memories_for_retrieval(
                &workspace_id,
                "2026-08-09T12:00:00.750+02:00",
                8,
            )
            .unwrap();
        assert_eq!(rows.len(), expected, "created={created}, updated={updated}");
    }
}

#[test]
fn precise_row_clock_preserves_exact_validity_and_supersession_boundaries() {
    let fixture = Fixture::new();
    let workspace_id = crate::core::workspace::stable_workspace_id(&fixture.workspace);
    let start = "2026-08-09T10:00:00Z";
    let before = "2026-08-09T09:59:59Z";
    let after = "2026-08-09T10:00:01Z";
    let at = "2026-08-09T10:00:00.750Z";
    let earlier = "2026-08-09T10:00:00.749999999Z";
    let later = "2026-08-09T10:00:00.750000001Z";
    for (from, to, superseded, expected) in [
        (start, after, "NULL".to_owned(), 1),
        // An expiry at .000 must not remain active at .750. Equality is
        // inclusive at the exact endpoint, not throughout its rounded second.
        (start, start, "NULL".to_owned(), 0),
        (start, at, "NULL".to_owned(), 1),
        (start, earlier, "NULL".to_owned(), 0),
        (start, later, "NULL".to_owned(), 1),
        (at, after, "NULL".to_owned(), 1),
        (later, after, "NULL".to_owned(), 0),
        (after, after, "NULL".to_owned(), 0),
        (before, before, "NULL".to_owned(), 0),
        (start, after, format!("'{start}'"), 0),
        (start, after, format!("'{at}'"), 0),
        (start, after, format!("'{earlier}'"), 0),
        (start, after, format!("'{later}'"), 1),
        (start, after, format!("'{after}'"), 1),
    ] {
        fixture.writer.execute_raw(&format!(
            "UPDATE memories SET created_at = '2026-08-09T10:00:00.500Z', updated_at = '2026-08-09T10:00:00.500Z', valid_from = '{from}', valid_to = '{to}', superseded_at = {superseded} WHERE id = '{}'", fixture.memory_id
        )).unwrap();
        let rows = fixture
            .reader
            .list_recent_current_memories_for_retrieval(
                &workspace_id,
                "2026-08-09T12:00:00.750+02:00",
                8,
            )
            .unwrap();
        assert_eq!(
            rows.len(),
            expected,
            "from={from}, to={to}, superseded={superseded}"
        );
    }
}

#[test]
fn invalid_recency_clock_is_an_error_not_an_empty_success() {
    let fixture = Fixture::new();
    let workspace_id = crate::core::workspace::stable_workspace_id(&fixture.workspace);
    for invalid in ["", "not-a-clock", "2026-08-09", "2026-08-09T10:00:00"] {
        assert!(
            fixture
                .reader
                .list_recent_current_memories_for_retrieval(&workspace_id, invalid, 8,)
                .is_err(),
            "reject {invalid:?}"
        );
    }
}
