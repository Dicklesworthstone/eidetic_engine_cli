//! Real-store code-less recall and authoritative final streamed diagnostics.

use super::{record_session_error_recall, structured_error_diagnostics};
use crate::core::error_diagnosis::{error_recall_report, pack_error_recall_query_seed};
use crate::core::incident_card::{incident_card_id, proof::CardChecks};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use serde_json::{Value, json};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;
type TestResult = Fallible<()>;
const WS: &str = "wsp_01234567890123456789012345";
const CWD: &str = "/tmp/cargo-text-recall";
const COMMAND: &str = "cargo check --locked";
const ROOT: &str = "error: failed to run custom build command for `widget v1.0.0`";
const CAUSE: &str = "\nCaused by:\n  missing generated native header\n";
const FIX: &str = "I fixed the build script failure by adding the missing generated native header before compiling the widget library.";

struct Fixture {
    db: DbConnection,
    session: String,
    line: u32,
}

impl Fixture {
    fn new() -> Fallible<Self> {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        db.insert_workspace(
            WS,
            &CreateWorkspaceInput {
                path: CWD.to_owned(),
                name: None,
            },
        )?;
        let session =
            crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x751100)).to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: WS.to_owned(),
                cass_session_id: "/sessions/cargo-text-recall.jsonl".to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 0,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"cargo-text-recall").to_hex()),
                metadata_json: None,
            },
        )?;
        Ok(Self {
            db,
            session,
            line: 0,
        })
    }

    fn add(&mut self, kind: &str, role: &str, record: Value) -> Fallible<String> {
        self.line += 1;
        let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
            0x751200 + u128::from(self.line),
        ))
        .to_string();
        let excerpt = record.to_string();
        self.db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: self.session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("cargo-text:{}", self.line),
                span_kind: kind.to_owned(),
                start_line: self.line,
                end_line: self.line,
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

    fn call(&mut self, id: &str, command: &str, cwd: &str) -> Fallible<String> {
        self.add(
            "tool_call",
            "assistant",
            json!({"type":"response_item","payload":{
                "type":"function_call","name":"exec_command","call_id":id,
                "arguments":json!({"cmd":command,"workdir":cwd}).to_string()
            }}),
        )
    }

    fn poll(&mut self, id: &str) -> Fallible<String> {
        self.add(
            "tool_call",
            "assistant",
            json!({"type":"response_item","payload":{
                "type":"function_call","name":"write_stdin","call_id":id,
                "arguments":json!({"session_id":42,"chars":""}).to_string()
            }}),
        )
    }

    fn result(&mut self, id: &str, body: &str, code: Option<i32>) -> Fallible<String> {
        let status = code.map_or_else(
            || "Process running with session ID 42".to_owned(),
            |code| format!("Process exited with code {code}"),
        );
        self.add(
            "tool_result",
            "tool",
            json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":id,
                "output":format!("Wall time: 0.01 seconds\n{status}\nOutput:\n{body}")
            }}),
        )
    }

    fn repair(&mut self) -> Fallible<String> {
        self.add(
            "message",
            "assistant",
            json!({"type":"assistant","message":{"role":"assistant","content":FIX}}),
        )
    }

    fn verify(&mut self) -> Fallible<String> {
        self.call("verify", COMMAND, CWD)?;
        self.result("verify", "", Some(0))
    }
}

#[test]
fn code_less_failures_persist_cited_repairs_and_packable_cards_idempotently() -> TestResult {
    let output = format!("{ROOT}{CAUSE}");
    let mut fixture = Fixture::new()?;
    fixture.call("fail", COMMAND, CWD)?;
    let failure = fixture.result("fail", &output, Some(101))?;
    let repair = fixture.repair()?;
    let proof = fixture.verify()?;
    let before = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.fingerprints_recorded, 1);
    assert_eq!(report.repair_links_recorded, 2);
    assert_eq!(report.incident_cards_recorded, 1);
    let diagnostic = structured_error_diagnostics(&output)
        .into_iter()
        .next()
        .ok_or("diagnostic")?;
    let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
    assert!(recall.exact);
    assert!(recall.helpful_repairs.contains(&repair));
    assert_eq!(recall.proof_links, vec![proof]);
    let card_id = incident_card_id(WS, &failure);
    assert!(recall.helpful_repairs.contains(&card_id));
    let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&fixture.db, WS)?.is_empty());
    let seed = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
    assert!(seed.contains("prior fix:"));
    assert!(seed.contains("generated native header"));
    for source in before {
        let after = fixture.db.get_evidence_span(&source.id)?.ok_or("source")?;
        assert_eq!(source.excerpt, after.excerpt);
        assert_eq!(source.content_hash, after.content_hash);
        assert_eq!(source.metadata_json, after.metadata_json);
        assert_eq!(
            source.canonical_provenance_uri(),
            after.canonical_provenance_uri()
        );
    }
    let second = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(second.incident_cards_recorded, 0);
    assert_eq!(
        fixture
            .db
            .list_error_repair_links(WS, &diagnostic.layered_key().key)?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn final_streamed_causes_replace_provisional_template_keys_without_changing_the_anchor()
-> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.call("fail", COMMAND, CWD)?;
    let failure = fixture.result("fail", ROOT, None)?;
    fixture.poll("finish")?;
    fixture.result("finish", CAUSE, Some(101))?;
    fixture.repair()?;
    let proof = fixture.verify()?;
    let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.fingerprints_recorded, 1);
    assert_eq!(report.incident_cards_recorded, 1);
    let final_diagnostic = structured_error_diagnostics(&format!("{ROOT}{CAUSE}"))
        .into_iter()
        .next()
        .ok_or("final diagnostic")?;
    let provisional = structured_error_diagnostics(ROOT)
        .into_iter()
        .next()
        .ok_or("prefix")?;
    assert_ne!(
        final_diagnostic.layered_key().key,
        provisional.layered_key().key
    );
    assert!(
        fixture
            .db
            .get_error_fingerprint(WS, &provisional.layered_key().key)?
            .is_none()
    );
    let recall = error_recall_report(&fixture.db, WS, &final_diagnostic)?;
    assert!(recall.exact);
    assert_eq!(recall.proof_links, vec![proof]);
    let card_id = incident_card_id(WS, &failure);
    assert!(recall.helpful_repairs.contains(&card_id));
    let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
    assert!(card.excerpt.contains("missing generated native header"));
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&fixture.db, WS)?.is_empty());
    assert!(
        fixture
            .db
            .list_error_repair_links(WS, &final_diagnostic.layered_key().key)?
            .iter()
            .all(|link| link.evidence_ref.as_deref() == Some(failure.as_str()))
    );
    Ok(())
}

#[test]
fn later_polled_rustc_errors_are_retained_in_the_same_failed_invocation() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.call("fail", COMMAND, CWD)?;
    let failure = fixture.result("fail", "error[E0308]: mismatched types\n", None)?;
    fixture.poll("finish")?;
    fixture.result("finish", "error[E0277]: missing trait bound\n", Some(101))?;
    fixture.repair()?;
    fixture.verify()?;
    let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.fingerprints_recorded, 2);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
    for key in ["rustc:E0308", "rustc:E0277"] {
        assert!(fixture.db.get_error_fingerprint(WS, key)?.is_some());
        let links = fixture.db.list_error_repair_links(WS, key)?;
        assert_eq!(links.len(), 3);
        assert!(
            links
                .iter()
                .any(|link| link.target_id == incident_card_id(WS, &failure))
        );
    }
    Ok(())
}

#[test]
fn an_invalid_final_native_report_cannot_authorize_a_later_successful_retry() -> TestResult {
    for (prefix, tail) in [
        (ROOT.to_owned(), "\nCaused by:".to_owned()),
        (
            json!({"reason":"compiler-message","message":{
                "code":{"code":"E0308"},"level":"error","message":"mismatched types"
            }})
            .to_string(),
            "\n{unfinished".to_owned(),
        ),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", &prefix, None)?;
        fixture.poll("finish")?;
        fixture.result("finish", &tail, Some(101))?;
        fixture.repair()?;
        fixture.verify()?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.fingerprints_recorded, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
    }
    Ok(())
}

#[test]
fn a_still_running_failure_cannot_be_verified_by_an_overlapping_retry() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.call("fail", COMMAND, CWD)?;
    fixture.result("fail", ROOT, None)?;
    fixture.repair()?;
    fixture.verify()?;
    fixture.poll("finish")?;
    fixture.result("finish", CAUSE, Some(101))?;
    let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 0);
    assert_eq!(report.incident_cards_recorded, 0);
    Ok(())
}

#[test]
fn changed_commands_or_working_directories_do_not_verify_code_less_repairs() -> TestResult {
    for (command, cwd) in [
        ("cargo check --locked --lib", CWD),
        (COMMAND, "/tmp/other-cargo"),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", &format!("{ROOT}{CAUSE}"), Some(101))?;
        fixture.repair()?;
        fixture.call("verify", command, cwd)?;
        fixture.result("verify", "", Some(0))?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
    }
    Ok(())
}

#[test]
fn quiet_retry_completion_does_not_accept_a_zero_exit_wrapping_another_cargo_failure() -> TestResult
{
    let mut fixture = Fixture::new()?;
    fixture.call("fail", COMMAND, CWD)?;
    fixture.result("fail", &format!("{ROOT}{CAUSE}"), Some(101))?;
    fixture.repair()?;
    fixture.call("verify", COMMAND, CWD)?;
    fixture.result(
        "verify",
        "error: failed to parse manifest at `Cargo.toml`",
        Some(0),
    )?;
    let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(report.failures_seen, 2);
    assert_eq!(report.resolved_failures, 0);
    assert_eq!(report.repair_links_recorded, 0);
    assert_eq!(report.incident_cards_recorded, 0);
    Ok(())
}
#[test]
fn terminal_failures_never_persist_repairs_even_when_the_wrapper_exits_zero() -> TestResult {
    let failure_output = format!("{ROOT}{CAUSE}");
    let diagnostic = structured_error_diagnostics(&failure_output)
        .into_iter()
        .next()
        .ok_or("diagnostic")?;
    for body in [
        "Tests: \u{1b}[31m1 failed\u{1b}[0m, 21 passed",
        "\u{1b}[31m1 failing\u{1b}[0m",
        "progress 20%\rTests: 1 failed\rTests: 22 passed",
        "Command timed\u{1b}[0m out\rFinished dev profile",
        "Finished dev profile\u{1b}[2K",
        "Finished dev profile\u{1b}[31",
    ] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", &failure_output, Some(101))?;
        fixture.repair()?;
        fixture.call("verify", COMMAND, CWD)?;
        fixture.result("verify", body, Some(0))?;
        let before = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert!(report.failures_seen >= 1, "{body:?}: {report:?}");
        assert_eq!(report.resolved_failures, 0, "{body:?}");
        assert_eq!(report.repair_links_recorded, 0, "{body:?}");
        assert_eq!(report.incident_cards_recorded, 0, "{body:?}");
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(recall.exact);
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
        assert!(
            !pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?.contains("prior fix:")
        );
        let replay = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(replay.repair_links_recorded, 0);
        assert_eq!(replay.incident_cards_recorded, 0);
        for source in before {
            let after = fixture.db.get_evidence_span(&source.id)?.ok_or("source")?;
            assert_eq!(after.excerpt, source.excerpt);
            assert_eq!(after.content_hash, source.content_hash);
            assert_eq!(after.metadata_json, source.metadata_json);
            assert_eq!(
                after.canonical_provenance_uri(),
                source.canonical_provenance_uri()
            );
        }
    }
    Ok(())
}

#[test]
fn completed_styled_retries_keep_live_card_proof_and_original_source_bytes() -> TestResult {
    let failure_output = format!("{ROOT}{CAUSE}");
    let diagnostic = structured_error_diagnostics(&failure_output)
        .into_iter()
        .next()
        .ok_or("diagnostic")?;
    for body in [
        "\u{1b}[32mFinished dev profile\u{1b}[0m",
        "progress 20%\rprogress 100%\rFinished dev profile",
        "\u{1b}]8;;https://example.test/build\u{7}Finished dev profile\u{1b}]8;;\u{7}",
        "\u{1b}]8;;https://example.test/build\u{1b}\\Finished dev profile\u{1b}]8;;\u{1b}\\",
    ] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        let failure = fixture.result("fail", &failure_output, Some(101))?;
        let repair = fixture.repair()?;
        fixture.call("verify", COMMAND, CWD)?;
        let proof = fixture.result("verify", body, Some(0))?;
        let before = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.resolved_failures, 1, "{body:?}: {report:?}");
        assert_eq!(report.incident_cards_recorded, 1);
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(recall.helpful_repairs.contains(&repair));
        assert_eq!(recall.proof_links, vec![proof]);
        let card_id = incident_card_id(WS, &failure);
        assert!(recall.helpful_repairs.contains(&card_id));
        let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        assert!(checks.withheld(&fixture.db, WS)?.is_empty());
        assert!(pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?.contains("prior fix:"));
        for source in before {
            let after = fixture.db.get_evidence_span(&source.id)?.ok_or("source")?;
            assert_eq!(source.excerpt, after.excerpt);
            assert_eq!(source.content_hash, after.content_hash);
            assert_eq!(source.metadata_json, after.metadata_json);
            assert_eq!(
                source.canonical_provenance_uri(),
                after.canonical_provenance_uri()
            );
        }
        let replay = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(replay.incident_cards_recorded, 0);
        assert_eq!(
            fixture
                .db
                .list_error_repair_links(WS, &diagnostic.layered_key().key)?
                .len(),
            3
        );
    }
    Ok(())
}

#[test]
fn split_terminal_sequences_wait_for_completion_without_poisoning_later_proof() -> TestResult {
    for (prefix, tail, completed) in [
        ("Finished \u{1b}[3", "2mdev profile\u{1b}[0m", true),
        ("Tests: \u{1b}[3", "1m1 failed\u{1b}[0m, 21 passed", false),
        (
            "Finished \u{1b}]8;;https://example.test/build",
            "\u{7}dev profile\u{1b}]8;;\u{7}",
            true,
        ),
        ("Finished \u{1b}[", "2Kdev profile", false),
    ] {
        let mut fixture = Fixture::new()?;
        let failure_output = format!("{ROOT}{CAUSE}");
        fixture.call("fail", COMMAND, CWD)?;
        let failure = fixture.result("fail", &failure_output, Some(101))?;
        fixture.repair()?;
        fixture.call("verify", COMMAND, CWD)?;
        fixture.result("verify", prefix, None)?;
        let interim = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(interim.resolved_failures, 0);
        assert_eq!(interim.incident_cards_recorded, 0);
        fixture.poll("finish")?;
        let proof = fixture.result("finish", tail, Some(0))?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(
            report.resolved_failures,
            if completed { 1 } else { 0 },
            "{prefix:?} {tail:?}"
        );
        let diagnostic = structured_error_diagnostics(&failure_output)
            .into_iter()
            .next()
            .ok_or("diagnostic")?;
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        if completed {
            assert_eq!(recall.proof_links, vec![proof]);
            let card_id = incident_card_id(WS, &failure);
            let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
            let mut checks = CardChecks::default();
            checks.observe(&card)?;
            assert!(checks.withheld(&fixture.db, WS)?.is_empty());
        } else {
            assert!(recall.helpful_repairs.is_empty());
            assert!(recall.proof_links.is_empty());
            assert_eq!(report.incident_cards_recorded, 0);
        }
    }
    Ok(())
}

#[test]
fn styled_cargo_failures_reach_live_recall_without_rewriting_source_evidence() -> TestResult {
    let plain = format!("{ROOT}{CAUSE}");
    let diagnostic = structured_error_diagnostics(&plain)
        .into_iter()
        .next()
        .ok_or("diagnostic")?;
    for styled in [
        format!("\u{1b}[31m{plain}\u{1b}[0m"),
        plain
            .replace("error:", "\u{1b}[31merror:\u{1b}[0m")
            .replace("Caused by:", "\u{1b}[1mCaused by:\u{1b}[0m"),
        format!("\u{1b}]8;;https://example.test/build\u{7}{plain}\u{1b}]8;;\u{7}"),
    ] {
        assert_eq!(
            structured_error_diagnostics(&styled),
            vec![diagnostic.clone()]
        );
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        let failure = fixture.result("fail", &styled, Some(101))?;
        let repair = fixture.repair()?;
        fixture.call("verify", COMMAND, CWD)?;
        let proof = fixture.result("verify", "\u{1b}[32mFinished dev profile\u{1b}[0m", Some(0))?;
        let before = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.fingerprints_recorded, 1);
        assert_eq!(report.resolved_failures, 1);
        assert_eq!(report.incident_cards_recorded, 1);
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(recall.exact);
        assert!(recall.helpful_repairs.contains(&repair));
        assert_eq!(recall.proof_links, vec![proof]);
        let card_id = incident_card_id(WS, &failure);
        let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        assert!(checks.withheld(&fixture.db, WS)?.is_empty());
        assert!(recall.helpful_repairs.contains(&card_id));
        assert!(pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?.contains("prior fix:"));
        for source in before {
            let after = fixture.db.get_evidence_span(&source.id)?.ok_or("source")?;
            assert_eq!(after.excerpt, source.excerpt);
            assert_eq!(after.content_hash, source.content_hash);
            assert_eq!(after.metadata_json, source.metadata_json);
            assert_eq!(
                after.canonical_provenance_uri(),
                source.canonical_provenance_uri()
            );
        }
        let repeated = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(repeated.incident_cards_recorded, 0);
        assert_eq!(
            fixture
                .db
                .list_error_repair_links(WS, &diagnostic.layered_key().key)?
                .len(),
            3
        );
    }
    Ok(())
}

#[test]
fn split_cargo_presentation_waits_for_complete_native_diagnostics_before_learning() -> TestResult {
    for complete in [false, true] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", "\u{1b}[3", None)?;
        let prefix = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(prefix.fingerprints_recorded, 0);
        assert_eq!(prefix.repair_links_recorded, 0);
        assert_eq!(prefix.incident_cards_recorded, 0);
        fixture.poll("finish")?;
        let tail = if complete {
            format!("1m{ROOT}{CAUSE}\u{1b}[0m")
        } else {
            format!("1m{ROOT}\nCaused by:\u{1b}[0m")
        };
        let failure = fixture.result("finish", &tail, Some(101))?;
        fixture.repair()?;
        let proof = fixture.verify()?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.resolved_failures, if complete { 1 } else { 0 });
        assert_eq!(report.incident_cards_recorded, if complete { 1 } else { 0 });
        if complete {
            let diagnostic = structured_error_diagnostics(&format!("{ROOT}{CAUSE}"))
                .into_iter()
                .next()
                .ok_or("diagnostic")?;
            let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
            assert_eq!(recall.proof_links, vec![proof]);
            let card = fixture
                .db
                .get_evidence_span(&incident_card_id(WS, &failure))?
                .ok_or("card")?;
            let mut checks = CardChecks::default();
            checks.observe(&card)?;
            assert!(checks.withheld(&fixture.db, WS)?.is_empty());
        } else {
            assert_eq!(report.repair_links_recorded, 0);
            assert_eq!(report.fingerprints_recorded, 0);
        }
    }
    Ok(())
}

#[test]
fn mixed_native_failures_share_one_verified_repair_without_captured_error_classes() -> TestResult {
    let cargo =
        format!("{ROOT}{CAUSE}  --- stdout\n  error[E9999]: captured build-script example\n");
    let coded = "error[E0308]: mismatched types\n --> src/widget.rs:2:1\n2 | fn widget() -> bool { 7 }\n  |                      ^ expected bool\n";
    let linker = "error: linking with `cc` failed: exit status: 1\n  = note: undefined symbol\n";
    for order in [
        [cargo.as_str(), coded, linker],
        [coded, linker, cargo.as_str()],
        [linker, cargo.as_str(), coded],
    ] {
        for styled in [false, true] {
            let plain = order.join("\n");
            let output = if styled {
                format!("\u{1b}[31m{plain}\u{1b}[0m")
            } else {
                plain.clone()
            };
            let diagnostics = structured_error_diagnostics(&output);
            assert_eq!(diagnostics.len(), 3);
            assert_eq!(diagnostics, structured_error_diagnostics(&plain));
            let mut fixture = Fixture::new()?;
            fixture.call("fail", COMMAND, CWD)?;
            let failure = fixture.result("fail", &output, Some(101))?;
            let repair = fixture.repair()?;
            let proof = fixture.verify()?;
            let before = fixture
                .db
                .list_evidence_spans_for_session(&fixture.session)?;
            let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
            assert_eq!(report.failures_seen, 1);
            assert_eq!(report.resolved_failures, 1);
            assert_eq!(report.fingerprints_recorded, 3);
            assert_eq!(report.incident_cards_recorded, 1);
            let card_id = incident_card_id(WS, &failure);
            for diagnostic in &diagnostics {
                let recall = error_recall_report(&fixture.db, WS, diagnostic)?;
                assert!(recall.exact);
                assert!(recall.helpful_repairs.contains(&repair));
                assert!(recall.helpful_repairs.contains(&card_id));
                assert_eq!(recall.proof_links, vec![proof.clone()]);
                assert!(
                    pack_error_recall_query_seed(&fixture.db, WS, diagnostic)?
                        .contains("prior fix:")
                );
                assert!(
                    fixture
                        .db
                        .list_error_repair_links(WS, &diagnostic.layered_key().key)?
                        .iter()
                        .all(|link| link.evidence_ref.as_deref() == Some(failure.as_str()))
                );
            }
            assert!(
                fixture
                    .db
                    .get_error_fingerprint(WS, "rustc:E9999")?
                    .is_none()
            );
            let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
            let mut checks = CardChecks::default();
            checks.observe(&card)?;
            assert!(checks.withheld(&fixture.db, WS)?.is_empty());
            for source in before {
                let after = fixture.db.get_evidence_span(&source.id)?.ok_or("source")?;
                assert_eq!(after.excerpt, source.excerpt);
                assert_eq!(after.content_hash, source.content_hash);
                assert_eq!(after.metadata_json, source.metadata_json);
                assert_eq!(
                    after.canonical_provenance_uri(),
                    source.canonical_provenance_uri()
                );
            }
            assert_eq!(
                record_session_error_recall(&fixture.db, WS, &fixture.session)?
                    .incident_cards_recorded,
                0
            );
            for diagnostic in &diagnostics {
                assert_eq!(
                    fixture
                        .db
                        .list_error_repair_links(WS, &diagnostic.layered_key().key)?
                        .len(),
                    3
                );
            }
        }
    }
    Ok(())
}

#[test]
fn streamed_mixed_failures_keep_one_anchor_and_add_the_later_native_class() -> TestResult {
    let coded = "error[E0308]: mismatched types\n";
    let cargo = format!("{ROOT}{CAUSE}");
    for (prefix, tail) in [(coded, cargo.as_str()), (cargo.as_str(), coded)] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        let failure = fixture.result("fail", prefix, None)?;
        let pending = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(pending.resolved_failures, 0);
        assert_eq!(pending.repair_links_recorded, 0);
        fixture.poll("finish")?;
        fixture.result("finish", tail, Some(101))?;
        let repair = fixture.repair()?;
        let proof = fixture.verify()?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 1);
        assert_eq!(report.fingerprints_recorded, 2);
        assert_eq!(report.incident_cards_recorded, 1);
        let diagnostics = structured_error_diagnostics(&format!("{prefix}{tail}"));
        assert_eq!(diagnostics.len(), 2);
        for diagnostic in &diagnostics {
            let recall = error_recall_report(&fixture.db, WS, diagnostic)?;
            assert!(recall.helpful_repairs.contains(&repair));
            assert!(
                recall
                    .helpful_repairs
                    .contains(&incident_card_id(WS, &failure))
            );
            assert_eq!(recall.proof_links, vec![proof.clone()]);
        }
        let card = fixture
            .db
            .get_evidence_span(&incident_card_id(WS, &failure))?
            .ok_or("card")?;
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        assert!(checks.withheld(&fixture.db, WS)?.is_empty());
    }
    Ok(())
}

#[test]
fn malformed_mixed_tails_cannot_turn_a_provisional_fingerprint_into_verified_advice() -> TestResult
{
    for (prefix, tail) in [
        (format!("{ROOT}{CAUSE}"), "error[E0308]: \n".to_owned()),
        (
            "error[E0308]: mismatched types\n".to_owned(),
            format!("{ROOT}\nCaused by:"),
        ),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", &prefix, None)?;
        let pending = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(pending.repair_links_recorded, 0);
        fixture.poll("finish")?;
        fixture.result("finish", &tail, Some(101))?;
        fixture.repair()?;
        fixture.verify()?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        assert!(structured_error_diagnostics(&format!("{prefix}{tail}")).is_empty());
        for diagnostic in [
            crate::core::error_recall::from_rustc(Some("E0308"), "mismatched types"),
            structured_error_diagnostics(&format!("{ROOT}{CAUSE}"))
                .into_iter()
                .next()
                .ok_or("cargo diagnostic")?,
        ] {
            let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
            assert!(recall.helpful_repairs.is_empty());
            assert!(recall.proof_links.is_empty());
        }
        assert_eq!(
            record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded,
            0
        );
    }
    Ok(())
}

const NATIVE_RUSTC_REPORT: &str = "warning: unused variable: `value`\n --> src/widget.rs:1:5\n1 | let value = 0;\n  |     ^^^^^\nerror[E0308]: mismatched types\n --> src/widget.rs:2:1\n2 | fn widget() -> bool { 7 }\n  |                      ^ expected bool\n3 | let sample = \"error[E9999]: source example\";\nerror[E0277]: the trait bound `Widget: Serialize` is not satisfied\n --> src/widget.rs:4:1\n4 | save(widget);\n  |      ^^^^^^\nSome errors have detailed explanations: E0277, E0308.\nFor more information about an error, try `rustc --explain E0277`.\nerror: could not compile `widget` (lib) due to 2 previous errors; 1 warning emitted\n";

fn native_rustc_repair(fixture: &mut Fixture) -> Fallible<String> {
    fixture.add(
        "message",
        "assistant",
        json!({"type":"assistant","message":{
            "role":"assistant",
            "content":"I fixed the mismatched types by returning a boolean from the widget function and added the Serialize derive to resolve the missing trait bound."
        }}),
    )
}

#[test]
fn standalone_rustc_reports_produce_complete_cited_repairs_without_source_decoys() -> TestResult {
    let expected = structured_error_diagnostics(NATIVE_RUSTC_REPORT);
    assert_eq!(expected.len(), 2);
    assert_eq!(expected[0].layered_key().key, "rustc:E0308");
    assert_eq!(expected[1].layered_key().key, "rustc:E0277");
    for output in [
        NATIVE_RUSTC_REPORT.to_owned(),
        format!("\u{1b}[31m{NATIVE_RUSTC_REPORT}\u{1b}[0m"),
        NATIVE_RUSTC_REPORT.replace("E0308", "E03\u{1b}[31m08\u{1b}[0m"),
        NATIVE_RUSTC_REPORT.replace("error[", "er\u{1b}[1mror\u{1b}[0m["),
        format!("\u{1b}]8;;https://example.test/compiler\u{7}{NATIVE_RUSTC_REPORT}\u{1b}]8;;\u{7}"),
    ] {
        assert_eq!(structured_error_diagnostics(&output), expected);
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        let failure = fixture.result("fail", &output, Some(101))?;
        let repair = native_rustc_repair(&mut fixture)?;
        let proof = fixture.verify()?;
        let original = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 1);
        assert_eq!(report.fingerprints_recorded, 2);
        assert_eq!(report.incident_cards_recorded, 1);
        let card_id = incident_card_id(WS, &failure);
        for diagnostic in &expected {
            let recall = error_recall_report(&fixture.db, WS, diagnostic)?;
            assert!(recall.exact);
            assert!(recall.helpful_repairs.contains(&repair));
            assert!(recall.helpful_repairs.contains(&card_id));
            assert_eq!(recall.proof_links, vec![proof.clone()]);
            assert!(pack_error_recall_query_seed(&fixture.db, WS, diagnostic)?.contains("prior fix:"));
        }
        assert!(fixture.db.get_error_fingerprint(WS, "rustc:E9999")?.is_none());
        let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
        assert!(card.excerpt.contains("Symptom: error[E0308]: mismatched types (src/widget.rs:2:1)"));
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        assert!(checks.withheld(&fixture.db, WS)?.is_empty());
        for row in original {
            let current = fixture.db.get_evidence_span(&row.id)?.ok_or("source")?;
            assert_eq!(current.excerpt, row.excerpt);
            assert_eq!(current.content_hash, row.content_hash);
            assert_eq!(current.metadata_json, row.metadata_json);
            assert_eq!(current.canonical_provenance_uri(), row.canonical_provenance_uri());
        }
        assert_eq!(record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded, 0);
        for diagnostic in &expected {
            assert_eq!(fixture.db.list_error_repair_links(WS, &diagnostic.layered_key().key)?.len(), 3);
        }
    }
    Ok(())
}

#[test]
fn standalone_rustc_polls_wait_for_a_complete_later_code_and_keep_the_original_anchor() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.call("fail", COMMAND, CWD)?;
    let failure = fixture.result("fail", "error[E0308]: mismatched types\n", None)?;
    let initial = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(initial.fingerprints_recorded, 1);
    assert_eq!(initial.repair_links_recorded, 0);
    fixture.poll("middle")?;
    fixture.result("middle", "error[E02\u{1b}[3", None)?;
    let incomplete = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(incomplete.resolved_failures, 0);
    assert_eq!(incomplete.repair_links_recorded, 0);
    assert_eq!(incomplete.incident_cards_recorded, 0);
    fixture.poll("finish")?;
    fixture.result("finish", "1m77\u{1b}[0m]: the trait bound `Widget: Serialize` is not satisfied\n", Some(101))?;
    let repair = native_rustc_repair(&mut fixture)?;
    let proof = fixture.verify()?;
    let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.fingerprints_recorded, 2);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
    for diagnostic in structured_error_diagnostics(NATIVE_RUSTC_REPORT) {
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(recall.helpful_repairs.contains(&repair));
        assert!(recall.helpful_repairs.contains(&incident_card_id(WS, &failure)));
        assert_eq!(recall.proof_links, vec![proof.clone()]);
        assert!(fixture.db.list_error_repair_links(WS, &diagnostic.layered_key().key)?
            .iter().all(|link| link.evidence_ref.as_deref() == Some(failure.as_str())));
    }
    let card = fixture.db.get_evidence_span(&incident_card_id(WS, &failure))?.ok_or("card")?;
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&fixture.db, WS)?.is_empty());
    Ok(())
}

#[test]
fn malformed_final_rustc_reports_never_become_verified_advice_after_a_clean_retry() -> TestResult {
    let diagnostic = crate::core::error_recall::from_rustc(Some("E0308"), "mismatched types");
    for tail in [
        "error[E0277]: ",
        "error[E027]: missing trait bound",
        "Caused by:",
        "\u{1b}[31",
        "\u{1b}[2K",
        "unrelated compiler output after the final diagnostic",
    ] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", "error[E0308]: mismatched types\n", None)?;
        let initial = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(initial.fingerprints_recorded, 1);
        assert_eq!(initial.repair_links_recorded, 0);
        fixture.poll("finish")?;
        fixture.result("finish", tail, Some(101))?;
        native_rustc_repair(&mut fixture)?;
        fixture.verify()?;
        let original = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.resolved_failures, 0, "{tail:?}");
        assert_eq!(report.repair_links_recorded, 0, "{tail:?}");
        assert_eq!(report.incident_cards_recorded, 0, "{tail:?}");
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
        assert!(!pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?.contains("prior fix:"));
        assert_eq!(fixture.db.list_evidence_spans_for_session(&fixture.session)?, original);
        assert_eq!(record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded, 0);
    }
    Ok(())
}
