//! Batch hydration for complete error-recall evidence reports.
//!
//! The report may enumerate a long repair history, but each read owns at most
//! 128 hydrated spans and their live sessions. Canonical target identities are
//! deduplicated before I/O. Repairs cross normal search admission; quarantined
//! proof bodies are never rendered. Output is still complete, not silently
//! truncated to the interactive pack limit.

use std::collections::BTreeSet;

use crate::db::{DbConnection, HydratedEvidenceSpan, Result};
use crate::models::EvidenceId;

use super::{ErrorRecallReport, RECALLED_REPAIR_TEXT_CHARS, RecalledRepairEvidence};

const HYDRATION_BATCH: usize = 128;

pub(super) fn read(
    connection: &DbConnection,
    workspace_id: &str,
    report: &ErrorRecallReport,
) -> Result<Vec<RecalledRepairEvidence>> {
    read_with(workspace_id, report, |ids| {
        connection.get_evidence_spans_with_sessions(ids)
    })
}

fn canonical_evidence_targets(targets: &[String]) -> BTreeSet<&str> {
    targets
        .iter()
        .filter(|target| {
            target
                .parse::<EvidenceId>()
                .is_ok_and(|id| id.to_string() == target.as_str())
        })
        .map(String::as_str)
        .collect()
}

fn read_with(
    workspace_id: &str,
    report: &ErrorRecallReport,
    mut hydrate: impl FnMut(&[&str]) -> Result<Vec<HydratedEvidenceSpan>>,
) -> Result<Vec<RecalledRepairEvidence>> {
    let repairs = canonical_evidence_targets(&report.helpful_repairs);
    let proofs = canonical_evidence_targets(&report.proof_links);
    let mut ids = repairs.union(&proofs).copied();
    let mut cards = Vec::new();
    let mut turns = Vec::new();
    let mut proof_locators = Vec::new();
    loop {
        let batch: Vec<_> = ids.by_ref().take(HYDRATION_BATCH).collect();
        if batch.is_empty() {
            break;
        }
        for row in hydrate(&batch)? {
            let span = &row.span;
            if repairs.contains(span.id.as_str()) && row.is_search_admitted(workspace_id) {
                let is_card = span.is_derived_incident_card();
                let text = span.reader_text();
                let recalled = RecalledRepairEvidence {
                    evidence_id: span.id.clone(),
                    role: if is_card { "incident_card" } else { "repair" },
                    provenance_uri: span.canonical_provenance_uri(),
                    text: Some(if is_card {
                        text.into_owned()
                    } else {
                        text.chars().take(RECALLED_REPAIR_TEXT_CHARS).collect()
                    }),
                };
                if is_card {
                    cards.push(recalled);
                } else {
                    turns.push(recalled);
                }
            }
            // Proof spans can legitimately be quarantined from search. Keep
            // their locator-only contract, but require a live session in the
            // same workspace instead of trusting the span's workspace alone.
            if proofs.contains(span.id.as_str())
                && span.workspace_id == workspace_id
                && row.session.as_ref().is_some_and(|session| {
                    session.id == span.session_id && session.workspace_id == workspace_id
                })
            {
                proof_locators.push(RecalledRepairEvidence {
                    evidence_id: span.id.clone(),
                    role: "proof",
                    provenance_uri: span.canonical_provenance_uri(),
                    text: None,
                });
            }
        }
    }
    // Do not depend on SQL row order, report insertion order or batch edges.
    for group in [&mut cards, &mut turns, &mut proof_locators] {
        group.sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));
        group.dedup_by(|left, right| left.evidence_id == right.evidence_id);
    }
    cards.append(&mut turns);
    cards.append(&mut proof_locators);
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::super::ErrorRecallOutcome;
    use super::*;
    use crate::core::error_recall::{ErrorFingerprint, from_rustc};
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
    };

    const WS: &str = "wsp_01234567890123456789012345";
    const OTHER_WS: &str = "wsp_01234567890123456789012346";
    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn db() -> TestResult<DbConnection> {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        for (workspace, path) in [
            (WS, "/tmp/batch-error-recall"),
            (OTHER_WS, "/tmp/other-batch-error-recall"),
        ] {
            connection.insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: path.to_owned(),
                    name: None,
                },
            )?;
        }
        Ok(connection)
    }

    fn session(connection: &DbConnection, workspace: &str, number: u128) -> TestResult<String> {
        let id = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        connection.insert_session(
            &id,
            &CreateSessionInput {
                workspace_id: workspace.to_owned(),
                cass_session_id: format!("/sessions/recall-batch-{number}.jsonl"),
                source_path: None,
                agent_name: Some("claude_code".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 0,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(&number.to_le_bytes()).to_hex()),
                metadata_json: None,
            },
        )?;
        Ok(id)
    }

    fn span(
        connection: &DbConnection,
        workspace: &str,
        session: &str,
        number: u32,
        card: bool,
    ) -> TestResult<String> {
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(u128::from(number))).to_string();
        let excerpt = if card {
            format!(
                "{}1-5): `cargo test` failed, then passed after a fix.\nSymptom: trait bound not satisfied.\nFix: Updated the cache trait implementation.\nVerified: `cargo test` succeeded afterwards.",
                crate::core::incident_card::INCIDENT_CARD_PREFIX
            )
        } else {
            serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[
                {"type":"text","text":format!("Repair {number} corrected the cache implementation. {}", "🦀".repeat(450))}
            ]}})
            .to_string()
        };
        connection.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: workspace.to_owned(),
                session_id: session.to_owned(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{session}:{number}"),
                span_kind: if card { "summary" } else { "message" }.to_owned(),
                start_line: number,
                end_line: number,
                start_byte: None,
                end_byte: None,
                role: (!card).then(|| "assistant".to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        Ok(id)
    }

    fn report() -> ErrorRecallReport {
        let fingerprint =
            ErrorFingerprint::from_canonical(&from_rustc(Some("E0277"), "trait bound failed"));
        ErrorRecallReport::from_outcome(
            &fingerprint,
            &ErrorRecallOutcome {
                fingerprint_key: fingerprint.layered_key().key,
                layer: "canonical_code",
                matched: None,
            },
        )
    }

    #[test]
    fn real_hydration_batches_across_128_ids_without_dropping_or_reordering_evidence() -> TestResult {
        let connection = db()?;
        let session = session(&connection, WS, 900)?;
        let mut report = report();
        for number in 1..=130 {
            report
                .helpful_repairs
                .push(span(&connection, WS, &session, number, number == 130)?);
        }
        let card = report.helpful_repairs[129].clone();
        let first = report.helpful_repairs[0].clone();
        report.helpful_repairs.reverse();
        report.helpful_repairs.push(first.clone());
        report.proof_links = vec![first.clone(), first.clone()];
        let mut batches = Vec::new();
        let actual = read_with(WS, &report, |ids| {
            batches.push(ids.len());
            connection.get_evidence_spans_with_sessions(ids)
        })?;
        assert_eq!(batches, [128, 2]);
        assert_eq!(actual.len(), 131);
        assert_eq!(actual[0].evidence_id, card);
        assert_eq!(actual[0].role, "incident_card");
        assert_eq!(actual[1].evidence_id, first);
        assert_eq!(actual[130].role, "proof");
        assert_eq!(actual[130].evidence_id, first);
        assert_eq!(actual[130].text, None);
        for item in actual.iter().filter(|item| item.role == "repair") {
            let original = connection
                .get_search_admitted_evidence_span(&item.evidence_id, WS)?
                .ok_or("missing admitted reference")?;
            assert_eq!(item.provenance_uri, original.canonical_provenance_uri());
            let expected: String = original
                .reader_text()
                .chars()
                .take(RECALLED_REPAIR_TEXT_CHARS)
                .collect();
            assert_eq!(item.text.as_deref(), Some(expected.as_str()));
        }
        assert_eq!(read(&connection, WS, &report)?, actual);
        Ok(())
    }

    #[test]
    fn invalid_and_aliased_target_ids_never_reach_hydration() -> TestResult {
        let connection = db()?;
        let session = session(&connection, WS, 901)?;
        let valid = span(&connection, WS, &session, 1, false)?;
        let mut report = report();
        report.helpful_repairs = vec![
            format!("{valid}\0suffix"),
            format!(" {valid}"),
            "ev_invalid".to_owned(),
            "mem_not_evidence".to_owned(),
        ];
        let mut calls = 0;
        let empty = read_with(WS, &report, |ids| {
            calls += 1;
            connection.get_evidence_spans_with_sessions(ids)
        })?;
        assert!(empty.is_empty());
        assert_eq!(calls, 0);
        report.helpful_repairs.push(valid.clone());
        let actual = read(&connection, WS, &report)?;
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].evidence_id, valid);
        Ok(())
    }

    #[test]
    fn stale_and_foreign_repairs_are_withheld_while_quarantined_proofs_stay_locator_only()
    -> TestResult {
        let connection = db()?;
        let local = session(&connection, WS, 902)?;
        let foreign_session = session(&connection, OTHER_WS, 903)?;
        let live = span(&connection, WS, &local, 1, false)?;
        let changed = span(&connection, WS, &local, 2, false)?;
        let quarantined = span(&connection, WS, &local, 3, false)?;
        let foreign = span(&connection, OTHER_WS, &foreign_session, 4, false)?;
        connection.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'UNSCREENED_REPAIR' WHERE id = '{changed}'"
        ))?;
        connection.execute_raw(&format!(
            "UPDATE evidence_spans SET search_eligibility = 'denied' WHERE id = '{quarantined}'"
        ))?;
        let mut report = report();
        report.helpful_repairs = vec![live.clone(), changed, quarantined.clone(), foreign.clone()];
        report.proof_links = vec![quarantined.clone(), foreign];
        let actual = read(&connection, WS, &report)?;
        assert_eq!(actual.len(), 2);
        assert_eq!(actual[0].evidence_id, live);
        assert_eq!(actual[0].role, "repair");
        assert_eq!(actual[1].evidence_id, quarantined);
        assert_eq!(actual[1].role, "proof");
        assert_eq!(actual[1].text, None);
        // A stale span-to-session workspace binding must not expose a proof
        // locator even when the span still claims the requested workspace.
        connection.execute_raw(&format!(
            "UPDATE sessions SET workspace_id = '{OTHER_WS}' WHERE id = '{local}'"
        ))?;
        assert!(read(&connection, WS, &report)?.is_empty());
        Ok(())
    }
}
