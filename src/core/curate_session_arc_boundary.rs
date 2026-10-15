//! Carry unfinished work across windows that also contain completed lessons.
//!
//! A CASS window is a storage boundary, not an episode boundary. Reuse the
//! existing projected clauses and outcome policy; never decode a body twice
//! or invent transcript positions for a clause within the original span.

use std::borrow::Cow;

use super::{
    PendingFailure, PendingFailures, StoredEvidenceSpan, failure_signal, resolution_signal,
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

#[cfg(test)]
#[path = "curate_session_arc_boundary_tests.rs"]
mod tests;
