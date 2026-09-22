//! Mine every disjoint conversation episode, not just the first hit per topic.
//!
//! Projection is shared with inline learning: JSON envelope fields, tool
//! results and metadata cannot masquerade as failures or repairs. Selection
//! never changes an evidence ID, hash, source locator, or durable excerpt.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use super::super::{
    ReviewSessionCandidate, StoredEvidenceSpan, StoredSession, build_session_arc_candidate_pair,
    review_topic_key, session_arc_failure_signal, session_arc_span_order,
};
use super::{inline_candidates, resolution_signal, text};

struct PendingFailure<'a> {
    source: &'a StoredEvidenceSpan,
    message: Cow<'a, str>,
    explicitly_marked: bool,
}

/// Retain complete reciprocal pairs. A repair consumes one preceding failure;
/// neither endpoint can be reused for another cross-window episode. Independent
/// topics can interleave, while another failure on the same topic replaces the
/// unresolved observation. Explicit markers can bridge different topic keys.
/// This remains conservative proposal generation, never automatic application.
pub(super) fn candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    if session.workspace_id != workspace_id {
        return Vec::new();
    }
    let mut ordered: Vec<_> = spans
        .iter()
        .filter(|span| span.workspace_id == workspace_id && span.session_id == session.id)
        .collect();
    ordered.sort_by(|left, right| session_arc_span_order(left, right));

    let mut output = Vec::new();
    let mut emitted = BTreeSet::new();
    let mut pending = BTreeMap::<String, PendingFailure<'_>>::new();
    for span in ordered {
        let Some(message) = text::message_text(&span.excerpt) else {
            // Do not establish a causal link across a record whose conversation
            // body cannot be safely interpreted. In particular, rejecting an
            // escaped instruction must not splice its neighboring text together.
            pending.clear();
            continue;
        };
        let inline = inline_candidates(workspace_id, session, std::slice::from_ref(span));
        if !inline.is_empty() {
            append_pairs(&mut output, &mut emitted, inline);
            // This window already owns a complete episode (possibly several).
            // Do not reuse it as a cross-window endpoint or bridge an unresolved
            // older failure across its independent failure/repair sequence.
            pending.clear();
            continue;
        }
        let topic = review_topic_key(message.as_ref());
        if resolution_signal(message.as_ref()) {
            let explicit_repair = message.to_ascii_lowercase().contains("fix:");
            let same_topic = pending
                .get(&topic)
                .filter(|failure| precedes(failure.source, span))
                .map(|_| topic.clone());
            let key = same_topic.or_else(|| {
                if !explicit_repair {
                    return None;
                }
                // Prefer the nearest still-unresolved explicit declaration,
                // rather than reusing the session's first failure forever.
                pending
                    .iter()
                    .filter(|(_, failure)| {
                        failure.explicitly_marked && precedes(failure.source, span)
                    })
                    .max_by(|(_, left), (_, right)| {
                        session_arc_span_order(left.source, right.source)
                    })
                    .map(|(key, _)| key.clone())
            });
            if let Some(key) = key
                && let Some(failure) = pending.remove(&key)
            {
                let pair_topic = if key != "noise" && key == topic {
                    key
                } else {
                    review_topic_key(&format!("{} {}", failure.message, message))
                };
                // Only the display projection changes. Provenance still binds
                // the complete original stored records, including JSON escapes.
                let mut failure_source = failure.source.clone();
                failure_source.excerpt = failure.message.into_owned();
                let mut repair_source = span.clone();
                repair_source.excerpt = message.into_owned();
                append_pairs(
                    &mut output,
                    &mut emitted,
                    build_session_arc_candidate_pair(
                        workspace_id,
                        session,
                        &pair_topic,
                        &failure_source,
                        &repair_source,
                    ),
                );
            }
            // A success mentioning an "error" is not a new failed attempt.
            continue;
        }
        if topic != "noise" && session_arc_failure_signal(message.as_ref()) {
            let explicitly_marked = message.to_ascii_lowercase().contains("failure arc:");
            pending.insert(
                topic,
                PendingFailure {
                    source: span,
                    message,
                    explicitly_marked,
                },
            );
        }
    }
    output
}

fn precedes(failure: &StoredEvidenceSpan, repair: &StoredEvidenceSpan) -> bool {
    failure.id != repair.id
        && failure.start_line <= failure.end_line
        && repair.start_line <= repair.end_line
        && failure.end_line < repair.start_line
}

fn append_pairs(
    output: &mut Vec<ReviewSessionCandidate>,
    emitted: &mut BTreeSet<String>,
    candidates: Vec<ReviewSessionCandidate>,
) {
    // Builders emit anti-pattern/rule pairs in order. Deduplicate both or
    // neither, preserving content-bound IDs and reciprocal metadata.
    for pair in candidates.chunks_exact(2) {
        if pair
            .iter()
            .any(|candidate| emitted.contains(&candidate.candidate_id))
        {
            continue;
        }
        for candidate in pair {
            emitted.insert(candidate.candidate_id.clone());
            output.push(candidate.clone());
        }
    }
}
