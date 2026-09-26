//! Real-store answer admission must agree with live pending-review holds.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{AskRequest, ask_data_json, evaluate_ask};
use crate::db::{
    CreateEvidenceSpanInput, CreateFeedbackQuarantineInput, CreateMemoryInput,
    CreateProceduralRuleInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
};
use crate::models::{EvidenceId, MemoryId, RuleId, SessionId, WorkspaceId};

const ANSWER: &str = "Run cargo fmt before every release tag.";
const QUESTION: &str = "Which command must run before every release tag?";
const TIME: &str = "2026-01-01T00:00:00Z";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2027-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

struct Fixture {
    db: DbConnection,
    root: tempfile::TempDir,
    workspace: String,
    other: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = DbConnection::open_file(root.path().join("ask.db")).unwrap();
        db.migrate().unwrap();
        let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(870)).to_string();
        let other = WorkspaceId::from_uuid(uuid::Uuid::from_u128(871)).to_string();
        for (id, path) in [(&workspace, "one"), (&other, "two")] {
            db.insert_workspace(
                id,
                &CreateWorkspaceInput {
                    path: root.path().join(path).to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
        }
        Self {
            db,
            root,
            workspace,
            other,
        }
    }

    fn memory(&self, number: u128) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: ANSWER.to_owned(),
                    workflow_id: None,
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("manual://reviewed-release".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: Some(TIME.to_owned()),
                    valid_to: None,
                },
            )
            .unwrap();
        id
    }

    fn rule(&self, number: u128, sources: Vec<String>) -> String {
        let id = RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_procedural_rule(
                &id,
                &CreateProceduralRuleInput {
                    workspace_id: self.workspace.clone(),
                    content: ANSWER.to_owned(),
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    scope: "global".to_owned(),
                    scope_pattern: None,
                    maturity: "candidate".to_owned(),
                    protected: true,
                    source_memory_ids: sources,
                    tags: Vec::new(),
                },
            )
            .unwrap();
        id
    }

    fn hold(&self, number: u32, workspace: &str, kind: &str, target: &str) -> String {
        let id = format!("fq_{number:026}");
        self.db
            .insert_feedback_quarantine(
                &id,
                &CreateFeedbackQuarantineInput {
                    workspace_id: workspace.to_owned(),
                    source_id: "PRIVATE-REVIEW-SOURCE".to_owned(),
                    target_type: kind.to_owned(),
                    target_id: target.to_owned(),
                    signal: "harmful".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    proposed_event_id: None,
                    recorded_at: TIME.to_owned(),
                    reason: "PRIVATE-REVIEW-REASON".to_owned(),
                    event_reason: None,
                    evidence_json: None,
                    session_id: None,
                    raw_event_hash: format!("blake3:{}", "a".repeat(64)),
                },
            )
            .unwrap();
        id
    }

    fn review(&self, id: &str, status: &str) {
        assert!(
            self.db
                .update_feedback_quarantine_status(id, status, Some("operator"), None)
                .unwrap()
        );
    }

    fn evidence(&self, number: u128, parent: Option<String>) -> String {
        let session = SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_session(
                &session,
                &CreateSessionInput {
                    workspace_id: self.workspace.clone(),
                    cass_session_id: format!("quarantine-session-{number}"),
                    source_path: None,
                    agent_name: Some("codex".to_owned()),
                    model: None,
                    started_at: Some(TIME.to_owned()),
                    ended_at: Some(TIME.to_owned()),
                    message_count: 1,
                    token_count: None,
                    content_hash: format!("blake3:{}", blake3::hash(session.as_bytes()).to_hex()),
                    metadata_json: None,
                },
            )
            .unwrap();
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: self.workspace.clone(),
                    session_id: session.clone(),
                    memory_id: parent,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("quarantine-span-{number}"),
                    span_kind: crate::cass::CassSpanKind::Message.as_str().to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: ANSWER.to_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(ANSWER.as_bytes()).to_hex()),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .unwrap();
        let span = self.db.get_evidence_span(&id).unwrap().unwrap();
        assert!(span.is_direct_pack_admitted_for_session(
            &self.workspace,
            &self.db.get_session(&session).unwrap().unwrap()
        ));
        id
    }

    fn corpus(&self) -> AskCorpus {
        load_current_ask_corpus(&self.db, &self.workspace, now()).unwrap()
    }
}

fn ids(corpus: &AskCorpus) -> BTreeSet<String> {
    corpus
        .candidates
        .iter()
        .map(|candidate| candidate.memory_id.clone())
        .collect()
}

fn answer(corpus: &AskCorpus) -> crate::core::ask::AskReport {
    evaluate_ask(
        &AskRequest {
            question: QUESTION.to_owned(),
            native_sources: corpus.native_sources.clone(),
            contradictions: corpus.contradictions.clone(),
            ..AskRequest::default()
        },
        &corpus.candidates,
    )
}

#[test]
fn held_memory_cannot_supply_an_answer_citation_or_nearest_evidence_hint() {
    let fixture = Fixture::new();
    let memory = fixture.memory(880);
    assert!(!answer(&fixture.corpus()).abstained);
    fixture.hold(1, &fixture.workspace, "memory", &memory);
    let before = fixture.db.get_memory(&memory).unwrap().unwrap();
    let generation = fixture
        .db
        .get_workspace_generation(&fixture.workspace)
        .unwrap();
    let audit_count = fixture.db.count_table_rows("audit_log").unwrap();
    let corpus = fixture.corpus();
    assert!(corpus.candidates.is_empty());
    assert!(corpus.contradictions.is_empty());
    let report = answer(&corpus);
    assert!(report.abstained && report.citations.is_empty());
    let public = ask_data_json(&report).to_string();
    for private in [
        memory.as_str(),
        ANSWER,
        "PRIVATE-REVIEW-SOURCE",
        "PRIVATE-REVIEW-REASON",
    ] {
        assert!(!public.contains(private));
    }
    assert_eq!(fixture.db.get_memory(&memory).unwrap().unwrap(), before);
    assert_eq!(
        fixture
            .db
            .get_workspace_generation(&fixture.workspace)
            .unwrap(),
        generation
    );
    assert_eq!(fixture.db.count_table_rows("audit_log").unwrap(), audit_count);
}

#[test]
fn holds_require_exact_workspace_and_target_kind_and_all_pending_events_must_close() {
    let fixture = Fixture::new();
    let memory = fixture.memory(881);
    fixture.hold(1, &fixture.other, "memory", &memory);
    fixture.hold(2, &fixture.workspace, "rule", &memory);
    assert!(ids(&fixture.corpus()).contains(&memory));
    let first = fixture.hold(3, &fixture.workspace, "memory", &memory);
    let second = fixture.hold(4, &fixture.workspace, "memory", &memory);
    assert!(fixture.corpus().candidates.is_empty());
    fixture.review(&first, "released");
    assert!(fixture.corpus().candidates.is_empty());
    fixture.review(&second, "rejected");
    assert!(ids(&fixture.corpus()).contains(&memory));
}

#[test]
fn native_rule_holds_do_not_alias_lineage_or_respect_protection_as_an_override() {
    let fixture = Fixture::new();
    let memory = fixture.memory(882);
    let rule = fixture.rule(883, vec![memory.clone()]);
    let source_hold = fixture.hold(1, &fixture.workspace, "memory", &memory);
    let corpus = fixture.corpus();
    assert_eq!(ids(&corpus), BTreeSet::from([rule.clone()]));
    assert_eq!(
        corpus.native_sources[&rule].source_memory_ids,
        [memory.clone()]
    );
    fixture.review(&source_hold, "rejected");
    fixture.hold(2, &fixture.workspace, "rule", &rule);
    assert_eq!(ids(&fixture.corpus()), BTreeSet::from([memory]));
    assert!(!fixture.corpus().native_sources.contains_key(&rule));
}

#[test]
fn linked_evidence_cannot_resurrect_a_held_memory_but_independent_evidence_remains() {
    let fixture = Fixture::new();
    let memory = fixture.memory(884);
    let linked = fixture.evidence(885, Some(memory.clone()));
    let independent = fixture.evidence(886, None);
    assert!(ids(&fixture.corpus()).contains(&linked));
    fixture.hold(1, &fixture.workspace, "memory", &memory);
    let corpus = fixture.corpus();
    assert_eq!(ids(&corpus), BTreeSet::from([independent.clone()]));
    assert!(!corpus.native_sources.contains_key(&linked));
    assert!(corpus.native_sources.contains_key(&independent));
}

#[test]
fn historical_and_verified_scope_cannot_bypass_current_review_and_advice_uses_same_hold() {
    let fixture = Fixture::new();
    let memory = fixture.memory(887);
    let rule = fixture.rule(888, Vec::new());
    fixture.hold(1, &fixture.workspace, "memory", &memory);
    fixture.hold(2, &fixture.workspace, "rule", &rule);
    for scope in [
        MemoryScope::Workspace,
        MemoryScope::Verified,
        MemoryScope::Global,
    ] {
        let corpus = load_scoped_ask_corpus(&fixture.db, &fixture.workspace, now(), scope).unwrap();
        assert!(corpus.candidates.is_empty());
    }
    let historical = DateTime::parse_from_rfc3339(TIME)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        load_current_ask_corpus(&fixture.db, &fixture.workspace, historical)
            .unwrap()
            .candidates
            .is_empty()
    );
    assert!(
        load_command_advice_corpus(&fixture.db, &fixture.workspace, now())
            .unwrap()
            .candidates
            .is_empty()
    );
}

#[test]
fn a_concurrent_hold_only_changes_the_next_owned_answer_snapshot() {
    let fixture = Fixture::new();
    let memory = fixture.memory(889);
    let rule = fixture.rule(890, Vec::new());
    let reader = DbConnection::open_file_read_only(fixture.root.path().join("ask.db")).unwrap();
    let captured = load_corpus_with_boundary(&reader, &fixture.workspace, now(), || {
        fixture.hold(1, &fixture.workspace, "memory", &memory);
        fixture.hold(2, &fixture.workspace, "rule", &rule);
        Ok(())
    })
    .unwrap();
    assert_eq!(ids(&captured), BTreeSet::from([memory, rule]));
    assert!(
        load_current_ask_corpus(&reader, &fixture.workspace, now())
            .unwrap()
            .candidates
            .is_empty()
    );
    reader.begin_read_snapshot().unwrap();
    reader.rollback_read_snapshot().unwrap();
}

#[test]
fn unreadable_review_authority_withholds_answers_without_leaking_or_leaving_a_snapshot() {
    for rules_only in [false, true] {
        let fixture = Fixture::new();
        if rules_only {
            fixture.rule(891, Vec::new());
        } else {
            fixture.memory(892);
        }
        fixture
            .db
            .execute_raw("ALTER TABLE feedback_quarantine RENAME TO unavailable_private_review")
            .unwrap();
        let error = load_current_ask_corpus(&fixture.db, &fixture.workspace, now()).unwrap_err();
        assert!(matches!(error, DomainError::Storage { .. }));
        let diagnostic = format!("{error:?}");
        assert!(diagnostic.contains("pending-review authority"));
        assert!(!diagnostic.contains("unavailable_private_review"));
        fixture.db.begin_read_snapshot().unwrap();
        fixture.db.rollback_read_snapshot().unwrap();
    }
}

#[test]
fn review_lookup_is_bind_safe_deduplicated_and_pages_past_512_candidates() {
    let fixture = Fixture::new();
    let candidates = (1000..1513)
        .map(|number| MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string())
        .collect::<Vec<_>>();
    let mut expected = BTreeSet::new();
    for (number, index) in [0, 255, 256, 512].into_iter().enumerate() {
        fixture.hold(
            number as u32 + 1,
            &fixture.workspace,
            "memory",
            &candidates[index],
        );
        expected.insert(candidates[index].clone());
    }
    let mut inputs = candidates.iter().map(String::as_str).collect::<Vec<_>>();
    inputs.extend([candidates[0].as_str(), "' OR 1 = 1 --"]);
    let actual = quarantine::held_ids(
        &fixture.db,
        &fixture.workspace,
        quarantine::Target::Memory,
        &inputs,
    )
    .unwrap();
    assert_eq!(actual, expected);
    assert!(
        quarantine::held_ids(
            &fixture.db,
            "' OR 1 = 1 --",
            quarantine::Target::Memory,
            &inputs,
        )
        .unwrap()
        .is_empty()
    );
}
