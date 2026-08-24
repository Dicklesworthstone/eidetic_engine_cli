//! Contract coverage for `fuzz_decode_cass_stdout_stream` (bd-375ve).
//!
//! `ee::cass::fuzz_decode_cass_stdout_stream` and its
//! `CassStdoutDecodeFuzzSummary` return type are public surfaces in
//! `src/cass/process.rs:883` (declared `#[doc(hidden)]` but `pub`)
//! exposed for fuzz harnesses. They have no contract test coverage at
//! any level — no inline `#[cfg(test)]` exercise in `src/cass/process.rs`
//! and no integration test references them.
//!
//! This file pins the bounded summary fields downstream fuzz harnesses
//! match against, mirroring the bd-3ry2a `parse_*_json_summary` pattern:
//!
//! - `line_count`        — total newline-delimited records consumed
//! - `bytes_seen`        — sum of per-line byte lengths including each
//!   line's delimiter bytes (LF, or CRLF), matching
//!   `record_stdout_line_stats` semantics since d7bf4e68
//! - `peak_line_bytes`   — maximum per-line byte length (post-strip,
//!   excluding delimiter bytes)
//! - `peak_buffer_bytes` — maximum internal buffer high-water mark
//!   (includes the trailing newline byte while the line is in the buffer,
//!   so it is at least `peak_line_bytes`)
//!
//! Deterministic; no external fixtures; no new public API.

use ee::cass::process::{CassStdoutDecodeFuzzSummary, fuzz_decode_cass_stdout_stream};

type TestResult = Result<(), String>;

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: &T,
    expected: &T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn decode(input: &[u8]) -> Result<CassStdoutDecodeFuzzSummary, String> {
    fuzz_decode_cass_stdout_stream(input).map_err(|error| format!("expected Ok, got {error:?}"))
}

#[test]
fn empty_input_reports_zero_across_all_summary_fields() -> TestResult {
    let summary = decode(b"")?;
    ensure_equal(
        &summary,
        &CassStdoutDecodeFuzzSummary {
            line_count: 0,
            bytes_seen: 0,
            peak_line_bytes: 0,
            peak_buffer_bytes: 0,
        },
        "empty input summary",
    )
}

#[test]
fn single_lf_terminated_line_reports_exact_counts_and_peaks() -> TestResult {
    let summary = decode(b"hello\n")?;
    // line.text reports 5 (the trailing LF is stripped) and
    // delimiter_bytes is 1, so record_stdout_line_stats counts:
    //   line_count        = 1
    //   bytes_seen        = 5 + 1 = 6 (text + LF delimiter)
    //   peak_line_bytes   = 5 (post-strip, excludes delimiters)
    // peak_buffer_bytes captures the underlying buf.len() before the
    // LF strip, which equals 6 (the read includes the LF).
    ensure_equal(
        &summary,
        &CassStdoutDecodeFuzzSummary {
            line_count: 1,
            bytes_seen: 6,
            peak_line_bytes: 5,
            peak_buffer_bytes: 6,
        },
        "single LF line summary",
    )
}

#[test]
fn multiple_lines_sum_bytes_and_report_largest_line() -> TestResult {
    // Three lines with distinct text lengths (1, 4, 2), each with a
    // 1-byte LF delimiter, yield:
    //   line_count        = 3
    //   bytes_seen        = (1+1) + (4+1) + (2+1) = 10
    //   peak_line_bytes   = max(1, 4, 2) = 4
    //   peak_buffer_bytes = peak_line_bytes + 1 (LF byte still in buf
    //                       when the high-water mark is recorded) = 5
    let summary = decode(b"a\nbbbb\ncc\n")?;
    ensure_equal(
        &summary,
        &CassStdoutDecodeFuzzSummary {
            line_count: 3,
            bytes_seen: 10,
            peak_line_bytes: 4,
            peak_buffer_bytes: 5,
        },
        "three-line summary",
    )
}

#[test]
fn crlf_line_endings_are_stripped_before_counting() -> TestResult {
    // "abc\r\n" counts as a 3-byte text (both \r and \n stripped)
    // with a 2-byte CRLF delimiter, so bytes_seen = 3 + 2 = 5.
    // peak_buffer_bytes still captures buf.len()=5 because
    // the buffer holds the raw bytes up to and including the LF.
    let summary = decode(b"abc\r\n")?;
    ensure_equal(
        &summary,
        &CassStdoutDecodeFuzzSummary {
            line_count: 1,
            bytes_seen: 5,
            peak_line_bytes: 3,
            peak_buffer_bytes: 5,
        },
        "CRLF line summary",
    )
}

#[test]
fn trailing_line_without_newline_is_still_counted() -> TestResult {
    // "first\nsecond" yields two lines: "first" (5 text bytes + 1 LF
    // delimiter) and "second" (6 bytes, no trailing delimiter), so
    // bytes_seen = 6 + 6 = 12. peak values track the second line
    // which is longer.
    let summary = decode(b"first\nsecond")?;
    ensure_equal(&summary.line_count, &2, "trailing-no-newline line count")?;
    ensure_equal(
        &summary.bytes_seen,
        &12,
        "trailing-no-newline bytes_seen ((5+1) + 6)",
    )?;
    ensure_equal(
        &summary.peak_line_bytes,
        &6,
        "trailing-no-newline peak_line_bytes",
    )?;
    // peak_buffer_bytes must always be at least peak_line_bytes
    // because the buffer holds the line bytes (plus any delimiter
    // byte) while the high-water mark is recorded.
    ensure(
        summary.peak_buffer_bytes >= summary.peak_line_bytes,
        format!(
            "peak_buffer_bytes ({}) must be >= peak_line_bytes ({})",
            summary.peak_buffer_bytes, summary.peak_line_bytes
        ),
    )
}

#[test]
fn empty_lines_contribute_delimiter_bytes_and_increment_line_count() -> TestResult {
    // "\n\n\n" is three empty lines: text.len()=0 each, but each
    // carries a 1-byte LF delimiter, so bytes_seen = 3. Only
    // peak_line_bytes stays zero; peak_buffer_bytes captures the LF.
    let summary = decode(b"\n\n\n")?;
    ensure_equal(
        &summary,
        &CassStdoutDecodeFuzzSummary {
            line_count: 3,
            bytes_seen: 3,
            peak_line_bytes: 0,
            peak_buffer_bytes: 1,
        },
        "empty-line-only input summary",
    )
}
