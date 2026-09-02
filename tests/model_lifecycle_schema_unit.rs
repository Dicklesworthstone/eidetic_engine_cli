//! bd-1iupc.1 - schema unit test for `ee.model_lifecycle.v1` (ADR 0074).
//!
//! This pins the contract that the follow-on model lifecycle collector will
//! emit: closed lifecycle states, redaction-safe asset provenance, explicit
//! dimension compatibility rules, and the degraded-code vocabulary shared by
//! model registry/search/index readiness.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{Value, json};

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.model_lifecycle.v1.json";
const SCHEMA_NAME: &str = "ee.model_lifecycle.v1";
const REDACTION_CONST: &str = "paths_workspace_relative_or_hashed_no_content";
const LIFECYCLE_STATES: [&str; 10] = [
    "available",
    "cold",
    "warming",
    "missing",
    "corrupt",
    "dimension_mismatch",
    "stale_index_model",
    "lexical_fallback",
    "unsupported_feature",
    "unknown",
];

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

fn string_set(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let node = value
        .pointer(pointer)
        .ok_or_else(|| format!("schema is missing pointer {pointer}"))?;
    let array = node
        .as_array()
        .ok_or_else(|| format!("{pointer} must be a JSON array"))?;
    let mut out = BTreeSet::new();
    for entry in array {
        let name = entry
            .as_str()
            .ok_or_else(|| format!("{pointer} contains a non-string entry: {entry}"))?;
        out.insert(name.to_owned());
    }
    Ok(out)
}

#[test]
fn schema_identity_and_status_marker() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_NAME),
        "schema title must be the schema name",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with(&format!("{SCHEMA_NAME}.json"))),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_NAME),
        "schema.const must pin the schema name",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/shipped")
            .and_then(Value::as_bool)
            == Some(true),
        "x-ee-status.shipped must be true once bd-1iupc.2 lands collectors",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/available_in_build")
            .and_then(Value::as_bool)
            == Some(true),
        "x-ee-status.available_in_build must be true once the collector is emitted",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/tracking_bead")
            .and_then(Value::as_str)
            == Some("bd-1iupc.2"),
        "x-ee-status.tracking_bead must point at the collector bead",
    )
}

#[test]
fn top_level_required_fields_match_adr() -> TestResult {
    let schema = load_schema()?;
    let actual = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "generatedAt",
        "workspaceFingerprint",
        "redactionStatus",
        "semanticReadiness",
        "models",
        "indexes",
        "degraded",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        actual == expected,
        format!("top-level required set drifted: {actual:?}"),
    )
}

#[test]
fn lifecycle_state_enum_is_closed() -> TestResult {
    let schema = load_schema()?;
    let actual = string_set(&schema, "/$defs/lifecycleState/enum")?;
    let expected: BTreeSet<String> = LIFECYCLE_STATES.iter().map(|s| (*s).to_owned()).collect();
    ensure(
        actual == expected,
        format!("lifecycle state vocabulary drifted: {actual:?}"),
    )
}

#[test]
fn asset_provenance_is_redaction_safe_and_complete() -> TestResult {
    let schema = load_schema()?;
    let actual = string_set(&schema, "/$defs/assetProvenance/required")?;
    let expected: BTreeSet<String> = [
        "sourceKind",
        "sourceUri",
        "registryEntryId",
        "modelRevision",
        "contentHash",
        "assetHash",
        "manifestHash",
        "checkedAt",
        "provenanceComplete",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        actual == expected,
        format!("asset provenance required set drifted: {actual:?}"),
    )?;
    ensure(
        schema
            .pointer("/properties/redactionStatus/const")
            .and_then(Value::as_str)
            == Some(REDACTION_CONST),
        "redactionStatus const drifted",
    )?;
    ensure(
        schema
            .pointer("/$defs/redactedPath/not/pattern")
            .and_then(Value::as_str)
            == Some("^/"),
        "redactedPath must forbid absolute paths",
    )
}

#[test]
fn dimension_compatibility_fields_and_rules_are_pinned() -> TestResult {
    let schema = load_schema()?;
    let actual = string_set(&schema, "/$defs/dimensionCompatibility/required")?;
    let expected: BTreeSet<String> = [
        "expectedDimension",
        "actualDimension",
        "indexDimension",
        "distanceMetric",
        "vectorDtype",
        "compatible",
        "rule",
        "mismatchReason",
        "repair",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        actual == expected,
        format!("dimension compatibility required set drifted: {actual:?}"),
    )?;
    let rules = string_set(
        &schema,
        "/$defs/dimensionCompatibility/properties/rule/enum",
    )?;
    let expected_rules: BTreeSet<String> = [
        "exact_dimension_metric_dtype",
        "lexical_no_dimension",
        "unsupported_feature",
        "not_probed",
        "unknown",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        rules == expected_rules,
        format!("dimension compatibility rules drifted: {rules:?}"),
    )
}

#[test]
fn degraded_vocabulary_covers_lifecycle_and_existing_readiness_codes() -> TestResult {
    let schema = load_schema()?;
    let actual = string_set(&schema, "/$defs/degradedEntry/properties/code/enum")?;
    let required = [
        "model_lifecycle_cold",
        "model_lifecycle_warming",
        "model_asset_missing",
        "model_asset_corrupt",
        "model_dimension_mismatch",
        "stale_index_model",
        "lexical_fallback",
        "unsupported_feature",
        "model_lifecycle_unknown",
        "model_registry_empty",
        "model_registry_no_available_entry",
        "semantic_model_unavailable",
        "semantic_dimension_exceeds_budget",
        "index_missing",
        "index_corrupt",
        "index_stale",
        "search_index_degraded",
    ];
    for code in required {
        ensure(
            actual.contains(code),
            format!("degraded code enum is missing {code}"),
        )?;
    }
    Ok(())
}

fn lexical_fallback_sample() -> Value {
    json!({
        "schema": SCHEMA_NAME,
        "generatedAt": "2026-06-14T00:00:00Z",
        "workspaceFingerprint": "0123456789ab",
        "redactionStatus": REDACTION_CONST,
        "semanticReadiness": {
            "state": "lexical_fallback",
            "mode": "lexical_fallback",
            "selectedModelId": null,
            "selectedIndexId": "search-main",
            "dimensionCompatibility": {
                "expectedDimension": null,
                "actualDimension": null,
                "indexDimension": null,
                "distanceMetric": null,
                "vectorDtype": null,
                "compatible": null,
                "rule": "lexical_no_dimension",
                "mismatchReason": "no available semantic embedding model",
                "repair": "install or enable a local Frankensearch embedding model, then rebuild the semantic index"
            },
            "degraded": [{
                "code": "lexical_fallback",
                "severity": "warning",
                "message": "Semantic retrieval is unavailable; lexical retrieval remains available.",
                "repair": "install or enable a local Frankensearch embedding model"
            }]
        },
        "models": [{
            "modelId": "frankensearch-hash-fallback",
            "provider": "hash",
            "purpose": "embedding",
            "registryStatus": "unknown",
            "state": "lexical_fallback",
            "assetProvenance": {
                "sourceKind": "hash_fallback",
                "sourceUri": null,
                "registryEntryId": null,
                "modelRevision": null,
                "contentHash": null,
                "assetHash": null,
                "manifestHash": null,
                "checkedAt": "2026-06-14T00:00:00Z",
                "provenanceComplete": false
            },
            "embeddingMetadata": null,
            "dimensionCompatibility": {
                "expectedDimension": null,
                "actualDimension": null,
                "indexDimension": null,
                "distanceMetric": null,
                "vectorDtype": null,
                "compatible": null,
                "rule": "lexical_no_dimension",
                "mismatchReason": "hash fallback is not a semantic model",
                "repair": "enable a semantic embedding model before semantic indexing"
            },
            "degraded": [{
                "code": "lexical_fallback",
                "severity": "warning",
                "message": "Hash fallback can keep lexical search honest but cannot prove semantic readiness.",
                "repair": null
            }]
        }],
        "indexes": [{
            "indexId": "search-main",
            "kind": "lexical",
            "state": "lexical_fallback",
            "storedModelId": null,
            "storedModelRevision": null,
            "storedModelHash": null,
            "storedDimension": null,
            "storedDistanceMetric": null,
            "storedVectorDtype": null,
            "lastRebuildAt": null,
            "derivedFrom": ["hashed:0123456789ab"],
            "dimensionCompatibility": {
                "expectedDimension": null,
                "actualDimension": null,
                "indexDimension": null,
                "distanceMetric": null,
                "vectorDtype": null,
                "compatible": null,
                "rule": "lexical_no_dimension",
                "mismatchReason": "lexical index has no semantic vector dimension",
                "repair": null
            },
            "degraded": [{
                "code": "lexical_fallback",
                "severity": "warning",
                "message": "Only lexical index metadata is available.",
                "repair": null
            }]
        }],
        "degraded": [{
            "code": "model_registry_empty",
            "severity": "warning",
            "message": "No available semantic model registry row was found.",
            "repair": "record or enable a local embedding model before semantic rebuild"
        }]
    })
}

#[test]
fn lexical_fallback_sample_uses_only_pinned_vocabulary() -> TestResult {
    let schema = load_schema()?;
    let sample = lexical_fallback_sample();
    ensure(
        sample.pointer("/schema").and_then(Value::as_str) == Some(SCHEMA_NAME),
        "sample schema name drifted",
    )?;
    ensure(
        sample.pointer("/redactionStatus").and_then(Value::as_str) == Some(REDACTION_CONST),
        "sample redactionStatus drifted",
    )?;

    let states = string_set(&schema, "/$defs/lifecycleState/enum")?;
    ensure(
        sample
            .pointer("/semanticReadiness/state")
            .and_then(Value::as_str)
            .is_some_and(|state| states.contains(state)),
        "readiness state is not in the lifecycle enum",
    )?;
    ensure(
        sample
            .pointer("/models/0/state")
            .and_then(Value::as_str)
            .is_some_and(|state| states.contains(state)),
        "model row state is not in the lifecycle enum",
    )?;
    ensure(
        sample
            .pointer("/indexes/0/state")
            .and_then(Value::as_str)
            .is_some_and(|state| states.contains(state)),
        "index row state is not in the lifecycle enum",
    )?;

    let degraded_codes = string_set(&schema, "/$defs/degradedEntry/properties/code/enum")?;
    for pointer in [
        "/semanticReadiness/degraded/0/code",
        "/models/0/degraded/0/code",
        "/indexes/0/degraded/0/code",
        "/degraded/0/code",
    ] {
        let code = sample
            .pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("sample is missing {pointer}"))?;
        ensure(
            degraded_codes.contains(code),
            format!("sample degraded code {code} is not in schema enum"),
        )?;
    }

    for pointer in [
        "/models/0/assetProvenance/sourceUri",
        "/indexes/0/derivedFrom/0",
    ] {
        if let Some(path) = sample.pointer(pointer).and_then(Value::as_str) {
            ensure(
                !path.starts_with('/'),
                format!("sample path at {pointer} leaks an absolute path"),
            )?;
        }
    }
    Ok(())
}
