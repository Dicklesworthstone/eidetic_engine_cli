#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::super::{SESSION_ITEM_CAP, build_resume_report};
use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateMemoryInput, CreateSessionInput, CreateWorkspaceInput,
    EvidenceProducerKind, StoredEvidenceSpan,
};
use crate::models::{EvidenceId, MemoryId, SessionId};
use std::path::PathBuf;

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    workspace_id: String,
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
                    path: workspace.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
        Self {
            _root: root,
            workspace,
            database,
            workspace_id,
            writer,
        }
    }

    fn options(&self, sessions: usize) -> ResumeOptions<'_> {
        ResumeOptions {
            workspace_path: &self.workspace,
            database_path: &self.database,
            sessions,
        }
    }

    fn session(&self, number: u128, start: Option<&str>, end: Option<&str>) -> String {
        let id = SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.writer
            .insert_session(
                &id,
                &CreateSessionInput {
                    workspace_id: self.workspace_id.clone(),
                    cass_session_id: format!("PRIVATE-UPSTREAM-{number}"),
                    source_path: Some("/home/private/transcript.jsonl".to_owned()),
                    agent_name: Some("PRIVATE-AGENT".to_owned()),
                    model: None,
                    started_at: start.map(str::to_owned),
                    ended_at: end.map(str::to_owned),
                    message_count: 1,
                    token_count: None,
                    content_hash: format!("blake3:{}", "a".repeat(64)),
                    metadata_json: None,
                },
            )
            .unwrap();
        id
    }

    fn evidence(
        &self,
        session: &str,
        number: u128,
        line: u64,
        body: &str,
        parent: Option<&str>,
    ) -> StoredEvidenceSpan {
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.writer
            .insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: self.workspace_id.clone(),
                    session_id: session.to_owned(),
                    memory_id: parent.map(str::to_owned),
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("PRIVATE-SPAN-{number}"),
                    span_kind: "message".to_owned(),
                    start_line: line as _,
                    end_line: (line + 1) as _,
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
        self.writer.get_evidence_span(&id).unwrap().unwrap()
    }

    fn memory(&self, number: u128) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.writer
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace_id.clone(),
                    level: "episodic".to_owned(),
                    kind: "note".to_owned(),
                    content: "Completed the migration; validate the next release.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    provenance_uri: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
        id
    }

    fn state(&self, sessions: usize) -> ResumeState {
        let reader = DbConnection::open_file_read_only(&self.database).unwrap();
        load(
            &reader,
            &self.options(sessions),
            &self.workspace,
            reference_time(),
        )
        .unwrap()
    }
}

fn reference_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn transcript_only_resume_preserves_native_identity_and_does_not_retarget_the_store() {
    let fixture = Fixture::new();
    let session = fixture.session(
        1,
        Some("2020-01-01T09:00:00Z"),
        Some("2020-01-01T10:00:00Z"),
    );
    let row = fixture.evidence(
        &session,
        2,
        7,
        "Café: migration completed; release validation remains.",
        None,
    );
    let before = [
        "memories",
        "sessions",
        "evidence_spans",
        "audit_log",
        "search_index_jobs",
    ]
    .map(|table| fixture.writer.count_table_rows(table).unwrap());
    let report = build_resume_report(&fixture.options(3)).unwrap();
    assert_eq!(
        report.episodic_total, 0,
        "do not relabel evidence as memory rows"
    );
    assert!(report.sessions.is_empty());
    assert!(
        report.nearby_stores.is_none(),
        "the addressed store has usable history"
    );
    assert_eq!(report.transcript_history.session_total, 1);
    assert_eq!(report.transcript_history.evidence_total, 1);
    let native = &report.transcript_history.sessions[0];
    assert_eq!(native.session_id, session);
    assert_eq!(native.activity_basis, "source_ended_at");
    assert_eq!(native.activity_at.as_deref(), Some("2020-01-01T10:00:00Z"));
    let item = &native.items[0];
    assert_eq!(item.evidence_id, row.id);
    assert_eq!(item.entity_revision, row.pack_entity_revision());
    assert_eq!(item.entity_kind, "evidence_span");
    assert_eq!(item.provenance_uri, row.canonical_provenance_uri());
    assert_eq!(item.trust_class, "cass_evidence");
    assert_eq!(item.content, row.excerpt);
    assert!(!item.content_truncated && !native.items_truncated);
    assert_eq!(
        &row.excerpt[item.content_byte_start..item.content_byte_end],
        item.content
    );
    assert!(report.open_loops.tagged_items.is_empty());
    assert!(report.open_loops.revisit_decisions.is_empty());
    assert!(report.next_commands[0].contains(&session));
    assert!(report.next_commands.len() <= 5);
    let data = serde_json::to_value(&report).unwrap();
    assert!(
        data["transcriptHistory"]["sessions"][0]["items"][0]
            .get("memoryId")
            .is_none()
    );
    for private in [
        "PRIVATE-UPSTREAM",
        "PRIVATE-AGENT",
        "PRIVATE-SPAN",
        "/home/private",
    ] {
        assert!(!data.to_string().contains(private));
    }
    let after = [
        "memories",
        "sessions",
        "evidence_spans",
        "audit_log",
        "search_index_jobs",
    ]
    .map(|table| fixture.writer.count_table_rows(table).unwrap());
    assert_eq!(before, after);
    assert_eq!(
        fixture.writer.get_evidence_span(&row.id).unwrap().unwrap(),
        row
    );
    assert!(!fixture.workspace.join(".ee/index").exists());
}

#[test]
fn source_chronology_not_ingestion_order_controls_recent_session_selection() {
    let fixture = Fixture::new();
    let recent = fixture.session(1, None, Some("2020-01-02T05:00:00-05:00"));
    fixture.evidence(&recent, 11, 1, "Recent source activity.", None);
    let old = fixture.session(2, Some("2020-01-01T10:00:00Z"), None);
    fixture.evidence(&old, 12, 1, "Old source imported later.", None);
    let unknown = fixture.session(3, None, None);
    fixture.evidence(&unknown, 13, 1, "Undated source.", None);
    let history = fixture.state(2).transcript_history;
    assert_eq!(history.session_total, 3);
    assert_eq!(history.evidence_total, 3);
    assert!(history.sessions_truncated);
    assert_eq!(history.sessions.len(), 2);
    assert_eq!(history.sessions[0].session_id, recent);
    assert_eq!(
        history.sessions[0].activity_at.as_deref(),
        Some("2020-01-02T10:00:00Z")
    );
    assert_eq!(history.sessions[1].session_id, old);
    assert_eq!(history.sessions[1].activity_basis, "source_started_at");
    let history = fixture.state(3).transcript_history;
    assert_eq!(history.sessions[2].session_id, unknown);
    assert_eq!(history.sessions[2].activity_basis, "unknown");
    assert!(history.sessions[2].activity_at.is_none());
}

#[test]
fn tail_windows_are_bounded_and_counts_cover_the_entire_admitted_session() {
    let fixture = Fixture::new();
    let session = fixture.session(1, None, None);
    for number in (1..=SESSION_ITEM_CAP + 7).rev() {
        fixture.evidence(
            &session,
            number as u128,
            number as u64,
            &format!("Window {number}."),
            None,
        );
    }
    let history = fixture.state(1).transcript_history;
    assert_eq!(history.evidence_total, SESSION_ITEM_CAP + 7);
    let session = &history.sessions[0];
    assert_eq!(session.admitted_span_count, SESSION_ITEM_CAP + 7);
    assert!(session.items_truncated);
    assert_eq!(session.items.len(), SESSION_ITEM_CAP);
    assert_eq!(session.items[0].start_line, (SESSION_ITEM_CAP + 7) as u64);
    assert_eq!(session.items.last().unwrap().start_line, 8);
    assert!(
        session
            .items
            .windows(2)
            .all(|pair| pair[0].start_line > pair[1].start_line)
    );
}

#[test]
fn denied_and_tampered_excerpts_do_not_enter_counts_or_session_hints() {
    let fixture = Fixture::new();
    let safe_session = fixture.session(1, None, None);
    let denied_session = fixture.session(2, None, Some("2020-01-03T00:00:00Z"));
    let safe = fixture.evidence(&safe_session, 11, 1, "The build completed.", None);
    let denied = fixture.evidence(&denied_session, 12, 1, "DENIED-CANARY", None);
    let tampered = fixture.evidence(&denied_session, 13, 3, "Original source.", None);
    fixture
        .writer
        .execute_raw(&format!(
            "UPDATE evidence_spans SET search_eligibility = 'denied' WHERE id = '{}'",
            denied.id
        ))
        .unwrap();
    fixture
        .writer
        .execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'TAMPERED-CANARY' WHERE id = '{}'",
            tampered.id
        ))
        .unwrap();
    let report = build_resume_report(&fixture.options(3)).unwrap();
    assert_eq!(report.transcript_history.session_total, 1);
    assert_eq!(report.transcript_history.evidence_total, 1);
    assert_eq!(
        report.transcript_history.sessions[0].items[0].evidence_id,
        safe.id
    );
    let output = serde_json::to_string(&report).unwrap();
    for hidden in [
        &denied.id,
        &tampered.id,
        &denied_session,
        "DENIED-CANARY",
        "TAMPERED-CANARY",
    ] {
        assert!(!output.contains(hidden));
    }
}

#[test]
fn linked_evidence_cannot_resurrect_a_retired_or_closed_seal_memory() {
    let fixture = Fixture::new();
    let session = fixture.session(1, None, None);
    let parent = fixture.memory(2);
    fixture.evidence(&session, 3, 1, "Linked history.", Some(&parent));
    assert_eq!(fixture.state(3).transcript_history.evidence_total, 1);
    fixture
        .writer
        .insert_memory_seal(
            &parent,
            &crate::models::memory_seal_commitment(b"private history"),
            "2020-01-01T00:00:00Z",
        )
        .unwrap();
    assert_eq!(fixture.state(3).transcript_history.evidence_total, 0);
    fixture
        .writer
        .mark_memory_seal_revealed(&parent, "2020-01-02T00:00:00Z")
        .unwrap();
    assert_eq!(fixture.state(3).transcript_history.evidence_total, 1);
    fixture
        .writer
        .execute_raw(&format!(
            "UPDATE memories SET valid_to = '2001-01-01T00:00:00Z' WHERE id = '{parent}'"
        ))
        .unwrap();
    assert_eq!(fixture.state(3).transcript_history.evidence_total, 0);
}

#[test]
fn concurrent_revocation_changes_the_next_bundle_not_half_the_current_snapshot() {
    let fixture = Fixture::new();
    let session = fixture.session(1, None, None);
    let row = fixture.evidence(&session, 2, 1, "Current source.", None);
    let reader = DbConnection::open_file_read_only(&fixture.database).unwrap();
    let state = load_with_boundary(
        &reader,
        &fixture.options(3),
        &fixture.workspace,
        reference_time(),
        || {
            fixture
                .writer
                .execute_raw(&format!(
                    "UPDATE evidence_spans SET search_eligibility = 'denied' WHERE id = '{}'",
                    row.id
                ))
                .unwrap();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(state.transcript_history.evidence_total, 1);
    assert_eq!(
        state.transcript_history.sessions[0].items[0].evidence_id,
        row.id
    );
    assert_eq!(fixture.state(3).transcript_history.evidence_total, 0);
}

#[test]
fn concurrent_chronology_update_is_not_mixed_with_old_memory_authority() {
    let fixture = Fixture::new();
    let session = fixture.session(1, None, Some("2020-01-01T00:00:00Z"));
    fixture.evidence(&session, 2, 1, "Current source.", None);
    let reader = DbConnection::open_file_read_only(&fixture.database).unwrap();
    let state =
        load_with_boundary(
            &reader,
            &fixture.options(3),
            &fixture.workspace,
            reference_time(),
            || {
                fixture.writer.execute_raw(&format!(
                "UPDATE sessions SET ended_at = '2020-01-02T00:00:00Z' WHERE id = '{session}'"
            )).unwrap();
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(
        state.transcript_history.sessions[0].activity_at.as_deref(),
        Some("2020-01-01T00:00:00Z")
    );
    assert_eq!(
        fixture.state(3).transcript_history.sessions[0]
            .activity_at
            .as_deref(),
        Some("2020-01-02T00:00:00Z")
    );
}

#[test]
fn a_shared_database_never_expands_the_addressed_workspace() {
    let fixture = Fixture::new();
    let session = fixture.session(1, None, None);
    fixture.evidence(&session, 2, 1, "Workspace-owned source.", None);
    let other = fixture._root.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let other_id = crate::core::workspace::stable_workspace_id(&other);
    fixture
        .writer
        .insert_workspace(
            &other_id,
            &CreateWorkspaceInput {
                path: other.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
    let reader = DbConnection::open_file_read_only(&fixture.database).unwrap();
    let state = load(
        &reader,
        &ResumeOptions {
            workspace_path: &other,
            database_path: &fixture.database,
            sessions: 3,
        },
        &other,
        reference_time(),
    )
    .unwrap();
    assert_eq!(state.workspace_id, other_id);
    assert_eq!(state.transcript_history.evidence_total, 0);
    assert!(state.transcript_history.sessions.is_empty());
}

#[test]
fn unicode_prefixes_have_exact_offsets_and_do_not_rewrite_the_source_revision() {
    let fixture = Fixture::new();
    let session = fixture.session(1, None, None);
    let body = format!("{}🦀 remaining source", "a".repeat(4095));
    let row = fixture.evidence(&session, 2, 1, &body, None);
    let history = fixture.state(3).transcript_history;
    let item = &history.sessions[0].items[0];
    assert_eq!(item.content_byte_end, 4095);
    assert!(item.content_truncated);
    assert_eq!(item.excerpt_bytes, body.len());
    assert_eq!(item.content, body[..4095]);
    assert_eq!(item.entity_revision, row.pack_entity_revision());
    assert_eq!(
        fixture
            .writer
            .get_evidence_span(&row.id)
            .unwrap()
            .unwrap()
            .excerpt,
        body
    );
}

#[test]
fn long_excerpts_are_screened_beyond_the_published_prefix() {
    let fixture = Fixture::new();
    let session = fixture.session(1, None, None);
    let private = " /home/private/notes.txt ";
    assert!(crate::policy::redact_public_replay_text(private).redacted);
    // Across the first window's end, and far past the published prefix.
    for (number, offset) in [(2, 4090), (3, 20_000)] {
        let body = format!("{}{private}{}", "a".repeat(offset), "b".repeat(64));
        let row = fixture.evidence(&session, number, number as u64, &body, None);
        assert_eq!(row.search_eligibility, "admitted", "withheld by resume");
    }
    let history = fixture.state(3).transcript_history;
    assert_eq!(history.evidence_total, 0);
    assert!(history.sessions.is_empty());
}

#[test]
fn corrupt_chronology_fails_closed_without_leaking_storage_values_or_leaving_a_snapshot() {
    let fixture = Fixture::new();
    let session = fixture.session(1, None, None);
    fixture.evidence(&session, 2, 1, "Public source.", None);
    fixture
        .writer
        .execute_raw(&format!(
            "UPDATE sessions SET ended_at = 'PRIVATE-INVALID-TIME' WHERE id = '{session}'"
        ))
        .unwrap();
    let reader = DbConnection::open_file_read_only(&fixture.database).unwrap();
    let error = load(
        &reader,
        &fixture.options(3),
        &fixture.workspace,
        reference_time(),
    )
    .err()
    .unwrap();
    let message = error.to_string();
    assert!(!message.contains("PRIVATE-INVALID-TIME"));
    assert!(!message.contains(&session));
    assert!(matches!(error, DomainError::Storage { .. }));
    reader
        .begin_read_snapshot()
        .expect("failed resume released its snapshot");
    reader.rollback_read_snapshot().unwrap();
}

#[test]
fn inverted_chronology_cannot_claim_a_recent_completed_session() {
    let fixture = Fixture::new();
    let session = fixture.session(1, Some("2020-01-02T00:00:00Z"), None);
    fixture
        .writer
        .execute_raw(&format!(
            "UPDATE sessions SET ended_at = '2020-01-01T00:00:00Z' WHERE id = '{session}'"
        ))
        .unwrap();
    fixture.evidence(&session, 2, 1, "Public source.", None);
    assert!(build_resume_report(&fixture.options(3)).is_err());
}

#[test]
fn header_pagination_and_equal_time_ties_do_not_drop_later_sessions() {
    let fixture = Fixture::new();
    for number in 1..=RESUME_STORAGE_PAGE_SIZE + 1 {
        fixture.session(number as u128, None, Some("2020-01-01T00:00:00Z"));
    }
    let later = SessionId::from_uuid(uuid::Uuid::from_u128(
        (RESUME_STORAGE_PAGE_SIZE + 1) as u128,
    ))
    .to_string();
    let first = SessionId::from_uuid(uuid::Uuid::from_u128(1)).to_string();
    fixture.evidence(&later, 1001, 1, "Evidence in the second header page.", None);
    fixture.evidence(&first, 1002, 1, "Evidence in the first header page.", None);
    let history = fixture.state(1).transcript_history;
    assert_eq!(
        history.session_total, 2,
        "unadmitted headers are not resumable sessions"
    );
    assert_eq!(history.evidence_total, 2);
    assert!(history.sessions_truncated);
    assert_eq!(history.sessions[0].session_id, first);
    let history = fixture.state(2).transcript_history;
    assert_eq!(history.sessions[1].session_id, later);
}
