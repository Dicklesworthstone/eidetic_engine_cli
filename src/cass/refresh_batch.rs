//! Bounded writes for the growing tail of an already imported CASS session.
//!
//! The refresh owner has already checked the complete source history and owns
//! the writer-fenced transaction. A failure here must propagate to that owner:
//! evidence, admission verdicts, audits, checkpoint and publication job either
//! all commit, or none of them do.

use crate::db::{
    CreateEvidenceSpanInput, DbConnection, DbError, EvidenceAdmissionTally, StoredSession,
};
use sqlmodel_core::Value;

use super::{
    CassViewSpanForImport, cass_redaction_audit_input, evidence_input, refusal,
    stable_cass_redaction_audit_id, stable_evidence_id,
};

// Bound temporary copies independently of the complete retained transcript.
// The database writer further splits each batch at its SQL parameter limit.
const REFRESH_WRITE_BATCH_ROWS: usize = 128;
const REFRESH_PREPARED_HEAP_BYTES: usize = 256 * 1024;

type PreparedRow = (String, CreateEvidenceSpanInput);

pub(super) fn insert_additions(
    connection: &DbConnection,
    workspace_id: &str,
    session: &StoredSession,
    additions: &[&CassViewSpanForImport],
) -> Result<(Vec<u32>, EvidenceAdmissionTally), DbError> {
    let mut added_lines = Vec::with_capacity(additions.len());
    let mut admission = EvidenceAdmissionTally::default();
    for batch in additions.chunks(REFRESH_WRITE_BATCH_ROWS) {
        let identities = batch
            .iter()
            .map(|span| stable_evidence_id(&session.id, &span.cass_span_id))
            .collect::<Vec<_>>();
        // Only existence matters here. Do not hydrate another session's
        // private transcript or issue a separate query for every new row.
        // Deliberately do NOT filter by workspace/session: evidence IDs occupy
        // a global namespace, including malformed or foreign-scope rows.
        let parameters = identities
            .iter()
            .map(|id| Value::Text(id.clone()))
            .collect::<Vec<_>>();
        let placeholders = std::iter::repeat_n("?", parameters.len())
            .collect::<Vec<_>>()
            .join(", ");
        let occupied = connection.query(
            &format!("SELECT 1 FROM evidence_spans WHERE id IN ({placeholders}) LIMIT 1"),
            &parameters,
        )?;
        if !occupied.is_empty() {
            return Err(refusal("cass_refresh_evidence_identity_exists"));
        }
        // Prepare owned inputs lazily, after the global identity probe. A row
        // count alone does not bound their excerpts, metadata or spare capacity.
        let rows = batch
            .iter()
            .zip(&identities)
            .map(|(span, id)| (id.clone(), evidence_input(workspace_id, &session.id, span)));
        admission.merge(&write_prepared_batches(
            rows,
            REFRESH_PREPARED_HEAP_BYTES,
            |rows| connection.insert_evidence_spans_in_session(rows, session),
        )?);
        for (span, evidence_id) in batch.iter().zip(&identities) {
            if span.redacted {
                connection.insert_audit(
                    &stable_cass_redaction_audit_id(evidence_id),
                    &cass_redaction_audit_input(workspace_id, &session.id, evidence_id, span),
                )?;
            }
            added_lines.push(span.end_line);
        }
    }
    Ok((added_lines, admission))
}

/// Account all owned string/vector capacity in a prepared input. The fixed
/// outer row buffer and the bounded identity probe are separate allocations.
/// This bounds preparation, not the captured transcript, scanner, DB or RSS.
fn prepared_heap_bytes((id, input): &PreparedRow) -> usize {
    let mut bytes = [
        id,
        &input.workspace_id,
        &input.session_id,
        &input.cass_span_id,
        &input.span_kind,
        &input.excerpt,
        &input.content_hash,
    ]
    .iter()
    .fold(0_usize, |total, text| total.saturating_add(text.capacity()));
    for text in [
        input.memory_id.as_ref(),
        input.role.as_ref(),
        input.metadata_json.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        bytes = bytes.saturating_add(text.capacity());
    }
    bytes = bytes.saturating_add(
        input
            .inherited_redaction_classes
            .capacity()
            .saturating_mul(std::mem::size_of::<String>()),
    );
    input
        .inherited_redaction_classes
        .iter()
        .fold(bytes, |total, class| total.saturating_add(class.capacity()))
}

fn exceeds_heap_limit(retained: usize, additional: usize, limit: usize) -> bool {
    retained
        .checked_add(additional)
        .is_none_or(|total| total > limit)
}

/// Keep at most one byte/row-bounded group plus one lookahead input. A row
/// larger than the budget is written alone, unchanged: never truncate evidence
/// to fit a batch. Full and oversized groups flush before preparing the next
/// row. The first writer error stops iteration and propagates to the refresh
/// transaction; no group commits independently or bypasses normal screening.
fn write_prepared_batches(
    rows: impl IntoIterator<Item = PreparedRow>,
    max_heap_bytes: usize,
    mut write: impl FnMut(&[PreparedRow]) -> Result<EvidenceAdmissionTally, DbError>,
) -> Result<EvidenceAdmissionTally, DbError> {
    let mut batch = Vec::with_capacity(REFRESH_WRITE_BATCH_ROWS);
    let mut heap_bytes = 0_usize;
    let mut admission = EvidenceAdmissionTally::default();
    for row in rows {
        let row_bytes = prepared_heap_bytes(&row);
        if !batch.is_empty() && exceeds_heap_limit(heap_bytes, row_bytes, max_heap_bytes) {
            admission.merge(&write(&batch)?);
            batch.clear();
            heap_bytes = 0;
        }
        heap_bytes = heap_bytes.saturating_add(row_bytes);
        batch.push(row);
        if batch.len() == REFRESH_WRITE_BATCH_ROWS || heap_bytes >= max_heap_bytes {
            admission.merge(&write(&batch)?);
            batch.clear();
            heap_bytes = 0;
        }
    }
    if !batch.is_empty() {
        admission.merge(&write(&batch)?);
    }
    Ok(admission)
}

#[cfg(test)]
#[path = "refresh_batch_regressions.rs"]
mod batch_regressions;

#[cfg(test)]
mod preparation_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::db::EvidenceProducerKind;
    use std::cell::Cell;

    fn row(index: usize, excerpt: String) -> PreparedRow {
        (
            format!("ev_test_{index:04}"),
            CreateEvidenceSpanInput {
                workspace_id: "workspace".to_owned(),
                session_id: "session".to_owned(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("source:{index:04}"),
                span_kind: "message".to_owned(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                content_hash: "unchanged-source-hash".to_owned(),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )
    }

    #[test]
    fn byte_groups_preserve_order_content_and_admission_tallies() {
        let row_bytes = prepared_heap_bytes(&row(0, "資料".to_owned()));
        let limit = row_bytes * 2;
        let mut observed = Vec::new();
        let mut sizes = Vec::new();
        let actual = write_prepared_batches(
            (0..5).map(|index| row(index, "資料".to_owned())),
            limit,
            |batch| {
                assert!(batch.iter().map(prepared_heap_bytes).sum::<usize>() <= limit);
                sizes.push(batch.len());
                let mut tally = EvidenceAdmissionTally::default();
                for (id, input) in batch {
                    assert_eq!(input.excerpt, "資料");
                    assert_eq!(input.content_hash, "unchanged-source-hash");
                    observed.push(id.clone());
                    tally.record(if id == "ev_test_0003" {
                        Some("fixture_denied")
                    } else {
                        None
                    });
                }
                Ok(tally)
            },
        )
        .unwrap();
        assert_eq!(sizes, [2, 2, 1]);
        assert_eq!(
            observed,
            (0..5)
                .map(|index| format!("ev_test_{index:04}"))
                .collect::<Vec<_>>()
        );
        let mut expected = EvidenceAdmissionTally::default();
        for index in 0..5 {
            expected.record(if index == 3 {
                Some("fixture_denied")
            } else {
                None
            });
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn row_cap_is_independent_of_the_heap_budget() {
        let mut sizes = Vec::new();
        write_prepared_batches(
            (0..260).map(|index| row(index, "small".to_owned())),
            usize::MAX,
            |batch| {
                sizes.push(batch.len());
                Ok(EvidenceAdmissionTally::default())
            },
        )
        .unwrap();
        assert_eq!(sizes, [128, 128, 4]);
    }

    #[test]
    fn all_optional_and_spare_capacities_are_charged() {
        let mut prepared = row(0, String::with_capacity(4096));
        let before = prepared_heap_bytes(&prepared);
        prepared.1.memory_id = Some(String::with_capacity(123));
        prepared.1.metadata_json = Some(String::with_capacity(456));
        prepared.1.inherited_redaction_classes = Vec::with_capacity(8);
        prepared
            .1
            .inherited_redaction_classes
            .push(String::with_capacity(789));
        let expected_extra = prepared.1.memory_id.as_ref().unwrap().capacity()
            + prepared.1.metadata_json.as_ref().unwrap().capacity()
            + prepared.1.inherited_redaction_classes.capacity() * std::mem::size_of::<String>()
            + prepared.1.inherited_redaction_classes[0].capacity();
        assert_eq!(prepared_heap_bytes(&prepared), before + expected_extra);
        assert!(before >= 4096);
        assert!(prepared.1.excerpt.is_empty());
    }

    #[test]
    fn oversized_singletons_flush_before_preparing_the_next_row() {
        let prepared = Cell::new(0);
        let written = Cell::new(0);
        let text = "資料".repeat(128);
        for limit in [0, 1] {
            prepared.set(0);
            written.set(0);
            write_prepared_batches(
                (0..3).map(|index| {
                    prepared.set(prepared.get() + 1);
                    row(index, text.clone())
                }),
                limit,
                |batch| {
                    assert_eq!(batch.len(), 1);
                    assert_eq!(batch[0].1.excerpt, text);
                    written.set(written.get() + 1);
                    assert_eq!(prepared.get(), written.get());
                    Ok(EvidenceAdmissionTally::default())
                },
            )
            .unwrap();
            assert_eq!(written.get(), 3);
        }
    }

    #[test]
    fn writer_failure_never_prepares_the_unvisited_tail() {
        let row_bytes = prepared_heap_bytes(&row(0, "small".to_owned()));
        for (limit, expected_prepared) in
            [(0, 1), (row_bytes * 2 + 1, 3), (usize::MAX, 128)]
        {
            let prepared = Cell::new(0);
            let mut calls = 0;
            let error = write_prepared_batches(
                (0..260).map(|index| {
                    prepared.set(prepared.get() + 1);
                    row(index, "small".to_owned())
                }),
                limit,
                |_| {
                    calls += 1;
                    Err(refusal("fixture_writer_failure"))
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("fixture_writer_failure"));
            assert_eq!(calls, 1);
            assert_eq!(prepared.get(), expected_prepared);
        }
    }

    #[test]
    fn empty_input_and_overflow_boundaries_are_explicit() {
        let tally = write_prepared_batches(std::iter::empty(), 0, |_| {
            panic!("empty input must not call the writer")
        })
        .unwrap();
        assert_eq!(tally, EvidenceAdmissionTally::default());
        assert!(!exceeds_heap_limit(10, 20, 30));
        assert!(exceeds_heap_limit(10, 21, 30));
        assert!(!exceeds_heap_limit(usize::MAX, 0, usize::MAX));
        assert!(exceeds_heap_limit(usize::MAX, 1, usize::MAX));
        assert!(exceeds_heap_limit(usize::MAX - 1, 2, usize::MAX));
    }
}
