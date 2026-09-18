#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{
    AskReport, AskRequest, ask_data_json, evaluate_ask, record_ask_retrieval_best_effort,
    render_ask_markdown,
};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
};
use crate::models::{
    CASS_EVIDENCE_SPAN_SCHEMA_V1, CASS_SESSION_SCHEMA_V1, EvidenceId, SessionId, WorkspaceId,
};

const BODY: &str = "Run cargo fmt before every release tag.";
const QUESTION: &str = "Which command must run before every release tag?";

fn fixture() -> (tempfile::TempDir, DbConnection, String) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".ee")).unwrap();
    let db = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
    db.migrate().unwrap();
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(710)).to_string();
    db.insert_workspace(
        &workspace,
        &CreateWorkspaceInput {
            path: root.path().to_string_lossy().into_owned(),
            name: None,
        },
    )
    .unwrap();
    (root, db, workspace)
}

fn session(db: &DbConnection, workspace: &str, n: u128) -> String {
    let id = SessionId::from_uuid(uuid::Uuid::from_u128(n)).to_string();
    db.insert_session(
        &id,
        &CreateSessionInput {
            workspace_id: workspace.to_owned(),
            cass_session_id: format!("/private/ask-session-{n}.jsonl"),
            source_path: Some(format!("/private/ask-session-{n}.jsonl")),
            agent_name: Some("codex".to_owned()),
            model: None,
            started_at: Some("2026-01-01T00:00:00Z".to_owned()),
            ended_at: Some("2026-01-01T01:00:00Z".to_owned()),
            message_count: 8,
            token_count: None,
            content_hash: blake3::hash(format!("session-{n}").as_bytes())
                .to_hex()
                .to_string(),
            metadata_json: Some(
                serde_json::json!({
                    "schema": CASS_SESSION_SCHEMA_V1,
                    "contentHashSource": "provided",
                    "messageCountObserved": true,
                })
                .to_string(),
            ),
        },
    )
    .unwrap();
    id
}

fn evidence(db: &DbConnection, workspace: &str, session: &str, n: u32, body: &str) -> String {
    let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(u128::from(n))).to_string();
    db.insert_evidence_span(
        &id,
        &CreateEvidenceSpanInput {
            workspace_id: workspace.to_owned(),
            session_id: session.to_owned(),
            memory_id: None,
            producer_kind: EvidenceProducerKind::CassImport,
            cass_span_id: format!("ask-span-{n}"),
            span_kind: crate::cass::CassSpanKind::Message.as_str().to_owned(),
            start_line: n,
            end_line: n,
            start_byte: None,
            end_byte: None,
            role: Some("user".to_owned()),
            excerpt: body.to_owned(),
            content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            metadata_json: Some(
                serde_json::json!({
                    "schema": CASS_EVIDENCE_SPAN_SCHEMA_V1,
                    "redactionStatus": "clean",
                    "redactionClasses": [],
                })
                .to_string(),
            ),
            inherited_redaction_classes: Vec::new(),
        },
    )
    .unwrap();
    let span = db.get_evidence_span(&id).unwrap().unwrap();
    let session = db.get_session(session).unwrap().unwrap();
    assert!(span.is_direct_pack_admitted_for_session(workspace, &session));
    id
}

fn request(corpus: &AskCorpus) -> AskRequest {
    AskRequest {
        question: QUESTION.to_owned(),
        contradictions: corpus.contradictions.clone(),
        native_sources: corpus.native_sources.clone(),
        ..AskRequest::default()
    }
}

fn answer(corpus: &AskCorpus) -> AskReport {
    evaluate_ask(&request(corpus), &corpus.candidates)
}

#[test]
fn native_cass_excerpt_is_answered_and_audited_without_a_memory_alias() {
    let (_root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let id = evidence(&db, &workspace, &session, 1, BODY);
    let span = db.get_evidence_span(&id).unwrap().unwrap();
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(corpus.candidates.len(), 1);
    let report = answer(&corpus);
    assert!(!report.abstained, "{report:?}");
    let data = ask_data_json(&report);
    let citation = &data["citations"][0];
    assert_eq!(citation["evidenceId"], id);
    assert_eq!(citation["entityId"], id);
    assert_eq!(citation["entityKind"], "evidence_span");
    assert_eq!(citation["entityRevision"], span.pack_entity_revision());
    assert_eq!(citation["provenanceUri"], span.canonical_provenance_uri());
    assert_eq!(citation["trustClass"], "cass_evidence");
    assert_eq!(citation["confidence"], 0.5);
    assert_eq!(citation["text"], BODY);
    assert!(citation.get("memoryId").is_none());
    assert!(!data.to_string().contains("/private/ask-session"));
    assert!(render_ask_markdown(&report).contains(&id));
    assert!(db.list_memories(&workspace, None, true).unwrap().is_empty());
    record_ask_retrieval_best_effort(&db, &workspace, &report);
    let audits = db.list_audit_by_target("evidence", &id, None).unwrap();
    assert_eq!(audits.len(), 1);
    assert!(
        db.list_audit_by_target("memory", &id, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_cass_abstention_hints_keep_evidence_identity() {
    let (_root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let id = evidence(&db, &workspace, &session, 1, BODY);
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    let mut weak = request(&corpus);
    weak.question = "release".to_owned();
    weak.min_confidence = 1.0;
    let report = evaluate_ask(&weak, &corpus.candidates);
    assert!(report.abstained);
    let data = ask_data_json(&report);
    assert_eq!(data["nearestEvidence"][0]["evidenceId"], id);
    assert!(data["nearestEvidence"][0].get("memoryId").is_none());
    assert!(!data.to_string().contains("matchedMemoryId"));
    assert!(!data.to_string().contains("/private/ask-session"));
}

#[test]
fn repeated_cass_windows_do_not_manufacture_answer_confidence() {
    let (_root, db, workspace) = fixture();
    let first_session = session(&db, &workspace, 1);
    evidence(&db, &workspace, &first_session, 1, BODY);
    let baseline = answer(&load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap());
    for n in 2..=4 {
        evidence(&db, &workspace, &first_session, n, BODY);
    }
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(corpus.candidates.len(), 4);
    let report = answer(&corpus);
    assert_eq!(report.confidence.to_bits(), baseline.confidence.to_bits());
    assert_eq!(report.confidence_components.corroboration, 1.0);
    let mut reversed = corpus.clone();
    reversed.candidates.reverse();
    assert_eq!(ask_data_json(&answer(&reversed)), ask_data_json(&report));
    let second_session = session(&db, &workspace, 2);
    evidence(&db, &workspace, &second_session, 5, BODY);
    let independent = answer(&load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap());
    assert!(independent.confidence > report.confidence);
}

#[test]
fn raw_transcripts_do_not_inherit_narrow_scope_authority() {
    let (_root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    evidence(&db, &workspace, &session, 1, BODY);
    for scope in [
        MemoryScope::Global,
        MemoryScope::Verified,
        MemoryScope::SelfOnly,
        MemoryScope::Team,
    ] {
        let corpus = load_scoped_ask_corpus(&db, &workspace, Utc::now(), scope).unwrap();
        assert!(corpus.candidates.is_empty(), "{scope:?}");
        assert!(corpus.native_sources.is_empty(), "{scope:?}");
    }
    for scope in [MemoryScope::Workspace, MemoryScope::Swarm] {
        let corpus = load_scoped_ask_corpus(&db, &workspace, Utc::now(), scope).unwrap();
        assert_eq!(corpus.candidates.len(), 1, "{scope:?}");
    }
}

#[test]
fn foreign_workspace_transcripts_never_enter_the_answer_corpus() {
    let (_root, db, workspace) = fixture();
    let foreign = WorkspaceId::from_uuid(uuid::Uuid::from_u128(711)).to_string();
    db.insert_workspace(
        &foreign,
        &CreateWorkspaceInput {
            path: "/other-ask-evidence-workspace".to_owned(),
            name: None,
        },
    )
    .unwrap();
    let local_session = session(&db, &workspace, 1);
    let local = evidence(&db, &workspace, &local_session, 1, BODY);
    let foreign_session = session(&db, &foreign, 2);
    let other = evidence(&db, &foreign, &foreign_session, 2, BODY);
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(corpus.candidates.len(), 1);
    assert_eq!(corpus.candidates[0].memory_id, local);
    assert!(!corpus.native_sources.contains_key(&other));
}

#[test]
fn tampered_excerpt_is_excluded_before_answering_or_nearest_evidence() {
    let (_root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let id = evidence(&db, &workspace, &session, 1, BODY);
    db.execute_raw(&format!(
        "UPDATE evidence_spans SET excerpt = 'release password=ask-evidence-secret-canary' WHERE id = '{id}'"
    ))
    .unwrap();
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert!(corpus.candidates.is_empty());
    assert!(corpus.native_sources.is_empty());
    let report = answer(&corpus);
    assert!(report.abstained);
    assert!(
        !ask_data_json(&report)
            .to_string()
            .contains("ask-evidence-secret-canary")
    );
}

#[test]
fn evidence_is_read_in_the_same_snapshot_as_memories() {
    let (root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let original = evidence(&db, &workspace, &session, 1, BODY);
    let writer = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
    let during = load_corpus_with_boundary(&db, &workspace, Utc::now(), || {
        evidence(&writer, &workspace, &session, 2, BODY);
        Ok(())
    })
    .unwrap();
    assert_eq!(during.candidates.len(), 1);
    assert_eq!(during.candidates[0].memory_id, original);
    let after = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(after.candidates.len(), 2);
}

#[test]
fn unicode_evidence_citations_are_exact_slices_of_the_stored_excerpt() {
    let (_root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let body = "Résumé: Run cargo fmt before every release tag.";
    let id = evidence(&db, &workspace, &session, 1, body);
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    let report = answer(&corpus);
    assert!(!report.abstained, "{report:?}");
    let stored = db.get_evidence_span(&id).unwrap().unwrap();
    for citation in &report.citations {
        assert_eq!(citation.memory_id, id);
        assert_eq!(
            stored.excerpt.get(citation.byte_start..citation.byte_end),
            Some(citation.text.as_str())
        );
    }
}

#[test]
fn admission_is_rechecked_after_hash_revocation_and_repair() {
    let (_root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let id = evidence(&db, &workspace, &session, 1, BODY);
    let stored = db.get_evidence_span(&id).unwrap().unwrap();
    let before = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(before.candidates.len(), 1);

    let bad_hash = "0".repeat(64);
    db.execute_raw(&format!(
        "UPDATE evidence_spans SET content_hash = '{bad_hash}' WHERE id = '{id}'"
    ))
    .unwrap();
    let revoked = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert!(revoked.candidates.is_empty());
    assert!(revoked.native_sources.is_empty());
    assert!(answer(&revoked).abstained);

    // Restoring the original digest must be visible on the next read without
    // a search-index rebuild or a fresh connection. A prior denial is not a
    // permanent negative cache, and the first read did not pin its snapshot.
    let original_hash = stored.content_hash;
    db.execute_raw(&format!(
        "UPDATE evidence_spans SET content_hash = '{original_hash}' WHERE id = '{id}'"
    ))
    .unwrap();
    let repaired = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(repaired.candidates.len(), 1);
    assert_eq!(repaired.candidates[0].memory_id, id);
    assert_eq!(repaired.native_sources, before.native_sources);
}

#[test]
fn clean_metadata_and_a_matching_digest_do_not_authorize_secret_text() {
    let (_root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let id = evidence(&db, &workspace, &session, 1, BODY);
    let unsafe_body = "Run cargo fmt before every release tag. password=ask-private-canary";
    let hash = blake3::hash(unsafe_body.as_bytes()).to_hex().to_string();
    db.execute_raw(&format!(
        "UPDATE evidence_spans SET excerpt = '{unsafe_body}', content_hash = '{hash}' WHERE id = '{id}'"
    ))
    .unwrap();
    let stored = db.get_evidence_span(&id).unwrap().unwrap();
    assert_eq!(stored.excerpt, unsafe_body);
    assert_eq!(stored.content_hash, hash);

    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert!(corpus.candidates.is_empty());
    assert!(corpus.native_sources.is_empty());
    let report = answer(&corpus);
    assert!(report.abstained);
    assert!(!ask_data_json(&report).to_string().contains("ask-private-canary"));
    assert!(!render_ask_markdown(&report).contains("ask-private-canary"));
}
