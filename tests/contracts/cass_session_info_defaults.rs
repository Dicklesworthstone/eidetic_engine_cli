//! Contract coverage for `CassSessionInfo::new` defaults (bd-rja7x).
//!
//! Sister to bd-2bwqd (CassImportOptions::new defaults). Today the inline
//! test `cass_session_info_builder_works` in `src/cass/session.rs` only
//! exercises the explicit-set builder path; the per-field defaults
//! produced by `CassSessionInfo::new(source_path)` are unpinned. Silently
//! changing the default `agent` from `Unknown` to a concrete agent, or
//! seeding `missing_metadata` with a non-empty vector, would alter the
//! import behavior across every CASS session row without surfacing in
//! any test.

use ee::cass::{CassAgent, CassSessionInfo};

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
fn new_preserves_source_path_argument() -> TestResult {
    let info = CassSessionInfo::new("/path/to/session.jsonl");
    ensure_equal(
        &info.source_path,
        &"/path/to/session.jsonl".to_string(),
        "source_path round-trips into String verbatim",
    )
}

#[test]
fn new_defaults_agent_to_unknown() -> TestResult {
    let info = CassSessionInfo::new("/path/to/session.jsonl");
    ensure_equal(
        &info.agent,
        &CassAgent::Unknown,
        "agent default must be Unknown so the importer treats unflagged sessions conservatively",
    )
}

#[test]
fn new_defaults_workspace_dir_to_none() -> TestResult {
    let info = CassSessionInfo::new("/path");
    ensure_equal(&info.workspace_dir, &None, "workspace_dir default")
}

#[test]
fn new_defaults_started_and_ended_at_to_none() -> TestResult {
    let info = CassSessionInfo::new("/path");
    ensure_equal(&info.started_at, &None, "started_at default")?;
    ensure_equal(&info.ended_at, &None, "ended_at default")
}

#[test]
fn new_defaults_message_and_token_counts_to_none() -> TestResult {
    let info = CassSessionInfo::new("/path");
    ensure_equal(&info.message_count, &None, "message_count default")?;
    ensure_equal(&info.token_count, &None, "token_count default")
}

#[test]
fn new_defaults_content_hash_and_source_to_none() -> TestResult {
    let info = CassSessionInfo::new("/path");
    ensure_equal(&info.content_hash, &None, "content_hash default")?;
    ensure_equal(
        &info.content_hash_source,
        &None,
        "content_hash_source default",
    )
}

#[test]
fn new_defaults_missing_metadata_to_empty_vector() -> TestResult {
    let info = CassSessionInfo::new("/path");
    ensure_equal(
        &info.missing_metadata,
        &Vec::<String>::new(),
        "missing_metadata starts empty; the parser pushes only for genuinely missing CASS metadata fields",
    )
}

#[test]
fn new_returns_full_default_struct() -> TestResult {
    // Single struct-shaped equality pin so any silently added field that
    // bypasses the per-field tests must still match the full default
    // snapshot to compile, and any default value change surfaces as a
    // diff in this test's output. Mirrors the bd-2bwqd pattern.
    let info = CassSessionInfo::new("/path/to/session.jsonl");
    let expected = CassSessionInfo {
        source_path: "/path/to/session.jsonl".to_string(),
        agent: CassAgent::Unknown,
        workspace_dir: None,
        started_at: None,
        ended_at: None,
        message_count: None,
        token_count: None,
        content_hash: None,
        missing_metadata: Vec::new(),
        content_hash_source: None,
    };
    ensure_equal(&info, &expected, "full default struct equality")
}
