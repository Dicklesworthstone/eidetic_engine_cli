//! Pack workspace guards and native rule section migrations retain provenance.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{
    CreateEvidenceSpanInput, CreatePackEvidenceItemInput, CreatePackRecordInput,
    CreatePackRuleItemInput, CreateProceduralRuleInput, CreateSessionInput, CreateWorkspaceInput,
    DbConnection, EvidenceProducerKind,
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

fn seed_rule_sources(connection: &DbConnection) {
    for (workspace, path) in [(WS_A, "/tmp/guard-a"), (WS_B, "/tmp/guard-b")] {
        connection
            .insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: path.to_owned(),
                    name: None,
                },
            )
            .expect("insert rule workspace");
    }
    for (rule_id, workspace) in [
        ("rule_00000000000000000000001932", WS_A),
        ("rule_00000000000000000000002932", WS_A),
        ("rule_00000000000000000000003932", WS_B),
    ] {
        connection
            .insert_procedural_rule(
                rule_id,
                &CreateProceduralRuleInput {
                    workspace_id: workspace.to_owned(),
                    content: "Avoid publishing before the release checks pass.".to_owned(),
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    trust_class: "agent_assertion".to_owned(),
                    scope: "workspace".to_owned(),
                    scope_pattern: None,
                    maturity: "candidate".to_owned(),
                    protected: true,
                    source_memory_ids: Vec::new(),
                    tags: vec!["anti-pattern".to_owned()],
                },
            )
            .expect("insert native rule");
    }
}

fn persist_rule(
    connection: &DbConnection,
    pack_id: &str,
    rule_id: &str,
    section: &str,
) -> super::Result<()> {
    connection
        .insert_pack_record_with_native_items(
            pack_id,
            &CreatePackRecordInput {
                task_paths: Vec::new(),
                workspace_id: WS_A.to_owned(),
                query: "prepare release".to_owned(),
                profile: "balanced".to_owned(),
                max_tokens: 100,
                used_tokens: 10,
                item_count: 1,
                omitted_count: 0,
                pack_hash: hash(pack_id),
                degraded_json: None,
                created_by: Some("ee pack".to_owned()),
            },
            &[],
            &[CreatePackRuleItemInput {
                pack_id: pack_id.to_owned(),
                rule_id: rule_id.to_owned(),
                entity_revision: hash(rule_id),
                rank: 1,
                section: section.to_owned(),
                estimated_tokens: 10,
                relevance: 0.8,
                utility: 0.7,
                why: "What NOT to do: release warning selected before action.".to_owned(),
                provenance_json: r#"{"schema":"ee.pack_item.provenance.v1","entries":[]}"#
                    .to_owned(),
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: Some("procedural_rule".to_owned()),
            }],
            &[],
            &[],
            None,
        )
        .map(|_| ())
}

#[test]
fn v132_rule_failure_sections_preserve_history_guards_and_recovery() {
    let directory = tempfile::tempdir().expect("migration and recovery directory");
    let connection =
        DbConnection::open_file(directory.path().join("source.db")).expect("open source");
    connection
        .ensure_migration_table()
        .expect("migration ledger");
    for migration in super::MIGRATIONS
        .iter()
        .filter(|migration| migration.version() < 132)
    {
        connection
            .apply_file_schema_rebuild_migration(migration, "2026-10-17T00:00:00Z")
            .expect("apply historical migration");
    }
    seed_rule_sources(&connection);
    let legacy_pack = "pack_00000000000000000000001932";
    let warning_pack = "pack_00000000000000000000002932";
    let ordinary_rule = "rule_00000000000000000000001932";
    let warning_rule = "rule_00000000000000000000002932";
    persist_rule(&connection, legacy_pack, ordinary_rule, "procedural_rules")
        .expect("V131 native rule pack");
    let original = connection
        .get_pack_history_for_recovery(legacy_pack)
        .expect("historical pack and replay bytes");
    persist_rule(&connection, warning_pack, warning_rule, "failures")
        .expect_err("V131 cannot persist a native warning section");
    assert!(connection.get_pack_record(warning_pack).unwrap().is_none());

    let upgrade = connection.migrate().expect("upgrade existing source");
    assert_eq!(upgrade.applied(), &[132]);
    assert_eq!(
        connection
            .get_pack_history_for_recovery(legacy_pack)
            .unwrap(),
        original,
        "migration must preserve historical rows, ranks, hashes and replay bytes"
    );
    assert!(connection.migrate().unwrap().applied().is_empty());
    assert!(connection.check_foreign_keys().unwrap().passed);
    assert_eq!(
        connection
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_pack_rule_items_rule'",
                &[],
            )
            .unwrap()
            .len(),
        1,
        "the rule lookup index survives the rebuild"
    );
    persist_rule(&connection, warning_pack, warning_rule, "failures")
        .expect("native warning persists after migration");
    let warning = connection
        .get_pack_history_for_recovery(warning_pack)
        .expect("warning history verifies");
    assert_eq!(warning.rule_items[0].section, "failures");
    assert_eq!(warning.rule_items[0].rule_id, warning_rule);
    let parsed = super::parse_stored_pack_ledger(&warning.record);
    let selected = &parsed.available_ledger().unwrap()["selectedItems"][0];
    assert_eq!(selected["entityKind"], "rule");
    assert_eq!(selected["section"], "failures");
    assert!(selected.get("memoryId").is_none());
    persist_rule(
        &connection,
        "pack_00000000000000000000003932",
        warning_rule,
        "evidence",
    )
    .expect_err("a native rule cannot use an unrelated section");

    let raw_insert = |rule_id: &str, rank: u32, section: &str| {
        connection.execute_raw(&format!(
            "INSERT INTO pack_rule_items (pack_id, rule_id, entity_revision, rank, section, estimated_tokens, relevance, utility, why, provenance_json, trust_class, trust_subclass) VALUES ('{legacy_pack}', '{rule_id}', '{revision}', {rank}, '{section}', 10, 0.5, 0.5, 'raw rule guard probe', '{{}}', 'agent_assertion', 'procedural_rule')",
            revision = hash(rule_id),
        ))
    };
    for foreign_or_missing in [
        "rule_00000000000000000000003932",
        "rule_00000000000000000000009932",
    ] {
        raw_insert(foreign_or_missing, 2, "failures")
            .expect_err("insert workspace guard refuses foreign or missing rules");
        connection
            .execute_raw(&format!(
                "UPDATE pack_rule_items SET rule_id = '{foreign_or_missing}' WHERE pack_id = '{legacy_pack}'"
            ))
            .expect_err("update workspace guard refuses foreign or missing rules");
    }
    raw_insert(warning_rule, 2, "evidence").expect_err("SQL rejects unrelated rule sections");
    raw_insert(warning_rule, 1, "failures")
        .expect_err("native selection ranks remain unique within a pack");
    raw_insert(ordinary_rule, 2, "failures")
        .expect_err("native rule identities remain unique within a pack");
    connection
        .execute_raw(&format!(
            "DELETE FROM procedural_rules WHERE id = '{warning_rule}'"
        ))
        .expect_err("selected rule provenance retains ON DELETE RESTRICT");

    let target = DbConnection::open_file(directory.path().join("recovered.db"))
        .expect("open recovery target");
    target.migrate().expect("migrate file target");
    seed_rule_sources(&target);
    target
        .insert_pack_histories_for_recovery([&original, &warning])
        .expect("restore both historical guidance and native warning");
    assert_eq!(
        target.get_pack_history_for_recovery(legacy_pack).unwrap(),
        original
    );
    assert_eq!(
        target.get_pack_history_for_recovery(warning_pack).unwrap(),
        warning
    );
    assert!(target.check_foreign_keys().unwrap().passed);

    connection
        .execute_raw(&format!(
            "DELETE FROM pack_records WHERE id = '{warning_pack}'"
        ))
        .expect("pack deletion cascades to its native child rows");
    assert!(
        connection
            .get_pack_rule_items(warning_pack)
            .unwrap()
            .is_empty()
    );
    assert!(
        connection
            .get_procedural_rule(warning_rule)
            .unwrap()
            .is_some()
    );
    assert!(connection.check_foreign_keys().unwrap().passed);
}
