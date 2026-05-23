//! Contract coverage for the public bounded CASS parser summaries
//! (bd-3ry2a).
//!
//! `ee::cass::parse_sessions_json_summary` and
//! `ee::cass::parse_view_json_summary` are the only public surfaces that
//! re-enter the importer's CASS-JSON parsers without going through
//! `import_cass_sessions`. Today they are exercised only by the fuzz
//! harnesses in `fuzz/fuzz_targets/cass_import_jsonl.rs` and
//! `fuzz/fuzz_targets/cass_envelope_decoder.rs`; no contract test pins
//! either the `CassImportParseSummary` field shape or the error variant
//! for malformed input.

use ee::cass::{CassImportError, parse_sessions_json_summary, parse_view_json_summary};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

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

#[test]
fn parse_sessions_json_summary_counts_accepted_items() -> TestResult {
    let input =
        br#"{"sessions":[{"path":"session-aaa"},{"path":"session-bbb"},{"path":"session-ccc"}]}"#;
    let summary = parse_sessions_json_summary(input)
        .map_err(|error| format!("expected Ok, got {error:?}"))?;

    ensure_equal(
        &summary.accepted_items,
        &3_u32,
        "accepted_items reflects session count",
    )?;
    ensure_equal(
        &summary.max_line,
        &0_u32,
        "sessions summary does not surface line numbers (only views do)",
    )?;
    ensure_equal(
        &summary.max_excerpt_bytes,
        &0_usize,
        "sessions summary does not surface excerpt bytes",
    )
}

#[test]
fn parse_sessions_json_summary_empty_sessions_returns_zero_count() -> TestResult {
    let input = br#"{"sessions":[]}"#;
    let summary = parse_sessions_json_summary(input)
        .map_err(|error| format!("expected Ok, got {error:?}"))?;
    ensure_equal(&summary.accepted_items, &0_u32, "empty sessions array")
}

#[test]
fn parse_sessions_json_summary_rejects_non_json_input() -> TestResult {
    let input = b"not json {";
    match parse_sessions_json_summary(input) {
        Err(CassImportError::InvalidJson { source, .. }) => ensure_equal(
            &source,
            &"sessions",
            "InvalidJson source tag must be \"sessions\"",
        ),
        Err(other) => Err(format!("expected InvalidJson, got {other:?}")),
        Ok(summary) => Err(format!("expected InvalidJson, got Ok({summary:?})")),
    }
}

#[test]
fn parse_sessions_json_summary_rejects_missing_sessions_array() -> TestResult {
    // The parser also rejects valid JSON that lacks the `sessions` array
    // (and is not a legacy `hits` payload). Pinning the rejection
    // surface stops a future agent from quietly accepting bare envelopes.
    let input = br#"{"not_sessions":[]}"#;
    match parse_sessions_json_summary(input) {
        Err(CassImportError::InvalidJson { source, message }) => {
            ensure_equal(&source, &"sessions", "InvalidJson source tag")?;
            ensure(
                message.contains("missing sessions array"),
                format!("error message must explain missing sessions array; got {message:?}"),
            )
        }
        Err(other) => Err(format!("expected InvalidJson, got {other:?}")),
        Ok(summary) => Err(format!("expected InvalidJson, got Ok({summary:?})")),
    }
}

#[test]
fn parse_view_json_summary_reports_max_line_and_max_excerpt_bytes() -> TestResult {
    // JSONL-shape: one span per line, line numbers must be positive and
    // strictly distinct, and the content excerpt drives max_excerpt_bytes.
    let input = b"\
        {\"line\":1,\"content\":\"hello\"}\n\
        {\"line\":2,\"content\":\"world!!!!!\"}\n\
        {\"line\":5,\"content\":\"hi\"}\n\
    ";
    let summary = parse_view_json_summary(input, "/tmp/cass-view.jsonl")
        .map_err(|error| format!("expected Ok, got {error:?}"))?;

    ensure_equal(&summary.accepted_items, &3_u32, "accepted_items per span")?;
    ensure_equal(
        &summary.max_line,
        &5_u32,
        "max_line tracks the largest end_line across spans",
    )?;
    ensure_equal(
        &summary.max_excerpt_bytes,
        &10_usize,
        "max_excerpt_bytes tracks the longest excerpt (\"world!!!!!\" is 10 bytes)",
    )
}

#[test]
fn parse_view_json_summary_empty_input_returns_zero_counts() -> TestResult {
    let summary = parse_view_json_summary(b"", "/tmp/empty-view.jsonl")
        .map_err(|error| format!("expected Ok on empty input, got {error:?}"))?;
    ensure_equal(
        &summary.accepted_items,
        &0_u32,
        "empty input accepted_items",
    )?;
    ensure_equal(&summary.max_line, &0_u32, "empty input max_line")?;
    ensure_equal(
        &summary.max_excerpt_bytes,
        &0_usize,
        "empty input max_excerpt_bytes",
    )
}

#[test]
fn parse_view_json_summary_rejects_non_utf8_input() -> TestResult {
    // Invalid UTF-8 must surface as InvalidJson with source="view" so
    // ingest can distinguish from a malformed-envelope failure.
    let input = b"\xff\xfe not valid utf-8";
    match parse_view_json_summary(input, "/tmp/bad-view.jsonl") {
        Err(CassImportError::InvalidJson { source, .. }) => {
            ensure_equal(&source, &"view", "InvalidJson source tag must be \"view\"")
        }
        Err(other) => Err(format!("expected InvalidJson, got {other:?}")),
        Ok(summary) => Err(format!("expected InvalidJson, got Ok({summary:?})")),
    }
}
