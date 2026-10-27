//! Standalone parser failures through migrated storage and cited recall.

use crate::core::cass_error_recall::{record_session_error_recall, structured_error_diagnostics};
use crate::core::error_diagnosis::{error_recall_report, pack_error_recall_query_seed};
use crate::core::incident_card::{incident_card_id, proof::CardChecks};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const COMMAND: &str = "python task_queue.py";
const REPAIR: &str = "The SyntaxError invalid syntax failure happened because the test_drain function declaration had a missing closing parenthesis. I fixed the syntax error by closing the parameter list in task_queue.py.";

fn failure() -> String {
    concat!(
        "  File \"/private/task_queue.py\", line 9\n",
        "    def test_drain(: # private_source_sentinel\n",
        "                   ^\n",
        "SyntaxError: invalid syntax\n",
    )
    .to_owned()
}

fn evidence_id(number: u32) -> String {
    crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(9900 + u128::from(number)))
        .to_string()
}

struct Episode {
    db: DbConnection,
    session: String,
    failure: String,
    repair: String,
    proof: String,
}

fn episode(failure: &str, proof: &str, retry: &str) -> TestResult<Episode> {
    let db = DbConnection::open_memory()?;
    db.migrate()?;
    db.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/python-syntax-recall".to_owned(),
            name: None,
        },
    )?;
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(9800)).to_string();
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: "/sessions/python-syntax.jsonl".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 5,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(b"python-syntax-recall").to_hex()),
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
                "content":failure,"is_error":true}),
        ),
        ("message", "assistant", json!({"type":"text","text":REPAIR})),
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"retry","name":"Bash",
                "input":{"command":retry}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"retry",
                "content":proof,"is_error":false}),
        ),
    ];
    for (offset, (kind, role, block)) in rows.iter().enumerate() {
        let number = u32::try_from(offset + 1)?;
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        db.insert_evidence_span(
            &evidence_id(number),
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
        failure: evidence_id(2),
        repair: evidence_id(3),
        proof: evidence_id(5),
    })
}

#[test]
fn syntax_repairs_reach_cited_recall_without_mutating_source_or_repeating_links() -> TestResult {
    let failed = failure();
    let diagnostic = structured_error_diagnostics(&failed)
        .into_iter()
        .next()
        .ok_or("diagnostic")?;
    let episode = episode(&failed, "queue workers completed\n", COMMAND)?;
    let before = episode
        .db
        .list_evidence_spans_for_session(&episode.session)?;
    let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.fingerprints_recorded, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.repair_links_recorded, 2);
    assert_eq!(report.incident_cards_recorded, 1);
    let card_id = incident_card_id(WS, &episode.failure);
    let card = episode
        .db
        .get_evidence_span(&card_id)?
        .ok_or("missing card")?;
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&episode.db, WS)?.is_empty());
    let recall = error_recall_report(&episode.db, WS, &diagnostic)?;
    assert!(recall.exact);
    assert!(recall.helpful_repairs.contains(&episode.repair));
    assert!(recall.helpful_repairs.contains(&card_id));
    assert_eq!(recall.proof_links, std::slice::from_ref(&episode.proof));
    let links = episode
        .db
        .list_error_repair_links(WS, &diagnostic.layered_key().key)?;
    assert_eq!(links.len(), 3);
    assert!(
        links
            .iter()
            .all(|link| { link.evidence_ref.as_deref() == Some(episode.failure.as_str()) })
    );
    let seed = pack_error_recall_query_seed(&episode.db, WS, &diagnostic)?;
    assert!(seed.contains("prior fix:") && seed.contains("closing parenthesis"));
    assert!(!seed.contains("private_source_sentinel"));
    for source in before {
        let after = episode
            .db
            .get_evidence_span(&source.id)?
            .ok_or("missing source")?;
        assert_eq!(after.excerpt, source.excerpt);
        assert_eq!(after.content_hash, source.content_hash);
        assert_eq!(after.metadata_json, source.metadata_json);
        assert_eq!(
            after.canonical_provenance_uri(),
            source.canonical_provenance_uri()
        );
    }
    assert_eq!(
        record_session_error_recall(&episode.db, WS, &episode.session)?.incident_cards_recorded,
        0
    );
    assert_eq!(
        episode
            .db
            .list_error_repair_links(WS, &diagnostic.layered_key().key)?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn parser_failures_damaged_retries_and_changed_commands_never_earn_repair_credit() -> TestResult {
    let failed = failure();
    let diagnostic = structured_error_diagnostics(&failed)
        .into_iter()
        .next()
        .ok_or("diagnostic")?;
    for (proof, command) in [
        (failed.clone(), COMMAND),
        (
            failed.replace("SyntaxError: invalid syntax\n", "SyntaxError: invalid"),
            COMMAND,
        ),
        (format!("{failed}Process exited with code 0\n"), COMMAND),
        (
            "queue workers completed\n".to_owned(),
            "python other_queue.py",
        ),
        (
            "queue workers completed\n".to_owned(),
            "python -m py_compile task_queue.py",
        ),
    ] {
        let episode = episode(&failed, &proof, command)?;
        let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        let recall = error_recall_report(&episode.db, WS, &diagnostic)?;
        assert!(recall.exact);
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
        assert!(
            !pack_error_recall_query_seed(&episode.db, WS, &diagnostic)?.contains("prior fix:")
        );
    }
    Ok(())
}

#[test]
fn a_partial_original_parser_report_cannot_be_salvaged_by_a_later_clean_command() -> TestResult {
    let failed = failure();
    for partial in [
        failed.replace("SyntaxError: invalid syntax\n", "SyntaxError: invalid"),
        failed.replace("line 9", "line unknown"),
        format!("{failed}unsupported note\n"),
    ] {
        let episode = episode(&partial, "queue workers completed\n", COMMAND)?;
        let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(report.fingerprints_recorded, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
    }
    Ok(())
}
