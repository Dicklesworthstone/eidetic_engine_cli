//! Contract coverage for Swarm SLO attribution event schemas.
//!
//! `tests/docs_schemas_match_responses.rs` already validates representative
//! adapter output against these schemas. This file pins the public schema
//! structure that scorecard/replay consumers rely on: exact required fields,
//! nested evidence/producer shapes, and enum parity with the Rust surface.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use ee::policy::{
    SWARM_SLO_COORDINATION_EVENT_SCHEMA_V1, SWARM_SLO_RESOURCE_USAGE_EVENT_SCHEMA_V1,
    SwarmSloAttributionBucket, SwarmSloPosture,
};
use serde_json::Value;

type TestResult = Result<(), String>;

const RESOURCE_SCHEMA_PATH: &str = "docs/schemas/ee.swarm_slo.resource_usage_event.v1.json";
const COORDINATION_SCHEMA_PATH: &str = "docs/schemas/ee.swarm_slo.coordination_event.v1.json";

const RESOURCE_REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "producer",
    "source",
    "stage",
    "bucket",
    "posture",
    "elapsedMs",
    "cpuMs",
    "memoryBytes",
    "ioReadBytes",
    "ioWriteBytes",
    "evidence",
];

const COORDINATION_REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "producer",
    "sourceKind",
    "bucket",
    "posture",
    "elapsedMs",
    "eventCount",
    "errorCount",
    "degradedCount",
    "repairCommand",
    "evidence",
];

const PRODUCER_REQUIRED_FIELDS: &[&str] = &[
    "kind",
    "attributionKey",
    "canonicalHash",
    "originalHash",
    "redacted",
];

const EVIDENCE_REQUIRED_FIELDS: &[&str] =
    &["field", "code", "valueHash", "redacted", "redactionReasons"];

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

fn load_schema(relative_path: &str) -> Result<Value, String> {
    let path = repo_root().join(relative_path);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn collect_string_set(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry {value}"))
        })
        .collect()
}

fn expected_string_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn require_exact_strings(
    schema: &Value,
    pointer: &str,
    expected: &[&str],
    label: &str,
) -> TestResult {
    let actual = collect_string_set(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let expected = expected_string_set(expected);
    ensure(
        actual == expected,
        format!("{label} drifted from exact set; expected {expected:?}, got {actual:?}"),
    )
}

fn require_schema_identity(schema: &Value, expected_schema: &str, label: &str) -> TestResult {
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(expected_schema),
        format!("{label} title must stay {expected_schema}"),
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(expected_schema),
        format!("{label} schema const must stay {expected_schema}"),
    )
}

fn require_example_fields(schema: &Value, required: &[&str], label: &str) -> TestResult {
    let example = schema
        .pointer("/examples/0")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{label}: schema must include an object example"))?;
    for field in required {
        ensure(
            example.contains_key(*field),
            format!("{label} example missing required field `{field}`"),
        )?;
    }
    Ok(())
}

fn require_no_raw_sensitive_example_values(schema: &Value, label: &str) -> TestResult {
    let example = schema
        .pointer("/examples/0")
        .ok_or_else(|| format!("{label}: schema must include an example"))?
        .to_string();
    for forbidden in ["PinkOriole", "api_key", "/tmp", "id_ed25519"] {
        ensure(
            !example.contains(forbidden),
            format!("{label} example leaks raw sensitive fixture value `{forbidden}`"),
        )?;
    }
    Ok(())
}

#[test]
fn swarm_slo_resource_usage_event_schema_required_fields_are_pinned() -> TestResult {
    let schema = load_schema(RESOURCE_SCHEMA_PATH)?;
    require_schema_identity(
        &schema,
        SWARM_SLO_RESOURCE_USAGE_EVENT_SCHEMA_V1,
        "resource usage event schema",
    )?;
    require_exact_strings(
        &schema,
        "/required",
        RESOURCE_REQUIRED_TOP_LEVEL,
        "resource usage event required fields",
    )?;
    require_exact_strings(
        &schema,
        "/$defs/producerAttribution/required",
        PRODUCER_REQUIRED_FIELDS,
        "resource usage event producer required fields",
    )?;
    require_exact_strings(
        &schema,
        "/$defs/redactedEvidence/required",
        EVIDENCE_REQUIRED_FIELDS,
        "resource usage event evidence required fields",
    )?;
    require_example_fields(
        &schema,
        RESOURCE_REQUIRED_TOP_LEVEL,
        "resource usage event schema",
    )?;
    require_no_raw_sensitive_example_values(&schema, "resource usage event schema")
}

#[test]
fn swarm_slo_coordination_event_schema_required_fields_are_pinned() -> TestResult {
    let schema = load_schema(COORDINATION_SCHEMA_PATH)?;
    require_schema_identity(
        &schema,
        SWARM_SLO_COORDINATION_EVENT_SCHEMA_V1,
        "coordination event schema",
    )?;
    require_exact_strings(
        &schema,
        "/required",
        COORDINATION_REQUIRED_TOP_LEVEL,
        "coordination event required fields",
    )?;
    require_exact_strings(
        &schema,
        "/$defs/producerAttribution/required",
        PRODUCER_REQUIRED_FIELDS,
        "coordination event producer required fields",
    )?;
    require_exact_strings(
        &schema,
        "/$defs/redactedEvidence/required",
        EVIDENCE_REQUIRED_FIELDS,
        "coordination event evidence required fields",
    )?;
    require_example_fields(
        &schema,
        COORDINATION_REQUIRED_TOP_LEVEL,
        "coordination event schema",
    )?;
    require_no_raw_sensitive_example_values(&schema, "coordination event schema")
}

#[test]
fn swarm_slo_event_schema_enums_match_rust_surfaces() -> TestResult {
    let resource_schema = load_schema(RESOURCE_SCHEMA_PATH)?;
    let coordination_schema = load_schema(COORDINATION_SCHEMA_PATH)?;
    let expected_buckets = [
        SwarmSloAttributionBucket::Storage.as_str(),
        SwarmSloAttributionBucket::Search.as_str(),
        SwarmSloAttributionBucket::Graph.as_str(),
        SwarmSloAttributionBucket::Pack.as_str(),
        SwarmSloAttributionBucket::Output.as_str(),
        SwarmSloAttributionBucket::Coordination.as_str(),
        SwarmSloAttributionBucket::Rch.as_str(),
        SwarmSloAttributionBucket::Tracker.as_str(),
        SwarmSloAttributionBucket::ExternalUnavailable.as_str(),
        SwarmSloAttributionBucket::UnknownResidual.as_str(),
    ];
    let expected_postures = [
        SwarmSloPosture::Ok.as_str(),
        SwarmSloPosture::Degraded.as_str(),
        SwarmSloPosture::Unavailable.as_str(),
        SwarmSloPosture::Blocked.as_str(),
        SwarmSloPosture::Unknown.as_str(),
    ];

    for (schema, label) in [
        (&resource_schema, "resource usage event schema"),
        (&coordination_schema, "coordination event schema"),
    ] {
        require_exact_strings(
            schema,
            "/$defs/attributionBucket/enum",
            &expected_buckets,
            &format!("{label} attribution bucket enum"),
        )?;
        require_exact_strings(
            schema,
            "/$defs/posture/enum",
            &expected_postures,
            &format!("{label} posture enum"),
        )?;
    }
    Ok(())
}
