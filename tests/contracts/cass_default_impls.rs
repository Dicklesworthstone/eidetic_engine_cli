//! Contract coverage for `Default` trait impls across public cass types
//! (bd-32v2x).
//!
//! Several cass types implement or derive `Default`. The resulting
//! default values flow into every default-constructed cass artifact
//! (`CassImportOptions::new` seeds `CassSessionInfo`, parsed JSON often
//! falls back to defaults for missing fields, etc.) but no test pins
//! them explicitly:
//!
//! * `CassClient::default()` (manual impl in `src/cass/client.rs:749`)
//! * `CassAgent::default()` (derived `#[default]` in `session.rs`)
//! * `CassSpanKind::default()` (derived `#[default]`)
//! * `CassRole::default()` (derived `#[default]`)
//! * `ImportCursor::default()` (derived from per-field defaults)
//!
//! Silently flipping `#[default]` from `Unknown` to `ClaudeCode` on
//! `CassAgent` or from `User` to `Assistant` on `CassRole` would alter
//! every fallback path without surfacing in any test. Sister to
//! bd-rja7x (CassSessionInfo::new defaults), bd-2bwqd (CassImportOptions
//! defaults), bd-ytz8b (CassClient::new_default defaults).

use ee::cass::{CassAgent, CassClient, CassRole, CassSpanKind, ImportCursor};

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

#[test]
fn cass_client_default_delegates_to_new_default() -> TestResult {
    ensure_equal(
        &CassClient::default(),
        &CassClient::new_default(),
        "CassClient::default() must equal CassClient::new_default() — the manual Default impl in client.rs:749 is a thin delegate",
    )
}

#[test]
fn cass_agent_default_is_unknown() -> TestResult {
    ensure_equal(
        &CassAgent::default(),
        &CassAgent::Unknown,
        "CassAgent::default() must be Unknown — flagged sessions must be opt-in, never default to ClaudeCode/Codex/etc.",
    )
}

#[test]
fn cass_span_kind_default_is_message() -> TestResult {
    ensure_equal(
        &CassSpanKind::default(),
        &CassSpanKind::Message,
        "CassSpanKind::default() must be Message — the most common span kind and the conservative fallback when CASS view JSON omits the kind",
    )
}

#[test]
fn cass_role_default_is_user() -> TestResult {
    ensure_equal(
        &CassRole::default(),
        &CassRole::User,
        "CassRole::default() must be User — the conservative fallback for ambiguous roles in CASS view spans",
    )
}

#[test]
fn import_cursor_default_equals_new() -> TestResult {
    // ImportCursor::new() returns Self::default(), so default() must
    // equal new() — and both must produce the empty starting state.
    ensure_equal(
        &ImportCursor::default(),
        &ImportCursor::new(),
        "ImportCursor::default() must equal ImportCursor::new() — both return the empty starting state",
    )
}

#[test]
fn import_cursor_default_starts_with_zero_counts() -> TestResult {
    let cursor = ImportCursor::default();
    ensure_equal(
        &cursor.total_discovered(),
        &0_u32,
        "ImportCursor::default().total_discovered() must start at 0",
    )?;
    ensure_equal(
        &cursor.sessions_imported,
        &0_u32,
        "sessions_imported starts at 0",
    )?;
    ensure_equal(
        &cursor.sessions_skipped,
        &0_u32,
        "sessions_skipped starts at 0",
    )?;
    ensure_equal(&cursor.spans_imported, &0_u32, "spans_imported starts at 0")?;
    ensure_equal(
        &cursor.last_source_path,
        &None,
        "last_source_path starts as None",
    )?;
    ensure_equal(&cursor.last_line, &None, "last_line starts as None")
}

#[test]
fn import_cursor_default_is_not_complete() -> TestResult {
    // is_complete requires sessions_discovered > 0, which a fresh
    // default cursor doesn't satisfy. Pin this so a future refactor
    // can't accidentally collapse the "no work yet" state into "done".
    let cursor = ImportCursor::default();
    if cursor.is_complete() {
        return Err(
            "ImportCursor::default() must not be marked complete (no sessions discovered yet)"
                .to_string(),
        );
    }
    Ok(())
}
