//! Borrowed clause boundaries for technical failure/repair evidence.
//!
//! A period inside a version, filename or configuration key is not a sentence
//! boundary. Delimiters inside Markdown code are source text, not prose
//! ordering. Fenced blocks and inline spans have different closing rules.
//! Preserve all bytes; this splitter never rewrites quoted commands or
//! fabricates transcript line offsets.
//! Bare outcome reports belong to the preceding observation. A failed retry
//! must not become a repair merely because its counters followed a newline.

pub(super) fn split(excerpt: &str) -> impl Iterator<Item = &str> {
    let raw: Vec<_> = split_raw(excerpt).collect();
    let mut grouped = Vec::with_capacity(raw.len());
    let mut first = 0;
    let mut byte_start = 0;
    while first < raw.len() {
        let mut end = first + 1;
        let mut bytes = raw[first].len();
        let mut veto = false;
        while let Some(part) = raw.get(end) {
            if !part.trim().is_empty() {
                let Some(failed) = outcome_report_veto(part) else {
                    break;
                };
                veto |= failed;
            }
            // Consume separators even when no report follows. Otherwise a
            // long blank run would be scanned again for every blank clause.
            bytes += part.len();
            end += 1;
        }
        if veto {
            // One contiguous borrowed observation lets the existing outcome
            // classifier see every contradictory report. Do not synthesize a
            // failure word, copy source text, or invent an evidence locator.
            grouped.push(&excerpt[byte_start..byte_start + bytes]);
        } else {
            // Successful observations retain their exact historical clauses,
            // candidate IDs and excerpts, including whitespace-only slices.
            grouped.extend_from_slice(&raw[first..end]);
        }
        first = end;
        byte_start += bytes;
    }
    grouped.into_iter()
}

/// A separate CASS window can continue a result report, but only when every
/// nonempty clause is a report. Inspect the complete window even after a veto:
/// a failed prefix must not conceal a later independent command or observation.
/// Callers still establish source admission and physical adjacency themselves.
pub(super) fn report_window_veto(excerpt: &str) -> Option<bool> {
    let mut parts = split_raw(excerpt)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .peekable();
    parts.peek()?;
    parts.try_fold(false, |veto, part| {
        outcome_report_veto(part).map(|failed| veto || failed)
    })
}

/// Only bare reports extend an observation. A command, named resource,
/// qualification or quoted example is a new clause, never a result trailer.
/// This recognizes framing; the existing outcome classifier owns the verdict.
fn outcome_report_veto(excerpt: &str) -> Option<bool> {
    #[cfg(test)]
    OUTCOME_TRAILER_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(excerpt.len())));
    let text = excerpt.trim();
    if let Some(veto) = super::process_report_veto(text) {
        return Some(veto || super::outcome_signal_text(text).has_counted_failure);
    }
    let lower = text.to_ascii_lowercase();
    if elapsed_report(&lower) {
        return Some(false);
    }
    let lower = counter_report_body(&lower)?;
    if !suite_counter_report(lower).unwrap_or_else(|| bare_counter_report(lower)) {
        return None;
    }
    let outcome = super::outcome_signal_text(text);
    Some(
        outcome.has_counted_failure
            || outcome
                .text
                .split(|ch: char| !ch.is_ascii_alphabetic())
                .any(|word| {
                    word.eq_ignore_ascii_case("failed") || word.eq_ignore_ascii_case("failing")
                }),
    )
}

/// Legacy bare counters keep their existing admission grammar. Runner headers
/// use a separate, complete grammar below; accepting `total` or `|` everywhere
/// would turn unrelated prose and shell expressions into result trailers.
fn bare_counter_report(lower: &str) -> bool {
    if !lower.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || ch.is_whitespace()
            || matches!(
                ch,
                ',' | ';' | ':' | '=' | '.' | '(' | ')' | '[' | ']' | '+' | '-'
            )
    }) {
        return false;
    }
    let mut has_count = false;
    let mut has_result = false;
    for word in lower.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if word.bytes().all(|byte| byte.is_ascii_digit()) {
            has_count = true;
            continue;
        }
        match word {
            "failed" | "failing" | "failure" | "failures" | "error" | "errors" | "passed"
            | "passing" | "succeeded" | "successes" | "ok" | "ignored" | "skipped" | "measured"
            | "filtered" => has_result = true,
            "test" | "tests" | "check" | "checks" | "assertion" | "assertions" | "result"
            | "results" | "out" | "unknown" | "null" => {}
            _ => return false,
        }
    }
    let result_header = lower.starts_with("test result:") || lower.starts_with("test results:");
    if !has_result || !(has_count || result_header || lower.contains([':', '='])) {
        return false;
    }
    true
}

/// Labeled Jest/Vitest summaries are counters, not independent observations.
/// Require the complete header and every counter field. In particular, never
/// skip a failed prefix's trailing command, filename, quotation or annotation.
fn suite_counter_report(text: &str) -> Option<bool> {
    let body = ["test suites", "test files", "snapshots", "tests"]
        .iter()
        .find_map(|header| {
            let rest = text.strip_prefix(*header)?;
            if let Some(rest) = rest.strip_prefix(':') {
                Some(rest.trim())
            } else {
                rest.starts_with(char::is_whitespace).then(|| rest.trim())
            }
        });
    let mut body = body?;
    // Preserve legacy label-first counters such as `tests failed: 1` and
    // `tests: failures=1`; only quantity-first Tests summaries use this grammar.
    if text.starts_with("tests") && !body.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    // Vitest uses a parenthesized total after its pipe-separated counters.
    // The optional total is a quantity only, not an arbitrary parenthetical.
    if let Some((counts, total)) = body.rsplit_once('(') {
        let Some(total) = total.strip_suffix(')') else {
            return Some(false);
        };
        if !decimal_count(total.trim()) {
            return Some(false);
        }
        body = counts.trim_end();
    }
    if body.is_empty() {
        return Some(false);
    }
    Some(body.split([',', '|']).all(|field| {
        let mut words = field.split_whitespace();
        let Some(count) = words.next() else {
            return false;
        };
        decimal_count(count)
            && matches!(
                words.next(),
                Some("failed" | "passed" | "skipped" | "pending" | "todo" | "total")
            )
            && words.next().is_none()
    }))
}

fn decimal_count(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Strip only recognized test-run framing for classification. A duration is
/// not a resource name: `1 failed in 0.03s` qualifies the preceding attempt,
/// whereas `1 failed in src/other.rs` must remain an independent observation.
/// Returned slices never replace the original evidence or candidate excerpts.
fn counter_report_body(text: &str) -> Option<&str> {
    let mut body = text.trim_end_matches(['.', ';']).trim();
    if body.starts_with("===") {
        if !body.ends_with("===") {
            return None;
        }
        body = body.trim_matches('=').trim();
    }
    if let Some((counts, elapsed)) = body.rsplit_once(" in ")
        && elapsed_duration(elapsed)
    {
        body = counts.trim_end();
    }
    Some(body)
}

fn elapsed_report(text: &str) -> bool {
    let text = text.trim_end_matches(['.', ';']).trim();
    if let Some(value) = text.strip_prefix("finished in ") {
        return elapsed_duration(value);
    }
    if let Some(value) = text.strip_prefix("time:") {
        // Jest may append its estimate. Both values must be complete durations;
        // neither arbitrary prose nor an independent command can be skipped.
        return match value.split_once(", estimated ") {
            Some((actual, estimated)) => elapsed_duration(actual) && elapsed_duration(estimated),
            None => elapsed_duration(value),
        };
    }
    if let Some(value) = text.strip_prefix("start at ") {
        return runner_clock(value.trim());
    }
    if let Some(value) = text.strip_prefix("duration")
        && value.starts_with(char::is_whitespace)
    {
        return runner_duration(value.trim());
    }
    if text == "ran all test suites" {
        return true;
    }
    // Python unittest writes the run count/timing before its final FAILED
    // report. Do not let that neutral line detach the actual outcome.
    let Some((run, elapsed)) = text
        .strip_prefix("ran ")
        .and_then(|body| body.split_once(" in "))
    else {
        return false;
    };
    let mut words = run.split_whitespace();
    let Some(count) = words.next() else {
        return false;
    };
    count.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(words.next(), Some("test" | "tests"))
        && words.next().is_none()
        && elapsed_duration(elapsed)
}

/// Accept only the complete HH:MM:SS footer, never a path, date or message.
fn runner_clock(text: &str) -> bool {
    let mut fields = text.split(':');
    for limit in [24_u8, 60, 60] {
        let Some(field) = fields.next() else {
            return false;
        };
        if field.len() != 2
            || !decimal_count(field)
            || !field.parse::<u8>().is_ok_and(|value| value < limit)
        {
            return false;
        }
    }
    fields.next().is_none()
}

/// Vitest's duration footer may include phase timings. Validate every phase;
/// unknown keys, repeated keys and trailing prose are not neutral metadata.
/// Both older collect/prepare and current import labels are explicit here.
fn runner_duration(text: &str) -> bool {
    let Some((elapsed, details)) = text.split_once('(') else {
        return elapsed_duration(text);
    };
    let Some(details) = details.strip_suffix(')') else {
        return false;
    };
    if !elapsed_duration(elapsed) {
        return false;
    }
    let mut seen = 0_u8;
    details.split(',').all(|detail| {
        let Some((phase, duration)) = detail.trim().split_once(char::is_whitespace) else {
            return false;
        };
        let bit = match phase {
            "transform" => 1,
            "setup" => 2,
            "collect" => 4,
            "import" => 8,
            "tests" => 16,
            "environment" => 32,
            "prepare" => 64,
            _ => return false,
        };
        if seen & bit != 0 {
            return false;
        }
        seen |= bit;
        elapsed_duration(duration)
    })
}

/// A finite, unsigned decimal duration with an explicit unit. Do not parse a
/// float, round a value, accept a path, or accept trailing diagnostic prose.
fn elapsed_duration(text: &str) -> bool {
    let text = text.trim();
    let Some(number) = text.strip_suffix("ms").or_else(|| text.strip_suffix('s')) else {
        return false;
    };
    let number = number.trim_end();
    let mut parts = number.split('.');
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    parts.next().is_some_and(digits) && parts.next().is_none_or(digits) && parts.next().is_none()
}

fn split_raw(excerpt: &str) -> impl Iterator<Item = &str> {
    let mut characters = excerpt.char_indices().peekable();
    let mut start = 0;
    let mut line_start = 0;
    let mut code_delimiter = 0;
    let mut fence: Option<(char, usize)> = None;
    std::iter::from_fn(move || {
        while let Some((position, ch)) = characters.next() {
            let end = position + ch.len_utf8();
            if ch == '\n' {
                line_start = end;
            }
            if matches!(ch, '`' | '~') && !escaped(excerpt, position) {
                let mut width = 1;
                while characters.peek().is_some_and(|(_, next)| *next == ch) {
                    let _ = characters.next();
                    width += 1;
                }
                let at_fence_position = fence_indent(&excerpt[line_start..position]);
                // A line has at most one possible fence position. Inspecting
                // its tail for every inline delimiter makes a long transcript
                // quadratic; short-circuit before any tail scan instead.
                let rest = if at_fence_position && width >= 3 {
                    line_tail(excerpt, position + width)
                } else {
                    ""
                };
                if let Some((marker, minimum_width)) = fence {
                    // Code may itself contain backticks, quotes, or apparent
                    // failure/repair markers. Only a fence on its own line can
                    // restore prose boundaries; a longer closer is valid.
                    if ch == marker
                        && width >= minimum_width
                        && at_fence_position
                        && rest.trim_matches([' ', '\t', '\r']).is_empty()
                    {
                        fence = None;
                    }
                    continue;
                }
                if code_delimiter == 0
                    && width >= 3
                    && at_fence_position
                    && (ch == '~' || !rest.contains('`'))
                {
                    fence = Some((ch, width));
                    continue;
                }
                if ch == '`' {
                    if code_delimiter == 0 {
                        code_delimiter = width;
                    } else if width == code_delimiter {
                        code_delimiter = 0;
                    }
                    continue;
                }
            }
            if fence.is_some() || code_delimiter != 0 {
                continue;
            }
            let boundary = matches!(ch, '\n' | ';') || (ch == '.' && sentence_period(excerpt, end));
            if boundary {
                let part = &excerpt[start..end];
                start = end;
                return Some(part);
            }
        }
        if start < excerpt.len() {
            let part = &excerpt[start..];
            start = excerpt.len();
            Some(part)
        } else {
            None
        }
    })
}

fn fence_indent(prefix: &str) -> bool {
    prefix.len() <= 3 && prefix.bytes().all(|byte| byte == b' ')
}

#[cfg(test)]
std::thread_local! {
    // Measure actual tail inspections, not elapsed time or a replica parser.
    static FENCE_TAIL_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static OUTCOME_TRAILER_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn line_tail(text: &str, start: usize) -> &str {
    let tail = text[start..].split('\n').next().unwrap_or("");
    #[cfg(test)]
    FENCE_TAIL_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(tail.len())));
    tail
}

fn escaped(text: &str, position: usize) -> bool {
    text.as_bytes()[..position]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn sentence_period(text: &str, end: usize) -> bool {
    if text[end..]
        .chars()
        .next()
        .is_some_and(|next| !next.is_whitespace())
    {
        return false;
    }
    let word = text[..end]
        .rsplit_once(char::is_whitespace)
        .map_or(&text[..end], |(_, word)| word);
    // These common technical abbreviations introduce the rest of a clause.
    !["e.g.", "i.e.", "vs."]
        .iter()
        .any(|item| word.eq_ignore_ascii_case(item))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAILURE: &str = "Failure arc: M7.cache.lookup in src/cache.rs failed with version 2.4.1.";
    const REPAIR: &str =
        "Fix: M7.cache.lookup in src/cache.rs was repaired by selecting stable identity bytes.";

    #[test]
    fn filenames_versions_and_configuration_keys_survive_in_both_halves() {
        let text = format!("{FAILURE} {REPAIR}");
        assert_eq!(
            split(&text).map(str::trim).collect::<Vec<_>>(),
            [FAILURE, REPAIR]
        );
        assert_eq!(super::super::inline_pair(&text), Some((FAILURE, REPAIR)));
        assert!(
            !text
                .split_inclusive(['\n', ';', '.'])
                .any(|part| part == FAILURE)
        );
    }

    #[test]
    fn code_delimiters_do_not_fragment_the_failure_or_repair() {
        let failure = "Failure arc: `cache.read(\"a.b\"); cache.close()` failed.";
        let repair = "Fix: ``cache.write(`key`, 2.4); cache.close()`` repaired the lookup.";
        let text = format!("{failure}\n{repair}");
        let parts: Vec<_> = split(&text)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        assert_eq!(parts, [failure, repair]);
        assert_eq!(super::super::inline_pair(&text), Some((failure, repair)));
    }

    #[test]
    fn technical_abbreviations_remain_with_their_explanations() {
        let failure = "Failure arc: cache failed for aliases, e.g. renamed branches.";
        let repair = "Fix: stable identity bytes repaired alias lookup, i.e. display labels no longer selected keys.";
        let text = format!("{failure} {repair}");
        assert_eq!(super::super::inline_pair(&text), Some((failure, repair)));
    }

    #[test]
    fn unicode_and_all_delimiters_roundtrip_without_losing_a_single_byte() {
        let pieces = [
            "資料", "🦀", "x.y", "e.g. ", ". ", ";", "\r\n", "`", "``", "\\`", "",
        ];
        let mut cases = 0;
        for left in pieces {
            for middle in pieces {
                for right in pieces {
                    let text = format!("{left}{middle}{right}");
                    let parts: Vec<_> = split(&text).collect();
                    assert!(parts.iter().all(|part| !part.is_empty()));
                    assert_eq!(parts.concat(), text);
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 1331);
    }

    #[test]
    fn code_fences_and_escaped_backticks_keep_their_exact_source_boundaries() {
        let code = "```text\ncache.value; version 2.4.1\n```";
        let text = format!("{code}\n{REPAIR}");
        let parts: Vec<_> = split(&text).map(str::trim).collect();
        assert_eq!(parts, [code, REPAIR]);
        let text = format!("Failure arc: escaped \\` label failed. {REPAIR}");
        assert_eq!(split(&text).count(), 2);
        assert_eq!(split("unfinished ` code; no period. yet").count(), 1);
    }

    #[test]
    fn actual_newline_and_semicolon_ordering_still_supports_plain_lessons() {
        let failure = "Failure arc: cache failed";
        let repair = "Fix: stable keys repaired the cache";
        for separator in ["\n", ";"] {
            let text = format!("{failure}{separator}{repair}");
            let (actual_failure, actual_repair) =
                super::super::inline_pair(&text).expect("ordered pair");
            assert_eq!(actual_failure.trim_end_matches(';'), failure);
            assert_eq!(actual_repair, repair);
        }
    }

    #[test]
    fn negated_and_predicted_repairs_cannot_supply_success_evidence() {
        for repair in [
            "Fix: the cache isn't fixed.",
            "Fix: the cache wasn’t repaired.",
            "Fix: stable keys haven't fixed the cache.",
            "Fix: the retry didn't succeed after the cache was repaired.",
            "Fix: the cache is not yet fixed.",
            "Fix: the cache will be fixed by stable keys.",
            "Fix: stable keys might have repaired the cache.",
            "Fix: stable keys should leave the cache repaired.",
            "Fix: the cache could be fixed by stable keys.",
        ] {
            assert!(!super::super::resolution_signal(repair), "{repair}");
            assert!(super::super::inline_pair(&format!("{FAILURE} {repair}")).is_none());
        }
    }

    #[test]
    fn observed_repairs_remain_eligible_with_and_without_a_prior_failed_attempt() {
        for repair in [
            REPAIR,
            "Fix: the cache is now fixed and verification passed.",
            "Fix: stable keys repaired the cache and the retry succeeded.",
        ] {
            assert!(super::super::resolution_signal(repair), "{repair}");
            assert_eq!(
                super::super::inline_pair(&format!("{FAILURE} {repair}")),
                Some((FAILURE, repair))
            );
        }
        let text = format!("{FAILURE} Failure arc: M7 cache patch failed. {REPAIR}");
        let pair = super::super::inline_pair(&text).expect("later observed repair");
        assert_eq!(pair.1, REPAIR);
    }

    #[test]
    fn session_arc_tilde_fences_keep_diagnostic_clauses_together() {
        let code = "~~~text\nFailure arc: cargo test failed;\nFix: a quoted example passed.\n~~~";
        let text = format!("{code}\n{REPAIR}");
        assert_eq!(
            split(&text).map(str::trim).collect::<Vec<_>>(),
            [code, REPAIR]
        );
        assert_eq!(split(&text).collect::<String>(), text);
    }

    #[test]
    fn session_arc_backticks_inside_a_fence_do_not_close_the_diagnostic_block() {
        let code = "```rust\nlet quoted = \"```\";\nFailure arc: quoted example failed.\nFix: quoted example passed.\n```";
        let text = format!("{code}\n{REPAIR}");
        assert_eq!(
            split(&text).map(str::trim).collect::<Vec<_>>(),
            [code, REPAIR]
        );
        assert_eq!(split(&text).collect::<String>(), text);
    }

    #[test]
    fn session_arc_longer_closing_fences_restore_later_prose_boundaries() {
        for marker in ['`', '~'] {
            for opening_width in 3..=6 {
                for closing_width in opening_width..=7 {
                    let opening = marker.to_string().repeat(opening_width);
                    let closing = marker.to_string().repeat(closing_width);
                    let code = format!("{opening}text\nraw diagnostic; version 2.4.1\n{closing}");
                    let text = format!("{code}\n{FAILURE} {REPAIR}");
                    assert_eq!(
                        split(&text).map(str::trim).collect::<Vec<_>>(),
                        [code.as_str(), FAILURE, REPAIR]
                    );
                    assert_eq!(super::super::inline_pair(&text), Some((FAILURE, REPAIR)));
                }
            }
        }
    }

    #[test]
    fn session_arc_wrong_or_nonterminal_fences_do_not_expose_inner_clauses() {
        for false_closer in ["``", "~~~", "prefix ```", "``` trailing text", "    ```"] {
            let code = format!("```text\n{false_closer}\nraw diagnostic; still code.\n```");
            let text = format!("{code}\n{REPAIR}");
            assert_eq!(
                split(&text).map(str::trim).collect::<Vec<_>>(),
                [code.as_str(), REPAIR],
                "{false_closer}"
            );
        }
    }

    #[test]
    fn session_arc_indented_crlf_fences_preserve_every_original_byte() {
        for indentation in ["", " ", "  ", "   "] {
            let code = format!("{indentation}~~~text\r\n資料; 🦀.\r\n{indentation}~~~~ \t\r\n");
            let text = format!("{code}{REPAIR}");
            let parts: Vec<_> = split(&text).collect();
            assert_eq!(parts, [code.as_str(), REPAIR]);
            assert_eq!(parts.concat(), text);
        }
    }

    #[test]
    fn session_arc_fences_never_override_an_existing_inline_span() {
        let quoted = "`multiline example\n~~~text\nnot a fence; still inline.\n~~~\n`";
        let text = format!("{quoted}\n{REPAIR}");
        assert_eq!(
            split(&text).map(str::trim).collect::<Vec<_>>(),
            [quoted, REPAIR]
        );
        assert_eq!(split("ordinary ~~~ prose; next clause.").count(), 2);
        assert_eq!(
            split("~~~text\nunclosed fence; still one record.").count(),
            1
        );
    }

    #[test]
    fn session_arc_inline_delimiters_do_not_rescan_the_remaining_transcript() {
        for repetitions in [1, 100, 10_000] {
            let line = "`token` ~ ``quoted`` ~~ ".repeat(repetitions);
            let text = format!("{line}\n{FAILURE}\n{REPAIR}");
            FENCE_TAIL_BYTES.with(|bytes| bytes.set(0));
            let parts: Vec<_> = split(&text).collect();
            // Lossless slices include the newline following FAILURE's final
            // period. It is a separator, not an extra semantic observation.
            assert_eq!(parts.concat(), text);
            let clauses: Vec<_> = parts
                .iter()
                .map(|part| part.trim())
                .filter(|part| !part.is_empty())
                .collect();
            assert_eq!(clauses, [line.trim(), FAILURE, REPAIR]);
            FENCE_TAIL_BYTES.with(|bytes| assert_eq!(bytes.get(), 0));
        }
    }

    #[test]
    fn session_arc_fence_tail_inspection_is_bounded_by_source_bytes() {
        let body = "quoted ``` and ~~~ data; version 2.4.1 ".repeat(10_000);
        let code = format!("```text\n{body}\n``` not a closer\n~~~\n````");
        let text = format!("{code}\n{REPAIR}");
        FENCE_TAIL_BYTES.with(|bytes| bytes.set(0));
        let parts: Vec<_> = split(&text).collect();
        assert_eq!(parts.concat(), text);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].trim(), code);
        assert_eq!(parts[1], REPAIR);
        FENCE_TAIL_BYTES.with(|bytes| {
            assert!(bytes.get() > 0, "real fence tails must have been inspected");
            assert!(bytes.get() <= text.len());
        });
    }

    #[test]
    fn failed_counter_trailers_cannot_leave_an_optimistic_repair_admitted() {
        for report in [
            "1 failed.",
            "errors: 2.",
            "21 passed, 1 failed.",
            "Process exited with code 0, 1 failed.",
            "0 passed; 1 assertion failed; 0 ignored.",
            "test result: FAILED.\n21 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out.",
            "1 failed.\n0 failed.\nProcess exited with code 0.",
        ] {
            let retry = format!("{REPAIR}\n{report}");
            let source = format!("{FAILURE}\n{retry}");
            let parts: Vec<_> = split(&source)
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect();
            assert_eq!(parts, [FAILURE, retry.as_str()], "{report}");
            assert!(!super::super::resolution_signal(parts[1]), "{report}");
            assert!(super::super::inline_pair(&source).is_none(), "{report}");
        }
    }

    #[test]
    fn numeric_and_unknown_failure_reports_never_round_down_to_a_clean_retry() {
        for report in [
            "errors: unknown.",
            "failures: null.",
            "1,000 failed.",
            "+0 failed.",
            ".0 failed.",
            "failures=0.5.",
            "999999999999999999999999999999 failed.",
        ] {
            let source = format!("{FAILURE}\n{REPAIR}\n{report}");
            assert!(super::super::inline_pair(&source).is_none(), "{report}");
            assert_eq!(split(&source).collect::<String>(), source);
        }
    }

    #[test]
    fn process_failures_propagate_through_counter_and_elapsed_time_trailers() {
        for status in ["101", "-9", "unknown", "0.5"] {
            for time in ["0.03s", "12ms", "3s"] {
                let report = format!(
                    "test result: ok.\n21 passed; 0 failed; 0 ignored; 0 measured; \
                     0 filtered out; finished in {time}.\nProcess exited with code {status}."
                );
                let source = format!("{FAILURE}\n{REPAIR}\n{report}");
                assert!(super::super::inline_pair(&source).is_none(), "{report}");
            }
        }
    }

    #[test]
    fn successful_reports_preserve_every_historical_clause_and_selected_excerpt() {
        for report in [
            "21 passed, 0 failed.",
            "test result: ok.\n21 passed; 0 failed; 0 ignored; finished in 0.03s.",
            "0 tests failed.\nProcess exited with code 0.",
        ] {
            let source = format!("{FAILURE}\n{REPAIR}\n{report}\n");
            assert_eq!(
                split(&source).collect::<Vec<_>>(),
                split_raw(&source).collect::<Vec<_>>()
            );
            assert_eq!(super::super::inline_pair(&source), Some((FAILURE, REPAIR)));
        }
    }

    #[test]
    fn reports_cannot_cross_independent_observations_or_quoted_examples() {
        for barrier in [
            "The independent lint command for src/ui.rs completed.",
            "Another command started for src/ui.rs.",
            "`1 failed`",
            "```text\n1 failed\n```",
            "Example: 1 failed.",
            "1 failed in src/unrelated.rs.",
        ] {
            assert!(outcome_report_veto(barrier).is_none(), "{barrier}");
            let source = format!("{FAILURE}\n{REPAIR}\n{barrier}\n1 failed.");
            assert_eq!(super::super::inline_pair(&source), Some((FAILURE, REPAIR)));
            assert_eq!(split(&source).collect::<String>(), source);
        }
    }

    #[test]
    fn failed_retry_can_still_be_followed_by_a_real_source_backed_repair() {
        let failure = "cargo test src/api.rs failed.";
        let retry = "Fixed src/api.rs and cargo test passed.\n1 failed.";
        let repair = "cargo test src/api.rs passed after restoring the guard.";
        let source = format!("{failure}\n{retry}\n{repair}");
        let pairs: Vec<_> = super::super::inline_pairs(&source).collect();
        assert_eq!(pairs, [(retry, repair)]);
        assert!(source.contains(pairs[0].0));
        assert!(source.contains(pairs[0].1));
    }

    #[test]
    fn structured_and_unicode_observations_keep_original_source_bytes() {
        let body = format!("資料 café 🦀\r\n{FAILURE}\r\n{REPAIR}\r\n21 passed;\r\n1 failed.");
        let raw = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [{"type": "text", "text": body}]}
        })
        .to_string();
        let hash = blake3::hash(raw.as_bytes());
        let projected = super::super::text::message_text(&raw).expect("admitted transcript");
        assert_eq!(projected, body);
        assert!(super::super::inline_pair(&projected).is_none());
        let mut offset = 0;
        for part in split(&projected) {
            assert_eq!(part.as_ptr(), projected[offset..].as_ptr());
            assert_eq!(part, &projected[offset..offset + part.len()]);
            offset += part.len();
        }
        assert_eq!(offset, projected.len());
        assert_eq!(blake3::hash(raw.as_bytes()), hash);
    }

    #[test]
    fn outcome_lookahead_is_linear_even_across_long_blank_and_report_runs() {
        for repetitions in [1, 100, 10_000] {
            let blanks = "\n \t\r\n".repeat(repetitions);
            let reports = "21 passed;\n0 failed;\n".repeat(repetitions);
            for ending in ["Another observation.", "1 failed."] {
                let source = format!("{FAILURE}\n{REPAIR}{blanks}{reports}{ending}");
                OUTCOME_TRAILER_BYTES.with(|bytes| bytes.set(0));
                assert_eq!(split(&source).collect::<String>(), source);
                OUTCOME_TRAILER_BYTES.with(|bytes| {
                    assert!(bytes.get() > 0);
                    assert!(bytes.get() <= source.len(), "a source suffix was rescanned");
                });
            }
        }
    }

    #[test]
    fn complete_result_windows_share_the_clause_outcome_policy() {
        for (source, expected) in [
            ("21 passed; 0 failed; finished in 0.03s.", Some(false)),
            (
                "1 failed.\n0 failed.\nProcess exited with code 0.",
                Some(true),
            ),
            (
                "test result: ok.\nProcess exited with code 101.",
                Some(true),
            ),
            ("errors: unknown.\nfinished in 12ms.", Some(true)),
            ("Process exited with code 0.5.", Some(true)),
            ("finished in 3s.", Some(false)),
            ("\n \t\r\n", None),
        ] {
            assert_eq!(report_window_veto(source), expected, "{source}");
        }
    }

    #[test]
    fn a_failed_prefix_cannot_hide_an_independent_window_tail() {
        for independent in [
            "Another command started for src/ui.rs.",
            "Example: 1 failed.",
            "`1 failed`",
            "```text\n1 failed\n```",
            "資料 café 🦀 was recorded.",
        ] {
            for source in [
                format!("1 failed.\n{independent}"),
                format!("{independent}\n1 failed."),
                format!("21 passed.\n{independent}\nProcess exited with code 1."),
            ] {
                let hash = blake3::hash(source.as_bytes());
                assert!(report_window_veto(&source).is_none(), "{source}");
                assert_eq!(blake3::hash(source.as_bytes()), hash);
            }
        }
    }

    #[test]
    fn complete_report_window_scanning_does_not_stop_at_the_first_failure() {
        let reports = "21 passed; 0 failed;\n".repeat(10_000);
        let source = format!("1 failed.\n{reports}Another command started.");
        OUTCOME_TRAILER_BYTES.with(|bytes| bytes.set(0));
        assert_eq!(report_window_veto(&source), None);
        OUTCOME_TRAILER_BYTES.with(|bytes| {
            assert!(bytes.get() >= reports.len() / 2);
            assert!(bytes.get() <= source.len());
        });
    }

    #[test]
    fn timed_python_reports_veto_repairs_in_inline_and_separate_windows() {
        for report in [
            "1 failed, 21 passed in 0.03s",
            "================ 1 failed, 21 passed in 0.03s ================",
            "1 error, 21 passed in 125ms",
            "Ran 22 tests in 0.003s\nFAILED (failures=1, errors=0)",
            "Ran 1 test in 1s\nFAILED (errors=1)",
        ] {
            assert_eq!(report_window_veto(report), Some(true), "{report}");
            let source = format!("{FAILURE}\n{REPAIR}\n{report}");
            assert!(super::super::inline_pair(&source).is_none(), "{report}");
            assert_eq!(split(&source).collect::<String>(), source);
        }
    }

    #[test]
    fn clean_timed_reports_do_not_detach_a_later_nonzero_exit() {
        for report in [
            "21 passed, 0 failed in 0.03s",
            "================ 21 passed in 0.03s ================",
            "Ran 21 tests in 0.003s",
        ] {
            assert_eq!(report_window_veto(report), Some(false), "{report}");
            let source = format!("{FAILURE}\n{REPAIR}\n{report}");
            assert_eq!(super::super::inline_pair(&source), Some((FAILURE, REPAIR)));
            let failed = format!("{source}\nProcess exited with code 101.");
            assert!(super::super::inline_pair(&failed).is_none(), "{report}");
            assert_eq!(
                report_window_veto(&format!("{report}\nProcess exited with code 101.")),
                Some(true)
            );
        }
    }

    #[test]
    fn test_timing_framing_cannot_swallow_resources_commands_or_examples() {
        for barrier in [
            "1 failed in src/other.rs",
            "1 failed in 0.03s while running src/other.rs",
            "Example: 1 failed in 0.03s",
            "`1 failed in 0.03s`",
            "=== 1 failed in 0.03s === then another command started",
            "=== 1 failed in 0.03s",
            "Ran cargo test in 0.03s",
            "Ran 21 tests in src/other.rs",
            "Ran 21 tests in 0.03s after the independent command",
        ] {
            assert_eq!(report_window_veto(barrier), None, "{barrier}");
            let source = format!("{FAILURE}\n{REPAIR}\n{barrier}\n1 failed.");
            assert_eq!(super::super::inline_pair(&source), Some((FAILURE, REPAIR)));
        }
    }

    #[test]
    fn report_durations_require_complete_decimal_numbers_and_units() {
        for valid in ["0s", "0.03s", "125ms", "1.25 s", "12 ms"] {
            assert!(elapsed_duration(valid), "{valid}");
        }
        for invalid in [
            "",
            "s",
            ".s",
            ".3s",
            "3.s",
            "3..0s",
            "-1s",
            "+0s",
            "NaNs",
            "infs",
            "0.03s command",
            "src/test.rs",
            "0/1s",
            "0.03",
            "１s",
        ] {
            assert!(!elapsed_duration(invalid), "{invalid}");
        }
    }

    #[test]
    fn labeled_suite_failures_veto_the_same_inline_and_window_observation() {
        for report in [
            "Test Suites: 1 failed, 2 passed, 3 total",
            "Tests: 1 failed, 21 passed, 22 total",
            "Snapshots: 1 failed, 0 passed, 1 total",
            "Test Files  1 failed | 2 passed (3)",
            "Tests  1 failed | 21 passed | 2 skipped (24)",
            "Test Files 1 failed | 2 passed (3)\nTests 1 failed | 21 passed (22)\nStart at 09:32:53\nDuration 4.80s (transform 44ms, setup 0ms, import 35ms, tests 4.52s, environment 0ms)",
            "Test Suites: 1 failed, 2 passed, 3 total\nTests: 1 failed, 21 passed, 22 total\nSnapshots: 0 total\nTime: 1.423 s, estimated 2 s",
        ] {
            assert_eq!(report_window_veto(report), Some(true), "{report}");
            let source = format!("{FAILURE}\n{REPAIR}\n{report}");
            assert!(super::super::inline_pair(&source).is_none(), "{report}");
            assert_eq!(split(&source).collect::<String>(), source);
        }
    }

    #[test]
    fn clean_suite_metadata_cannot_detach_the_final_process_outcome() {
        for report in [
            "Test Suites: 2 passed, 2 total\nTests: 21 passed, 21 total\nSnapshots: 0 total\nTime: 1.423 s",
            "Tests  21 passed | 2 skipped (23)\nDuration 125ms",
            "Tests: 21 passed, 0 failed, 21 total\nTime: 1.423 s, estimated 2 s",
            "Snapshots: 0 total",
            "Tests 21 passed (21)\nStart at 23:59:59\nDuration 4.80s (transform 44ms, setup 0ms, collect 35ms, tests 4.52s, environment 0ms, prepare 81ms)",
            "Tests: 21 passed, 21 total\nRan all test suites.",
        ] {
            assert_eq!(report_window_veto(report), Some(false), "{report}");
            let source = format!("{FAILURE}\n{REPAIR}\n{report}");
            assert_eq!(super::super::inline_pair(&source), Some((FAILURE, REPAIR)));
            for status in ["1", "101", "unknown", "0.5"] {
                let source = format!("{source}\nProcess exited with code {status}.");
                assert!(super::super::inline_pair(&source).is_none(), "{source}");
            }
        }
    }

    #[test]
    fn runner_headers_do_not_admit_partial_reports_or_shell_pipelines() {
        for barrier in [
            "Test Suites: 1 failed, 2 passed, 3 total in src/other.rs",
            "Tests: 1 failed | run next command",
            "Tests: 1 failed (src/other.rs)",
            "Test Files 1 failed | 2 passed (3) after another command",
            "Snapshots: 0 total; an independent command started",
            "Tests: 1 failed,",
            "Tests: 1 failed || 2 passed",
            "Tests: 1 failed (3) (4)",
            "Tests: total",
            "TestsDirectory: 1 failed, 2 total",
            "Test Suites/example: 1 failed, 2 total",
            "1 failed | 2 passed",
            "Example: Tests: 1 failed, 2 total",
            "`Tests: 1 failed, 2 total`",
            "Time: 1.423 s, estimated 2 s after another command",
            "Duration 125ms for src/other.rs",
            "Duration 1s (tests 1ms, command src/other.rs)",
            "Duration 1s (tests 1ms, tests 2ms)",
            "Duration 1s (tests 1ms) after another command",
            "Start at 24:00:00",
            "Start at 12:60:00",
            "Start at 12:00:60",
            "Start at 1:00:00",
            "Start at 12:00:00 then another command",
            "Ran all test suites matching src/other.rs",
        ] {
            assert_eq!(report_window_veto(barrier), None, "{barrier}");
            // The complete-window predicate rejects mixed observations even
            // when a failed result appears before the independent tail.
            let mixed = format!("Tests: 1 failed, 1 total\n{barrier}");
            assert_eq!(report_window_veto(&mixed), None, "{mixed}");
        }
    }

    #[test]
    fn labeled_result_observations_preserve_unicode_bytes_and_bounded_scanning() {
        let reports = "Tests: 21 passed, 21 total\nSnapshots: 0 total\n".repeat(10_000);
        let source = format!(
            "資料 café 🦀\r\n{FAILURE}\r\n{REPAIR}\r\n{reports}Test Suites: 1 failed, 1 total"
        );
        OUTCOME_TRAILER_BYTES.with(|bytes| bytes.set(0));
        let parts: Vec<_> = split(&source).collect();
        assert_eq!(parts.concat(), source);
        let mut offset = 0;
        for part in parts {
            assert_eq!(part.as_ptr(), source[offset..].as_ptr());
            offset += part.len();
        }
        assert_eq!(offset, source.len());
        OUTCOME_TRAILER_BYTES.with(|bytes| {
            assert!(bytes.get() >= reports.len());
            assert!(bytes.get() <= source.len());
        });
        assert!(super::super::inline_pair(&source).is_none());
    }

    #[test]
    fn runner_header_extension_preserves_legacy_label_first_failure_counters() {
        for report in [
            "tests failed: 1",
            "tests: failures=1",
            "tests: errors: unknown",
        ] {
            assert_eq!(report_window_veto(report), Some(true), "{report}");
            let source = format!("{FAILURE}\n{REPAIR}\n{report}");
            assert!(super::super::inline_pair(&source).is_none(), "{report}");
        }
    }

    #[test]
    fn real_sequence_path_honors_runner_results_scope_adjacency_and_provenance() {
        use crate::db::{StoredEvidenceSpan, StoredSession};

        let session = StoredSession {
            id: "ses_resource0000000000000000000".into(),
            workspace_id: "wsp_resource0000000000000000000".into(),
            cass_session_id: "runner-outcome-regression".into(),
            source_path: None,
            agent_name: None,
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 0,
            token_count: None,
            content_hash: "blake3:runner-session".into(),
            metadata_json: None,
            imported_at: "2026-10-13T00:00:00Z".into(),
            updated_at: "2026-10-13T00:00:00Z".into(),
        };
        for structured in [false, true] {
            let span = |id: &str, line: u32, body: &str| {
                let excerpt = if structured {
                    serde_json::json!({"type": "assistant", "message": {
                        "role": "assistant", "content": body
                    }})
                    .to_string()
                } else {
                    body.to_owned()
                };
                let hash = format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex());
                StoredEvidenceSpan {
                    id: id.into(),
                    workspace_id: session.workspace_id.clone(),
                    session_id: session.id.clone(),
                    memory_id: None,
                    cass_span_id: format!("cass-{id}"),
                    span_kind: "message".into(),
                    start_line: line,
                    end_line: line,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".into()),
                    excerpt,
                    reader_projection: None,
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
                    created_at: "2026-10-13T00:00:00Z".into(),
                    updated_at: "2026-10-13T00:00:00Z".into(),
                }
            };
            for (report, veto) in [
                ("1 failed, 21 passed in 0.03s", true),
                ("Ran 22 tests in 0.003s\nFAILED (errors=1)", true),
                ("Tests: 1 failed, 21 passed, 22 total", true),
                ("Test Files 1 failed | 2 passed (3)", true),
                ("Tests: 21 passed, 21 total\nTime: 1.423 s", false),
                (
                    "Tests 21 passed (21)\nStart at 09:32:53\nDuration 1s (tests 125ms)",
                    false,
                ),
            ] {
                let repair = "Fixed src/api.rs by restoring the guard.";
                let mut spans = vec![
                    span("failure", 1, "cargo test src/api.rs failed."),
                    span("repair", 2, repair),
                    span("report", 3, report),
                ];
                let rows =
                    super::super::sequence::candidates(&session.workspace_id, &session, &spans);
                assert_eq!(rows.len(), if veto { 0 } else { 2 }, "{report}");
                for row in &rows {
                    let arc = row.session_arc.as_ref().expect("source-backed pair");
                    assert_eq!(arc.failure_span.evidence_span_id, spans[0].id);
                    assert_eq!(arc.resolution_span.evidence_span_id, spans[1].id);
                    assert_eq!(arc.failure_span.content_hash, spans[0].content_hash);
                    assert_eq!(arc.resolution_span.content_hash, spans[1].content_hash);
                    assert_eq!(arc.resolution_span.excerpt, repair);
                }
                if veto {
                    // A result cannot veto an unrelated, non-adjacent attempt
                    // or cross the workspace filter at the sequence boundary.
                    spans[2].start_line = 9;
                    spans[2].end_line = 9;
                    assert_eq!(
                        super::super::sequence::candidates(&session.workspace_id, &session, &spans)
                            .len(),
                        2
                    );
                    spans[2].start_line = 3;
                    spans[2].end_line = 3;
                    spans[2].workspace_id = "other-workspace".into();
                    assert_eq!(
                        super::super::sequence::candidates(&session.workspace_id, &session, &spans)
                            .len(),
                        2
                    );
                }
            }
        }
    }
}
