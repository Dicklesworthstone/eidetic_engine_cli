//! Borrowed clause boundaries for technical failure/repair evidence.
//!
//! A period inside a version, filename or configuration key is not a sentence
//! boundary. Delimiters inside a Markdown code span are source text, not prose
//! ordering. Preserve all bytes; this splitter never rewrites quoted commands
//! or fabricates transcript line offsets.

pub(super) fn split(excerpt: &str) -> impl Iterator<Item = &str> {
    let mut characters = excerpt.char_indices().peekable();
    let mut start = 0;
    let mut code_delimiter = 0;
    std::iter::from_fn(move || {
        while let Some((position, ch)) = characters.next() {
            if ch == '`' && !escaped(excerpt, position) {
                let mut width = 1;
                while characters.peek().is_some_and(|(_, next)| *next == '`') {
                    let _ = characters.next();
                    width += 1;
                }
                if code_delimiter == 0 {
                    code_delimiter = width;
                } else if width == code_delimiter {
                    code_delimiter = 0;
                }
                continue;
            }
            if code_delimiter != 0 {
                continue;
            }
            let end = position + ch.len_utf8();
            let boundary = matches!(ch, '\n' | ';')
                || (ch == '.' && sentence_period(excerpt, end));
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
    if text[end..].chars().next().is_some_and(|next| !next.is_whitespace()) {
        return false;
    }
    let word = text[..end]
        .rsplit_once(char::is_whitespace)
        .map_or(&text[..end], |(_, word)| word);
    // These common technical abbreviations introduce the rest of a clause.
    !["e.g.", "i.e.", "vs."].iter().any(|item| word.eq_ignore_ascii_case(item))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAILURE: &str = "Failure arc: M7.cache.lookup in src/cache.rs failed with version 2.4.1.";
    const REPAIR: &str = "Fix: M7.cache.lookup in src/cache.rs was repaired by selecting stable identity bytes.";

    #[test]
    fn filenames_versions_and_configuration_keys_survive_in_both_halves() {
        let text = format!("{FAILURE} {REPAIR}");
        assert_eq!(split(&text).map(str::trim).collect::<Vec<_>>(), [FAILURE, REPAIR]);
        assert_eq!(super::super::inline_pair(&text), Some((FAILURE, REPAIR)));
        assert!(!text.split_inclusive(['\n', ';', '.']).any(|part| part == FAILURE));
    }

    #[test]
    fn code_delimiters_do_not_fragment_the_failure_or_repair() {
        let failure = "Failure arc: `cache.read(\"a.b\"); cache.close()` failed.";
        let repair = "Fix: ``cache.write(`key`, 2.4); cache.close()`` repaired the lookup.";
        let text = format!("{failure}\n{repair}");
        let parts: Vec<_> = split(&text).map(str::trim).filter(|part| !part.is_empty()).collect();
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
        let pieces = ["資料", "🦀", "x.y", "e.g. ", ". ", ";", "\r\n", "`", "``", "\\`", ""];
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
            let (actual_failure, actual_repair) = super::super::inline_pair(&text).expect("ordered pair");
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
            assert_eq!(super::super::inline_pair(&format!("{FAILURE} {repair}")), Some((FAILURE, repair)));
        }
        let text = format!("{FAILURE} Failure arc: M7 cache patch failed. {REPAIR}");
        let pair = super::super::inline_pair(&text).expect("later observed repair");
        assert_eq!(pair.1, REPAIR);
    }
}
