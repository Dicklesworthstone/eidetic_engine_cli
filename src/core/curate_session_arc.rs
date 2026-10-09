//! Source-backed failed-to-fixed learning. Proposals never apply themselves.

use super::*;

#[path = "curate_session_arc_clauses.rs"]
mod clauses;
#[path = "curate_session_arc_sequence.rs"]
mod sequence;
#[path = "curate_session_arc_text.rs"]
pub(crate) mod text;

pub(super) fn sequence_candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    sequence::candidates(workspace_id, session, spans)
}

/// A failure and its repair may live in one imported CASS window. Keep the
/// original evidence ID, hash, and complete locator: sentence boundaries in an
/// excerpt are not transcript line boundaries and must not invent provenance.
pub(super) fn inline_candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    let mut candidates = Vec::new();
    let mut seen_ids = BTreeSet::new();
    for span in spans {
        if span.workspace_id != workspace_id || span.session_id != session.id {
            continue;
        }
        let Some(message) = text::message_text(&span.excerpt) else {
            continue;
        };
        for (failure, repair) in inline_pairs(message.as_ref()) {
            let mut failure_span = span.clone();
            failure_span.excerpt = failure.to_owned();
            let mut repair_span = span.clone();
            repair_span.excerpt = repair.to_owned();
            let topic = review_topic_key(&format!("{failure} {repair}"));
            let pair = build_session_arc_candidate_pair(
                workspace_id,
                session,
                &topic,
                &failure_span,
                &repair_span,
            );
            // Repeated observations (or equal compacted lesson content) must
            // not persist duplicate IDs or leave only one reciprocal member.
            // Keep the builder's content-bound identities and source package.
            if pair
                .iter()
                .any(|candidate| seen_ids.contains(&candidate.candidate_id))
            {
                continue;
            }
            for candidate in pair {
                seen_ids.insert(candidate.candidate_id.clone());
                candidates.push(candidate);
            }
        }
    }
    candidates
}

/// Walk the complete admitted message using the same subject identities as
/// cross-window learning. CASS window boundaries must not hide ordinary repairs
/// or turn two unrelated files into a lesson because both mention "test".
/// A repair consumes its failure; another failure replaces only an unresolved
/// observation of the same subject. Each half borrows an exact source clause.
fn inline_pairs(excerpt: &str) -> impl Iterator<Item = (&str, &str)> {
    struct PendingClause<'a> {
        text: &'a str,
        position: usize,
        explicitly_marked: bool,
    }
    let mut pending = BTreeMap::<sequence::FailureKey, PendingClause<'_>>::new();
    // Technical tokens and quoted commands stay intact. A bare occurrence of
    // both keywords in a single clause is not evidence of temporal ordering.
    let mut parts: Vec<_> = clauses::split(excerpt)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| (part, false))
        .collect();
    // Tool summaries often put the final exit status after an optimistic test
    // summary. Classify contiguous outcome trailers before consuming a failure
    // so sentence/window boundaries cannot turn a failed retry into a lesson.
    let mut following_veto = false;
    for (part, trailer_veto) in parts.iter_mut().rev() {
        *trailer_veto = following_veto;
        following_veto =
            process_report_veto(part).is_some_and(|current_veto| current_veto || following_veto);
    }
    parts
        .into_iter()
        .enumerate()
        .filter_map(move |(position, (part, trailer_veto))| {
            let topic = review_topic_key(part);
            let resources = sequence::resource_keys(part);
            if !trailer_veto && resolution_signal(part) {
                let subjects: Vec<_> = pending.keys().collect();
                let anchored = subjects.iter().any(|key| !resources.is_disjoint(&key.1));
                // A marker supplies ordering, not permission to contradict a
                // named subject. An ambiguous anchor must remain unresolved;
                // falling back to the nearest marker would invent causation.
                let marked_repair = !anchored && part.to_ascii_lowercase().contains("fix:");
                let explicitly_linked = if marked_repair {
                    pending
                        .iter()
                        .filter(|(_, failure)| failure.explicitly_marked)
                        .max_by_key(|(_, failure)| failure.position)
                        .map(|(key, _)| key.clone())
                } else {
                    None
                };
                let key = explicitly_linked
                    .or_else(|| sequence::matching_subject(&subjects, &topic, &resources));
                let key = key.or_else(|| {
                    if pending.len() != 1 || !sequence::refers_to_previous_failure(part) {
                        return None;
                    }
                    let (key, failure) = pending.first_key_value()?;
                    (failure.position.checked_add(1) == Some(position)
                        && (resources.is_empty() || key.1.is_empty()))
                    .then(|| key.clone())
                });
                return key
                    .and_then(|key| pending.remove(&key).map(|failure| (failure.text, part)));
            }
            if topic != "noise" && failure_signal(part) {
                pending.insert(
                    (topic, resources),
                    PendingClause {
                        text: part,
                        position,
                        explicitly_marked: part.to_ascii_lowercase().contains("failure arc:"),
                    },
                );
            }
            None
        })
}

// Existing clause-boundary tests also pin the historical first-pair behavior.
#[cfg(test)]
fn inline_pair(excerpt: &str) -> Option<(&str, &str)> {
    inline_pairs(excerpt).next()
}

/// A classification-only view of test counters and observed process outcomes.
/// The conversation, evidence hash, locator and proposal excerpts stay intact.
struct OutcomeSignalText<'a> {
    text: std::borrow::Cow<'a, str>,
    has_counted_failure: bool,
    has_process_failure: bool,
    has_unverified_process_outcome: bool,
}

/// Recognize integer counters, not a trailing zero in a decimal, fraction,
/// signed value or grouped number. A numeric shape that is not a proven zero
/// remains a failure veto; it is never used to erase a failure word.
fn outcome_count_value(excerpt: &str, token: (usize, &str)) -> Option<bool> {
    let (start, value) = token;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let bytes = excerpt.as_bytes();
    let end = start + value.len();
    if start > 0 {
        let previous = bytes[start - 1];
        if matches!(previous, b'.' | b'/' | b'\\' | b'+' | b'-')
            || (matches!(previous, b',' | b':') && start > 1 && bytes[start - 2].is_ascii_digit())
        {
            return Some(true);
        }
    }
    if end + 1 < bytes.len()
        && matches!(
            bytes[end],
            b'.' | b',' | b':' | b'/' | b'\\' | b'+' | b'-' | b'*'
        )
        && bytes[end + 1].is_ascii_alphanumeric()
    {
        return Some(true);
    }
    // No integer conversion: an arbitrarily large positive count still vetoes
    // a repair rather than overflowing into an unknown or zero observation.
    Some(value.bytes().any(|byte| byte != b'0'))
}

fn outcome_count(excerpt: &str, tokens: &[(usize, &str)], index: usize) -> Option<bool> {
    let (start, word) = tokens[index];
    let whitespace_between = |left: usize, right: usize| {
        let gap = &excerpt[left..right];
        !gap.is_empty() && gap.chars().all(char::is_whitespace)
    };
    let before = index.checked_sub(1).and_then(|previous| {
        let (position, token) = tokens[previous];
        if !whitespace_between(position + token.len(), start) {
            return None;
        }
        if let Some(count) = outcome_count_value(excerpt, tokens[previous]) {
            return Some(count);
        }
        // Both `0 failed` and `0 tests failed` are explicit quantities.
        if !matches!(
            token.to_ascii_lowercase().as_str(),
            "test" | "tests" | "check" | "checks" | "assertion" | "assertions"
        ) {
            return None;
        }
        let number = previous.checked_sub(1)?;
        let (position, token) = tokens[number];
        whitespace_between(position + token.len(), tokens[previous].0)
            .then(|| outcome_count_value(excerpt, tokens[number]))
            .flatten()
    });
    let after = tokens.get(index + 1).and_then(|token| {
        let gap = excerpt[start + word.len()..token.0].trim();
        matches!(gap, ":" | "=").then(|| outcome_count_value(excerpt, *token).unwrap_or(true))
    });
    match (before, after) {
        // Conflicting counters must never turn a positive failure into zero.
        (Some(left), Some(right)) => Some(left || right),
        (Some(count), None) | (None, Some(count)) => Some(count),
        (None, None) => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessOutcome {
    Succeeded,
    Failed,
    Unverified,
}

/// Recognize reported exit results, not shell invocations such as `exit 1`.
/// Multiple reports are independent: a later zero cannot erase a nonzero exit
/// from the same observation. Missing/malformed results veto a positive lesson
/// but do not invent an observed failure or a successful execution.
fn process_outcome(
    excerpt: &str,
    tokens: &[(usize, &str)],
    index: usize,
) -> Option<ProcessOutcome> {
    let (_, word) = tokens[index];
    let followed_by = |position: usize, expected: &str| {
        let Some(&(next_start, next_word)) = tokens.get(position + 1) else {
            return false;
        };
        let (start, word) = tokens[position];
        let gap = &excerpt[start + word.len()..next_start];
        next_word.eq_ignore_ascii_case(expected)
            && !gap.is_empty()
            && gap.chars().all(char::is_whitespace)
    };
    let (last, assignment_required) = match word.to_ascii_lowercase().as_str() {
        "exit_code" | "exit_status" | "exitcode" | "exitstatus" => (index, false),
        "exit" if followed_by(index, "code") || followed_by(index, "status") => (index + 1, false),
        "exited"
            if followed_by(index, "with")
                && (followed_by(index + 1, "code") || followed_by(index + 1, "status")) =>
        {
            (index + 2, false)
        }
        "exit" => (index, true),
        _ => return None,
    };
    let (start, word) = tokens[last];
    let suffix = &excerpt[start + word.len()..];
    let mut value = suffix.trim_start();
    if let Some(rest) = value.strip_prefix(':').or_else(|| value.strip_prefix('=')) {
        value = rest.trim_start();
    } else if assignment_required || (!suffix.is_empty() && suffix.len() == value.len()) {
        return None;
    }

    let negative = value.starts_with('-');
    let digits = value.strip_prefix('-').unwrap_or(value);
    let width = digits.bytes().take_while(u8::is_ascii_digit).count();
    if width == 0 {
        return Some(ProcessOutcome::Unverified);
    }
    let tail = &digits[width..];
    let boundary = tail.is_empty()
        || tail.starts_with(char::is_whitespace)
        || tail.starts_with([',', ';', ')', ']', '}'])
        || tail
            .strip_prefix('.')
            .is_some_and(|after| after.is_empty() || after.starts_with(char::is_whitespace));
    if !boundary {
        // A decimal, fraction, expression, unit or radix prefix is not an
        // integer exit status. In particular `0.5` must never become zero.
        return Some(ProcessOutcome::Unverified);
    }
    let nonzero = digits[..width].bytes().any(|byte| byte != b'0');
    Some(if nonzero {
        // Avoid integer parsing: even a huge nonzero result cannot overflow
        // into a successful exit. Negative signal-derived statuses fail too.
        ProcessOutcome::Failed
    } else if negative {
        ProcessOutcome::Unverified
    } else {
        ProcessOutcome::Succeeded
    })
}

/// Only bare process-result clauses can annotate the preceding observation.
/// An independent command or ordinary message ends the trailer run instead of
/// retroactively invalidating an earlier, completed episode.
fn process_report_veto(excerpt: &str) -> Option<bool> {
    let lower = excerpt.trim_start().to_ascii_lowercase();
    let report = [
        "process exited with code",
        "process exited with status",
        "command exited with code",
        "command exited with status",
        "exited with code",
        "exited with status",
        "exit code",
        "exit status",
        "exit_code",
        "exit_status",
        "exitcode",
        "exitstatus",
    ]
    .iter()
    .any(|prefix| {
        lower.strip_prefix(*prefix).is_some_and(|suffix| {
            suffix.is_empty()
                || suffix.starts_with(char::is_whitespace)
                || suffix.starts_with([':', '='])
        })
    }) || lower
        .strip_prefix("exit")
        .is_some_and(|suffix| suffix.trim_start().starts_with([':', '=']));
    if !report {
        return None;
    }
    let outcome = outcome_signal_text(excerpt);
    Some(outcome.has_process_failure || outcome.has_unverified_process_outcome)
}

fn outcome_signal_text(excerpt: &str) -> OutcomeSignalText<'_> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (position, ch) in excerpt
        .char_indices()
        .chain(std::iter::once((excerpt.len(), ' ')))
    {
        if ch.is_alphanumeric() || ch == '_' {
            let _ = start.get_or_insert(position);
        } else if let Some(start) = start.take() {
            tokens.push((start, &excerpt[start..position]));
        }
    }
    let mut zero_words = Vec::new();
    let mut has_counted_failure = false;
    let mut has_process_failure = false;
    let mut has_unverified_process_outcome = false;
    for (index, (start, word)) in tokens.iter().copied().enumerate() {
        match process_outcome(excerpt, &tokens, index) {
            Some(ProcessOutcome::Failed) => has_process_failure = true,
            Some(ProcessOutcome::Unverified) => has_unverified_process_outcome = true,
            Some(ProcessOutcome::Succeeded) | None => {}
        }
        let is_failure = match word.to_ascii_lowercase().as_str() {
            "failed" | "failing" | "failure" | "failures" | "error" | "errors" => true,
            "passed" | "passing" | "succeeded" | "successes" => false,
            _ => continue,
        };
        match outcome_count(excerpt, &tokens, index) {
            Some(false) => zero_words.push((start, start + word.len())),
            Some(true) => has_counted_failure |= is_failure,
            None => {}
        }
    }
    let text = if zero_words.is_empty() {
        std::borrow::Cow::Borrowed(excerpt)
    } else {
        // Copy once, preserving byte positions and every non-counter word.
        // A zero success count is neutral too: `0 passed` cannot verify a fix.
        let mut text = String::with_capacity(excerpt.len());
        let mut copied = 0;
        for (start, end) in zero_words {
            text.push_str(&excerpt[copied..start]);
            text.extend(std::iter::repeat_n(' ', end - start));
            copied = end;
        }
        text.push_str(&excerpt[copied..]);
        std::borrow::Cow::Owned(text)
    };
    OutcomeSignalText {
        text,
        has_counted_failure,
        has_process_failure,
        has_unverified_process_outcome,
    }
}

pub(super) fn failure_signal(excerpt: &str) -> bool {
    let outcome = outcome_signal_text(excerpt);
    outcome.has_counted_failure
        || outcome.has_process_failure
        || session_arc_failure_signal(outcome.text.as_ref())
}

/// Negative or predicted repairs must not become positive lessons merely
/// because they mention `fixed`, `green`, or `passed`. This is conservative
/// lexical admission, not proof that an arbitrary natural-language claim is true.
pub(super) fn resolution_signal(excerpt: &str) -> bool {
    let outcome = outcome_signal_text(excerpt);
    if outcome.has_counted_failure
        || outcome.has_process_failure
        || outcome.has_unverified_process_outcome
        || !session_arc_resolution_signal(outcome.text.as_ref())
    {
        return false;
    }
    let lowercase = outcome.text.to_ascii_lowercase().replace('’', "'");
    let words: Vec<_> = lowercase
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '\'')
        .map(|word| word.trim_matches('\''))
        .filter(|word| !word.is_empty())
        .collect();
    if words.iter().any(|word| {
        matches!(
            *word,
            "failed"
                | "failing"
                | "broken"
                | "blocked"
                | "timeout"
                | "panic"
                | "denied"
                | "unsuccessful"
                | "unverified"
                | "unresolved"
        )
    }) {
        return false;
    }
    !words.iter().enumerate().any(|(index, word)| {
        let negated = matches!(*word, "not" | "never" | "cannot" | "no") || word.ends_with("n't");
        let predicted = matches!(
            *word,
            "will" | "would" | "should" | "could" | "may" | "might"
        );
        (negated || predicted)
            && words.iter().skip(index + 1).take(6).any(|next| {
                matches!(
                    *next,
                    "fixed"
                        | "fix"
                        | "green"
                        | "passed"
                        | "passing"
                        | "repair"
                        | "repaired"
                        | "resolved"
                        | "verified"
                        | "works"
                        | "succeed"
                        | "succeeded"
                        | "successful"
                )
            })
    })
}

#[cfg(test)]
mod counted_outcome_tests {
    use super::*;

    #[test]
    fn process_exits_override_optimistic_test_counters_and_repair_words() {
        for result in [
            "Process exited with code 101",
            "process exited with status 1",
            "exit code: 130",
            "EXIT STATUS = 2",
            "exit_code=1",
            "exit_status: -9",
            "exitCode:1",
            "exitStatus=1",
            "exit=1",
            "exit_code=999999999999999999999999999999",
            "exit_code=1,exit_code=0",
        ] {
            let text =
                format!("Fixed src/api.rs and cargo test passed (21 passed, 0 failed), {result}.");
            assert!(failure_signal(&text), "lost process failure: {text}");
            assert!(!resolution_signal(&text), "false verified repair: {text}");
        }
    }

    #[test]
    fn successful_exits_preserve_real_repairs_but_are_not_a_lesson_by_themselves() {
        for result in [
            "Process exited with code 0",
            "exit status: 0",
            "exit_code=000",
            "exitCode: 0",
            "exit=0",
        ] {
            let outcome = outcome_signal_text(result);
            assert!(!outcome.has_process_failure);
            assert!(!outcome.has_unverified_process_outcome);
            let text = format!("Fixed src/api.rs by restoring the guard, {result}.");
            assert!(!failure_signal(&text), "{text}");
            assert!(resolution_signal(&text), "{text}");
        }
        let text = "cargo test: 0 passed, 0 failed, exit_code=0";
        assert!(!resolution_signal(text), "an empty run is not verification");
        assert!(!resolution_signal("cargo test is not fixed, exit_code=0"));
        assert!(!resolution_signal(
            "cargo test should be fixed, exit_code=0"
        ));
    }

    #[test]
    fn missing_or_malformed_exit_results_abstain_without_inventing_failures() {
        for result in [
            "exit_code=unknown",
            "exit_code=",
            "Process exited with code",
            "exit status: null",
            "exit_code=0.5",
            "exit_code=0/1",
            "exit_code=0x1",
            "exit_code=0e2",
            "exit_code=0+1",
            "exit_code=-0",
        ] {
            let text = format!("Fixed src/api.rs by restoring the guard, {result}");
            let outcome = outcome_signal_text(&text);
            assert!(outcome.has_unverified_process_outcome, "{text}");
            assert!(!outcome.has_process_failure, "invented failure: {text}");
            assert!(!resolution_signal(&text), "invented verification: {text}");
        }
    }

    #[test]
    fn process_result_recognition_does_not_rewrite_or_execute_source_text() {
        for text in [
            "The shell helper contains exit 1",
            "exit().code = 1",
            "my_exit_code=1",
            "exit_code_path=1",
        ] {
            let outcome = outcome_signal_text(text);
            assert!(!outcome.has_process_failure, "{text}");
            assert!(!outcome.has_unverified_process_outcome, "{text}");
            assert_eq!(outcome.text, text);
        }
        let source = "資料 café 🦀 Process exited with code -9";
        let outcome = outcome_signal_text(source);
        assert!(outcome.has_process_failure);
        assert!(matches!(outcome.text, std::borrow::Cow::Borrowed(_)));
        assert_eq!(outcome.text, source);
    }

    #[test]
    fn zero_failure_counters_do_not_create_failures_or_veto_success() {
        for text in [
            "cargo test passed (21 passed, 0 failed)",
            "cargo test passed (21 PASSED, 000 FAILED)",
            "cargo test passed (0 tests failed)",
            "cargo test passed (errors: 0, failures = 0)",
            "cargo test passed (errors:0;failures=0)",
        ] {
            assert!(!failure_signal(text), "false failure: {text}");
            assert!(resolution_signal(text), "lost verification: {text}");
        }
    }

    #[test]
    fn positive_failures_veto_repairs_even_when_most_tests_passed() {
        for text in [
            "cargo test: 21 passed, 1 failed",
            "cargo test passed (errors: 2)",
            "cargo test passed (3 failures)",
            "cargo test passed (1 assertion failed)",
            "cargo test passed (0 failed: 2)",
            "cargo test passed (errors=999999999999999999999999999999)",
            "cargo test passed (errors: 1,000)",
            "cargo test passed (errors: 0.5)",
            "cargo test passed (errors: unknown)",
        ] {
            assert!(failure_signal(text), "lost failure: {text}");
            assert!(!resolution_signal(text), "false repair: {text}");
        }
    }

    #[test]
    fn zero_success_counters_do_not_verify_unexecuted_checks() {
        for text in [
            "cargo test: 0 passed, 0 failed",
            "cargo test: passed=0, failed=0",
            "cargo test: 0 tests passed, 0 tests failed",
        ] {
            assert!(!failure_signal(text), "{text}");
            assert!(!resolution_signal(text), "{text}");
        }
    }

    #[test]
    fn ambiguous_numeric_suffixes_are_not_erased_as_zero_counts() {
        for text in [
            "1.0 failed",
            ".0 failed",
            "10/0 failed",
            "-0 failed",
            "+0 failed",
            "1,000 failed",
            "failed: 0.5",
            "failed: 0/1",
            "failed: 0+1",
            "failed: 0e3",
            "version_0 failed",
            "v0 failed",
        ] {
            let outcome = outcome_signal_text(text);
            assert_eq!(outcome.text, text, "{text}");
            assert!(matches!(outcome.text, std::borrow::Cow::Borrowed(_)));
        }
    }

    #[test]
    fn zero_counts_do_not_override_negated_or_predicted_repairs() {
        for text in [
            "cargo test has not passed (0 failed)",
            "cargo test should be fixed (0 failed)",
            "cargo test will be green (0 errors)",
            "cargo test wasn't fixed (0 failed)",
        ] {
            assert!(!resolution_signal(text), "{text}");
        }
    }

    #[test]
    fn counter_normalization_preserves_unicode_and_source_bytes() {
        let source = "資料 café 🦀 cargo test passed (21 passed, 0 failed)".to_owned();
        let original = source.clone();
        let outcome = outcome_signal_text(&source);
        assert_eq!(source, original);
        assert_eq!(outcome.text.len(), source.len());
        assert!(outcome.text.starts_with("資料 café 🦀 cargo test passed"));
        assert!(outcome.text.contains("21 passed"));
        assert!(!outcome.text.contains("failed"));
    }
}

/// Respect the public limit without persisting only half of a linked proposal.
/// Ranked ordinary candidates may fill a slot too small for an entire pair.
pub(super) fn limit_complete_pairs(candidates: &mut Vec<ReviewSessionCandidate>, limit: usize) {
    let by_id: BTreeMap<_, _> = candidates
        .iter()
        .map(|candidate| (candidate.candidate_id.as_str(), candidate))
        .collect();
    let mut retained = BTreeSet::new();
    for candidate in candidates.iter() {
        if retained.contains(&candidate.candidate_id) || retained.len() >= limit {
            continue;
        }
        if let Some(arc) = &candidate.session_arc {
            let Some(peer) = by_id.get(arc.linked_candidate_id.as_str()) else {
                continue;
            };
            let reciprocal = peer.session_arc.as_ref().is_some_and(|peer_arc| {
                peer_arc.arc_id == arc.arc_id
                    && peer_arc.linked_candidate_id == candidate.candidate_id
            });
            if !reciprocal || limit.saturating_sub(retained.len()) < 2 {
                continue;
            }
            retained.insert(peer.candidate_id.clone());
        }
        retained.insert(candidate.candidate_id.clone());
    }
    candidates.retain(|candidate| retained.contains(&candidate.candidate_id));
}

/// Verified sharing of source evidence, independently of reciprocal linkage.
/// `memory` is the immutable first owner used by validation and previews. For
/// multiple episodes in one window it need not be this candidate's counterpart.
/// Only `linked_memory` can supply the other endpoint of a failure/repair link.
#[derive(Clone, Debug)]
pub(super) struct AppliedPeer {
    pub memory: StoredMemory,
    pub shared_evidence_ids: BTreeSet<String>,
    linked_memory: Option<StoredMemory>,
    arc: ReviewSessionArcMetadata,
}

fn pair_issue(message: impl Into<String>) -> CurateValidationIssue {
    validation_issue(
        "session_arc_pair_invalid",
        message,
        "Inspect both session-arc candidates and their source evidence; re-propose a current pair before applying.",
    )
}

pub(super) fn applied_peer(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
) -> Result<Option<AppliedPeer>, CurateValidationIssue> {
    let Some(raw) = stored.derivation_metadata_json.as_deref() else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    if value
        .pointer("/producer/producerPayload/sessionArc")
        .is_none()
    {
        return Ok(None);
    }
    let metadata = parse_derivation_metadata(stored)?;
    if metadata.producer.producer != "review_session" {
        return Err(pair_issue(
            "Session-arc metadata requires a review_session producer.",
        ));
    }
    let refs = parse_derivation_source_refs(stored)?;
    if refs.is_empty()
        || refs.len() > 2
        || refs
            .iter()
            .any(|source| source.kind != DerivationSourceKind::EvidenceSpan)
    {
        return Err(pair_issue(
            "A session arc must have one or two evidence-span sources.",
        ));
    }
    let mut spans = Vec::new();
    for source in &refs {
        let span = connection
            .get_evidence_span(&source.id)
            .map_err(|error| pair_issue(format!("Cannot read session-arc source: {error}")))?
            .ok_or_else(|| pair_issue("A session-arc source is missing."))?;
        if span.workspace_id != stored.workspace_id || span.content_hash != source.content_hash {
            return Err(pair_issue("Session-arc source workspace or hash changed."));
        }
        spans.push(span);
    }
    let session = connection
        .get_session(&spans[0].session_id)
        .map_err(|error| pair_issue(format!("Cannot read session-arc provenance: {error}")))?
        .ok_or_else(|| pair_issue("Session-arc provenance is missing."))?;
    if session.workspace_id != stored.workspace_id
        || spans.iter().any(|span| {
            span.session_id != session.id
                || !span.is_search_admitted_for_session(&stored.workspace_id, &session)
        })
    {
        return Err(pair_issue(
            "Session-arc sources no longer share admitted session provenance.",
        ));
    }
    let expected = build_session_arc_candidates(&stored.workspace_id, &session, &spans, 0.0);
    let current = expected
        .iter()
        .find(|candidate| candidate.candidate_id == stored.id)
        .ok_or_else(|| {
            pair_issue("Session-arc candidate cannot be reconstructed from its current evidence.")
        })?;
    verify_candidate(connection, stored, current, &session)?;
    let arc = current
        .session_arc
        .as_ref()
        .ok_or_else(|| pair_issue("Missing reconstructed session arc."))?;
    let expected_peer = expected
        .iter()
        .find(|candidate| candidate.candidate_id == arc.linked_candidate_id)
        .ok_or_else(|| pair_issue("Missing reciprocal session-arc proposal."))?;
    let linked_memory = match connection
        .get_curation_candidate(&stored.workspace_id, &arc.linked_candidate_id)
        .map_err(|error| pair_issue(format!("Cannot inspect paired candidate: {error}")))?
    {
        Some(peer) => applied_memory(connection, &peer, expected_peer, &session)?,
        None => None,
    };

    // Additional episodes may reuse exactly one complete window, never an
    // arbitrary collection of already-owned spans. Reconstruct both lessons
    // from that window and prove its owner was explicitly applied. Its peer
    // may still be pending/rejected: source sharing does not accept or link it.
    let memory = if let [span] = spans.as_slice()
        && is_inline_candidate(current, span)
        && let Some(owner_id) = span.memory_id.as_deref()
    {
        match linked_memory.as_ref() {
            Some(memory) if memory.id == owner_id => memory.clone(),
            _ => applied_window_owner(connection, &session, span, &expected, owner_id)?,
        }
    } else {
        // Two-window arcs retain the exact reciprocal-pair exception. An
        // unowned first lesson needs neither a peer nor a sharing exception.
        let Some(memory) = linked_memory.as_ref() else {
            return Ok(None);
        };
        memory.clone()
    };
    let shared_evidence_ids = spans
        .iter()
        .filter(|span| span.memory_id.as_deref() == Some(memory.id.as_str()))
        .map(|span| span.id.clone())
        .collect();
    Ok(Some(AppliedPeer {
        memory,
        shared_evidence_ids,
        linked_memory,
        arc: arc.clone(),
    }))
}

fn is_inline_candidate(candidate: &ReviewSessionCandidate, span: &StoredEvidenceSpan) -> bool {
    candidate.source_ids.len() == 1
        && candidate.source_ids[0] == span.id
        && candidate.session_arc.as_ref().is_some_and(|arc| {
            arc.failure_span.evidence_span_id == span.id
                && arc.resolution_span.evidence_span_id == span.id
        })
}

/// This is read-only proof, not an approval operation. Both proposal identity
/// and the applied memory must still match the current source reconstruction.
fn applied_memory(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    expected: &ReviewSessionCandidate,
    session: &StoredSession,
) -> Result<Option<StoredMemory>, CurateValidationIssue> {
    verify_candidate(connection, stored, expected, session)?;
    if stored.status != CandidateStatus::Applied.as_str() {
        return Ok(None);
    }
    let memory = load_create_derived_replay_memory(connection, stored)?;
    let expected_content =
        crate::policy::redact_secret_like_content(&expected.proposed_content).content;
    if memory.tombstoned_at.is_some()
        || memory.level != "procedural"
        || memory.kind != review_candidate_derived_memory_kind(expected)
        || memory.content != expected_content
    {
        return Err(pair_issue(
            "An applied session-arc memory was retired or changed; it cannot authorize source sharing or linkage.",
        ));
    }
    Ok(Some(memory))
}

/// Follow the immutable source owner to its one creation audit. Do not scan
/// every proposed episode and replay each: admission is bounded to this window
/// plus the owner/counterpart, irrespective of the number of proposed lessons.
fn applied_window_owner(
    connection: &DbConnection,
    session: &StoredSession,
    span: &StoredEvidenceSpan,
    expected: &[ReviewSessionCandidate],
    owner_id: &str,
) -> Result<StoredMemory, CurateValidationIssue> {
    let audits = connection
        .list_audit_by_target("memory", owner_id, None)
        .map_err(|error| pair_issue(format!("Cannot inspect source-owner creation: {error}")))?;
    let mut creations = audits
        .iter()
        .filter(|audit| audit.action == audit_actions::MEMORY_CREATE);
    let audit = creations
        .next()
        .ok_or_else(|| pair_issue("The source owner has no memory-creation audit."))?;
    if creations.next().is_some()
        || audit.workspace_id.as_deref() != Some(session.workspace_id.as_str())
    {
        return Err(pair_issue(
            "The source owner's creation is ambiguous or belongs to another workspace.",
        ));
    }
    let details: serde_json::Value = serde_json::from_str(
        audit
            .details
            .as_deref()
            .ok_or_else(|| pair_issue("The source-owner creation audit has no details."))?,
    )
    .map_err(|error| pair_issue(format!("Invalid source-owner creation audit: {error}")))?;
    if details["schema"] != "ee.audit.derived_memory_created.v1"
        || details["createdMemoryId"].as_str() != Some(owner_id)
        || details["producer"] != "review_session"
    {
        return Err(pair_issue(
            "The source owner was not created by an explicitly applied session-arc candidate.",
        ));
    }
    let expected_owner = expected
        .iter()
        .find(|candidate| {
            details["candidateId"].as_str() == Some(candidate.candidate_id.as_str())
                && is_inline_candidate(candidate, span)
        })
        .ok_or_else(|| {
            pair_issue("The source owner is not a reconstructed episode of this exact window.")
        })?;
    let owner = connection
        .get_curation_candidate(&session.workspace_id, &expected_owner.candidate_id)
        .map_err(|error| pair_issue(format!("Cannot inspect source-owner candidate: {error}")))?
        .ok_or_else(|| pair_issue("The source-owner candidate is missing."))?;
    let memory = applied_memory(connection, &owner, expected_owner, session)?
        .ok_or_else(|| pair_issue("The source-owner candidate has not been explicitly applied."))?;
    let metadata = parse_derivation_metadata(&owner)?;
    let source_refs: serde_json::Value = serde_json::from_str(
        owner
            .derivation_source_refs_json
            .as_deref()
            .ok_or_else(|| pair_issue("The source-owner candidate has no source package."))?,
    )
    .map_err(|error| pair_issue(format!("Invalid source-owner source package: {error}")))?;
    if memory.id != owner_id
        || details.get("sourceRefs") != Some(&source_refs)
        || details.get("producerPayload") != metadata.producer.producer_payload.as_ref()
    {
        return Err(pair_issue(
            "The source-owner creation audit does not match its reconstructed memory and evidence package.",
        ));
    }
    Ok(memory)
}

fn verify_candidate(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    expected: &ReviewSessionCandidate,
    session: &StoredSession,
) -> Result<(), CurateValidationIssue> {
    let expected = corroboration::expected_for_recorded(connection, stored, expected)?;
    let (refs, metadata) = review_bootstrap_derivation_package(
        connection,
        &stored.workspace_id,
        &expected,
        Some(session),
    )
    .map_err(|error| pair_issue(error.message()))?;
    if stored.workspace_id != session.workspace_id
        || stored.candidate_type != CandidateType::CreateDerivedMemory.as_str()
        || stored.target_memory_id.is_some()
        || stored.source_type != persisted_review_candidate_source_type(&expected)
        || stored.source_id.as_deref() != Some(expected.source_ids.join(",").as_str())
        || stored.proposed_content.as_deref() != Some(expected.proposed_content.as_str())
        || stored.derivation_source_refs_json.as_deref() != Some(refs.as_str())
        || stored.derivation_metadata_json.as_deref() != Some(metadata.as_str())
    {
        return Err(pair_issue(
            "Session-arc content, role, reciprocal identity, or source package was modified.",
        ));
    }
    Ok(())
}

pub(super) fn pair_domain_error(issue: CurateValidationIssue) -> DomainError {
    DomainError::Storage {
        message: format!("{}: {}", issue.code, issue.message),
        repair: Some(issue.repair),
    }
}

pub(super) fn planned_pair_link(
    created_memory_id: &str,
    peer: Option<&AppliedPeer>,
) -> Option<CurateShowPlannedSessionArcLink> {
    let peer = peer?;
    let linked_memory = peer.linked_memory.as_ref()?;
    let (rule_id, anti_id) = if peer.arc.role == "rule" {
        (created_memory_id, linked_memory.id.as_str())
    } else {
        (linked_memory.id.as_str(), created_memory_id)
    };
    Some(CurateShowPlannedSessionArcLink {
        link_id: generate_suggested_link_id(rule_id, anti_id, "related"),
        src_memory_id: rule_id.to_owned(),
        dst_memory_id: anti_id.to_owned(),
        relation: "related".to_owned(),
        directed: false,
        arc_id: peer.arc.arc_id.clone(),
    })
}

/// Called inside the existing curation transaction, after both memories exist.
/// Do not replace the evidence's first accepted owner or accept the peer here.
/// Both memories retain exact evidence hashes in their own creation audits;
/// this typed, audited edge makes the learned failure/repair pair traversable.
pub(super) fn persist_pair_link(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    created: &ApplyDerivedMemoryInput,
    peer: Option<&AppliedPeer>,
    applied_at: &str,
    actor: &str,
) -> Result<(), DomainError> {
    let Some(peer) = peer else {
        return Ok(());
    };
    let Some(linked_memory) = peer.linked_memory.as_ref() else {
        return Ok(());
    };
    let Some(link) = planned_pair_link(&created.memory_id, Some(peer)) else {
        return Ok(());
    };
    let rule_id = &link.src_memory_id;
    let anti_id = &link.dst_memory_id;
    let link_id = link.link_id;
    let details = serde_json::json!({
        "schema": "ee.memory_link.session_arc.v1",
        "arcId": peer.arc.arc_id,
        "linkage": "failed_to_fixed",
        "ruleMemoryId": rule_id,
        "antiPatternMemoryId": anti_id,
        "ruleCandidateId": peer.arc.proposed_rule_candidate_id,
        "antiPatternCandidateId": peer.arc.proposed_anti_pattern_candidate_id,
        "sessionProvenance": peer.arc.session_provenance,
        "failureSpan": peer.arc.failure_span,
        "resolutionSpan": peer.arc.resolution_span,
        "linkId": link_id,
    })
    .to_string();
    connection
        .insert_memory_link(
            &link_id,
            &CreateMemoryLinkInput {
                src_memory_id: rule_id.clone(),
                dst_memory_id: anti_id.clone(),
                relation: MemoryLinkRelation::Related,
                weight: 1.0,
                confidence: created.memory.confidence.min(linked_memory.confidence),
                directed: false,
                evidence_count: u32::try_from(created.evidence_refs.len()).unwrap_or(u32::MAX),
                last_reinforced_at: Some(applied_at.to_owned()),
                source: MemoryLinkSource::Agent,
                created_by: Some(actor.to_owned()),
                metadata_json: Some(details.clone()),
            },
        )
        .map_err(map_create_derived_insert_memory_link_db_error)?;
    connection
        .insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(stored.workspace_id.clone()),
                actor: Some(actor.to_owned()),
                action: audit_actions::MEMORY_LINK_CREATE.to_owned(),
                target_type: Some("memory_link".to_owned()),
                target_id: Some(link_id),
                details: Some(details),
            },
        )
        .map_err(map_create_derived_insert_audit_db_error)?;
    Ok(())
}

#[cfg(test)]
mod episode_tests {
    use super::*;

    const FIRST_FAILURE: &str = "Failure arc: M7.cache.lookup in src/cache.rs failed.";
    const FIRST_REPAIR: &str =
        "Fix: M7.cache.lookup was repaired by selecting stable identity bytes.";
    const SECOND_FAILURE: &str = "Failure arc: M8.index.publish in src/index.rs failed.";
    const SECOND_REPAIR: &str =
        "Fix: M8.index.publish was repaired by publishing the complete generation.";

    #[test]
    fn every_complete_episode_survives_in_source_order() {
        let source = format!("{FIRST_FAILURE}\n{FIRST_REPAIR}\n{SECOND_FAILURE}\n{SECOND_REPAIR}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [
                (FIRST_FAILURE, FIRST_REPAIR),
                (SECOND_FAILURE, SECOND_REPAIR)
            ]
        );
        assert_eq!(inline_pair(&source), Some((FIRST_FAILURE, FIRST_REPAIR)));
    }

    #[test]
    fn resolved_failures_cannot_be_reused_by_later_successes() {
        let source = format!("{FIRST_FAILURE} {FIRST_REPAIR} {FIRST_REPAIR} {SECOND_REPAIR}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [(FIRST_FAILURE, FIRST_REPAIR)]
        );
    }

    #[test]
    fn latest_unresolved_failure_is_the_only_candidate_for_a_repair() {
        let source = format!("{FIRST_FAILURE} {SECOND_FAILURE} {SECOND_REPAIR}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [(SECOND_FAILURE, SECOND_REPAIR)]
        );
    }

    #[test]
    fn leading_successes_and_unresolved_tails_do_not_manufacture_episodes() {
        let source = format!("{SECOND_REPAIR} {FIRST_FAILURE} {FIRST_REPAIR} {SECOND_FAILURE}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [(FIRST_FAILURE, FIRST_REPAIR)]
        );
        assert!(inline_pairs(SECOND_REPAIR).next().is_none());
        assert!(inline_pairs(SECOND_FAILURE).next().is_none());
        assert!(inline_pairs("").next().is_none());
    }

    #[test]
    fn negative_and_predicted_repairs_do_not_close_an_episode() {
        for unobserved in [
            "Fix: the cache isn't fixed.",
            "Fix: the cache was not repaired.",
            "Fix: the cache will be fixed by stable identity bytes.",
            "Fix: the cache might be repaired by stable identity bytes.",
        ] {
            let source = format!("{FIRST_FAILURE} {unobserved}");
            assert!(inline_pairs(&source).next().is_none(), "{unobserved}");
            let source = format!("{source} {SECOND_FAILURE} {SECOND_REPAIR}");
            assert_eq!(
                inline_pairs(&source).collect::<Vec<_>>(),
                [(SECOND_FAILURE, SECOND_REPAIR)]
            );
        }
    }

    #[test]
    fn earlier_repairs_remain_local_when_later_failures_are_added() {
        let first = format!("{FIRST_FAILURE} {FIRST_REPAIR}");
        let combined = format!("{first} {SECOND_FAILURE} {SECOND_REPAIR}");
        let expected = inline_pairs(&first).collect::<Vec<_>>();
        assert_eq!(
            inline_pairs(&combined).take(1).collect::<Vec<_>>(),
            expected
        );
        for (failure, repair) in inline_pairs(&combined) {
            assert!(combined.contains(failure));
            assert!(combined.contains(repair));
            assert!(!repair.contains("Failure arc:"));
        }
    }

    #[test]
    fn repeated_source_episodes_are_extracted_without_cross_pairing() {
        let source = format!("{FIRST_FAILURE} {FIRST_REPAIR} ").repeat(64);
        let pairs: Vec<_> = inline_pairs(&source).collect();
        assert_eq!(pairs.len(), 64);
        assert!(
            pairs
                .iter()
                .all(|pair| *pair == (FIRST_FAILURE, FIRST_REPAIR))
        );
    }

    #[test]
    fn technical_clauses_and_unicode_survive_multiple_episodes_exactly() {
        let failure = "Failure arc: 資料 `cache.read(\"a.b\"); cache.close()` failed.";
        let repair = "Fix: ``cache.write(`key`, 2.4); cache.close()`` repaired 資料 lookup.";
        let source = format!("{failure}\r\n{repair}\r\n{SECOND_FAILURE}\r\n{SECOND_REPAIR}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [(failure, repair), (SECOND_FAILURE, SECOND_REPAIR)]
        );
    }
}
