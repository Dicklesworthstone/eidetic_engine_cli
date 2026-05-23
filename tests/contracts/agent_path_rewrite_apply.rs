//! Contract coverage for `AgentPathRewrite::apply` (bd-mtb2i).
//!
//! `AgentPathRewrite::apply` (src/core/agent_detect.rs:130) rewrites a
//! source path when its prefix matches `self.from` on a path boundary.
//! Today the only test exercising this method is the inline
//! `source_path_rewrite_respects_connector_and_path_boundary` at
//! `src/core/agent_detect.rs:642`, which routes through the
//! higher-level `rewrite_agent_source_path` and only covers
//! happy/missing/partial paths. The bare `apply()` method has 4
//! branches in its `(to_ends_with_sep, rest_without_leading_sep)`
//! match plus exact-match shortcut, and a `/` vs `\` separator
//! choice in the `(false, None)` case — none of these are directly
//! pinned at unit level.

use ee::core::agent_detect::AgentPathRewrite;

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

fn rewrite(from: &str, to: &str) -> AgentPathRewrite {
    AgentPathRewrite {
        origin_id: "test-origin".to_string(),
        connector_slug: "test-connector".to_string(),
        from: from.to_string(),
        to: to.to_string(),
    }
}

#[test]
fn exact_match_returns_to_verbatim() -> TestResult {
    // First branch of apply(): if path == self.from, return Some(to.clone())
    // without any boundary or separator logic.
    let rule = rewrite("/home/agent/.codex", "/remote/codex");
    ensure_equal(
        &rule.apply("/home/agent/.codex"),
        &Some("/remote/codex".to_string()),
        "exact match returns to verbatim",
    )
}

#[test]
fn no_prefix_match_returns_none() -> TestResult {
    let rule = rewrite("/home/agent/.codex", "/remote/codex");
    ensure_equal(
        &rule.apply("/other/path"),
        &None,
        "path that does not start with self.from returns None",
    )
}

#[test]
fn prefix_match_without_separator_boundary_returns_none() -> TestResult {
    // self.from = "/home/agent/.codex" (no trailing /), path =
    // "/home/agent/.codex-old/file" — the prefix matches as a string
    // but there is no separator between the from and the rest, so
    // this is a partial-component match that must NOT rewrite.
    let rule = rewrite("/home/agent/.codex", "/remote/codex");
    ensure_equal(
        &rule.apply("/home/agent/.codex-old/file"),
        &None,
        "prefix without separator boundary must NOT rewrite",
    )
}

#[test]
fn forward_slash_separator_with_no_trailing_sep_in_rule() -> TestResult {
    // self.from has no trailing /, self.to has no trailing /, the rest
    // starts with /. The (false, Some(_)) match arm uses the full
    // `rest` (including the leading separator) so the result is
    // `{to}{rest}`.
    let rule = rewrite("/home/agent/.codex", "/remote/codex");
    ensure_equal(
        &rule.apply("/home/agent/.codex/sessions/2026.jsonl"),
        &Some("/remote/codex/sessions/2026.jsonl".to_string()),
        "rest starts with /, to has no trailing /, full rest preserved",
    )
}

#[test]
fn forward_slash_with_trailing_sep_in_rule() -> TestResult {
    // self.from ends with /, self.to ends with /. The (true, Some(_))
    // arm strips the leading separator from rest before concatenation
    // so we don't double up.
    let rule = rewrite("/home/agent/.codex/", "/remote/codex/");
    ensure_equal(
        &rule.apply("/home/agent/.codex/sessions/2026.jsonl"),
        &Some("/remote/codex/sessions/2026.jsonl".to_string()),
        "both ends-with-sep: leading sep stripped from rest to avoid double slash",
    )
}

#[test]
fn from_ends_with_sep_but_no_rest_separator() -> TestResult {
    // self.from = "/home/agent/.codex/" (ends with /), path ends
    // exactly at from (i.e., path == "/home/agent/.codex/"). The
    // from_ends_with_sep branch matches, rest is empty, output is
    // to verbatim. This exercises the (false, None) and (true, None)
    // arms depending on whether to ends with sep.
    let rule = rewrite("/home/agent/.codex/", "/remote/codex");
    ensure_equal(
        &rule.apply("/home/agent/.codex/"),
        &Some("/remote/codex".to_string()),
        "rest empty with from-ends-with-sep returns to verbatim",
    )
}

#[test]
fn backslash_separator_chosen_when_from_ends_with_backslash() -> TestResult {
    // (false, None) arm: neither from nor to ends with a separator,
    // and rest does not start with one either. The separator is
    // selected based on which one self.from ends with — '\\' if from
    // ends with '\\', otherwise '/'. Construct a case where from
    // ends with '\\' and verify the inserted separator.
    let rule = rewrite("C:\\home\\.codex", "D:\\remote\\codex");
    ensure_equal(
        &rule.apply("C:\\home\\.codex\\sessions\\2026.jsonl"),
        &Some("D:\\remote\\codex\\sessions\\2026.jsonl".to_string()),
        "Windows-style \\ separator: rest starts with \\, to has no trailing \\, full rest preserved",
    )
}
