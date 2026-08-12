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
//! 4. `severity` is one of {info, low, warning, medium, high, critical}.
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

use ee::models::DegradationSeverity;

type TestResult = Result<(), String>;
const CURRENT_MIGRATION_REPAIR_COMMAND: &str = "ee migrate run --workspace . --json";
const LEGACY_MIGRATION_REPAIR_COMMAND: &str = "ee db migrate";

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

fn doctor_dependency_source_file() -> PathBuf {
    src_dir().join("core").join("doctor.rs")
}

fn hooks_installer_source_file() -> PathBuf {
    src_dir().join("hooks").join("installer.rs")
}

fn curate_source_file() -> PathBuf {
    src_dir().join("core").join("curate.rs")
}

fn docs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs")
}

fn allowed_severities() -> BTreeSet<&'static str> {
    DegradationSeverity::ALL
        .into_iter()
        .map(DegradationSeverity::as_str)
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
    ensure_current_migration_repair_surface(&value, &ctx)?;

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
    //
    // Retired fixtures (per SCHEMA.md "Retired fixtures keep the
    // historical `code` and `expected_emission` shape ... while the
    // e2e driver asserts the production emission pattern is absent")
    // intentionally outlive their production emission, so the src/
    // cross-reference is skipped for them. Production emission absence
    // for retired codes is asserted by the per-emission tests under
    // `tests/focus_suggest_schema.rs`, `scripts/e2e_overhaul/*.sh`, and
    // the schema-drift gate, not by this static catalog walker.
    let retired = value
        .pointer("/retired")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !retired {
        let src = src_dir();
        let appears = code_appears_in_source(code, &src)?;
        ensure(
            appears,
            format!(
                "{ctx}: code `{code}` does not appear as a literal under {}; \
                 either the fixture documents a fictional code or the code was \
                 removed from production without updating the catalog. \
                 If the code is intentionally retired, set `retired: true` and \
                 `retired_by: {{ bead, reason }}` on the fixture.",
                src.display()
            ),
        )?;
    } else {
        // For retired fixtures, require a `retired_by.bead` reason so the
        // tombstone always cites the bead that removed the emission. This
        // keeps catalog forensics simple — every retired entry is traceable.
        // Both fields must be non-empty: an empty `bead` or `reason` would
        // satisfy the structural string-type check but defeat the
        // forensics purpose (an unattributed retirement is exactly the
        // drift the tombstone exists to prevent).
        let bead = ensure_string_field(&value, "/retired_by/bead", &ctx)?;
        ensure(
            !bead.trim().is_empty(),
            format!("{ctx}: retired_by.bead must be a non-empty string"),
        )?;
        let reason = ensure_string_field(&value, "/retired_by/reason", &ctx)?;
        ensure(
            !reason.trim().is_empty(),
            format!("{ctx}: retired_by.reason must be a non-empty string"),
        )?;
        // Assert the retirement is HONEST: the retired code must NOT
        // appear as a quoted string literal anywhere under src/. Without
        // this check, the retired flag is a unilateral assertion the
        // catalog walker takes on faith — a developer could flip
        // `retired: true` while leaving the live emission in place, or a
        // later commit could re-introduce the code as a string literal,
        // and nothing in this static gate would catch it. The leading
        // comment notes "Production emission absence for retired codes
        // is asserted by the per-emission tests under
        // tests/focus_suggest_schema.rs, scripts/e2e_overhaul/*.sh, and
        // the schema-drift gate" — but that's only true for codes that
        // happen to have a focused absence-asserting test. For a newly
        // retired code without such a test, this is the only line of
        // defense.
        let src = src_dir();
        let appears = code_appears_in_source(code, &src)?;
        ensure(
            !appears,
            format!(
                "{ctx}: code `{code}` is marked retired (retired_by.bead = `{bead}`) \
                 but still appears as a quoted string literal under {}. \
                 Either the retirement is incomplete (remove the live emission) \
                 or the code was re-introduced after retirement. \
                 If the new emission is intentional, drop the retired flag and \
                 ship a fresh fixture for the resurrected code.",
                src.display()
            ),
        )?;
    }

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

fn collect_doctor_dependency_degraded_codes() -> Result<Vec<String>, String> {
    let doctor_path = doctor_dependency_source_file();
    let doctor_source = fs::read_to_string(&doctor_path)
        .map_err(|error| format!("read {}: {error}", doctor_path.display()))?;
    let dependency_code_regex = Regex::new(r#"degradation_code:\s*"([^"]+)""#)
        .map_err(|error| format!("compile doctor dependency-code regex: {error}"))?;
    let codes: BTreeSet<String> = dependency_code_regex
        .captures_iter(&doctor_source)
        .filter_map(|captures| captures.get(1).map(|match_| match_.as_str().to_owned()))
        .collect();
    Ok(codes.into_iter().collect())
}

fn collect_git_hook_ahead_risk_degraded_codes() -> Result<Vec<String>, String> {
    let hooks_path = hooks_installer_source_file();
    let source = fs::read_to_string(&hooks_path)
        .map_err(|error| format!("read {}: {error}", hooks_path.display()))?;
    let ahead_risk_regex = Regex::new(r#"degraded_codes:\s*vec!\[\s*"([^"]+)"\.to_owned\(\)\s*\]"#)
        .map_err(|error| format!("compile hook ahead-risk degraded-code regex: {error}"))?;
    let codes: BTreeSet<String> = ahead_risk_regex
        .captures_iter(&source)
        .filter_map(|captures| captures.get(1).map(|match_| match_.as_str().to_owned()))
        .collect();
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

fn ensure_current_migration_repair_surface(fixture: &Value, ctx: &str) -> TestResult {
    let expected = fixture
        .pointer("/expected_emission")
        .ok_or_else(|| format!("{ctx}: missing expected_emission"))?;
    for field in ["repair_contains", "repair_string"] {
        if let Some(value) = expected.get(field).and_then(Value::as_str)
            && value.contains(LEGACY_MIGRATION_REPAIR_COMMAND)
        {
            return Err(format!(
                "{ctx}: expected_emission.{field} uses legacy migration repair `{LEGACY_MIGRATION_REPAIR_COMMAND}`; use `{CURRENT_MIGRATION_REPAIR_COMMAND}`"
            ));
        }
    }
    if let Some(values) = expected.get("repair_strings").and_then(Value::as_array) {
        for (idx, value) in values.iter().enumerate() {
            if let Some(text) = value.as_str()
                && text.contains(LEGACY_MIGRATION_REPAIR_COMMAND)
            {
                return Err(format!(
                    "{ctx}: expected_emission.repair_strings[{idx}] uses legacy migration repair `{LEGACY_MIGRATION_REPAIR_COMMAND}`; use `{CURRENT_MIGRATION_REPAIR_COMMAND}`"
                ));
            }
        }
    }
    Ok(())
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

fn taxonomy_has_code(taxonomy: &str, code: &str) -> bool {
    taxonomy
        .lines()
        .any(|line| line.contains(&format!("| `{code}` |")))
}

fn generated_docs_has_fixture_link(docs: &str, code: &str) -> bool {
    docs.contains(&format!("## `{code}`"))
        && docs.contains(&format!("tests/fixtures/failure_modes/{code}.json"))
}

fn fixture_readme_has_code(readme: &str, code: &str) -> bool {
    readme
        .lines()
        .any(|line| line.contains(&format!("| `{code}` |")))
}

#[test]
fn git_hook_ahead_risk_degraded_codes_have_fixture_taxonomy_and_docs() -> TestResult {
    let codes = collect_git_hook_ahead_risk_degraded_codes()?;
    ensure(
        !codes.is_empty(),
        format!(
            "{}: expected at least one hook ahead-risk degraded code",
            hooks_installer_source_file().display()
        ),
    )?;

    let taxonomy_path = docs_dir().join("degraded_code_taxonomy.md");
    let generated_docs_path = docs_dir().join("degraded_codes.md");
    let readme_path = fixtures_dir().join("README.md");
    let taxonomy = fs::read_to_string(&taxonomy_path)
        .map_err(|error| format!("read {}: {error}", taxonomy_path.display()))?;
    let generated_docs = fs::read_to_string(&generated_docs_path)
        .map_err(|error| format!("read {}: {error}", generated_docs_path.display()))?;
    let readme = fs::read_to_string(&readme_path)
        .map_err(|error| format!("read {}: {error}", readme_path.display()))?;

    let mut errors = Vec::new();
    for code in codes {
        let fixture_path = fixtures_dir().join(format!("{code}.json"));
        if !fixture_path.exists() {
            errors.push(format!(
                "{}: missing hook ahead-risk degraded-code fixture for `{code}`",
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
                "{ctx}: fixture code {:?} must match hook ahead-risk code `{code}`",
                fixture_code
            ));
        }
        if !array_contains_string(&fixture, "/surfaces", "hook git-readiness") {
            errors.push(format!(
                "{ctx}: surfaces[] must include `hook git-readiness` for `{code}`"
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
        if !taxonomy_has_code_with_severity(&taxonomy, &code, severity) {
            errors.push(format!(
                "{}: missing taxonomy row for `{code}` with severity `{severity}`",
                taxonomy_path.display()
            ));
        }
        if !fixture_readme_has_code(&readme, &code) {
            errors.push(format!(
                "{}: missing failure-mode README row for `{code}`",
                readme_path.display()
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
            "{} hook ahead-risk degraded catalog error(s):\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
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
fn curate_apply_index_publish_failed_contract_is_pinned() -> TestResult {
    const CODE: &str = "curate_apply_index_publish_failed";
    const REPAIR: &str = "ee job run index_coalesce --workspace . --json";
    const MESSAGE_FRAGMENTS: [&str; 4] = [
        "create-derived memory was committed",
        "automatic publication of durable search-index job",
        "did not complete",
        "Search may omit the new memory until the durable job is retried",
    ];

    let fixture_path = fixtures_dir().join(format!("{CODE}.json"));
    validate_fixture(&fixture_path)?;
    let fixture = read_fixture(&fixture_path)?;
    ensure(
        array_contains_string(&fixture, "/surfaces", "curate apply"),
        format!(
            "{}: surfaces[] must include `curate apply`",
            fixture_path.display()
        ),
    )?;
    ensure(
        fixture.pointer("/severity").and_then(Value::as_str) == Some("medium"),
        format!("{}: severity must remain `medium`", fixture_path.display()),
    )?;
    ensure(
        fixture.pointer("/repair_present").and_then(Value::as_bool) == Some(true),
        format!(
            "{}: repair_present must remain true",
            fixture_path.display()
        ),
    )?;
    ensure(
        fixture
            .pointer("/trigger/description")
            .and_then(Value::as_str)
            .is_some_and(|description| description.contains("publisher claims that job")),
        format!(
            "{}: trigger must pin real post-claim publication failure",
            fixture_path.display()
        ),
    )?;
    ensure(
        fixture
            .pointer("/trigger/invocation")
            .and_then(Value::as_str)
            == Some("ee curate apply <candidate-id> --workspace . --json"),
        format!(
            "{}: invocation must pin `ee curate apply`",
            fixture_path.display()
        ),
    )?;
    ensure(
        fixture
            .pointer("/expected_emission/repair_string")
            .and_then(Value::as_str)
            == Some(REPAIR),
        format!(
            "{}: repair_string must remain `{REPAIR}`",
            fixture_path.display()
        ),
    )?;

    let expected_fragments = fixture
        .pointer("/expected_emission/message_contains")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "{}: expected_emission.message_contains[] missing",
                fixture_path.display()
            )
        })?;
    let source_path = curate_source_file();
    let source = fs::read_to_string(&source_path)
        .map_err(|error| format!("read {}: {error}", source_path.display()))?;
    for fragment in MESSAGE_FRAGMENTS {
        ensure(
            expected_fragments
                .iter()
                .any(|value| value.as_str() == Some(fragment)),
            format!(
                "{}: expected_emission.message_contains[] must pin `{fragment}`",
                fixture_path.display()
            ),
        )?;
        ensure(
            source.contains(fragment),
            format!(
                "{}: runtime emission must contain fixture fragment `{fragment}`",
                source_path.display()
            ),
        )?;
    }
    ensure(
        source.contains(REPAIR),
        format!(
            "{}: runtime repair must contain `{REPAIR}`",
            source_path.display()
        ),
    )?;

    let taxonomy_path = docs_dir().join("degraded_code_taxonomy.md");
    let taxonomy = fs::read_to_string(&taxonomy_path)
        .map_err(|error| format!("read {}: {error}", taxonomy_path.display()))?;
    ensure(
        taxonomy_has_code_with_severity(&taxonomy, CODE, "medium"),
        format!(
            "{}: missing `{CODE}` taxonomy row with medium severity",
            taxonomy_path.display()
        ),
    )?;

    let readme_path = fixtures_dir().join("README.md");
    let readme = fs::read_to_string(&readme_path)
        .map_err(|error| format!("read {}: {error}", readme_path.display()))?;
    ensure(
        fixture_readme_has_code(&readme, CODE),
        format!("{}: missing `{CODE}` catalog row", readme_path.display()),
    )?;

    let generated_docs_path = docs_dir().join("degraded_codes.md");
    let generated_docs = fs::read_to_string(&generated_docs_path)
        .map_err(|error| format!("read {}: {error}", generated_docs_path.display()))?;
    ensure(
        generated_docs_has_fixture_link(&generated_docs, CODE),
        format!(
            "{}: missing `{CODE}` heading or fixture link",
            generated_docs_path.display()
        ),
    )?;

    Ok(())
}

#[test]
fn doctor_dependency_degraded_codes_have_fixture_taxonomy_and_docs() -> TestResult {
    let codes = collect_doctor_dependency_degraded_codes()?;
    ensure(
        !codes.is_empty(),
        format!(
            "{}: expected at least one dependency degradation code",
            doctor_dependency_source_file().display()
        ),
    )?;

    let taxonomy_path = docs_dir().join("degraded_code_taxonomy.md");
    let generated_docs_path = docs_dir().join("degraded_codes.md");
    let readme_path = fixtures_dir().join("README.md");
    let taxonomy = fs::read_to_string(&taxonomy_path)
        .map_err(|error| format!("read {}: {error}", taxonomy_path.display()))?;
    let generated_docs = fs::read_to_string(&generated_docs_path)
        .map_err(|error| format!("read {}: {error}", generated_docs_path.display()))?;
    let readme = fs::read_to_string(&readme_path)
        .map_err(|error| format!("read {}: {error}", readme_path.display()))?;

    let mut errors = Vec::new();
    for code in codes {
        let fixture_path = fixtures_dir().join(format!("{code}.json"));
        if !fixture_path.exists() {
            errors.push(format!(
                "{}: missing doctor dependency degraded-code fixture for `{code}`",
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
                "{ctx}: fixture code {:?} must match doctor dependency code `{code}`",
                fixture_code
            ));
        }

        let surfaces = fixture
            .pointer("/surfaces")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let has_dependency_surface = surfaces
            .iter()
            .any(|surface| surface.as_str() == Some("dependency contract"));
        let has_doctor_surface = surfaces
            .iter()
            .any(|surface| surface.as_str() == Some("doctor"));
        if !has_dependency_surface && !has_doctor_surface {
            errors.push(format!(
                "{ctx}: surfaces[] must include `dependency contract` or `doctor` for `{code}`"
            ));
        }

        if !taxonomy_has_code(&taxonomy, &code) {
            errors.push(format!(
                "{}: missing taxonomy row for `{code}`",
                taxonomy_path.display()
            ));
        }
        if !fixture_readme_has_code(&readme, &code) {
            errors.push(format!(
                "{}: missing failure-mode README row for `{code}`",
                readme_path.display()
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
            "{} doctor dependency degraded catalog error(s):\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
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
