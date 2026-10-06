//! Regression coverage for reusing the paged reader's source admission.

use std::collections::BTreeMap;

use chrono::Utc;

use crate::core::ask::{AskCorpus, load_current_ask_corpus};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind, StoredEvidenceSpan,
};
use crate::models::{EvidenceId, MemoryScope, SessionId, WorkspaceId};

const BODY: &str = "Run cargo fmt before release.";

struct Fixture {
    root: tempfile::TempDir,
    db: DbConnection,
    workspace: String,
    session: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".ee")).unwrap();
        let db = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
        db.migrate().unwrap();
        let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(91001)).to_string();
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        let session = SessionId::from_uuid(uuid::Uuid::from_u128(91002)).to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: workspace.clone(),
                cass_session_id: "private-upstream-session".to_owned(),
                source_path: Some("/home/private/transcript.jsonl".to_owned()),
                agent_name: None,
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: format!("blake3:{}", "a".repeat(64)),
                metadata_json: None,
            },
        )
        .unwrap();
        Self {
            root,
            db,
            workspace,
            session,
        }
    }

    fn evidence(&self, number: u128, body: &str) -> StoredEvidenceSpan {
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: self.workspace.clone(),
                    session_id: self.session.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("private-upstream-span-{number}"),
                    span_kind: "message".to_owned(),
                    // Deliberately tie every line window: paging must retain
                    // the evidence ID tie-breaker instead of dropping rows.
                    start_line: 7,
                    end_line: 8,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: body.to_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex()),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .unwrap();
        self.db.get_evidence_span(&id).unwrap().unwrap()
    }

    fn corpus(&self) -> AskCorpus {
        load_current_ask_corpus(&self.db, &self.workspace, Utc::now()).unwrap()
    }
}

#[test]
fn joined_admission_matches_full_predicate_across_tied_keyset_pages() {
    let fixture = Fixture::new();
    fixture
        .db
        .with_transaction(|| {
            for number in 1..=270 {
                fixture.evidence(number, BODY);
            }
            Ok(())
        })
        .unwrap();
    // The oracle uses the old, full live-row/session admission predicate,
    // independently of the paged visitor used by the production loader.
    let session = fixture.db.get_session(&fixture.session).unwrap().unwrap();
    let expected: BTreeMap<_, _> = fixture
        .db
        .list_evidence_spans_for_workspace(&fixture.workspace)
        .unwrap()
        .into_iter()
        .filter(|span| span.is_direct_pack_admitted_for_session(&fixture.workspace, &session))
        .map(|span| {
            (
                span.id.clone(),
                (
                    span.excerpt.clone(),
                    span.pack_entity_revision(),
                    span.canonical_provenance_uri(),
                ),
            )
        })
        .collect();
    let corpus = fixture.corpus();
    assert_eq!(expected.len(), 270);
    assert_eq!(corpus.candidates.len(), expected.len());
    assert_eq!(corpus.native_sources.len(), expected.len());
    let actual: BTreeMap<_, _> = corpus
        .candidates
        .iter()
        .map(|candidate| {
            assert_eq!(candidate.trust_class, "cass_evidence");
            assert_eq!(candidate.confidence, 0.5);
            (
                candidate.memory_id.clone(),
                (
                    candidate.content.clone(),
                    corpus.native_sources[&candidate.memory_id]
                        .entity_revision
                        .clone(),
                    candidate.provenance_uri.clone().unwrap(),
                ),
            )
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn pack_denial_hash_drift_and_private_bodies_remain_withheld() {
    let fixture = Fixture::new();
    let safe = fixture.evidence(1, BODY);
    let denied = fixture.evidence(2, BODY);
    let tampered = fixture.evidence(3, BODY);
    fixture.evidence(
        4,
        "Run cargo fmt using file:///home/private/withheld-canary.",
    );
    fixture
        .db
        .execute_raw(&format!(
            "UPDATE evidence_spans SET pack_eligibility = 'denied' WHERE id = '{}'",
            denied.id
        ))
        .unwrap();
    fixture
        .db
        .execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'withheld-canary' WHERE id = '{}'",
            tampered.id
        ))
        .unwrap();
    let corpus = fixture.corpus();
    assert_eq!(corpus.candidates.len(), 1);
    assert_eq!(corpus.candidates[0].memory_id, safe.id);
    assert_eq!(corpus.native_sources.len(), 1);
}

#[test]
fn joined_session_cannot_authorize_a_foreign_workspace() {
    let fixture = Fixture::new();
    fixture.evidence(1, BODY);
    assert_eq!(fixture.corpus().candidates.len(), 1);
    let foreign = WorkspaceId::from_uuid(uuid::Uuid::from_u128(91003)).to_string();
    fixture
        .db
        .insert_workspace(
            &foreign,
            &CreateWorkspaceInput {
                path: fixture
                    .root
                    .path()
                    .join("foreign")
                    .to_string_lossy()
                    .into_owned(),
                name: None,
            },
        )
        .unwrap();
    fixture
        .db
        .execute_raw(&format!(
            "UPDATE sessions SET workspace_id = '{foreign}' WHERE id = '{}'",
            fixture.session
        ))
        .unwrap();
    assert!(fixture.corpus().candidates.is_empty());
}

#[test]
fn narrower_scopes_cannot_inherit_transcript_admission() {
    let fixture = Fixture::new();
    fixture.evidence(1, BODY);
    for scope in [
        MemoryScope::SelfOnly,
        MemoryScope::Team,
        MemoryScope::Global,
        MemoryScope::Verified,
    ] {
        let mut candidates = Vec::new();
        let mut sources = BTreeMap::new();
        fixture.db.begin_read_snapshot().unwrap();
        super::append_evidence(
            &fixture.db,
            &fixture.workspace,
            scope,
            &mut candidates,
            &mut sources,
        )
        .unwrap();
        fixture.db.commit_read_snapshot().unwrap();
        assert!(candidates.is_empty());
        assert!(sources.is_empty());
    }
}

#[test]
fn pack_revocation_is_observed_at_the_next_snapshot_not_mid_read() {
    let fixture = Fixture::new();
    let span = fixture.evidence(1, BODY);
    let writer = DbConnection::open_file(&fixture.root.path().join(".ee/ee.db")).unwrap();
    let pinned = super::super::load_corpus_with_boundary(
        &fixture.db,
        &fixture.workspace,
        Utc::now(),
        || {
            writer
                .with_transaction(|| {
                    writer.execute_raw(&format!(
                        "UPDATE evidence_spans SET pack_eligibility = 'denied' WHERE id = '{}'",
                        span.id
                    ))
                })
                .unwrap();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(pinned.candidates.len(), 1);
    assert_eq!(pinned.candidates[0].memory_id, span.id);
    assert!(fixture.corpus().candidates.is_empty());
}

#[test]
fn evidence_read_failure_withholds_answers_and_releases_the_snapshot() {
    let fixture = Fixture::new();
    fixture
        .db
        .execute_raw("ALTER TABLE evidence_spans RENAME TO unavailable_evidence")
        .unwrap();
    let error = load_current_ask_corpus(&fixture.db, &fixture.workspace, Utc::now()).unwrap_err();
    assert!(!error.message().contains("unavailable_evidence"));
    fixture.db.begin_read_snapshot().unwrap();
    fixture.db.commit_read_snapshot().unwrap();
}
