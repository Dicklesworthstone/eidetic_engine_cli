//! Mine every disjoint conversation episode, not just the first hit per topic.
//!
//! Projection is shared with inline learning: JSON envelope fields, tool
//! results and metadata cannot masquerade as failures or repairs. Selection
//! never changes an evidence ID, hash, source locator, or durable excerpt.

use std::borrow::Cow;
use std::collections::BTreeSet;

use super::super::{
    ReviewSessionCandidate, StoredEvidenceSpan, StoredSession, build_session_arc_candidate_pair,
    review_topic_key, session_arc_span_order,
};
use super::{failure_signal, inline_candidates_from_message, resolution_signal, text};

#[path = "curate_session_arc_boundary.rs"]
mod boundary;

#[path = "curate_session_arc_dedup.rs"]
mod dedup;

#[path = "curate_session_arc_subject_index.rs"]
mod subject_index;
use subject_index::PendingFailures;

struct PendingFailure<'a> {
    source: &'a StoredEvidenceSpan,
    message: Cow<'a, str>,
    explicitly_marked: bool,
    requires_resource_match: bool,
}

/// Read each projected window once and hold at most one lookahead body. Bare
/// result windows qualify the preceding physically contiguous observation;
/// they are not independent repairs. A later clean report cannot erase an
/// earlier failure in the same run. Never concatenate or relabel source spans.
fn observation_groups<'a>(
    spans: impl Iterator<Item = &'a StoredEvidenceSpan>,
) -> impl Iterator<Item = (&'a StoredEvidenceSpan, Option<Cow<'a, str>>, bool)> {
    let mut projected = spans
        .map(|span| (span, text::message_text(&span.excerpt)))
        .peekable();
    std::iter::from_fn(move || {
        let (span, message) = projected.next()?;
        if message.is_none() {
            return Some((span, message, false));
        }
        let mut tail = span;
        let mut veto = false;
        while let Some((next, Some(next_message))) = projected.peek() {
            // Missing, overlapping, inverted or overflowed positions cannot
            // prove that an otherwise bare report belongs to this operation.
            if !precedes(tail, next) || tail.end_line.checked_add(1) != Some(next.start_line) {
                break;
            }
            let Some(failed) = super::clauses::report_window_veto(next_message) else {
                break;
            };
            veto |= failed;
            tail = *next;
            let _ = projected.next();
        }
        Some((span, message, veto))
    })
}

/// Retain complete reciprocal pairs. A repair consumes one preceding failure;
/// neither endpoint can be reused for another cross-window episode. Independent
/// topics can interleave, while another failure on the same subject replaces the
/// unresolved observation only when its concrete resource set also agrees.
/// Explicit markers and unambiguous resource references can bridge topic keys.
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
    let mut pending = PendingFailures::default();
    for (span, message, trailer_veto) in observation_groups(ordered.into_iter()) {
        let Some(message) = message else {
            // Do not establish a causal link across a record whose conversation
            // body cannot be safely interpreted. In particular, rejecting an
            // escaped instruction must not splice its neighboring text together.
            pending.clear();
            continue;
        };
        let inline = inline_candidates_from_message(workspace_id, session, span, message.as_ref());
        if !inline.is_empty() {
            // A contradicted window cannot supply a verified inline lesson
            // either. Keep its bytes intact and abstain, rather than assign a
            // later report to one of several possible repairs inside it.
            if !trailer_veto {
                append_pairs(&mut output, &mut emitted, inline);
            }
            // Completed work on another named subject must not erase an
            // interleaved failure just because CASS grouped that work into one
            // window. Unnamed or overlapping subjects remain causal barriers.
            retain_disjoint_failures(&mut pending, message.as_ref());
            // A completed inline episode must not swallow a later unfinished
            // failure just because both were captured in this source window.
            boundary::retain_tail(&mut pending, span, message.as_ref());
            continue;
        }
        let topic = review_topic_key(message.as_ref());
        let resources = resource_keys(message.as_ref());
        if resolution_signal(message.as_ref()) {
            if trailer_veto {
                // Do not consume the real failure or invent a failed excerpt
                // from this optimistic claim. A later actual repair can still
                // resolve the original source-backed observation.
                continue;
            }
            let key = matching_failure(&pending, &topic, message.as_ref(), span, &resources);
            if let Some(key) = key
                && let Some(failure) = pending.remove(&key)
            {
                let pair_topic = if key.0 != "noise" && key.0 == topic {
                    key.0
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
        if topic != "noise" && failure_signal(message.as_ref()) {
            let explicitly_marked = message.to_ascii_lowercase().contains("failure arc:");
            pending.insert(
                (topic, resources),
                PendingFailure {
                    source: span,
                    message,
                    explicitly_marked,
                    requires_resource_match: false,
                },
            );
        }
    }
    dedup::retain_distinct_pairs(output)
}

/// Keep only failures whose concrete subjects are disjoint from the complete
/// decoded window. Every outcome clause must identify its subject: a named
/// repair cannot make an anonymous failure independent of an older episode.
/// Retained failures require an explicit resource match on their eventual
/// repair; markers, coarse topics and pronouns cannot jump across completed work.
fn retain_disjoint_failures(pending: &mut PendingFailures<'_>, message: &str) {
    if pending.is_empty() {
        return;
    }
    let mut observed = BTreeSet::new();
    for clause in super::clauses::split(message) {
        let resources = resource_keys(clause);
        if resources.is_empty() && (failure_signal(clause) || resolution_signal(clause)) {
            pending.clear();
            return;
        }
        observed.extend(resources);
    }
    pending.retain_disjoint(&observed);
}

/// A topic is only a coarse hint. Concrete source identities keep interleaved
/// failures in the same subsystem separate and can connect a repair whose
/// wording no longer contains the failure's subsystem keyword.
pub(super) type FailureKey = (String, BTreeSet<String>);

fn matching_failure(
    pending: &PendingFailures<'_>,
    topic: &str,
    message: &str,
    repair: &StoredEvidenceSpan,
    resources: &BTreeSet<String>,
) -> Option<FailureKey> {
    let eligible = pending.eligible(
        topic,
        resources,
        message.to_ascii_lowercase().contains("fix:"),
        |key, failure| {
            precedes(failure.source, repair)
                && (!failure.requires_resource_match || !resources.is_disjoint(&key.1))
        },
    );
    let subjects: Vec<_> = eligible.iter().map(|(key, _)| *key).collect();
    let anchored = subjects.iter().any(|key| !resources.is_disjoint(&key.1));
    if !anchored && message.to_ascii_lowercase().contains("fix:") {
        // Markers may bridge subsystem wording, not contradictory concrete
        // subjects. An ambiguous anchor must still abstain rather than fall
        // back to recency.
        if let Some((key, _)) = eligible
            .iter()
            .copied()
            .filter(|(_, failure)| failure.explicitly_marked)
            .max_by(|(_, left), (_, right)| session_arc_span_order(left.source, right.source))
        {
            // No eligible failure shares a resource in this branch. Reject a
            // contradictory latest marker before topic/pronoun fallback; do
            // not skip it to an older failure with an unspecified subject.
            return (resources.is_empty() || key.1.is_empty()).then(|| key.clone());
        }
    }
    if let Some(key) = matching_subject(&subjects, topic, resources) {
        return Some(key);
    }
    // "Fixed it by ..." explicitly refers back without repeating the topic.
    // Accept this narrow form only for one unresolved, physically adjacent
    // window. A generic success, an intervening turn, or multiple failures is
    // not enough. This remains an attributed proposal, not a proof of causation.
    if pending.len() == 1 && refers_to_previous_failure(message) {
        let (key, failure) = eligible.first()?;
        if failure.source.end_line.checked_add(1) == Some(repair.start_line)
            && (resources.is_empty() || key.1.is_empty())
        {
            return Some((*key).clone());
        }
    }
    None
}

/// Match the subject independently of whether CASS stored one message window
/// or several. Both callers supply only temporally preceding failures. Exact
/// resources override coarse topics; an ambiguous resource never picks the
/// newest failure merely because it was nearby.
pub(super) fn matching_subject(
    eligible: &[&FailureKey],
    topic: &str,
    resources: &BTreeSet<String>,
) -> Option<FailureKey> {
    let anchored: Vec<_> = eligible
        .iter()
        .copied()
        .filter(|key| !resources.is_disjoint(&key.1))
        .collect();
    if !anchored.is_empty() {
        let topical: Vec<_> = anchored
            .iter()
            .copied()
            .filter(|key| key.0 == topic)
            .collect();
        let choices = if topical.is_empty() {
            &anchored
        } else {
            &topical
        };
        // Two unresolved failures naming the same resource are ambiguous. Do
        // not manufacture a causal relation merely by choosing the nearest.
        return (choices.len() == 1).then(|| choices[0].clone());
    }
    let topical: Vec<_> = eligible
        .iter()
        .copied()
        .filter(|key| key.0 == topic && (resources.is_empty() || key.1.is_empty()))
        .collect();
    (topical.len() == 1).then(|| topical[0].clone())
}

pub(super) fn refers_to_previous_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let words: Vec<_> = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let skip = usize::from(matches!(words.first().copied(), Some("i" | "we")));
    let words = &words[skip..];
    matches!(
        words.first().copied(),
        Some("fixed" | "resolved" | "repaired")
    ) && matches!(words.get(1).copied(), Some("it" | "this" | "that"))
        && words
            .iter()
            .skip(2)
            .any(|word| matches!(*word, "by" | "after" | "with"))
}

/// Keep exact, case-sensitive file paths and qualified Rust symbols. Never
/// turn a basename, wildcard, URL, shell expression, or generic prose noun into
/// an alias for another resource. Diagnostic line/column suffixes identify a
/// position within the same file, not another resource. This classification
/// never changes the spelling or location recorded in the source evidence.
pub(super) fn resource_keys(message: &str) -> BTreeSet<String> {
    message
        .split_whitespace()
        .filter_map(|raw| {
            let token = raw
                .trim_start_matches(['"', '\'', '`', '(', '[', '{'])
                .trim_end_matches(['"', '\'', '`', ']', '}', ',', ';', '.', '!', '?']);
            let symbol = token.strip_suffix("()").unwrap_or(token);
            let identifier = |part: &str| {
                let mut chars = part.chars();
                chars
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
                    && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            };
            if symbol.contains("::") && symbol.split("::").all(identifier) {
                return Some(symbol.to_owned());
            }
            let path = diagnostic_resource_path(token);
            if path.is_empty()
                || path.contains("://")
                || !path.chars().all(|ch| {
                    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':')
                })
            {
                return None;
            }
            if path.contains(':')
                && !(path.as_bytes().get(1) == Some(&b':')
                    && path.as_bytes()[0].is_ascii_alphabetic()
                    && !path[2..].contains(':'))
            {
                return None;
            }
            let extension = path.rsplit('.').next().unwrap_or("");
            let named_file = matches!(
                extension,
                "rs" | "py"
                    | "go"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "c"
                    | "cc"
                    | "cpp"
                    | "h"
                    | "hpp"
                    | "java"
                    | "kt"
                    | "swift"
                    | "rb"
                    | "sql"
                    | "toml"
                    | "json"
                    | "yaml"
                    | "yml"
                    | "sh"
                    | "md"
                    | "lock"
                    | "env"
                    | "txt"
                    | "log"
                    | "jsonl"
                    | "db"
                    | "sqlite"
            ) && path.contains('.');
            let absolute_file =
                (path.starts_with('/') || path.starts_with("./") || path.starts_with("../"))
                    && !path.ends_with('/')
                    && path
                        .split('/')
                        .any(|part| !part.is_empty() && part != "." && part != "..");
            (named_file || absolute_file).then(|| path.to_owned())
        })
        .collect()
}

/// Strip only a complete, recognized source-position suffix. The caller still
/// validates the entire remaining path, including Windows drive syntax. Never
/// resolve paths, fold case, or promote a basename to a workspace-relative path.
fn diagnostic_resource_path(token: &str) -> &str {
    // MSVC diagnostics put a colon after the closing location parenthesis.
    // Remove that punctuation only for recognition, not from malformed input.
    let decorated = token.strip_suffix(':').unwrap_or(token);
    if let Some((path, position)) = decorated
        .strip_suffix(')')
        .and_then(|value| value.rsplit_once('('))
    {
        let valid = match position.split_once(',') {
            Some((line, column)) => source_position(line, false) && source_position(column, true),
            None => source_position(position, false),
        };
        if valid {
            return path;
        }
    }
    let token = token.trim_end_matches(')');
    let position = token.strip_suffix(':').unwrap_or(token);
    let Some((prefix, last)) = position.rsplit_once(':') else {
        return token;
    };
    if !source_position(last, true) {
        return token;
    }
    if let Some((path, line)) = prefix.rsplit_once(':')
        && source_position(line, false)
    {
        return path;
    }
    if source_position(last, false) {
        prefix
    } else {
        token
    }
}

fn source_position(value: &str, allow_zero: bool) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value
            .parse::<u32>()
            .is_ok_and(|position| allow_zero || position > 0)
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
    // Consume complete owned pairs so accepting a lesson does not clone its
    // content and full provenance package. As before, discard an odd tail and
    // deduplicate both endpoints or neither, preserving reciprocal identities.
    let mut candidates = candidates.into_iter();
    while let (Some(left), Some(right)) = (candidates.next(), candidates.next()) {
        let pair = [left, right];
        if pair
            .iter()
            .any(|candidate| emitted.contains(&candidate.candidate_id))
        {
            continue;
        }
        for candidate in pair {
            emitted.insert(candidate.candidate_id.clone());
            output.push(candidate);
        }
    }
}

#[cfg(test)]
#[path = "curate_session_arc_sequence_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "curate_session_arc_interleaving_tests.rs"]
mod interleaving_tests;

#[cfg(test)]
#[path = "curate_session_arc_sequence_cost_tests.rs"]
mod cost_tests;
