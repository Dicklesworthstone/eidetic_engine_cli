//! Contract coverage for `From<io::Error> for CassError` (bd-14ceu).
//!
//! `src/cass/error.rs:166` wraps any `std::io::Error` into
//! `CassError::Io { message: io_error.to_string() }`. Every `?`-operator
//! in the cass subprocess path (e.g. `cmd.spawn()?`, stream reads,
//! pipe drains) flows through this impl, so the wrapped variant and
//! message-string mapping are part of the public surface. Today no
//! test under `tests/` pins either the variant target or the message
//! contract.
//!
//! Sister to bd-zp0dh (From<CassError>/From<DbError> for CassImportError).

use std::io;

use ee::cass::CassError;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn from_io_error_wraps_into_io_variant() -> TestResult {
    let inner = io::Error::new(io::ErrorKind::NotFound, "synthetic cass spawn failure");
    let wrapped: CassError = inner.into();
    match wrapped {
        CassError::Io { .. } => Ok(()),
        other => Err(format!(
            "From<io::Error> for CassError must produce CassError::Io {{ .. }}; got {other:?}"
        )),
    }
}

#[test]
fn from_io_error_preserves_message_via_to_string() -> TestResult {
    let inner = io::Error::new(
        io::ErrorKind::PermissionDenied,
        "cass binary not executable",
    );
    let expected_message = inner.to_string();
    let wrapped: CassError = inner.into();
    match wrapped {
        CassError::Io { message } => ensure(
            message == expected_message,
            format!(
                "io::Error::to_string() must round-trip into CassError::Io.message verbatim;\
                     \n--- expected\n{expected_message}\n+++ got\n{message}"
            ),
        ),
        other => Err(format!("expected Io, got {other:?}")),
    }
}

#[test]
fn from_io_error_kind_str_is_io() -> TestResult {
    // CassError::Io must map to the stable `io` kind string used in
    // ee.error.v2 envelopes. bd-hzk96 pins the kind vocabulary
    // separately at the kind_str surface; this test guards the path
    // io::Error -> CassError -> kind_str() jointly so the wrapping
    // does not silently route to a different variant whose kind_str
    // also returns "io".
    let inner = io::Error::other("synthetic cass io failure");
    let wrapped: CassError = inner.into();
    ensure(
        wrapped.kind_str() == "io",
        format!(
            "From<io::Error> path must yield kind_str() == \"io\"; got {:?}",
            wrapped.kind_str()
        ),
    )
}

#[test]
fn question_mark_operator_picks_up_from_io_impl() -> TestResult {
    // If the From<io::Error> impl were dropped or renamed, this closure
    // would not compile. Runtime test keeps the conversion checked at
    // call-site syntax, not just direct .into().
    fn io_path() -> Result<(), CassError> {
        let raw: Result<(), io::Error> = Err(io::Error::other("synthetic"));
        raw?;
        Ok(())
    }

    match io_path() {
        Err(CassError::Io { .. }) => Ok(()),
        other => Err(format!(
            "?-operator over io::Error must yield CassError::Io {{ .. }}; got {other:?}"
        )),
    }
}
