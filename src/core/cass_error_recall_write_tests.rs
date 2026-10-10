//! Real-store regression coverage for history-independent CASS write receipts.

use super::*;
use crate::core::error_diagnosis::{
    error_recall_report, persist_error_repair_links, recalled_repair_evidence,
};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
};
use serde_json::json;

const WS: &str = "wsp_01234567890123456789012345";
type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn store() -> TestResult<DbConnection> {
    let connection = DbConnection::open_memory()?;
    connection.migrate()?;
    connection.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/cass-write-receipt-test".to_owned(),
            name: None,
        },
    )?;
    Ok(connection)
}

fn insert_episode(
    connection: &DbConnection,
    seed: u128,
    resolved: bool,
) -> TestResult<(String, String, String, String)> {
    let session_id = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
    connection.insert_session(
        &session_id,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: format!("/sessions/receipt-{seed}.jsonl"),
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
    )?;
    let rows = [
        ("tool_call", "assistant", json!({
            "type": "tool_use", "id": "call_failure", "name": "Bash",
            "input": {"command": "cargo test --lib"}
        })),
        ("tool_result", "user", json!({
            "type": "tool_result", "tool_use_id": "call_failure", "is_error": true,
            "content": "error[E0277]: the trait bound Widget: Serialize is not satisfied\n  --> src/widget.rs:41:9"
        })),
        ("message", "assistant", json!({
            "type": "text",
            "text": "Widget needs to derive Serialize; adding the derive in src/widget.rs fixes the bound."
        })),
        ("tool_call", "assistant", json!({
            "type": "tool_use", "id": "call_success", "name": "Bash",
            "input": {"command": "cargo test --lib"}
        })),
        ("tool_result", "user", json!({
            "type": "tool_result", "tool_use_id": "call_success", "is_error": false,
            "content": "running 3 tests\ntest result: ok. 3 passed; 0 failed"
        })),
    ];
    let id = |number: u32| {
        crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
            seed * 10 + u128::from(number),
        ))
        .to_string()
    };
    for (offset, (kind, role, block)) in rows.iter().enumerate().take(if resolved { 5 } else { 2 }) {
        let number = u32::try_from(offset + 1)?;
        let excerpt = json!({
            "type": role,
            "message": {"role": role, "content": [block]}
        })
        .to_string();
        connection.insert_evidence_span(
            &id(number),
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: session_id.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{session_id}:{number}"),
                span_kind: (*kind).to_owned(),
                start_line: number,
                end_line: number,
                start_byte: None,
                end_byte: None,
                role: Some((*role).to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
    }
    Ok((session_id, id(2), id(3), id(5)))
}

#[test]
fn repeated_error_imports_keep_complete_history_and_stable_per_run_counts() -> TestResult {
    let connection = store()?;
    let canonical = from_rustc(Some("E0277"), "missing Serialize implementation");
    let mut expected_repairs = BTreeSet::new();
    let mut expected_proofs = BTreeSet::new();
    for seed in 1..=3_u128 {
        let (session, failure, repair, proof) = insert_episode(&connection, seed, true)?;
        let report = record_session_error_recall(&connection, WS, &session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 1);
        assert_eq!(report.fingerprints_recorded, 1);
        assert_eq!(report.repair_links_recorded, 2);
        assert_eq!(report.incident_cards_recorded, 1);
        expected_repairs.insert(repair);
        expected_repairs.insert(crate::core::incident_card::incident_card_id(WS, &failure));
        expected_proofs.insert(proof);
        let before = connection.list_error_repair_links(WS, "rustc:E0277")?;
        assert_eq!(before.len(), usize::try_from(seed)? * 3);
        let rerun = record_session_error_recall(&connection, WS, &session)?;
        assert_eq!(rerun.repair_links_recorded, report.repair_links_recorded);
        assert_eq!(rerun.incident_cards_recorded, 0);
        let after = connection.list_error_repair_links(WS, "rustc:E0277")?;
        assert_eq!(after.len(), before.len());
        for (old, new) in before.iter().zip(&after) {
            assert_eq!(old.link_id, new.link_id);
            assert_eq!(old.evidence_ref, new.evidence_ref);
            assert_eq!(old.created_at, new.created_at);
        }
    }
    let report = error_recall_report(&connection, WS, &canonical)?;
    assert!(report.exact);
    assert_eq!(report.helpful_repairs, expected_repairs.into_iter().collect::<Vec<_>>());
    assert_eq!(report.proof_links, expected_proofs.into_iter().collect::<Vec<_>>());
    let evidence = recalled_repair_evidence(&connection, WS, &report)?;
    assert_eq!(evidence.len(), 9);
    assert_eq!(evidence.iter().filter(|item| item.role == "incident_card").count(), 3);
    assert_eq!(evidence.iter().filter(|item| item.role == "repair").count(), 3);
    assert_eq!(evidence.iter().filter(|item| item.role == "proof" && item.text.is_none()).count(), 3);
    Ok(())
}

#[test]
fn unresolved_observations_do_not_count_an_existing_class_history() -> TestResult {
    let connection = store()?;
    let canonical = from_rustc(Some("E0277"), "missing Serialize implementation");
    let historical = ErrorRepairLinkRecording {
        helpful_repairs: (0..256).map(|number| format!("mem_old_{number:04}")).collect(),
        evidence_ref: Some("ev_prior_failure".to_owned()),
        ..ErrorRepairLinkRecording::default()
    };
    persist_error_repair_links(&connection, WS, &canonical, &historical)?;
    let (session, _, _, _) = insert_episode(&connection, 100, false)?;
    for _ in 0..2 {
        let report = record_session_error_recall(&connection, WS, &session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.fingerprints_recorded, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
    }
    let report = error_recall_report(&connection, WS, &canonical)?;
    assert!(report.exact);
    assert_eq!(report.helpful_repairs.len(), 256);
    assert!(report.proof_links.is_empty());
    Ok(())
}
