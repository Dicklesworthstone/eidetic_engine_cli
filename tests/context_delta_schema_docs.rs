//! Static contract tests for the first context-delta documentation slice.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use ee::core::context_delta::{
    ContextDeltaItemSnapshot, ContextDeltaOptions, ContextDeltaPackSnapshot, compute_context_delta,
};

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(path: &str) -> Result<String, String> {
    fs::read_to_string(repo_root().join(path)).map_err(|error| format!("read {path}: {error}"))
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string at {pointer}"))
}

fn required_fields_at(value: &Value, pointer: &str) -> Result<Vec<String>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing required array at {pointer}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{pointer} contains a non-string item"))
        })
        .collect()
}

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context} missing `{needle}`"))
    }
}

#[test]
fn context_delta_schema_pins_item_diff_and_token_budget_contract() -> TestResult {
    let schema_text = read_repo_file("docs/schemas/ee.context.delta.v1.json")?;
    let schema: Value =
        serde_json::from_str(&schema_text).map_err(|error| format!("schema parse: {error}"))?;

    if string_at(&schema, "/properties/schema/const")? != "ee.context.delta.v1" {
        return Err("schema const must be ee.context.delta.v1".to_owned());
    }

    let description = string_at(&schema, "/description")?;
    for needle in [
        "server verifies",
        "v1 changes are additive only",
        "ee.context.delta.v2",
        "rather than RFC 6902 JSON Patch",
    ] {
        ensure_contains(description, needle, "schema description")?;
    }

    let data_required = required_fields_at(&schema, "/$defs/contextDelta/required")?;
    for field in [
        "priorPackHash",
        "newPackHash",
        "items",
        "tokenSavings",
        "serverDecision",
    ] {
        if !data_required.iter().any(|candidate| candidate == field) {
            return Err(format!("contextDelta.required missing {field}"));
        }
    }

    let item_required = required_fields_at(&schema, "/$defs/itemDiff/required")?;
    for field in ["added", "removed", "modified"] {
        if !item_required.iter().any(|candidate| candidate == field) {
            return Err(format!("itemDiff.required missing {field}"));
        }
    }

    let token_required = required_fields_at(&schema, "/$defs/tokenSavings/required")?;
    for field in [
        "fullBytes",
        "deltaBytes",
        "savedBytes",
        "savedPercent",
        "netPackTokens",
    ] {
        if !token_required.iter().any(|candidate| candidate == field) {
            return Err(format!("tokenSavings.required missing {field}"));
        }
    }

    let server_decision_required = required_fields_at(
        &schema,
        "/$defs/contextDelta/properties/serverDecision/required",
    )?;
    for field in [
        "computedFromServerVerifiedPackRecord",
        "deltaChained",
        "format",
    ] {
        if !server_decision_required
            .iter()
            .any(|candidate| candidate == field)
        {
            return Err(format!("serverDecision.required missing {field}"));
        }
    }

    Ok(())
}

/// Returns the JSON-Pointer-style key names a schema permits on a
/// closed `additionalProperties: false` object at the given pointer.
fn schema_property_names(schema: &Value, properties_pointer: &str) -> Result<Vec<String>, String> {
    schema
        .pointer(properties_pointer)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("missing properties at {properties_pointer}"))?
        .keys()
        .map(|name| Ok::<_, String>(name.clone()))
        .collect()
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

/// Builds a representative envelope using only the public compute API,
/// then asserts every serialized key matches the v1 schema's closed
/// property set. This is the contract test the original review finding
/// said was missing: it would have caught the prior `{schema, …}` flat
/// envelope, the missing `serverDecision`, and the `{old, new}` object
/// field-change shape immediately.
#[test]
fn context_delta_rust_envelope_matches_schema_property_set() -> TestResult {
    let schema_text = read_repo_file("docs/schemas/ee.context.delta.v1.json")?;
    let schema: Value =
        serde_json::from_str(&schema_text).map_err(|error| format!("schema parse: {error}"))?;

    let prior_item = ContextDeltaItemSnapshot::new("mem_a")
        .with_field("contentHash", Value::String("old".to_string()))
        .with_field("estimatedTokens", serde_json::json!(10));
    let new_item = ContextDeltaItemSnapshot::new("mem_a")
        .with_field("contentHash", Value::String("new".to_string()))
        .with_field("estimatedTokens", serde_json::json!(12));
    let prior = ContextDeltaPackSnapshot::new("h1", 1, 1024, 320, vec![prior_item]);
    let new = ContextDeltaPackSnapshot::new("h2", 2, 1100, 360, vec![new_item]);
    let envelope = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
        .map_err(|error| format!("compute_context_delta: {error}"))?;
    let serialized =
        serde_json::to_value(&envelope).map_err(|error| format!("serialize envelope: {error}"))?;
    validate_json_schema(&serialized, &schema, &schema, "$")?;

    let envelope_object = serialized
        .as_object()
        .ok_or_else(|| "envelope must serialize as a JSON object".to_string())?;
    let envelope_keys = envelope_object
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_envelope = schema_property_names(&schema, "/properties")?;
    for key in &envelope_keys {
        if !allowed_envelope.iter().any(|allowed| allowed == key) {
            return Err(format!(
                "envelope key `{key}` is not in the v1 schema property set ({allowed_envelope:?})"
            ));
        }
    }
    for required in ["schema", "success", "data", "degraded"] {
        if !envelope_keys.contains(required) {
            return Err(format!("envelope missing required key `{required}`"));
        }
    }

    let data_object = serialized
        .pointer("/data")
        .and_then(Value::as_object)
        .ok_or_else(|| "data must be an object".to_string())?;
    let data_keys = data_object
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_data = schema_property_names(&schema, "/$defs/contextDelta/properties")?;
    for key in &data_keys {
        if !allowed_data.iter().any(|allowed| allowed == key) {
            return Err(format!(
                "data key `{key}` is not in the v1 contextDelta property set ({allowed_data:?})"
            ));
        }
    }
    for required in [
        "priorPackHash",
        "newPackHash",
        "items",
        "tokenSavings",
        "serverDecision",
    ] {
        if !data_keys.contains(required) {
            return Err(format!("data missing required key `{required}`"));
        }
    }

    let server_decision = serialized
        .pointer("/data/serverDecision")
        .and_then(Value::as_object)
        .ok_or_else(|| "serverDecision must be an object".to_string())?;
    let server_keys = server_decision
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_server = schema_property_names(
        &schema,
        "/$defs/contextDelta/properties/serverDecision/properties",
    )?;
    for key in &server_keys {
        if !allowed_server.iter().any(|allowed| allowed == key) {
            return Err(format!(
                "serverDecision key `{key}` is not in the v1 schema property set ({allowed_server:?})"
            ));
        }
    }
    for required in [
        "computedFromServerVerifiedPackRecord",
        "deltaChained",
        "format",
    ] {
        if !server_keys.contains(required) {
            return Err(format!("serverDecision missing required key `{required}`"));
        }
    }

    let field_change = serialized
        .pointer("/data/items/modified/0/fieldChanges/contentHash")
        .ok_or_else(|| "modified item field change missing".to_string())?;
    let pair = field_change.as_array().ok_or_else(|| {
        format!("ordinary field change must serialize as a JSON array, got {field_change}")
    })?;
    if pair.len() != 2 {
        return Err(format!(
            "ordinary field change must be a two-element [old, new] array; got {pair:?}"
        ));
    }
    Ok(())
}

#[test]
fn context_delta_apply_guide_covers_agent_safety_rules() -> TestResult {
    let guide = read_repo_file("docs/agent-ux/context-delta-apply.md")?;
    let first_non_empty = guide
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .ok_or_else(|| "guide has no prose".to_owned())?;

    if first_non_empty != "Delta payloads add to your prompt; they do not replace the base pack." {
        return Err("guide must open with the base+delta prompt-budget warning".to_owned());
    }

    for needle in [
        "data.pack.hash",
        "same workspace",
        "The server never chains deltas",
        "data.tokenSavings.netPackTokens",
        "No-op deltas use empty arrays",
        "Delta v1 is JSON-only",
        "context_delta_format_unsupported",
        "should not add `ee context apply-delta",
        "context_delta_prior_unknown",
    ] {
        ensure_contains(&guide, needle, "context delta apply guide")?;
    }

    Ok(())
}
