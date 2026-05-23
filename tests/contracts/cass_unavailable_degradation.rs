//! Contract coverage for the `cass_unavailable` degradation code routing
//! in `ee.error.v2` envelopes (bd-33t39).
//!
//! `src/output/mod.rs::domain_error_degraded` (private) maps any
//! `DomainError::Import` or `DomainError::ImportWithDetails` whose
//! message contains the case-insensitive substring "cass binary" to
//! `ErrorDegradation { code: "cass_unavailable", severity: "medium",
//! .. }`. This routing surfaces through the public `error_response_json`
//! into the ee.error.v2 envelope's `degraded[]` array.
//!
//! Today the routing is exercised end-to-end by
//! `tests/usr005_degraded_scenario.rs` (which boots the real ee binary
//! with no cass binary discoverable). No unit-level pin asserts the
//! mapping at the JSON envelope boundary. A future agent who renames
//! the substring trigger ("cass binary" -> "cass executable"), changes
//! the severity from "medium" to "high", or applies the mapping to
//! variants other than Import/ImportWithDetails would break the
//! agent-facing degradation contract without surfacing in any unit
//! test.

use ee::models::DomainError;
use ee::output::error_response_json;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn domain_error_import_with_cass_binary_message_routes_to_cass_unavailable() -> TestResult {
    let error = DomainError::Import {
        message: "cass binary not found on $PATH".to_string(),
        repair: Some("install cass or set [cass.binary] in config".to_string()),
    };
    let json = error_response_json(&error);
    ensure(
        json.contains(r#""code":"cass_unavailable""#),
        format!("ee.error.v2 envelope must contain code=\"cass_unavailable\"; got {json}"),
    )?;
    ensure(
        json.contains(r#""severity":"medium""#),
        format!("cass_unavailable degradation must have severity=\"medium\"; got {json}"),
    )
}

#[test]
fn domain_error_import_with_details_routes_to_cass_unavailable() -> TestResult {
    // The wrapper variant `ImportWithDetails` carries diagnostics JSON
    // alongside the message. The substring trigger must still apply
    // to the message field, not the details.
    let error = DomainError::ImportWithDetails {
        message: "cass binary not executable".to_string(),
        repair: Some("set EE_CASS_BINARY to a trusted executable".to_string()),
        details_json: r#"{"subprocessDiagnostics":{"schema":"ee.cass.subprocess_diagnostics.v1"}}"#
            .to_string(),
    };
    let json = error_response_json(&error);
    ensure(
        json.contains(r#""code":"cass_unavailable""#),
        format!("ImportWithDetails must also route to cass_unavailable; got {json}"),
    )?;
    ensure(
        json.contains(r#""severity":"medium""#),
        format!("severity must be medium; got {json}"),
    )
}

#[test]
fn cass_binary_substring_match_is_case_insensitive() -> TestResult {
    // src/output/mod.rs lowercases the message before substring
    // search, so "Cass Binary" must trigger the same routing as
    // "cass binary".
    let error = DomainError::Import {
        message: "Cass Binary unavailable".to_string(),
        repair: None,
    };
    let json = error_response_json(&error);
    ensure(
        json.contains(r#""code":"cass_unavailable""#),
        format!("case-insensitive match must trigger cass_unavailable; got {json}"),
    )
}

#[test]
fn non_import_variants_do_not_route_to_cass_unavailable() -> TestResult {
    // Even if a Storage error message happens to mention "cass binary",
    // the routing must not fire — only Import / ImportWithDetails are
    // the source variants.
    let error = DomainError::Storage {
        message: "cass binary checksum mismatch in cache".to_string(),
        repair: None,
    };
    let json = error_response_json(&error);
    ensure(
        !json.contains(r#""code":"cass_unavailable""#),
        format!(
            "Storage variant must NOT route to cass_unavailable even when message mentions cass binary; got {json}"
        ),
    )
}

#[test]
fn import_error_without_cass_binary_substring_does_not_route() -> TestResult {
    // Import errors whose messages don't contain "cass binary" must
    // not route through the cass_unavailable path.
    let error = DomainError::Import {
        message: "session JSONL parse failed at line 42".to_string(),
        repair: None,
    };
    let json = error_response_json(&error);
    ensure(
        !json.contains(r#""code":"cass_unavailable""#),
        format!(
            "Import without 'cass binary' substring must NOT route to cass_unavailable; got {json}"
        ),
    )
}
