//! J6.1 contract: per-fixture repair-string pinning + regex extraction
//! (`bd-17c65.10.6.1`).
//!
//! Walks `tests/fixtures/failure_modes/*.json` and, for each fixture
//! that has a pinning field set under `expected_emission`
//! (`repair_string` xor `repair_strings`), asserts the J6.1 contract:
//!
//! 1. Exactly one pinning field is populated when either is present
//!    (mutual exclusion).
//! 2. Pinned strings are non-empty.
//! 3. When `repair_command_regex` is present, it compiles under the
//!    Rust `regex-lite` crate and contains exactly one named capture
//!    group named `cmd`.
//! 4. When both a pinning field and `repair_command_regex` are
//!    present, the regex matches at least one of the pinned strings
//!    and the `cmd` group is non-empty.
//! 5. When `repair_contains` is also set, each pinned string contains
//!    the `repair_contains` substring (cross-field consistency).
//!
//! The contract is opt-in per fixture: fixtures with `repair_present:
//! true` but no pinning field are skipped (full backfill is tracked
//! by follow-up bead `bd-17c65.10.6.1.1`). Once a fixture is pinned,
//! the contract test prevents silent drift.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use regex_lite::Regex;
use serde_json::Value;

type TestResult = Result<(), String>;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("failure_modes")
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

/// Collect pinned strings from a fixture's expected_emission. Returns
/// None if neither `repair_string` nor `repair_strings` is populated
/// (fixture is unpinned). Returns an error if BOTH are populated
/// (the schema forbids that) or if the values have wrong types.
fn collect_pinned_strings(expected: &Value, ctx: &str) -> Result<Option<Vec<String>>, String> {
    let single = expected.get("repair_string").filter(|v| !v.is_null());
    let array = expected.get("repair_strings").filter(|v| !v.is_null());

    match (single, array) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(format!(
            "{ctx}: both `repair_string` and `repair_strings` are set; \
             pick one. Use `repair_strings` only for codes that emit \
             multiple repair variants from different trigger branches."
        )),
        (Some(s), None) => {
            let value = s
                .as_str()
                .ok_or_else(|| format!("{ctx}: `repair_string` must be a string"))?;
            ensure(
                !value.is_empty(),
                format!("{ctx}: `repair_string` must not be empty"),
            )?;
            Ok(Some(vec![value.to_owned()]))
        }
        (None, Some(arr)) => {
            let items = arr
                .as_array()
                .ok_or_else(|| format!("{ctx}: `repair_strings` must be an array"))?;
            ensure(
                !items.is_empty(),
                format!("{ctx}: `repair_strings` must not be empty when set"),
            )?;
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("{ctx}: `repair_strings[{idx}]` must be a string"))?;
                ensure(
                    !s.is_empty(),
                    format!("{ctx}: `repair_strings[{idx}]` must not be empty"),
                )?;
                out.push(s.to_owned());
            }
            Ok(Some(out))
        }
    }
}

/// Validate the named-capture contract on a repair_command_regex
/// string. Returns the compiled regex on success.
fn compile_repair_command_regex(pattern: &str, ctx: &str) -> Result<Regex, String> {
    let regex = Regex::new(pattern)
        .map_err(|error| format!("{ctx}: `repair_command_regex` failed to compile: {error}"))?;

    // regex-lite exposes capture_names() returning an iterator over
    // Option<&str>; named captures appear as Some(name).
    let names: Vec<&str> = regex.capture_names().flatten().collect();
    ensure(
        names.len() == 1,
        format!(
            "{ctx}: `repair_command_regex` must have exactly one named \
             capture group; found {} named groups ({:?})",
            names.len(),
            names
        ),
    )?;
    ensure(
        names[0] == "cmd",
        format!(
            "{ctx}: `repair_command_regex` named capture must be `cmd`; \
             found `{}`",
            names[0]
        ),
    )?;
    Ok(regex)
}

fn validate_fixture_pinning(path: &Path) -> TestResult {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let ctx = path.display().to_string();

    let expected = value
        .get("expected_emission")
        .ok_or_else(|| format!("{ctx}: missing expected_emission"))?;

    let pinned = collect_pinned_strings(expected, &ctx)?;
    let regex_str = expected
        .get("repair_command_regex")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_str()
                .ok_or_else(|| format!("{ctx}: `repair_command_regex` must be a string"))
                .map(str::to_owned)
        })
        .transpose()?;

    // Skip entirely-unpinned fixtures (backfill is incremental per
    // bd-17c65.10.6.1.1).
    if pinned.is_none() && regex_str.is_none() {
        return Ok(());
    }

    // Compile the regex if present and validate its name contract.
    let regex = match regex_str.as_deref() {
        Some(pattern) => Some(compile_repair_command_regex(pattern, &ctx)?),
        None => None,
    };

    // If we have both pinned strings and a regex, assert the regex
    // matches at least one pinned string and the `cmd` capture is
    // non-empty.
    if let (Some(strings), Some(re)) = (pinned.as_deref(), regex.as_ref()) {
        let mut matched = false;
        for s in strings {
            if let Some(caps) = re.captures(s) {
                if let Some(cmd) = caps.name("cmd") {
                    ensure(
                        !cmd.as_str().is_empty(),
                        format!(
                            "{ctx}: `repair_command_regex` matched repair string `{}` but \
                             extracted an empty `cmd` group",
                            s
                        ),
                    )?;
                    matched = true;
                    break;
                }
            }
        }
        ensure(
            matched,
            format!(
                "{ctx}: `repair_command_regex` does not match any of the pinned \
                 repair strings. Pinned: {:?}",
                strings
            ),
        )?;
    }

    // Cross-field consistency: if repair_contains is set, every pinned
    // string must contain it.
    if let Some(strings) = pinned.as_deref() {
        if let Some(contains) = expected
            .get("repair_contains")
            .filter(|v| !v.is_null())
            .and_then(Value::as_str)
        {
            for s in strings {
                ensure(
                    s.contains(contains),
                    format!(
                        "{ctx}: pinned repair string `{}` does not contain the \
                         `repair_contains` substring `{}`; fix one of them",
                        s, contains
                    ),
                )?;
            }
        }
    }

    Ok(())
}

#[test]
fn failure_mode_fixtures_pinned_repairs_are_consistent() -> TestResult {
    let dir = fixtures_dir();
    let fixtures = list_fixture_files(&dir)?;
    let mut errors: Vec<String> = Vec::new();
    for path in &fixtures {
        if let Err(error) = validate_fixture_pinning(path) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} fixture(s) failed J6.1 repair-pinning validation:\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}

#[test]
fn failure_mode_fixtures_show_some_pinned_coverage() -> TestResult {
    // Ratcheted floor for the J6.1.1 backfill (bd-17c65.10.6.1.1).
    //
    // The floor only ratchets UP — never down. It is the minimum count
    // of fixtures that MUST carry a pinned `repair_string` or
    // `repair_strings` field. When new fixtures are added with pinned
    // repairs, raise this constant in the same PR. When new fixtures
    // are added without pinning, the floor stays where it is and the
    // unpinned fixtures contribute backfill debt against
    // `bd-17c65.10.6.1.1`.
    //
    // History:
    //   - 2026-05-13 J6.1 seed: 6 pinned (swarm-brief connector codes).
    //   - 2026-05-13 J6.1.1 first backfill pass: 82 pinned via
    //     `scripts/audit_randomness_sources.sh`-pattern grep + jq
    //     patching against verified production literals.
    const PINNED_FLOOR: usize = 80;

    let dir = fixtures_dir();
    let fixtures = list_fixture_files(&dir)?;
    let mut pinned_count = 0usize;
    for path in &fixtures {
        let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let Some(expected) = value.get("expected_emission") else {
            continue;
        };
        let single = expected
            .get("repair_string")
            .filter(|v| !v.is_null())
            .is_some();
        let array = expected
            .get("repair_strings")
            .filter(|v| !v.is_null())
            .is_some();
        if single || array {
            pinned_count += 1;
        }
    }
    ensure(
        pinned_count >= PINNED_FLOOR,
        format!(
            "J6.1.1 floor: at least {PINNED_FLOOR} fixtures must be pinned with \
             `repair_string` or `repair_strings`; found {pinned_count}. The floor \
             ratchets UP only — never down. If you removed a pinned fixture, pin \
             a replacement to keep the floor satisfied OR raise the floor when \
             adding new pinned fixtures."
        ),
    )
}
