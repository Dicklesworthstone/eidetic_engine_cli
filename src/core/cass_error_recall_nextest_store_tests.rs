//! Real migrated-store derivation and public recall, not a mock index.

use super::*;
use crate::core::error_diagnosis::{error_recall_report, pack_error_recall_query_seed};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const COMMAND: &str = "cargo nextest run";
const REPAIR: &str = "The cache round_trip test failed because writes were not flushed. I fixed round_trip by flushing pending cache writes before reads in src/cache.rs.";

fn episode(
    proof: &str,
    proof_command: &str,
) -> Result<(DbConnection, String, Vec<String>), Box<dyn std::error::Error>> {
    episode_with_failure(&failed().text, proof, proof_command)
}

fn episode_with_failure(
    failure: &str,
    proof: &str,
    proof_command: &str,
) -> Result<(DbConnection, String, Vec<String>), Box<dyn std::error::Error>> {
    let connection = DbConnection::open_memory()?;
    connection.migrate()?;
    connection.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/nextest-recall-store".to_owned(),
            name: None,
        },
    )?;
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(5100)).to_string();
    connection.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: "/sessions/nextest-recall.jsonl".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 5,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(b"nextest-recall-store").to_hex()),
            metadata_json: None,
        },
    )?;
    let rows = [
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"fail","name":"Bash","input":{"command":COMMAND}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"fail","content":failure,"is_error":true}),
        ),
        ("message", "assistant", json!({"type":"text","text":REPAIR})),
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"verify","name":"Bash","input":{"command":proof_command}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"verify","content":proof,"is_error":false}),
        ),
    ];
    let mut ids = Vec::new();
    for (offset, (kind, role, block)) in rows.iter().enumerate() {
        let line = u32::try_from(offset + 1)?;
        let id =
            crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(5200 + u128::from(line)))
                .to_string();
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        connection.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{session}:{line}"),
                span_kind: (*kind).to_owned(),
                start_line: line,
                end_line: line,
                start_byte: None,
                end_byte: None,
                role: Some((*role).to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        ids.push(id);
    }
    Ok((connection, session, ids))
}

#[test]
fn nextest_import_recall_card_and_pack_context_share_provenance_and_are_idempotent() -> TestResult {
    let (db, session, ids) = episode(&passed().text, COMMAND)?;
    let before = db.list_evidence_spans_for_session(&session)?;
    let report = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.fingerprints_recorded, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.repair_links_recorded, 2);
    assert_eq!(report.incident_cards_recorded, 1);
    let diagnostic = structured_error_diagnostics(&failed().text).remove(0);
    let recall = error_recall_report(&db, WS, &diagnostic)?;
    assert!(recall.exact);
    assert_eq!(recall.proof_links, vec![ids[4].clone()]);
    assert!(recall.helpful_repairs.contains(&ids[2]));
    let card = crate::core::incident_card::incident_card_id(WS, &ids[1]);
    assert!(recall.helpful_repairs.contains(&card));
    let seed = pack_error_recall_query_seed(&db, WS, &diagnostic)?;
    assert!(seed.contains("prior fix:"));
    assert!(seed.contains("flushing"));
    let links = db.list_error_repair_links(WS, &diagnostic.layered_key().key)?;
    assert_eq!(links.len(), 3);
    assert!(
        links
            .iter()
            .all(|link| link.evidence_ref.as_deref() == Some(ids[1].as_str()))
    );
    for source in before {
        let after = db
            .get_evidence_span(&source.id)?
            .ok_or("source disappeared")?;
        assert_eq!(source.excerpt, after.excerpt);
        assert_eq!(source.content_hash, after.content_hash);
        assert_eq!(
            source.canonical_provenance_uri(),
            after.canonical_provenance_uri()
        );
    }
    let rerun = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(rerun.incident_cards_recorded, 0);
    assert_eq!(
        db.list_error_repair_links(WS, &diagnostic.layered_key().key)?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn changed_commands_or_missing_subjects_keep_real_store_failures_unresolved() -> TestResult {
    for (proof, command) in [
        (passed().text, "cargo nextest run --no-fail-fast --lib"),
        (
            passed()
                .text
                .replace("cache cache::round_trip", "other cache::round_trip"),
            COMMAND,
        ),
        (report(&[], 5), COMMAND),
        ("Process exited with code 0".to_owned(), COMMAND),
    ] {
        let (db, session, _) = episode(&proof, command)?;
        let report = record_session_error_recall(&db, WS, &session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        let diagnostic = structured_error_diagnostics(&failed().text).remove(0);
        let recall = error_recall_report(&db, WS, &diagnostic)?;
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
    }
    Ok(())
}

#[test]
fn default_fail_fast_run_records_the_repair_only_after_a_complete_native_retry() -> TestResult {
    let failure = super::fail_fast::failure_report();
    let retry = super::fail_fast::retry_report();
    let (db, session, ids) = episode_with_failure(&failure, &retry, COMMAND)?;
    let report = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.repair_links_recorded, 2);
    assert_eq!(report.incident_cards_recorded, 1);
    let diagnostic = structured_error_diagnostics(&failure).remove(0);
    let recall = error_recall_report(&db, WS, &diagnostic)?;
    assert!(recall.helpful_repairs.contains(&ids[2]));
    assert_eq!(recall.proof_links, vec![ids[4].clone()]);
    assert!(pack_error_recall_query_seed(&db, WS, &diagnostic)?.contains("flushing"));
    Ok(())
}

#[path = "cass_error_recall_nextest_growth_tests.rs"]
mod growth;
