//! Real migrated-store failure -> repair -> incident-card -> recall regressions.

use super::*;
use crate::core::cass_error_recall::record_session_error_recall;
use crate::core::error_diagnosis::{
    error_recall_report, pack_error_recall_query_seed, recalled_repair_evidence,
};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const COMMAND: &str = "pytest -v --color=no tests/test_queue.py";
const REPAIR: &str = "The test_drain test failed because the queue fixture was shared between tests. I fixed test_drain by constructing a fresh queue for each test in tests/conftest.py.";

struct Episode {
    db: DbConnection,
    session: String,
    failure: String,
    repair: String,
    proof: String,
}

fn report(status: &str, counts: &str) -> String {
    format!(
        "================ test session starts ================\ncollected 1 item\ntests/test_queue.py::test_drain {status} [100%]\n================ {counts} in 0.01s ================\n"
    )
}

fn episode(proof: &str, proof_command: &str) -> TestResult<Episode> {
    let db = DbConnection::open_memory()?;
    db.migrate()?;
    db.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/pytest-terminal-recall".to_owned(),
            name: None,
        },
    )?;
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(9400)).to_string();
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: "/sessions/pytest-terminal.jsonl".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 5,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(b"pytest-terminal-recall").to_hex()),
            metadata_json: None,
        },
    )?;
    let rows = [
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"failed","name":"Bash",
                "input":{"command":COMMAND}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"failed",
                "content":report("FAILED", "1 failed"),"is_error":true}),
        ),
        ("message", "assistant", json!({"type":"text","text":REPAIR})),
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"retry","name":"Bash",
                "input":{"command":proof_command}}),
        ),
        (
            "tool_result",
            "user",
            // An optimistic tool wrapper cannot erase a native refusal.
            json!({"type":"tool_result","tool_use_id":"retry",
                "content":proof,"is_error":false}),
        ),
    ];
    let id = |number: u32| {
        crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(9500 + u128::from(number)))
            .to_string()
    };
    for (offset, (kind, role, block)) in rows.iter().enumerate() {
        let number = u32::try_from(offset + 1)?;
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        db.insert_evidence_span(
            &id(number),
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{session}:{number}"),
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
    Ok(Episode {
        db,
        session,
        failure: id(2),
        repair: id(3),
        proof: id(5),
    })
}

#[test]
fn native_pytest_repair_is_cited_packable_and_idempotent_without_source_mutation() -> TestResult {
    let episode = episode(&report("PASSED", "1 passed"), COMMAND)?;
    let before = episode.db.list_evidence_spans_for_session(&episode.session)?;
    let result = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(result.failures_seen, 1);
    assert_eq!(result.resolved_failures, 1);
    assert_eq!(result.repair_links_recorded, 2);
    assert_eq!(result.incident_cards_recorded, 1);
    let diagnostic = diagnostic("tests/test_queue.py::test_drain");
    let recall = error_recall_report(&episode.db, WS, &diagnostic)?;
    let card = crate::core::incident_card::incident_card_id(WS, &episode.failure);
    assert!(recall.exact);
    assert!(recall.helpful_repairs.contains(&episode.repair));
    assert!(recall.helpful_repairs.contains(&card));
    assert_eq!(recall.proof_links, [episode.proof.clone()]);
    let links = episode
        .db
        .list_error_repair_links(WS, &diagnostic.layered_key().key)?;
    assert_eq!(links.len(), 3);
    assert!(links.iter().all(|link| {
        link.evidence_ref.as_deref() == Some(episode.failure.as_str())
    }));
    let evidence = recalled_repair_evidence(&episode.db, WS, &recall)?;
    assert!(evidence.iter().any(|item| {
        item.evidence_id == card && item.role == "incident_card"
            && item.text.as_deref().is_some_and(|text| text.contains("Fix:"))
    }));
    assert!(evidence.iter().any(|item| {
        item.evidence_id == episode.proof && item.text.is_none()
    }));
    let query = pack_error_recall_query_seed(&episode.db, WS, &diagnostic)?;
    assert!(query.contains("prior fix:"));
    assert!(query.contains("fresh queue"));
    for source in before {
        let after = episode
            .db
            .get_evidence_span(&source.id)?
            .ok_or("missing source")?;
        assert_eq!(after.excerpt, source.excerpt);
        assert_eq!(after.content_hash, source.content_hash);
        assert_eq!(after.canonical_provenance_uri(), source.canonical_provenance_uri());
    }
    let rerun = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(rerun.incident_cards_recorded, 0);
    assert_eq!(
        episode.db.list_error_repair_links(WS, &diagnostic.layered_key().key)?.len(),
        3
    );
    Ok(())
}

#[test]
fn skipped_filtered_and_incomplete_green_wrappers_never_create_helpful_repairs() -> TestResult {
    let green = report("PASSED", "1 passed");
    let partial = green.split("================ 1 passed").next().unwrap();
    for proof in [
        report("SKIPPED", "1 skipped"),
        green.replace("test_drain", "test_other"),
        partial.to_owned(),
        ". [100%]\n1 passed in 0.01s\n".to_owned(),
    ] {
        let episode = episode(&proof, COMMAND)?;
        let result = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(result.failures_seen, 1);
        assert_eq!(result.resolved_failures, 0);
        assert_eq!(result.repair_links_recorded, 0);
        assert_eq!(result.incident_cards_recorded, 0);
        let recall = error_recall_report(
            &episode.db,
            WS,
            &diagnostic("tests/test_queue.py::test_drain"),
        )?;
        assert!(recall.exact);
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
    }
    Ok(())
}

#[test]
fn named_test_pass_does_not_replace_exact_invocation_coverage() -> TestResult {
    let episode = episode(
        &report("PASSED", "1 passed"),
        "pytest -v --color=no other/test_queue.py",
    )?;
    let result = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(result.failures_seen, 1);
    assert_eq!(result.resolved_failures, 0);
    assert_eq!(result.repair_links_recorded, 0);
    assert_eq!(result.incident_cards_recorded, 0);
    Ok(())
}
