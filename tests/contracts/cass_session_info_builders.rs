//! Contract coverage for `CassSessionInfo` builder methods (bd-387m4).
//!
//! `CassSessionInfo` (src/cass/session.rs:123) has three pub builder
//! methods — `with_agent`, `with_workspace`, `with_content_hash` —
//! whose round-trip behavior is not pinned anywhere. The `new()`
//! defaults are pinned in `tests/contracts/cass_session_info_defaults.rs`
//! (bd-rja7x), but the builders themselves have no dedicated contract
//! test.
//!
//! `with_content_hash` carries a non-obvious side effect: it also sets
//! `content_hash_source` to the literal string `"provided"`. A future
//! refactor that drops the side-effect, or changes the marker to
//! `"caller_supplied"`, would silently break downstream content-hash
//! provenance attribution without any test failing.
//!
//! This file pins:
//!   - `with_agent(a)` sets `agent = a` (round-trip).
//!   - `with_workspace(p)` wraps the argument into `Some(p)`.
//!   - `with_content_hash(h)` sets both `content_hash = Some(h)` AND
//!     `content_hash_source = Some("provided")` atomically.
//!
//! Mirrors bd-rja7x / bd-2whz8 bounded-contract pin pattern:
//! deterministic, no fixtures, no new public API.

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

fn fresh() -> CassSessionInfo {
    CassSessionInfo::new("/tmp/session.jsonl")
}

#[test]
fn with_agent_replaces_default_unknown_agent() -> TestResult {
    let session = fresh().with_agent(CassAgent::ClaudeCode);
    ensure_equal(
        &session.agent,
        &CassAgent::ClaudeCode,
        "with_agent must replace the default Unknown agent",
    )
}

#[test]
fn with_agent_round_trips_each_known_agent_variant() -> TestResult {
    for agent in [
        CassAgent::ClaudeCode,
        CassAgent::Codex,
        CassAgent::Cursor,
        CassAgent::Gemini,
        CassAgent::ChatGpt,
        CassAgent::Unknown,
    ] {
        let session = fresh().with_agent(agent);
        if session.agent != agent {
            return Err(format!(
                "with_agent({agent:?}) round-trip failed: agent={:?}",
                session.agent
            ));
        }
    }
    Ok(())
}

#[test]
fn with_workspace_wraps_argument_into_some() -> TestResult {
    let session = fresh().with_workspace("/tmp/project");
    ensure_equal(
        &session.workspace_dir,
        &Some("/tmp/project".to_string()),
        "with_workspace must wrap the argument into Some(...)",
    )
}

#[test]
fn with_content_hash_sets_hash_and_marks_source_as_provided() -> TestResult {
    // Guard the non-obvious side effect: with_content_hash MUST also
    // set content_hash_source to "provided". This marker is what
    // downstream provenance attribution distinguishes "agent-supplied"
    // from "derived" or "missing" hashes.
    let session = fresh().with_content_hash("blake3:0123456789abcdef");
    ensure_equal(
        &session.content_hash,
        &Some("blake3:0123456789abcdef".to_string()),
        "with_content_hash must set content_hash to Some(arg)",
    )?;
    ensure_equal(
        &session.content_hash_source,
        &Some("provided".to_string()),
        "with_content_hash must also set content_hash_source to \"provided\" — \
         this marker drives downstream provenance attribution",
    )
}

#[test]
fn builders_chain_without_clobbering_each_other() -> TestResult {
    // The three builders are independent: chaining them must preserve
    // every individually-set field. This guards against future
    // accidental field-reset bugs (e.g. with_workspace clearing the
    // agent field).
    let session = fresh()
        .with_agent(CassAgent::Codex)
        .with_workspace("/tmp/project")
        .with_content_hash("blake3:deadbeef");

    ensure_equal(&session.agent, &CassAgent::Codex, "agent survives chain")?;
    ensure_equal(
        &session.workspace_dir,
        &Some("/tmp/project".to_string()),
        "workspace_dir survives chain",
    )?;
    ensure_equal(
        &session.content_hash,
        &Some("blake3:deadbeef".to_string()),
        "content_hash survives chain",
    )?;
    ensure_equal(
        &session.content_hash_source,
        &Some("provided".to_string()),
        "content_hash_source survives chain",
    )
}
