//! Contract checks for the bd-3omr5 mesh command-mode matrix.
//!
//! The runtime e2e proves the current local-first behavior. These checks pin
//! the machine-readable fixture and docs so future cache/revisable/blocking
//! work cannot silently drift from the accepted mode vocabulary, precedence,
//! surface matrix, or response-envelope rules.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const DOC_PATH: &str = "docs/mesh/command_modes.md";
const FIXTURE_PATH: &str = "tests/fixtures/mesh/command_modes.v1.json";
const MODES: &[&str] = &["off", "cache", "revisable", "blocking"];
const SURFACES: &[&str] = &["search", "context", "pack", "why", "status"];
const PRECEDENCE: &[&str] = &["command_flag", "environment", "config", "built_in_default"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_text(relative: &str) -> Result<String, String> {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let text = read_text(relative)?;
    serde_json::from_str(&text).map_err(|error| format!("parse {relative}: {error}"))
}

fn string_array(json: &Value, pointer: &str) -> Result<Vec<String>, String> {
    let array = json
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{pointer} missing array"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{pointer} contains non-string item: {item:?}"))
        })
        .collect()
}

fn ensure_string_eq(json: &Value, pointer: &str, expected: &str) -> TestResult {
    let actual = json
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{pointer} missing string"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{pointer}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_bool_eq(json: &Value, pointer: &str, expected: bool) -> TestResult {
    let actual = json
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{pointer} missing bool"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{pointer}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_contains(haystack: &str, needle: &str, label: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{label} missing {needle:?}"))
    }
}

#[test]
fn command_modes_fixture_pins_vocabulary_and_precedence() -> TestResult {
    let fixture = read_json(FIXTURE_PATH)?;

    ensure_string_eq(&fixture, "/schema", "ee.mesh.command_modes.v1")?;
    ensure_string_eq(&fixture, "/bead", "bd-3omr5")?;
    ensure_string_eq(&fixture, "/defaultMode", "off")?;

    if string_array(&fixture, "/precedence")? != PRECEDENCE {
        return Err(format!(
            "precedence drifted: expected {PRECEDENCE:?}, got {:?}",
            string_array(&fixture, "/precedence")?
        ));
    }
    if string_array(&fixture, "/surfaces")? != SURFACES {
        return Err(format!(
            "surfaces drifted: expected {SURFACES:?}, got {:?}",
            string_array(&fixture, "/surfaces")?
        ));
    }

    let modes = fixture
        .pointer("/modes")
        .and_then(Value::as_array)
        .ok_or_else(|| "modes missing array".to_string())?;
    let actual_modes: Vec<String> = modes
        .iter()
        .map(|mode| {
            mode.get("mode")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("mode row missing string mode: {mode:?}"))
        })
        .collect::<Result<_, _>>()?;
    if actual_modes != MODES {
        return Err(format!(
            "mode order drifted: expected {MODES:?}, got {actual_modes:?}"
        ));
    }

    for mode in modes {
        let mode_name = mode
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("mode row missing mode: {mode:?}"))?;
        for field in ["summary", "network", "cache", "revisionTokens"] {
            let value = mode
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("mode {mode_name} missing string field {field}"))?;
            if value.trim().is_empty() {
                return Err(format!("mode {mode_name} field {field} is empty"));
            }
        }
        mode.get("blockingPeerWait")
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("mode {mode_name} missing bool blockingPeerWait"))?;
    }

    ensure_bool_eq(&fixture, "/modes/0/blockingPeerWait", false)?;
    ensure_bool_eq(&fixture, "/modes/1/blockingPeerWait", false)?;
    ensure_bool_eq(&fixture, "/modes/2/blockingPeerWait", false)?;
    ensure_bool_eq(&fixture, "/modes/3/blockingPeerWait", true)?;
    ensure_string_eq(
        &fixture,
        "/modes/3/currentRuntimeStatus",
        "unsupported_until_latency_budgeted_peer_query_lands",
    )?;

    Ok(())
}

#[test]
fn surface_matrix_covers_every_surface_mode_pair() -> TestResult {
    let fixture = read_json(FIXTURE_PATH)?;
    let matrix = fixture
        .pointer("/surfaceMatrix")
        .and_then(Value::as_object)
        .ok_or_else(|| "surfaceMatrix missing object".to_string())?;

    for surface in SURFACES {
        let row = matrix
            .get(*surface)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("surfaceMatrix missing {surface} row"))?;
        for mode in MODES {
            let value = row
                .get(*mode)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("surfaceMatrix.{surface}.{mode} missing string"))?;
            if value.trim().is_empty() {
                return Err(format!("surfaceMatrix.{surface}.{mode} is empty"));
            }
        }
    }

    let envelope_rules = fixture
        .pointer("/responseEnvelopeRules")
        .and_then(Value::as_array)
        .ok_or_else(|| "responseEnvelopeRules missing array".to_string())?;
    let envelope_modes: Vec<String> = envelope_rules
        .iter()
        .map(|rule| {
            let mode = rule
                .get("mode")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("envelope rule missing mode: {rule:?}"))?;
            for field in ["resultShape", "degraded"] {
                let value = rule
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("envelope rule {mode} missing {field}"))?;
                if value.trim().is_empty() {
                    return Err(format!("envelope rule {mode}.{field} is empty"));
                }
            }
            Ok(mode.to_owned())
        })
        .collect::<Result<_, String>>()?;
    if envelope_modes != MODES {
        return Err(format!(
            "responseEnvelopeRules modes drifted: expected {MODES:?}, got {envelope_modes:?}"
        ));
    }

    Ok(())
}

#[test]
fn docs_and_fixture_stay_cross_referenced() -> TestResult {
    let docs = read_text(DOC_PATH)?;
    let fixture = read_json(FIXTURE_PATH)?;

    ensure_contains(&docs, "tests/fixtures/mesh/command_modes.v1.json", DOC_PATH)?;
    ensure_contains(&docs, "docs/mesh/verification_matrix.md", DOC_PATH)?;
    ensure_contains(&docs, "response-envelope rules", DOC_PATH)?;

    for mode in MODES {
        ensure_contains(&docs, &format!("`{mode}`"), DOC_PATH)?;
    }
    for command in [
        "`ee search --mesh <mode>`",
        "`ee context --mesh <mode>`",
        "`ee pack --mesh <mode>`",
        "`ee why --mesh <mode>`",
        "`ee status --mesh <mode>`",
    ] {
        ensure_contains(&docs, command, DOC_PATH)?;
    }

    let redaction_rules = string_array(&fixture, "/redactionRules")?;
    for required in [
        "remote workspace paths",
        "denied or quarantined material",
        "status may expose counts",
    ] {
        if !redaction_rules.iter().any(|rule| rule.contains(required)) {
            return Err(format!("redactionRules missing phrase {required:?}"));
        }
    }

    Ok(())
}
