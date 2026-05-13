//! J6 contract: failure-mode fixture catalog structural validator
//! (bd-17c65.10.6).
//!
//! Walks `tests/fixtures/failure_modes/*.json`, parses each fixture
//! against the `ee.failure_mode_fixture.v1` schema, and asserts:
//!
//! 1. Schema field is present and equals `ee.failure_mode_fixture.v1`.
//! 2. Required top-level fields (`code`, `introduced_by`, `surfaces`,
//!    `severity`, `repair_present`, `trigger`, `expected_emission`)
//!    exist with the right types.
//! 3. Filename stem matches the fixture's `code`.
//! 4. `severity` is one of {info, low, medium, high, critical}.
//! 5. The `code` string appears as a literal in `src/` so a fixture
//!    cannot document a fictional code or stay behind after a code
//!    removal.
//!
//! The validator is structural only. Per-epic e2e drivers under
//! `scripts/e2e_overhaul/` exercise each emission end-to-end against
//! the real binary; this test is the static reference that keeps the
//! catalog from drifting away from production.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

type TestResult = Result<(), String>;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("failure_modes")
}

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn allowed_severities() -> BTreeSet<&'static str> {
    ["info", "low", "warning", "medium", "high", "critical"]
        .into_iter()
        .collect()
}

fn list_fixture_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("failed to read {}: {error}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    Ok(paths)
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_string_field<'a>(
    value: &'a Value,
    pointer: &str,
    context: &str,
) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}: missing string at {pointer}"))
}

fn ensure_bool_field(value: &Value, pointer: &str, context: &str) -> Result<bool, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{context}: missing bool at {pointer}"))
}

fn ensure_array_field<'a>(
    value: &'a Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context}: missing array at {pointer}"))
}

/// Returns true if the literal `"<code>"` appears anywhere under src/.
/// Uses `grep -RFq` so the search is fast and exact (no regex escaping
/// surprises in the fixture code strings).
fn code_appears_in_source(code: &str, src: &Path) -> Result<bool, String> {
    let needle = format!("\"{code}\"");
    let output = Command::new("grep")
        .arg("-RFlq")
        .arg(&needle)
        .arg(src)
        .output()
        .map_err(|error| format!("failed to spawn grep: {error}"))?;
    // grep exits 0 on match, 1 on no-match, >1 on error.
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(other) => Err(format!(
            "grep failed with exit {other}: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
        None => Err("grep terminated by signal".to_owned()),
    }
}

fn validate_fixture(path: &Path) -> TestResult {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let ctx = path.display().to_string();

    // (1) schema pin.
    let schema = ensure_string_field(&value, "/schema", &ctx)?;
    ensure(
        schema == "ee.failure_mode_fixture.v1",
        format!("{ctx}: unexpected schema `{schema}`; expected ee.failure_mode_fixture.v1"),
    )?;

    // (2) required top-level fields.
    let code = ensure_string_field(&value, "/code", &ctx)?;
    let _bead = ensure_string_field(&value, "/introduced_by/bead", &ctx)?;
    let _epic = ensure_string_field(&value, "/introduced_by/epic_letter", &ctx)?;
    let surfaces = ensure_array_field(&value, "/surfaces", &ctx)?;
    ensure(
        !surfaces.is_empty(),
        format!("{ctx}: surfaces[] must list at least one CLI surface"),
    )?;
    for (idx, surface) in surfaces.iter().enumerate() {
        ensure(
            surface.is_string(),
            format!("{ctx}: surfaces[{idx}] must be a string"),
        )?;
    }
    let severity = ensure_string_field(&value, "/severity", &ctx)?;
    ensure(
        allowed_severities().contains(severity),
        format!(
            "{ctx}: severity `{severity}` not in {{info, low, warning, medium, high, critical}}",
        ),
    )?;
    let _repair_present = ensure_bool_field(&value, "/repair_present", &ctx)?;
    let _ = ensure_string_field(&value, "/trigger/description", &ctx)?;
    let _setup = ensure_array_field(&value, "/trigger/setup_commands", &ctx)?;
    let _invocation = ensure_string_field(&value, "/trigger/invocation", &ctx)?;
    let expected_code = ensure_string_field(&value, "/expected_emission/code", &ctx)?;
    ensure(
        expected_code == code,
        format!(
            "{ctx}: expected_emission.code `{expected_code}` does not match top-level code `{code}`",
        ),
    )?;
    let expected_sev = ensure_string_field(&value, "/expected_emission/severity", &ctx)?;
    ensure(
        expected_sev == severity,
        format!(
            "{ctx}: expected_emission.severity `{expected_sev}` does not match top-level severity `{severity}`",
        ),
    )?;
    let _msg_contains = ensure_array_field(&value, "/expected_emission/message_contains", &ctx)?;

    // (3) filename stem matches code.
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{ctx}: cannot read filename stem"))?;
    ensure(
        stem == code,
        format!("{ctx}: filename stem `{stem}` must equal fixture code `{code}`"),
    )?;
    ensure(
        code.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
        format!("{ctx}: code `{code}` must match [a-z][a-z0-9_]*"),
    )?;
    ensure(
        code.starts_with(|c: char| c.is_ascii_lowercase()),
        format!("{ctx}: code `{code}` must start with a lowercase letter"),
    )?;

    // (5) cross-reference against src/.
    let src = src_dir();
    let appears = code_appears_in_source(code, &src)?;
    ensure(
        appears,
        format!(
            "{ctx}: code `{code}` does not appear as a literal under {}; \
             either the fixture documents a fictional code or the code was \
             removed from production without updating the catalog",
            src.display()
        ),
    )?;

    Ok(())
}

#[test]
fn failure_mode_fixtures_validate_catalog() -> TestResult {
    let dir = fixtures_dir();
    let fixtures = list_fixture_files(&dir)?;
    ensure(
        !fixtures.is_empty(),
        format!(
            "no fixtures in {}; J6 seed catalog must ship at least one fixture",
            dir.display()
        ),
    )?;

    let mut errors: Vec<String> = Vec::new();
    let mut codes: BTreeSet<String> = BTreeSet::new();
    for path in &fixtures {
        if let Err(error) = validate_fixture(path) {
            errors.push(error);
            continue;
        }
        let bytes = fs::read(path).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if let Some(code) = value.pointer("/code").and_then(Value::as_str) {
            if !codes.insert(code.to_owned()) {
                errors.push(format!(
                    "{}: duplicate code `{code}` already documented",
                    path.display()
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} fixture(s) failed validation:\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}

#[test]
fn failure_mode_catalog_has_schema_and_readme() -> TestResult {
    let dir = fixtures_dir();
    let schema_doc = dir.join("SCHEMA.md");
    let readme = dir.join("README.md");
    ensure(
        schema_doc.exists(),
        format!("{}: SCHEMA.md must exist", schema_doc.display()),
    )?;
    ensure(
        readme.exists(),
        format!("{}: README.md must exist", readme.display()),
    )?;
    Ok(())
}
