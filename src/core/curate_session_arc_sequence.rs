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
    let mut pending = BTreeMap::<FailureKey, PendingFailure<'_>>::new();
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
        let resources = resource_keys(message.as_ref());
        if resolution_signal(message.as_ref()) {
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
        if topic != "noise" && session_arc_failure_signal(message.as_ref()) {
            let explicitly_marked = message.to_ascii_lowercase().contains("failure arc:");
            pending.insert(
                (topic, resources),
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

/// A topic is only a coarse hint. Concrete source identities keep interleaved
/// failures in the same subsystem separate and can connect a repair whose
/// wording no longer contains the failure's subsystem keyword.
pub(super) type FailureKey = (String, BTreeSet<String>);

fn matching_failure(
    pending: &BTreeMap<FailureKey, PendingFailure<'_>>,
    topic: &str,
    message: &str,
    repair: &StoredEvidenceSpan,
    resources: &BTreeSet<String>,
) -> Option<FailureKey> {
    if message.to_ascii_lowercase().contains("fix:") {
        // An explicit declaration is stronger than inferred topic/resource
        // agreement. It may describe a change in a different file or subsystem.
        if let Some((key, _)) = pending
            .iter()
            .filter(|(_, failure)| failure.explicitly_marked && precedes(failure.source, repair))
            .max_by(|(_, left), (_, right)| session_arc_span_order(left.source, right.source))
        {
            return Some(key.clone());
        }
    }
    let eligible: Vec<_> = pending
        .iter()
        .filter(|(_, failure)| precedes(failure.source, repair))
        .collect();
    let subjects: Vec<_> = eligible.iter().map(|(key, _)| *key).collect();
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
/// an alias for another resource. Quoting and sentence punctuation are wrappers;
/// the spelling inside the resource is the identity used for comparison.
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
            let path = token.trim_end_matches(')');
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
    //
    // `as_chunks::<2>().0` is the complete-pairs half, so a trailing odd
    // candidate is still dropped exactly as `chunks_exact(2)` dropped it
    // (bd-4aw2d: clippy::chunks_exact_to_as_chunks, the last lib blocker).
    for pair in candidates.as_chunks::<2>().0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> StoredSession {
        StoredSession {
            id: "ses_resource0000000000000000000".into(),
            workspace_id: "wsp_resource0000000000000000000".into(),
            cass_session_id: "ordinary-repair-prose".into(),
            source_path: None,
            agent_name: None,
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 0,
            token_count: None,
            content_hash: "blake3:resource-session".into(),
            metadata_json: None,
            imported_at: "2026-09-22T00:00:00Z".into(),
            updated_at: "2026-09-22T00:00:00Z".into(),
        }
    }

    fn span(id: &str, line: u32, text: &str) -> StoredEvidenceSpan {
        let hash = format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex());
        StoredEvidenceSpan {
            id: id.into(),
            workspace_id: session().workspace_id,
            session_id: session().id,
            memory_id: None,
            cass_span_id: format!("cass-{id}"),
            span_kind: "message".into(),
            start_line: line,
            end_line: line,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".into()),
            excerpt: text.into(),
            content_hash: hash.clone(),
            metadata_json: None,
            producer_kind: "cass_import".into(),
            screening_version: 1,
            secret_redaction_status: "clean".into(),
            redaction_classes_json: "[]".into(),
            instruction_risk: "none".into(),
            search_eligibility: "admitted".into(),
            pack_eligibility: "admitted".into(),
            canonical_provenance_revision: 1,
            canonical_excerpt_hash: Some(hash),
            security_policy_epoch: 1,
            upstream_ref_hash: Some(format!("blake3:cass-{id}")),
            created_at: "2026-09-22T00:00:00Z".into(),
            updated_at: "2026-09-22T00:00:00Z".into(),
        }
    }

    fn mine(spans: &[StoredEvidenceSpan]) -> Vec<ReviewSessionCandidate> {
        let session = session();
        candidates(&session.workspace_id, &session, spans)
    }

    fn endpoints(rows: &[ReviewSessionCandidate]) -> Vec<(&str, &str)> {
        rows.iter()
            .filter(|row| row.candidate_kind == "session_arc_rule")
            .map(|row| {
                let arc = row.session_arc.as_ref().expect("source-backed arc");
                (
                    arc.failure_span.evidence_span_id.as_str(),
                    arc.resolution_span.evidence_span_id.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn session_arc_ordinary_repairs_do_not_need_repeated_subsystem_words() {
        for (failure, repair) in [
            (
                "The build failed in `src/cache.rs`.",
                "Fixed `src/cache.rs` by replacing the unstable key.",
            ),
            (
                "A regression broke `Cache::get()`.",
                "`Cache::get()` was repaired by preserving identity bytes.",
            ),
            (
                "The database migration failed in migrations/0085.sql.",
                "Guarding null inputs in migrations/0085.sql resolved the issue.",
            ),
            (
                "The search cache failed in src/ranker.rs.",
                "Fixed src/ranker.rs by clearing the obsolete generation.",
            ),
            (
                "The agent helper failed in src/owner.rs.",
                "Repaired src/owner.rs by retaining the lock descriptor.",
            ),
            (
                "The transcript parser failed in src/reader.rs.",
                "Fixed src/reader.rs by preserving message order.",
            ),
            (
                "The import failed while calling Store::insert().",
                "Store::insert() was fixed by preserving the original source hash.",
            ),
            (
                "The worker failed while opening Cargo.lock.",
                "We fixed it by restoring the pinned dependency versions.",
            ),
        ] {
            let spans = [span("failure", 10, failure), span("repair", 11, repair)];
            let rows = mine(&spans);
            assert_eq!(rows.len(), 2, "{failure} / {repair}");
            assert_eq!(endpoints(&rows), [("failure", "repair")]);
            for row in &rows {
                let arc = row.session_arc.as_ref().unwrap();
                assert_eq!(arc.failure_span.content_hash, spans[0].content_hash);
                assert_eq!(arc.resolution_span.content_hash, spans[1].content_hash);
                assert!(row.proposed_content.contains(failure));
                assert!(row.proposed_content.contains(repair));
            }
            let reversed = [spans[1].clone(), spans[0].clone()];
            assert_eq!(mine(&reversed), rows);

            // The same lesson must survive when CASS puts both observations
            // in one message window rather than two separate spans.
            let body = format!("{failure}\n{repair}");
            let combined = span("combined", 10, &body);
            let inline = mine(std::slice::from_ref(&combined));
            assert_eq!(inline.len(), 2, "single window: {body}");
            assert_eq!(endpoints(&inline), [("combined", "combined")]);
            for row in &inline {
                let arc = row.session_arc.as_ref().unwrap();
                assert_eq!(row.source_ids, std::slice::from_ref(&combined.id));
                assert_eq!(arc.failure_span.excerpt, failure);
                assert_eq!(arc.resolution_span.excerpt, repair);
                assert_eq!(arc.failure_span.content_hash, combined.content_hash);
                assert_eq!(arc.resolution_span.content_hash, combined.content_hash);
            }
        }
    }

    #[test]
    fn session_arc_interleaved_failures_share_a_topic_not_a_resource() {
        let spans = [
            span("failure-a", 1, "cargo test src/api.rs failed."),
            span("failure-b", 2, "cargo test src/ui.rs failed."),
            span(
                "repair-a",
                3,
                "Fixed src/api.rs by adding the missing guard.",
            ),
            span(
                "repair-b",
                4,
                "Fixed src/ui.rs by retaining the expected state.",
            ),
        ];
        let rows = mine(&spans);
        assert_eq!(
            endpoints(&rows),
            [("failure-a", "repair-a"), ("failure-b", "repair-b")]
        );
        for row in &rows {
            let sources: Vec<_> = spans
                .iter()
                .filter(|source| row.source_ids.contains(&source.id))
                .cloned()
                .collect();
            assert!(
                mine(&sources).contains(row),
                "application must reconstruct identical source-bound candidates"
            );
        }
        let mut reversed = spans.to_vec();
        reversed.reverse();
        assert_eq!(mine(&reversed), rows);
    }

    #[test]
    fn session_arc_shared_topic_cannot_override_different_concrete_resources() {
        for (failure, repair) in [
            (
                "cargo test src/api.rs failed.",
                "cargo test src/ui.rs passed.",
            ),
            (
                "cargo test src/Cache.rs failed.",
                "cargo test src/cache.rs passed.",
            ),
            ("cargo test src/api.rs failed.", "cargo test api.rs passed."),
            (
                "cargo test Store::open() failed.",
                "cargo test OtherStore::open() passed.",
            ),
        ] {
            assert!(
                mine(&[span("failure", 1, failure), span("repair", 2, repair)]).is_empty(),
                "{failure} / {repair}"
            );
            assert!(
                mine(&[span("combined", 1, &format!("{failure} {repair}"))]).is_empty(),
                "same-topic different-resource clauses must not pair: {failure} / {repair}"
            );
        }
    }

    #[test]
    fn session_arc_resource_ambiguity_requires_a_disambiguating_topic() {
        let mut spans = vec![
            span("format-failed", 1, "cargo fmt src/main.rs failed."),
            span("lint-failed", 2, "cargo clippy src/main.rs failed."),
            span("ambiguous", 3, "Fixed src/main.rs by changing its imports."),
        ];
        assert!(
            mine(&spans).is_empty(),
            "shared file alone does not identify the failed operation"
        );
        spans.push(span("format-fixed", 4, "cargo fmt src/main.rs passed."));
        assert_eq!(
            endpoints(&mine(&spans)),
            [("format-failed", "format-fixed")]
        );
    }

    #[test]
    fn session_arc_pronoun_repairs_require_one_adjacent_unresolved_failure() {
        let failure = span("failure", 1, "The worker failed while opening Cargo.lock.");
        let repair = "We fixed it by restoring the pinned dependency versions.";
        assert_eq!(mine(&[failure.clone(), span("repair", 2, repair)]).len(), 2);
        assert!(mine(&[failure.clone(), span("repair", 3, repair)]).is_empty());
        assert!(
            mine(&[
                failure.clone(),
                span("another-failure", 2, "cargo clippy failed."),
                span("repair", 3, repair)
            ])
            .is_empty()
        );
        for non_repair in [
            "It works now.",
            "Fixed the unrelated database writes.",
            "We will fix it by restoring the pinned versions.",
            "We fixed it by restoring the pinned versions but the retry failed.",
            "We fixed it by replacing Another.lock.",
        ] {
            assert!(
                mine(&[failure.clone(), span("repair", 2, non_repair)]).is_empty(),
                "{non_repair}"
            );
        }
    }

    #[test]
    fn session_arc_resources_come_from_decoded_bodies_not_json_metadata() {
        let failure = serde_json::json!({"type":"assistant", "content":"The build failed.",
            "metadata":{"file":"src/cache.rs"}})
        .to_string();
        let repair = serde_json::json!({"type":"assistant", "content":"Fixed src/cache.rs by changing the key."}).to_string();
        assert!(mine(&[span("failure", 1, &failure), span("repair", 2, &repair)]).is_empty());
        let failure =
            serde_json::json!({"type":"assistant", "content":"The build failed in src/cache.rs.",
            "metadata":{"file":"src/unrelated.rs"}})
            .to_string();
        let spans = [span("failure", 1, &failure), span("repair", 2, &repair)];
        let rows = mine(&spans);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0]
                .session_arc
                .as_ref()
                .unwrap()
                .failure_span
                .content_hash,
            spans[0].content_hash
        );
        assert!(
            rows.iter()
                .all(|row| !row.proposed_content.contains("src/unrelated.rs"))
        );
    }

    #[test]
    fn session_arc_resource_spelling_is_not_a_basename_or_expression_alias() {
        assert_eq!(
            resource_keys("`src/api.rs`, Store::open() Cargo.lock (migrations/001.sql)."),
            BTreeSet::from([
                "src/api.rs".into(),
                "Store::open".into(),
                "Cargo.lock".into(),
                "migrations/001.sql".into()
            ])
        );
        for text in [
            "cache policy fix",
            "https://host/api.rs",
            "${ROOT}/api.rs",
            "src/*.rs",
            "src/a?.rs",
            "accept/reject",
        ] {
            assert!(resource_keys(text).is_empty(), "{text}");
        }
        assert_ne!(resource_keys("src/api.rs"), resource_keys("api.rs"));
        assert_ne!(resource_keys("src/Cache.rs"), resource_keys("src/cache.rs"));
    }

    #[test]
    fn session_arc_policy_windows_keep_observed_risk_repair_and_both_sources() {
        let failure = "Failure arc: storing silently would violate the no-loop-takeover policy.";
        let repair = "Fix: require accept/reject commands and audit every accepted capture.";
        let sources = [
            span("policy-failure", 3, failure),
            span("policy-repair", 4, repair),
        ];
        let rows = mine(&sources);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let specificity = crate::curate::specificity_score(&row.proposed_content);
            assert!(
                specificity.passes_threshold,
                "{specificity:?}: {}",
                row.proposed_content
            );
            assert!(specificity.structural_signals.has_provenance_uri);
            assert!(row.proposed_content.contains(&format!("Risk: {failure}")));
            assert!(
                row.proposed_content
                    .contains(&format!("Mitigation: {repair}"))
            );
            for source in &sources {
                assert!(
                    row.proposed_content
                        .contains(&source.canonical_provenance_uri())
                );
            }
            assert!(
                !row.proposed_content.contains("cargo"),
                "never invent a command to pass specificity"
            );
            let arc = row.session_arc.as_ref().unwrap();
            assert_eq!(arc.failure_span.content_hash, sources[0].content_hash);
            assert_eq!(arc.resolution_span.content_hash, sources[1].content_hash);
        }
    }

    #[test]
    fn session_arc_already_specific_two_window_content_keeps_its_identity() {
        let failure = "cargo test failed because the cache key was stale.";
        let repair = "Fixed the cache key and cargo test passed.";
        let sources = [span("failure", 1, failure), span("repair", 2, repair)];
        let expected = [
            format!(
                "Anti-pattern for `testing`: this session hit a failure after `{failure}`. The later fix was `{repair}`."
            ),
            format!(
                "Rule for `testing`: when `{failure}` appears, apply the later repair: `{repair}`."
            ),
        ];
        let rows = mine(&sources);
        assert_eq!(rows.len(), 2);
        for (row, expected) in rows.iter().zip(expected) {
            assert!(crate::curate::specificity_score(&expected).passes_threshold);
            assert_eq!(row.proposed_content, expected);
            assert_eq!(
                row.content_hash,
                format!("blake3:{}", blake3::hash(expected.as_bytes()).to_hex())
            );
        }
    }
}
