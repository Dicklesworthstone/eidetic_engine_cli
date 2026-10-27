//! Cost regressions for the real sequence path, not just its lookahead iterator.

use super::tests::{endpoints, marked_span, mine, span};
use super::{ReviewSessionCandidate, append_pairs};
use std::collections::BTreeSet;

const FAILURE: &str = "cargo test src/api.rs failed.";
const REPAIR: &str = "Fixed src/api.rs by restoring the guard.";

fn completed_pair() -> Vec<ReviewSessionCandidate> {
    let rows = mine(&[span("combined", 1, &format!("{FAILURE}\n{REPAIR}"))]);
    assert_eq!(rows.len(), 2, "nonvacuous reciprocal-pair fixture");
    rows
}

#[test]
fn sequence_mines_structured_episodes_with_one_projection_per_window() {
    let sources: Vec<_> = (0..512_u32)
        .map(|index| {
            let body = format!(
                "cargo test src/cost-{index}.rs failed.\nFixed src/cost-{index}.rs by restoring the guard."
            );
            marked_span(&format!("source-{index}"), index + 1, &body, true)
        })
        .collect();
    let original = sources.clone();
    let before = crate::cass::transcript::projection_calls_for_test();
    let rows = mine(&sources);
    let after = crate::cass::transcript::projection_calls_for_test();
    assert_eq!(after - before, u64::try_from(sources.len()).unwrap());
    assert_eq!(rows.len(), sources.len() * 2);
    assert_eq!(sources, original);
    for (pair, source) in rows.as_chunks::<2>().0.iter().zip(&sources) {
        for row in pair {
            assert_eq!(row.source_ids, std::slice::from_ref(&source.id));
            let arc = row.session_arc.as_ref().expect("original provenance");
            assert_eq!(arc.failure_span.content_hash, source.content_hash);
            assert_eq!(arc.resolution_span.content_hash, source.content_hash);
        }
    }
}

#[test]
fn sequence_decodes_reports_and_denied_records_once_but_skips_foreign_sources() {
    let mut foreign_workspace = marked_span("foreign-workspace", 7, FAILURE, true);
    foreign_workspace.workspace_id = "another-workspace".into();
    let mut foreign_session = marked_span("foreign-session", 8, REPAIR, true);
    foreign_session.session_id = "another-session".into();
    let sources = [
        marked_span("failure", 1, FAILURE, true),
        marked_span("repair", 2, REPAIR, true),
        marked_span("report", 3, "21 passed; 0 failed.", true),
        span("tool", 4, r#"{"type":"tool_result","content":"ignored"}"#),
        span(
            "malformed",
            5,
            r#"{"type":"assistant","content":"unfinished"#,
        ),
        marked_span(
            "combined",
            6,
            "cargo test src/ui.rs failed.\nFixed src/ui.rs by restoring the guard.",
            true,
        ),
        foreign_workspace,
        foreign_session,
    ];
    let original = sources.clone();
    let before = crate::cass::transcript::projection_calls_for_test();
    let rows = mine(&sources);
    let after = crate::cass::transcript::projection_calls_for_test();
    assert_eq!(
        after - before,
        6,
        "foreign spans are filtered before projection"
    );
    assert_eq!(
        endpoints(&rows),
        [("failure", "repair"), ("combined", "combined")]
    );
    assert_eq!(sources, original);
}

#[test]
fn publishing_owned_pairs_moves_content_and_provenance_allocations() {
    let pair = completed_pair();
    let allocations: Vec<_> = pair
        .iter()
        .map(|row| {
            let arc = row.session_arc.as_ref().unwrap();
            (
                row.proposed_content.as_ptr(),
                row.source_ids.as_ptr(),
                arc.failure_span.excerpt.as_ptr(),
                arc.resolution_span.excerpt.as_ptr(),
            )
        })
        .collect();
    let expected = pair.clone();
    let mut output = Vec::new();
    let mut emitted = BTreeSet::new();
    append_pairs(&mut output, &mut emitted, pair);
    assert_eq!(output, expected);
    assert_eq!(emitted.len(), 2);
    for (row, allocation) in output.iter().zip(allocations) {
        let arc = row.session_arc.as_ref().unwrap();
        assert_eq!(row.proposed_content.as_ptr(), allocation.0);
        assert_eq!(row.source_ids.as_ptr(), allocation.1);
        assert_eq!(arc.failure_span.excerpt.as_ptr(), allocation.2);
        assert_eq!(arc.resolution_span.excerpt.as_ptr(), allocation.3);
    }
}

#[test]
fn a_duplicate_endpoint_drops_both_members_without_reserving_the_peer() {
    let pair = completed_pair();
    for endpoint in 0..2 {
        let mut emitted = BTreeSet::from([pair[endpoint].candidate_id.clone()]);
        let before = emitted.clone();
        let mut output = Vec::new();
        append_pairs(&mut output, &mut emitted, pair.clone());
        assert!(output.is_empty());
        assert_eq!(emitted, before);
    }
}

#[test]
fn odd_tails_are_discarded_without_poisoning_deduplication() {
    let pair = completed_pair();
    let mut orphan = pair[0].clone();
    orphan.candidate_id = "unpaired-trailing-candidate".into();
    let mut input = pair.clone();
    input.push(orphan.clone());
    let mut output = Vec::new();
    let mut emitted = BTreeSet::new();
    append_pairs(&mut output, &mut emitted, input);
    assert_eq!(output, pair);
    assert!(!emitted.contains(&orphan.candidate_id));
    append_pairs(&mut output, &mut emitted, pair.clone());
    assert_eq!(output, pair, "duplicate pairs are still all-or-nothing");
    for incomplete in [Vec::new(), vec![orphan]] {
        let mut output = Vec::new();
        let mut emitted = BTreeSet::new();
        append_pairs(&mut output, &mut emitted, incomplete);
        assert!(output.is_empty());
        assert!(emitted.is_empty());
    }
}
