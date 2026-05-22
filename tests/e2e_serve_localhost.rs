use ee::serve::{SERVE_ENDPOINT_SCHEMA_V1, ServeLimits, render_serve_transport_exchange};
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

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
