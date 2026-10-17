//! Shipping derivation and reader proof over captured, real Cargo execution.

use crate::core::cass_error_recall::record_session_error_recall;
use crate::core::incident_card::{incident_card_id, proof::CardChecks};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use crate::models::{EvidenceId, SessionId};
use serde_json::json;

const WS: &str = "wsp_01234567890123456789012345";
const COMMAND: &str = "cargo test --lib -- --test-threads=1";
// Captured with nightly-2026-08-31 from a dependency-free two-test crate;
// after fixing round_trip(), the retry added a third, regression test.
const FAILED: &str = "\nrunning 2 tests\ntest cache ... FAILED\ntest control ... ok\n\nfailures:\n\n---- cache stdout ----\n\nthread 'cache' (5004) panicked at src/lib.rs:2:22:\ncache must flush before reading\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\nfailures:\n    cache\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n";
const PASSED: &str = "\nrunning 3 tests\ntest cache ... ok\ntest control ... ok\ntest regression ... ok\n\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n";
const REPAIR: &str = "I fixed the cache by flushing writes before reading. Added a regression test to exercise a second read.";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Episode {
    db: DbConnection,
    session: String,
    failure: String,
    repair: String,
}

fn episode(retry: &str, command: &str) -> TestResult<Episode> {
    let db = DbConnection::open_memory()?;
    db.migrate()?;
    db.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/cargo-growth-real-execution".to_owned(),
            name: None,
        },
    )?;
    let session = SessionId::from_uuid(uuid::Uuid::from_u128(0x8820)).to_string();
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: "real-cargo-growth".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 5,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(b"real-cargo-growth").to_hex()),
            metadata_json: None,
        },
    )?;
    let id = |line: u32| {
        EvidenceId::from_uuid(uuid::Uuid::from_u128(0x8830 + u128::from(line))).to_string()
    };
    for (index, (kind, role, block)) in [
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"fail","name":"Bash","input":{"command":COMMAND}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"fail","content":FAILED,"is_error":true}),
        ),
        ("message", "assistant", json!({"type":"text","text":REPAIR})),
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"retry","name":"Bash","input":{"command":command}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"retry","content":retry,"is_error":false}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let line = u32::try_from(index + 1)?;
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        db.insert_evidence_span(
            &id(line),
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("real-cargo:{line}"),
                span_kind: kind.to_owned(),
                start_line: line,
                end_line: line,
                start_byte: None,
                end_byte: None,
                role: Some(role.to_owned()),
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
    })
}

#[test]
fn real_cargo_growth_creates_a_currently_proved_idempotent_incident() -> TestResult {
    let episode = episode(PASSED, COMMAND)?;
    let originals = episode
        .db
        .list_evidence_spans_for_session(&episode.session)?;
    let first = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(first.failures_seen, 1);
    assert_eq!(first.resolved_failures, 1);
    assert_eq!(first.incident_cards_recorded, 1);
    let card_id = incident_card_id(WS, &episode.failure);
    let card = episode
        .db
        .get_search_admitted_evidence_span(&card_id, WS)?
        .ok_or("admitted derived card")?;
    assert!(card.excerpt.contains("regression test"));
    assert!(card.excerpt.contains("Verified: `cargo test`"));
    assert!(!crate::policy::redact_public_replay_body(&card.reader_text()).redacted);
    episode.db.begin_read_snapshot()?;
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&episode.db, WS)?.is_empty());
    episode.db.rollback_read_snapshot()?;
    let second = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(second.resolved_failures, 1);
    assert_eq!(second.incident_cards_recorded, 0);
    for original in originals {
        let after = episode
            .db
            .get_evidence_span(&original.id)?
            .ok_or("original source retained")?;
        assert_eq!(after.excerpt, original.excerpt);
        assert_eq!(after.content_hash, original.content_hash);
        assert_eq!(after.metadata_json, original.metadata_json);
        assert_eq!(
            after.canonical_provenance_uri(),
            original.canonical_provenance_uri()
        );
    }
    assert!(
        episode
            .db
            .get_search_admitted_evidence_span(&episode.repair, WS)?
            .is_some()
    );
    Ok(())
}

#[test]
fn apparent_growth_cannot_erase_old_controls_or_change_the_invocation() -> TestResult {
    let skipped = PASSED
        .replace("test control ... ok", "test control ... ignored")
        .replace(
            "3 passed; 0 failed; 0 ignored",
            "2 passed; 0 failed; 1 ignored",
        );
    let dropped = PASSED
        .replace("running 3 tests", "running 2 tests")
        .replace("test control ... ok\n", "")
        .replace("3 passed", "2 passed");
    for (retry, command) in [
        (skipped.as_str(), COMMAND),
        (dropped.as_str(), COMMAND),
        (PASSED, "cargo test cache --lib -- --test-threads=1"),
    ] {
        let episode = episode(retry, command)?;
        let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        assert!(
            episode
                .db
                .get_evidence_span(&incident_card_id(WS, &episode.failure))?
                .is_none()
        );
        assert!(
            episode
                .db
                .get_search_admitted_evidence_span(&episode.repair, WS)?
                .is_some()
        );
    }
    Ok(())
}
