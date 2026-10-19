//! Proposed lessons from completed, source-backed tool repair episodes.
//!
//! Tool records prove the episode but never become ordinary memory sources.
//! Only the admitted assistant repair turns own a proposed memory. The existing
//! corroboration package commits to the complete source context and reconstructs
//! the episode again at validation/apply. Nothing here writes a card or memory.

use super::*;
use crate::core::cass_error_recall::session_failure_arcs;
use crate::core::incident_card::{IncidentCardSources, error_class_label};

const PRODUCER: &str = "verified_tool_repair.v1";

pub(super) fn candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    if session.workspace_id != workspace_id {
        return Vec::new();
    }
    let sources = IncidentCardSources::new(spans);
    let by_id = spans.iter().fold(BTreeMap::new(), |mut rows, span| {
        rows.entry(span.id.as_str()).or_insert(span);
        rows
    });
    let mut seen = BTreeSet::new();
    let mut proposals = Vec::new();
    for arc in session_failure_arcs(workspace_id, session, spans) {
        let Some(resolution) = &arc.resolution else {
            continue;
        };
        // Reuse the existing extractive renderer's sentence selection, source
        // screening and budget. A naked success or narration is not a lesson.
        let Some(card) = sources.draft(workspace_id, &session.id, &arc) else {
            continue;
        };
        let Some(fix) = card.input.excerpt.lines().find_map(|line| line.strip_prefix("Fix: ")) else {
            continue;
        };
        let Some(diagnostic) = arc.diagnostics.first() else {
            continue;
        };
        let source_ids = resolution.repair_ids.iter().cloned().collect::<BTreeSet<_>>();
        if source_ids.is_empty() || source_ids.iter().any(|id| {
            by_id.get(id.as_str()).is_none_or(|span| {
                !span.is_search_admitted_for_session(workspace_id, session)
                    || review_learning_text(span).is_none()
            })
        }) {
            continue;
        }
        let source_ids: Vec<_> = source_ids.into_iter().collect();
        let class = error_class_label(&diagnostic.layered_key().key);
        let command = arc.family.to_string();
        let content = format!(
            "Observed repair for {class} during `{command}`:\nFailure: {}\nRepair: {fix}\nVerification: the same command and execution context succeeded afterwards.",
            diagnostic.message_template,
        );
        let screen = crate::policy::screen_external_text_for_ingestion(&content);
        if screen.redacted || screen.instruction_like || screen.content != content
            || !matches!(screen.instruction_risk, "none" | "low")
            || content.contains("[REDACTED")
            || !crate::curate::specificity_score(&content).passes_threshold
        {
            continue;
        }
        let topic_key = review_topic_key(&content);
        if topic_key == "noise" {
            continue;
        }
        let content_hash = content_hash_for_candidate(&content);
        let candidate_id = deterministic_curate_id(&[
            workspace_id, &session.id, PRODUCER, &arc.attempt_id,
            &arc.failure_id, &resolution.proof_id, &source_ids.join(","), &content_hash,
        ]);
        if !seen.insert(candidate_id.clone()) {
            continue;
        }
        // An exact diagnostic and complete repair records, not a repeated
        // keyword or shortened card, define independent corroboration. Process
        // binding remains separately proved inside every supporting episode.
        let mut signature_parts = vec![PRODUCER.to_owned(), command];
        for diagnostic in &arc.diagnostics {
            signature_parts.push(diagnostic.layered_key().key);
            signature_parts.push(diagnostic.message_template.clone());
        }
        let reason = format!(
            "Verified tool-repair proposal from session {}: failure {} followed by admitted repair turn(s) {} and completed same-context retry {}. Tool output is proof context, not a memory source. This observed repair requires review; it is not a general guarantee.",
            session.id, arc.failure_id, source_ids.join(", "), resolution.proof_id,
        );
        proposals.push(ReviewSessionCandidate {
            candidate_id,
            candidate_type: CandidateType::CreateDerivedMemory.as_str().to_owned(),
            candidate_kind: REVIEW_CANDIDATE_KIND_PROPOSE_NEW_MEMORY.to_owned(),
            topic_key,
            target_memory_id: None,
            proposed_content: content,
            // Only the canonical scorer may replace this pending score.
            proposed_confidence: 0.0,
            source_type: CandidateSource::AgentInference.as_str().to_owned(),
            source_ids,
            reason,
            confidence: 0.0,
            content_hash,
            session_arc: None,
            persisted: false,
            corroboration: Some(ReviewCorroboration::pending(digest(signature_parts))),
        });
    }
    proposals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::incident_card::proof::tests::{Fixture, TestResult, WS};

    #[test]
    fn verified_tool_episode_proposes_admitted_repairs_without_persisting_cards() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let spans = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        let proposed = candidates(WS, &session, &spans);
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].source_ids, vec![fixture.repair.clone()]);
        assert!(proposed[0].proposed_content.contains("Observed repair for rustc:E0308"));
        assert!(proposed[0].proposed_content.contains("flushing pending writes"));
        assert_eq!(proposed[0].confidence, 0.0);
        assert_eq!(candidates(WS, &session, &spans)[0].candidate_id, proposed[0].candidate_id);
        assert_eq!(fixture.db.list_evidence_spans_for_session(&fixture.session)?, spans);
        assert!(fixture.db.list_curation_candidates(WS, None, None, None)?.is_empty());
        Ok(())
    }

    #[test]
    fn no_completed_retry_means_no_verified_repair_proposal() -> TestResult {
        let fixture = Fixture::new_public(false)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let spans = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        assert!(candidates(WS, &session, &spans).is_empty());
        assert!(candidates("foreign-workspace", &session, &spans).is_empty());
        assert!(candidates(WS, &session, &[]).is_empty());
        Ok(())
    }

    #[test]
    fn an_empty_prose_queue_still_discovers_and_revalidates_a_verified_tool_repair() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let mut proposed = Vec::new();
        score_candidates(&fixture.db, WS, &session, &mut proposed, 0.6, 10)?;
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].confidence, 0.6);
        assert!(proposed[0].corroboration.as_ref().is_some_and(ReviewCorroboration::recorded));
        validate_draft(&fixture.db, WS, &proposed[0]).map_err(|error| error.message)?;
        assert!(fixture.db.list_curation_candidates(WS, None, None, None)?.is_empty());
        Ok(())
    }
}
