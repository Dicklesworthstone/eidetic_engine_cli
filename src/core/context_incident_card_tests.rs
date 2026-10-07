//! Pack-lane coverage for derived incident cards (ADR 0091): a matched turn
//! brings its covering card, and cards of one error class collapse.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput};
use serde_json::json;

struct Fixture {
    workspace_id: String,
    db: DbConnection,
}

impl Fixture {
    fn new() -> Self {
        let workspace_id =
            crate::models::WorkspaceId::from_uuid(uuid::Uuid::from_u128(0x59)).to_string();
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: "/tmp/incident-card-pack".to_owned(),
                name: None,
            },
        )
        .unwrap();
        Self { workspace_id, db }
    }

    fn session(&self, seed: u128) -> String {
        let id = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        self.db
            .insert_session(
                &id,
                &CreateSessionInput {
                    workspace_id: self.workspace_id.clone(),
                    cass_session_id: format!("/sessions/{seed}.jsonl"),
                    source_path: None,
                    agent_name: Some("claude_code".to_owned()),
                    model: None,
                    started_at: None,
                    ended_at: None,
                    message_count: 0,
                    token_count: None,
                    content_hash: format!("blake3:{}", blake3::hash(&seed.to_le_bytes()).to_hex()),
                    metadata_json: None,
                },
            )
            .unwrap();
        id
    }

    fn line(
        &self,
        session_id: &str,
        number: u32,
        span_kind: &str,
        role: &str,
        record: &serde_json::Value,
    ) -> String {
        let excerpt = record.to_string();
        let digest = blake3::hash(format!("{session_id}:{number}").as_bytes());
        let mut seed = [0_u8; 16];
        seed.copy_from_slice(&digest.as_bytes()[..16]);
        let id = EvidenceId::from_uuid(uuid::Uuid::from_bytes(seed)).to_string();
        self.db
            .insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: self.workspace_id.clone(),
                    session_id: session_id.to_owned(),
                    memory_id: None,
                    producer_kind: crate::db::EvidenceProducerKind::CassImport,
                    cass_span_id: format!("{session_id}:{number}"),
                    span_kind: span_kind.to_owned(),
                    start_line: number,
                    end_line: number,
                    start_byte: None,
                    end_byte: None,
                    role: Some(role.to_owned()),
                    content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                    excerpt,
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .unwrap();
        id
    }

    /// One fixed compile error: call, failing result, explaining turn,
    /// verifying call and result. Returns (repair turn id, card id).
    fn incident(&self, seed: u128, explanation: &str) -> (String, String) {
        let session_id = self.session(seed);
        self.line(
            &session_id,
            1,
            "tool_call",
            "assistant",
            &json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "cargo build"}}
            ]}}),
        );
        let failure = self.line(
            &session_id,
            2,
            "tool_result",
            "user",
            &json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "is_error": true,
                 "content": "error[E0277]: the trait bound `Widget: Serialize` is not satisfied"}
            ]}}),
        );
        let repair = self.line(
            &session_id,
            3,
            "message",
            "assistant",
            &json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "text", "text": explanation}
            ]}}),
        );
        self.line(
            &session_id,
            4,
            "tool_call",
            "assistant",
            &json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_2", "name": "Bash", "input": {"command": "cargo test"}}
            ]}}),
        );
        self.line(
            &session_id,
            5,
            "tool_result",
            "user",
            &json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_2", "is_error": false,
                 "content": "test result: ok. 2 passed"}
            ]}}),
        );
        let report = crate::core::cass_error_recall::record_session_error_recall(
            &self.db,
            &self.workspace_id,
            &session_id,
        )
        .unwrap();
        assert_eq!(report.incident_cards_recorded, 1);
        (
            repair,
            crate::core::incident_card::incident_card_id(&self.workspace_id, &failure),
        )
    }

    fn candidate(&self, evidence_id: &str, relevance: f32) -> DirectEvidencePackCandidate {
        let span = self.db.get_evidence_span(evidence_id).unwrap().unwrap();
        direct_evidence_pack_candidate(span, relevance, "lexical", "widget serialize", None)
            .unwrap()
    }
}

#[test]
fn a_matched_turn_brings_its_card_and_one_card_speaks_for_its_error_class() {
    let fixture = Fixture::new();
    let (first_turn, first_card) = fixture.incident(
        0x59_0101,
        "Widget needs to derive Serialize because the store writes it. Added the derive in src/widget.rs.",
    );
    let (second_turn, second_card) = fixture.incident(
        0x59_0102,
        "The Widget type was missing its Serialize derive, so I added it and the bound is satisfied.",
    );
    assert_ne!(first_card, second_card);

    let unrelated_session = fixture.session(0x59_0103);
    let unrelated = fixture.line(
        &unrelated_session,
        1,
        "message",
        "user",
        &json!({"type": "user", "message": {"role": "user", "content": "Please serialize the widget cache too."}}),
    );

    let preferred = prefer_incident_cards(
        &fixture.db,
        std::slice::from_ref(&fixture.workspace_id),
        &crate::models::QueryFilters::default(),
        "widget serialize",
        vec![
            fixture.candidate(&first_turn, 0.9),
            fixture.candidate(&unrelated, 0.7),
            fixture.candidate(&second_turn, 0.6),
        ],
    );
    let ids = preferred
        .iter()
        .map(|candidate| candidate.item.evidence_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![first_card.as_str(), unrelated.as_str()]);

    let card = &preferred[0];
    assert!(card.incident_card);
    assert_eq!(
        card.item.trust.subclass.as_deref(),
        Some("derived_incident_card")
    );
    assert_eq!(
        card.item.relevance.into_inner(),
        0.9,
        "the card takes the turn's rank"
    );
    assert_eq!((card.item.start_line, card.item.end_line), (1, 5));
    assert!(
        card.item
            .content
            .starts_with(crate::core::incident_card::INCIDENT_CARD_PREFIX)
    );
    assert!(
        card.item
            .why
            .contains(&format!("through its source turn {first_turn}")),
        "{}",
        card.item.why
    );
    assert!(
        card.item
            .why
            .contains("error class rustc:E0277 was seen in 2 incidents"),
        "{}",
        card.item.why
    );
    assert!(!preferred[1].incident_card);

    // A card that matched directly keeps its own rank; its turn adds nothing.
    let direct = prefer_incident_cards(
        &fixture.db,
        std::slice::from_ref(&fixture.workspace_id),
        &crate::models::QueryFilters::default(),
        "widget serialize",
        vec![
            fixture.candidate(&first_turn, 0.9),
            fixture.candidate(&first_card, 0.5),
        ],
    );
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].item.evidence_id, first_card);
    assert_eq!(direct[0].item.relevance.into_inner(), 0.5);
}
