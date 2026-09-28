//! Native pack diffs must compare verified historical identities and revisions
//! without fabricating memory aliases or exposing private source material.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::db::{
    CreateEvidenceSpanInput, CreateMemoryInput, CreatePackEvidenceItemInput, CreatePackItemInput,
    CreatePackRecordInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind, StoredPackRecord,
};
use serde_json::{Value, json};

const WORKSPACE_ID: &str = "wsp_00000000000000000000000301";
const SESSION_ID: &str = "sess_00000000000000000000000301";
const MEMORY_A: &str = "mem_00000000000000000000000001";
const MEMORY_B: &str = "mem_00000000000000000000000002";
const EVIDENCE_A: &str = "ev_00000000000000000000000001";
const EVIDENCE_B: &str = "ev_00000000000000000000000002";
const EVIDENCE_C: &str = "ev_00000000000000000000000003";
const PACK_A: &str = "pack_00000000000000000000000301";
const PACK_B: &str = "pack_00000000000000000000000302";
const PACK_C: &str = "pack_00000000000000000000000303";

fn hash(text: &str) -> String {
    format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex())
}

fn fixture() -> DbConnection {
    let connection = DbConnection::open_memory().expect("open native diff database");
    connection.migrate().expect("migrate native diff database");
    connection
        .insert_workspace(
            WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: "/tmp/native-pack-diff".to_owned(),
                name: Some("native pack diff".to_owned()),
            },
        )
        .expect("insert workspace");
    for memory_id in [MEMORY_A, MEMORY_B] {
        connection
            .insert_memory(
                memory_id,
                &CreateMemoryInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("Release diagnosis retains durable context {memory_id}."),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("file://AGENTS.md".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert durable memory");
    }
    connection
        .insert_session(
            SESSION_ID,
            &CreateSessionInput {
                workspace_id: WORKSPACE_ID.to_owned(),
                cass_session_id: "private-upstream-diff-session".to_owned(),
                source_path: Some("/Users/private-user/transcripts/session.jsonl".to_owned()),
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: Some("2026-09-27T12:00:00Z".to_owned()),
                ended_at: Some("2026-09-27T12:05:00Z".to_owned()),
                message_count: 3,
                token_count: None,
                content_hash: hash("native diff session"),
                metadata_json: None,
            },
        )
        .expect("insert CASS session");
    for (evidence_id, line) in [(EVIDENCE_A, 1), (EVIDENCE_B, 2), (EVIDENCE_C, 3)] {
        let excerpt = format!("The release diagnosis records observed result {line}.");
        connection
            .insert_evidence_span(
                evidence_id,
                &CreateEvidenceSpanInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    session_id: SESSION_ID.to_owned(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("private-upstream-span-{line}"),
                    span_kind: "message".to_owned(),
                    start_line: line,
                    end_line: line,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    content_hash: hash(&excerpt),
                    excerpt,
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .expect("insert admitted native evidence");
    }
    connection
}

fn memory_item(pack_id: &str, memory_id: &str, rank: u32) -> CreatePackItemInput {
    CreatePackItemInput {
        pack_id: pack_id.to_owned(),
        memory_id: memory_id.to_owned(),
        rank,
        section: "procedural_rules".to_owned(),
        estimated_tokens: 10,
        relevance: 0.8,
        utility: 0.7,
        combined_score: None,
        attempt_family_multiplicity: None,
        why: "Selected memory keeps durable release context.".to_owned(),
        diversity_key: None,
        provenance_json: r#"{"schema":"ee.pack_item.provenance.v1","entries":[]}"#.to_owned(),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: None,
    }
}

fn evidence_item(
    connection: &DbConnection,
    pack_id: &str,
    evidence_id: &str,
    rank: u32,
) -> CreatePackEvidenceItemInput {
    let span = connection
        .get_evidence_span(evidence_id)
        .expect("read native evidence")
        .expect("native evidence exists");
    CreatePackEvidenceItemInput {
        pack_id: pack_id.to_owned(),
        evidence_id: evidence_id.to_owned(),
        entity_revision: span.pack_entity_revision(),
        rank,
        section: "evidence".to_owned(),
        estimated_tokens: 10,
        relevance: 0.8,
        utility: 0.5,
        why: "Selected transcript supports deterministic release diagnosis.".to_owned(),
        provenance_json: json!({
            "schema": "ee.pack_item.provenance.v1",
            "entries": [{ "uri": span.canonical_provenance_uri() }],
        })
        .to_string(),
        trust_class: "cass_evidence".to_owned(),
        trust_subclass: Some("imported_transcript_excerpt".to_owned()),
    }
}

fn persist_pack(
    connection: &DbConnection,
    pack_id: &str,
    memories: &[CreatePackItemInput],
    evidence: &[CreatePackEvidenceItemInput],
) -> StoredPackRecord {
    let item_count = u32::try_from(memories.len() + evidence.len()).expect("small fixture count");
    connection
        .insert_pack_record_with_timings_task_lens_and_evidence(
            pack_id,
            &CreatePackRecordInput {
                task_paths: Vec::new(),
                workspace_id: WORKSPACE_ID.to_owned(),
                query: "release diagnosis".to_owned(),
                profile: "balanced".to_owned(),
                max_tokens: 1000,
                used_tokens: item_count * 10,
                item_count,
                omitted_count: 0,
                pack_hash: hash(pack_id),
                degraded_json: None,
                created_by: Some("ee pack".to_owned()),
            },
            memories,
            evidence,
            &[],
            None,
        )
        .expect("persist native pack through the verified writer");
    let record = connection
        .get_pack_record(pack_id)
        .expect("read persisted pack")
        .expect("pack exists");
    assert!(
        super::parse_pack_ledger(&record)
            .available_ledger()
            .is_some(),
        "test fixtures must pass the real ledger integrity and record-binding gate"
    );
    record
}

fn diff(record_a: &StoredPackRecord, record_b: &StoredPackRecord) -> Value {
    super::collect_pack_diff(
        record_a,
        &super::parse_pack_ledger(record_a),
        record_b,
        &super::parse_pack_ledger(record_b),
    )
}

fn assert_entity(item: &Value, kind: &str, id: &str) {
    assert_eq!(item["entity"], json!({"kind": kind, "id": id}));
}

#[test]
fn native_pack_diff_reports_mixed_add_remove_revision_and_redaction_changes() {
    let connection = fixture();
    let old_evidence = evidence_item(&connection, PACK_A, EVIDENCE_A, 2);
    let old_revision = old_evidence.entity_revision.clone();
    let record_a = persist_pack(
        &connection,
        PACK_A,
        &[memory_item(PACK_A, MEMORY_A, 1)],
        &[
            old_evidence,
            evidence_item(&connection, PACK_A, EVIDENCE_B, 3),
        ],
    );

    // Keep the source security commitment internally consistent. The normal
    // pack writer rechecks admission and the new revision before committing.
    let revised_excerpt = "The release diagnosis now records a confirmed successful result.";
    let revised_hash = hash(revised_excerpt);
    connection
        .execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = '{revised_excerpt}', content_hash = '{revised_hash}', canonical_excerpt_hash = '{revised_hash}', metadata_json = json_set(metadata_json, '$.canonicalExcerptHash', '{revised_hash}') WHERE id = '{EVIDENCE_A}'"
        ))
        .expect("revise the source fixture with a matching canonical commitment");
    let revision_only = persist_pack(
        &connection,
        PACK_C,
        &[memory_item(PACK_C, MEMORY_A, 1)],
        &[
            evidence_item(&connection, PACK_C, EVIDENCE_A, 2),
            evidence_item(&connection, PACK_C, EVIDENCE_B, 3),
        ],
    );
    let revision_diff = diff(&record_a, &revision_only);
    assert_eq!(revision_diff["summary"]["changedCount"], json!(1));
    assert_entity(&revision_diff["changed"][0], "evidence_span", EVIDENCE_A);
    assert_eq!(revision_diff["changed"][0]["revisionChanged"], json!(true));
    assert_eq!(revision_diff["changed"][0]["whyChanged"], json!(false));
    assert_eq!(revision_diff["changed"][0]["rankDelta"], json!(0));
    let secret_probe = format!("{}{}", "AKIA", "Q".repeat(16));
    let mut new_evidence = evidence_item(&connection, PACK_B, EVIDENCE_A, 1);
    let new_revision = new_evidence.entity_revision.clone();
    assert_ne!(old_revision, new_revision);
    new_evidence.relevance = 0.9;
    new_evidence.why = format!("Observed credential {secret_probe} in failed tool output.");
    let record_b = persist_pack(
        &connection,
        PACK_B,
        &[
            memory_item(PACK_B, MEMORY_A, 2),
            memory_item(PACK_B, MEMORY_B, 4),
        ],
        &[
            new_evidence,
            evidence_item(&connection, PACK_B, EVIDENCE_C, 3),
        ],
    );
    let result = diff(&record_a, &record_b);
    assert_eq!(result["summary"]["replayable"], json!(true));
    assert_eq!(result["summary"]["addedCount"], json!(2));
    assert_eq!(result["summary"]["removedCount"], json!(1));
    assert_eq!(result["summary"]["changedCount"], json!(2));
    assert_eq!(result["summary"]["redactionChangeCount"], json!(1));
    assert_entity(&result["added"][0], "memory", MEMORY_B);
    assert_eq!(result["added"][0]["entityRevision"], Value::Null);
    assert_entity(&result["added"][1], "evidence_span", EVIDENCE_C);
    assert_entity(&result["removed"][0], "evidence_span", EVIDENCE_B);
    assert_entity(&result["changed"][0], "memory", MEMORY_A);
    assert_eq!(result["changed"][0]["rankDelta"], json!(1));
    assert_eq!(result["changed"][0]["revisionChanged"], json!(false));
    let evidence_change = &result["changed"][1];
    assert_entity(evidence_change, "evidence_span", EVIDENCE_A);
    assert_eq!(
        evidence_change["old"]["entityRevision"],
        json!(old_revision)
    );
    assert_eq!(
        evidence_change["new"]["entityRevision"],
        json!(new_revision)
    );
    assert_eq!(evidence_change["rankDelta"], json!(-1));
    assert_eq!(evidence_change["revisionChanged"], json!(true));
    assert_eq!(evidence_change["whyChanged"], json!(true));
    assert_eq!(evidence_change["redactionChanged"], json!(true));
    assert!(
        evidence_change["scoreDelta"]["relevance"]
            .as_f64()
            .is_some_and(|delta| delta > 0.09)
    );
    assert_eq!(
        result["redactionChanges"][0]["entity"],
        evidence_change["entity"]
    );
    assert_eq!(
        result,
        diff(&record_a, &record_b),
        "typed ordering is stable"
    );
    let rendered = result.to_string();
    for private_value in [
        secret_probe.as_str(),
        "private-upstream",
        "/Users/private-user/",
    ] {
        assert!(
            !rendered.contains(private_value),
            "native diff must retain public projection privacy"
        );
    }
    assert!(
        !rendered.contains("memoryId"),
        "v3 identities must not fabricate memory aliases"
    );
    assert_eq!(
        connection.get_pack_record(PACK_A).expect("reread old pack"),
        Some(record_a),
        "diff does not rewrite the historical source revision"
    );
    assert_eq!(
        connection.get_pack_record(PACK_B).expect("reread new pack"),
        Some(record_b),
        "diff leaves current persisted bytes unchanged"
    );
}

#[test]
fn unchanged_mixed_native_pack_has_no_added_removed_or_changed_entities() {
    let connection = fixture();
    let record = persist_pack(
        &connection,
        PACK_A,
        &[memory_item(PACK_A, MEMORY_A, 2)],
        &[evidence_item(&connection, PACK_A, EVIDENCE_A, 1)],
    );
    let result = diff(&record, &record);
    assert_eq!(result["summary"]["replayable"], json!(true));
    assert_eq!(result["summary"]["hashMatch"], json!(true));
    for field in ["added", "removed", "changed", "redactionChanges"] {
        assert_eq!(result[field], json!([]));
    }
    assert_eq!(result["likelyCauses"], json!(["no_change"]));
    let parsed = super::parse_pack_ledger(&record);
    assert_eq!(
        super::diff_items_for_pack(&parsed).len(),
        2,
        "unchanged evidence remains in the comparison set"
    );
}

#[test]
fn corrupted_native_pack_ledger_exposes_no_diff_or_private_record_text() {
    let connection = fixture();
    let trusted = persist_pack(
        &connection,
        PACK_A,
        &[memory_item(PACK_A, MEMORY_A, 1)],
        &[evidence_item(&connection, PACK_A, EVIDENCE_A, 2)],
    );
    let wrong_hash = hash("a different ledger commitment");
    connection
        .execute_raw(&format!(
            "UPDATE pack_records SET ledger_hash = '{wrong_hash}', query = '/Users/private-user/untrusted-query.txt' WHERE id = '{PACK_A}'"
        ))
        .expect("corrupt the persisted fixture commitment");
    let corrupt = connection
        .get_pack_record(PACK_A)
        .expect("read corrupt record")
        .expect("corrupt record exists");
    let parsed = super::parse_pack_ledger(&corrupt);
    assert!(parsed.available_ledger().is_none());
    let result = diff(&trusted, &corrupt);
    assert_eq!(result["summary"]["replayable"], json!(false));
    for field in [
        "added",
        "removed",
        "changed",
        "redactionChanges",
        "derivedAssetChanges",
    ] {
        assert_eq!(
            result[field],
            json!([]),
            "untrusted native comparison must fail closed"
        );
    }
    assert_eq!(
        result["likelyCauses"],
        json!(["ledger_unavailable_or_untrusted"])
    );
    let projected = super::pack_record_json(&corrupt, &parsed);
    assert_eq!(projected["query"], Value::Null);
    assert_eq!(projected["recordMetadataVerified"], json!(false));
    assert!(!projected.to_string().contains("private-user"));
    assert!(!result.to_string().contains("private-user"));
    assert_eq!(
        connection
            .get_pack_record(PACK_A)
            .expect("reread corrupt record"),
        Some(corrupt)
    );
}

#[test]
fn native_diff_parser_preserves_legacy_memories_and_rejects_inconsistent_evidence() {
    let legacy = json!({"memoryId": MEMORY_A, "rank": 1});
    let item = super::diff_item_from_ledger(&legacy).expect("historical memory identity");
    let projected = super::diff_item_json(&item);
    assert_entity(&projected, "memory", MEMORY_A);
    assert_eq!(projected["entityRevision"], Value::Null);

    let valid = json!({
        "entityKind": "evidence_span",
        "entityId": EVIDENCE_A,
        "evidenceSpanId": EVIDENCE_A,
        "entityRevision": hash("native evidence revision"),
        "rank": 2,
    });
    assert!(super::diff_item_from_ledger(&valid).is_some());
    for (field, replacement) in [
        ("entityKind", json!("unknown_source")),
        ("entityId", json!(EVIDENCE_B)),
        ("evidenceSpanId", json!(MEMORY_A)),
        ("entityRevision", json!("unverified revision")),
        ("entityRevision", Value::Null),
        ("memoryId", json!(MEMORY_A)),
    ] {
        let mut malformed = valid.clone();
        malformed[field] = replacement;
        assert!(
            super::diff_item_from_ledger(&malformed).is_none(),
            "inconsistent {field} must not become a comparable native identity"
        );
    }
}
