#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{
    AskReport, AskRequest, ask_data_json, evaluate_ask, record_ask_retrieval_best_effort,
    render_ask_markdown,
};
use crate::db::{
    CreateEvidenceSpanInput, CreateFeedbackEventInput, CreateFeedbackQuarantineInput,
    CreateMemoryInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
};
use crate::models::{EvidenceId, MemoryId, SessionId, WorkspaceId};

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
            content_hash: format!(
                "blake3:{}",
                blake3::hash(format!("session-{n}").as_bytes()).to_hex()
            ),
            metadata_json: None,
        },
    )
    .unwrap();
    id
}

fn evidence(db: &DbConnection, workspace: &str, session: &str, n: u32, body: &str) -> String {
    evidence_with_parent(db, workspace, session, n, body, None)
}

fn evidence_with_parent(
    db: &DbConnection,
    workspace: &str,
    session: &str,
    n: u32,
    body: &str,
    parent: Option<&str>,
) -> String {
    let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(u128::from(n))).to_string();
    db.insert_evidence_span(
        &id,
        &CreateEvidenceSpanInput {
            workspace_id: workspace.to_owned(),
            session_id: session.to_owned(),
            memory_id: parent.map(str::to_owned),
            producer_kind: EvidenceProducerKind::CassImport,
            cass_span_id: format!("ask-span-{n}"),
            span_kind: crate::cass::CassSpanKind::Message.as_str().to_owned(),
            start_line: n,
            end_line: n,
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
    let span = db.get_evidence_span(&id).unwrap().unwrap();
    let session = db.get_session(session).unwrap().unwrap();
    assert!(
        span.is_direct_pack_admitted_for_session(workspace, &session),
        "seeded assistant excerpt {id} must have live admission"
    );
    id
}

fn request(corpus: &AskCorpus) -> AskRequest {
    AskRequest {
        question: QUESTION.to_owned(),
        contradictions: corpus.contradictions.clone(),
        native_sources: corpus.native_sources.clone(),
        evidence_harmful_counts: corpus.evidence_harmful_counts.clone(),
        ..AskRequest::default()
    }
}

fn answer(corpus: &AskCorpus) -> AskReport {
    evaluate_ask(&request(corpus), &corpus.candidates)
}

fn feedback(
    db: &DbConnection,
    workspace: &str,
    target_type: &str,
    target_id: &str,
    number: u32,
    created_at: &str,
) {
    let id = format!("fb_{number:026}");
    db.insert_feedback_event(
        &id,
        &CreateFeedbackEventInput {
            workspace_id: workspace.to_owned(),
            target_type: target_type.to_owned(),
            target_id: target_id.to_owned(),
            signal: "harmful".to_owned(),
            weight: 1.0,
            source_type: "outcome_observed".to_owned(),
            source_id: None,
            reason: None,
            evidence_json: None,
            session_id: None,
        },
    )
    .unwrap();
    db.execute_raw(&format!(
        "UPDATE feedback_events SET created_at = '{created_at}' WHERE id = '{id}'"
    ))
    .unwrap();
}

#[test]
fn accepted_evidence_feedback_changes_answers_at_the_exact_cutoff() {
    let (_root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let harmed = evidence(&db, &workspace, &session, 1, BODY);
    let clean = evidence(&db, &workspace, &session, 2, BODY);
    let cutoff = DateTime::parse_from_rfc3339("2026-09-28T12:00:00.123456789Z")
        .unwrap()
        .with_timezone(&Utc);
    let baseline = load_current_ask_corpus(&db, &workspace, cutoff).unwrap();
    let baseline_report = answer(&baseline);
    assert!(!baseline_report.abstained);
    assert_eq!(baseline_report.citations[0].memory_id, harmed);
    assert_eq!(baseline.evidence_harmful_counts.get(&harmed), Some(&0));
    feedback(
        &db,
        &workspace,
        "evidence",
        &harmed,
        1,
        "2026-09-28T14:00:00.123456789+02:00",
    );
    // A one-nanosecond future event must not demote today's clean answer.
    for number in 2..10 {
        feedback(
            &db,
            &workspace,
            "evidence",
            &clean,
            number,
            "2026-09-28T12:00:00.123456790Z",
        );
    }
    // Target kind is part of the identity even when the raw ID collides.
    feedback(
        &db,
        &workspace,
        "memory",
        &clean,
        10,
        "invalid-other-kind-time",
    );
    let foreign = WorkspaceId::from_uuid(uuid::Uuid::from_u128(711)).to_string();
    db.insert_workspace(
        &foreign,
        &CreateWorkspaceInput {
            path: "/other-ask-feedback-workspace".to_owned(),
            name: None,
        },
    )
    .unwrap();
    feedback(
        &db,
        &foreign,
        "evidence",
        &clean,
        11,
        "invalid-foreign-time",
    );
    db.insert_feedback_quarantine(
        "fq_01234567890123456789012345",
        &CreateFeedbackQuarantineInput {
            workspace_id: workspace.clone(),
            source_id: "test:pending-ask-evidence".to_owned(),
            target_type: "evidence".to_owned(),
            target_id: clean.clone(),
            signal: "harmful".to_owned(),
            weight: 1.0,
            source_type: "human_explicit".to_owned(),
            proposed_event_id: None,
            recorded_at: "2026-09-28T00:00:00Z".to_owned(),
            reason: "pending review".to_owned(),
            event_reason: None,
            evidence_json: None,
            session_id: None,
            raw_event_hash: format!("blake3:{}", "a".repeat(64)),
        },
    )
    .unwrap();
    let before =
        load_current_ask_corpus(&db, &workspace, cutoff - chrono::Duration::nanoseconds(1))
            .unwrap();
    assert_eq!(answer(&before).citations[0].memory_id, harmed);
    let corpus = load_current_ask_corpus(&db, &workspace, cutoff).unwrap();
    assert_eq!(corpus.evidence_harmful_counts.get(&harmed), Some(&1));
    assert_eq!(corpus.evidence_harmful_counts.get(&clean), Some(&0));
    let report = answer(&corpus);
    assert!(!report.abstained && !report.conflict_detected);
    assert_eq!(report.citations[0].memory_id, clean);
    assert_eq!(
        report.confidence.to_bits(),
        baseline_report.confidence.to_bits()
    );
    let stored = db.get_evidence_span(&clean).unwrap().unwrap();
    let data = ask_data_json(&report);
    assert_eq!(data["citations"][0]["evidenceId"], clean);
    assert_eq!(
        data["citations"][0]["entityRevision"],
        stored.pack_entity_revision()
    );
    assert_eq!(data["citations"][0]["confidence"], 0.5);
    assert_eq!(data["citations"][0]["text"], BODY);
    assert!(data["citations"][0].get("memoryId").is_none());
    let mut reversed = corpus.clone();
    reversed.candidates.reverse();
    assert_eq!(data, ask_data_json(&answer(&reversed)));
    let later = load_current_ask_corpus(&db, &workspace, cutoff + chrono::Duration::nanoseconds(1))
        .unwrap();
    assert_eq!(later.evidence_harmful_counts.get(&clean), Some(&8));
    let mut lower_floor = request(&later);
    lower_floor.min_confidence = 0.1;
    assert_eq!(
        evaluate_ask(&lower_floor, &later.candidates).citations[0].memory_id,
        harmed
    );
    // The configured risk penalty affects answer confidence, not the source's
    // confidence or its byte-exact abstention hint.
    let only_harmed: Vec<_> = corpus
        .candidates
        .iter()
        .filter(|candidate| candidate.memory_id == harmed)
        .cloned()
        .collect();
    let mut strict = request(&corpus);
    strict.feedback_scoring_config.harmful_penalty_per_hit = 0.8;
    let withheld = evaluate_ask(&strict, &only_harmed);
    assert!(withheld.abstained);
    assert!(withheld.citations.is_empty());
    assert_eq!(
        ask_data_json(&withheld)["nearestEvidence"][0]["evidenceId"],
        harmed
    );
    assert_eq!(only_harmed[0].confidence, 0.5);
}

#[test]
fn evidence_feedback_belongs_to_the_corpus_snapshot_and_unavailable_is_not_zero() {
    let (root, db, workspace) = fixture();
    let session = session(&db, &workspace, 1);
    let harmed = evidence(&db, &workspace, &session, 1, BODY);
    let clean = evidence(&db, &workspace, &session, 2, BODY);
    let cutoff = DateTime::parse_from_rfc3339("2026-09-28T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let writer = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
    let pinned = load_corpus_with_boundary(&db, &workspace, cutoff, || {
        feedback(
            &writer,
            &workspace,
            "evidence",
            &harmed,
            1,
            "2026-09-28T12:00:00Z",
        );
        Ok(())
    })
    .unwrap();
    assert_eq!(pinned.evidence_harmful_counts.get(&harmed), Some(&0));
    assert_eq!(answer(&pinned).citations[0].memory_id, harmed);
    let fresh = load_current_ask_corpus(&db, &workspace, cutoff).unwrap();
    assert_eq!(fresh.evidence_harmful_counts.get(&harmed), Some(&1));
    assert_eq!(answer(&fresh).citations[0].memory_id, clean);
    db.execute_raw("UPDATE feedback_events SET created_at = 'private-feedback-time-canary'")
        .unwrap();
    let error = load_current_ask_corpus(&db, &workspace, cutoff).unwrap_err();
    assert!(matches!(error, DomainError::Storage { .. }));
    assert!(!error.to_string().contains("private-feedback-time-canary"));
    assert!(!error.to_string().contains(&harmed));
    db.begin_read_snapshot().unwrap();
    db.rollback_read_snapshot().unwrap();
    // No evidence is admitted in this scope, so an irrelevant malformed
    // evidence event cannot introduce a new failure into a memory/rule ask.
    let narrow = load_scoped_ask_corpus(&db, &workspace, cutoff, MemoryScope::Global).unwrap();
    assert!(narrow.candidates.is_empty());
    assert!(narrow.evidence_harmful_counts.is_empty());
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

    let bad_hash = format!("blake3:{}", "0".repeat(64));
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
    let hash = format!("blake3:{}", blake3::hash(unsafe_body.as_bytes()).to_hex());
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
    assert!(
        !ask_data_json(&report)
            .to_string()
            .contains("ask-private-canary")
    );
    assert!(!render_ask_markdown(&report).contains("ask-private-canary"));
}

fn parent_memory(db: &DbConnection, workspace: &str) -> String {
    let id = MemoryId::from_uuid(uuid::Uuid::from_u128(77)).to_string();
    db.insert_memory(
        &id,
        &CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            content: BODY.to_owned(),
            level: "semantic".to_owned(),
            kind: "note".to_owned(),
            workflow_id: None,
            confidence: 0.95,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_uri: Some("manual://parent-admission".to_owned()),
            tags: Vec::new(),
            valid_from: Some("1990-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        },
    )
    .unwrap();
    id
}

#[test]
fn linked_evidence_follows_parent_admission_without_hiding_unlinked_evidence() {
    let (root, db, workspace) = fixture();
    let parent = parent_memory(&db, &workspace);
    let first_session = session(&db, &workspace, 1);
    let linked = evidence_with_parent(&db, &workspace, &first_session, 1, BODY, Some(&parent));
    let second_session = session(&db, &workspace, 2);
    let unlinked = evidence(&db, &workspace, &second_session, 2, BODY);
    let live = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(live.candidates.len(), 3);
    assert_eq!(
        live.native_sources[&linked].source_memory_ids,
        vec![parent.clone()]
    );
    for mutation in [
        "valid_to = '2000-01-01T00:00:00Z'",
        "valid_from = '2999-01-01T00:00:00Z'",
        "tombstoned_at = '2000-01-01T00:00:00Z'",
        "content = 'password=private-parent-canary'",
    ] {
        db.execute_raw(&format!(
            "UPDATE memories SET valid_from = '1990-01-01T00:00:00Z', valid_to = NULL, tombstoned_at = NULL, content = '{BODY}' WHERE id = '{parent}'"
        ))
        .unwrap();
        db.execute_raw(&format!(
            "UPDATE memories SET {mutation} WHERE id = '{parent}'"
        ))
        .unwrap();
        let withheld = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
        assert_eq!(withheld.candidates.len(), 1, "{mutation}");
        assert_eq!(withheld.candidates[0].memory_id, unlinked);
        assert!(!withheld.native_sources.contains_key(&linked));
        let output = ask_data_json(&answer(&withheld)).to_string();
        assert!(!output.contains(&linked));
        assert!(!output.contains(&parent));
        assert!(!output.contains("private-parent-canary"));
    }
    db.execute_raw(&format!(
        "UPDATE memories SET valid_from = '1990-01-01T00:00:00Z', valid_to = NULL, tombstoned_at = NULL, content = '{BODY}' WHERE id = '{parent}'"
    ))
    .unwrap();
    let restored = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(restored.candidates.len(), 3);
    assert!(restored.native_sources.contains_key(&linked));
    assert!(!root.path().join(".ee/index").exists());
}

#[test]
fn concurrent_parent_revocation_obeys_the_owned_evidence_snapshot() {
    let (root, db, workspace) = fixture();
    let parent = parent_memory(&db, &workspace);
    let session = session(&db, &workspace, 1);
    let linked = evidence_with_parent(&db, &workspace, &session, 1, BODY, Some(&parent));
    let writer = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
    let pinned = load_corpus_with_boundary(&db, &workspace, Utc::now(), || {
        writer
            .with_transaction(|| {
                writer.execute_raw(&format!(
                    "UPDATE memories SET valid_to = '2000-01-01T00:00:00Z' WHERE id = '{parent}'"
                ))
            })
            .unwrap();
        Ok(())
    })
    .unwrap();
    assert_eq!(pinned.candidates.len(), 2);
    assert!(pinned.native_sources.contains_key(&linked));
    let excerpt = pinned
        .candidates
        .iter()
        .find(|item| item.memory_id == linked)
        .unwrap();
    assert_eq!(excerpt.confidence, 0.5);
    assert_eq!(excerpt.trust_class, "cass_evidence");
    let current = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert!(current.candidates.is_empty());
    assert!(current.native_sources.is_empty());
    assert!(answer(&current).abstained);
}
