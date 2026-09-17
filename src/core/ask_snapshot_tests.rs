#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{AskRequest, evaluate_ask};
use crate::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, MemoryLinkRelation,
    MemoryLinkSource,
};
use crate::models::WorkspaceId;

const ANCHOR_ID: &str = "mem_00000000000000000000000001";
const OPPOSITION_ID: &str = "mem_00000000000000000000000002";
const EDGE_ID: &str = "link_00000000000000000000000001";

fn reference_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-17T12:00:00Z")
        .expect("fixed clock")
        .with_timezone(&Utc)
}

fn fixture() -> (tempfile::TempDir, DbConnection, DbConnection, String) {
    let root = tempfile::tempdir().expect("real snapshot store");
    let database = root.path().join("ask.db");
    let reader = DbConnection::open_file(&database).expect("reader");
    reader.migrate().expect("schema");
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([42; 16])).to_string();
    reader
        .insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: Some("ask coherent snapshot".to_owned()),
            },
        )
        .expect("workspace");
    for (id, content) in [
        (ANCHOR_ID, "Run cargo fmt before release."),
        (
            OPPOSITION_ID,
            "Formatting is prohibited by deployment policy.",
        ),
    ] {
        reader
            .insert_memory(
                id,
                &CreateMemoryInput {
                    workspace_id: workspace.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some(format!("manual://ask-snapshot/{id}")),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
                    valid_to: None,
                },
            )
            .expect("current memory");
    }
    // Open before pinning the reader, so connection initialization is not part
    // of the controlled interleaving. This is a second real storage connection.
    let writer = DbConnection::open_file(&database).expect("writer");
    (root, reader, writer, workspace)
}

fn insert_edge(connection: &DbConnection) {
    connection
        .insert_memory_link(
            EDGE_ID,
            &CreateMemoryLinkInput {
                src_memory_id: ANCHOR_ID.to_owned(),
                dst_memory_id: OPPOSITION_ID.to_owned(),
                relation: MemoryLinkRelation::Contradicts,
                weight: 1.0,
                confidence: 0.9,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Human,
                created_by: None,
                metadata_json: None,
            },
        )
        .expect("stored opposition");
}

fn answer(corpus: AskCorpus) -> super::super::AskReport {
    evaluate_ask(
        &AskRequest {
            question: "Run cargo fmt before release".to_owned(),
            contradictions: corpus.contradictions,
            ..AskRequest::default()
        },
        &corpus.candidates,
    )
}

fn assert_snapshot_released(connection: &DbConnection) {
    connection
        .begin_read_snapshot()
        .expect("a new read must not encounter a leaked transaction");
    connection.commit_read_snapshot().expect("finish probe");
}

#[test]
fn concurrent_expiration_and_edge_change_do_not_mix_old_bodies_with_new_links() {
    let (_root, reader, writer, workspace) = fixture();
    insert_edge(&writer);
    let corpus = load_corpus_with_boundary(&reader, &workspace, reference_time(), || {
        writer
            .with_transaction(|| {
                writer.execute_raw(&format!(
                    "UPDATE memories SET valid_to = '2021-01-01T00:00:00Z' WHERE id = '{OPPOSITION_ID}'"
                ))?;
                writer.execute_raw(&format!(
                    "UPDATE memory_links SET confidence = 0.1 WHERE id = '{EDGE_ID}'"
                ))?;
                Ok(())
            })
            .expect("writer must commit while the old read snapshot remains pinned");
        Ok(())
    })
    .expect("coherent pre-write corpus");
    assert_eq!(corpus.candidates.len(), 2);
    assert_eq!(corpus.contradictions.len(), 1);
    assert_eq!(corpus.contradictions[0].confidence, 0.9);
    let old = answer(corpus);
    assert!(!old.abstained && old.conflict_detected);
    assert_eq!(
        old.sides.as_ref().unwrap()[1].citations[0].memory_id,
        OPPOSITION_ID
    );

    let next = load_current_ask_corpus(&reader, &workspace, reference_time()).unwrap();
    assert_eq!(next.candidates.len(), 1);
    assert_eq!(next.candidates[0].memory_id, ANCHOR_ID);
    assert!(next.contradictions.is_empty());
    let new = answer(next);
    assert!(!new.abstained && !new.conflict_detected);
    assert_eq!(new.citations[0].memory_id, ANCHOR_ID);
}

#[test]
fn a_newly_committed_edge_is_visible_only_to_the_next_corpus_snapshot() {
    let (_root, reader, writer, workspace) = fixture();
    let old = load_corpus_with_boundary(&reader, &workspace, reference_time(), || {
        insert_edge(&writer);
        Ok(())
    })
    .expect("old snapshot");
    assert_eq!(old.candidates.len(), 2);
    assert!(old.contradictions.is_empty());
    assert!(!answer(old).conflict_detected);
    let next = load_current_ask_corpus(&reader, &workspace, reference_time()).unwrap();
    assert_eq!(next.contradictions.len(), 1);
    assert_eq!(next.contradictions[0].id, EDGE_ID);
    assert!(answer(next).conflict_detected);
}

#[test]
fn a_failed_read_releases_the_snapshot_and_returns_no_partial_corpus() {
    let (_root, reader, _writer, workspace) = fixture();
    let failed = load_corpus_with_boundary(&reader, &workspace, reference_time(), || {
        Err(DomainError::Storage {
            message: "injected boundary failure".to_owned(),
            repair: None,
        })
    });
    assert!(matches!(failed, Err(DomainError::Storage { .. })));
    assert_snapshot_released(&reader);
    assert_eq!(
        load_current_ask_corpus(&reader, &workspace, reference_time())
            .unwrap()
            .candidates
            .len(),
        2
    );
}

#[test]
fn unwinding_releases_only_the_owned_snapshot() {
    let (_root, reader, _writer, workspace) = fixture();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = load_corpus_with_boundary(&reader, &workspace, reference_time(), || {
            panic!("controlled failure after the memory read");
        });
    }));
    assert!(panicked.is_err());
    assert_snapshot_released(&reader);
    assert!(load_current_ask_corpus(&reader, &workspace, reference_time()).is_ok());
}

#[test]
fn a_failed_nested_begin_does_not_roll_back_the_callers_transaction() {
    let (_root, reader, _writer, workspace) = fixture();
    reader.begin_read_snapshot().unwrap();
    reader.list_memories(&workspace, None, false).unwrap();
    assert!(load_current_ask_corpus(&reader, &workspace, reference_time()).is_err());
    reader
        .commit_read_snapshot()
        .expect("the caller's transaction must still exist");
    assert_snapshot_released(&reader);
}

#[test]
fn missing_schema_failure_does_not_leave_a_transaction_open() {
    let root = tempfile::tempdir().unwrap();
    let connection = DbConnection::open_file(&root.path().join("unmigrated.db")).unwrap();
    assert!(load_current_ask_corpus(&connection, "missing", reference_time()).is_err());
    assert_snapshot_released(&connection);
}

#[test]
fn successful_reads_release_the_snapshot_before_returning_owned_evidence() {
    let (_root, reader, _writer, workspace) = fixture();
    let corpus = load_current_ask_corpus(&reader, &workspace, reference_time()).unwrap();
    assert_snapshot_released(&reader);
    // A later write on this very connection must be independent of the read.
    insert_edge(&reader);
    assert!(corpus.contradictions.is_empty());
    assert!(!answer(corpus).conflict_detected);
    assert!(
        answer(load_current_ask_corpus(&reader, &workspace, reference_time()).unwrap())
            .conflict_detected
    );
}
