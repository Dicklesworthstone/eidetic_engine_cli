//! bd-1rwxf: contract test that validates a real emitted
//! [`ContextDeltaEnvelope`] against `docs/schemas/ee.context.delta.v1.json`.
//!
//! The original review finding (bd-1h96m) was that
//! `tests/context_delta_schema_docs.rs` only inspected the *schema file*
//! and the Rust envelope's *key names*, never the full schema-vs-emission
//! join. This contract test closes that gap: it computes a real envelope
//! via the public API, serializes it, and walks the v1 JSON Schema with
//! a self-contained subset validator that handles `$ref`, `oneOf`,
//! `const`, `enum`, `type`, `required`, `additionalProperties`, `items`,
//! `prefixItems`, `minItems`/`maxItems`, and `minimum` — every keyword
//! the v1 schema uses.
//!
//! The validator is intentionally embedded (rather than extracted to
//! `tests/support/`) so this contract is self-contained: a future
//! breakage of the shared helper cannot silently weaken this test.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use eidetic_engine_cli::core::context_delta::{
    ContextDeltaItemSnapshot, ContextDeltaOptions, ContextDeltaPackSnapshot, compute_context_delta,
};

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_schema() -> Result<Value, String> {
    let text = fs::read_to_string(repo_root().join("docs/schemas/ee.context.delta.v1.json"))
        .map_err(|error| format!("read schema: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse schema: {error}"))
}

fn diverse_envelope_value() -> Result<Value, String> {
    // Mix of added / removed / modified to exercise each branch of the
    // schema's itemDiff plus a non-trivial fieldChange `[old, new]` array.
    let prior_a = ContextDeltaItemSnapshot::new("mem_a")
        .with_field("contentHash", Value::String("old-a".to_string()))
        .with_field("estimatedTokens", serde_json::json!(10));
    let prior_c = ContextDeltaItemSnapshot::new("mem_c")
        .with_field("contentHash", Value::String("c".to_string()))
        .with_field("estimatedTokens", serde_json::json!(30));
    let new_a = ContextDeltaItemSnapshot::new("mem_a")
        .with_field("contentHash", Value::String("new-a".to_string()))
        .with_field("estimatedTokens", serde_json::json!(12));
    let new_b = ContextDeltaItemSnapshot::new("mem_b")
        .with_field("contentHash", Value::String("b".to_string()))
        .with_field("estimatedTokens", serde_json::json!(20));

    let prior = ContextDeltaPackSnapshot::new("h1", 1, 1024, 320, vec![prior_a, prior_c]);
    let new = ContextDeltaPackSnapshot::new("h2", 2, 1100, 360, vec![new_a, new_b]);
    let envelope = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
        .map_err(|error| format!("compute_context_delta: {error}"))?;
    serde_json::to_value(&envelope).map_err(|error| format!("serialize envelope: {error}"))
}

fn oversized_envelope_value() -> Result<Value, String> {
    // Forces the fallback path so the contract test also validates the
    // `serverDecision.fallbackReason` and `degraded[]` shapes.
    let prior = ContextDeltaPackSnapshot::new("h1", 1, 1024, 320, Vec::new());
    let new_only_item = ContextDeltaItemSnapshot::new("mem_a")
        .with_field("contentHash", Value::String("a".to_string()))
        .with_field("estimatedTokens", serde_json::json!(10));
    let new = ContextDeltaPackSnapshot::new("h2", 2, 1100, 360, vec![new_only_item]);
    let envelope = compute_context_delta(&prior, &new, ContextDeltaOptions::new(Some(1)))
        .map_err(|error| format!("compute_context_delta: {error}"))?;
    serde_json::to_value(&envelope).map_err(|error| format!("serialize envelope: {error}"))
}

#[test]
fn context_delta_v1_envelope_validates_against_published_schema() -> TestResult {
    let schema = read_schema()?;
    let envelope = diverse_envelope_value()?;
    validate_json_schema(&envelope, &schema, &schema, "$")
}

#[test]
fn context_delta_v1_fallback_envelope_validates_against_published_schema() -> TestResult {
    let schema = read_schema()?;
    let envelope = oversized_envelope_value()?;
    validate_json_schema(&envelope, &schema, &schema, "$")
}

#[test]
fn context_delta_v1_validator_rejects_legacy_object_field_change() -> TestResult {
    // Sanity-pin the validator: feeding it a payload shaped like the
    // pre-bd-1h96m drift (`fieldChanges.<name>` as `{old, new}` object
    // instead of the schema's `[old, new]` array) must fail validation.
    // If this test ever starts passing, the validator has weakened and
    // the positive tests above no longer prove anything.
    let schema = read_schema()?;
    let mut envelope = diverse_envelope_value()?;
    let modified = envelope
        .pointer_mut("/data/items/modified/0/fieldChanges/contentHash")
        .ok_or_else(|| "missing contentHash fieldChange".to_string())?;
    *modified = serde_json::json!({"old": "old-a", "new": "new-a"});

    match validate_json_schema(&envelope, &schema, &schema, "$") {
        Ok(()) => Err(
            "validator accepted the legacy {old, new} object field change; it must enforce the schema oneOf"
                .into(),
        ),
        Err(_) => Ok(()),
    }
}

#[test]
fn context_delta_v1_validator_rejects_dropped_server_decision() -> TestResult {
    // Sanity-pin the validator: dropping the schema-required
    // `serverDecision` field must fail validation. Mirrors the second
    // drift mode bd-1h96m fixed.
    let schema = read_schema()?;
    let mut envelope = diverse_envelope_value()?;
    envelope
        .pointer_mut("/data")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "data is not an object".to_string())?
        .remove("serverDecision");

    match validate_json_schema(&envelope, &schema, &schema, "$") {
        Ok(()) => {
            Err("validator accepted an envelope missing serverDecision; required-field handling is broken"
                .into())
        }
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
        let target = resolve_local_ref(root_schema, reference)?;
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

    let expected_types = schema_types(schema);
    if !expected_types.is_empty() {
        if !expected_types
            .iter()
            .any(|expected_type| json_type_matches(value, expected_type))
        {
            return Err(format!(
                "{path} expected type {:?}, got {}",
                expected_types,
                json_type_name(value)
            ));
        }
        if value.is_null() {
            return Ok(());
        }
    }

    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
        && value.as_f64().is_some_and(|actual| actual < minimum)
    {
        return Err(format!("{path} expected minimum {minimum}, got {value}"));
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
                Some(Value::Object(_)) => {
                    validate_json_schema(
                        child,
                        &schema["additionalProperties"],
                        root_schema,
                        &child_path,
                    )?;
                }
                Some(Value::Bool(true)) | None => {}
                Some(other) => {
                    return Err(format!("{path} unsupported additionalProperties: {other}"));
                }
            }
        }
    }

    if let Some(items) = value.as_array() {
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
            && items.len() < min_items as usize
        {
            return Err(format!(
                "{path} expected at least {min_items} items, got {}",
                items.len()
            ));
        }
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64)
            && items.len() > max_items as usize
        {
            return Err(format!(
                "{path} expected at most {max_items} items, got {}",
                items.len()
            ));
        }
        if let Some(prefix_items) = schema.get("prefixItems").and_then(Value::as_array) {
            for (index, item_schema) in prefix_items.iter().enumerate() {
                let Some(item) = items.get(index) else {
                    break;
                };
                validate_json_schema(item, item_schema, root_schema, &format!("{path}[{index}]"))?;
            }
        }
        if let Some(item_schema) = schema.get("items")
            && !schema.get("prefixItems").is_some_and(Value::is_array)
        {
            for (index, item) in items.iter().enumerate() {
                validate_json_schema(item, item_schema, root_schema, &format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn resolve_local_ref<'a>(root: &'a Value, reference: &str) -> Result<&'a Value, String> {
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| format!("only local JSON Schema refs are supported, got {reference}"))?;
    root.pointer(pointer)
        .ok_or_else(|| format!("schema reference {reference} did not resolve"))
}

fn schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
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
        Value::Number(number) if number.as_i64().is_some() || number.as_u64().is_some() => {
            "integer"
        }
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
