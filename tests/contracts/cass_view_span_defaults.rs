//! Contract coverage for `CassViewSpan::new` defaults and `with_role`
//! builder (bd-20dng).
//!
//! Sister to bd-rja7x (CassSessionInfo::new defaults) and bd-2bwqd
//! (CassImportOptions::new defaults). Today the inline tests in
//! `src/cass/session.rs` only cover `CassViewSpan::line_count()`:
//!
//! * `cass_view_span_line_count_is_correct`
//! * `cass_view_span_line_count_rejects_inverted_ranges`
//!
//! Nothing pins the per-field defaults from `new()` or the behavior of
//! the `with_role` builder. Silently flipping the default role from
//! `None` to `Some(CassRole::User)` would alter every imported
//! view-span without surfacing in any test.

use ee::cass::{CassRole, CassSpanKind, CassViewSpan};

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

fn fresh_span() -> CassViewSpan {
    CassViewSpan::new(
        "/path/to/session.jsonl",
        "session-aaa:42",
        CassSpanKind::Message,
        10,
        12,
        "hello world",
        "blake3:abc",
    )
}

#[test]
fn new_preserves_source_path_argument() -> TestResult {
    ensure_equal(
        &fresh_span().source_path,
        &"/path/to/session.jsonl".to_string(),
        "source_path round-trips into String verbatim",
    )
}

#[test]
fn new_preserves_cass_span_id_argument() -> TestResult {
    ensure_equal(
        &fresh_span().cass_span_id,
        &"session-aaa:42".to_string(),
        "cass_span_id round-trips into String verbatim",
    )
}

#[test]
fn new_preserves_span_kind_argument() -> TestResult {
    ensure_equal(
        &fresh_span().span_kind,
        &CassSpanKind::Message,
        "span_kind round-trips verbatim",
    )
}

#[test]
fn new_preserves_line_range_arguments() -> TestResult {
    let span = fresh_span();
    ensure_equal(&span.start_line, &10_u32, "start_line round-trips")?;
    ensure_equal(&span.end_line, &12_u32, "end_line round-trips")
}

#[test]
fn new_preserves_excerpt_and_content_hash_arguments() -> TestResult {
    let span = fresh_span();
    ensure_equal(
        &span.excerpt,
        &"hello world".to_string(),
        "excerpt round-trips into String verbatim",
    )?;
    ensure_equal(
        &span.content_hash,
        &"blake3:abc".to_string(),
        "content_hash round-trips into String verbatim",
    )
}

#[test]
fn new_defaults_role_to_none() -> TestResult {
    ensure_equal(
        &fresh_span().role,
        &None,
        "role default must be None — role is opt-in via with_role; \
         non-message spans (ToolCall/ToolResult) often lack a conversation role",
    )
}

#[test]
fn with_role_sets_role_to_some() -> TestResult {
    let span = fresh_span().with_role(CassRole::Assistant);
    ensure_equal(
        &span.role,
        &Some(CassRole::Assistant),
        "with_role(Assistant) sets the field to Some(Assistant) — proves the builder is a real setter",
    )
}

#[test]
fn with_role_overwrites_previous_role() -> TestResult {
    // Builder reassignment: with_role(User) followed by with_role(System)
    // must end with System, not stack-into a different shape.
    let span = fresh_span()
        .with_role(CassRole::User)
        .with_role(CassRole::System);
    ensure_equal(
        &span.role,
        &Some(CassRole::System),
        "later with_role overrides earlier with_role",
    )
}
