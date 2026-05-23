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
