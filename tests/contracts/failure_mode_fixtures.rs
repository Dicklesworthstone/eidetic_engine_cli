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

use regex_lite::Regex;
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

fn degradation_source_file() -> PathBuf {
    src_dir().join("models").join("degradation.rs")
}

fn hygiene_beads_state_source_file() -> PathBuf {
    src_dir().join("core").join("hygiene_beads_state.rs")
}

fn docs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs")
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

fn collect_workspace_hygiene_codes() -> Result<Vec<String>, String> {
    let mut codes = BTreeSet::new();

    let degradation_path = degradation_source_file();
    let degradation_source = fs::read_to_string(&degradation_path)
        .map_err(|error| format!("read {}: {error}", degradation_path.display()))?;
    let degradation_regex =
        Regex::new(r#"pub const WORKSPACE_HYGIENE_[A-Z0-9_]+_CODE:\s*&str\s*=\s*"([^"]+)""#)
            .map_err(|error| format!("compile workspace-hygiene code regex: {error}"))?;
    codes.extend(
        degradation_regex
            .captures_iter(&degradation_source)
            .filter_map(|captures| captures.get(1).map(|match_| match_.as_str().to_owned())),
    );

    let beads_path = hygiene_beads_state_source_file();
    let beads_source = fs::read_to_string(&beads_path)
        .map_err(|error| format!("read {}: {error}", beads_path.display()))?;
    let beads_regex =
        Regex::new(r#"pub const [A-Z0-9_]+:\s*&str\s*=\s*"(workspace_hygiene_[^"]+)""#)
            .map_err(|error| format!("compile beads workspace-hygiene code regex: {error}"))?;
    codes.extend(
        beads_regex
            .captures_iter(&beads_source)
            .filter_map(|captures| captures.get(1).map(|match_| match_.as_str().to_owned())),
    );

    Ok(codes.into_iter().collect())
}

fn read_fixture(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn array_contains_string(value: &Value, pointer: &str, expected: &str) -> bool {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(expected)))
}

fn string_array_is_non_empty(value: &Value, pointer: &str) -> bool {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty() && items.iter().all(Value::is_string))
}

fn repair_strings_are_pinned(expected: &Value) -> bool {
    expected
        .get("repair_string")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
        || expected
            .get("repair_strings")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty() && items.iter().all(Value::is_string))
}

fn fixture_only_rationale(code: &str) -> Option<&'static str> {
    match code {
        // These are shared git degraded codes. Their catalog fixtures are
        // currently public-triggered by `ee swarm brief` and list
        // `workspace hygiene` as a surface until the workspace-hygiene
        // command emits the same shared code through its own CLI path.
        "git_unavailable" | "git_not_repository" => Some("shared git degraded fixture"),
        _ => None,
    }
}

fn taxonomy_has_code_with_severity(taxonomy: &str, code: &str, severity: &str) -> bool {
    taxonomy.lines().any(|line| {
        line.contains(&format!("| `{code}` |")) && line.contains(&format!("| {severity} |"))
    })
}

fn generated_docs_has_fixture_link(docs: &str, code: &str) -> bool {
    docs.contains(&format!("## `{code}`"))
        && docs.contains(&format!("tests/fixtures/failure_modes/{code}.json"))
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
        let value = match read_fixture(path) {
            Ok(value) => value,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
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

#[test]
fn workspace_hygiene_degraded_codes_have_fixture_taxonomy_and_trigger_contract() -> TestResult {
    let codes = collect_workspace_hygiene_codes()?;
    ensure(
        !codes.is_empty(),
        format!(
            "{}: expected at least one WORKSPACE_HYGIENE_*_CODE constant",
            degradation_source_file().display()
        ),
    )?;

    let taxonomy_path = docs_dir().join("degraded_code_taxonomy.md");
    let generated_docs_path = docs_dir().join("degraded_codes.md");
    let taxonomy = fs::read_to_string(&taxonomy_path)
        .map_err(|error| format!("read {}: {error}", taxonomy_path.display()))?;
    let generated_docs = fs::read_to_string(&generated_docs_path)
        .map_err(|error| format!("read {}: {error}", generated_docs_path.display()))?;

    let mut errors = Vec::new();
    for code in codes {
        let fixture_path = fixtures_dir().join(format!("{code}.json"));
        if !fixture_path.exists() {
            errors.push(format!(
                "{}: missing workspace-hygiene degraded-code fixture for `{code}`",
                fixture_path.display()
            ));
            continue;
        }

        let fixture = match read_fixture(&fixture_path) {
            Ok(fixture) => fixture,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let ctx = fixture_path.display().to_string();

        let fixture_code = fixture.pointer("/code").and_then(Value::as_str);
        if fixture_code != Some(code.as_str()) {
            errors.push(format!(
                "{ctx}: fixture code {:?} must match workspace-hygiene constant `{code}`",
                fixture_code
            ));
        }
        if !array_contains_string(&fixture, "/surfaces", "workspace hygiene") {
            errors.push(format!(
                "{ctx}: surfaces[] must include `workspace hygiene` for `{code}`"
            ));
        }

        let severity = fixture
            .pointer("/severity")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        let expected = fixture
            .pointer("/expected_emission")
            .unwrap_or(&Value::Null);
        let expected_severity = expected
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        if expected_severity != severity {
            errors.push(format!(
                "{ctx}: expected_emission.severity `{expected_severity}` must match fixture severity `{severity}`"
            ));
        }
        if !string_array_is_non_empty(expected, "/message_contains") {
            errors.push(format!(
                "{ctx}: expected_emission.message_contains must pin at least one substring"
            ));
        }
        if fixture.pointer("/repair_present").and_then(Value::as_bool) != Some(true) {
            errors.push(format!(
                "{ctx}: repair_present must be true for workspace-hygiene degraded code `{code}`"
            ));
        }
        if expected
            .get("repair_contains")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            errors.push(format!(
                "{ctx}: expected_emission.repair_contains must pin the repair topic"
            ));
        }
        if !repair_strings_are_pinned(expected) {
            errors.push(format!(
                "{ctx}: expected_emission must pin repair_string or repair_strings"
            ));
        }

        let invocation = fixture
            .pointer("/trigger/invocation")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !invocation.contains("ee workspace hygiene") && fixture_only_rationale(&code).is_none() {
            errors.push(format!(
                "{ctx}: trigger.invocation must use `ee workspace hygiene` or the test must document a fixture-only rationale for `{code}`"
            ));
        }

        if !taxonomy_has_code_with_severity(&taxonomy, &code, severity) {
            errors.push(format!(
                "{}: missing taxonomy row for `{code}` with severity `{severity}`",
                taxonomy_path.display()
            ));
        }
        if !generated_docs_has_fixture_link(&generated_docs, &code) {
            errors.push(format!(
                "{}: generated degraded-code docs must include heading and fixture link for `{code}`",
                generated_docs_path.display()
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} workspace-hygiene degraded catalog error(s):\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}
