//! Real Go 1.27.0 test2json captures through durable derivation and reader proof.
//! Times, OutputType decorations and fractional durations are original bytes.

use super::*;
use crate::core::cass_error_recall::{ToolOutput, record_session_error_recall};
use crate::core::error_diagnosis::error_recall_report;
use crate::core::incident_card::{incident_card_id, proof::CardChecks};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use crate::models::{EvidenceId, SessionId};
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const COMMAND: &str = "go test -json -count=1 ./...";
const REPAIR: &str = "Fixed the cache test by flushing pending writes before reads in cache.go. Added a regression test for a second read.";

// Captured by executing a dependency-free Go project: TestCache failed while
// TestControl passed; after fixing the function, all three tests passed.
const FAILED: &str = r#"{"Time":"2026-10-17T23:57:02.036921723Z","Action":"start","Package":"example.org/cache"}
{"Time":"2026-10-17T23:57:02.038546128Z","Action":"run","Package":"example.org/cache","Test":"TestCache"}
{"Time":"2026-10-17T23:57:02.038593755Z","Action":"output","Package":"example.org/cache","Test":"TestCache","Output":"=== RUN   TestCache\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.043795366Z","Action":"output","Package":"example.org/cache","Test":"TestCache","Output":"    cache_test.go:4: cache must flush before reading\n","OutputType":"error"}
{"Time":"2026-10-17T23:57:02.043834072Z","Action":"output","Package":"example.org/cache","Test":"TestCache","Output":"--- FAIL: TestCache (0.01s)\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.043839124Z","Action":"fail","Package":"example.org/cache","Test":"TestCache","Elapsed":0.01}
{"Time":"2026-10-17T23:57:02.043847274Z","Action":"run","Package":"example.org/cache","Test":"TestControl"}
{"Time":"2026-10-17T23:57:02.043849438Z","Action":"output","Package":"example.org/cache","Test":"TestControl","Output":"=== RUN   TestControl\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.04700741Z","Action":"output","Package":"example.org/cache","Test":"TestControl","Output":"--- PASS: TestControl (0.00s)\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.047017279Z","Action":"pass","Package":"example.org/cache","Test":"TestControl","Elapsed":0}
{"Time":"2026-10-17T23:57:02.047019754Z","Action":"output","Package":"example.org/cache","Output":"FAIL\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.047382903Z","Action":"output","Package":"example.org/cache","Output":"FAIL\texample.org/cache\t0.010s\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.047412921Z","Action":"fail","Package":"example.org/cache","Elapsed":0.01}
"#;
const PASSED: &str = r#"{"Time":"2026-10-17T23:57:02.250404555Z","Action":"start","Package":"example.org/cache"}
{"Time":"2026-10-17T23:57:02.25211376Z","Action":"run","Package":"example.org/cache","Test":"TestCache"}
{"Time":"2026-10-17T23:57:02.252166293Z","Action":"output","Package":"example.org/cache","Test":"TestCache","Output":"=== RUN   TestCache\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.257330293Z","Action":"output","Package":"example.org/cache","Test":"TestCache","Output":"--- PASS: TestCache (0.01s)\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.257375091Z","Action":"pass","Package":"example.org/cache","Test":"TestCache","Elapsed":0.01}
{"Time":"2026-10-17T23:57:02.257389052Z","Action":"run","Package":"example.org/cache","Test":"TestControl"}
{"Time":"2026-10-17T23:57:02.257392521Z","Action":"output","Package":"example.org/cache","Test":"TestControl","Output":"=== RUN   TestControl\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.26055534Z","Action":"output","Package":"example.org/cache","Test":"TestControl","Output":"--- PASS: TestControl (0.00s)\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.260594026Z","Action":"pass","Package":"example.org/cache","Test":"TestControl","Elapsed":0}
{"Time":"2026-10-17T23:57:02.260600638Z","Action":"run","Package":"example.org/cache","Test":"TestRegression"}
{"Time":"2026-10-17T23:57:02.260604157Z","Action":"output","Package":"example.org/cache","Test":"TestRegression","Output":"=== RUN   TestRegression\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.262739331Z","Action":"output","Package":"example.org/cache","Test":"TestRegression","Output":"--- PASS: TestRegression (0.00s)\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.262771627Z","Action":"pass","Package":"example.org/cache","Test":"TestRegression","Elapsed":0}
{"Time":"2026-10-17T23:57:02.262775692Z","Action":"output","Package":"example.org/cache","Output":"PASS\n","OutputType":"frame"}
{"Time":"2026-10-17T23:57:02.263005658Z","Action":"output","Package":"example.org/cache","Output":"ok  \texample.org/cache\t0.012s\n"}
{"Time":"2026-10-17T23:57:02.263033993Z","Action":"pass","Package":"example.org/cache","Elapsed":0.013}
"#;

struct Episode {
    db: DbConnection,
    session: String,
}

impl Episode {
    fn new() -> TestResult<Self> {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        db.insert_workspace(
            WS,
            &CreateWorkspaceInput {
                path: "/tmp/go-fractional-native-recall".to_owned(),
                name: None,
            },
        )?;
        let session = SessionId::from_uuid(uuid::Uuid::from_u128(0x60_7000)).to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: WS.to_owned(),
                cass_session_id: "real-go-timing".to_owned(),
                source_path: None,
                agent_name: Some("claude_code".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 7,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"real-go-timing").to_hex()),
                metadata_json: None,
            },
        )?;
        Ok(Self { db, session })
    }

    fn append(&self, line: u32, kind: &str, role: &str, block: Value) -> TestResult<String> {
        let id =
            EvidenceId::from_uuid(uuid::Uuid::from_u128(0x60_7000 + u128::from(line))).to_string();
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        self.db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: self.session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("real-go:{line}"),
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
        Ok(id)
    }

    fn call(&self, line: u32, id: &str, command: &str) -> TestResult<String> {
        self.append(
            line,
            "tool_call",
            "assistant",
            json!({
                "type":"tool_use","id":id,"name":"Bash","input":{"command":command}
            }),
        )
    }

    fn result(&self, line: u32, id: &str, text: &str, is_error: bool) -> TestResult<String> {
        self.append(
            line,
            "tool_result",
            "user",
            json!({
                "type":"tool_result","tool_use_id":id,"content":text,"is_error":is_error
            }),
        )
    }
}

#[test]
fn actual_go_fractional_capture_records_repair_proof_and_preserves_originals() -> TestResult {
    let episode = Episode::new()?;
    let diagnostic = inspect(FAILED)
        .map_err(|_| "failure capture")?
        .ok_or("Go failure")?
        .diagnostics
        .remove(0);
    episode.call(1, "failure", COMMAND)?;
    let failed = episode.result(2, "failure", FAILED, true)?;
    let repair = episode.append(
        3,
        "message",
        "assistant",
        json!({"type":"text","text":REPAIR}),
    )?;
    let narrowed = PASSED
        .lines()
        .filter(|line| !line.contains("\"Test\":\"TestControl\""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        ToolOutput {
            text: narrowed.clone(),
            is_error: Some(false),
            exit_code: Some(0)
        }
        .succeeded()
    );
    episode.call(4, "narrowed", COMMAND)?;
    let wrong = episode.result(5, "narrowed", &narrowed, false)?;
    let initial = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(initial.failures_seen, 1);
    assert_eq!(initial.resolved_failures, 0);
    assert_eq!(initial.incident_cards_recorded, 0);
    assert!(
        error_recall_report(&episode.db, WS, &diagnostic)?
            .helpful_repairs
            .is_empty()
    );
    episode.call(6, "retry", COMMAND)?;
    let proof = episode.result(7, "retry", PASSED, false)?;
    let original = episode
        .db
        .list_evidence_spans_for_session(&episode.session)?;
    let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
    let recall = error_recall_report(&episode.db, WS, &diagnostic)?;
    assert!(recall.helpful_repairs.contains(&repair));
    assert_eq!(recall.proof_links, vec![proof]);
    assert!(!recall.proof_links.contains(&wrong));
    let card = episode
        .db
        .get_search_admitted_evidence_span(&incident_card_id(WS, &failed), WS)?
        .ok_or("admitted Go card")?;
    episode.db.begin_read_snapshot()?;
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&episode.db, WS)?.is_empty());
    episode.db.rollback_read_snapshot()?;
    let count = episode.db.count_table_rows("error_repair_links")?;
    assert_eq!(
        record_session_error_recall(&episode.db, WS, &episode.session)?.incident_cards_recorded,
        0
    );
    assert_eq!(episode.db.count_table_rows("error_repair_links")?, count);
    for span in original {
        let after = episode
            .db
            .get_evidence_span(&span.id)?
            .ok_or("retained source")?;
        assert_eq!(after.excerpt, span.excerpt);
        assert_eq!(after.metadata_json, span.metadata_json);
        assert_eq!(after.content_hash, span.content_hash);
        assert_eq!(
            after.canonical_provenance_uri(),
            span.canonical_provenance_uri()
        );
    }
    Ok(())
}

#[test]
fn actual_go_capture_cannot_be_certified_by_changed_invocation_or_bad_tail() -> TestResult {
    for (command, retry) in [
        (
            "go test -json -count=1 -run TestCache ./...",
            PASSED.to_owned(),
        ),
        (COMMAND, format!("{PASSED}{{")),
        (
            COMMAND,
            PASSED.replace("example.org/cache", "example.org/other"),
        ),
    ] {
        let episode = Episode::new()?;
        episode.call(1, "failure", COMMAND)?;
        let failure = episode.result(2, "failure", FAILED, true)?;
        episode.append(
            3,
            "message",
            "assistant",
            json!({"type":"text","text":REPAIR}),
        )?;
        episode.call(4, "retry", command)?;
        episode.result(5, "retry", &retry, false)?;
        let report = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        assert!(
            episode
                .db
                .get_evidence_span(&incident_card_id(WS, &failure))?
                .is_none()
        );
    }
    Ok(())
}
