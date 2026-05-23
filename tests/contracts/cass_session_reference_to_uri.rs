//! Contract coverage for `CassSessionReference::to_uri` (bd-2wjz2).
//!
//! Pin the exact string format produced by `to_uri` for each
//! line-start/line-end combination. `bd-gb01r`
//! (`cass_session_uri_contract`) already pins
//! `normalize_cass_session_uri` parsing and asserts a round-trip is
//! stable, but it does not pin the exact rendered string — a future
//! refactor could swap `#L10-20` for `#L10-L20` (or vice versa) and the
//! round-trip would still pass because `normalize_cass_session_uri`
//! accepts both shapes.
//!
//! This contract closes that gap by asserting the rendered URI string
//! byte-for-byte for the four canonical shapes.

use ee::cass::CassSessionReference;

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
fn to_uri_renders_bare_session_id_without_fragment() -> TestResult {
    let r = reference("abc123", None, None);
    ensure_equal(
        &r.to_uri(),
        &"cass-session://abc123".to_string(),
        "no line_start -> no fragment",
    )
}

#[test]
fn to_uri_renders_single_line_anchor_without_range() -> TestResult {
    // Equal start/end must collapse to the single-line `#L42` form.
    let r = reference("abc123", Some(42), Some(42));
    ensure_equal(
        &r.to_uri(),
        &"cass-session://abc123#L42".to_string(),
        "line_start == line_end collapses to single anchor",
    )
}

#[test]
fn to_uri_renders_range_without_second_l_prefix() -> TestResult {
    // The canonical rendered form is `#L<start>-<end>` (one L, then the
    // range). normalize_cass_session_uri also accepts `#L10-L20` on the
    // input side, but to_uri must produce the bare-dash form.
    let r = reference("abc123", Some(10), Some(20));
    ensure_equal(
        &r.to_uri(),
        &"cass-session://abc123#L10-20".to_string(),
        "line_start != line_end renders #L<start>-<end>",
    )
}

#[test]
fn to_uri_treats_missing_line_end_as_line_start() -> TestResult {
    // line_end == None must behave as if line_end == line_start: a
    // single anchor with no '-' suffix.
    let r = reference("abc123", Some(7), None);
    ensure_equal(
        &r.to_uri(),
        &"cass-session://abc123#L7".to_string(),
        "line_end None collapses to single anchor",
    )
}

#[test]
fn to_uri_preserves_punctuated_session_id_verbatim() -> TestResult {
    let r = reference("session-2026.05.23_run_01", None, None);
    ensure_equal(
        &r.to_uri(),
        &"cass-session://session-2026.05.23_run_01".to_string(),
        "session_id with -, ., _ preserved verbatim",
    )
}

#[test]
fn to_uri_does_not_render_fragment_when_line_start_is_none() -> TestResult {
    // Even if line_end is Some, line_start being None must suppress the
    // entire fragment. The implementation reads `if let Some(line_start)`
    // — line_end alone cannot produce a `#L` prefix.
    let r = reference("abc123", None, Some(99));
    ensure_equal(
        &r.to_uri(),
        &"cass-session://abc123".to_string(),
        "line_start None suppresses fragment even when line_end is Some",
    )
}
