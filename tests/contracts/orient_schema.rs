//! bd-orient-store-discovery-ft1z5: public `ee.orient.v1` contract wiring and
//! representative instance validation.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use ee::output::{public_schemas, render_schema_export_json};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const SCHEMA_ID: &str = ee::models::ORIENT_SCHEMA_V1;
const SCHEMA_REL: &str = "docs/schemas/ee.orient.v1.json";

fn load_schema() -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_REL);
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

#[test]
fn orient_schema_identity_registry_inventory_and_golden_are_pinned() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "schema title must equal the canonical model schema id",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_ID),
        "properties.schema.const must pin the canonical model schema id",
    )?;
    ensure(
        ee::models::KNOWN_SCHEMAS
            .iter()
            .filter(|schema| **schema == SCHEMA_ID)
            .count()
            == 1,
        "ee.orient.v1 must occur exactly once in KNOWN_SCHEMAS",
    )?;

    let entries = public_schemas()
        .iter()
        .filter(|entry| entry.id == SCHEMA_ID)
        .collect::<Vec<_>>();
    ensure(
        entries.len() == 1,
        "public schema registry must contain ee.orient.v1 exactly once",
    )?;
    ensure(entries[0].version == "1", "registry version must be 1")?;
    ensure(
        entries[0].category == "context",
        "registry category must be context",
    )?;
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some(SCHEMA_ID)))
        .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported == schema,
        "registry export must equal the schema file",
    )?;

    let inventory: Value = serde_json::from_str(include_str!(
        "../fixtures/contracts/public_contract_inventory.json"
    ))
    .map_err(|error| format!("contract inventory did not parse: {error}"))?;
    let inventory_entries = inventory
        .pointer("/contracts")
        .and_then(Value::as_array)
        .ok_or("contract inventory is missing contracts")?
        .iter()
        .filter(|entry| entry.get("schemaId").and_then(Value::as_str) == Some(SCHEMA_ID))
        .collect::<Vec<_>>();
    ensure(
        inventory_entries.len() == 1,
        "contract inventory must contain ee.orient.v1 exactly once",
    )?;
    ensure(
        inventory_entries[0].get("status").and_then(Value::as_str) == Some("current")
            && inventory_entries[0]
                .get("schemaFile")
                .and_then(Value::as_str)
                == Some(SCHEMA_REL),
        "contract inventory must identify the current orient schema file",
    )?;

    let schema_list: Value = serde_json::from_str(include_str!(
        "../fixtures/golden/schema/schema_list_json.golden"
    ))
    .map_err(|error| format!("schema-list golden did not parse: {error}"))?;
    let golden_entries = schema_list
        .pointer("/data/schemas")
        .and_then(Value::as_array)
        .ok_or("schema-list golden is missing data.schemas")?
        .iter()
        .filter(|entry| entry.get("id").and_then(Value::as_str) == Some(SCHEMA_ID))
        .count();
    ensure(
        golden_entries == 1,
        "schema-list golden must contain ee.orient.v1 exactly once",
    )
}

#[test]
fn orient_fast_and_full_instances_validate_and_unknown_fields_fail() -> TestResult {
    let schema = load_schema()?;
    let fast = json!({
        "schema": SCHEMA_ID,
        "command": "orient",
        "mode": "fast",
        "embed_backend": "deterministic_hash",
        "version": "0.2.0",
        "workspace": "/repo",
        "task": "resume work",
        "sideEffectFree": true,
        "configMutation": "never",
        "swarmBrief": {},
        "doctor": {},
        "install": {},
        "workspaceHygiene": {},
        "pack": null,
        "fastContent": {
            "schema": "ee.orient.fast_content.v1",
            "posture": "ready",
            "strategy": {
                "recent": "context_admitted_recency_v1",
                "relevant": "direct_lexical_admitted_v1",
                "sectionOverlap": "preserved",
                "recentLimit": 5,
                "relevantLimit": 5
            },
            "recent": [{
                "id": "mem_01",
                "snippet": "Use the adjacent populated store.",
                "createdAt": "2026-08-12T12:00:00Z",
                "tags": ["orientation"],
                "provenance": [{
                    "uri": "memory://mem_01",
                    "scheme": "memory",
                    "label": "mem_01",
                    "locator": null,
                    "note": "explicit memory"
                }]
            }],
            "relevant": [{
                "id": "mem_02",
                "snippet": "Retain the selected store identity.",
                "createdAt": "2026-08-12T11:00:00Z",
                "tags": ["orientation", "store-identity"],
                "provenance": [{
                    "uri": "memory://mem_02",
                    "scheme": "memory",
                    "label": "mem_02",
                    "locator": null,
                    "note": "explicit relevant memory"
                }]
            }],
            "issues": [{
                "component": "relevant",
                "status": "degraded",
                "code": "context_evidence_freshness_missing_source",
                "severity": "low",
                "message": "Evidence source is missing.",
                "repair": null
            }]
        },
        "primer": null,
        "decisions": {},
        "learnGaps": {},
        "revivals": {},
        "nextCommands": ["ee pack --workspace /repo -- resume work"],
        "storeDiscovery": {
            "addressedStorePath": "/repo/.ee/ee.db",
            "addressedState": "thin",
            "addressedDocuments": 1,
            "thinStoreThreshold": 3,
            "storeEmpty": false,
            "outcome": "complete",
            "nearbyStores": [{
                "workspaceRoot": "/repo/child",
                "storeDir": "/repo/child/.ee",
                "documents": 3,
                "lastWrite": "2026-08-12T12:00:00Z",
                "provenance": "child_scan"
            }]
        }
    });
    ee::testing::validate_json_schema_instance(&fast, &schema)?;

    let full = json!({
        "schema": SCHEMA_ID,
        "command": "orient",
        "mode": "full",
        "embed_backend": "neural_local",
        "version": "0.2.0",
        "workspace": "/repo",
        "task": "prepare release",
        "sideEffectFree": true,
        "configMutation": "never",
        "swarmBrief": {},
        "doctor": {},
        "install": {},
        "workspaceHygiene": {},
        "pack": {},
        "fastContent": null,
        "primer": null,
        "decisions": {},
        "learnGaps": {},
        "revivals": {},
        "nextCommands": []
    });
    ee::testing::validate_json_schema_instance(&full, &schema)?;

    let mut root_unknown = fast.clone();
    root_unknown
        .as_object_mut()
        .ok_or("fast fixture must be an object")?
        .insert("uncontractedField".to_owned(), Value::Bool(true));
    ensure(
        ee::testing::validate_json_schema_instance(&root_unknown, &schema).is_err(),
        "strict orient schema must reject an uncontracted root field",
    )?;

    let fast_content = fast
        .pointer("/fastContent")
        .cloned()
        .ok_or("fast fixture is missing fastContent")?;
    let mut invalid_mode = fast.clone();
    invalid_mode["mode"] = json!("turbo");
    let mut fast_without_content = fast.clone();
    fast_without_content["fastContent"] = Value::Null;
    let mut fast_with_pack = fast.clone();
    fast_with_pack["pack"] = json!({});
    let mut full_without_pack = full.clone();
    full_without_pack["pack"] = Value::Null;
    let mut full_with_fast_content = full;
    full_with_fast_content["fastContent"] = fast_content;
    for (case, invalid) in [
        ("unknown mode", invalid_mode),
        ("fast mode with null fastContent", fast_without_content),
        ("fast mode with object pack", fast_with_pack),
        ("full mode with null pack", full_without_pack),
        ("full mode with object fastContent", full_with_fast_content),
    ] {
        ensure(
            ee::testing::validate_json_schema_instance(&invalid, &schema).is_err(),
            format!("orient schema must reject {case}"),
        )?;
    }

    for pointer in [
        "/fastContent",
        "/fastContent/strategy",
        "/fastContent/recent/0",
        "/fastContent/recent/0/provenance/0",
        "/fastContent/relevant/0",
        "/fastContent/relevant/0/provenance/0",
        "/fastContent/issues/0",
        "/storeDiscovery",
        "/storeDiscovery/nearbyStores/0",
    ] {
        let mut nested_unknown = fast.clone();
        nested_unknown
            .pointer_mut(pointer)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("fixture path {pointer} must be an object"))?
            .insert("uncontractedField".to_owned(), Value::Bool(true));
        ensure(
            ee::testing::validate_json_schema_instance(&nested_unknown, &schema).is_err(),
            format!("strict orient schema must reject an unknown field at {pointer}"),
        )?;
    }

    let outcomes = schema
        .pointer("/$defs/storeDiscovery/properties/outcome/enum")
        .and_then(Value::as_array)
        .ok_or("storeDiscovery outcome enum must be present")?;
    ensure(
        outcomes == &vec![json!("complete"), json!("truncated"), json!("unavailable")],
        "storeDiscovery must preserve complete, truncated, and unavailable as distinct outcomes",
    )?;
    ensure(
        schema
            .pointer("/$defs/storeDiscovery/properties/scanned")
            .is_none()
            && schema
                .pointer("/$defs/storeDiscovery/properties/truncated")
                .is_none(),
        "orient schema must not retain legacy booleans that collapse discovery outcomes",
    )?;

    for outcome in ["truncated", "unavailable"] {
        let mut typed_outcome = fast.clone();
        typed_outcome["storeDiscovery"]["outcome"] = json!(outcome);
        ee::testing::validate_json_schema_instance(&typed_outcome, &schema)?;
    }

    let runtime_fast_limit = u64::try_from(ee::core::orient::ORIENT_FAST_CONTENT_LIMIT)
        .map_err(|error| format!("runtime fast-content limit did not fit u64: {error}"))?;
    for pointer in [
        "/$defs/fastContent/properties/strategy/properties/recentLimit/const",
        "/$defs/fastContent/properties/strategy/properties/relevantLimit/const",
        "/$defs/fastContent/properties/recent/maxItems",
        "/$defs/fastContent/properties/relevant/maxItems",
    ] {
        ensure(
            schema.pointer(pointer).and_then(Value::as_u64) == Some(runtime_fast_limit),
            format!("{pointer} must equal the runtime fast-content limit"),
        )?;
    }
    for section in ["recent", "relevant"] {
        let item = fast["fastContent"][section][0].clone();
        let mut at_limit = fast.clone();
        at_limit["fastContent"][section] = Value::Array(vec![
            item.clone();
            ee::core::orient::ORIENT_FAST_CONTENT_LIMIT
        ]);
        ee::testing::validate_json_schema_instance(&at_limit, &schema)?;

        let mut above_limit = at_limit;
        above_limit["fastContent"][section]
            .as_array_mut()
            .ok_or_else(|| format!("fastContent.{section} must be an array"))?
            .push(item);
        ensure(
            ee::testing::validate_json_schema_instance(&above_limit, &schema).is_err(),
            format!("orient schema must reject more than the runtime limit in {section}"),
        )?;
    }
    for (field, invalid_limit) in [("recentLimit", 4), ("relevantLimit", 6)] {
        let mut invalid = fast.clone();
        invalid["fastContent"]["strategy"][field] = json!(invalid_limit);
        ensure(
            ee::testing::validate_json_schema_instance(&invalid, &schema).is_err(),
            format!("orient schema must reject a non-runtime {field}"),
        )?;
    }

    ensure(
        schema
            .pointer("/$defs/fastContentItem/properties/snippet/maxLength")
            .and_then(Value::as_u64)
            == Some(480),
        "fast-content snippets must remain contractually capped at 480 characters",
    )?;
    let mut oversized_snippet = fast;
    oversized_snippet["fastContent"]["recent"][0]["snippet"] = json!("λ".repeat(481));
    ensure(
        ee::testing::validate_json_schema_instance(&oversized_snippet, &schema).is_err(),
        "orient schema must reject a 481-character fast-content snippet",
    )?;
    Ok(())
}
