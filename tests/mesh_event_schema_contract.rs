//! Contract checks for the optional mesh event schema and hash rules.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use ee::models::{KNOWN_SCHEMAS, MESH_EVENT_SCHEMA_V1};
use serde_json::{Map, Value, json};

type TestResult = Result<(), String>;

const BASELINE_FEATURE: &str = "mesh.event.v1";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let mut ordered = Map::new();
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                ordered.insert(key.clone(), canonicalize(&values[key]));
            }
            Value::Object(ordered)
        }
        _ => value.clone(),
    }
}

fn event_digest(event: &Value) -> Result<String, String> {
    let mut hashable = event.clone();
    let object = hashable
        .as_object_mut()
        .ok_or_else(|| "event must be an object".to_string())?;
    object.remove("eventHash");
    object.remove("eventId");
    let canonical = canonicalize(&hashable);
    let bytes = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn event_id_from_hash(event_hash: &str) -> Result<String, String> {
    let digest = event_hash
        .strip_prefix("blake3:")
        .ok_or_else(|| format!("event hash lacks blake3 prefix: {event_hash}"))?;
    Ok(format!("mesh_evt_{digest}"))
}

fn sample_event() -> Result<Value, String> {
    let mut event = json!({
        "schema": MESH_EVENT_SCHEMA_V1,
        "eventId": "mesh_evt_PLACEHOLDER",
        "originNodeId": "node_alpha_000001",
        "originWorkspaceId": "wsp_release_mesh_000001",
        "producerPeerId": "peer_agent_000001",
        "seq": 1,
        "prevEventHash": null,
        "eventHash": "blake3:0000000000000000000000000000000000000000000000000000000000000000",
        "eventKind": "create",
        "logicalMemoryId": "mem_release_policy_000001",
        "contentHash": "blake3:1111111111111111111111111111111111111111111111111111111111111111",
        "bodyRef": {
            "kind": "inlinePreview",
            "previewHash": "blake3:2222222222222222222222222222222222222222222222222222222222222222",
            "sizeBytes": 128
        },
        "materialLane": "metadata",
        "redactionClass": "preview",
        "trustLane": "peerAgent",
        "trustClaim": {
            "producer": "agent-mail-peer"
        },
        "requiredFeatures": [BASELINE_FEATURE],
        "producedAt": "2026-05-16T13:40:00Z"
    });
    let event_hash = event_digest(&event)?;
    let event_id = event_id_from_hash(&event_hash)?;
    let object = event
        .as_object_mut()
        .ok_or_else(|| "event must be an object".to_string())?;
    object.insert("eventHash".to_string(), Value::String(event_hash));
    object.insert("eventId".to_string(), Value::String(event_id));
    Ok(event)
}

fn unsupported_required_features(
    event: &Value,
    known: &BTreeSet<&str>,
) -> Result<Vec<String>, String> {
    let features = event
        .get("requiredFeatures")
        .and_then(Value::as_array)
        .ok_or_else(|| "event missing requiredFeatures array".to_string())?;
    let mut unsupported = Vec::new();
    for feature in features {
        let feature = feature
            .as_str()
            .ok_or_else(|| "requiredFeatures entries must be strings".to_string())?;
        if !known.contains(feature) {
            unsupported.push(feature.to_owned());
        }
    }
    unsupported.sort();
    Ok(unsupported)
}

#[test]
fn mesh_event_schema_is_documented_and_registered() -> TestResult {
    let schema = read_json("docs/schemas/ee.mesh.event.v1.json")?;

    ensure(
        schema.pointer("/$schema").and_then(Value::as_str)
            == Some("https://json-schema.org/draft/2020-12/schema"),
        "json schema draft must be 2020-12",
    )?;
    ensure(
        schema.pointer("/$id").and_then(Value::as_str)
            == Some("https://eidetic-engine/schemas/ee.mesh.event.v1.json"),
        "schema id must match docs path",
    )?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(MESH_EVENT_SCHEMA_V1),
        "schema title must match runtime constant",
    )?;
    ensure(
        schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "mesh event schema must be closed to unknown top-level fields",
    )?;
    ensure(
        KNOWN_SCHEMAS.contains(&MESH_EVENT_SCHEMA_V1),
        "known schema registry must include mesh event",
    )?;
    let supported = ee::core::supported_schemas()
        .into_iter()
        .map(|schema| schema.schema)
        .collect::<Vec<_>>();
    ensure(
        supported.contains(&MESH_EVENT_SCHEMA_V1),
        "supported schema registry must include mesh event",
    )
}

#[test]
fn mesh_event_hash_and_event_id_are_canonical_and_stable() -> TestResult {
    let event = sample_event()?;
    let expected_hash = event
        .get("eventHash")
        .and_then(Value::as_str)
        .ok_or_else(|| "sample event missing eventHash".to_string())?
        .to_owned();
    let expected_id = event
        .get("eventId")
        .and_then(Value::as_str)
        .ok_or_else(|| "sample event missing eventId".to_string())?
        .to_owned();

    ensure(
        event_digest(&event)? == expected_hash,
        "event hash must be recomputable from canonical event without eventHash",
    )?;
    ensure(
        event_id_from_hash(&expected_hash)? == expected_id,
        "event id must be derived from event hash",
    )?;

    let mut reordered = Map::new();
    let object = event
        .as_object()
        .ok_or_else(|| "sample event must be an object".to_string())?;
    for key in object.keys().rev() {
        reordered.insert(key.clone(), object[key].clone());
    }
    ensure(
        event_digest(&Value::Object(reordered))? == expected_hash,
        "object key order must not affect mesh event hash",
    )
}

#[test]
fn mesh_event_unknown_required_features_are_rejected_before_replay() -> TestResult {
    let mut event = sample_event()?;
    event
        .as_object_mut()
        .ok_or_else(|| "sample event must be an object".to_string())?
        .insert(
            "requiredFeatures".to_string(),
            json!([BASELINE_FEATURE, "mesh.future-required-lane"]),
        );
    let known = BTreeSet::from([BASELINE_FEATURE]);

    ensure(
        unsupported_required_features(&event, &known)? == vec!["mesh.future-required-lane"],
        "unknown required features must be rejected or quarantined before replay",
    )
}
