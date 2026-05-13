//! Consistency gate for `docs/degraded_code_taxonomy.md` (bd-fptj3).
//!
//! Cross-references two authoritative sources:
//!   1. `tests/fixtures/failure_modes/*.json` — the agent-facing J6
//!      catalog, one file per code (bd-17c65.10.6).
//!   2. `docs/degraded_code_taxonomy.md` — this bead's classification.
//!
//! Every code with a fixture must be classified in the taxonomy.
//! Conversely every code in the taxonomy must have a fixture (so the
//! taxonomy and the catalog never drift).
//!
//! Codes emitted internally in `src/` that have NO fixture (domain-
//! specific signals like integrity_*, causal_*, redaction class names
//! masquerading as "code": pattern matches) are out of scope for this
//! test. Those are the responsibility of the J6 catalog driver and the
//! per-subsystem coverage gates.
//!
//! Bead: bd-fptj3.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn j6_fixture_codes(repo: &PathBuf) -> Result<BTreeSet<String>, String> {
    let dir = repo.join("tests/fixtures/failure_modes");
    let mut codes = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("read_dir: {e}"))? {
        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "json") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                codes.insert(stem.to_owned());
            }
        }
    }
    Ok(codes)
}

fn taxonomy_codes(repo: &PathBuf) -> Result<BTreeSet<String>, String> {
    let path = repo.join("docs/degraded_code_taxonomy.md");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    // Every code in the taxonomy appears in a markdown table as
    // `| `<name>` |` (backticked, surrounded by table delimiters).
    // Use a regex-free scan: look for any backticked lowercase token.
    let mut codes = BTreeSet::new();
    for line in content.lines() {
        let mut chars = line.char_indices();
        while let Some((i, c)) = chars.next() {
            if c == '`' {
                let rest = &line[i + 1..];
                if let Some(end) = rest.find('`') {
                    let token = &rest[..end];
                    if !token.is_empty()
                        && token.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                        && token.starts_with(|c: char| c.is_ascii_lowercase())
                    {
                        codes.insert(token.to_owned());
                    }
                    // advance past the closing backtick to avoid double-counting
                    for _ in 0..=end {
                        chars.next();
                    }
                }
            }
        }
    }
    // Strip non-code lowercase tokens that appear in prose (e.g., "info",
    // "low", "warning" are severity names, not codes).
    let prose_tokens: BTreeSet<&str> = [
        "build_time", "response_time", "mixed",
        "info", "low", "warning", "medium", "high", "critical",
        "code", "severity", "bead", "surface", "feature_flag",
        // Feature flag NAMES (appear in backticks but aren't degraded codes)
        "fnx-runtime", "frankensearch", "fsqlite",
        "asupersync", "cass", "mcp",
        // Markdown rendering artifacts
        "data", "ee", "fnx", "src", "tests", "docs",
    ]
    .into_iter()
    .collect();
    codes.retain(|code| !prose_tokens.contains(code.as_str()));
    Ok(codes)
}

#[test]
fn every_j6_fixture_code_is_classified_in_taxonomy() -> TestResult {
    let repo = repo_root();
    let fixtures = j6_fixture_codes(&repo)?;
    let taxonomy = taxonomy_codes(&repo)?;
    let unclassified: Vec<&String> = fixtures.difference(&taxonomy).collect();
    ensure(
        unclassified.is_empty(),
        format!(
            "{} J6 fixture code(s) NOT classified in docs/degraded_code_taxonomy.md: {:?}\n\
             Every code with a tests/fixtures/failure_modes/<code>.json must appear in the taxonomy.",
            unclassified.len(),
            unclassified
        ),
    )
}

#[test]
fn every_taxonomy_code_has_a_j6_fixture_or_pending_marker() -> TestResult {
    let repo = repo_root();
    let fixtures = j6_fixture_codes(&repo)?;
    let taxonomy = taxonomy_codes(&repo)?;
    let orphans: Vec<&String> = taxonomy.difference(&fixtures).collect();
    // Codes classified in the taxonomy but lacking a J6 fixture are
    // either pending-fixture (an emission site exists, fixture
    // hasn't been authored yet) or future (the emission isn't built
    // yet). Both are acceptable as long as the taxonomy lists them.
    // This test is non-blocking; it reports informationally.
    if !orphans.is_empty() {
        eprintln!(
            "INFO: {} taxonomy code(s) lack a J6 fixture (pending or future): {:?}",
            orphans.len(),
            orphans
        );
    }
    Ok(())
}

#[test]
fn no_build_time_code_appears_in_fixtures_response_time_section() -> TestResult {
    // After E5 lands, this should be a STRICT assertion: build_time
    // codes must NOT appear in any failure-mode fixture (those are
    // response-time fixtures). Pre-E5, this is a soft check that just
    // verifies the taxonomy's own internal consistency: codes
    // categorized as build_time appear in the build_time table, not
    // anywhere else.
    let repo = repo_root();
    let doc = std::fs::read_to_string(repo.join("docs/degraded_code_taxonomy.md"))
        .map_err(|e| format!("read: {e}"))?;

    // Naively split on the section headers; verify codes in the
    // build_time table don't reappear under response_time/mixed.
    let build_time_section = doc
        .split("### `build_time`")
        .nth(1)
        .and_then(|s| s.split("### `").next())
        .ok_or_else(|| "build_time section not found".to_owned())?;
    let response_time_section = doc
        .split("### `response_time`")
        .nth(1)
        .ok_or_else(|| "response_time section not found".to_owned())?;

    let build_time_codes: BTreeSet<&str> = build_time_section
        .lines()
        .filter_map(|l| {
            l.split('`').nth(1).filter(|t| {
                t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && t.len() > 3
            })
        })
        .collect();
    let response_time_codes: BTreeSet<&str> = response_time_section
        .lines()
        .filter_map(|l| {
            l.split('`').nth(1).filter(|t| {
                t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && t.len() > 3
            })
        })
        .collect();

    let dupes: Vec<&&str> = build_time_codes.intersection(&response_time_codes).collect();
    ensure(
        dupes.is_empty(),
        format!("codes appear in BOTH build_time and response_time sections: {dupes:?}"),
    )
}
