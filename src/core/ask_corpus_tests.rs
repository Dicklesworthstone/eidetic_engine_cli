#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{AskRequest, ask_data_json, evaluate_ask};
use crate::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, MemoryLinkRelation,
    MemoryLinkSource,
};
use crate::models::WorkspaceId;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-17T12:00:00Z")
        .expect("fixed clock")
        .with_timezone(&Utc)
}

fn fixture() -> (tempfile::TempDir, DbConnection, String) {
    let root = tempfile::tempdir().expect("temporary real store");
    let connection = DbConnection::open_file(&root.path().join("ask.db")).expect("open");
    connection.migrate().expect("migrate real schema");
    let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([41; 16])).to_string();
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: Some("ask lifecycle regression".to_owned()),
            },
        )
        .expect("workspace");
    (root, connection, workspace_id)
}

fn seed(
    connection: &DbConnection,
    workspace_id: &str,
    number: usize,
    content: &str,
    valid_from: &str,
    valid_to: Option<&str>,
) -> String {
    let id = format!("mem_{number:026}");
    connection
        .insert_memory(
            &id,
            &CreateMemoryInput {
                workspace_id: workspace_id.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: content.to_owned(),
                workflow_id: None,
                confidence: 1.0,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some(format!("manual://ask-lifecycle/{number}")),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: Some(valid_from.to_owned()),
                valid_to: valid_to.map(str::to_owned),
            },
        )
        .expect("memory");
    id
}

fn edge(connection: &DbConnection, src: &str, dst: &str) {
    connection
        .insert_memory_link(
            "link_00000000000000000000000001",
            &CreateMemoryLinkInput {
                src_memory_id: src.to_owned(),
                dst_memory_id: dst.to_owned(),
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
        .expect("contradiction");
}

#[test]
fn unbounded_and_one_sided_windows_are_supported() {
    assert!(validity_contains(None, None, now()).unwrap());
    assert!(validity_contains(Some("2020-01-01T00:00:00Z"), None, now()).unwrap());
    assert!(validity_contains(None, Some("2100-01-01T00:00:00Z"), now()).unwrap());
}

#[test]
fn expired_and_future_windows_are_not_current() {
    assert!(!validity_contains(None, Some("2026-09-17T11:59:59Z"), now()).unwrap());
    assert!(!validity_contains(Some("2026-09-17T12:00:01Z"), None, now()).unwrap());
}

#[test]
fn both_endpoints_are_inclusive_like_search() {
    let boundary = "2026-09-17T12:00:00Z";
    assert!(validity_contains(Some(boundary), None, now()).unwrap());
    assert!(validity_contains(None, Some(boundary), now()).unwrap());
    assert!(validity_contains(Some(boundary), Some(boundary), now()).unwrap());
}

#[test]
fn offsets_and_subsecond_precision_are_compared_as_instants() {
    assert!(
        validity_contains(
            Some("2026-09-17T08:00:00-04:00"),
            Some("2026-09-17T14:00:00+02:00"),
            now(),
        )
        .unwrap()
    );
    assert!(!validity_contains(Some("2026-09-17T12:00:00.000000001Z"), None, now()).unwrap());
    assert!(!validity_contains(None, Some("2026-09-17T11:59:59.999999999Z"), now()).unwrap());
}

#[test]
fn malformed_or_reversed_windows_fail_closed() {
    for malformed in ["", "tomorrow", "2026-09-17", "2026-13-01T00:00:00Z"] {
        assert!(validity_contains(Some(malformed), None, now()).is_err());
        assert!(validity_contains(None, Some(malformed), now()).is_err());
    }
    assert!(
        validity_contains(
            Some("2100-01-01T00:00:00Z"),
            Some("2020-01-01T00:00:00Z"),
            now(),
        )
        .is_err()
    );
}

#[test]
fn an_ineligible_bound_does_not_hide_corruption_in_the_other_bound() {
    assert!(validity_contains(Some("2100-01-01T00:00:00Z"), Some("invalid"), now()).is_err());
    assert!(validity_contains(Some("invalid"), Some("2020-01-01T00:00:00Z"), now()).is_err());
}

#[test]
fn validity_errors_do_not_echo_private_metadata() {
    let private = "file:///private/operator/secret-validity-value";
    let error = validity_contains(Some(private), None, now()).unwrap_err();
    let rendered = format!("{error:?}");
    assert!(!rendered.contains(private));
    assert!(matches!(error, DomainError::Storage { .. }));
}

#[test]
fn real_store_admits_only_current_bodies_and_preserves_citations() {
    let (_root, connection, workspace) = fixture();
    let current = seed(
        &connection,
        &workspace,
        1,
        "Run cargo fmt before release.",
        "2020-01-01T00:00:00Z",
        None,
    );
    let expired = seed(
        &connection,
        &workspace,
        2,
        "Never run cargo fmt before release.",
        "2020-01-01T00:00:00Z",
        Some("2021-01-01T00:00:00Z"),
    );
    let future = seed(
        &connection,
        &workspace,
        3,
        "Never run cargo fmt before release.",
        "2100-01-01T00:00:00Z",
        None,
    );
    edge(&connection, &current, &expired);
    let corpus = load_current_ask_corpus(&connection, &workspace, now()).expect("current corpus");
    assert_eq!(corpus.candidates.len(), 1);
    assert_eq!(corpus.candidates[0].memory_id, current);
    assert!(corpus.contradictions.is_empty());
    let request = AskRequest {
        question: "Run cargo fmt before release".to_owned(),
        contradictions: corpus.contradictions,
        ..AskRequest::default()
    };
    let report = evaluate_ask(&request, &corpus.candidates);
    assert!(!report.abstained && !report.conflict_detected);
    assert_eq!(report.citations.len(), 1);
    assert_eq!(report.citations[0].memory_id, current);
    assert_eq!(report.citations[0].text, "Run cargo fmt before release.");
    assert_eq!(
        report.citations[0].provenance_uri.as_deref(),
        Some("manual://ask-lifecycle/1")
    );
    let output = ask_data_json(&report).to_string();
    assert!(!output.contains(&expired));
    assert!(!output.contains(&future));
}

#[test]
fn ineligible_evidence_cannot_leak_through_abstention_hints() {
    let (_root, connection, workspace) = fixture();
    let expired = seed(
        &connection,
        &workspace,
        1,
        "Retired private lunar deployment advice.",
        "2020-01-01T00:00:00Z",
        Some("2021-01-01T00:00:00Z"),
    );
    seed(
        &connection,
        &workspace,
        2,
        "Unrelated veterinary notes.",
        "2020-01-01T00:00:00Z",
        None,
    );
    let corpus = load_current_ask_corpus(&connection, &workspace, now()).unwrap();
    let report = evaluate_ask(
        &AskRequest {
            question: "lunar deployment".to_owned(),
            ..AskRequest::default()
        },
        &corpus.candidates,
    );
    assert!(report.abstained);
    let output = ask_data_json(&report).to_string();
    assert!(!output.contains(&expired));
    assert!(!output.contains("Retired private"));
}

#[test]
fn current_explicit_opposition_still_reaches_the_engine() {
    let (_root, connection, workspace) = fixture();
    let anchor = seed(
        &connection,
        &workspace,
        1,
        "Run cargo fmt before release.",
        "2020-01-01T00:00:00Z",
        None,
    );
    let opposing = seed(
        &connection,
        &workspace,
        2,
        "Formatting is prohibited by deployment policy.",
        "2020-01-01T00:00:00Z",
        None,
    );
    edge(&connection, &anchor, &opposing);
    let corpus = load_current_ask_corpus(&connection, &workspace, now()).unwrap();
    assert_eq!(corpus.contradictions.len(), 1);
    let report = evaluate_ask(
        &AskRequest {
            question: "Run cargo fmt before release".to_owned(),
            contradictions: corpus.contradictions,
            ..AskRequest::default()
        },
        &corpus.candidates,
    );
    assert!(!report.abstained && report.conflict_detected);
    assert_eq!(
        report.sides.as_ref().unwrap()[1].citations[0].memory_id,
        opposing
    );
}

#[test]
fn reading_current_corpus_does_not_mutate_durable_memory_or_audits() {
    let (_root, connection, workspace) = fixture();
    seed(
        &connection,
        &workspace,
        1,
        "Run cargo fmt before release.",
        "2020-01-01T00:00:00Z",
        None,
    );
    let memories = connection.list_memories(&workspace, None, true).unwrap();
    let audits = connection.list_audit_entries(None, None).unwrap();
    load_current_ask_corpus(&connection, &workspace, now()).unwrap();
    assert_eq!(
        memories,
        connection.list_memories(&workspace, None, true).unwrap()
    );
    assert_eq!(
        audits.len(),
        connection.list_audit_entries(None, None).unwrap().len()
    );
}
