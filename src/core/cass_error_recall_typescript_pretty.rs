//! Complete plain and color/context TypeScript diagnostic reports.
//!
//! Only SGR styling is removed; terminal cursor and hyperlink controls cannot
//! rewrite an observation. Source gutters, related information and message
//! chains stay opaque. Every error and optional summary is checked before any
//! fingerprint is returned. A diagnostic never supplies completion authority.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    CanonicalDiagnostic, Header, MAX_DIAGNOSTICS_PER_FAILURE, MAX_RECORDS,
    MAX_SCANNED_OUTPUT_BYTES, canonical, header, positive_decimal, source_path,
};

#[derive(Clone, Copy)]
struct Location<'a> {
    file: &'a str,
    row: &'a str,
}

struct Record<'a> {
    diagnostic: Header<'a>,
    location: Option<Location<'a>>,
    has_context: bool,
}

fn record(line: &str) -> Result<Option<Record<'_>>, ()> {
    if line.starts_with(char::is_whitespace) {
        return Ok(None);
    }
    if let Some(diagnostic) = header(line)? {
        let location = if line.starts_with("error TS") {
            None
        } else {
            let (location, _) = line.split_once("): ").ok_or(())?;
            let (file, position) = location.rsplit_once('(').ok_or(())?;
            let (row, _) = position.split_once(',').ok_or(())?;
            Some(Location { file, row })
        };
        return Ok(Some(Record {
            diagnostic,
            location,
            has_context: false,
        }));
    }
    let Some((source, diagnostic)) = line.split_once(" - error TS") else {
        return Ok(None);
    };
    let Some((file_row, column)) = source.rsplit_once(':') else {
        return Ok(None);
    };
    let Some((file, row)) = file_row.rsplit_once(':') else {
        return Ok(None);
    };
    if !source_path(file) {
        return Ok(None);
    }
    if !positive_decimal(row, 10) || !positive_decimal(column, 10) {
        return Err(());
    }
    // Parse the exact borrowed suffix, not a concatenated replacement body.
    let diagnostic = &line[line.len() - diagnostic.len() - "error TS".len()..];
    let diagnostic = header(diagnostic)?.ok_or(())?;
    Ok(Some(Record {
        diagnostic,
        location: Some(Location { file, row }),
        has_context: true,
    }))
}

/// Restrict normalization to the SGR attributes emitted by tsc. Uncolored
/// reports borrow their input. Every caller bounds input before normalization.
fn strip_sgr(text: &str) -> Result<Cow<'_, str>, ()> {
    if !text.contains('\u{1b}') {
        return Ok(Cow::Borrowed(text));
    }
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find('\u{1b}') {
        output.push_str(&rest[..position]);
        let sequence = rest[position..].strip_prefix("\u{1b}[").ok_or(())?;
        let end = sequence.find('m').filter(|end| *end <= 32).ok_or(())?;
        if !sequence[..end].split(';').all(|parameter| {
            parameter.is_empty()
                || (parameter.len() <= 3
                    && parameter.bytes().all(|byte| byte.is_ascii_digit())
                    && parameter.parse::<u8>().is_ok_and(|value| {
                        matches!(value, 0 | 1 | 7 | 22 | 27 | 30..=37 | 39 | 90..=97)
                    }))
        }) {
            return Err(());
        }
        rest = &sequence[end + 1..];
    }
    output.push_str(rest);
    Ok(Cow::Owned(output))
}

/// The printed line number marks source data even when its text resembles a
/// complete diagnostic. Only located color/context records may have gutters.
fn source_gutter(line: &str) -> bool {
    let line = line.trim_start_matches(' ');
    line.trim_end() == "..."
        || line
            .split_once(' ')
            .is_some_and(|(number, _)| positive_decimal(number, 10))
}

type Files<'a> = BTreeMap<&'a str, (usize, &'a str)>;

fn summary(total: usize, files: &Files<'_>) -> String {
    match (total, files.len()) {
        (1, 0) => "Found 1 error.".to_owned(),
        (_, 0) => format!("Found {total} errors."),
        (_, 1) => {
            let Some((file, (_, row))) = files.first_key_value() else {
                return String::new();
            };
            if total == 1 {
                format!("Found 1 error in {file}:{row}")
            } else {
                format!("Found {total} errors in the same file, starting at: {file}:{row}")
            }
        }
        (_, count) => format!("Found {total} errors in {count} files."),
    }
}

fn check_footer<'a>(mut lines: impl Iterator<Item = &'a str>, files: &Files<'_>) -> Result<(), ()> {
    if files.len() <= 1 {
        return if lines.next().is_none() { Ok(()) } else { Err(()) };
    }
    if lines.next().map(str::trim_start) != Some("Errors  Files") {
        return Err(());
    }
    let mut seen = BTreeSet::new();
    for line in lines {
        if line.chars().any(char::is_control) {
            return Err(());
        }
        let (count, reference) = line.trim_start_matches(' ').split_once("  ").ok_or(())?;
        let (file, row) = reference.rsplit_once(':').ok_or(())?;
        if !positive_decimal(count, 3)
            || !positive_decimal(row, 10)
            || !seen.insert(file)
            || !files.get(file).is_some_and(|(expected_count, expected_row)| {
                count.parse::<usize>() == Ok(*expected_count) && row == *expected_row
            })
        {
            return Err(());
        }
    }
    if seen.len() != files.len() {
        return Err(());
    }
    Ok(())
}

pub(super) fn decode(text: &str) -> Result<Option<Vec<CanonicalDiagnostic>>, ()> {
    // Even family detection is byte-bounded: a giant first line or run of
    // blank/style-only lines must not allocate or scan an unbounded report.
    let probe = super::super::bounded(text);
    let mut claimed = false;
    for raw in probe.lines() {
        let line = match strip_sgr(raw) {
            Ok(line) => line,
            Err(()) if raw.contains("error") && raw.contains("TS") => return Err(()),
            Err(()) => return Ok(None),
        };
        if line.trim().is_empty() {
            continue;
        }
        claimed = record(&line)?.is_some();
        break;
    }
    if !claimed {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    let text = strip_sgr(text)?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let mut files = Files::new();
    let mut keys = BTreeSet::new();
    let mut diagnostics = Vec::new();
    let mut total = 0;
    let mut has_context = false;
    while let Some(line) = lines.next() {
        if line.chars().any(char::is_control) {
            return Err(());
        }
        if line.starts_with("Found ") {
            if line != summary(total, &files) {
                return Err(());
            }
            check_footer(lines, &files)?;
            return Ok(Some(diagnostics));
        }
        if has_context && source_gutter(line) {
            continue;
        }
        if let Some(record) = record(line)? {
            total += 1;
            if total > MAX_RECORDS {
                return Err(());
            }
            has_context = record.has_context;
            if let Some(source) = record.location {
                let file = files.entry(source.file).or_insert((0, source.row));
                file.0 += 1;
            }
            if keys.insert(record.diagnostic.code)
                && diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
            {
                diagnostics.push(canonical(&record.diagnostic));
            }
        } else if !line.starts_with("  ") {
            return Err(());
        }
    }
    Ok(Some(diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics,
    };

    fn native(file: &str, row: u32, code: u32) -> String {
        format!(
            "{file}:{row}:9 - error TS{code}: Type 'string' is not assignable to type 'number'.\n\n{row} const value: number = 'wrong';\n          ~~~~~\n"
        )
    }

    fn colored() -> String {
        "\u{1b}[96msrc/widget.ts\u{1b}[0m:\u{1b}[93m7\u{1b}[0m:\u{1b}[93m9\u{1b}[0m - \u{1b}[91merror\u{1b}[0m\u{1b}[90m TS2322: \u{1b}[0mType 'string' is not assignable to type 'number'.\n\n\u{1b}[7m7\u{1b}[0m const value: number = 'wrong';\n\u{1b}[7m \u{1b}[0m         \u{1b}[91m~~~~~\u{1b}[0m\n\nFound 1 error in src/widget.ts\u{1b}[90m:7\u{1b}[0m\n".to_owned()
    }

    fn optimistic(text: String) -> ToolOutput {
        ToolOutput {
            text,
            is_error: Some(false),
            exit_code: Some(0),
        }
    }

    #[test]
    fn colored_context_and_plain_output_recall_the_same_native_error() {
        let plain = "src/widget.ts(7,9): error TS2322: Type 'string' is not assignable to type 'number'.";
        let expected = structured_error_diagnostics(plain);
        assert_eq!(expected.len(), 1);
        for text in [colored(), native("src/widget.ts", 7, 2322)] {
            assert_eq!(structured_error_diagnostics(&text), expected);
            let output = optimistic(text);
            assert_eq!(failure_diagnostics(&output), expected);
            assert!(output.failed());
            assert!(!output.succeeded());
        }
    }

    #[test]
    fn global_first_reports_preserve_later_located_plain_diagnostics() {
        let global = "error TS18003: No inputs were found.\n";
        let located = "src/a.ts(2,1): error TS2322: Invalid type.\n";
        for text in [format!("{global}{located}"), format!("{located}{global}")] {
            let diagnostics = structured_error_diagnostics(&text);
            assert_eq!(diagnostics.len(), 2);
            assert!(
                diagnostics
                    .iter()
                    .any(|item| item.layered_key().key == "shell:tsc:TS2322")
            );
            assert!(!optimistic(text).succeeded());
        }
    }

    #[test]
    fn gutters_related_information_and_message_chains_never_create_fingerprints() {
        let text = format!(
            "{}\n8 src/other.ts:1:1 - error TS9999: SOURCE_SENTINEL\n  ~~~~~\n  RelatedType: nested message.\n  src/related.ts:2:1\n    2 error TS9876: RELATED_SENTINEL\n      ~~~~~\n    The expected type comes from a property declared here.\nFound 1 error in src/widget.ts:7\n",
            native("src/widget.ts", 7, 2322)
        );
        let diagnostics = structured_error_diagnostics(&text);
        assert_eq!(diagnostics.len(), 1);
        let printed = format!("{diagnostics:?}");
        assert!(!printed.contains("SENTINEL"));
        assert!(!printed.contains("related.ts"));
    }

    #[test]
    fn same_file_and_multi_file_summaries_agree_with_every_error_record() {
        let first = native("a.ts", 2, 2322);
        let second = native("a.ts", 8, 2345);
        let third = native("b.ts", 4, 2307);
        let same =
            format!("{first}{second}\nFound 2 errors in the same file, starting at: a.ts:2\n");
        assert_eq!(structured_error_diagnostics(&same).len(), 2);
        let multi = format!(
            "{first}{second}{third}\nFound 3 errors in 2 files.\n\nErrors  Files\n     2  a.ts:2\n     1  b.ts:4\n"
        );
        assert_eq!(structured_error_diagnostics(&multi).len(), 3);
        for invalid in [
            multi.replace("Found 3", "Found 2"),
            multi.replace("in 2 files.", "in 3 files."),
            multi.replace("     2  a.ts:2", "     1  a.ts:2"),
            multi.replace("b.ts:4\n", "c.ts:4\n"),
            multi.replace("     1  b.ts:4\n", ""),
            format!("{multi}     1  b.ts:4\n"),
            format!("{multi}Process exited with code 0\n"),
        ] {
            assert!(decode(&invalid).is_err(), "{invalid}");
            assert!(structured_error_diagnostics(&invalid).is_empty());
        }
    }

    #[test]
    fn global_diagnostics_and_global_summary_have_no_fabricated_locations() {
        for text in [
            "\u{1b}[91merror\u{1b}[0m\u{1b}[90m TS18003: \u{1b}[0mNo inputs were found.\nFound 1 error.\n",
            "error TS18003: No inputs were found.\nerror TS18003: No other inputs were found.\nFound 2 errors.\n",
        ] {
            let diagnostics = structured_error_diagnostics(text);
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].layered_key().key, "shell:tsc:TS18003");
        }
    }

    #[test]
    fn source_and_summary_locations_preserve_windows_colons_and_spaces() {
        for file in [
            "C:\\private\\widget.ts",
            "/private/my project/(generated)/widget.tsx",
            "app/[id]/page.ts",
            "project - one/widget.ts",
        ] {
            let text = format!("{}\nFound 1 error in {file}:7\n", native(file, 7, 2322));
            assert_eq!(structured_error_diagnostics(&text).len(), 1, "{file}");
        }
    }

    #[test]
    fn unsupported_controls_cannot_erase_or_rewrite_an_error() {
        for sequence in [
            "\u{1b}[2J",
            "\u{1b}[1A",
            "\u{1b}]8;;https://example.invalid\u{7}",
            "\u{1b}[999m",
            "\u{1b}[91",
        ] {
            for text in [
                format!("{sequence}{}", native("a.ts", 1, 2322)),
                format!("{}{sequence}", native("a.ts", 1, 2322)),
            ] {
                assert!(decode(&text).is_err(), "{text:?}");
                assert!(!optimistic(text).succeeded());
            }
        }
        assert_eq!(strip_sgr("\u{1b}[1;91merror\u{1b}[0m").unwrap(), "error");
    }

    #[test]
    fn colored_other_producers_and_nested_examples_remain_other_producers() {
        for text in [
            format!("Example:\n{}", colored()),
            format!("```text\n{}\n```", colored()),
            serde_json::json!({"example": colored()}).to_string(),
            "\u{1b}[32mfinished\u{1b}[0m".to_owned(),
            "\u{1b}[2Jdifferent tool".to_owned(),
        ] {
            assert!(decode(&text).unwrap().is_none());
        }
    }

    #[test]
    fn malformed_headers_summaries_and_watch_trailers_are_not_clean_proofs() {
        for text in [
            "a.ts:0:1 - error TS2322: Invalid row.".to_owned(),
            "a.ts:1:0 - error TS2322: Invalid column.".to_owned(),
            "a.ts:1:1 - error TSx: Invalid code.".to_owned(),
            format!("{}Found 0 errors.\n", native("a.ts", 1, 2322)),
            format!("{}Found 1 error in a.ts:2\n", native("a.ts", 1, 2322)),
            format!(
                "{}Found 1 error. Watching for file changes.\n",
                native("a.ts", 1, 2322)
            ),
        ] {
            assert!(decode(&text).is_err(), "{text}");
            assert!(structured_error_diagnostics(&text).is_empty());
            assert!(!optimistic(text).succeeded());
        }
    }

    #[test]
    fn limits_cover_original_bytes_and_records_beyond_the_presentation_cap() {
        let exact = "error TS2322: Invalid type.\n".repeat(MAX_RECORDS);
        assert_eq!(structured_error_diagnostics(&exact).len(), 1);
        assert!(decode(&format!("{exact}error TS2322: One too many.\n")).is_err());
        let styled = format!(
            "{}{}",
            colored(),
            "\u{1b}[0m".repeat(MAX_SCANNED_OUTPUT_BYTES / 4)
        );
        assert!(decode(&styled).is_err());
        let five = (1000..1005)
            .map(|code| native("a.ts", 1, code))
            .collect::<String>();
        assert_eq!(
            structured_error_diagnostics(&five).len(),
            MAX_DIAGNOSTICS_PER_FAILURE
        );
        assert!(decode(&format!("{five}a.ts:1:1 - error TSx: Broken tail.\n")).is_err());
    }
}
