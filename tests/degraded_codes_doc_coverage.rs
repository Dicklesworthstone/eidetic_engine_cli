//! K3 — `docs/degraded_codes.md` coverage gate (bd-17c65.11.3).
//!
//! Asserts the auto-generated catalog at `docs/degraded_codes.md`
//! covers every fixture in `tests/fixtures/failure_modes/` and has
//! no orphan sections (sections whose code has no corresponding
//! fixture). Wired as the CI gate that catches doc/fixture drift
//! after a regen is forgotten OR a fixture is added without
//! re-running the generator.
//!
//! Coverage includes:
//!
//!   1. `every_fixture_has_a_doc_section` — for each
//!      `tests/fixtures/failure_modes/*.json`, parse `.code` and
//!      assert `docs/degraded_codes.md` contains `## \`<code>\``.
//!      Catches the dominant failure mode: fixture added, regen
//!      forgotten.
//!
//!   2. `every_doc_section_has_a_fixture` — for each `##` heading
//!      in the doc whose body is a code identifier, assert a fixture
//!      file exists. Catches the inverse: doc has a stale section
//!      from a fixture that was deleted/renamed.
//!
//!   3. `doc_carries_auto_generated_disclaimer` — the doc's
//!      preamble must mention "AUTO-GENERATED" so a human editor
//!      reading the file immediately understands why edits get
//!      overwritten on the next regen. Pure file I/O gate.
//!
//!   4. `fixture_codes_and_doc_codes_are_disjoint_sets_size_match` —
//!      belt-and-suspenders set equality for fixture codes and doc
//!      sections.
//!
//!   5. `retired_fixture_sections_are_marked_retired` — retired
//!      tombstone fixtures must render as historical, non-emitting
//!      sections rather than live degraded codes.
//!
//! No `use ee::*` imports — the test runs purely off file contents
//! and is independent of any lib state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures_dir() -> PathBuf {
    repo_root()
        .join("tests")
        .join("fixtures")
        .join("failure_modes")
}

fn doc_path() -> PathBuf {
    repo_root().join("docs").join("degraded_codes.md")
}

fn read_fixture_json(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {} as JSON: {e}", path.display()))
}

/// Extract every `.code` field from the fixture JSON files in
/// `tests/fixtures/failure_modes/`. Returns codes sorted (the doc
/// generator emits sections in sort order, so this list is the
/// expected section order too).
fn collect_fixture_codes() -> Result<BTreeSet<String>, String> {
    let dir = fixtures_dir();
    let entries = fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    let mut codes: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry under {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let value = read_fixture_json(&path)?;
        let code = value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("fixture {} missing `.code` field", path.display()))?;
        codes.insert(code.to_string());
    }
    if codes.is_empty() {
        return Err(format!(
            "no fixtures found under {}; K3 catalog generator has nothing to emit",
            dir.display(),
        ));
    }
    Ok(codes)
}

fn collect_retired_fixture_beads() -> Result<BTreeMap<String, String>, String> {
    let dir = fixtures_dir();
    let entries = fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    let mut retired = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry under {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let value = read_fixture_json(&path)?;
        if value
            .get("retired")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            let code = value
                .get("code")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("fixture {} missing `.code` field", path.display()))?;
            let bead = value
                .pointer("/retired_by/bead")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "retired fixture {} missing `.retired_by.bead`",
                        path.display()
                    )
                })?;
            retired.insert(code.to_string(), bead.to_string());
        }
    }
    Ok(retired)
}

/// Extract every code that appears as a `## \`<code>\`` H2 heading
/// in the doc. The catalog's per-code section format is exactly
/// `## \`<code>\`` (per `scripts/build_degraded_codes_doc.sh`); this
/// parser greps for that pattern and pulls the code out.
fn collect_doc_section_codes(doc_text: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for line in doc_text.lines() {
        // Section headings look exactly like: `## \`code_name\``
        if let Some(rest) = line.strip_prefix("## `")
            && let Some(end) = rest.find('`')
        {
            let code = &rest[..end];
            // Skip obvious non-code H2 headings the doc might use
            // for its preamble (e.g. "Catalog summary", "How agents
            // consume this catalog", "Reporting drift"). These don't
            // start with a backtick, so the strip_prefix above
            // already filters them.
            if !code.is_empty()
                && code
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                out.insert(code.to_string());
            }
        }
    }
    out
}

fn collect_doc_section_codes_in_order(doc_text: &str) -> Vec<String> {
    doc_text
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("## `")?;
            let end = rest.find('`')?;
            let code = &rest[..end];
            (!code.is_empty()
                && code
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            .then(|| code.to_owned())
        })
        .collect()
}

fn doc_section<'a>(doc_text: &'a str, code: &str) -> Option<&'a str> {
    let heading = format!("## `{code}`");
    let start = doc_text.find(&heading)?;
    let tail = &doc_text[start..];
    let end = tail.find("\n---\n").unwrap_or(tail.len());
    Some(&tail[..end])
}

#[test]
fn every_fixture_has_a_doc_section() -> TestResult {
    let fixture_codes = collect_fixture_codes()?;
    let doc_text = fs::read_to_string(doc_path())
        .map_err(|e| format!("read {}: {e}", doc_path().display()))?;
    let doc_codes = collect_doc_section_codes(&doc_text);

    let missing: Vec<&String> = fixture_codes.difference(&doc_codes).collect();
    if !missing.is_empty() {
        return Err(format!(
            "K3 catalog has {} fixture(s) with no matching `## \\`{{code}}\\`` section in docs/degraded_codes.md:\n{}\n\nRun `./scripts/build_degraded_codes_doc.sh` to regenerate.",
            missing.len(),
            missing
                .iter()
                .take(10)
                .map(|c| format!("  - `{c}`"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    Ok(())
}

#[test]
fn every_doc_section_has_a_fixture() -> TestResult {
    let fixture_codes = collect_fixture_codes()?;
    let doc_text = fs::read_to_string(doc_path())
        .map_err(|e| format!("read {}: {e}", doc_path().display()))?;
    let doc_codes = collect_doc_section_codes(&doc_text);

    let orphans: Vec<&String> = doc_codes.difference(&fixture_codes).collect();
    if !orphans.is_empty() {
        return Err(format!(
            "K3 catalog has {} orphan `## \\`{{code}}\\`` section(s) in docs/degraded_codes.md with no corresponding fixture:\n{}\n\nEither (a) restore the fixture under tests/fixtures/failure_modes/<code>.json, or (b) re-run `./scripts/build_degraded_codes_doc.sh` to drop the orphan section.",
            orphans.len(),
            orphans
                .iter()
                .take(10)
                .map(|c| format!("  - `{c}`"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    Ok(())
}

#[test]
fn doc_carries_auto_generated_disclaimer() -> TestResult {
    let doc_text = fs::read_to_string(doc_path())
        .map_err(|e| format!("read {}: {e}", doc_path().display()))?;
    // The disclaimer is in the file header so a human reading the
    // first screen of the file immediately sees the regen contract.
    let head = doc_text.chars().take(2000).collect::<String>();
    if !head.contains("AUTO-GENERATED") {
        return Err(
            "docs/degraded_codes.md is missing the AUTO-GENERATED disclaimer in its first 2000 chars. The disclaimer is required so a human editor reading the file understands edits get overwritten on the next regen. Restore the header block via `./scripts/build_degraded_codes_doc.sh`.".to_string(),
        );
    }
    if !head.contains("build_degraded_codes_doc.sh") {
        return Err(
            "docs/degraded_codes.md preamble must reference the generator script `scripts/build_degraded_codes_doc.sh` so a reader knows how to regenerate.".to_string(),
        );
    }
    Ok(())
}

#[test]
fn fixture_codes_and_doc_codes_are_disjoint_sets_size_match() -> TestResult {
    // Belt-and-suspenders: combine the two preceding tests by
    // asserting the two sets are equal. This catches the corner
    // case where both directions fail simultaneously but in a way
    // that the per-direction diffs miss (extremely unlikely but
    // free to check).
    let fixture_codes = collect_fixture_codes()?;
    let doc_text = fs::read_to_string(doc_path())
        .map_err(|e| format!("read {}: {e}", doc_path().display()))?;
    let doc_codes = collect_doc_section_codes(&doc_text);
    if fixture_codes != doc_codes {
        let missing = fixture_codes.difference(&doc_codes).count();
        let orphan = doc_codes.difference(&fixture_codes).count();
        return Err(format!(
            "K3 catalog has size-mismatched sets: {} fixture(s) without doc sections, {} doc section(s) without fixtures. Run `./scripts/build_degraded_codes_doc.sh` to reconcile.",
            missing, orphan,
        ));
    }
    Ok(())
}

#[test]
fn doc_sections_are_sorted_by_fixture_code() -> TestResult {
    let expected = collect_fixture_codes()?.into_iter().collect::<Vec<_>>();
    let doc_text = fs::read_to_string(doc_path())
        .map_err(|e| format!("read {}: {e}", doc_path().display()))?;
    let actual = collect_doc_section_codes_in_order(&doc_text);
    if actual != expected {
        return Err(format!(
            "docs/degraded_codes.md sections are not in fixture-code order; run ./scripts/build_degraded_codes_doc.sh\nexpected first drift: {:?}\nactual first drift: {:?}",
            expected
                .iter()
                .zip(&actual)
                .find(|(left, right)| left != right),
            actual
                .iter()
                .zip(&expected)
                .find(|(left, right)| left != right),
        ));
    }
    Ok(())
}

#[test]
fn generated_doc_is_byte_identical_to_builder_output() -> TestResult {
    let script = repo_root()
        .join("scripts")
        .join("build_degraded_codes_doc.sh");
    let output = Command::new("bash")
        .arg(&script)
        .arg("--check")
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run {} --check: {error}", script.display()))?;
    if !output.status.success() {
        return Err(format!(
            "generated degraded-code catalog drifted from its builder:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

#[test]
fn retired_fixture_sections_are_marked_retired() -> TestResult {
    let retired = collect_retired_fixture_beads()?;
    let doc_text = fs::read_to_string(doc_path())
        .map_err(|e| format!("read {}: {e}", doc_path().display()))?;
    let mut errors = Vec::new();

    for (code, bead) in retired {
        let Some(section) = doc_section(&doc_text, &code) else {
            errors.push(format!("retired fixture `{code}` has no doc section"));
            continue;
        };
        let status =
            format!("**Status:** retired by {bead}. No current production path emits this code.");
        if !section.contains(&status) {
            errors.push(format!(
                "retired fixture `{code}` section is missing exact retired status `{status}`",
            ));
        }
        if !section.contains("**Historical trigger.**") {
            errors.push(format!(
                "retired fixture `{code}` section must label the trigger as historical",
            ));
        }
        if !section.contains("**Historical invocation.**") {
            errors.push(format!(
                "retired fixture `{code}` section must label the invocation as historical",
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} retired fixture doc section(s) failed validation:\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}
