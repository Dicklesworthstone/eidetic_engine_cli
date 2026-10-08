//! The V129 pack workspace guards keep V126's semantics without joins.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{
    CreateEvidenceSpanInput, CreatePackEvidenceItemInput, CreatePackRecordInput,
    CreateSessionInput, CreateWorkspaceInput, DbConnection, EvidenceProducerKind,
};

const WS_A: &str = "wsp_00000000000000000000001929";
const WS_B: &str = "wsp_00000000000000000000002929";

fn hash(text: &str) -> String {
    format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex())
}

fn seed(connection: &DbConnection, workspace: &str, session: &str, evidence: &str) {
    connection
        .insert_session(
            session,
            &CreateSessionInput {
                workspace_id: workspace.to_owned(),
                cass_session_id: format!("upstream-{session}"),
                source_path: None,
                agent_name: Some("claude_code".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: hash(session),
                metadata_json: None,
            },
        )
        .expect("insert session");
    let excerpt = format!("Release notes for {evidence} were verified.");
    connection
        .insert_evidence_span(
            evidence,
            &CreateEvidenceSpanInput {
                workspace_id: workspace.to_owned(),
                session_id: session.to_owned(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("span-{evidence}"),
                span_kind: "message".to_owned(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                content_hash: hash(&excerpt),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )
        .expect("insert evidence");
}

fn fixture() -> DbConnection {
    let connection = DbConnection::open_memory().expect("open");
    connection.migrate().expect("migrate");
    for (workspace, path) in [(WS_A, "/tmp/guard-a"), (WS_B, "/tmp/guard-b")] {
        connection
            .insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: path.to_owned(),
                    name: None,
                },
            )
            .expect("insert workspace");
    }
    seed(
        &connection,
        WS_A,
        "sess_00000000000000000000001929",
        "ev_00000000000000000000001929",
    );
    seed(
        &connection,
        WS_B,
        "sess_00000000000000000000002929",
        "ev_00000000000000000000002929",
    );
    connection
}

fn persist(connection: &DbConnection, pack_id: &str, evidence_id: &str) -> super::Result<()> {
    let revision = connection
        .get_evidence_span(evidence_id)
        .expect("read evidence")
        .map_or_else(|| hash("missing"), |span| span.pack_entity_revision());
    let item = CreatePackEvidenceItemInput {
        pack_id: pack_id.to_owned(),
        evidence_id: evidence_id.to_owned(),
        entity_revision: revision,
        rank: 1,
        section: "evidence".to_owned(),
        estimated_tokens: 10,
        relevance: 0.8,
        utility: 0.5,
        why: "Selected transcript supports the release diagnosis.".to_owned(),
        provenance_json: r#"{"schema":"ee.pack_item.provenance.v1","entries":[]}"#.to_owned(),
        trust_class: "cass_evidence".to_owned(),
        trust_subclass: Some("imported_transcript_excerpt".to_owned()),
    };
    connection
        .insert_pack_record_with_timings_task_lens_and_evidence(
            pack_id,
            &CreatePackRecordInput {
                task_paths: Vec::new(),
                workspace_id: WS_A.to_owned(),
                query: "release diagnosis".to_owned(),
                profile: "balanced".to_owned(),
                max_tokens: 1000,
                used_tokens: 10,
                item_count: 1,
                omitted_count: 0,
                pack_hash: hash(pack_id),
                degraded_json: None,
                created_by: Some("ee pack".to_owned()),
            },
            &[],
            &[item],
            &[],
            None,
        )
        .map(|_| ())
}

#[test]
fn pack_evidence_guard_accepts_same_workspace_and_refuses_foreign_or_missing_sources() {
    let connection = fixture();
    persist(
        &connection,
        "pack_00000000000000000000001929",
        "ev_00000000000000000000001929",
    )
    .expect("evidence from the pack's own workspace is accepted");

    // The writer refuses foreign evidence before SQL; the trigger is the
    // backstop for any other writer, so drive it with raw SQL.
    let raw_insert = |evidence_id: &str| {
        connection.execute_raw(&format!(
            "INSERT INTO pack_evidence_items (pack_id, evidence_id, entity_revision, rank, section, estimated_tokens, relevance, utility, why, provenance_json, trust_class, trust_subclass) VALUES ('pack_00000000000000000000001929', '{evidence_id}', '{revision}', 2, 'evidence', 10, 0.5, 0.5, 'raw guard probe', '{{}}', 'cass_evidence', NULL)",
            revision = hash(evidence_id),
        ))
    };
    let foreign = raw_insert("ev_00000000000000000000002929")
        .expect_err("evidence from another workspace is refused by the trigger");
    assert!(
        foreign
            .to_string()
            .contains("pack evidence and session must belong to the pack workspace"),
        "{foreign}"
    );
    let missing = raw_insert("ev_00000000000000000000009929")
        .expect_err("evidence that does not exist is refused");
    assert!(!missing.to_string().is_empty());
    let foreign = persist(
        &connection,
        "pack_00000000000000000000003929",
        "ev_00000000000000000000002929",
    )
    .expect_err("the writer refuses foreign evidence too");
    assert!(foreign.to_string().contains("workspace"), "{foreign}");
    assert!(
        connection
            .get_pack_record("pack_00000000000000000000003929")
            .expect("read")
            .is_none(),
        "a refused item rolls the whole pack back"
    );

    persist(
        &connection,
        "pack_00000000000000000000004929",
        "ev_00000000000000000000004929",
    )
    .expect_err("evidence that does not exist is refused");
}

#[test]
fn v129_guards_contain_no_joins() {
    let sql = super::V129_PACK_GUARD_TRIGGERS_WITHOUT_JOINS.sql();
    assert!(!sql.to_ascii_uppercase().contains(" JOIN "), "{sql}");
    assert_eq!(sql.matches("CREATE TRIGGER").count(), 10);
    assert_eq!(sql.matches("DROP TRIGGER IF EXISTS").count(), 10);
}
