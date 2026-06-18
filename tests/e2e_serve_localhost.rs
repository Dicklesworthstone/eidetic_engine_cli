use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};

use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::serve::{SERVE_ENDPOINT_SCHEMA_V1, ServeLimits, render_serve_transport_exchange};
use serde_json::{Value as JsonValue, json};

#[path = "support/test_tracing.rs"]
mod test_tracing;

type TestResult = Result<(), String>;

// bd-2bw8m: endpoint-level regression coverage for serve query percent decoding.
fn current_dir_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct CurrentDirGuard<'a> {
    _guard: MutexGuard<'a, ()>,
    previous: PathBuf,
}

impl Drop for CurrentDirGuard<'_> {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

fn enter_current_dir(path: &Path) -> Result<CurrentDirGuard<'static>, String> {
    let guard = current_dir_test_lock()
        .lock()
        .map_err(|_| "current-dir test lock poisoned".to_owned())?;
    let previous =
        std::env::current_dir().map_err(|error| format!("read current directory: {error}"))?;
    std::env::set_current_dir(path)
        .map_err(|error| format!("set current directory to {}: {error}", path.display()))?;
    Ok(CurrentDirGuard {
        _guard: guard,
        previous,
    })
}

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
    if event_lines != vec!["event: complete"] && event_lines != vec!["event: error"] {
        return Err(format!(
            "expected exactly one terminal SSE completion/error frame, got body {body:?}"
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
fn serve_foreground_cli_accepts_one_real_status_request() -> TestResult {
    let _trace = test_tracing::init_test_tracing(
        "bd-3usjw.4",
        "serve_foreground_cli_accepts_one_real_status_request",
    );
    let token = "01234567890123456789012345678901";
    let mut child = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args([
            "serve",
            "--foreground",
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--json",
        ])
        .env("EE_SERVE_TOKEN", token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn ee serve foreground: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "serve child stdout was not piped".to_owned())?;
    let mut stdout_reader = BufReader::new(stdout);
    let mut startup_line = String::new();
    let startup_bytes = stdout_reader
        .read_line(&mut startup_line)
        .map_err(|error| format!("read serve startup line: {error}"))?;
    if startup_bytes == 0 {
        let _ = child.kill();
        let status = child
            .wait()
            .map_err(|error| format!("wait for failed serve child: {error}"))?;
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        return Err(format!(
            "serve child exited before startup JSON: status={status}, stderr={stderr}"
        ));
    }

    let startup: JsonValue = serde_json::from_str(startup_line.trim())
        .map_err(|error| format!("parse serve startup JSON: {error}; line={startup_line}"))?;
    assert_eq!(startup["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(startup["success"].as_bool(), Some(true));
    assert_eq!(
        startup["data"]["schema"].as_str(),
        Some(ee::serve::SERVE_STARTUP_SCHEMA_V1)
    );
    assert_eq!(
        startup["data"]["startup"]["readiness"]["state"].as_str(),
        Some("ready")
    );
    let port = startup["data"]["listener"]["boundPort"]
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .ok_or_else(|| format!("startup missing listener boundPort: {startup}"))?;
    if port == 0 {
        return Err(format!(
            "serve listener must expose an OS-assigned port: {startup}"
        ));
    }

    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|error| format!("connect to serve listener on port {port}: {error}"))?;
    let request = format!(
        "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("write serve request: {error}"))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| format!("shutdown serve request writer: {error}"))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("read serve response: {error}"))?;
    if !response.starts_with("HTTP/1.1 200 OK\r\n") {
        return Err(format!("expected 200 response, got {response}"));
    }
    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("status"));
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(payload["success"].as_bool(), Some(true));
    assert_eq!(payload["data"]["command"].as_str(), Some("status"));

    let status = child
        .wait()
        .map_err(|error| format!("wait for serve child: {error}"))?;
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        return Err(format!(
            "serve child failed: status={status}, stderr={stderr}"
        ));
    }

    let mut trailing_stdout = String::new();
    stdout_reader
        .read_to_string(&mut trailing_stdout)
        .map_err(|error| format!("read trailing serve stdout: {error}"))?;
    if !trailing_stdout.trim().is_empty() {
        return Err(format!(
            "serve foreground should emit one startup JSON line, got trailing stdout: {trailing_stdout}"
        ));
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr)
            .map_err(|error| format!("read serve stderr: {error}"))?;
    }
    if !stderr.trim().is_empty() {
        return Err(format!(
            "serve foreground stderr should be clean, got: {stderr}"
        ));
    }

    Ok(())
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

    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("search"));
    assert_eq!(
        envelope["request"]["query"]["q"][0].as_str(),
        Some("\u{2713}"),
        "request metadata must preserve decoded UTF-8 before dispatch"
    );
    let payload = &envelope["response"]["payload"];
    if response.starts_with("HTTP/1.1 200 OK\r\n") {
        assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
        assert_eq!(payload["success"].as_bool(), Some(true));
        assert_eq!(payload["data"]["command"].as_str(), Some("search"));
        assert_eq!(payload["data"]["query"].as_str(), Some("\u{2713}"));
    } else if response.starts_with("HTTP/1.1 500 Internal Server Error\r\n") {
        assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
        assert_eq!(payload["error"]["code"].as_str(), Some("search_index"));
    } else {
        return Err(format!(
            "expected 200 or real search-index error, got {response}"
        ));
    }
    assert!(
        payload["data"]["businessLogicExecuted"].is_null(),
        "search endpoint must not return the old transport-only stub payload: {payload}"
    );
    Ok(())
}

#[test]
fn serve_events_endpoint_returns_terminal_subscribe_poll_frame() -> TestResult {
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
    assert_eq!(event["sse"]["terminal"].as_bool(), Some(true));
    let payload = &event["response"]["payload"];
    match event["sse"]["eventKind"].as_str() {
        Some("complete") => {
            assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
            assert_eq!(payload["success"].as_bool(), Some(true));
            assert_eq!(payload["data"]["command"].as_str(), Some("subscribe poll"));
            assert_eq!(
                payload["data"]["serve"]["handlerSurface"].as_str(),
                Some("serve.sse.events")
            );
            assert_eq!(payload["data"]["serve"]["readOnly"].as_bool(), Some(true));
            let argv = json_string_array(&payload["data"]["serve"]["dispatchPlan"]["cliArgv"])?;
            assert!(argv.is_empty(), "events dispatch must not expose CLI argv");
        }
        Some("error") => {
            assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
            assert_eq!(payload["error"]["code"].as_str(), Some("storage"));
        }
        other => {
            return Err(format!(
                "expected complete/error SSE event, got {other:?}: {event}"
            ));
        }
    }
    Ok(())
}

#[test]
fn serve_why_endpoint_attempts_real_memory_explanation() -> TestResult {
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

    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("why"));
    let payload = &envelope["response"]["payload"];
    if response.starts_with("HTTP/1.1 200 OK\r\n") {
        assert_eq!(
            envelope["response"]["payloadSchema"].as_str(),
            Some("ee.response.v2")
        );
        assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
        assert_eq!(payload["data"]["command"].as_str(), Some("why"));
        assert_eq!(
            payload["data"]["memoryId"].as_str(),
            Some("mem_00000000000000000000000001")
        );
    } else if response.starts_with("HTTP/1.1 404 Not Found\r\n")
        || response.starts_with("HTTP/1.1 500 Internal Server Error\r\n")
    {
        assert_eq!(
            envelope["response"]["payloadSchema"].as_str(),
            Some("ee.error.v2")
        );
        assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
        let code = payload["error"]["code"].as_str();
        assert!(
            matches!(code, Some("not_found" | "storage")),
            "why must fail through the real storage/explain path when unavailable, got {payload}"
        );
    } else {
        return Err(format!(
            "expected real why success/not-found/storage response, got {response}"
        ));
    }
    assert!(
        payload["data"]["businessLogicExecuted"].is_null(),
        "why endpoint must not return the old dispatch-plan-only stub payload: {payload}"
    );
    Ok(())
}

#[test]
fn serve_why_endpoint_returns_canonical_why_payload_shape() -> TestResult {
    let token = "01234567890123456789012345678901";
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let remembered = remember_memory(&RememberMemoryOptions {
        workspace_path: workspace.path(),
        database_path: None,
        content: "Serve why must preserve the canonical ee why JSON payload.",
        workflow_id: Some("serve-why-canonical"),
        level: "procedural",
        kind: "rule",
        tags: Some("serve,why,contract"),
        confidence: 0.91,
        source: Some("serve://e2e/why-canonical"),
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: false,
        propose_candidates: false,
    })
    .map_err(|error| error.to_string())?;
    let memory_id = remembered.memory_id.to_string();
    let _current_dir = enter_current_dir(workspace.path())?;
    let raw = format!(
        "GET /v1/why/{memory_id} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-why-canonical-payload",
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
        envelope["request"]["cliEquivalent"].as_str(),
        Some("ee why <memory-id> --json")
    );
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.response.v2")
    );
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(payload["success"].as_bool(), Some(true));

    let data = payload["data"]
        .as_object()
        .ok_or_else(|| format!("why payload data must be an object: {payload}"))?;
    assert_eq!(data.get("command").and_then(JsonValue::as_str), Some("why"));
    assert_eq!(
        data.get("memoryId").and_then(JsonValue::as_str),
        Some(memory_id.as_str())
    );
    assert_eq!(data.get("found").and_then(JsonValue::as_bool), Some(true));

    for key in [
        "storage",
        "retrieval",
        "graphRetrievalFeatures",
        "selection",
        "agentProfile",
        "bayesPosterior",
        "lifecycle",
        "contradictions",
        "links",
        "history",
        "verificationEvidence",
        "coordinationFallbackEvidence",
        "attestationBundle",
        "degraded",
    ] {
        if !data.contains_key(key) {
            return Err(format!(
                "/v1/why dropped canonical ee why field {key}: {payload}"
            ));
        }
    }

    let selection = data
        .get("selection")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| format!("why selection must preserve nested object shape: {payload}"))?;
    if selection.contains_key("latestPackSelectionPresent") {
        return Err(format!(
            "/v1/why must expose latestPackSelection, not a flattened presence flag: {payload}"
        ));
    }
    if !selection.contains_key("latestPackSelection") {
        return Err(format!(
            "/v1/why selection dropped latestPackSelection: {payload}"
        ));
    }
    assert!(
        payload["data"]["businessLogicExecuted"].is_null(),
        "why endpoint must not return the old dispatch-plan-only stub payload: {payload}"
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

// bd-3c3i5: GET /v1/status crosses into a real subsystem
// (core::status::StatusReport::gather + output::render_status_json). This test
// pins that the transport envelope carries the real ee.response.v2 status
// payload, proving the integration boundary in the absence of mocks.
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
// percent-decoded task value flows through to the cli.pack dispatch plan
// with the correct cliArgv shape and surface metadata.
#[test]
fn serve_context_endpoint_routes_single_task_to_cli_pack_dispatch() -> TestResult {
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
    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("context"));
    assert_eq!(envelope["request"]["path"].as_str(), Some("/v1/context"));
    assert_eq!(
        envelope["request"]["cliEquivalent"].as_str(),
        Some("ee pack \"<task>\" --json"),
    );
    let payload = &envelope["response"]["payload"];
    if response.starts_with("HTTP/1.1 200 OK\r\n") {
        assert_eq!(
            envelope["response"]["payloadSchema"].as_str(),
            Some("ee.response.v2")
        );
        assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
        assert_eq!(payload["success"].as_bool(), Some(true));
        assert_eq!(payload["data"]["command"].as_str(), Some("pack"));
        assert_eq!(
            payload["data"]["request"]["query"].as_str(),
            Some("plan a refactor")
        );
        assert!(
            payload["data"]["pack"].is_object(),
            "pack payload missing pack object: {payload}"
        );
    } else if response.starts_with("HTTP/1.1 500 Internal Server Error\r\n") {
        assert_eq!(
            envelope["response"]["payloadSchema"].as_str(),
            Some("ee.error.v2")
        );
        assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
        assert_eq!(payload["error"]["code"].as_str(), Some("storage"));
    } else {
        return Err(format!(
            "expected 200 or real pack storage error, got {response}"
        ));
    }
    assert!(
        payload["data"]["businessLogicExecuted"].is_null(),
        "context endpoint must not return the old dispatch-plan-only stub payload: {payload}"
    );
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

// bd-o8w0t: GET /v1/doctor is one of two remaining parameterless read-only
// endpoints with no transport-level E2E coverage. Pin its read_only_cli_dispatch
// shape end-to-end through render_serve_transport_exchange.
#[test]
fn serve_doctor_endpoint_routes_to_cli_doctor_dispatch() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/doctor HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-doctor-dispatch",
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
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("doctor"));
    assert_eq!(envelope["request"]["path"].as_str(), Some("/v1/doctor"));
    assert_eq!(envelope["request"]["method"].as_str(), Some("GET"));
    assert_eq!(
        envelope["request"]["cliEquivalent"].as_str(),
        Some("ee doctor --json"),
    );
    assert_eq!(envelope["response"]["statusCode"].as_u64(), Some(200));
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.response.v2"),
    );
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(payload["success"].as_bool(), Some(true));
    assert_eq!(payload["data"]["command"].as_str(), Some("doctor"));
    assert_eq!(payload["fields"].as_str(), Some("doctor_concise"));
    assert_eq!(payload["data"]["mode"].as_str(), Some("concise"));
    assert!(
        payload["data"]["coreChecks"].is_array(),
        "doctor payload must carry real compact core checks: {payload}"
    );
    assert!(
        payload["data"]["advisorySummary"].is_object(),
        "doctor payload must carry the compact advisory summary: {payload}"
    );
    assert!(
        payload["data"]["checks"].is_null(),
        "default doctor endpoint must not return the exhaustive checks firehose: {payload}"
    );
    assert!(
        payload["data"]["businessLogicExecuted"].is_null(),
        "doctor endpoint must not return the old dispatch-plan-only stub payload: {payload}"
    );
    Ok(())
}

// bd-o8w0t: GET /v1/swarm/brief is the only multi-segment v1 endpoint after
// /v1/why/{id}. Its path is the literal '/v1/swarm/brief' (no path params),
// so request.path must round-trip unchanged through the transport metadata.
// Pin the cli.swarm.brief dispatch contract and verify the multi-segment
// path is preserved verbatim — guarding against any future refactor that
// might confuse it with the /v1/why path-segment matcher.
#[test]
fn serve_swarm_brief_endpoint_routes_multi_segment_path_to_cli_dispatch() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/swarm/brief HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-swarm-brief-dispatch",
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
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("swarmBrief"),);
    assert_eq!(
        envelope["request"]["path"].as_str(),
        Some("/v1/swarm/brief"),
    );
    assert_eq!(
        envelope["request"]["cliEquivalent"].as_str(),
        Some("ee swarm brief --json"),
    );
    assert_eq!(envelope["response"]["statusCode"].as_u64(), Some(200));
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.response.v2"),
    );
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(payload["success"].as_bool(), Some(true));
    assert_eq!(
        payload["data"]["schema"].as_str(),
        Some("ee.swarm.brief.v1")
    );
    assert!(
        payload["data"]["sources"].is_array(),
        "swarm brief payload must carry real sources array: {payload}"
    );
    assert!(
        payload["data"]["businessLogicExecuted"].is_null(),
        "swarm brief endpoint must not return the old dispatch-plan-only stub payload: {payload}"
    );
    Ok(())
}

// bd-rpaqi: serve_token_posture posture.state='weak' fires before
// serve_auth_state can examine the request. The transport must short-circuit
// to a 401 with response.degradedCodes carrying 'serve_auth_weak_token' even
// when the client sends a syntactically-valid Authorization header. Pin this
// so any future refactor that accidentally promotes weak tokens to accepted
// status is caught at the transport boundary.
#[test]
fn serve_status_endpoint_with_weak_server_token_short_circuits_with_weak_degraded_code()
-> TestResult {
    let weak_token = "tooshort"; // 8 bytes -> 64 bits, far below 256-bit minimum
    let raw = format!(
        "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {weak_token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-status-weak-token",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(weak_token),
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
    assert_eq!(degraded_codes, vec!["serve_auth_weak_token"]);
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
    assert_eq!(payload["error"]["code"].as_str(), Some("policy_denied"));
    assert_eq!(
        payload["error"]["details"]["authState"].as_str(),
        Some("weak"),
    );
    assert_eq!(
        payload["error"]["details"]["tokenMaterialExposed"].as_bool(),
        Some(false),
    );
    Ok(())
}

// bd-rpaqi: client presents a syntactically-valid Authorization Bearer header
// whose value does NOT match the server's configured token. serve_auth_state
// must distinguish this from 'missing' by returning 'rejected', and the
// degraded code must surface as 'serve_auth_rejected' so operators can
// distinguish a misconfigured client from a missing header in audit logs.
#[test]
fn serve_status_endpoint_with_mismatched_token_short_circuits_with_rejected_degraded_code()
-> TestResult {
    let server_token = "01234567890123456789012345678901";
    let client_token = "wrongtokenthatissamelengthbutbad";
    assert_eq!(
        client_token.len(),
        server_token.len(),
        "client token should be same length as server token to isolate the mismatch path"
    );
    let raw = format!(
        "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {client_token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-status-rejected-token",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(server_token),
        0,
    );
    if !response.starts_with("HTTP/1.1 401 Unauthorized\r\n") {
        return Err(format!("expected 401 response, got {response}"));
    }
    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("status"));
    assert_eq!(envelope["response"]["statusCode"].as_u64(), Some(401));
    let degraded_codes = json_string_array(&envelope["response"]["degradedCodes"])?;
    assert_eq!(degraded_codes, vec!["serve_auth_rejected"]);
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
    assert_eq!(payload["error"]["code"].as_str(), Some("policy_denied"));
    assert_eq!(
        payload["error"]["details"]["authState"].as_str(),
        Some("rejected"),
    );
    // The mismatched token bytes must NOT leak into the rendered response;
    // serve_auth_failure_envelope sets tokenMaterialExposed=false as a
    // structural promise.
    assert_eq!(
        payload["error"]["details"]["tokenMaterialExposed"].as_bool(),
        Some(false),
    );
    if response.contains(client_token) {
        return Err(
            "rejected-token response must NOT contain the mismatched token bytes".to_owned(),
        );
    }
    Ok(())
}

// bd-rpaqi: any GET /v1/<unknown-path> reaches the dispatch table per bd-da9h1
// (auth gate only fires for endpoints with auth_required()=true, and Unknown
// declares auth_required()=false). The transport adapter must answer with a
// 404 carrying the SERVE_ENDPOINT_SCHEMA_V1 envelope, request.endpoint='unknown',
// and the canonical 'No ee serve v2 endpoint is registered for GET <path>.'
// usage message — so endpoint-discovery errors aren't masked by auth failures.
#[test]
fn serve_unknown_endpoint_returns_404_with_endpoint_discovery_error() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "GET /v1/nonexistent-endpoint HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-unknown-endpoint",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );
    if !response.starts_with("HTTP/1.1 404 Not Found\r\n") {
        return Err(format!("expected 404 response, got {response}"));
    }
    let envelope = response_body_json(&response)?;
    assert_eq!(envelope["schema"].as_str(), Some(SERVE_ENDPOINT_SCHEMA_V1));
    assert_eq!(envelope["request"]["endpoint"].as_str(), Some("unknown"));
    assert_eq!(envelope["request"]["method"].as_str(), Some("GET"));
    assert_eq!(
        envelope["request"]["path"].as_str(),
        Some("/v1/nonexistent-endpoint"),
    );
    assert_eq!(envelope["response"]["statusCode"].as_u64(), Some(404));
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.error.v2"),
    );
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.error.v2"));
    assert_eq!(payload["error"]["code"].as_str(), Some("usage"));
    assert_eq!(
        payload["error"]["message"].as_str(),
        Some("No ee serve v2 endpoint is registered for GET /v1/nonexistent-endpoint."),
    );
    Ok(())
}

// bd-3sqfi: POST /v1/durable-write is the only mutable endpoint in the v2
// surface. Its first concrete operation is a narrow `remember` write that
// routes through ee's existing memory service instead of returning the old
// transport-only dispatch envelope.
#[test]
fn serve_durable_write_endpoint_remembers_memory_with_audited_handler() -> TestResult {
    let token = "01234567890123456789012345678901";
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let body = json!({
        "operation": "remember",
        "workspace": workspace.path().display().to_string(),
        "content": "Serve durable-write remembered this fact.",
        "level": "episodic",
        "kind": "fact",
        "tags": ["serve", "durable-write"],
        "confidence": 0.73,
        "source": "serve://e2e/durable-write",
        "autoLink": false,
        "proposeCandidates": false
    })
    .to_string();
    let raw = format!(
        "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let response = render_serve_transport_exchange(
        "req-durable-write-remember",
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
    assert_eq!(
        envelope["request"]["endpoint"].as_str(),
        Some("durableWrite"),
    );
    assert_eq!(
        envelope["request"]["path"].as_str(),
        Some("/v1/durable-write"),
    );
    assert_eq!(envelope["request"]["method"].as_str(), Some("POST"));
    // DurableWrite has no CLI equivalent (yet) — cliEquivalent must be null.
    if !envelope["request"]["cliEquivalent"].is_null() {
        return Err(format!(
            "/v1/durable-write must not expose a CLI equivalent; got {envelope}"
        ));
    }
    // POST endpoints carry contentLengthRequired=true.
    assert_eq!(
        envelope["request"]["contentLengthRequired"].as_bool(),
        Some(true),
    );
    assert_eq!(
        envelope["request"]["bodyBytes"].as_u64(),
        Some(body.len() as u64),
    );
    assert_eq!(envelope["response"]["statusCode"].as_u64(), Some(200));
    assert_eq!(
        envelope["response"]["payloadSchema"].as_str(),
        Some("ee.response.v2"),
    );
    let payload = &envelope["response"]["payload"];
    assert_eq!(payload["schema"].as_str(), Some("ee.response.v2"));
    assert_eq!(payload["success"].as_bool(), Some(true));
    assert_eq!(payload["data"]["execution"].as_str(), Some("executed"),);
    assert_eq!(
        payload["data"]["executionBoundary"].as_str(),
        Some("serve_durable_write"),
    );
    assert_eq!(
        payload["data"]["businessLogicExecuted"].as_bool(),
        Some(true),
    );
    assert_eq!(payload["data"]["operation"].as_str(), Some("remember"));
    assert_eq!(
        payload["data"]["handlerSurface"].as_str(),
        Some("serve.durable_write.remember"),
    );
    let plan = &payload["data"]["dispatchPlan"];
    assert_eq!(plan["endpoint"].as_str(), Some("durableWrite"));
    assert_eq!(plan["handlerSurface"].as_str(), Some("serve.durable_write"));
    // mutable=true is the structural promise that downstream agents inspect
    // before treating the response as a write-effect result.
    assert_eq!(plan["mutable"].as_bool(), Some(true));
    assert_eq!(plan["sseStream"].as_bool(), Some(false));
    let argv = json_string_array(&plan["cliArgv"])?;
    if !argv.is_empty() {
        return Err(format!(
            "durable-write handler must expose no CLI argv: {argv:?}"
        ));
    }
    let result = &payload["data"]["result"];
    assert_eq!(result["command"].as_str(), Some("remember"));
    assert_eq!(
        result["content"].as_str(),
        Some("Serve durable-write remembered this fact."),
    );
    assert_eq!(result["level"].as_str(), Some("episodic"));
    assert_eq!(result["kind"].as_str(), Some("fact"));
    assert_eq!(result["persisted"].as_bool(), Some(true));
    assert_eq!(result["dryRun"].as_bool(), Some(false));
    assert_eq!(result["redactionStatus"].as_str(), Some("accepted"));
    if result["memoryId"].as_str().is_none() {
        return Err(format!(
            "durable remember must return a memoryId: {payload}"
        ));
    }
    if result["auditId"].as_str().is_none() {
        return Err(format!(
            "durable remember must return an auditId: {payload}"
        ));
    }
    if result["indexJobId"].as_str().is_none() {
        return Err(format!(
            "durable remember must return an indexJobId: {payload}"
        ));
    }
    Ok(())
}

#[test]
fn serve_durable_write_endpoint_payload_errors_return_usage_400_envelope() -> TestResult {
    let token = "01234567890123456789012345678901";
    let empty_body = format!(
        "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\n\r\n"
    );
    let empty_response = render_serve_transport_exchange(
        "req-durable-write-empty-body",
        empty_body.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );
    if !empty_response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
        return Err(format!(
            "empty durable-write body must return 400, got {empty_response}"
        ));
    }
    let empty_envelope = response_body_json(&empty_response)?;
    assert_eq!(empty_envelope["response"]["statusCode"].as_u64(), Some(400));
    assert_eq!(
        empty_envelope["response"]["payloadSchema"].as_str(),
        Some("ee.error.v2"),
    );
    let empty_payload = &empty_envelope["response"]["payload"];
    assert_eq!(empty_payload["schema"].as_str(), Some("ee.error.v2"));
    assert_eq!(empty_payload["error"]["code"].as_str(), Some("usage"));

    let unsupported_body = json!({
        "operation": "replace",
        "workspace": "/tmp/ee-serve-durable-write",
        "content": "unsupported operation"
    })
    .to_string();
    let unsupported_raw = format!(
        "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{unsupported_body}",
        unsupported_body.len()
    );
    let unsupported_response = render_serve_transport_exchange(
        "req-durable-write-unsupported-operation",
        unsupported_raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );
    if !unsupported_response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
        return Err(format!(
            "unsupported durable-write operation must return 400, got {unsupported_response}"
        ));
    }
    let unsupported_envelope = response_body_json(&unsupported_response)?;
    assert_eq!(
        unsupported_envelope["response"]["statusCode"].as_u64(),
        Some(400),
    );
    assert_eq!(
        unsupported_envelope["response"]["payloadSchema"].as_str(),
        Some("ee.error.v2"),
    );
    let unsupported_payload = &unsupported_envelope["response"]["payload"];
    assert_eq!(unsupported_payload["schema"].as_str(), Some("ee.error.v2"),);
    assert_eq!(
        unsupported_payload["error"]["code"].as_str(),
        Some("serve_durable_write_unsupported_operation"),
    );
    Ok(())
}

// bd-3uvoo: POST /v1/durable-write without an Authorization header must
// short-circuit through the shared serve_auth_failure_envelope (same code
// path validated for /v1/status in bd-3c3i5) but with the durable-write
// endpoint identity preserved in request.endpoint and request metadata.
#[test]
fn serve_durable_write_endpoint_missing_auth_short_circuits_with_auth_failure_envelope()
-> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\n"
        .to_owned();
    let response = render_serve_transport_exchange(
        "req-durable-write-missing-auth",
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
    assert_eq!(
        envelope["request"]["endpoint"].as_str(),
        Some("durableWrite"),
    );
    assert_eq!(envelope["request"]["method"].as_str(), Some("POST"));
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
    // The durable-write handler must NOT run for auth-rejected POSTs — the
    // mutable=true flag from the dispatch plan would otherwise mislead a
    // downstream observer into believing a write was performed.
    if payload["data"]["dispatchPlan"]["mutable"].as_bool() == Some(true) {
        return Err(format!(
            "auth-missing POST /v1/durable-write must NOT surface the mutable dispatch plan; got {payload}"
        ));
    }
    Ok(())
}

// bd-tujpb: parse_serve_http_request has four additional pre-parse rejection
// branches not covered by bd-17386/bd-2e5g1 — header byte limit overflow,
// declared body byte limit overflow, non-integer Content-Length, and
// relative request target (no leading '/'). Each branch produces 400 with
// the flat ee.error.v2 envelope. The limit branches use custom ServeLimits
// to keep the test cheap rather than synthesizing megabyte-sized requests.
#[test]
fn serve_limit_and_content_length_violations_rejected_with_canonical_400_messages() -> TestResult {
    let token = "01234567890123456789012345678901";

    // Tight limits keep the malformed-request fixtures small enough to read
    // at a glance while still exercising the >limit code paths.
    let tight_limits = ee::serve::ServeLimits {
        max_header_bytes: 256,
        max_body_bytes: 64,
        ..ee::serve::ServeLimits::default()
    };

    // (a) Header bytes exceed max_header_bytes: pad an X-Pad header so the
    // total header section is well over 256 bytes before the \r\n\r\n
    // terminator.
    let bloated_pad = "A".repeat(400);
    let header_overflow_raw = format!(
        "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nX-Pad: {bloated_pad}\r\n\r\n"
    );
    // (b) Declared body bytes exceed max_body_bytes: Content-Length larger
    // than 64 (tight max). Body bytes can be empty since the check fires on
    // the declared length, not the actual byte count.
    let body_overflow_raw = format!(
        "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: 1000\r\n\r\n"
    );
    // (c) Content-Length is not a non-negative integer.
    let bad_content_length_raw = format!(
        "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: abc\r\n\r\n"
    );
    // (d) Target is a relative path (no leading '/').
    let relative_target_raw = format!(
        "GET v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
    );

    let cases: [(&str, String, &str); 4] = [
        (
            "req-limits-header-overflow",
            header_overflow_raw,
            "HTTP request headers exceed the 256 byte limit.",
        ),
        (
            "req-limits-body-overflow",
            body_overflow_raw,
            "HTTP request body exceeds the 64 byte limit.",
        ),
        (
            "req-limits-content-length-not-integer",
            bad_content_length_raw,
            "Content-Length must be a non-negative integer.",
        ),
        (
            "req-limits-relative-target",
            relative_target_raw,
            "HTTP request target must be an absolute path.",
        ),
    ];

    for (request_id, raw, expected_message) in cases {
        let response = render_serve_transport_exchange(
            request_id,
            raw.as_bytes(),
            &tight_limits,
            Some(token),
            0,
        );
        if !response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
            return Err(format!(
                "case {request_id} expected 400 response, got {response}"
            ));
        }
        let envelope = response_body_json(&response)?;
        assert_eq!(
            envelope["schema"].as_str(),
            Some("ee.error.v2"),
            "case {request_id} expected flat ee.error.v2 envelope, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["code"].as_str(),
            Some("usage"),
            "case {request_id} expected usage error code, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["severity"].as_str(),
            Some("low"),
            "case {request_id} expected low severity for usage error, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["message"].as_str(),
            Some(expected_message),
            "case {request_id} expected message {expected_message:?}, got {envelope}"
        );
    }
    Ok(())
}

// bd-2e5g1: parse_serve_http_request has four pre-parse rejection branches
// for malformed HTTP/1.1 request shapes that don't even reach the
// method/path inspection: missing CRLF terminator, wrong HTTP version,
// header line missing ':' separator, and empty header name. All four
// produce 400 with the flat ee.error.v2 envelope (no exchange wrapper).
// Drive each with a real raw byte stream and pin the canonical message so
// future refactors can't accidentally swallow these branches into a panic
// or leak partial-parse state.
#[test]
fn serve_malformed_request_line_and_headers_rejected_with_canonical_400_messages() -> TestResult {
    let token = "01234567890123456789012345678901";
    let cases: [(&str, String, &str); 4] = [
        (
            "req-malformed-no-crlf-terminator",
            // No "\r\n\r\n" terminator — entire payload is one request line.
            "GET /v1/status HTTP/1.1".to_owned(),
            "HTTP request is missing the CRLF header terminator.",
        ),
        (
            "req-malformed-wrong-http-version",
            format!(
                "GET /v1/status HTTP/2.0\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
            ),
            "Only narrow HTTP/1.1 request lines are supported.",
        ),
        (
            "req-malformed-header-no-colon",
            format!(
                "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nBadHeaderNoColon foo bar\r\nAuthorization: Bearer {token}\r\n\r\n"
            ),
            "HTTP header line is missing ':' separator.",
        ),
        (
            "req-malformed-empty-header-name",
            format!(
                "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n: value\r\nAuthorization: Bearer {token}\r\n\r\n"
            ),
            "HTTP header name must not be empty.",
        ),
    ];
    for (request_id, raw, expected_message) in cases {
        let response = render_serve_transport_exchange(
            request_id,
            raw.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            0,
        );
        if !response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
            return Err(format!(
                "case {request_id} expected 400 response, got {response}"
            ));
        }
        let envelope = response_body_json(&response)?;
        assert_eq!(
            envelope["schema"].as_str(),
            Some("ee.error.v2"),
            "case {request_id} expected flat ee.error.v2 envelope, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["code"].as_str(),
            Some("usage"),
            "case {request_id} expected usage error code, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["severity"].as_str(),
            Some("low"),
            "case {request_id} expected low severity for usage error, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["message"].as_str(),
            Some(expected_message),
            "case {request_id} expected message {expected_message:?}, got {envelope}"
        );
    }
    Ok(())
}

// bd-17386: parse_serve_http_request enforces four HTTP-shape rules that
// fire before endpoint dispatch can run — chunked rejection, POST without
// Content-Length, body shorter than Content-Length, and body longer than
// Content-Length (no keepalive). All four return 400 with the flat
// ee.error.v2 envelope (NOT the SERVE_ENDPOINT_SCHEMA_V1 exchange wrapper)
// because the request itself was unparseable. Pin each canonical message.
#[test]
fn serve_http_shape_violations_rejected_with_canonical_400_messages() -> TestResult {
    let token = "01234567890123456789012345678901";
    let cases: [(&str, String, &str); 4] = [
        (
            "req-shape-chunked",
            format!(
                "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nTransfer-Encoding: chunked\r\n\r\n"
            ),
            "Chunked uploads are not accepted by the first ee serve v2 slice.",
        ),
        (
            "req-shape-post-no-content-length",
            format!(
                "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
            ),
            "POST requests must include an explicit Content-Length.",
        ),
        (
            "req-shape-body-shorter",
            format!(
                "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: 10\r\n\r\nshort"
            ),
            "HTTP request body is shorter than Content-Length.",
        ),
        (
            "req-shape-body-longer",
            format!(
                "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\n\r\nextra"
            ),
            "HTTP request body contains bytes beyond Content-Length; keepalive is not supported.",
        ),
    ];
    for (request_id, raw, expected_message) in cases {
        let response = render_serve_transport_exchange(
            request_id,
            raw.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            0,
        );
        if !response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
            return Err(format!(
                "case {request_id} expected 400 response, got {response}"
            ));
        }
        let envelope = response_body_json(&response)?;
        assert_eq!(
            envelope["schema"].as_str(),
            Some("ee.error.v2"),
            "case {request_id} expected flat ee.error.v2 envelope, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["code"].as_str(),
            Some("usage"),
            "case {request_id} expected usage error code, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["severity"].as_str(),
            Some("low"),
            "case {request_id} expected low severity for usage error, got {envelope}"
        );
        assert_eq!(
            envelope["error"]["message"].as_str(),
            Some(expected_message),
            "case {request_id} expected message {expected_message:?}, got {envelope}"
        );
    }
    Ok(())
}

// bd-rpaqi: parse_serve_http_request rejects any method outside {GET, POST}
// before endpoint dispatch can run. The error path uses the flat
// serve_error_payload (NOT the SERVE_ENDPOINT_SCHEMA_V1 exchange envelope)
// because the request itself was unparseable. Pin both the 400 status and
// the canonical 'Only GET and POST are supported by ee serve v2.' message.
#[test]
fn serve_unsupported_http_method_rejected_with_canonical_400_envelope() -> TestResult {
    let token = "01234567890123456789012345678901";
    let raw = format!(
        "DELETE /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\n\r\n"
    );
    let response = render_serve_transport_exchange(
        "req-delete-method",
        raw.as_bytes(),
        &ServeLimits::default(),
        Some(token),
        0,
    );
    if !response.starts_with("HTTP/1.1 400 Bad Request\r\n") {
        return Err(format!("expected 400 response, got {response}"));
    }
    let envelope = response_body_json(&response)?;
    // The pre-request-parse error path produces a flat ee.error.v2 envelope
    // rather than the SERVE_ENDPOINT_SCHEMA_V1 exchange wrapper.
    assert_eq!(envelope["schema"].as_str(), Some("ee.error.v2"));
    assert_eq!(envelope["error"]["code"].as_str(), Some("usage"));
    assert_eq!(envelope["error"]["severity"].as_str(), Some("low"));
    assert_eq!(
        envelope["error"]["message"].as_str(),
        Some("Only GET and POST are supported by ee serve v2."),
    );
    Ok(())
}
