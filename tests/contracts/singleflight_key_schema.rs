//! Contract checks for the canonical single-flight key schema.

use std::fs;
use std::path::PathBuf;

use ee::core::supported_schemas;
use ee::models::{
    KNOWN_SCHEMAS, SINGLEFLIGHT_KEY_CANONICAL_VERSION, SINGLEFLIGHT_KEY_SCHEMA_V1,
    SINGLEFLIGHT_POSTURE_SCHEMA_V1, SingleFlightKey, SingleFlightKeyInput, SingleFlightSurface,
    sample_singleflight_keys,
};
use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

#[test]
fn singleflight_key_schema_is_documented_and_registered() -> TestResult {
    let schema = read_json("docs/schemas/ee.singleflight.key.v1.json")?;

    ensure_json_str(
        &schema,
        "/$schema",
        "https://json-schema.org/draft/2020-12/schema",
    )?;
    ensure_json_str(
        &schema,
        "/$id",
        "https://eidetic-engine/schemas/ee.singleflight.key.v1.json",
    )?;
    ensure_json_str(&schema, "/title", SINGLEFLIGHT_KEY_SCHEMA_V1)?;
    ensure_json_bool(&schema, "/additionalProperties", false)?;
    ensure_json_str(
        &schema,
        "/properties/schema/const",
        SINGLEFLIGHT_KEY_SCHEMA_V1,
    )?;

    if !KNOWN_SCHEMAS.contains(&SINGLEFLIGHT_KEY_SCHEMA_V1) {
        return Err("KNOWN_SCHEMAS missing ee.singleflight.key.v1".to_owned());
    }

    let supported = supported_schemas()
        .into_iter()
        .map(|schema| schema.schema)
        .collect::<Vec<_>>();
    if !supported.contains(&SINGLEFLIGHT_KEY_SCHEMA_V1) {
        return Err("supported_schemas() missing ee.singleflight.key.v1".to_owned());
    }

    Ok(())
}

#[test]
fn singleflight_posture_schema_is_documented_and_registered() -> TestResult {
    let schema = read_json("docs/schemas/ee.singleflight.posture.v1.json")?;

    ensure_json_str(
        &schema,
        "/$schema",
        "https://json-schema.org/draft/2020-12/schema",
    )?;
    ensure_json_str(
        &schema,
        "/$id",
        "https://eidetic-engine/schemas/ee.singleflight.posture.v1.json",
    )?;
    ensure_json_str(&schema, "/title", SINGLEFLIGHT_POSTURE_SCHEMA_V1)?;
    ensure_json_bool(&schema, "/additionalProperties", false)?;
    ensure_json_str(
        &schema,
        "/properties/schema/const",
        SINGLEFLIGHT_POSTURE_SCHEMA_V1,
    )?;
    if schema
        .pointer("/$defs/surfacePosture/required")
        .and_then(Value::as_array)
        .is_none_or(|required| !required.iter().any(|field| field == "lastKey"))
    {
        return Err("single-flight posture schema must require surface lastKey".to_owned());
    }
    ensure_json_bool(&schema, "/$defs/lastKey/additionalProperties", false)?;

    if !KNOWN_SCHEMAS.contains(&SINGLEFLIGHT_POSTURE_SCHEMA_V1) {
        return Err("KNOWN_SCHEMAS missing ee.singleflight.posture.v1".to_owned());
    }

    let supported = supported_schemas()
        .into_iter()
        .map(|schema| schema.schema)
        .collect::<Vec<_>>();
    if !supported.contains(&SINGLEFLIGHT_POSTURE_SCHEMA_V1) {
        return Err("supported_schemas() missing ee.singleflight.posture.v1".to_owned());
    }

    Ok(())
}

#[test]
fn singleflight_key_fixture_is_redaction_safe_and_stable() -> TestResult {
    let generated = serde_json::to_string_pretty(&sample_singleflight_keys())
        .map_err(|error| format!("serialize sample keys: {error}"))?;
    let fixture_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("singleflight")
        .join("key_samples.json.golden");
    let fixture = fs::read_to_string(&fixture_path)
        .map_err(|error| format!("read {}: {error}", fixture_path.display()))?;
    if generated != fixture.trim_end_matches('\n') {
        return Err(format!(
            "single-flight key golden drifted\nexpected fixture:\n{}\nactual:\n{}",
            fixture, generated
        ));
    }

    for forbidden in [
        "release token secret",
        "secret",
        "/workspace/eidetic_engine_cli",
    ] {
        if generated.contains(forbidden) {
            return Err(format!(
                "single-flight key fixture leaked forbidden raw text: {forbidden}"
            ));
        }
    }

    Ok(())
}

#[test]
fn singleflight_key_model_covers_required_non_equivalence_fields() -> TestResult {
    let mut input = SingleFlightKeyInput::new(
        SingleFlightSurface::Context,
        "/workspace/eidetic_engine_cli",
        42,
        "ee.context.v1",
    );
    input.index_generation = Some(17);
    input.graph_generation = Some(9);
    input.query_text = Some("prepare release with sensitive token");
    input.profile = Some("balanced");
    input.max_tokens = Some(4000);
    input.source_mode = Some("hybrid");
    input.redaction_level = Some("standard");
    input.explain = true;
    input.feature_flags = &["graph", "lexical-bm25"];
    input.option_pairs = &[("packDna", "enabled")];

    let key = SingleFlightKey::from_input(&input);
    ensure_str(&key.schema, SINGLEFLIGHT_KEY_SCHEMA_V1, "schema")?;
    ensure_u32(
        key.canonical_version,
        SINGLEFLIGHT_KEY_CANONICAL_VERSION,
        "canonicalVersion",
    )?;
    ensure_bool(key.explain, true, "explain")?;
    ensure_option_u64(key.index_generation, Some(17), "indexGeneration")?;
    ensure_option_u64(key.graph_generation, Some(9), "graphGeneration")?;
    ensure_option_str(key.profile.as_deref(), Some("balanced"), "profile")?;
    ensure_option_u32(key.max_tokens, Some(4000), "maxTokens")?;
    ensure_option_str(key.source_mode.as_deref(), Some("hybrid"), "sourceMode")?;
    ensure_option_str(
        key.redaction_level.as_deref(),
        Some("standard"),
        "redactionLevel",
    )
}

fn ensure_json_str(json: &Value, pointer: &str, expected: &str) -> TestResult {
    let actual = json
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{pointer} missing string"))?;
    ensure_str(actual, expected, pointer)
}

fn ensure_json_bool(json: &Value, pointer: &str, expected: bool) -> TestResult {
    let actual = json
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{pointer} missing bool"))?;
    ensure_bool(actual, expected, pointer)
}

fn ensure_str(actual: &str, expected: &str, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_bool(actual: bool, expected: bool, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected}, got {actual}"))
    }
}

fn ensure_u32(actual: u32, expected: u32, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected}, got {actual}"))
    }
}

fn ensure_option_u32(actual: Option<u32>, expected: Option<u32>, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_option_u64(actual: Option<u64>, expected: Option<u64>, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_option_str(actual: Option<&str>, expected: Option<&str>, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}
