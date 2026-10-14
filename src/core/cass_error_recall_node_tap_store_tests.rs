//! Native Node reports through the real migrated store and public recall path.

use super::decode;
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
const COMMAND: &str = "node --test --test-reporter=tap cache.test.cjs";
const REPAIR: &str = "The cache returned a stale revision because the lookup read the old entry. I fixed the cache lookup to return the stored revision instead of the stale entry.";
const FAILED: &str = include_str!("../../tests/fixtures/cass_node_tap/flat_failed.tap");
const PASSED: &str = include_str!("../../tests/fixtures/cass_node_tap/flat_passed.tap");

struct Episode {
    db: DbConnection,
    session: String,
}

fn evidence_id(number: u32) -> String {
    crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(9700 + u128::from(number)))
        .to_string()
}

fn episode(failure: &str, proof: &str, proof_command: &str) -> TestResult<Episode> {
    let db = DbConnection::open_memory()?;
    db.migrate()?;
    db.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/node-tap-recall".to_owned(),
            name: None,
        },
    )?;
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(9600)).to_string();
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: "/sessions/node-tap.jsonl".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 5,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(b"node-tap-recall").to_hex()),
            metadata_json: None,
        },
    )?;
    // Authored transcript framing around captured native producer output.
    // The green wrapper is deliberately optimistic, including negative cases.
    let rows = [
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"failure","name":"Bash",
                "input":{"command":COMMAND}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"failure",
                "content":failure,"is_error":true}),
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
    Ok(Episode { db, session })
}

#[test]
fn native_node_repairs_reach_cited_cards_and_pack_recall_without_source_mutation() -> TestResult {
    for (failure, proof) in [
        (FAILED, PASSED),
        (
            include_str!("../../tests/fixtures/cass_node_tap/nested_failed.tap"),
            include_str!("../../tests/fixtures/cass_node_tap/nested_passed.tap"),
        ),
    ] {
        let episode = episode(failure, proof, COMMAND)?;
        let before = episode
            .db
            .list_evidence_spans_for_session(&episode.session)?;
        let result = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(result.failures_seen, 1);
        assert_eq!(result.resolved_failures, 1);
        assert_eq!(result.repair_links_recorded, 2);
        assert_eq!(result.incident_cards_recorded, 1);
        let decoded = decode(failure)
            .map_err(|()| "invalid native fixture")?
            .ok_or("missing native fixture")?;
        let diagnostic = decoded
            .output
            .diagnostics
            .first()
            .ok_or("missing diagnostic")?;
        let recall = error_recall_report(&episode.db, WS, diagnostic)?;
        let card = crate::core::incident_card::incident_card_id(WS, &evidence_id(2));
        assert!(recall.exact);
        assert!(recall.helpful_repairs.contains(&evidence_id(3)));
        assert!(recall.helpful_repairs.contains(&card));
        assert_eq!(recall.proof_links, vec![evidence_id(5)]);
        let links = episode
            .db
            .list_error_repair_links(WS, &diagnostic.layered_key().key)?;
        assert_eq!(links.len(), 3);
        assert!(
            links
                .iter()
                .all(|link| link.evidence_ref.as_deref() == Some(evidence_id(2).as_str()))
        );
        let evidence = recalled_repair_evidence(&episode.db, WS, &recall)?;
        assert!(evidence.iter().any(|item| {
            item.evidence_id == card
                && item.role == "incident_card"
                && item
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("Fix:"))
        }));
        assert!(
            evidence
                .iter()
                .any(|item| item.evidence_id == evidence_id(5) && item.text.is_none())
        );
        let seed = pack_error_recall_query_seed(&episode.db, WS, diagnostic)?;
        assert!(seed.contains("prior fix:"));
        assert!(seed.contains("stored revision"));
        for source in before {
            let after = episode
                .db
                .get_evidence_span(&source.id)?
                .ok_or("missing source")?;
            assert_eq!(after.excerpt, source.excerpt);
            assert_eq!(after.content_hash, source.content_hash);
            assert_eq!(
                after.canonical_provenance_uri(),
                source.canonical_provenance_uri()
            );
        }
        let rerun = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(rerun.incident_cards_recorded, 0);
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
fn invalid_node_retries_never_persist_helpful_repairs() -> TestResult {
    let skipped = PASSED
        .replace(
            "ok 1 - cache returns the saved revision\n",
            "ok 1 - cache returns the saved revision # SKIP\n",
        )
        .replace("# pass 2", "# pass 1")
        .replace("# skipped 0", "# skipped 1");
    let decoded = decode(FAILED)
        .map_err(|()| "invalid native fixture")?
        .ok_or("missing native fixture")?;
    let diagnostic = decoded
        .output
        .diagnostics
        .first()
        .ok_or("missing diagnostic")?;
    for (proof, command) in [
        (skipped, COMMAND),
        (PASSED.replace("saved revision", "unrelated value"), COMMAND),
        ("TAP version 13\n# Subtest: incomplete".to_owned(), COMMAND),
        (
            PASSED.to_owned(),
            "node --test --test-reporter=tap other.test.cjs",
        ),
    ] {
        let episode = episode(FAILED, &proof, command)?;
        let result = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(result.failures_seen, 1);
        assert_eq!(result.resolved_failures, 0);
        assert_eq!(result.repair_links_recorded, 0);
        assert_eq!(result.incident_cards_recorded, 0);
        let recall = error_recall_report(&episode.db, WS, diagnostic)?;
        assert!(recall.exact);
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
    }
    Ok(())
}
