//! Contract coverage for `ee::cass::normalize_cass_session_uri` (bd-gb01r).
//!
//! `normalize_cass_session_uri` is the gate that protects evidence-span
//! provenance from accepting traversal-shaped session ids, smuggled query
//! strings, or malformed line-range fragments. Before this file it had no
//! direct test in `src/cass/session.rs` or under `tests/`, so silently
//! relaxing any branch would leave only downstream importer assertions to
//! catch it.

use ee::cass::{CassSessionReference, normalize_cass_session_uri};

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

fn assert_ok(input: &str, expected: CassSessionReference) -> TestResult {
    let parsed = normalize_cass_session_uri(input).map_err(|error| format!("{input}: {error}"))?;
    ensure_equal(&parsed, &expected, &format!("parsed {input}"))
}

fn assert_err(input: &str, expected_reason: &'static str) -> TestResult {
    match normalize_cass_session_uri(input) {
        Ok(ok) => Err(format!(
            "{input}: expected error {expected_reason}, got Ok({ok:?})"
        )),
        Err(error) => ensure_equal(
            &error.reason(),
            &expected_reason,
            &format!("reason for {input:?}"),
        ),
    }
}

fn reference(
    session_id: &str,
    line_start: Option<u32>,
    line_end: Option<u32>,
) -> CassSessionReference {
    CassSessionReference {
        session_id: session_id.to_owned(),
        line_start,
        line_end,
    }
}

#[test]
fn normalize_accepts_bare_session_id() -> TestResult {
    assert_ok("cass-session://abc123", reference("abc123", None, None))
}

#[test]
fn normalize_trims_surrounding_whitespace_before_scheme_check() -> TestResult {
    assert_ok("  cass-session://abc123  ", reference("abc123", None, None))
}

#[test]
fn normalize_accepts_line_anchor_fragment() -> TestResult {
    assert_ok(
        "cass-session://abc123#L42",
        reference("abc123", Some(42), Some(42)),
    )
}

#[test]
fn normalize_accepts_line_range_fragment() -> TestResult {
    assert_ok(
        "cass-session://abc123#L10-L20",
        reference("abc123", Some(10), Some(20)),
    )
}

#[test]
fn normalize_accepts_punctuated_session_id() -> TestResult {
    assert_ok(
        "cass-session://session-2026.05.23_run_01",
        reference("session-2026.05.23_run_01", None, None),
    )
}

#[test]
fn normalize_rejects_missing_scheme() -> TestResult {
    assert_err("session://abc123", "missing_cass_session_scheme")?;
    assert_err("abc123", "missing_cass_session_scheme")?;
    assert_err("", "missing_cass_session_scheme")
}

#[test]
fn normalize_rejects_empty_session_id() -> TestResult {
    assert_err("cass-session://", "missing_session_id")
}

#[test]
fn normalize_rejects_query_string() -> TestResult {
    assert_err("cass-session://abc?line=10", "unsupported_uri_component")
}

#[test]
fn normalize_rejects_control_chars() -> TestResult {
    let with_newline = "cass-session://abc\n123";
    assert_err(with_newline, "unsupported_uri_component")?;
    let with_tab = "cass-session://abc\t123";
    assert_err(with_tab, "unsupported_uri_component")
}

#[test]
fn normalize_rejects_path_traversal_session_id() -> TestResult {
    assert_err("cass-session://../etc/passwd", "unsafe_session_id")?;
    assert_err("cass-session://abc..", "unsafe_session_id")?;
    assert_err("cass-session://a/b", "unsafe_session_id")?;
    assert_err("cass-session://a\\b", "unsafe_session_id")
}

#[test]
fn normalize_rejects_non_ascii_session_id() -> TestResult {
    assert_err("cass-session://sessión", "unsupported_session_id_character")
}

#[test]
fn normalize_rejects_unsupported_punctuation_in_session_id() -> TestResult {
    assert_err("cass-session://abc+123", "unsupported_session_id_character")?;
    assert_err("cass-session://abc!123", "unsupported_session_id_character")?;
    assert_err("cass-session://abc@123", "unsupported_session_id_character")?;
    assert_err("cass-session://abc:123", "unsupported_session_id_character")
}

#[test]
fn normalize_rejects_fragment_without_l_prefix() -> TestResult {
    assert_err("cass-session://abc#42", "unsupported_fragment")?;
    assert_err("cass-session://abc#start", "unsupported_fragment")
}

#[test]
fn normalize_rejects_non_numeric_line_token() -> TestResult {
    assert_err("cass-session://abc#Labc", "invalid_line_number")?;
    assert_err("cass-session://abc#L1.5", "invalid_line_number")?;
    assert_err("cass-session://abc#L-3", "invalid_line_number")
}

#[test]
fn normalize_rejects_line_number_zero() -> TestResult {
    assert_err("cass-session://abc#L0", "line_number_zero")?;
    assert_err("cass-session://abc#L0-L5", "line_number_zero")?;
    assert_err("cass-session://abc#L5-L0", "line_number_zero")
}

#[test]
fn normalize_rejects_reversed_line_range() -> TestResult {
    assert_err("cass-session://abc#L20-L10", "line_range_reversed")
}

#[test]
fn normalize_round_trips_through_to_uri() -> TestResult {
    let cases = [
        "cass-session://abc123",
        "cass-session://abc123#L42",
        "cass-session://abc123#L10-L20",
        "cass-session://session-2026.05.23_run_01",
    ];
    for raw in cases {
        let parsed = normalize_cass_session_uri(raw).map_err(|error| format!("{raw}: {error}"))?;
        let rendered = parsed.to_uri();
        let reparsed = normalize_cass_session_uri(&rendered)
            .map_err(|error| format!("round-trip {raw} -> {rendered}: {error}"))?;
        ensure_equal(&reparsed, &parsed, &format!("round-trip stable for {raw}"))?;
    }
    Ok(())
}

#[test]
fn normalize_error_display_includes_reason() -> TestResult {
    let error = normalize_cass_session_uri("session://nope").expect_err("expected error");
    let displayed = format!("{error}");
    ensure(
        displayed.contains("missing_cass_session_scheme"),
        format!("display should mention reason: {displayed}"),
    )
}
