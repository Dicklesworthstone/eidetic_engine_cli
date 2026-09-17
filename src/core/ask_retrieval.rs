//! Select the evidence that a successful ask actually exposed to the caller.
//!
//! Retrieval audit rows feed memory-debt and retention decisions. Conflict
//! reports deliberately have no top-level citations: their evidence lives in
//! `sides`. Ignoring those citations makes actively used opposing memories look
//! unused. Conversely, scanning a candidate or displaying nearest evidence on
//! abstention is not a successful retrieval and must not protect the whole
//! corpus from retention review.

use std::collections::BTreeSet;

use super::{AskCitation, AskReport};

pub(super) fn cited_memories(report: &AskReport) -> Vec<&AskCitation> {
    if report.abstained || report.extractiveness_violated {
        return Vec::new();
    }

    let direct = report
        .citations
        .iter()
        .filter(|_| !report.conflict_detected);
    let sides = report
        .sides
        .iter()
        .flatten()
        .flat_map(|side| &side.citations)
        .filter(|_| report.conflict_detected);
    let mut seen = BTreeSet::new();
    direct
        .chain(sides)
        .filter(|citation| seen.insert(citation.memory_id.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ask::{AskCandidate, AskRequest, AskSide, ask_data_json, evaluate_ask};

    fn candidate(id: &str, content: &str) -> AskCandidate {
        AskCandidate {
            memory_id: id.to_owned(),
            content: content.to_owned(),
            confidence: 1.0,
            trust_class: "human_explicit".to_owned(),
            provenance_uri: Some(format!("manual://retrieval/{id}")),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            team_provenance: None,
        }
    }

    fn report(conflict: bool) -> AskReport {
        let mut candidates = vec![
            candidate("format", "Run cargo fmt before release."),
            candidate("unrelated", "The database listens on port 5432."),
        ];
        if conflict {
            candidates.push(candidate("opposition", "Do not run cargo fmt before release."));
        }
        evaluate_ask(
            &AskRequest {
                question: "Run cargo fmt before release".to_owned(),
                ..AskRequest::default()
            },
            &candidates,
        )
    }

    fn ids(report: &AskReport) -> Vec<&str> {
        cited_memories(report)
            .into_iter()
            .map(|citation| citation.memory_id.as_str())
            .collect()
    }

    #[test]
    fn records_both_sides_of_a_conflict_not_the_scanned_corpus() {
        let report = report(true);
        assert!(!report.abstained && report.conflict_detected);
        assert!(report.citations.is_empty());
        assert_eq!(report.candidates_scanned, 3);
        assert_eq!(ids(&report), ["format", "opposition"]);
    }

    #[test]
    fn ordinary_answers_keep_their_existing_citation_metadata() {
        let report = report(false);
        assert!(!report.abstained && !report.conflict_detected);
        assert_eq!(ids(&report), ["format"]);
        let citations = cited_memories(&report);
        assert!(std::ptr::eq(citations[0], &report.citations[0]));
        assert_eq!(citations[0].trust_class, "human_explicit");
        assert_eq!(citations[0].provenance_uri.as_deref(), Some("manual://retrieval/format"));
    }

    #[test]
    fn one_memory_gets_one_read_signal_even_across_conflict_sides() {
        let mut report = report(true);
        let sides = report.sides.as_mut().unwrap();
        let duplicate = sides[0].citations[0].clone();
        sides[0].citations.push(duplicate.clone());
        sides[1].citations.push(duplicate);
        assert_eq!(ids(&report), ["format", "opposition"]);
    }

    #[test]
    fn duplicate_ordinary_citations_do_not_inflate_the_read_signal() {
        let mut report = report(false);
        report.citations.push(report.citations[0].clone());
        assert_eq!(ids(&report), ["format"]);
    }

    #[test]
    fn abstention_never_counts_retained_citations_as_successful_retrieval() {
        for conflict in [false, true] {
            let mut report = report(conflict);
            report.abstained = true;
            assert!(cited_memories(&report).is_empty());
        }
        let missed = evaluate_ask(
            &AskRequest {
                question: "orbital mechanics".to_owned(),
                ..AskRequest::default()
            },
            &[candidate("nearest", "Run cargo fmt before release.")],
        );
        assert!(missed.abstained);
        assert!(!missed.nearest_evidence.as_ref().unwrap().is_empty());
        assert!(cited_memories(&missed).is_empty());
    }

    #[test]
    fn extractiveness_failure_never_preserves_unvalidated_evidence() {
        for conflict in [false, true] {
            let mut report = report(conflict);
            report.extractiveness_violated = true;
            assert!(cited_memories(&report).is_empty());
        }
    }

    #[test]
    fn conflict_reports_do_not_record_a_hidden_top_level_answer() {
        let mut report = report(true);
        let mut hidden = report.sides.as_ref().unwrap()[0].citations[0].clone();
        hidden.memory_id = "not-presented".to_owned();
        report.citations.push(hidden);
        assert_eq!(ids(&report), ["format", "opposition"]);
    }

    #[test]
    fn ordinary_reports_do_not_record_hidden_conflict_sides() {
        let mut report = report(false);
        let mut hidden = report.citations[0].clone();
        hidden.memory_id = "not-presented".to_owned();
        report.sides = Some(vec![AskSide {
            label: "unused".to_owned(),
            answer_text: "not the emitted answer".to_owned(),
            citations: vec![hidden],
        }]);
        assert_eq!(ids(&report), ["format"]);
    }

    #[test]
    fn corrupt_evidence_does_not_generate_missing_knowledge_capture_advice() {
        let candidates = [
            candidate("same", "Run cargo fmt before release."),
            candidate("same", "Do not run cargo fmt before release."),
        ];
        let report = evaluate_ask(
            &AskRequest {
                question: "Run cargo fmt before release".to_owned(),
                ..AskRequest::default()
            },
            &candidates,
        );
        assert!(report.abstained && report.extractiveness_violated);
        assert!(cited_memories(&report).is_empty());
        assert!(ask_data_json(&report).get("queryAssist").is_none());
    }
}
