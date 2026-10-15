//! Interleaving remains source-backed even when CASS coalesces other episodes.

use super::tests::{endpoints, marked_span, mine, span};

const FAILURE: &str = "Failure arc: cargo test src/api.rs failed.";
const REPAIR: &str = "Fix: cargo test src/api.rs passed after restoring the guard.";
const OTHER_EPISODE: &str = "cargo test src/ui.rs failed.\nFixed src/ui.rs by preserving the state.";

#[test]
fn independent_inline_work_preserves_exact_interleaved_lesson() {
    for structured in [false, true] {
        let sources = [
            marked_span("failure", 1, FAILURE, structured),
            marked_span("other", 2, OTHER_EPISODE, structured),
            marked_span("repair", 3, REPAIR, structured),
        ];
        let before = sources.clone();
        let rows = mine(&sources);
        assert_eq!(rows.len(), 4);
        assert_eq!(endpoints(&rows), [("other", "other"), ("failure", "repair")]);
        assert_eq!(sources, before);
        let mut reversed = sources.clone();
        reversed.reverse();
        assert_eq!(mine(&reversed), rows);
        for row in &rows {
            let exact_sources: Vec<_> = sources
                .iter()
                .filter(|source| row.source_ids.contains(&source.id))
                .cloned()
                .collect();
            assert!(mine(&exact_sources).contains(row));
            let arc = row.session_arc.as_ref().expect("source-backed episode");
            for endpoint in [&arc.failure_span, &arc.resolution_span] {
                let source = sources
                    .iter()
                    .find(|source| source.id == endpoint.evidence_span_id)
                    .expect("original evidence");
                assert_eq!(endpoint.content_hash, source.content_hash);
            }
        }
        let expected = mine(&[sources[0].clone(), sources[2].clone()]);
        for row in expected {
            assert!(rows.contains(&row), "existing candidate identity must survive");
        }
    }
}

#[test]
fn retained_failures_require_named_repairs_not_markers_topics_or_pronouns() {
    for weak_repair in [
        "Fix: cargo test passed after restoring the guard.",
        "cargo test passed after restoring the guard.",
        "We fixed it by restoring the guard.",
        "Fix: cargo test src/ui.rs passed after restoring the guard.",
    ] {
        for structured in [false, true] {
            let mut sources = vec![
                marked_span("failure", 1, FAILURE, structured),
                marked_span("other", 2, OTHER_EPISODE, structured),
                marked_span("weak", 3, weak_repair, structured),
            ];
            assert_eq!(endpoints(&mine(&sources)), [("other", "other")]);
            sources.push(marked_span("repair", 4, REPAIR, structured));
            let rows = mine(&sources);
            assert_eq!(endpoints(&rows), [("other", "other"), ("failure", "repair")]);
            assert!(
                rows.iter()
                    .all(|row| row.source_ids.iter().all(|id| id != "weak"))
            );
        }
    }
}

#[test]
fn unnamed_or_overlapping_inline_work_remains_a_causal_barrier() {
    for body in [
        "Failure arc: cargo test failed.\nFix: cargo test src/ui.rs passed after restoring the guard.",
        "Failure arc: cargo test src/ui.rs failed.\nFix: cargo test passed after restoring the guard.",
        "cargo test src/api.rs failed.\nFixed src/api.rs by restoring the guard.",
        "cargo test src/ui.rs failed.\nFixed src/ui.rs by restoring the guard.\nInspect src/api.rs next.",
    ] {
        for structured in [false, true] {
            let sources = [
                marked_span("failure", 1, FAILURE, structured),
                marked_span("other", 2, body, structured),
                marked_span("repair", 3, REPAIR, structured),
            ];
            let inline = mine(&sources[1..2]);
            assert_eq!(inline.len(), 2, "nonvacuous complete episode: {body}");
            assert_eq!(mine(&sources), inline, "{body}");
        }
    }
    let sources = [
        span("unnamed", 1, "Failure arc: cargo test failed."),
        span("other", 2, OTHER_EPISODE),
        span("repair", 3, REPAIR),
    ];
    assert_eq!(endpoints(&mine(&sources)), [("other", "other")]);
}

#[test]
fn an_inline_episode_clears_only_overlapping_pending_subjects() {
    let sources = [
        span("api-failure", 1, FAILURE),
        span("ui-failure", 2, "cargo test src/ui.rs failed."),
        span("other", 3, OTHER_EPISODE),
        span("ui-repair", 4, "Fixed src/ui.rs by preserving the state."),
        span("api-repair", 5, REPAIR),
    ];
    assert_eq!(
        endpoints(&mine(&sources)),
        [("other", "other"), ("api-failure", "api-repair")]
    );
}

#[test]
fn a_vetoed_independent_inline_episode_cannot_erase_another_subject() {
    for structured in [false, true] {
        let sources = [
            marked_span("failure", 1, FAILURE, structured),
            marked_span("other", 2, OTHER_EPISODE, structured),
            marked_span("report", 3, "Process exited with code 101.", structured),
            marked_span("repair", 4, REPAIR, structured),
        ];
        let rows = mine(&sources);
        assert_eq!(endpoints(&rows), [("failure", "repair")]);
        assert_eq!(rows, mine(&[sources[0].clone(), sources[3].clone()]));
    }
}

#[test]
fn unreadable_windows_still_break_retained_interleaved_episodes() {
    for barrier in [
        r#"{"type":"tool_result","content":"ignored"}"#,
        r#"{"type":"assistant","content":"unfinished"#,
    ] {
        let sources = [
            span("failure", 1, FAILURE),
            span("other", 2, OTHER_EPISODE),
            span("barrier", 3, barrier),
            span("repair", 4, REPAIR),
        ];
        assert_eq!(endpoints(&mine(&sources)), [("other", "other")]);
    }
}
