//! K5 — Migration v0.1→v0.2 enforcement suite (bd-17c65.11.5).
//!
//! Pins the wire-contract delta between `ee` v0.1 and v0.2 documented
//! in `docs/migration_v0_1_to_v0_2.md`. Five sub-tests cover four
//! complementary invariants:
//!
//!   1. `no_forbidden_v0_1_wire_keys_in_src` — greps `src/` for JSON
//!      key string literals that v0.2 retires (e.g. `"content_preview"`,
//!      `"selectionCertificate.algorithm"`). One per row in the
//!      `FORBIDDEN_V1_WIRE_KEYS` table. Today the table is empty of
//!      hits; this test is the CI gate that catches a regression
//!      reintroducing them.
//!
//!   2. `migration_guide_has_section_per_breaking_bead` — parses
//!      `docs/migration_v0_1_to_v0_2.md` for `### {token} —` anchors
//!      and asserts every closed breaking-change bead in the v0.2
//!      milestone has a section (standalone OR combined like
//!      `### C1 + C3 —`). Drift between the guide and the
//!      breaking-beads table fails CI.
//!
//!   3. `forbidden_table_has_no_duplicate_keys` — linter for the
//!      const table; a duplicate row would make a single drift hit
//!      report twice.
//!
//!   4. `forbidden_table_keys_are_canonical_form` — every row's key
//!      is wrapped in JSON quote characters so the grep matches
//!      actual JSON emission, not internal Rust identifier names
//!      that legitimately share the spelling.
//!
//!   5. `healthy_field_pending_v0_3_removal` — *informational, not
//!      gating.* Reports the count of `field_bool("healthy"`
//!      emissions remaining in `src/output/mod.rs`. The v0.2
//!      transition window keeps the field; v0.3 enforces removal.
//!      Fails only if the count GROWS (regression).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult = Result<(), String>;

/// Canonical forbidden v0.1 wire-format JSON keys. Each entry is the
/// key as it would appear inside double-quotes in emitted JSON.
///
/// **Format:** `(key_with_quotes, retired_by_bead, rationale)`.
const FORBIDDEN_V1_WIRE_KEYS: &[(&str, &str, &str)] = &[
    (
        "\"content_preview\"",
        "bd-17c65.4.1",
        "D1: replaced by `content` + `content_truncated` on every list/preview surface",
    ),
    (
        "\"selectionCertificate.algorithm\"",
        "bd-17c65.1.1",
        "A1 phase 2: collapsed into pack.meta.algorithm + per-item fields",
    ),
    (
        "\"selectionCertificate.objective\"",
        "bd-17c65.1.1",
        "A1 phase 2: collapsed into pack.meta.algorithm",
    ),
    (
        "\"selectionCertificate.steps\"",
        "bd-17c65.1.1",
        "A1 phase 2: trace collapsed onto each item via rank",
    ),
    (
        "\"selectionCertificate.selectedItems\"",
        "bd-17c65.1.1",
        "A1 phase 2: items[] now carries the union of fields",
    ),
    (
        "\"provenanceFooter.entries\"",
        "bd-17c65.1.1",
        "A1 phase 2: provenance moved to per-item items[].provenance[]",
    ),
    (
        "\"ee.error.v1\"",
        "bd-17c65.4.7",
        "A10: error envelope bumped to ee.error.v2 with structured recovery[]",
    ),
    (
        "\"ee.pack.v1\"",
        "bd-17c65.1.1",
        "A1 phase 2: pack object bumped to ee.pack.v2",
    ),
    (
        "\"degraded_context\"",
        "bd-17c65.5.2",
        "E2: meta-banner code deleted; per-signal codes already surface in degraded[]",
    ),
];

/// Beads whose closing landed a breaking wire-contract change in v0.2.
/// Every entry MUST have a section in `docs/migration_v0_1_to_v0_2.md`.
///
/// **Format:** `(bead_id, section_token_in_guide)`.
const BREAKING_BEADS: &[(&str, &str)] = &[
    ("bd-17c65.4.1", "D1"),
    ("bd-17c65.5.1", "E1"),
    ("bd-17c65.5.2", "E2"),
    ("bd-17c65.5.3", "E3"),
    ("bd-17c65.1.1", "A1 phase 2"),
    ("bd-17c65.1.4", "A4"),
    ("bd-17c65.4.7", "A10"),
    ("bd-17c65.2.1", "B1"),
    ("bd-17c65.3.1", "C1"),
    ("bd-17c65.3.3", "C3"),
    ("bd-17c65.6.1", "F1"),
    ("bd-17c65.8.1", "H1"),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn src_dir() -> PathBuf {
    repo_root().join("src")
}

fn migration_guide_path() -> PathBuf {
    repo_root().join("docs").join("migration_v0_1_to_v0_2.md")
}

/// Count grep `-RF` hits for `needle` under `src/`, excluding:
///   - the migration-shim subdirectory (legitimate v1-compat reads).
///   - lines containing `LEGACY_` (tombstone const declarations like
///     `LEGACY_DEGRADED_CONTEXT_CODE` that intentionally preserve the
///     v1 string for J6-catalog cross-references but never emit).
///   - doc-comment lines (`//`, `///`, `//!`) that document historical
///     state without producing wire output.
fn grep_count_excluding_shims(needle: &str, src: &Path) -> Result<usize, String> {
    let output = Command::new("grep")
        .arg("-RF")
        .arg("--include=*.rs")
        .arg("--exclude-dir=migration")
        .arg(needle)
        .arg(src)
        .output()
        .map_err(|e| format!("spawn grep: {e}"))?;
    match output.status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&output.stdout);
            let real_hits: usize = text
                .lines()
                .filter(|line| {
                    let trimmed = line.split(':').nth(2).unwrap_or(line).trim_start();
                    !trimmed.starts_with("//!")
                        && !trimmed.starts_with("///")
                        && !trimmed.starts_with("//")
                        && !line.contains("LEGACY_")
                })
                .count();
            Ok(real_hits)
        }
        Some(1) => Ok(0),
        Some(other) => Err(format!(
            "grep exit {other}: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
        None => Err("grep terminated by signal".to_owned()),
    }
}

#[test]
fn no_forbidden_v0_1_wire_keys_in_src() -> TestResult {
    let src = src_dir();
    let mut hits: Vec<String> = Vec::new();
    for (key, bead, rationale) in FORBIDDEN_V1_WIRE_KEYS {
        let count = grep_count_excluding_shims(key, &src)?;
        if count > 0 {
            hits.push(format!(
                "  - {key}: {count} occurrence(s) under {} ({bead}: {rationale})",
                src.display(),
            ));
        }
    }
    if !hits.is_empty() {
        return Err(format!(
            "v0.1 wire-format keys still present in src/ ({} key(s)):\n{}\n\nPer docs/migration_v0_1_to_v0_2.md every listed bead retired its wire key in v0.2. Fix the impl, then this test goes green.",
            hits.len(),
            hits.join("\n"),
        ));
    }
    Ok(())
}

#[test]
fn migration_guide_has_section_per_breaking_bead() -> TestResult {
    let guide_path = migration_guide_path();
    let guide = fs::read_to_string(&guide_path)
        .map_err(|e| format!("read {}: {e}", guide_path.display()))?;
    let mut missing: Vec<String> = Vec::new();
    for (bead_id, section_token) in BREAKING_BEADS {
        // Accept: standalone (### D1 —), bead-id anchor (### bd-…),
        // combined-LHS (### C1 +), or combined-RHS (+ C3 —) headings.
        let standalone = format!("### {section_token} —");
        let combined_lhs = format!("### {section_token} +");
        let combined_rhs = format!("+ {section_token} —");
        let bead_anchor = format!("### {bead_id}");
        let plain_anchor = format!("### {section_token}\n");
        if !guide.contains(&standalone)
            && !guide.contains(&combined_lhs)
            && !guide.contains(&combined_rhs)
            && !guide.contains(&bead_anchor)
            && !guide.contains(&plain_anchor)
        {
            missing.push(format!(
                "  - {bead_id} (section token `{section_token}`)",
            ));
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "docs/migration_v0_1_to_v0_2.md is missing breaking-change sections for {} bead(s):\n{}\n\nAdd a `### {{token}} — {{description}}` heading per the K5 template (Before/After/Agent rewrite/Migration tool) for each listed bead.",
            missing.len(),
            missing.join("\n"),
        ));
    }
    Ok(())
}

#[test]
fn forbidden_table_has_no_duplicate_keys() -> TestResult {
    let mut seen: Vec<&str> = Vec::new();
    for (key, _, _) in FORBIDDEN_V1_WIRE_KEYS {
        if seen.contains(key) {
            return Err(format!(
                "FORBIDDEN_V1_WIRE_KEYS has duplicate row for {key} — pick one canonical entry",
            ));
        }
        seen.push(key);
    }
    Ok(())
}

#[test]
fn forbidden_table_keys_are_canonical_form() -> TestResult {
    for (key, bead, _) in FORBIDDEN_V1_WIRE_KEYS {
        if !key.starts_with('"') || !key.ends_with('"') {
            return Err(format!(
                "FORBIDDEN_V1_WIRE_KEYS row {key:?} for {bead} is not in canonical \"\\\"key\\\"\" form",
            ));
        }
        if key.len() < 3 {
            return Err(format!(
                "FORBIDDEN_V1_WIRE_KEYS row {key:?} for {bead} is empty inside the quotes",
            ));
        }
    }
    Ok(())
}

#[test]
fn breaking_beads_table_has_no_duplicates() -> TestResult {
    let mut seen_ids: Vec<&str> = Vec::new();
    let mut seen_tokens: Vec<&str> = Vec::new();
    for (bead_id, token) in BREAKING_BEADS {
        if seen_ids.contains(bead_id) {
            return Err(format!(
                "BREAKING_BEADS has duplicate row for {bead_id} — pick one canonical entry",
            ));
        }
        if seen_tokens.contains(token) {
            return Err(format!(
                "BREAKING_BEADS has duplicate section token `{token}` (used by {bead_id} and a prior row)",
            ));
        }
        seen_ids.push(bead_id);
        seen_tokens.push(token);
    }
    Ok(())
}

#[test]
fn healthy_field_pending_v0_3_removal() -> TestResult {
    // INFORMATIONAL — NOT a CI gate during the v0.2 transition window.
    // Fails ONLY if the count grows above the snapshot (regression).
    const SNAPSHOT_EMISSION_COUNT: usize = 5;
    let src = src_dir();
    let needle = "field_bool(\"healthy\"";
    let count = grep_count_excluding_shims(needle, &src)?;
    if count > SNAPSHOT_EMISSION_COUNT {
        return Err(format!(
            "field_bool(\"healthy\", ...) emission count grew from {SNAPSHOT_EMISSION_COUNT} (snapshot) to {count} — E1 (bd-17c65.5.1) plan was to retire this field by v0.3, not add new emission sites.",
        ));
    }
    eprintln!(
        "[K5 migration progress] field_bool(\"healthy\", ...) emissions in src/: {count} (snapshot: {SNAPSHOT_EMISSION_COUNT}, target for v0.3 removal: 0)",
    );
    Ok(())
}
