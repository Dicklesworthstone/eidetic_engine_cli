//! Contract checks for the daemon request/response JSON Schemas.
//!
//! bd-333u0 pins the response `result` / `error` xor invariant structurally.
//! bd-3cwdd pins real Rust-serialized daemon envelopes against the published
//! schema files so Rust type drift cannot silently diverge from the docs.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use ee::daemon::DAEMON_RESPONSE_SCHEMA_V1;
use ee::daemon::protocol::{DaemonRequest, DaemonResponse};
use ee::daemon::server::{
    DAEMON_SEARCH_EXECUTION_FAILED_CODE, DAEMON_SEARCH_PARAMS_INVALID_CODE,
    DAEMON_SEARCH_REQUEST_SCHEMA_V1, DAEMON_SEARCH_RESPONSE_SCHEMA_V2, METHOD_CAPABILITIES,
    METHOD_CONTEXT, METHOD_ECHO, METHOD_SEARCH, METHOD_SHUTDOWN, METHOD_TELEMETRY, METHOD_WRITE,
    METHOD_WRITE_JOURNAL,
};

type TestResult = Result<(), String>;

const REQUEST_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.request.v1.json";
const RESPONSE_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.response.v1.json";
const SEARCH_REQUEST_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.search.request.v1.json";
const SEARCH_RESPONSE_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.search.response.v2.json";

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

fn collect_string_set(value: &Value, context: &str) -> Result<BTreeSet<String>, String> {
    let array = value
        .as_array()
        .ok_or_else(|| format!("{context} must be an array, got {value}"))?;
    array
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context} contains non-string value {entry}"))
        })
        .collect()
}

fn string_set(fields: &[&str]) -> BTreeSet<String> {
    fields.iter().map(|field| (*field).to_owned()).collect()
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
                METHOD_ECHO,
                METHOD_SEARCH,
                METHOD_SHUTDOWN,
                METHOD_TELEMETRY,
                METHOD_WRITE,
                METHOD_WRITE_JOURNAL
            ],
            "authorization": {
                "ee.daemon.capabilities": "same_uid",
                "ee.daemon.context": "same_uid_workspace",
                "ee.daemon.echo": "same_uid",
                "ee.daemon.search": "same_uid_workspace",
                "ee.daemon.shutdown": "same_uid",
                "ee.daemon.telemetry": "same_uid",
                "ee.daemon.write": "same_uid_workspace",
                "ee.daemon.write_journal": "same_uid_workspace"
            },
            "method_schemas": {
                "ee.daemon.search": {
                    "request": DAEMON_SEARCH_REQUEST_SCHEMA_V1,
                    "response": DAEMON_SEARCH_RESPONSE_SCHEMA_V2
                }
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
    let root_required = collect_string_set(&schema["required"], "schema root required")?;
    let expected_root_required = string_set(&["schema", "request_id", "agent_id"]);
    if root_required != expected_root_required {
        return Err(format!(
            "schema root required drifted; expected {expected_root_required:?}, got {root_required:?}"
        ));
    }

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

    let actual_branch_required = one_of
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            collect_string_set(
                &branch["required"],
                &format!("schema root oneOf[{index}].required"),
            )
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let expected_branch_required = [string_set(&["result"]), string_set(&["error"])]
        .into_iter()
        .collect::<BTreeSet<_>>();
    if actual_branch_required != expected_branch_required {
        return Err(format!(
            "schema root oneOf required branches drifted; expected {expected_branch_required:?}, got {actual_branch_required:?}"
        ));
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
        "daemon_context_params_invalid",
        "daemon_context_deadline_exceeded",
        "daemon_context_execution_failed",
        DAEMON_SEARCH_PARAMS_INVALID_CODE,
        DAEMON_SEARCH_EXECUTION_FAILED_CODE,
    ] {
        if !description.contains(code) {
            return Err(format!("schema error.description missing {code}"));
        }
    }

    Ok(())
}

#[test]
fn daemon_search_method_schemas_pin_paths_timings_and_nested_strictness() -> TestResult {
    let request = read_schema(SEARCH_REQUEST_SCHEMA_PATH)?;
    let response = read_schema(SEARCH_RESPONSE_SCHEMA_PATH)?;
    if request.pointer("/$id").and_then(Value::as_str)
        != Some("https://eidetic-engine/schemas/ee.daemon.search.request.v1.json")
        || response.pointer("/$id").and_then(Value::as_str)
            != Some("https://eidetic-engine/schemas/ee.daemon.search.response.v2.json")
    {
        return Err("daemon search method schema ids drifted".to_owned());
    }
    for (schema, pointer, context) in [
        (&request, "/additionalProperties", "search request"),
        (&response, "/additionalProperties", "search response"),
        (
            &response,
            "/properties/response/additionalProperties",
            "canonical response",
        ),
        (
            &response,
            "/properties/response/properties/data/additionalProperties",
            "canonical search data",
        ),
        (
            &response,
            "/properties/timing/additionalProperties",
            "search timing",
        ),
        (
            &response,
            "/$defs/searchDocument/additionalProperties",
            "search document",
        ),
    ] {
        if schema.pointer(pointer).and_then(Value::as_bool) != Some(false) {
            return Err(format!("{context} must reject unknown fields"));
        }
    }
    let timing_required = collect_string_set(
        &response["properties"]["timing"]["required"],
        "search timing required",
    )?;
    let expected_timing = string_set(&["daemonTotal", "embedderPreparation", "indexOpen", "query"]);
    if timing_required != expected_timing {
        return Err(format!(
            "search timing required fields drifted; expected {expected_timing:?}, got {timing_required:?}"
        ));
    }
    let request_description = request
        .pointer("/description")
        .and_then(Value::as_str)
        .ok_or_else(|| "search request description missing".to_owned())?;
    for phrase in ["absolute", "canonical", "symlink escape"] {
        if !request_description.contains(phrase) {
            return Err(format!(
                "search request containment contract missing {phrase:?}"
            ));
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

    for method in [
        METHOD_CAPABILITIES,
        METHOD_ECHO,
        METHOD_CONTEXT,
        METHOD_SEARCH,
        METHOD_SHUTDOWN,
        METHOD_TELEMETRY,
        METHOD_WRITE,
        METHOD_WRITE_JOURNAL,
    ] {
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

/// bd-2yg7d / bd-1dzhz: the daemon schema descriptions are the machine-facing
/// contract for where clients should expect the socket. The production
/// resolver (`default_daemon_socket_path`, ADR 0055) never defaults to a bare
/// shared socket directly under /tmp — that shape was the bd-3j0td
/// cross-tenant attack surface — so no daemon schema may describe it as a
/// default. The forbidden-substring arm is the planted regression trap: it
/// fails against the pre-fix descriptions that claimed the bare path on
/// macOS, and fails again if anyone reintroduces the claim.
#[test]
fn daemon_schema_descriptions_document_per_uid_socket_default() -> TestResult {
    const DAEMON_LIFECYCLE_SCHEMAS: &[&str] = &[
        REQUEST_SCHEMA_PATH,
        "docs/schemas/ee.daemon.start.v1.json",
        "docs/schemas/ee.daemon.stop.v1.json",
    ];
    // The pre-fix descriptions claimed this bare shared default on macOS.
    let forbidden_bare_default = "/tmp/ee-daemon.sock";
    // The true fallback documented by the resolver: a per-UID parent under
    // the temp root, alongside the XDG-first Linux default.
    let required_per_uid_fallback = "${TMPDIR:-/tmp}/ee-${uid}/daemon.sock";
    let required_xdg_default = "${XDG_RUNTIME_DIR}/ee/daemon.sock";

    for schema_path in DAEMON_LIFECYCLE_SCHEMAS {
        let path = repo_root().join(schema_path);
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if text.contains(forbidden_bare_default) {
            return Err(format!(
                "{schema_path} still documents the forbidden bare shared socket default \
                 {forbidden_bare_default}; the resolver publishes per-UID paths only \
                 (bd-3j0td, bd-10ex7, ADR 0055)"
            ));
        }
        if !text.contains(required_per_uid_fallback) {
            return Err(format!(
                "{schema_path} does not document the per-UID fallback \
                 {required_per_uid_fallback} that production `default_daemon_socket_path` uses"
            ));
        }
        if !text.contains(required_xdg_default) {
            return Err(format!(
                "{schema_path} does not document the XDG-first Linux default \
                 {required_xdg_default}"
            ));
        }
    }
    Ok(())
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
