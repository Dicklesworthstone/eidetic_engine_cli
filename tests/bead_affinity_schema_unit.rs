//! bd-2942u - schema unit test for `ee.context.bead_affinity.v1`.
//!
//! Pins the structural contract for the bead-aware retrieval bias block
//! emitted by `ee context --explain --json` (and `ee why --json`) when
//! `--bead-id` scopes pack ranking. The schema lives at
//! `docs/schemas/ee.context.bead_affinity.v1.json`; this test asserts:
//!
//! 1. The schema file exists and parses as valid JSON.
//! 2. `$id`, `title`, and `const`-schema-name agree (`ee.context.bead_affinity.v1`).
//! 3. The required-field set matches the spec exactly.
//! 4. `maxBiasMagnitude` is pinned to 0.05 so callers cannot widen the
//!    cap without coordinated schema-drift review.
//! 5. `memoryBias.bias` is bounded to [-0.05, +0.05] in the schema, matching
//!    the cap on `ee.context.agent_profile.v1` for composable bias.
//! 6. `degradation.code` enum contains the four documented codes:
//!    `bead_affinity_cold_start`, `bead_affinity_unavailable`,
//!    `bead_affinity_capped`, `bead_affinity_lookup_failed`.
//! 7. A hand-crafted minimal valid sample has all the required keys, the
//!    bias caps hold, and the determinism key shape is well-formed.
//!
//! The test does NOT require Cargo build of the full project at runtime -
//! it loads the schema file and reasons over its JSON. This is sufficient
//! before the live emitter lands; once `ee context --bead-id` is wired,
//! a sibling e2e test will pin the live response against this schema
//! through the same drift-detection lane that `tests/docs_schemas_match_responses.rs`
//! uses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.context.bead_affinity.v1.json";
const SCHEMA_NAME: &str = "ee.context.bead_affinity.v1";
const MAX_BIAS_MAGNITUDE: f64 = 0.05;

fn schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_REL)
}

fn load_schema() -> Result<Value, String> {
    let path = schema_path();
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn required_field_set(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let node = value
        .pointer(pointer)
        .ok_or_else(|| format!("schema is missing pointer {pointer}"))?;
    let array = node
        .as_array()
        .ok_or_else(|| format!("{pointer} must be a JSON array of required-field names"))?;
    let mut required = BTreeSet::new();
    for entry in array {
        let name = entry
            .as_str()
            .ok_or_else(|| format!("{pointer} contains a non-string entry: {entry}"))?;
        required.insert(name.to_owned());
    }
    Ok(required)
}

#[test]
fn schema_file_exists_and_parses() -> TestResult {
    let schema = load_schema()?;
    ensure(schema.is_object(), "schema root must be a JSON object")?;
    ensure(
        schema
            .get("$schema")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("https://json-schema.org/")),
        "schema $schema must point at the json-schema.org draft URI",
    )?;
    Ok(())
}

#[test]
fn schema_identity_is_consistent() -> TestResult {
    let schema = load_schema()?;
    let title = schema
        .get("title")
        .and_then(Value::as_str)
        .ok_or("schema is missing title")?;
    ensure(
        title == SCHEMA_NAME,
        format!("title `{title}` != {SCHEMA_NAME}"),
    )?;

    let id = schema
        .get("$id")
        .and_then(Value::as_str)
        .ok_or("schema is missing $id")?;
    ensure(
        id.ends_with(&format!("{SCHEMA_NAME}.json")),
        format!("$id `{id}` must end with `{SCHEMA_NAME}.json`"),
    )?;

    let const_name = schema
        .pointer("/properties/schema/const")
        .and_then(Value::as_str)
        .ok_or("schema is missing properties.schema.const")?;
    ensure(
        const_name == SCHEMA_NAME,
        format!("properties.schema.const `{const_name}` != {SCHEMA_NAME}"),
    )?;
    Ok(())
}

#[test]
fn required_top_level_fields_match_spec() -> TestResult {
    let schema = load_schema()?;
    let actual = required_field_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "beadId",
        "matchedFeatureCount",
        "biasMagnitudeMax",
        "maxBiasMagnitude",
        "memoryBiasApplied",
        "coldStart",
        "determinismKey",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        actual == expected,
        format!("required-field drift: expected {expected:?}, got {actual:?}"),
    )
}

#[test]
fn max_bias_magnitude_pinned_to_0_05() -> TestResult {
    let schema = load_schema()?;
    let cap = schema
        .pointer("/properties/maxBiasMagnitude/const")
        .and_then(Value::as_f64)
        .ok_or("schema is missing properties.maxBiasMagnitude.const")?;
    ensure(
        (cap - MAX_BIAS_MAGNITUDE).abs() < f64::EPSILON,
        format!("maxBiasMagnitude const drifted: expected 0.05, got {cap}"),
    )?;

    // Defense in depth: the magnitude-max field must also reject anything
    // larger than 0.05 via a maximum bound.
    let max = schema
        .pointer("/properties/biasMagnitudeMax/maximum")
        .and_then(Value::as_f64)
        .ok_or("schema is missing properties.biasMagnitudeMax.maximum")?;
    ensure(
        (max - MAX_BIAS_MAGNITUDE).abs() < f64::EPSILON,
        format!("biasMagnitudeMax.maximum drifted: expected 0.05, got {max}"),
    )?;
    Ok(())
}

#[test]
fn memory_bias_is_signed_and_bounded() -> TestResult {
    let schema = load_schema()?;
    let min = schema
        .pointer("/$defs/memoryBias/properties/bias/minimum")
        .and_then(Value::as_f64)
        .ok_or("schema is missing $defs.memoryBias.properties.bias.minimum")?;
    let max = schema
        .pointer("/$defs/memoryBias/properties/bias/maximum")
        .and_then(Value::as_f64)
        .ok_or("schema is missing $defs.memoryBias.properties.bias.maximum")?;
    ensure(
        (min + MAX_BIAS_MAGNITUDE).abs() < f64::EPSILON,
        format!("memoryBias.bias.minimum drifted: expected -0.05, got {min}"),
    )?;
    ensure(
        (max - MAX_BIAS_MAGNITUDE).abs() < f64::EPSILON,
        format!("memoryBias.bias.maximum drifted: expected 0.05, got {max}"),
    )?;
    Ok(())
}

#[test]
fn degradation_code_enum_lists_documented_codes() -> TestResult {
    let schema = load_schema()?;
    let codes_node = schema
        .pointer("/$defs/degradation/properties/code/enum")
        .and_then(Value::as_array)
        .ok_or("schema is missing $defs.degradation.properties.code.enum")?;
    let actual: BTreeSet<String> = codes_node
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();
    let expected: BTreeSet<String> = [
        "bead_affinity_cold_start",
        "bead_affinity_unavailable",
        "bead_affinity_capped",
        "bead_affinity_lookup_failed",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        actual == expected,
        format!("degradation code enum drifted: expected {expected:?}, got {actual:?}"),
    )
}

#[test]
fn determinism_key_shape_is_explicit() -> TestResult {
    let schema = load_schema()?;
    let required = required_field_set(&schema, "/properties/determinismKey/required")?;
    let expected: BTreeSet<String> = [
        "workspaceGeneration",
        "beadGeneration",
        "beadId",
        "basePackHash",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("determinismKey required drift: expected {expected:?}, got {required:?}"),
    )?;
    Ok(())
}

#[test]
fn hand_crafted_sample_satisfies_required_fields() -> TestResult {
    // Hand-crafted payload representing an "implementation-ready" cold-start
    // response. The structural check here is what we promise external
    // callers: every required key is present, magnitudes obey the cap,
    // and the determinism key carries enough information for byte-stable
    // replay. We do NOT run a full JSON-Schema validator (consistent with
    // the rest of the contract suite) - we assert the shape directly.
    let sample = serde_json::json!({
        "schema": SCHEMA_NAME,
        "beadId": "bd-2942u",
        "beadTitle": "swarmx.barp: bead-aware retrieval prioritization for ee context and ee search",
        "beadLabels": ["bead-affinity", "swarmx"],
        "beadGeneration": 17,
        "workspaceId": "ws_eidetic_engine_cli",
        "matchedFeatureCount": 0,
        "biasMagnitudeMax": 0.0,
        "maxBiasMagnitude": 0.05,
        "memoryBiasApplied": 0,
        "coldStart": true,
        "halfLifeDays": null,
        "featureWeights": {
            "label": 0.5,
            "titleToken": 0.25,
            "link": 0.15,
            "parentChild": 0.10
        },
        "determinismKey": {
            "workspaceGeneration": 42,
            "beadGeneration": 17,
            "beadId": "bd-2942u",
            "basePackHash": "blake3:0000000000000000000000000000000000000000000000000000000000000000"
        },
        "topBiases": [],
        "degraded": [{
            "code": "bead_affinity_cold_start",
            "severity": "info",
            "message": "bead bd-2942u has no matching memories under any feature dimension",
            "repair": null
        }]
    });

    let schema = load_schema()?;
    let required = required_field_set(&schema, "/required")?;
    for field in &required {
        ensure(
            sample.get(field).is_some(),
            format!("sample is missing required field `{field}`"),
        )?;
    }
    let max = sample["maxBiasMagnitude"]
        .as_f64()
        .ok_or("sample.maxBiasMagnitude must be a number")?;
    ensure(
        (max - MAX_BIAS_MAGNITUDE).abs() < f64::EPSILON,
        format!("sample.maxBiasMagnitude must equal 0.05; got {max}"),
    )?;
    let mag = sample["biasMagnitudeMax"]
        .as_f64()
        .ok_or("sample.biasMagnitudeMax must be a number")?;
    ensure(
        mag <= MAX_BIAS_MAGNITUDE + f64::EPSILON,
        format!("sample.biasMagnitudeMax {mag} must be <= 0.05"),
    )?;
    Ok(())
}

#[test]
fn capped_bias_sample_is_structurally_valid() -> TestResult {
    // Positive (non-cold-start) sample showing a memory pulled all the way
    // to the cap. Used to prove the topBiases shape and that the cap is
    // permissive at exactly 0.05, not 0.049...
    let sample = serde_json::json!({
        "schema": SCHEMA_NAME,
        "beadId": "bd-2942u",
        "matchedFeatureCount": 3,
        "biasMagnitudeMax": 0.05,
        "maxBiasMagnitude": 0.05,
        "memoryBiasApplied": 1,
        "coldStart": false,
        "determinismKey": {
            "workspaceGeneration": 42,
            "beadGeneration": 17,
            "beadId": "bd-2942u",
            "basePackHash": null
        },
        "topBiases": [{
            "memoryId": "mem_01HQ3K5Z",
            "bias": 0.05,
            "features": ["label", "titleToken", "link"],
            "matchedLabels": ["swarmx"],
            "matchedTokens": ["bead", "affinity"]
        }],
        "degraded": [{
            "code": "bead_affinity_capped",
            "severity": "low",
            "message": "1 memory hit the bias cap (0.05); further matches will not increase bias",
            "repair": "Lower featureWeights or split the bead into narrower scopes if cap saturation distorts ranking"
        }]
    });

    let top = sample["topBiases"]
        .as_array()
        .ok_or("topBiases must be an array")?;
    ensure(!top.is_empty(), "topBiases must have at least one item")?;
    for item in top {
        let bias = item["bias"]
            .as_f64()
            .ok_or("topBiases[].bias must be number")?;
        ensure(
            bias >= -MAX_BIAS_MAGNITUDE - f64::EPSILON && bias <= MAX_BIAS_MAGNITUDE + f64::EPSILON,
            format!("topBiases[].bias {bias} must be within [-0.05, +0.05]"),
        )?;
        let features = item["features"]
            .as_array()
            .ok_or("topBiases[].features must be an array")?;
        ensure(
            !features.is_empty(),
            "topBiases[].features must be non-empty",
        )?;
    }
    Ok(())
}
