//! bd-333u0: contract checks for `docs/schemas/ee.daemon.response.v1.json`.
//!
//! The schema prose says exactly one of `result` or `error` is present. This
//! test pins that invariant structurally so non-Rust daemon clients cannot
//! accept responses that the Rust serde boundary would reject.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.daemon.response.v1.json";
const DAEMON_RESPONSE_SCHEMA_V1: &str = "ee.daemon.response.v1";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_schema() -> Result<Value, String> {
    let path = repo_root().join(SCHEMA_PATH);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn success_response() -> Value {
    serde_json::json!({
        "schema": DAEMON_RESPONSE_SCHEMA_V1,
        "request_id": "req-success",
        "agent_id": "agent-a",
        "workspace_id": "workspace-a",
        "result": {
            "echo": true
        },
        "degraded_codes": []
    })
}

fn error_response() -> Value {
    serde_json::json!({
        "schema": DAEMON_RESPONSE_SCHEMA_V1,
        "request_id": "req-error",
        "agent_id": "agent-a",
        "error": {
            "code": "daemon_unknown_method",
            "message": "unknown method"
        },
        "degraded_codes": ["daemon_unknown_method"]
    })
}

#[test]
fn daemon_response_schema_structurally_requires_result_xor_error() -> TestResult {
    let schema = read_schema()?;

    let one_of = schema
        .pointer("/oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema root oneOf missing".to_string())?;
    if one_of.len() != 2 {
        return Err(format!(
            "schema root oneOf must have 2 branches, got {}",
            one_of.len()
        ));
    }

    for field in ["result", "error"] {
        if !one_of.iter().any(|branch| {
            branch
                .pointer("/required")
                .and_then(Value::as_array)
                .is_some_and(|required| required.iter().any(|value| value.as_str() == Some(field)))
        }) {
            return Err(format!(
                "schema root oneOf missing required branch for {field}"
            ));
        }
    }

    Ok(())
}

#[test]
fn daemon_response_schema_documents_seed_error_codes() -> TestResult {
    let schema = read_schema()?;
    let description = schema
        .pointer("/properties/error/description")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema error.description missing".to_string())?;

    for code in [
        "daemon_unknown_method",
        "daemon_request_decode_failed",
        "daemon_request_schema_mismatch",
        "daemon_ann_warmload_not_yet_implemented",
    ] {
        if !description.contains(code) {
            return Err(format!("schema error.description missing {code}"));
        }
    }

    Ok(())
}

#[test]
fn daemon_response_schema_accepts_result_response() -> TestResult {
    let schema = read_schema()?;
    validate_json_schema(&success_response(), &schema, "$")
}

#[test]
fn daemon_response_schema_accepts_error_response() -> TestResult {
    let schema = read_schema()?;
    validate_json_schema(&error_response(), &schema, "$")
}

#[test]
fn daemon_response_schema_rejects_both_result_and_error() -> TestResult {
    let schema = read_schema()?;
    let mut response = success_response();
    response
        .as_object_mut()
        .ok_or_else(|| "success response is not an object".to_string())?
        .insert(
            "error".to_string(),
            serde_json::json!({
                "code": "daemon_unknown_method",
                "message": "unknown method"
            }),
        );

    match validate_json_schema(&response, &schema, "$") {
        Ok(()) => Err("validator accepted response with both result and error".to_string()),
        Err(_) => Ok(()),
    }
}

#[test]
fn daemon_response_schema_rejects_neither_result_nor_error() -> TestResult {
    let schema = read_schema()?;
    let response = serde_json::json!({
        "schema": DAEMON_RESPONSE_SCHEMA_V1,
        "request_id": "req-empty",
        "agent_id": "agent-a",
        "degraded_codes": []
    });

    match validate_json_schema(&response, &schema, "$") {
        Ok(()) => Err("validator accepted response missing both result and error".to_string()),
        Err(_) => Ok(()),
    }
}

fn validate_json_schema(value: &Value, schema: &Value, path: &str) -> TestResult {
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let matches = options
            .iter()
            .filter(|candidate| validate_json_schema(value, candidate, path).is_ok())
            .count();
        if matches != 1 {
            return Err(format!("{path} matched {matches} oneOf branches"));
        }
    }

    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err(format!("{path} expected const {expected}, got {value}"));
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

    if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64)
        && value
            .as_str()
            .is_some_and(|actual| actual.chars().count() < min_length as usize)
    {
        return Err(format!("{path} shorter than minLength {min_length}"));
    }

    if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64)
        && value
            .as_str()
            .is_some_and(|actual| actual.chars().count() > max_length as usize)
    {
        return Err(format!("{path} longer than maxLength {max_length}"));
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
                validate_json_schema(child, property_schema, &child_path)?;
                continue;
            }
            match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    return Err(format!("{path} contains unexpected field {key}"));
                }
                Some(Value::Bool(true)) | None => {}
                Some(other) => {
                    return Err(format!("{path} unsupported additionalProperties: {other}"));
                }
            }
        }
    }

    if let Some(items) = value.as_array()
        && let Some(item_schema) = schema.get("items")
    {
        for (index, item) in items.iter().enumerate() {
            validate_json_schema(item, item_schema, &format!("{path}[{index}]"))?;
        }
    }

    Ok(())
}

fn schema_types(schema: &Value) -> Option<Vec<&str>> {
    match schema.get("type") {
        Some(Value::String(kind)) => Some(vec![kind.as_str()]),
        Some(Value::Array(kinds)) => Some(kinds.iter().filter_map(Value::as_str).collect()),
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
        Value::Number(number) if number.as_i64().is_some() || number.as_u64().is_some() => {
            "integer"
        }
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
