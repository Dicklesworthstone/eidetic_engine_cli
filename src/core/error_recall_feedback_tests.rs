//! Negative feedback must affect advice, not merely an unused report field.

use super::tests::{OTHER_WS, TestResult, WS, db, report, session, span};
use super::*;
use crate::core::error_diagnosis::{
    ErrorRepairLinkRecording, error_recall_report, record_error_repair_links,
};
use crate::core::error_recall::from_rustc;

fn mark(
    connection: &DbConnection,
    workspace: &str,
    code: &str,
    id: &str,
    harmful: bool,
) -> TestResult {
    let mut recording = ErrorRepairLinkRecording::default();
    if harmful {
        recording.harmful_repairs.push(id.to_owned());
    } else {
        recording.helpful_repairs.push(id.to_owned());
    }
    record_error_repair_links(
        connection,
        workspace,
        &from_rustc(Some(code), "trait bound failed"),
        &recording,
    )?;
    Ok(())
}

#[test]
fn cached_helpful_report_cannot_override_new_harmful_feedback() -> TestResult {
    let connection = db()?;
    let session = session(&connection, WS, 910)?;
    let bad = span(&connection, WS, &session, 1, false)?;
    let good = span(&connection, WS, &session, 2, false)?;
    for id in [&bad, &good] {
        mark(&connection, WS, "E0277", id, false)?;
    }
    let canonical = from_rustc(Some("E0277"), "trait bound failed");
    let cached = error_recall_report(&connection, WS, &canonical)?;
    assert_eq!(read(&connection, WS, &cached)?.len(), 2);
    let source = connection.get_evidence_span(&bad)?;
    mark(&connection, WS, "E0277", &bad, true)?;
    let links = connection.list_error_repair_links(WS, "rustc:E0277")?;
    let mut hydrated = Vec::new();
    let actual = read_with(&connection, WS, &cached, |ids| {
        hydrated.extend(ids.iter().map(|id| (*id).to_owned()));
        connection.get_evidence_spans_with_sessions(ids)
    })?;
    assert_eq!(hydrated, std::slice::from_ref(&good));
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].evidence_id, good);
    assert_eq!(actual[0].role, "repair");
    assert!(
        actual[0]
            .text
            .as_deref()
            .is_some_and(|text| text.contains("Repair 2"))
    );
    assert_eq!(connection.get_evidence_span(&bad)?, source);
    assert_eq!(
        connection.list_error_repair_links(WS, "rustc:E0277")?,
        links
    );
    let history = error_recall_report(&connection, WS, &canonical)?;
    assert!(history.helpful_repairs.contains(&bad));
    assert_eq!(history.harmful_repairs, std::slice::from_ref(&bad));
    // Repeated positive observation is not an explicit adjudication of harm.
    mark(&connection, WS, "E0277", &bad, false)?;
    assert_eq!(read(&connection, WS, &cached)?, actual);
    Ok(())
}

#[test]
fn declared_harmful_repairs_are_not_hydrated_but_independent_proofs_remain_locators() -> TestResult
{
    let connection = db()?;
    let session = session(&connection, WS, 911)?;
    let bad_card = span(&connection, WS, &session, 1, true)?;
    let good = span(&connection, WS, &session, 2, false)?;
    let mut report = report();
    report.helpful_repairs = vec![bad_card.clone(), good.clone()];
    report.harmful_repairs = vec![bad_card.clone()];
    let mut hydrated = Vec::new();
    let positive = read_with(&connection, WS, &report, |ids| {
        hydrated.extend(ids.iter().map(|id| (*id).to_owned()));
        connection.get_evidence_spans_with_sessions(ids)
    })?;
    assert_eq!(hydrated, [good]);
    assert_eq!(positive.len(), 1);
    report.proof_links.push(bad_card.clone());
    let complete = read(&connection, WS, &report)?;
    assert_eq!(complete.len(), 2);
    assert_eq!(complete[0], positive[0]);
    assert_eq!(complete[1].evidence_id, bad_card);
    assert_eq!(complete[1].role, "proof");
    assert_eq!(complete[1].text, None);
    Ok(())
}

#[test]
fn harmful_link_scope_and_identity_are_exact() -> TestResult {
    let connection = db()?;
    let session = session(&connection, WS, 912)?;
    let live = span(&connection, WS, &session, 1, false)?;
    let mut report = report();
    report.helpful_repairs.push(live.clone());
    mark(&connection, OTHER_WS, "E0277", &live, true)?;
    mark(&connection, WS, "E0308", &live, true)?;
    mark(&connection, WS, "E0277", &format!("{live}\0suffix"), true)?;
    let actual = read(&connection, WS, &report)?;
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].evidence_id, live);
    mark(&connection, WS, "E0277", &live, true)?;
    assert!(read(&connection, WS, &report)?.is_empty());
    // A dual-role target is still not a body-bearing repair.
    report.proof_links.push(live);
    let actual = read(&connection, WS, &report)?;
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].role, "proof");
    assert_eq!(actual[0].text, None);
    Ok(())
}

#[test]
fn harmful_batch_prefix_does_not_hide_later_admitted_evidence() -> TestResult {
    let connection = db()?;
    let session = session(&connection, WS, 913)?;
    let mut report = report();
    for number in 1..=129 {
        let id = span(&connection, WS, &session, number, false)?;
        if number <= 128 {
            mark(&connection, WS, "E0277", &id, true)?;
        }
        report.helpful_repairs.push(id);
    }
    let good = report.helpful_repairs[128].clone();
    report.helpful_repairs.reverse();
    let mut hydrated = Vec::new();
    let actual = read_with(&connection, WS, &report, |ids| {
        hydrated.extend(ids.iter().map(|id| (*id).to_owned()));
        connection.get_evidence_spans_with_sessions(ids)
    })?;
    assert_eq!(hydrated, std::slice::from_ref(&good));
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].evidence_id, good);
    assert!(
        actual[0]
            .text
            .as_deref()
            .is_some_and(|text| text.contains("Repair 129"))
    );
    Ok(())
}

#[test]
fn missing_feedback_authority_fails_before_hydration_and_releases_owned_snapshot() -> TestResult {
    let connection = db()?;
    let session = session(&connection, WS, 914)?;
    let id = span(&connection, WS, &session, 1, false)?;
    let mut report = report();
    report.helpful_repairs.push(id);
    assert_eq!(read(&connection, WS, &report)?.len(), 1);
    connection.execute_raw("ALTER TABLE error_repair_links RENAME TO held_feedback_history")?;
    let mut hydration_calls = 0;
    let result = read_with(&connection, WS, &report, |ids| {
        hydration_calls += 1;
        connection.get_evidence_spans_with_sessions(ids)
    });
    assert!(result.is_err());
    assert_eq!(hydration_calls, 0);
    assert!(
        result
            .err()
            .is_some_and(|error| error.to_string().contains("positive guidance withheld"))
    );
    // No leaked read transaction after the failure.
    connection.begin_read_snapshot()?;
    connection.rollback_read_snapshot()?;
    Ok(())
}

#[test]
fn feedback_reads_leave_the_callers_transaction_and_uncommitted_marks_intact() -> TestResult {
    let connection = db()?;
    let session = session(&connection, WS, 915)?;
    let id = span(&connection, WS, &session, 1, false)?;
    let mut report = report();
    report.helpful_repairs.push(id.clone());
    let result: crate::db::Result<()> = connection.with_transaction(|| {
        record_error_repair_links(
            &connection,
            WS,
            &from_rustc(Some("E0277"), "trait bound failed"),
            &ErrorRepairLinkRecording {
                harmful_repairs: vec![id.clone()],
                ..Default::default()
            },
        )?;
        assert!(read(&connection, WS, &report)?.is_empty());
        assert_eq!(
            connection.list_error_repair_links(WS, "rustc:E0277")?.len(),
            1
        );
        connection.execute_raw("INSERT INTO missing_feedback_rollback_table VALUES (1)")?;
        Ok(())
    });
    assert!(result.is_err());
    assert!(
        connection
            .list_error_repair_links(WS, "rustc:E0277")?
            .is_empty()
    );
    assert_eq!(read(&connection, WS, &report)?.len(), 1);
    Ok(())
}
