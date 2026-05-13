//! C4 contract test (eidetic_engine_cli bd-17c65.3.4).
//!
//! When `ee remember` rejects a tag or content for policy reasons,
//! the error envelope's `error.details` must include enough structured
//! information for an agent to recover without trial-and-error:
//!
//! Tag rejection (`error.code == "usage"`):
//!   - `acceptedPattern` — the regex for valid tags
//!   - `acceptedExamples` — 3+ concrete valid tags
//!   - `matchedAt[]` — byte offsets of the offending characters with
//!     reasons (`space_disallowed`, `control_disallowed`, `delimiter_reserved`, …)
//!   - `normalizedFormCandidate` — what the input would become after
//!     NFC + lowercase, useful for "try this instead" suggestions
//!   - `maxBytes` — the 64-byte cap so callers know the length bound
//!
//! Content/secret rejection (`error.code == "policy_denied"`):
//!   - `detectedPattern` / `detectedPatterns[]` — which secret-detector
//!     rule matched (e.g. `openai_sk_prefix`)
//!   - `matchedAt[]` — byte offsets with `pattern_id`
//!   - `bypassFlag` — `--allow-secret-mention` so the agent knows the
//!     override exists without scraping help text
//!   - `configKey` / `configRegexKey` — the workspace config keys that
//!     can permanently whitelist a phrase or pattern
//!
//! Both error paths are already wired in src/core/memory.rs via
//! `remember_tag_usage_error` and `refuse_secret_content`. This
//! contract test locks those payloads in so a future refactor that
//! quietly drops a field is caught at CI time.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;

use ee::db::DbConnection;
use ee::models::memory::Tag;
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

struct InitializedWorkspace {
    _dir: TempDir,
    workspace: PathBuf,
}

fn init_workspace() -> Result<InitializedWorkspace, String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let database = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(database.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);
    Ok(InitializedWorkspace {
        _dir: dir,
        workspace,
    })
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(ee_binary())
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))
}

fn parse_error_envelope(out: &std::process::Output) -> Result<Value, String> {
    let s = String::from_utf8(out.stdout.clone())
        .map_err(|error| format!("stdout not UTF-8: {error}"))?;
    let json: Value =
        serde_json::from_str(&s).map_err(|error| format!("not JSON: {error}\nout: {s}"))?;
    // Envelope may be ee.error.v1 or ee.error.v2. The current shipped
    // schema is v2; the test accepts either to avoid coupling to a
    // particular envelope version while still asserting on the
    // payload underneath.
    let schema = json
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing schema".to_string())?;
    if !matches!(schema, "ee.error.v1" | "ee.error.v2") {
        return Err(format!("unexpected error schema: {schema}"));
    }
    Ok(json)
}

#[test]
fn tag_rejection_error_includes_accepted_pattern_and_examples() -> TestResult {
    let ws = init_workspace()?;
    // A tag containing `=` is rejected — `=` is in the reserved-
    // delimiter set in `tag_rejection_reason`. Picked over `0.1.0`
    // because C3 relaxed dots/colons/underscores into the accepted
    // set, so `0.1.0` is now valid; over a comma, because commas
    // are the tag splitter; and over whitespace because the parser
    // would trim leading/trailing spaces first.
    let out = run_ee(&[
        "--workspace",
        ws.workspace.to_str().unwrap(),
        "remember",
        "Tag rejection probe.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--tags",
        "foo=bar",
        "--json",
    ])?;

    let json = parse_error_envelope(&out)?;
    let code = json
        .pointer("/error/code")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing error.code".to_string())?;
    if code != "usage" {
        return Err(format!(
            "expected error.code=usage, got {code}; json: {json}"
        ));
    }
    let details = json
        .pointer("/error/details")
        .ok_or_else(|| format!("missing error.details; json: {json}"))?;

    // Required fields per C4 acceptance.
    for field in [
        "acceptedPattern",
        "acceptedExamples",
        "matchedAt",
        "normalizedFormCandidate",
        "maxBytes",
    ] {
        if details.get(field).is_none() {
            return Err(format!(
                "tag rejection details missing required field `{field}`; details={details}"
            ));
        }
    }

    // acceptedPattern must be a non-empty string.
    let pattern = details
        .get("acceptedPattern")
        .and_then(Value::as_str)
        .ok_or_else(|| "acceptedPattern not a string".to_string())?;
    if pattern.is_empty() {
        return Err("acceptedPattern must not be empty".to_string());
    }

    // acceptedExamples must be a non-empty array of strings; the C4
    // spec calls for "3-5 examples — agents pattern-match well from
    // a handful".
    let examples = details
        .get("acceptedExamples")
        .and_then(Value::as_array)
        .ok_or_else(|| "acceptedExamples not an array".to_string())?;
    if examples.len() < 3 {
        return Err(format!(
            "acceptedExamples should carry at least 3 entries, got {}: {examples:?}",
            examples.len()
        ));
    }
    if examples.iter().any(|v| !v.is_string()) {
        return Err(format!("acceptedExamples must be strings: {examples:?}"));
    }

    // matchedAt[] must carry at least one rejection with reason+offsets.
    let matched = details
        .get("matchedAt")
        .and_then(Value::as_array)
        .ok_or_else(|| "matchedAt not an array".to_string())?;
    if matched.is_empty() {
        return Err("matchedAt must surface at least one offending region".to_string());
    }
    let first = matched
        .first()
        .ok_or_else(|| "matchedAt empty".to_string())?;
    for field in ["start", "end", "reason"] {
        if first.get(field).is_none() {
            return Err(format!(
                "matchedAt[0] missing required field `{field}`; got {first}"
            ));
        }
    }

    Ok(())
}

#[test]
fn tag_rejection_accepted_examples_are_actually_valid_tags() -> TestResult {
    // The examples we hand out to agents must actually be accepted by
    // the live `Tag::parse` validator. Without this check, an agent
    // that trusted `acceptedExamples` and reused one verbatim would
    // hit the same rejection it was trying to recover from.
    //
    // This is a stronger guarantee than regex-matching the advertised
    // `acceptedPattern`: it exercises the real validation code,
    // catching cases where the pattern advertised in the envelope
    // diverges from the live validator.
    let ws = init_workspace()?;
    let out = run_ee(&[
        "--workspace",
        ws.workspace.to_str().unwrap(),
        "remember",
        "Tag examples self-consistency probe.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--tags",
        "foo=bar",
        "--json",
    ])?;
    let json = parse_error_envelope(&out)?;
    let examples = json
        .pointer("/error/details/acceptedExamples")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing acceptedExamples".to_string())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for example in &examples {
        Tag::parse(example).map_err(|error| {
            format!("acceptedExamples carries `{example}` but `Tag::parse` rejects it: {error:?}")
        })?;
    }
    Ok(())
}

#[test]
fn content_secret_rejection_error_includes_bypass_flag_and_config_key() -> TestResult {
    let ws = init_workspace()?;
    // Use a synthesized OpenAI-style key prefix. C4 design note: "No
    // real secrets in trigger setups. Use known-fake values; the
    // secret-value detector is tested with synthesized prefixes
    // (sk-FAKE…), not real keys." This value matches the
    // openai_sk_prefix pattern (sk- followed by 48 chars), but the
    // body is "FAKE" repeated.
    let synthesized_key = format!("sk-{}", "FAKE".repeat(12));
    let content = format!("Test fixture leaking a synthesized fake API key: {synthesized_key}");

    let out = run_ee(&[
        "--workspace",
        ws.workspace.to_str().unwrap(),
        "remember",
        &content,
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--json",
    ])?;
    let json = parse_error_envelope(&out)?;
    let code = json
        .pointer("/error/code")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing error.code".to_string())?;
    if code != "policy_denied" {
        return Err(format!(
            "expected error.code=policy_denied, got {code}; json: {json}"
        ));
    }
    let details = json
        .pointer("/error/details")
        .ok_or_else(|| format!("missing error.details; json: {json}"))?;

    for field in [
        "detectedPattern",
        "detectedPatterns",
        "matchedAt",
        "bypassFlag",
        "configKey",
    ] {
        if details.get(field).is_none() {
            return Err(format!(
                "secret rejection details missing required field `{field}`; details={details}"
            ));
        }
    }

    // bypassFlag must specifically be `--allow-secret-mention` so an
    // agent reading the error knows the exact flag to add.
    let bypass = details
        .get("bypassFlag")
        .and_then(Value::as_str)
        .ok_or_else(|| "bypassFlag not a string".to_string())?;
    if bypass != "--allow-secret-mention" {
        return Err(format!(
            "bypassFlag must be `--allow-secret-mention`, got `{bypass}`"
        ));
    }

    // matchedAt[] entries must carry start/end offsets + pattern_id.
    let matched = details
        .get("matchedAt")
        .and_then(Value::as_array)
        .ok_or_else(|| "matchedAt not an array".to_string())?;
    if matched.is_empty() {
        return Err("matchedAt must surface at least one detection".to_string());
    }
    let first = matched
        .first()
        .ok_or_else(|| "matchedAt empty".to_string())?;
    for field in ["start", "end", "pattern_id"] {
        if first.get(field).is_none() {
            return Err(format!(
                "matchedAt[0] missing required field `{field}`; got {first}"
            ));
        }
    }

    // CRITICAL invariant from C4 design considerations: "for content
    // with detected secret value, NEVER include the secret in
    // error.details. Only byte offsets." Walk the entire details
    // payload and assert the synthesized key body never appears.
    let details_text =
        serde_json::to_string(details).map_err(|error| format!("serialize details: {error}"))?;
    if details_text.contains(&synthesized_key) {
        return Err(format!(
            "secret rejection details must not echo the matched secret value; \
             details contained `{synthesized_key}`: {details_text}"
        ));
    }

    Ok(())
}

#[test]
fn content_secret_rejection_does_not_leak_secret_in_message() -> TestResult {
    // Companion to the previous test — even the human-facing
    // error.message must not echo the secret value. The message can
    // name the *pattern* (`openai_sk_prefix`) but not the value.
    let ws = init_workspace()?;
    let synthesized_key = format!("sk-{}", "ZEBR".repeat(12));
    let content = format!("Configure with key {synthesized_key} for tests.");

    let out = run_ee(&[
        "--workspace",
        ws.workspace.to_str().unwrap(),
        "remember",
        &content,
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--json",
    ])?;
    let json = parse_error_envelope(&out)?;
    let message = json
        .pointer("/error/message")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing error.message; json={json}"))?;
    if message.contains(&synthesized_key) {
        return Err(format!(
            "error.message must not echo the matched secret value; got: {message}"
        ));
    }
    Ok(())
}
