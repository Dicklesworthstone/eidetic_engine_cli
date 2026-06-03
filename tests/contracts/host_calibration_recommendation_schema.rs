//! bd-1zb7k.12.3.5: schema-drift contract for the H3 calibrated
//! resource-profile recommender.
//!
//! The recommender (src/core/profile.rs) ships two response envelopes:
//!
//! - `ee.host_calibration.host_class.v1` — pure host-class
//!   classification derived from a caller-provided host profile probe
//!   and explicit calibration freshness evidence.
//! - `ee.host_calibration.recommendation.v1` — explainable budget
//!   delta recommendation for context, cache, graph, index, and burst
//!   admission, with topology warnings and degraded signals.
//!
//! Both envelopes are read-only (`sideEffectFree: true`); the
//! recommendation envelope additionally promises no config mutation
//! (`configMutation: false` is required so the H4 status/doctor/
//! support-bundle integration can pin no-mutation expectations).
//!
//! This contract pins the invariants H4 and H5 will compose on:
//!
//! 1. Envelope shape (`$id`, `title`, `properties.schema.const`, the
//!    top-level `required` set, and the `sideEffectFree` const).
//! 2. `hostClass` enum exactly matches the 6 documented values —
//!    constrained / portable / laptop / workstation / local_256gb /
//!    rch_only_topology. Additions break consumers that switch over
//!    the closed set; removals break the recommender.
//! 3. `calibrationFreshness` enum is exactly fresh/stale/partial/
//!    synthetic_only/contradictory/missing/unavailable. Confidence is
//!    exactly low/medium/high.
//! 4. The `reasonCode` taxonomy covers each input dimension
//!    (cpu, memory, disk, target_dir, rch_topology, calibration,
//!    synthetic_fixture) so the human-facing repair-action rendering
//!    can rely on at least one reason per dimension.
//! 5. The recommendation envelope requires `budgetDeltas`,
//!    `recommendedProfile`, `topologyWarnings`, and `degraded` so
//!    H4 can render explainable deltas and surface fleet posture.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const HOST_CLASS_SCHEMA_PATH: &str = "docs/schemas/ee.host_calibration.host_class.v1.json";
const RECOMMENDATION_SCHEMA_PATH: &str = "docs/schemas/ee.host_calibration.recommendation.v1.json";
const HOST_CLASS_SCHEMA_ID: &str =
    "https://eidetic-engine/schemas/ee.host_calibration.host_class.v1.json";
const RECOMMENDATION_SCHEMA_ID: &str =
    "https://eidetic-engine/schemas/ee.host_calibration.recommendation.v1.json";
const HOST_CLASS_SCHEMA_NAME: &str = "ee.host_calibration.host_class.v1";
const RECOMMENDATION_SCHEMA_NAME: &str = "ee.host_calibration.recommendation.v1";

const REQUIRED_HOST_CLASSES: &[&str] = &[
    "constrained",
    "portable",
    "laptop",
    "workstation",
    "local_256gb",
    "rch_only_topology",
];
const REQUIRED_CALIBRATION_FRESHNESS: &[&str] = &[
    "fresh",
    "stale",
    "partial",
    "synthetic_only",
    "contradictory",
    "missing",
    "unavailable",
];
const REQUIRED_CONFIDENCE: &[&str] = &["low", "medium", "high"];
const REQUIRED_REASON_CODE_PREFIXES: &[&str] = &[
    "cpu_logical_cores_",
    "memory_available_",
    "disk_capacity_",
    "target_dir_",
    "rch_topology_",
    "calibration_",
    "synthetic_fixture_",
];
const REQUIRED_HOST_CLASS_TOP_LEVEL: &[&str] = &[
    "schema",
    "sideEffectFree",
    "hostClass",
    "profileCeiling",
    "confidence",
    "calibrationFreshness",
    "reasonCodes",
    "repairActions",
    "degraded",
];
const REQUIRED_RECOMMENDATION_TOP_LEVEL: &[&str] = &[
    "schema",
    "sideEffectFree",
    "configMutation",
    "hostProfileSchema",
    "configuredProfile",
    "recommendedProfile",
    "effectiveProfile",
    "confidence",
    "budgetDeltas",
    "reasonCodes",
    "calibrationFreshness",
    "topologyWarnings",
    "degraded",
];

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

fn load_schema(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn collect_strings(node: &Value, ctx: &str) -> Result<Vec<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got: {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry: {value}"))
        })
        .collect()
}

fn collect_string_set(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    Ok(collect_strings(node, ctx)?.into_iter().collect())
}

#[test]
fn host_class_schema_has_expected_envelope_and_side_effect_free_const() -> TestResult {
    let schema = load_schema(HOST_CLASS_SCHEMA_PATH)?;
    ensure(
        schema["$id"] == HOST_CLASS_SCHEMA_ID,
        format!(
            "expected `$id` = {HOST_CLASS_SCHEMA_ID}; got: {}",
            schema["$id"]
        ),
    )?;
    ensure(
        schema["title"] == HOST_CLASS_SCHEMA_NAME,
        format!(
            "expected `title` = {HOST_CLASS_SCHEMA_NAME}; got: {}",
            schema["title"]
        ),
    )?;
    let schema_const = &schema["properties"]["schema"]["const"];
    ensure(
        schema_const == HOST_CLASS_SCHEMA_NAME,
        format!("expected properties.schema.const = {HOST_CLASS_SCHEMA_NAME}; got: {schema_const}"),
    )?;
    let side_effect_const = &schema["properties"]["sideEffectFree"]["const"];
    ensure(
        side_effect_const == &Value::Bool(true),
        format!(
            "host_class schema must declare sideEffectFree const true; got: {side_effect_const}"
        ),
    )?;
    let actual = collect_string_set(&schema["required"], "host_class.required")?;
    let expected = REQUIRED_HOST_CLASS_TOP_LEVEL
        .iter()
        .map(|field| (*field).to_owned())
        .collect::<BTreeSet<_>>();
    ensure(
        actual == expected,
        format!(
            "REQUIRED_HOST_CLASS_TOP_LEVEL drifted from schema required array\nexpected={expected:?}\nactual={actual:?}"
        ),
    )?;
    Ok(())
}

#[test]
fn host_class_enum_matches_documented_six_classes_exactly() -> TestResult {
    let schema = load_schema(HOST_CLASS_SCHEMA_PATH)?;
    let values = collect_strings(&schema["properties"]["hostClass"]["enum"], "hostClass.enum")?;
    ensure(
        values.len() == REQUIRED_HOST_CLASSES.len()
            && REQUIRED_HOST_CLASSES
                .iter()
                .all(|expected| values.iter().any(|v| v == expected)),
        format!(
            "hostClass enum must match exactly {REQUIRED_HOST_CLASSES:?}; got: {values:?}. \
             Additions break consumers that exhaustively switch on the closed \
             set; removals break the recommender."
        ),
    )?;
    Ok(())
}

#[test]
fn host_class_confidence_and_freshness_enums_are_pinned() -> TestResult {
    let schema = load_schema(HOST_CLASS_SCHEMA_PATH)?;
    let confidence = collect_strings(
        &schema["properties"]["confidence"]["enum"],
        "confidence.enum",
    )?;
    ensure(
        confidence.len() == REQUIRED_CONFIDENCE.len()
            && REQUIRED_CONFIDENCE
                .iter()
                .all(|c| confidence.iter().any(|v| v == c)),
        format!("confidence enum must be {REQUIRED_CONFIDENCE:?}; got: {confidence:?}"),
    )?;
    let freshness = collect_strings(
        &schema["properties"]["calibrationFreshness"]["enum"],
        "calibrationFreshness.enum",
    )?;
    ensure(
        freshness.len() == REQUIRED_CALIBRATION_FRESHNESS.len()
            && REQUIRED_CALIBRATION_FRESHNESS
                .iter()
                .all(|c| freshness.iter().any(|v| v == c)),
        format!(
            "calibrationFreshness enum must be {REQUIRED_CALIBRATION_FRESHNESS:?}; got: {freshness:?}"
        ),
    )?;
    Ok(())
}

#[test]
fn host_class_reason_codes_cover_each_input_dimension() -> TestResult {
    let schema = load_schema(HOST_CLASS_SCHEMA_PATH)?;
    let codes = collect_strings(&schema["$defs"]["reasonCode"]["enum"], "reasonCode.enum")?;
    for prefix in REQUIRED_REASON_CODE_PREFIXES {
        ensure(
            codes.iter().any(|c| c.starts_with(prefix)),
            format!(
                "reasonCode taxonomy is missing the `{prefix}*` family; got: {codes:?}. \
                 The H3 recommender requires at least one reason per input dimension \
                 (cpu / memory / disk / target_dir / rch_topology / calibration / synthetic_fixture)."
            ),
        )?;
    }
    Ok(())
}

#[test]
fn recommendation_schema_has_expected_envelope_and_read_only_consts() -> TestResult {
    let schema = load_schema(RECOMMENDATION_SCHEMA_PATH)?;
    ensure(
        schema["$id"] == RECOMMENDATION_SCHEMA_ID,
        format!(
            "expected `$id` = {RECOMMENDATION_SCHEMA_ID}; got: {}",
            schema["$id"]
        ),
    )?;
    ensure(
        schema["title"] == RECOMMENDATION_SCHEMA_NAME,
        format!(
            "expected `title` = {RECOMMENDATION_SCHEMA_NAME}; got: {}",
            schema["title"]
        ),
    )?;
    let schema_const = &schema["properties"]["schema"]["const"];
    ensure(
        schema_const == RECOMMENDATION_SCHEMA_NAME,
        format!(
            "expected properties.schema.const = {RECOMMENDATION_SCHEMA_NAME}; got: {schema_const}"
        ),
    )?;
    let side_effect_const = &schema["properties"]["sideEffectFree"]["const"];
    ensure(
        side_effect_const == &Value::Bool(true),
        format!(
            "recommendation schema must declare sideEffectFree const true; got: {side_effect_const}"
        ),
    )?;
    let config_mutation_const = &schema["properties"]["configMutation"]["const"];
    ensure(
        config_mutation_const == &Value::Bool(false),
        format!(
            "recommendation schema must declare configMutation const false; got: {config_mutation_const}. \
             H4 status/doctor/support-bundle integration depends on the recommender never mutating config."
        ),
    )?;
    let actual = collect_string_set(&schema["required"], "recommendation.required")?;
    let expected = REQUIRED_RECOMMENDATION_TOP_LEVEL
        .iter()
        .map(|field| (*field).to_owned())
        .collect::<BTreeSet<_>>();
    ensure(
        actual == expected,
        format!(
            "REQUIRED_RECOMMENDATION_TOP_LEVEL drifted from schema required array\nexpected={expected:?}\nactual={actual:?}"
        ),
    )
}
