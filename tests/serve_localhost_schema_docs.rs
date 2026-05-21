//! Static contracts for the serve-localhost v2 schema slice.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(path: &str) -> Result<String, String> {
    fs::read_to_string(repo_root().join(path)).map_err(|error| format!("read {path}: {error}"))
}

fn parse_schema(path: &str) -> Result<Value, String> {
    let text = read_repo_file(path)?;
    serde_json::from_str(&text).map_err(|error| format!("parse {path}: {error}"))
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

fn enum_values_at(value: &Value, pointer: &str) -> Result<Vec<String>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing enum array at {pointer}"))?
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
fn serve_startup_schema_pins_bind_auth_and_limit_contract() -> TestResult {
    let schema = parse_schema("docs/schemas/ee.serve.startup.v1.json")?;

    if string_at(&schema, "/properties/schema/const")? != "ee.serve.startup.v1" {
        return Err("startup schema const must be ee.serve.startup.v1".to_owned());
    }

    let description = string_at(&schema, "/description")?;
    for needle in [
        "loopback-only by default",
        "--allow-non-loopback",
        "EE_SERVE_TOKEN",
        "never exposes",
        "HTTP-framework",
    ] {
        ensure_contains(description, needle, "startup schema description")?;
    }

    let required = required_fields_at(&schema, "/required")?;
    for field in [
        "bind",
        "protocol",
        "tokenPosture",
        "readiness",
        "endpoints",
        "limits",
        "degraded",
    ] {
        if !required.iter().any(|candidate| candidate == field) {
            return Err(format!("startup.required missing {field}"));
        }
    }

    let forbidden_deps = enum_values_at(
        &schema,
        "/properties/protocol/properties/forbiddenHttpDeps/items/enum",
    )?;
    for dep in ["hyper", "axum", "tower", "reqwest"] {
        if !forbidden_deps.iter().any(|candidate| candidate == dep) {
            return Err(format!("startup forbidden deps enum missing {dep}"));
        }
    }

    let endpoints = enum_values_at(&schema, "/$defs/endpointSummary/properties/endpoint/enum")?;
    for endpoint in [
        "status",
        "doctor",
        "search",
        "context",
        "why",
        "swarmBrief",
        "durableWrite",
        "events",
    ] {
        if !endpoints.iter().any(|candidate| candidate == endpoint) {
            return Err(format!("startup endpoint enum missing {endpoint}"));
        }
    }

    Ok(())
}

#[test]
fn serve_endpoint_schema_pins_cli_parity_and_sse_boundaries() -> TestResult {
    let schema = parse_schema("docs/schemas/ee.serve.endpoint.v1.json")?;

    if string_at(&schema, "/properties/schema/const")? != "ee.serve.endpoint.v1" {
        return Err("endpoint schema const must be ee.serve.endpoint.v1".to_owned());
    }

    let description = string_at(&schema, "/description")?;
    for needle in [
        "HTTP/1.1 request metadata",
        "CLI-equivalent surface",
        "bearer-token posture",
        "ee.response.v2 or ee.error.v2",
        "Business logic remains owned by the CLI/core services",
    ] {
        ensure_contains(description, needle, "endpoint schema description")?;
    }

    let request_required = required_fields_at(&schema, "/$defs/request/required")?;
    for field in [
        "requestId",
        "method",
        "path",
        "endpoint",
        "cliEquivalent",
        "auth",
        "bodyBytes",
        "query",
    ] {
        if !request_required.iter().any(|candidate| candidate == field) {
            return Err(format!("request.required missing {field}"));
        }
    }

    let payload_schemas = enum_values_at(&schema, "/$defs/response/properties/payloadSchema/enum")?;
    for payload_schema in ["ee.response.v2", "ee.error.v2"] {
        if !payload_schemas
            .iter()
            .any(|candidate| candidate == payload_schema)
        {
            return Err(format!(
                "response.payloadSchema enum missing {payload_schema}"
            ));
        }
    }

    if schema
        .pointer("/$defs/request/properties/chunkedUploadAccepted/const")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("chunked uploads must be rejected in the v2 schema slice".to_owned());
    }

    if schema
        .pointer("/$defs/sse/properties/readOnly/const")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("SSE stream schema must be read-only".to_owned());
    }

    Ok(())
}

#[test]
fn serve_schemas_stay_aligned_with_adr_0033_terms() -> TestResult {
    let adr = read_repo_file("docs/adr/0033-serve-localhost-v2-design.md")?;
    for needle in [
        "std::net",
        "Asupersync",
        "127.0.0.1",
        "EE_SERVE_TOKEN",
        "Content-Length",
        "GET /v1/context",
        "GET /v1/events",
        "TcpStream",
    ] {
        ensure_contains(&adr, needle, "ADR 0033")?;
    }

    let startup = read_repo_file("docs/schemas/ee.serve.startup.v1.json")?;
    let endpoint = read_repo_file("docs/schemas/ee.serve.endpoint.v1.json")?;
    for needle in ["EE_SERVE_TOKEN", "loopback", "HTTP/1.1", "sseReadOnly"] {
        ensure_contains(&startup, needle, "startup schema")?;
    }
    for needle in [
        "Content-Length",
        "ee.response.v2",
        "ee.error.v2",
        "chunkedUploadAccepted",
    ] {
        ensure_contains(&endpoint, needle, "endpoint schema")?;
    }

    Ok(())
}
