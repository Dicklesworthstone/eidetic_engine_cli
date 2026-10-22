//! Apply external credential matches without replacing JSON syntax.
//!
//! Keep the original whole-record scan: a provider's identifying context may
//! live in another field or disappear during a different redaction. Decode
//! only validated JSON strings, merge original and decoded matches before any
//! rewrite, and retain every other source byte. Quoted JSON stays ordinary text.

use crate::policy::{
    SecretRedactionMatch, SecretRedactionReport, key_value_redaction_code, redact_git_capture_text,
    redaction_placeholder,
};

use super::{InvalidEncodedJson, decode_records, same_record_classes};

pub(in crate::policy::ingestion) fn preserve_json_strings(
    input: &str,
    original: SecretRedactionReport,
) -> Result<SecretRedactionReport, InvalidEncodedJson> {
    let start = input.trim_start();
    if !original.redacted || !(start.starts_with('{') || start.starts_with('[')) {
        return Ok(original);
    }
    // The shared validator enforces the 1 MiB input, 256-record, unique-key,
    // physical-record-boundary and parser-depth budgets before token walking.
    decode_records(input)?;
    let mut reasons = original.redacted_reasons;
    let mut whole = original.matches;
    sort_matches(&mut whole);
    let mut next_match = 0;
    let mut cursor = 0;
    let mut output = String::with_capacity(input.len());
    let mut applied = Vec::new();
    let mut previous_string: Option<(usize, usize)> = None;
    while let Some(relative_quote) = input[cursor..].find('"') {
        let quote = cursor + relative_quote;
        let token = JsonString::read(input, quote)?;
        let decoded = redact_git_capture_text(&token.text);
        reasons.extend(decoded.redacted_reasons);
        let mut local = decoded.matches;
        local.retain(|matched| !already_redacted(&token.text, matched));
        let mut incoming = Vec::new();
        while let Some(matched) = whole.get(next_match).filter(|m| m.start < token.end) {
            next_match += 1;
            incoming.push(matched.clone());
        }
        if let Some((key_start, key_end)) = previous_string
            && input[key_end..quote].trim_start().starts_with(':')
        {
            // An earlier raw YAML block can hide later occurrences of the
            // same key from that scanner's cursor. Screen each original JSON
            // key/value pair too; a value's string alone lacks its key label.
            // Adjacent token slices overlap only at the string tokens, so
            // their total scan size remains bounded by twice the input size.
            let pair = redact_git_capture_text(&input[key_start..token.end]);
            reasons.extend(pair.redacted_reasons);
            incoming.extend(pair.matches.into_iter().filter_map(|mut matched| {
                matched.start += key_start;
                matched.end += key_start;
                (matched.end > key_end).then_some(matched)
            }));
        }
        for matched in &incoming {
            if already_redacted(input, matched) {
                continue;
            }
            if matched.start < token.body_start() {
                // URL authority scanning found the colon between a JSON key
                // and its value, not a password colon inside a URL string.
                // Only this structural proof permits discarding a raw match.
                if matched.pattern_id == "url_password"
                    && matched.start == quote
                    && input[..quote].trim_end().ends_with(':')
                {
                    continue;
                }
                return Err(InvalidEncodedJson);
            }
            let local_start = token.decoded_offset(matched.start, false)?;
            if matched.end > token.body_end() {
                // A decoded YAML block or URL proves its own local end. An
                // incomplete credential cannot borrow an end from a neighbor.
                let yaml = matches!(input.as_bytes().get(matched.start), Some(b'|' | b'>'))
                    && crate::policy::SECRET_KEY_PATTERNS
                        .iter()
                        .any(|pattern| pattern.code == matched.pattern_id);
                if !(yaml || matched.pattern_id == "url_password")
                    || !local.iter().any(|candidate| {
                        candidate.pattern_id == matched.pattern_id && candidate.start == local_start
                    })
                {
                    return Err(InvalidEncodedJson);
                }
                continue;
            }
            local.push(SecretRedactionMatch {
                pattern_id: matched.pattern_id,
                start: local_start,
                end: token.decoded_offset(matched.end, true)?,
            });
        }
        for matched in &mut local {
            let value = token
                .text
                .get(matched.start..matched.end)
                .ok_or(InvalidEncodedJson)?;
            matched.pattern_id = key_value_redaction_code(matched.pattern_id, value);
        }
        discard_shadowed_pii(&token.text, &mut local);
        sort_matches(&mut local);
        let mut rewritten = String::new();
        let mut consumed = 0;
        for matched in &local {
            if matched.start >= consumed {
                rewritten.push_str(
                    token
                        .text
                        .get(consumed..matched.start)
                        .ok_or(InvalidEncodedJson)?,
                );
                rewritten.push_str(&redaction_placeholder(matched.pattern_id));
            }
            consumed = consumed.max(matched.end);
            applied.push(SecretRedactionMatch {
                pattern_id: matched.pattern_id,
                start: token.source_offset(matched.start)?,
                end: token.source_offset(matched.end)?,
            });
        }
        output.push_str(&input[cursor..quote]);
        if local.is_empty() {
            output.push_str(&input[quote..token.end]);
        } else {
            rewritten.push_str(token.text.get(consumed..).ok_or(InvalidEncodedJson)?);
            output.push_str(&serde_json::to_string(&rewritten).map_err(|_| InvalidEncodedJson)?);
        }
        if output.len() > super::super::MAX_SCAN_BYTES {
            return Err(InvalidEncodedJson);
        }
        cursor = token.end;
        previous_string = Some((quote, token.end));
    }
    if next_match != whole.len() {
        // Matches in numbers, literals or other syntax have no unambiguous
        // string replacement. Never repair their types or discard their fields.
        return Err(InvalidEncodedJson);
    }
    if let Some((key_start, key_end)) = previous_string
        && input[key_end..].trim_start().starts_with(':')
        && redact_git_capture_text(&input[key_start..])
            .matches
            .iter()
            .any(|matched| matched.end + key_start > key_end)
    {
        return Err(InvalidEncodedJson);
    }
    output.push_str(&input[cursor..]);
    if !same_record_classes(input, &output) {
        return Err(InvalidEncodedJson);
    }
    sort_matches(&mut applied);
    // Detection reports include matches swallowed by an earlier provider
    // replacement (e.g. an SSN-shaped fragment inside a GitHub token). Keep
    // the existing scanners' actual reason precedence, while retaining the
    // union of all original spans for replacement and match accounting.
    reasons.retain(|reason| applied.iter().any(|matched| matched.pattern_id == *reason));
    reasons.sort_unstable();
    reasons.dedup();
    Ok(SecretRedactionReport {
        redacted: !applied.is_empty(),
        content: output,
        redacted_reasons: reasons,
        matches: applied,
    })
}

fn already_redacted(input: &str, matched: &SecretRedactionMatch) -> bool {
    input.get(matched.start..matched.end).is_some_and(|value| {
        if matched.pattern_id == "url_password" {
            value == redaction_placeholder("url_password")
        } else {
            value.starts_with("[REDACTED:")
        }
    })
}

fn discard_shadowed_pii(text: &str, matches: &mut Vec<SecretRedactionMatch>) {
    let is_pii = |pattern| matches!(pattern, "email_address" | "phone_number" | "ssn");
    let mut url_passwords = matches
        .iter()
        .filter(|matched| matched.pattern_id == "url_password")
        .map(|matched| (matched.end, matched.start))
        .collect::<Vec<_>>();
    url_passwords.sort_unstable();
    let mut credentials = matches
        .iter()
        .filter(|matched| !is_pii(matched.pattern_id))
        .map(|matched| (matched.start, matched.end))
        .collect::<Vec<_>>();
    credentials.sort_unstable();
    let mut covered: Vec<(usize, usize)> = Vec::with_capacity(credentials.len());
    for (start, end) in credentials {
        if let Some(previous) = covered.last_mut()
            && start <= previous.1
        {
            previous.1 = previous.1.max(end);
        } else {
            covered.push((start, end));
        }
    }
    // Fully covered PII has no bytes left after credential replacement. For
    // user:password@host, prove that the entire email-shaped local part lies
    // inside a URL password ending at that same @ before preserving the host.
    // Keep every other partial overlap: replacing only part of a credential
    // does not prove its remaining suffix is harmless. All spans here belong
    // to this decoded string, never an unresolved cross-field raw match.
    matches.retain(|matched| {
        if !is_pii(matched.pattern_id) {
            return true;
        }
        let index = covered.partition_point(|&(_, end)| end <= matched.start);
        if covered
            .get(index)
            .is_some_and(|&(start, end)| start <= matched.start && end >= matched.end)
        {
            return false;
        }
        if matched.pattern_id == "email_address"
            && let Some(at) = text[matched.start..matched.end].find('@')
        {
            let at = matched.start + at;
            let index = url_passwords.partition_point(|&(end, _)| end < at);
            if url_passwords.get(index).is_some_and(|&(end, start)| {
                end == at && start <= matched.start && matched.start < end
            }) {
                return false;
            }
        }
        true
    });
}

fn sort_matches(matches: &mut Vec<SecretRedactionMatch>) {
    matches.sort_by(|left, right| {
        (left.pattern_id, left.start, left.end).cmp(&(right.pattern_id, right.start, right.end))
    });
    let mut merged: Vec<SecretRedactionMatch> = Vec::with_capacity(matches.len());
    for matched in matches.drain(..) {
        if let Some(previous) = merged.last_mut()
            && previous.pattern_id == matched.pattern_id
            && matched.start <= previous.end
        {
            previous.end = previous.end.max(matched.end);
        } else {
            merged.push(matched);
        }
    }
    *matches = merged;
    matches.sort_by(|left, right| {
        (left.start, left.end, left.pattern_id).cmp(&(right.start, right.end, right.pattern_id))
    });
}

struct JsonString {
    quote: usize,
    end: usize,
    text: String,
    // At most one pair per source byte, bounded by the validated 1 MiB input.
    // Only one string and its map are retained while walking the source.
    boundaries: Vec<(usize, usize)>,
}

impl JsonString {
    fn read(input: &str, quote: usize) -> Result<Self, InvalidEncodedJson> {
        let mut cursor = quote + 1;
        loop {
            match input.as_bytes().get(cursor) {
                Some(b'"') => break,
                Some(b'\\') => cursor += 2,
                Some(_) => cursor += 1,
                None => return Err(InvalidEncodedJson),
            }
        }
        let end = cursor + 1;
        let text: String =
            serde_json::from_str(&input[quote..end]).map_err(|_| InvalidEncodedJson)?;
        let mut boundaries = Vec::new();
        let mut source = quote + 1;
        boundaries.push((source, 0));
        for (decoded, ch) in text.char_indices() {
            let width = if input.as_bytes().get(source) == Some(&b'\\') {
                if input.as_bytes().get(source + 1) == Some(&b'u') {
                    if ch.len_utf8() == 4 { 12 } else { 6 }
                } else {
                    2
                }
            } else {
                ch.len_utf8()
            };
            source += width;
            boundaries.push((source, decoded + ch.len_utf8()));
        }
        if source != cursor {
            return Err(InvalidEncodedJson);
        }
        Ok(Self {
            quote,
            end,
            text,
            boundaries,
        })
    }

    fn body_start(&self) -> usize {
        self.quote + 1
    }

    fn body_end(&self) -> usize {
        self.end - 1
    }

    fn decoded_offset(&self, source: usize, end: bool) -> Result<usize, InvalidEncodedJson> {
        match self
            .boundaries
            .binary_search_by_key(&source, |&(raw, _)| raw)
        {
            Ok(index) => Ok(self.boundaries[index].1),
            // A raw detector can start at the n in an encoded newline before
            // an email. Expand outward to the complete decoded character;
            // never split an escape, surrogate pair, or UTF-8 code point.
            Err(index) if index > 0 && index < self.boundaries.len() => {
                Ok(self.boundaries[index - usize::from(!end)].1)
            }
            Err(_) => Err(InvalidEncodedJson),
        }
    }

    fn source_offset(&self, decoded: usize) -> Result<usize, InvalidEncodedJson> {
        self.boundaries
            .binary_search_by_key(&decoded, |&(_, text)| text)
            .map(|index| self.boundaries[index].0)
            .map_err(|_| InvalidEncodedJson)
    }
}

#[cfg(test)]
#[path = "ingestion_json_redaction_tests.rs"]
mod tests;
