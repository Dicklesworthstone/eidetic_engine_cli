//! Conformance harness for the narrow `ee serve` HTTP/1.1 transport.
//!
//! Scope: one small fixture matrix over the public parser and transport
//! renderer. This harness does not open sockets and does not execute the
//! heavyweight search/context handlers; it pins the transport contract that
//! recently moved from dispatch-plan stubs to real endpoint envelopes.

use ee::serve::{
    SERVE_ENDPOINT_SCHEMA_V1, ServeEndpoint, ServeLimits, parse_serve_http_request,
    render_serve_http_json_response, render_serve_transport_exchange,
};
use serde_json::{Value as JsonValue, json};

type TestResult = Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequirementLevel {
    Must,
    Should,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Requirement {
    id: &'static str,
    level: RequirementLevel,
    clause: &'static str,
    covered_by: &'static str,
}

const REQUIREMENTS: &[Requirement] = &[
    Requirement {
        id: "SERVE-TRANSPORT-001",
        level: RequirementLevel::Must,
        clause: "request line uses exactly METHOD SP target SP HTTP/1.1",
        covered_by: "serve_parser_rejects_nonconforming_request_framing",
    },
    Requirement {
        id: "SERVE-TRANSPORT-002",
        level: RequirementLevel::Must,
        clause: "only GET and POST are accepted by the narrow v2 transport",
        covered_by: "serve_parser_rejects_nonconforming_request_framing",
    },
    Requirement {
        id: "SERVE-TRANSPORT-003",
        level: RequirementLevel::Must,
        clause: "request target must be an absolute path",
        covered_by: "serve_parser_rejects_nonconforming_request_framing",
    },
    Requirement {
        id: "SERVE-TRANSPORT-004",
        level: RequirementLevel::Must,
        clause: "duplicate header rows are rejected instead of collapsed",
        covered_by: "serve_parser_rejects_nonconforming_request_framing",
    },
    Requirement {
        id: "SERVE-TRANSPORT-005",
        level: RequirementLevel::Must,
        clause: "chunked transfer codings are rejected before Content-Length framing",
        covered_by: "serve_parser_rejects_nonconforming_request_framing",
    },
    Requirement {
        id: "SERVE-TRANSPORT-006",
        level: RequirementLevel::Must,
        clause: "POST requests require an explicit Content-Length",
        covered_by: "serve_parser_rejects_nonconforming_request_framing",
    },
    Requirement {
        id: "SERVE-TRANSPORT-007",
        level: RequirementLevel::Must,
        clause: "body size, short body, and extra keepalive bytes fail closed",
        covered_by: "serve_parser_rejects_nonconforming_request_framing",
    },
    Requirement {
        id: "SERVE-TRANSPORT-008",
        level: RequirementLevel::Must,
        clause: "auth-gated endpoints return 401 before dispatch and never expose token material",
        covered_by: "serve_transport_rejects_missing_auth_with_endpoint_error_envelope",
    },
    Requirement {
        id: "SERVE-TRANSPORT-009",
        level: RequirementLevel::Must,
        clause: "JSON transport responses set exact Content-Length, no-store, and close",
        covered_by: "serve_json_transport_response_pins_headers_and_body_schema",
    },
    Requirement {
        id: "SERVE-TRANSPORT-010",
        level: RequirementLevel::Must,
        clause: "events endpoint emits a terminal text/event-stream frame with endpoint metadata",
        covered_by: "serve_events_transport_emits_terminal_sse_endpoint_envelope",
    },
    Requirement {
        id: "SERVE-TRANSPORT-011",
        level: RequirementLevel::Should,
        clause: "valid parser fixtures identify their endpoint without invoking handlers",
        covered_by: "serve_parser_accepts_minimal_valid_endpoint_fixtures",
    },
];

struct RejectionFixture {
    id: &'static str,
    raw: &'static [u8],
    expected_message: &'static str,
    limits: Option<ServeLimits>,
}

fn ensure<T>(actual: T, expected: T, label: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label}: expected {expected:?}, got {actual:?}"))
    }
}

fn split_http_response(response: &str) -> Result<(&str, &str), String> {
    response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "HTTP response missing header/body separator".to_owned())
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}: ");
    headers
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
}

fn parse_json_body(response: &str) -> Result<JsonValue, String> {
    let (_, body) = split_http_response(response)?;
    serde_json::from_str(body).map_err(|error| format!("response body is not JSON: {error}"))
}

fn sse_event_json(body: &str) -> Result<JsonValue, String> {
    let data_line = body
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .ok_or_else(|| "SSE frame missing data line".to_owned())?;
    serde_json::from_str(data_line).map_err(|error| format!("SSE data is not JSON: {error}"))
}

#[test]
fn serve_transport_conformance_matrix_covers_required_clauses() -> TestResult {
    ensure(REQUIREMENTS.len(), 11, "requirement count")?;
    let must_count = REQUIREMENTS
        .iter()
        .filter(|requirement| requirement.level == RequirementLevel::Must)
        .count();
    ensure(must_count, 10, "MUST clause count")?;

    for requirement in REQUIREMENTS {
        ensure(
            requirement.id.starts_with("SERVE-TRANSPORT-"),
            true,
            requirement.id,
        )?;
        ensure(
            requirement.covered_by.starts_with("serve_"),
            true,
            requirement.id,
        )?;
        ensure(requirement.clause.is_empty(), false, requirement.id)?;
    }
    Ok(())
}

#[test]
fn serve_parser_rejects_nonconforming_request_framing() -> TestResult {
    let tiny_body = ServeLimits {
        max_body_bytes: 4,
        ..ServeLimits::default()
    };
    let fixtures = [
        RejectionFixture {
            id: "tab-request-line",
            raw: b"GET\t/v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            expected_message: "single-space separators",
            limits: None,
        },
        RejectionFixture {
            id: "unsupported-method",
            raw: b"PUT /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            expected_message: "Only GET and POST",
            limits: None,
        },
        RejectionFixture {
            id: "relative-target",
            raw: b"GET v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            expected_message: "absolute path",
            limits: None,
        },
        RejectionFixture {
            id: "duplicate-content-length",
            raw: b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            expected_message: "appears more than once",
            limits: None,
        },
        RejectionFixture {
            id: "chunked-coding-list",
            raw: b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: identity, chunked; q=1\r\nContent-Length: 0\r\n\r\n",
            expected_message: "Chunked uploads are not accepted",
            limits: None,
        },
        RejectionFixture {
            id: "post-without-content-length",
            raw: b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            expected_message: "explicit Content-Length",
            limits: None,
        },
        RejectionFixture {
            id: "body-over-limit",
            raw: b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 5\r\n\r\n12345",
            expected_message: "body exceeds",
            limits: Some(tiny_body),
        },
        RejectionFixture {
            id: "short-body",
            raw: b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{",
            expected_message: "shorter than Content-Length",
            limits: None,
        },
        RejectionFixture {
            id: "extra-keepalive-bytes",
            raw: b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\nX",
            expected_message: "bytes beyond Content-Length",
            limits: None,
        },
    ];

    for fixture in fixtures {
        let limits = fixture.limits.unwrap_or_default();
        let error = parse_serve_http_request(fixture.raw, &limits)
            .expect_err("nonconforming fixture should fail closed");
        let message = error.to_string();
        ensure(message.contains(fixture.expected_message), true, fixture.id)?;
    }
    Ok(())
}

#[test]
fn serve_parser_accepts_minimal_valid_endpoint_fixtures() -> TestResult {
    let fixtures = [
        (
            b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".as_slice(),
            ServeEndpoint::Status,
            "/v1/status",
        ),
        (
            b"GET /v1/events HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".as_slice(),
            ServeEndpoint::Events,
            "/v1/events",
        ),
        (
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}"
                .as_slice(),
            ServeEndpoint::DurableWrite,
            "/v1/durable-write",
        ),
    ];

    for (raw, endpoint, path) in fixtures {
        let request = parse_serve_http_request(raw, &ServeLimits::default()).map_err(|error| {
            format!("valid {path} fixture should parse without transport error: {error}")
        })?;
        ensure(request.endpoint, endpoint, path)?;
        ensure(request.path.as_str(), path, path)?;
    }
    Ok(())
}

#[test]
fn serve_transport_rejects_missing_auth_with_endpoint_error_envelope() -> TestResult {
    let token = "01234567890123456789012345678901";
    let response = render_serve_transport_exchange(
        "req-conformance-auth",
        b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        &ServeLimits::default(),
        Some(token),
        23,
    );

    ensure(
        response.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        true,
        "auth status line",
    )?;
    ensure(
        response.contains(token),
        false,
        "auth response must not expose bearer token",
    )?;

    let envelope = parse_json_body(&response)?;
    ensure(
        envelope["schema"].as_str(),
        Some(SERVE_ENDPOINT_SCHEMA_V1),
        "serve endpoint envelope schema",
    )?;
    ensure(
        envelope["request"]["endpoint"].as_str(),
        Some("status"),
        "endpoint metadata",
    )?;
    ensure(
        envelope["request"]["auth"]["state"].as_str(),
        Some("missing"),
        "auth state",
    )?;
    ensure(
        envelope["response"]["statusCode"].as_u64(),
        Some(401),
        "payload status",
    )?;
    ensure(
        envelope["response"]["payload"]["schema"].as_str(),
        Some("ee.error.v2"),
        "wrapped error schema",
    )
}

#[test]
fn serve_json_transport_response_pins_headers_and_body_schema() -> TestResult {
    let payload = json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {"ok": true},
        "degraded": []
    });
    let response = render_serve_http_json_response(200, &payload);

    let (headers, body) = split_http_response(&response)?;
    ensure(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        true,
        "status line",
    )?;
    ensure(
        header_value(headers, "Content-Type"),
        Some("application/json; charset=utf-8"),
        "content type",
    )?;
    ensure(
        header_value(headers, "Cache-Control"),
        Some("no-store"),
        "cache",
    )?;
    ensure(header_value(headers, "Connection"), Some("close"), "close")?;
    let content_length = header_value(headers, "Content-Length")
        .ok_or_else(|| "missing Content-Length".to_owned())?
        .parse::<usize>()
        .map_err(|error| error.to_string())?;
    ensure(content_length, body.len(), "exact Content-Length")?;

    let body_json: JsonValue =
        serde_json::from_str(body).map_err(|error| format!("body JSON parse failed: {error}"))?;
    ensure(
        body_json["schema"].as_str(),
        Some("ee.response.v2"),
        "body schema",
    )
}

#[test]
fn serve_events_transport_emits_terminal_sse_endpoint_envelope() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/events HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-conformance-events",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        31,
    );

    let (headers, body) = split_http_response(&response)?;
    ensure(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        true,
        "SSE status line",
    )?;
    ensure(
        header_value(headers, "Content-Type"),
        Some("text/event-stream; charset=utf-8"),
        "SSE content type",
    )?;
    ensure(
        header_value(headers, "Content-Length").is_none(),
        true,
        "SSE omits Content-Length",
    )?;
    ensure(
        body.starts_with("event: complete\n") || body.starts_with("event: error\n"),
        true,
        "SSE terminal event kind",
    )?;

    let event = sse_event_json(body)?;
    ensure(
        event["schema"].as_str(),
        Some(SERVE_ENDPOINT_SCHEMA_V1),
        "SSE endpoint envelope schema",
    )?;
    ensure(
        event["request"]["endpoint"].as_str(),
        Some("events"),
        "SSE endpoint metadata",
    )?;
    ensure(
        event["request"]["auth"]["state"].as_str(),
        Some("accepted"),
        "SSE auth state",
    )?;
    ensure(
        event["sse"]["terminal"].as_bool(),
        Some(true),
        "SSE terminal marker",
    )?;
    ensure(
        event["sse"]["readOnly"].as_bool(),
        Some(true),
        "SSE read-only marker",
    )?;
    let payload_schema = event["response"]["payload"]["schema"].as_str();
    ensure(
        matches!(payload_schema, Some("ee.response.v2" | "ee.error.v2")),
        true,
        "SSE payload schema",
    )
}
