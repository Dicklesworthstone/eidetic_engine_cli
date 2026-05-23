use ee::serve::{SERVE_ENDPOINT_SCHEMA_V1, ServeLimits, render_serve_transport_exchange};
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

// bd-2bw8m: endpoint-level regression coverage for serve query percent decoding.
fn split_http_response(response: &str) -> Result<(&str, &str), String> {
    response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "HTTP response missing header/body separator".to_owned())
}

fn response_body_json(response: &str) -> Result<JsonValue, String> {
    let (_, body) = split_http_response(response)?;
    serde_json::from_str(body).map_err(|error| error.to_string())
}

fn json_string_array(value: &JsonValue) -> Result<Vec<&str>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("expected array, got {value}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .ok_or_else(|| format!("expected string item, got {item}"))
        })
        .collect()
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (header_name, value) = line.split_once(": ")?;
        header_name.eq_ignore_ascii_case(name).then_some(value)
    })
}

fn response_sse_data_json(response: &str) -> Result<JsonValue, String> {
    let (headers, body) = split_http_response(response)?;
    if header_value(headers, "Content-Type") != Some("text/event-stream; charset=utf-8") {
        return Err(format!("expected SSE content type, got {headers}"));
    }
    if header_value(headers, "Content-Length").is_some() {
        return Err(format!(
            "SSE response must omit Content-Length, got {headers}"
        ));
    }

    let event_lines = body
        .lines()
        .filter(|line| line.starts_with("event: "))
        .collect::<Vec<_>>();
    if event_lines != vec!["event: header"] {
        return Err(format!(
            "expected exactly one SSE header frame, got body {body:?}"
        ));
    }

    let data_lines = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>();
    if data_lines.len() != 1 {
        return Err(format!("expected exactly one SSE data line, got {body:?}"));
    }

    serde_json::from_str(data_lines[0]).map_err(|error| error.to_string())
}

#[test]
fn serve_search_invalid_utf8_query_returns_clean_error_envelope() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/search?q=%FF HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-invalid-query-utf8",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );

    if !response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
        return Err(format!("expected 400 response, got {response}"));
    }
    if response.contains('\u{00ff}') || response.contains('\u{fffd}') {
        return Err(format!(
            "invalid UTF-8 response leaked mojibake: {response}"
        ));
    }

    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some("ee.error.v2"));
    assert_eq!(envelope["error"]["code"].as_str(), Some("usage"));
    assert_eq!(envelope["error"]["severity"].as_str(), Some("low"));
    assert_eq!(
        envelope["error"]["message"].as_str(),
        Some("Percent-decoded query value is not valid UTF-8.")
    );
    Ok(())
}

#[test]
fn serve_search_transport_decodes_utf8_query_before_handler_dispatch() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/search?q=%E2%9C%93 HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-search-utf8",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );

    if !response.starts_with("HTTP/1.1 200 OK\r\n") {
        return Err(format!("expected 200 response, got {response}"));
    }

    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("search"));
    assert_eq!(
        envelope["response"]["payload"]["data"]["dispatchPlan"]["handlerSurface"].as_str(),
        Some("cli.search")
    );
    assert_eq!(
        envelope["response"]["payload"]["data"]["businessLogicExecuted"].as_bool(),
        Some(false)
    );
    let argv =
        json_string_array(&envelope["response"]["payload"]["data"]["dispatchPlan"]["cliArgv"])?;
    assert_eq!(argv, vec!["ee", "search", "\u{2713}", "--json"]);
    Ok(())
}

#[test]
fn serve_events_endpoint_returns_single_sse_header_frame() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/events HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-events-e2e",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );

    if !response.starts_with("HTTP/1.1 200 OK\r\n") {
        return Err(format!("expected 200 response, got {response}"));
    }

    let event = response_sse_data_json(&response)?;
    assert_eq!(event["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(event["request"]["endpoint"].as_str(), Some("events"));
    assert_eq!(event["sse"]["eventKind"].as_str(), Some("header"));
    assert_eq!(event["sse"]["terminal"].as_bool(), Some(false));
    let payload = &event["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(payload["success"].as_bool(), Some(true));
    assert_eq!(
        payload["data"]["execution"].as_str(),
        Some("transport_only")
    );
    assert_eq!(
        payload["data"]["businessLogicExecuted"].as_bool(),
        Some(false)
    );
    assert_eq!(
        payload["data"]["dispatchPlan"]["handlerSurface"].as_str(),
        Some("serve.sse.events")
    );
    let argv = json_string_array(&payload["data"]["dispatchPlan"]["cliArgv"])?;
    assert!(argv.is_empty(), "events dispatch must not expose CLI argv");
    Ok(())
}

#[test]
fn serve_why_endpoint_routes_memory_id_to_cli_dispatch_plan() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/why/mem_00000000000000000000000001 HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-why-e2e",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );

    if !response.starts_with("HTTP/1.1 200 OK\r\n") {
        return Err(format!("expected 200 response, got {response}"));
    }

    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("why"));
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.response.v2")
    );
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(
        payload["data"]["dispatchPlan"]["handlerSurface"].as_str(),
        Some("cli.why")
    );
    let argv = json_string_array(&payload["data"]["dispatchPlan"]["cliArgv"])?;
    assert_eq!(
        argv,
        vec!["ee", "why", "mem_00000000000000000000000001", "--json"]
    );
    Ok(())
}

#[test]
fn serve_why_endpoint_rejects_empty_or_nested_memory_id_path_segments() -> TestResult {
    let token = "01234567890123456789012345678901";
    let usage_message = "GET /v1/why/{memory_id} requires exactly one memory ID path segment.";

    for (request_id, target) in [
        ("req-why-empty-e2e", "/v1/why/"),
        ("req-why-nested-e2e", "/v1/why/mem_1/extra"),
    ] {
        let raw = format!(
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        let response = render_serve_transport_exchange(
            request_id,
            raw.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            0,
        );

        if !response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
            return Err(format!(
                "expected 400 response for {target}, got {response}"
            ));
        }
        let envelope = response_body_json(&response)?;
        assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
        assert_eq!(envelope["request"]["endpoint"].as_str(), Some("why"));
        assert_eq!(
            envelope["response"]["payloadSchema"].as_str(),
            Some("ee.error.v2")
        );
        let payload = &envelope["response"]["payload"];
        assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
        assert_eq!(payload["error"]["code"].as_str(), Some("usage"));
        assert_eq!(payload["error"]["message"].as_str(), Some(usage_message));
    }

    Ok(())
}

// bd-3c3i5: GET /v1/status is the only ee serve v2 endpoint whose transport
// adapter crosses into a real subsystem (core::status::StatusReport::gather +
// output::render_status_json). Every other endpoint stops at a dispatch-plan
// placeholder with execution='not_started'. This test pins that the transport
// envelope actually carries the real ee.response.v2 status payload, proving
// the integration boundary in the absence of mocks.
#[test]
fn serve_status_endpoint_crosses_into_real_status_report_gather() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-status-real-gather",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );
    if !response.starts_with("HTTP/1.1 200 OK\r\n") {
        return Err(format!("expected 200 response, got {response}"));
    }
    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("status"));
    assert_eq!(envelope["request"]["path"].as_str(), Some("/v1/status"));
    assert_eq!(envelope["request"]["method"].as_str(), Some("GET"));
    assert_eq!(
        envelope["request"]["cliEquivalent"].as_str(),
        Some("ee status --json"),
    );
    assert_eq!(envelope["response"]["statusCode"].as_u64(), Some(200));
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.response.v2"),
    );
    // The payload must be the actual StatusReport JSON rendered through
    // output::render_status_json, not a transport-only placeholder. Confirm
    // by spot-checking fields that only the real renderer populates and that
    // the placeholder serve_dispatch_payload_json never emits.
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(payload["success"].as_bool(), Some(true));
    assert_eq!(payload["data"]["command"].as_str(), Some("status"));
    if payload["data"]["version"].as_str().is_none() {
        return Err(format!(
            "status payload must carry data.version from the real StatusReport; got {payload}"
        ));
    }
    if !payload["data"]["runtime"].is_object() {
        return Err(format!(
            "status payload must carry data.runtime object from real StatusReport; got {payload}"
        ));
    }
    if !payload["data"]["capabilities"].is_object() {
        return Err(format!(
            "status payload must carry data.capabilities object from real StatusReport; got {payload}"
        ));
    }
    if !payload["data"]["posture"].is_object() {
        return Err(format!(
            "status payload must carry data.posture object from real StatusReport; got {payload}"
        ));
    }
    // The placeholder transport payload would expose data.execution and
    // data.dispatchPlan; the real status payload must not.
    if payload["data"]["execution"].as_str() == Some("not_started")
        || payload["data"]["execution"].as_str() == Some("transport_only")
    {
        return Err(format!(
            "status endpoint must not return a placeholder transport payload; got {payload}"
        ));
    }
    if payload["data"]["dispatchPlan"].is_object() {
        return Err(format!(
            "status payload must not carry the transport-only dispatchPlan; got {payload}"
        ));
    }
    if !payload["data"]["businessLogicExecuted"].is_null() {
        return Err(format!(
            "status payload must not carry the transport-only businessLogicExecuted flag; got {payload}"
        ));
    }
    Ok(())
}

// bd-3c3i5: GET /v1/status without an Authorization header must trip the
// shared auth-failure envelope rather than ever reaching StatusReport::gather.
// Pins that the auth gate fires before any real subsystem dispatch.
#[test]
fn serve_status_endpoint_missing_auth_short_circuits_with_auth_failure_envelope() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".to_owned();
    let response = render_serve_transport_exchange(
        "req-status-missing-auth",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );
    if !response.starts_with("HTTP/1.1 401 Unauthorized\r\n") {
        return Err(format!("expected 401 response, got {response}"));
    }
    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("status"));
    assert_eq!(envelope["response"]["statusCode"].as_u64(), Some(401));
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.error.v2"),
    );
    let degraded_codes = json_string_array(&envelope["response"]["degradedCodes"])?;
    assert_eq!(degraded_codes, vec!["serve_auth_missing"]);
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
    assert_eq!(payload["error"]["code"].as_str(), Some("policy_denied"));
    assert_eq!(
        payload["error"]["details"]["authState"].as_str(),
        Some("missing"),
    );
    assert_eq!(
        payload["error"]["details"]["tokenMaterialExposed"].as_bool(),
        Some(false),
    );
    // The real StatusReport payload would carry data.command='status'. The
    // auth-failure envelope must not include it — proving the request short-
    // circuited before StatusReport::gather.
    if payload["data"]["command"].as_str() == Some("status") {
        return Err(format!(
            "auth-rejected response must NOT contain the real status payload; got {payload}"
        ));
    }
    Ok(())
}

// bd-3c3i5: GET /v1/status?extra=value must still route to the parameterless
// status endpoint. Query parameters on a parameterless endpoint are ignored
// at routing, and the real status payload must still come back. Pins that
// query bytes don't break path routing for status (an easily-broken
// parser-edge invariant under no-mock conditions).
#[test]
fn serve_status_endpoint_ignores_irrelevant_query_parameters() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/status?extra=value&other=1 HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-status-with-query",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );
    if !response.starts_with("HTTP/1.1 200 OK\r\n") {
        return Err(format!("expected 200 response, got {response}"));
    }
    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("status"));
    assert_eq!(envelope["request"]["path"].as_str(), Some("/v1/status"));
    assert_eq!(
        envelope["response"]["payload"]["data"]["command"].as_str(),
        Some("status"),
    );
    // Query map must reflect what came in even though the endpoint ignores
    // it — proving the parser parsed it and routing chose to ignore.
    let query = &envelope["request"]["query"];
    if !query.is_object() {
        return Err(format!("request.query must be an object; got {query}"));
    }
    if query["extra"].is_null() && query["other"].is_null() {
        return Err(format!(
            "request.query must surface the parsed query params; got {query}"
        ));
    }
    Ok(())
}

// bd-2niwj: GET /v1/context exercises the require_single_query_value path on
// the ?task= parameter — the same shared validator used by /v1/search. This
// transport-level test pins the positive route: a single non-empty
// percent-decoded task value flows through to the cli.context dispatch plan
// with the correct cliArgv shape and surface metadata.
#[test]
fn serve_context_endpoint_routes_single_task_to_cli_context_dispatch() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/context?task=plan%20a%20refactor HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-context-positive",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );
    if !response.starts_with("HTTP/1.1 200 OK\r\n") {
        return Err(format!("expected 200 response, got {response}"));
    }
    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("context"));
    assert_eq!(envelope["request"]["path"].as_str(), Some("/v1/context"));
    assert_eq!(
        envelope["request"]["cliEquivalent"].as_str(),
        Some("ee context \"<task>\" --json"),
    );
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.response.v2"),
    );
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(payload["success"].as_bool(), Some(true));
    assert_eq!(payload["data"]["execution"].as_str(), Some("not_started"),);
    assert_eq!(
        payload["data"]["businessLogicExecuted"].as_bool(),
        Some(false),
    );
    let plan = &payload["data"]["dispatchPlan"];
    assert_eq!(plan["handlerSurface"].as_str(), Some("cli.context"));
    assert_eq!(plan["mutable"].as_bool(), Some(false));
    assert_eq!(plan["sseStream"].as_bool(), Some(false));
    let argv = json_string_array(&plan["cliArgv"])?;
    assert_eq!(argv, vec!["ee", "context", "plan a refactor", "--json"]);
    Ok(())
}

// bd-2niwj: each negative branch of require_single_query_value at /v1/context
// has its own canonical usage message that downstream agents rely on for
// diagnostics. Drive the transport with the four shapes (missing, multi-
// value, empty value, whitespace-only value) and pin each exact message at
// the SERVE_ENDPOINT_SCHEMA_V1 boundary.
#[test]
fn serve_context_endpoint_rejects_invalid_task_query_with_canonical_messages() -> TestResult {
    let token = "01234567890123456789012345678901";
    let cases: [(&str, &str, &str); 4] = [
        (
            "req-context-missing",
            "/v1/context",
            "/v1/context requires a `task` query parameter.",
        ),
        (
            "req-context-multi",
            "/v1/context?task=a&task=b",
            "/v1/context requires exactly one `task` query parameter.",
        ),
        (
            "req-context-empty",
            "/v1/context?task=",
            "/v1/context requires a non-empty `task` query parameter.",
        ),
        (
            "req-context-whitespace",
            "/v1/context?task=%20%20",
            "/v1/context requires a non-empty `task` query parameter.",
        ),
    ];
    for (request_id, target, expected_message) in cases {
        let raw = format!(
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        let response = render_serve_transport_exchange(
            request_id,
            raw.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            0,
        );
        if !response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
            return Err(format!(
                "expected 400 response for {target}, got {response}"
            ));
        }
        let envelope = response_body_json(&response)?;
        assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
        assert_eq!(envelope["request"]["endpoint"].as_str(), Some("context"));
        assert_eq!(
            envelope["response"]["payloadSchema"].as_str(),
            Some("ee.error.v2"),
        );
        let payload = &envelope["response"]["payload"];
        assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
        assert_eq!(
            payload["error"]["code"].as_str(),
            Some("usage"),
            "case {target} expected usage error code, got {payload}"
        );
        assert_eq!(
            payload["error"]["message"].as_str(),
            Some(expected_message),
            "case {target} expected message {expected_message:?}, got {payload}"
        );
    }
    Ok(())
}
