//! Exercise grouped Python failures through real migrated storage, not mocks.

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
const REPAIR: &str = "The test_drain worker failed because the queue fixture was shared between tasks. I fixed test_drain by constructing a fresh queue for each task in task_queue.py.";

fn failure() -> String {
    concat!(
        "  + Exception Group Traceback (most recent call last):\n",
        "  |   File \"/private/task_queue.py\", line 9, in main\n",
        "  |     private_source_sentinel()\n",
        "  | ExceptionGroup: private_group_label (2 sub-exceptions)\n",
        "  +-+---------------- 1 ----------------\n",
        "    | ValueError: test_drain queue fixture was shared\n",
        "    +---------------- 2 ----------------\n",
        "    | RuntimeError: test_drain worker used stale queue\n",
        "    +------------------------------------\n",
    )
    .to_owned()
}

fn evidence_id(number: u32) -> String {
    crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(9700 + u128::from(number)))
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
            path: "/tmp/python-group-recall".to_owned(),
            name: None,
        },
    )?;
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(9600)).to_string();
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: "/sessions/python-groups.jsonl".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 5,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(b"python-group-recall").to_hex()),
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
fn grouped_failures_reach_cited_recall_and_pack_expansion_without_source_rewrites() -> TestResult {
    let failed = failure();
    let diagnostics = structured_error_diagnostics(&failed);
    assert_eq!(diagnostics.len(), 2);
    let episode = episode(&failed, "queue workers completed", COMMAND)?;
    let before = episode
        .db
        .list_evidence_spans_for_session(&episode.session)?;
    let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.fingerprints_recorded, 2);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.repair_links_recorded, 4);
    assert_eq!(report.incident_cards_recorded, 1);
    let card_id = incident_card_id(WS, &episode.failure);
    let card = episode
        .db
        .get_evidence_span(&card_id)?
        .ok_or("missing card")?;
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&episode.db, WS)?.is_empty());
    for diagnostic in &diagnostics {
        let recall = error_recall_report(&episode.db, WS, diagnostic)?;
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
        let seed = pack_error_recall_query_seed(&episode.db, WS, diagnostic)?;
        assert!(seed.contains("prior fix:") && seed.contains("fresh queue"));
        assert!(!seed.contains("private_group_label"));
        assert!(!seed.contains("private_source_sentinel"));
    }
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
    for diagnostic in &diagnostics {
        assert_eq!(
            episode
                .db
                .list_error_repair_links(WS, &diagnostic.layered_key().key)?
                .len(),
            3
        );
    }
    Ok(())
}

#[test]
fn grouped_or_damaged_retries_and_different_commands_never_teach_a_helpful_repair() -> TestResult {
    let failed = failure();
    let diagnostics = structured_error_diagnostics(&failed);
    for (proof, command) in [
        (failed.clone(), COMMAND),
        (
            failed.replace("    +------------------------------------\n", ""),
            COMMAND,
        ),
        (
            failed.replace(
                "---------------- 2 ----------------",
                "---------------- ... ----------------",
            ),
            COMMAND,
        ),
        (format!("{failed}Process exited with code 0\n"), COMMAND),
        (
            "queue workers completed".to_owned(),
            "python other_queue.py",
        ),
    ] {
        let episode = episode(&failed, &proof, command)?;
        let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        for diagnostic in &diagnostics {
            let recall = error_recall_report(&episode.db, WS, diagnostic)?;
            assert!(recall.exact);
            assert!(recall.helpful_repairs.is_empty());
            assert!(recall.proof_links.is_empty());
            assert!(
                !pack_error_recall_query_seed(&episode.db, WS, diagnostic)?.contains("prior fix:")
            );
        }
    }
    Ok(())
}

#[test]
fn an_incomplete_group_cannot_leave_prefix_fingerprints_even_after_a_clean_retry() -> TestResult {
    let full = failure();
    let diagnostics = structured_error_diagnostics(&full);
    for partial in [
        full.replace("(2 sub-exceptions)", "(3 sub-exceptions)"),
        full.replace("    +------------------------------------\n", ""),
        full.replace(
            "RuntimeError: test_drain worker used stale queue",
            "unfinished",
        ),
    ] {
        let episode = episode(&partial, "queue workers completed", COMMAND)?;
        let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(report.fingerprints_recorded, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        for diagnostic in &diagnostics {
            let recall = error_recall_report(&episode.db, WS, diagnostic)?;
            assert!(!recall.exact);
            assert!(recall.helpful_repairs.is_empty());
            assert!(recall.proof_links.is_empty());
        }
    }
    Ok(())
}
