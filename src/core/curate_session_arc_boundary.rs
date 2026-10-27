//! Carry unfinished work across windows that also contain completed lessons.
//!
//! A CASS window is a storage boundary, not an episode boundary. Reuse the
//! existing projected clauses and outcome policy; never decode a body twice
//! or invent transcript positions for a clause within the original span.

use std::borrow::Cow;

use super::{
    PendingFailure, PendingFailures, ReviewSessionCandidate, StoredEvidenceSpan, StoredSession,
    build_session_arc_candidate_pair, failure_signal, matching_failure, resolution_signal,
    resource_keys, review_topic_key,
};

/// Retain named failed observations after the last completed/positive outcome
/// in a mixed window. Earlier clauses remain the inline detector's business;
/// do not reinterpret ambiguous interior work as another causal episode.
///
/// An unnamed failure is a barrier, not an alias for a named subject. The
/// eventual repair must explicitly name the retained resource. This prevents
/// topic/marker/pronoun fallbacks from jumping across the completed work.
/// Caller first applies the existing whole-window barrier to older failures.
pub(super) fn retain_tail<'a>(
    pending: &mut PendingFailures<'a>,
    span: &'a StoredEvidenceSpan,
    message: &str,
) {
    let clauses: Vec<_> = super::super::clauses::split(message)
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
        .collect();
    let mut tail = Vec::new();
    for clause in clauses.into_iter().rev() {
        // Positive results are not failures merely because they mention an
        // error. Bare contradictory reports are already grouped by split().
        if resolution_signal(clause) {
            break;
        }
        if !failure_signal(clause) {
            continue;
        }
        let topic = review_topic_key(clause);
        let resources = resource_keys(clause);
        if topic == "noise" || resources.is_empty() {
            break;
        }
        tail.push(((topic, resources), clause));
    }
    // Use source order so the latest unresolved failure of an identical
    // subject replaces the earlier one, exactly as on the ordinary path.
    for (key, clause) in tail.into_iter().rev() {
        pending.insert(
            key,
            PendingFailure {
                source: span,
                message: Cow::Owned(clause.to_owned()),
                explicitly_marked: clause.to_ascii_lowercase().contains("failure arc:"),
                requires_resource_match: true,
            },
        );
    }
}

/// A window can straddle two episodes without containing a complete inline
/// pair: a repair at its opening followed by the next failure. Leave ordinary
/// single-outcome windows untouched so existing candidate excerpts/IDs remain
/// stable. The shared classifier, including grouped report vetoes, decides
/// which clauses are outcomes; mere keyword order is not sufficient.
pub(super) fn straddles_episodes(message: &str) -> bool {
    let mut has_repair = false;
    for clause in super::super::clauses::split(message)
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
    {
        if resolution_signal(clause) {
            has_repair = true;
        } else if failure_signal(clause) {
            return has_repair;
        }
    }
    false
}

/// Complete preceding, explicitly named failures with repair clauses before
/// the first failed observation in this mixed window. Do not search through
/// an interior failure to find a convenient repair for older work. The same
/// temporal/ambiguity checks as ordinary learning remain authoritative.
/// Caller refuses the entire positive path when a following report vetoes it.
pub(super) fn prefix_pairs(
    workspace_id: &str,
    session: &StoredSession,
    pending: &mut PendingFailures<'_>,
    span: &StoredEvidenceSpan,
    message: &str,
) -> Vec<ReviewSessionCandidate> {
    let mut output = Vec::new();
    for clause in super::super::clauses::split(message)
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
    {
        // split() has already attached contradictory outcome trailers to
        // their observation. Such a clause cannot enter the repair branch.
        if !resolution_signal(clause) {
            if failure_signal(clause) {
                break;
            }
            continue;
        }
        let resources = resource_keys(clause);
        if resources.is_empty() {
            continue;
        }
        let topic = review_topic_key(clause);
        let Some(key) = matching_failure(pending, &topic, clause, span, &resources)
            .filter(|key| !resources.is_disjoint(&key.1))
        else {
            continue;
        };
        let Some(failure) = pending.remove(&key) else {
            continue;
        };
        output.extend(build_pair(
            workspace_id,
            session,
            failure,
            key.0,
            span,
            Cow::Borrowed(clause),
            &topic,
        ));
    }
    output
}

/// Shared construction for whole-window and boundary repairs. Only display
/// excerpts change; both paths retain the original complete source locator,
/// hash and identity and go through the same source-package/pair builder.
/// Moving the ordinary path's Cow preserves its existing allocation behavior.
pub(super) fn build_pair(
    workspace_id: &str,
    session: &StoredSession,
    failure: PendingFailure<'_>,
    failure_topic: String,
    repair: &StoredEvidenceSpan,
    repair_message: Cow<'_, str>,
    repair_topic: &str,
) -> Vec<ReviewSessionCandidate> {
    let pair_topic = if failure_topic != "noise" && failure_topic == repair_topic {
        failure_topic
    } else {
        review_topic_key(&format!("{} {}", failure.message, repair_message))
    };
    let mut failure_source = failure.source.clone();
    failure_source.excerpt = failure.message.into_owned();
    let mut repair_source = repair.clone();
    repair_source.excerpt = repair_message.into_owned();
    build_session_arc_candidate_pair(
        workspace_id,
        session,
        &pair_topic,
        &failure_source,
        &repair_source,
    )
}

#[cfg(test)]
#[path = "curate_session_arc_boundary_tests.rs"]
mod tests;
