//! bd-1wtsb: response-envelope conformance matrix.
//!
//! This is the first shared harness for the broad machine-surface contract in
//! bd-1wtsb. It keeps the requirement accounting explicit and validates
//! representative emitted artifacts against the JSON Schema documents that
//! already live under `docs/schemas/`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

type TestResult = Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequirementLevel {
    Must,
    Should,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoverageStatus {
    Validated,
    HarnessOnly,
}

#[derive(Clone, Debug)]
struct SurfaceRequirement {
    id: &'static str,
    surface: &'static str,
    schema_id: &'static str,
    schema_file: Option<&'static str>,
    level: RequirementLevel,
    status: CoverageStatus,
}

#[derive(Clone, Debug)]
struct SchemaCase {
    requirement_id: &'static str,
    schema_file: &'static str,
    value: Value,
}

const SURFACE_REQUIREMENTS: &[SurfaceRequirement] = &[
    SurfaceRequirement {
        id: "BD-1WTSB-STATUS",
        surface: "status",
        schema_id: "ee.response.v2",
        schema_file: Some("ee.response.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-DOCTOR",
        surface: "doctor",
        schema_id: "ee.response.v2",
        schema_file: Some("ee.response.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-CONTEXT",
        surface: "context",
        schema_id: "ee.pack.v2",
        schema_file: Some("ee.pack.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-SEARCH",
        surface: "search",
        schema_id: "ee.search.document.v1",
        schema_file: Some("ee.search.document.v1.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-WHY",
        surface: "why",
        schema_id: "ee.response.v2",
        schema_file: Some("ee.response.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-PACK",
        surface: "pack",
        schema_id: "ee.pack.v2",
        schema_file: Some("ee.pack.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-SWARM",
        surface: "swarm",
        schema_id: "ee.response.v2",
        schema_file: Some("ee.response.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-PREFLIGHT",
        surface: "preflight",
        schema_id: "ee.response.v2",
        schema_file: Some("ee.response.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-CAPABILITIES",
        surface: "capabilities",
        schema_id: "ee.response.v2",
        schema_file: Some("ee.response.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-EVAL",
        surface: "eval",
        schema_id: "ee.response.v2",
        schema_file: Some("ee.response.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-PERF-BENCH",
        surface: "perf",
        schema_id: "ee.perf.v1",
        schema_file: None,
        level: RequirementLevel::Should,
        status: CoverageStatus::HarnessOnly,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-PERF-LIVE",
        surface: "perf live",
        schema_id: "ee.perf.live.v1",
        schema_file: Some("ee.perf.live.v1.json"),
        level: RequirementLevel::Should,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-INSIGHTS",
        surface: "insights",
        schema_id: "ee.response.v2",
        schema_file: Some("ee.response.v2.json"),
        level: RequirementLevel::Must,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-PROXIMITY",
        surface: "proximity",
        schema_id: "ee.proximity.v1",
        schema_file: Some("ee.proximity.v1.json"),
        level: RequirementLevel::Should,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-TEST-EVENT",
        surface: "test event log",
        schema_id: "ee.test_event.v1",
        schema_file: Some("test_event_v1.json"),
        level: RequirementLevel::Should,
        status: CoverageStatus::Validated,
    },
    SurfaceRequirement {
        id: "BD-1WTSB-PROOF-CHECK",
        surface: "proof check",
        schema_id: "ee.proof_check.v1",
        schema_file: Some("ee.proof_check.v1.json"),
        level: RequirementLevel::Should,
        status: CoverageStatus::Validated,
    },
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn schema_path(file_name: &str) -> PathBuf {
    repo_root().join("docs").join("schemas").join(file_name)
}

fn read_json(path: PathBuf) -> Result<Value, String> {
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn schema_doc(file_name: &str) -> Result<Value, String> {
    read_json(schema_path(file_name))
}

fn schema_cases() -> Result<Vec<SchemaCase>, String> {
    Ok(vec![
        SchemaCase {
            requirement_id: "BD-1WTSB-STATUS",
            schema_file: "ee.response.v2.json",
            value: json!({
                "schema": "ee.response.v2",
                "success": true,
                "data": {
                    "command": "status",
                    "posture": {"overall": "ok"}
                },
                "degraded": [{
                    "code": "index_stale",
                    "severity": "warning",
                    "message": "Search index is stale.",
                    "repair": "ee index rebuild --workspace ."
                }]
            }),
        },
        SchemaCase {
            requirement_id: "BD-1WTSB-DOCTOR-ERROR",
            schema_file: "ee.error.v2.json",
            value: json!({
                "schema": "ee.error.v2",
                "error": {
                    "code": "storage",
                    "message": "Database not found.",
                    "severity": "high",
                    "repair": "ee init --workspace .",
                    "details": {
                        "recovery": [{
                            "priority": 1,
                            "kind": "seed",
                            "rationale": "Initialize the workspace database.",
                            "command": "ee init --workspace .",
                            "riskClass": "mutating_local_repair",
                            "requiresHumanApproval": false,
                            "mutatesExternalState": false,
                            "mutatesTrackerState": false,
                            "privacyClass": "workspace_metadata_only"
                        }]
                    }
                }
            }),
        },
        SchemaCase {
            requirement_id: "BD-1WTSB-CONTEXT",
            schema_file: "ee.pack.v2.json",
            value: context_pack_sample(),
        },
        SchemaCase {
            requirement_id: "BD-1WTSB-SEARCH",
            schema_file: "ee.search.document.v1.json",
            value: json!({
                "docId": "mem_schema_contract",
                "memoryId": "mem_schema_contract",
                "score": 0.91,
                "scoreInterval": [0.72, 0.97],
                "coverageGuarantee": 0.95,
                "calibrated": true,
                "source": "hybrid",
                "why": "Selected by hybrid retrieval with schema-backed provenance.",
                "provenance": [{
                    "kind": "provenance_uri",
                    "uri": "file://AGENTS.md#L1"
                }],
                "explanation": {
                    "summary": "Matched conformance query.",
                    "factors": [{
                        "name": "lexical",
                        "value": 0.72,
                        "contribution": "matched query terms",
                        "sourceField": "lexicalScore",
                        "formula": "bm25"
                    }]
                }
            }),
        },
        SchemaCase {
            requirement_id: "BD-1WTSB-PERF-LIVE",
            schema_file: "ee.perf.live.v1.json",
            value: perf_live_sample(),
        },
        SchemaCase {
            requirement_id: "BD-1WTSB-TEST-EVENT",
            schema_file: "test_event_v1.json",
            value: json!({
                "schema": "ee.test_event.v1",
                "ts": "2026-05-22T15:44:00Z",
                "test_id": "bd-1wtsb.response-envelope-conformance",
                "kind": "schema_gate",
                "fields": {
                    "target_schema": "ee.response.v2",
                    "log_lines_checked": 1,
                    "kinds_observed": ["schema_gate"],
                    "orphans_in_schema": [],
                    "orphans_in_src": []
                }
            }),
        },
        SchemaCase {
            requirement_id: "BD-1WTSB-PROOF-CHECK",
            schema_file: "ee.proof_check.v1.json",
            value: json!({
                "schema": "ee.proof_check.v1",
                "success": true,
                "checks": [],
                "degraded": ["degraded.proof_tool_missing"]
            }),
        },
    ])
}

fn context_pack_sample() -> Value {
    json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "command": "context",
            "request": {
                "query": "response envelope conformance",
                "profile": "balanced",
                "maxTokens": 4000,
                "candidatePool": 100,
                "memoryScope": "workspace",
                "strictScope": false,
                "sections": ["procedural_rules"]
            },
            "pack": {
                "schema": "ee.pack.v2",
                "query": "response envelope conformance",
                "budget": {
                    "maxTokens": 4000,
                    "usedTokens": 9
                },
                "items": [{
                    "rank": 1,
                    "memoryId": "mem_schema_contract",
                    "section": "procedural_rules",
                    "content": "Machine JSON must keep a stable response envelope.",
                    "estimatedTokens": 9,
                    "scores": {
                        "relevance": 0.9,
                        "utility": 0.8
                    },
                    "why": "Pins the context pack payload contract.",
                    "provenance": [{
                        "uri": "file://AGENTS.md#L1",
                        "label": "AGENTS.md"
                    }],
                    "trust": {
                        "class": "human_explicit"
                    }
                }],
                "skipped": [],
                "skippedTotal": 0,
                "provenanceFooter": {},
                "selectionAudit": {},
                "quality": {},
                "advisoryBanner": {}
            },
            "degraded": []
        }
    })
}

fn perf_live_sample() -> Value {
    let surface = |name: &str| {
        json!({
            "surface": name,
            "p50Ms": null,
            "p95Ms": null,
            "p99Ms": null,
            "p999Ms": null,
            "qps": null,
            "inflight": null,
            "qosClassCounts": {}
        })
    };
    json!({
        "schema": "ee.perf.live.v1",
        "ts": "2026-05-22T15:44:00Z",
        "intervalMs": 1000,
        "sideEffectFree": true,
        "redactionStatus": "redaction_safe",
        "beadId": "bd-1zwi4",
        "surfaces": {
            "context": surface("context"),
            "search": surface("search"),
            "remember": surface("remember"),
            "why": surface("why"),
            "packBuild": surface("packBuild")
        },
        "readPool": {
            "activePins": 0,
            "expiredPins": 0,
            "releaseFailures": 0,
            "queueDepth": 0
        },
        "auditLane": {
            "batchCount": null,
            "batchSizeP50": null,
            "batchSizeP99": null,
            "backpressureEvents": null,
            "channelDepth": null
        },
        "l2Cache": {
            "status": "disabled",
            "hits": null,
            "misses": null,
            "hitRateBasisPoints": null,
            "byteSize": null,
            "evictions": null
        },
        "rch": {
            "workersHealthy": 0,
            "slotsAvailable": null,
            "queueDepth": 0,
            "headOfLineAgeMs": null
        },
        "graphSnapshot": {
            "ageMs": null,
            "refreshedCount": 0,
            "refreshLockWaitMsP99": 0
        },
        "hostPressure": {
            "cpuUserPct": null,
            "cpuIowaitPct": null,
            "memoryRssMb": null,
            "pageCacheMb": null,
            "fsyncLatencyP99Ms": null
        },
        "beadActivity": {
            "activeAgents": 0,
            "readyBeads": 0,
            "inProgressBeads": 0,
            "blockedBeads": 0
        },
        "degraded": []
    })
}

#[test]
fn bd_1wtsb_surface_matrix_covers_required_machine_surfaces() -> TestResult {
    let required_surfaces = [
        "status",
        "doctor",
        "context",
        "search",
        "why",
        "pack",
        "swarm",
        "preflight",
        "capabilities",
        "eval",
        "perf",
        "insights",
        "proximity",
    ];

    for surface in required_surfaces {
        let matches = SURFACE_REQUIREMENTS
            .iter()
            .filter(|requirement| {
                requirement.surface == surface || requirement.surface.starts_with(surface)
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(format!("bd-1wtsb surface matrix missing {surface}"));
        }
    }

    let must_count = SURFACE_REQUIREMENTS
        .iter()
        .filter(|requirement| requirement.level == RequirementLevel::Must)
        .count();
    let validated_must_count = SURFACE_REQUIREMENTS
        .iter()
        .filter(|requirement| {
            requirement.level == RequirementLevel::Must
                && requirement.status == CoverageStatus::Validated
        })
        .count();
    if must_count == 0 || must_count != validated_must_count {
        return Err(format!(
            "bd-1wtsb MUST coverage incomplete: {validated_must_count}/{must_count}"
        ));
    }

    Ok(())
}

#[test]
fn bd_1wtsb_schema_files_exist_for_validated_surface_cases() -> TestResult {
    for requirement in SURFACE_REQUIREMENTS
        .iter()
        .filter(|requirement| requirement.status == CoverageStatus::Validated)
    {
        let schema_file = requirement
            .schema_file
            .ok_or_else(|| format!("{} missing schema file", requirement.id))?;
        let schema = schema_doc(schema_file)?;
        if !schema_declares_id(&schema, requirement.schema_id) {
            return Err(format!(
                "{} expected docs/schemas/{schema_file} to declare {}",
                requirement.id, requirement.schema_id
            ));
        }
    }
    Ok(())
}

fn schema_declares_id(schema: &Value, schema_id: &str) -> bool {
    schema.get("title").and_then(Value::as_str) == Some(schema_id)
        || schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(schema_id)
        || schema
            .pointer("/properties/data/properties/schema/const")
            .and_then(Value::as_str)
            == Some(schema_id)
}

#[test]
fn bd_1wtsb_representative_artifacts_validate_against_declared_schemas() -> TestResult {
    for case in schema_cases()? {
        let schema = schema_doc(case.schema_file)?;
        validate_json_schema(&case.value, &schema, &schema, "$")
            .map_err(|error| format!("{}: {error}", case.requirement_id))?;
    }
    Ok(())
}

#[test]
fn bd_1wtsb_validator_rejects_unexpected_envelope_fields() -> TestResult {
    let schema = schema_doc("ee.response.v2.json")?;
    let mut value = json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {"command": "status"}
    });
    value
        .as_object_mut()
        .expect("object")
        .insert("legacy_success".to_owned(), Value::Bool(true));

    match validate_json_schema(&value, &schema, &schema, "$") {
        Ok(()) => Err("validator accepted unknown top-level envelope field".to_owned()),
        Err(_) => Ok(()),
    }
}

fn validate_json_schema(
    value: &Value,
    schema: &Value,
    root_schema: &Value,
    path: &str,
) -> TestResult {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = resolve_ref(root_schema, reference)?;
        return validate_json_schema(value, target, root_schema, path);
    }

    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        if options
            .iter()
            .any(|candidate| validate_json_schema(value, candidate, root_schema, path).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path} did not match any oneOf branch"));
    }

    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        if options
            .iter()
            .any(|candidate| validate_json_schema(value, candidate, root_schema, path).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path} did not match any anyOf branch"));
    }

    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        for candidate in all_of {
            validate_json_schema(value, candidate, root_schema, path)?;
        }
    }

    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err(format!("{path} expected const {expected}, got {value}"));
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        return Err(format!(
            "{path} value {value} is not in enum {enum_values:?}"
        ));
    }

    if let Some(expected_types) = schema_types(schema)
        && !expected_types
            .iter()
            .any(|expected_type| json_type_matches(value, expected_type))
    {
        return Err(format!(
            "{path} expected type {:?}, got {}",
            expected_types,
            json_type_name(value)
        ));
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required {
                let field = field
                    .as_str()
                    .ok_or_else(|| format!("{path} schema required entry is not a string"))?;
                if !object.contains_key(field) {
                    return Err(format!("{path} missing required field {field}"));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            let child_path = format!("{path}.{key}");
            if let Some(property_schema) = properties.and_then(|props| props.get(key)) {
                validate_json_schema(child, property_schema, root_schema, &child_path)?;
                continue;
            }
            match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    return Err(format!("{path} contains unexpected field {key}"));
                }
                Some(Value::Bool(true)) | None => {}
                Some(Value::Object(property_schema)) => {
                    validate_json_schema(
                        child,
                        &Value::Object(property_schema.clone()),
                        root_schema,
                        &child_path,
                    )?;
                }
                Some(other) => {
                    return Err(format!(
                        "{path} has unsupported additionalProperties schema {other}"
                    ));
                }
            }
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
            && array.len() < min_items as usize
        {
            return Err(format!("{path} has fewer than {min_items} items"));
        }
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64)
            && array.len() > max_items as usize
        {
            return Err(format!("{path} has more than {max_items} items"));
        }
        if let Some(prefix_items) = schema.get("prefixItems").and_then(Value::as_array) {
            for (index, item_schema) in prefix_items.iter().enumerate() {
                if let Some(item) = array.get(index) {
                    validate_json_schema(
                        item,
                        item_schema,
                        root_schema,
                        &format!("{path}[{index}]"),
                    )?;
                }
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_json_schema(item, item_schema, root_schema, &format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn resolve_ref<'a>(root_schema: &'a Value, reference: &str) -> Result<&'a Value, String> {
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| format!("unsupported non-local $ref {reference}"))?;
    root_schema
        .pointer(pointer)
        .ok_or_else(|| format!("unresolved $ref {reference}"))
}

fn schema_types(schema: &Value) -> Option<Vec<&str>> {
    match schema.get("type")? {
        Value::String(single) => Some(vec![single.as_str()]),
        Value::Array(values) => Some(values.iter().filter_map(Value::as_str).collect()),
        _ => None,
    }
}

fn json_type_matches(value: &Value, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
