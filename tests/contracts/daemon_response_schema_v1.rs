//! Contract checks for the daemon request/response JSON Schemas.
//!
//! bd-333u0 pins the response `result` / `error` xor invariant structurally.
//! bd-3cwdd pins real Rust-serialized daemon envelopes against the published
//! schema files so Rust type drift cannot silently diverge from the docs.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use ee::daemon::DAEMON_RESPONSE_SCHEMA_V1;
use ee::daemon::protocol::{DaemonRequest, DaemonResponse};

type TestResult = Result<(), String>;

const REQUEST_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.request.v1.json";
const RESPONSE_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.response.v1.json";
const METHOD_CAPABILITIES: &str = "ee.daemon.capabilities";
const METHOD_CONTEXT: &str = "ee.daemon.context";
const METHOD_ECHO: &str = "ee.daemon.echo";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_schema(schema_path: &str) -> Result<Value, String> {
    let path = repo_root().join(schema_path);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn read_request_schema() -> Result<Value, String> {
    read_schema(REQUEST_SCHEMA_PATH)
}

fn read_response_schema() -> Result<Value, String> {
    read_schema(RESPONSE_SCHEMA_PATH)
}

fn serialized_request() -> Result<Value, String> {
    let mut request = DaemonRequest::new(
        "req-schema-cross-validation",
        "agent-schema-cross-validation",
        METHOD_ECHO,
        serde_json::json!({"hello": "world", "n": 42}),
    );
    request.workspace_id = Some("workspace-schema-cross-validation".to_owned());
    serde_json::to_value(request).map_err(|error| format!("serialize DaemonRequest: {error}"))
}

fn capabilities_request() -> Result<Value, String> {
    let request = DaemonRequest::new(
        "req-capabilities-schema-cross-validation",
        "agent-schema-cross-validation",
        METHOD_CAPABILITIES,
        serde_json::json!({}),
    );
    serde_json::to_value(request).map_err(|error| format!("serialize DaemonRequest: {error}"))
}

fn success_response() -> Value {
    serde_json::to_value(DaemonResponse::ok(
        "req-success",
        "agent-a",
        Some("workspace-a".to_owned()),
        serde_json::json!({"echo": true}),
    ))
    .expect("DaemonResponse::ok must serialize")
}

fn capabilities_response() -> Value {
    serde_json::to_value(DaemonResponse::ok(
        "req-capabilities",
        "agent-a",
        None,
        serde_json::json!({
            "protocol": "ee.daemon",
            "request_schemas": ["ee.daemon.request.v1"],
            "response_schemas": [DAEMON_RESPONSE_SCHEMA_V1],
            "methods": [
                METHOD_CAPABILITIES,
                METHOD_CONTEXT,
                METHOD_ECHO
            ],
            "authorization": {
                "ee.daemon.capabilities": "same_uid",
                "ee.daemon.context": "same_uid_workspace",
                "ee.daemon.echo": "same_uid"
            },
            "forward_compat": {
                "v1_unknown_fields": "rejected",
                "v1_unknown_methods": "daemon_unknown_method",
                "v2_migration": "Call ee.daemon.capabilities with ee.daemon.request.v1 before sending any non-v1 schema or method; downgrade to an advertised schema/method when absent."
            }
        }),
    ))
    .expect("DaemonResponse::ok must serialize")
}

fn error_response() -> Value {
    serde_json::to_value(
        DaemonResponse::err(
            "req-error",
            "agent-a",
            None,
            "daemon_unknown_method",
            "unknown method",
        )
        .with_degraded("daemon_unknown_method"),
    )
    .expect("DaemonResponse::err must serialize")
}

#[test]
fn daemon_schema_cross_validation_accepts_serialized_rust_request() -> TestResult {
    let schema = read_request_schema()?;
    validate_json_schema(&serialized_request()?, &schema, "$")
}

#[test]
fn daemon_schema_cross_validation_accepts_capabilities_request() -> TestResult {
    let schema = read_request_schema()?;
    validate_json_schema(&capabilities_request()?, &schema, "$")
}

#[test]
fn daemon_schema_cross_validation_accepts_serialized_rust_result_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&success_response(), &schema, "$")
}

#[test]
fn daemon_schema_cross_validation_accepts_capabilities_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&capabilities_response(), &schema, "$")
}

#[test]
fn daemon_schema_cross_validation_accepts_serialized_rust_error_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&error_response(), &schema, "$")
}

#[test]
fn daemon_response_schema_structurally_requires_result_xor_error() -> TestResult {
    let schema = read_response_schema()?;

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
    let schema = read_response_schema()?;
    let description = schema
        .pointer("/properties/error/description")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema error.description missing".to_string())?;

    for code in [
        "daemon_unknown_method",
        "daemon_method_unauthorized",
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
fn daemon_request_schema_documents_strict_v1_capabilities_migration() -> TestResult {
    let schema = read_request_schema()?;
    let description = schema
        .pointer("/description")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema description missing".to_string())?;

    for needle in [
        METHOD_CAPABILITIES,
        "unknown top-level fields and unknown methods are rejected",
        "downgrade or fall back to the in-process CLI path",
    ] {
        if !description.contains(needle) {
            return Err(format!(
                "request schema description missing migration phrase {needle:?}"
            ));
        }
    }

    Ok(())
}

#[test]
fn daemon_request_schema_advertises_seed_methods() -> TestResult {
    let schema = read_request_schema()?;
    let methods = schema
        .pointer("/properties/method/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema method enum missing".to_string())?;

    for method in [METHOD_CAPABILITIES, METHOD_CONTEXT, METHOD_ECHO] {
        if !methods.iter().any(|value| value.as_str() == Some(method)) {
            return Err(format!("request schema method enum missing {method}"));
        }
    }

    Ok(())
}

#[test]
fn daemon_response_schema_accepts_result_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&success_response(), &schema, "$")
}

#[test]
fn daemon_response_schema_accepts_error_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&error_response(), &schema, "$")
}

#[test]
fn daemon_response_schema_rejects_both_result_and_error() -> TestResult {
    let schema = read_response_schema()?;
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
    let schema = read_response_schema()?;
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

    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.iter().any(|option| option == value)
    {
        return Err(format!("{path} expected one of {options:?}, got {value}"));
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
