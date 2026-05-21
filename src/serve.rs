use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::core::degraded_aggregation::{
    AggregatedDegradation, DegradationAggregationInput, aggregate_degraded_entries,
};
use crate::models::DomainError;
use crate::steward::{DaemonForegroundOptions, DaemonForegroundReport, JobRunResult, JobType};

pub const SUBSYSTEM: &str = "serve";
pub const DAEMON_JOB_TABLE_SCHEMA_V1: &str = "ee.steward.daemon_job_table.v1";
pub const DAEMON_JOB_ROW_SCHEMA_V1: &str = "ee.steward.daemon_job_row.v1";
pub const DAEMON_STATUS_SCHEMA_V1: &str = "ee.steward.daemon_status.v1";
pub const DAEMON_RECOVERY_SCHEMA_V1: &str = "ee.steward.daemon_recovery.v1";
pub const DAEMON_WRITE_OWNER_IDENTITY: &str = "ee-daemon-single-write-owner";
pub const SERVE_UNAVAILABLE_V1_CODE: &str = "serve_unavailable_v1";
pub const SERVE_STARTUP_SCHEMA_V1: &str = "ee.serve.startup.v1";
pub const SERVE_ENDPOINT_SCHEMA_V1: &str = "ee.serve.endpoint.v1";
pub const DEFAULT_SERVE_HOST: &str = "127.0.0.1";
pub const DEFAULT_SERVE_PORT: u16 = 8766;
pub const MIN_SERVE_TOKEN_BITS: usize = 256;

fn trace_serve_localhost(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "serve-localhost",
        request_id = "daemon_foreground_request",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.4"),
        surface = "serve_localhost",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "serve localhost adapter checkpoint"
    );
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[must_use]
pub fn serve_unavailable_v1_error() -> DomainError {
    trace_serve_localhost("input", 0, &[]);
    trace_serve_localhost("dependency_check", 0, &[SERVE_UNAVAILABLE_V1_CODE]);
    trace_serve_localhost("response", 0, &[SERVE_UNAVAILABLE_V1_CODE]);
    DomainError::UsageCodeWithDetails {
        code: SERVE_UNAVAILABLE_V1_CODE,
        message: "The localhost HTTP adapter is planned for v2; forbidden-dep-clean HTTP/SSE is not wired in v1.".to_owned(),
        repair: Some(
            "Track bd-3usjw.4 and docs/adr/0033-serve-localhost-v2-design.md; use direct CLI commands such as `ee context`, `ee search`, `ee why`, and `ee status` for now."
                .to_owned(),
        ),
        details_json: json!({
            "surface": "serve_localhost",
            "selectedPath": "honest_defer_to_v2",
            "trackingBead": "bd-3usjw.4",
            "designAdr": "docs/adr/0033-serve-localhost-v2-design.md",
            "recovery": [
                {
                    "priority": 1,
                    "kind": "broaden",
                    "rationale": "Use the direct context-pack CLI surface instead of the planned localhost adapter.",
                    "command": "ee context \"<task>\" --workspace . --json",
                    "resultsIn": "A deterministic context pack response on stdout."
                },
                {
                    "priority": 2,
                    "kind": "broaden",
                    "rationale": "Use direct search when an HTTP search endpoint would have been used.",
                    "command": "ee search \"<query>\" --workspace . --json",
                    "resultsIn": "A deterministic search response on stdout."
                },
                {
                    "priority": 3,
                    "kind": "broaden",
                    "rationale": "Use direct status and doctor checks for readiness probes.",
                    "command": "ee status --workspace . --json && ee doctor --workspace . --json",
                    "resultsIn": "Local CLI readiness and repair information without a background HTTP server."
                }
            ]
        })
        .to_string(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeLimits {
    pub max_header_bytes: usize,
    pub max_body_bytes: usize,
    pub connection_read_timeout_ms: u64,
    pub handler_budget_ms: u64,
    pub response_write_timeout_ms: u64,
    pub sse_event_buffer: usize,
}

impl Default for ServeLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: 16 * 1024,
            max_body_bytes: 1024 * 1024,
            connection_read_timeout_ms: 5_000,
            handler_budget_ms: 30_000,
            response_write_timeout_ms: 5_000,
            sse_event_buffer: 64,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeStartupOptions {
    pub host: String,
    pub port: u16,
    pub allow_non_loopback: bool,
    pub limits: ServeLimits,
}

impl Default for ServeStartupOptions {
    fn default() -> Self {
        Self {
            host: DEFAULT_SERVE_HOST.to_owned(),
            port: DEFAULT_SERVE_PORT,
            allow_non_loopback: false,
            limits: ServeLimits::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServeEndpoint {
    Status,
    Doctor,
    Search,
    Context,
    Why,
    SwarmBrief,
    DurableWrite,
    Events,
    Unknown,
}

impl ServeEndpoint {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Doctor => "doctor",
            Self::Search => "search",
            Self::Context => "context",
            Self::Why => "why",
            Self::SwarmBrief => "swarmBrief",
            Self::DurableWrite => "durableWrite",
            Self::Events => "events",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn cli_equivalent(self) -> Option<&'static str> {
        match self {
            Self::Status => Some("ee status --json"),
            Self::Doctor => Some("ee doctor --json"),
            Self::Search => Some("ee search \"<query>\" --json"),
            Self::Context => Some("ee context \"<task>\" --json"),
            Self::Why => Some("ee why <memory-id> --json"),
            Self::SwarmBrief => Some("ee swarm brief --json"),
            Self::DurableWrite | Self::Events | Self::Unknown => None,
        }
    }

    #[must_use]
    pub const fn mutable(self) -> bool {
        matches!(self, Self::DurableWrite)
    }

    #[must_use]
    pub const fn auth_required(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeHttpRequest {
    pub method: String,
    pub target: String,
    pub path: String,
    pub endpoint: ServeEndpoint,
    pub query: BTreeMap<String, Vec<String>>,
    pub headers: BTreeMap<String, String>,
    pub body_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeDispatchPlan {
    pub endpoint: ServeEndpoint,
    pub handler_surface: &'static str,
    pub cli_argv: Vec<String>,
    pub payload_schema: &'static str,
    pub mutable: bool,
    pub sse_stream: bool,
}

impl ServeDispatchPlan {
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        json!({
            "endpoint": self.endpoint.as_str(),
            "handlerSurface": self.handler_surface,
            "cliArgv": self.cli_argv,
            "payloadSchema": self.payload_schema,
            "mutable": self.mutable,
            "sseStream": self.sse_stream
        })
    }
}

#[derive(Debug)]
pub struct ServeListenerBinding {
    pub listener: TcpListener,
    pub metadata: JsonValue,
}

#[must_use]
pub fn serve_startup_report_json(
    options: &ServeStartupOptions,
    token: Option<&str>,
) -> Result<JsonValue, DomainError> {
    let loopback_only = serve_host_is_loopback(&options.host)?;
    if !loopback_only && !options.allow_non_loopback {
        return Err(serve_policy_error(
            "serve_non_loopback_requires_opt_in",
            format!(
                "Refusing to bind ee serve to non-loopback host '{}'.",
                options.host
            ),
            "Re-run with --allow-non-loopback only after configuring EE_SERVE_TOKEN.",
            json!({
                "host": options.host,
                "allowNonLoopback": options.allow_non_loopback,
                "recovery": [
                    {
                        "priority": 1,
                        "kind": "narrow",
                        "command": "ee serve --foreground --host 127.0.0.1 --json"
                    },
                    {
                        "priority": 2,
                        "kind": "configure",
                        "command": "export EE_SERVE_TOKEN=<256-bit-random-token>"
                    }
                ]
            }),
        ));
    }

    let token_posture = serve_token_posture(token);
    if !loopback_only && token_posture.state != "configured" {
        return Err(serve_policy_error(
            "serve_non_loopback_requires_strong_token",
            "Refusing non-loopback ee serve without a configured 256-bit EE_SERVE_TOKEN."
                .to_owned(),
            "Set EE_SERVE_TOKEN to at least 32 random bytes before using --allow-non-loopback.",
            json!({
                "host": options.host,
                "tokenState": token_posture.state,
                "minimumBits": MIN_SERVE_TOKEN_BITS,
                "recovery": [
                    {
                        "priority": 1,
                        "kind": "configure",
                        "command": "export EE_SERVE_TOKEN=<256-bit-random-token>"
                    }
                ]
            }),
        ));
    }

    let can_accept = token_posture.state == "configured";
    let degraded = serve_startup_degraded(&token_posture);
    Ok(json!({
        "schema": SERVE_STARTUP_SCHEMA_V1,
        "bind": {
            "host": options.host,
            "port": options.port,
            "loopbackOnly": loopback_only,
            "allowNonLoopback": options.allow_non_loopback,
            "policy": if loopback_only {
                "loopback_default"
            } else {
                "non_loopback_explicit"
            }
        },
        "protocol": {
            "httpVersion": "HTTP/1.1",
            "sseReadOnly": true,
            "forbiddenHttpDeps": ["hyper", "axum", "tower", "reqwest"]
        },
        "tokenPosture": {
            "source": "EE_SERVE_TOKEN",
            "state": token_posture.state,
            "minimumBits": MIN_SERVE_TOKEN_BITS,
            "tokenMaterialExposed": false,
            "repair": token_posture.repair
        },
        "readiness": {
            "state": if can_accept { "ready" } else { "policy_denied" },
            "canAcceptConnections": can_accept,
            "mutableEndpointsEnabled": can_accept,
            "reason": if can_accept {
                JsonValue::Null
            } else {
                json!("EE_SERVE_TOKEN is required before accepting localhost HTTP requests.")
            }
        },
        "endpoints": serve_endpoint_catalog_json(),
        "limits": {
            "maxHeaderBytes": options.limits.max_header_bytes,
            "maxBodyBytes": options.limits.max_body_bytes,
            "connectionReadTimeoutMs": options.limits.connection_read_timeout_ms,
            "handlerBudgetMs": options.limits.handler_budget_ms,
            "responseWriteTimeoutMs": options.limits.response_write_timeout_ms,
            "sseEventBuffer": options.limits.sse_event_buffer
        },
        "degraded": degraded
    }))
}

pub fn bind_serve_listener(
    options: &ServeStartupOptions,
    token: Option<&str>,
) -> Result<ServeListenerBinding, DomainError> {
    let startup = serve_startup_report_json(options, token)?;
    let token_posture = serve_token_posture(token);
    if token_posture.state != "configured" {
        return Err(serve_policy_error(
            "serve_token_required_before_bind",
            "Refusing to bind ee serve before EE_SERVE_TOKEN is configured with at least 256 bits."
                .to_owned(),
            "Set EE_SERVE_TOKEN to at least 32 random bytes before starting the listener.",
            json!({
                "host": options.host,
                "port": options.port,
                "tokenState": token_posture.state,
                "minimumBits": MIN_SERVE_TOKEN_BITS,
                "tokenMaterialExposed": false
            }),
        ));
    }

    let bind_addr = format!("{}:{}", options.host, options.port);
    let listener = TcpListener::bind(&bind_addr).map_err(|error| DomainError::Configuration {
        message: format!("Failed to bind ee serve listener at {bind_addr}: {error}"),
        repair: Some(
            "Choose another loopback port or stop the process using this port.".to_owned(),
        ),
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| DomainError::Configuration {
            message: format!("Failed to set ee serve listener nonblocking mode: {error}"),
            repair: Some("Retry `ee serve --foreground` or use direct CLI commands.".to_owned()),
        })?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| DomainError::Configuration {
            message: format!("Failed to inspect ee serve listener local address: {error}"),
            repair: Some("Retry `ee serve --foreground` or use direct CLI commands.".to_owned()),
        })?;

    let metadata = json!({
        "schema": SERVE_STARTUP_SCHEMA_V1,
        "listener": {
            "requestedHost": options.host,
            "requestedPort": options.port,
            "boundHost": local_addr.ip().to_string(),
            "boundPort": local_addr.port(),
            "loopbackOnly": local_addr.ip().is_loopback(),
            "nonblocking": true,
            "tokenMaterialExposed": false
        },
        "startup": startup
    });
    Ok(ServeListenerBinding { listener, metadata })
}

pub fn serve_dispatch_plan(request: &ServeHttpRequest) -> Result<ServeDispatchPlan, DomainError> {
    match request.endpoint {
        ServeEndpoint::Status => Ok(read_only_cli_dispatch(
            ServeEndpoint::Status,
            "cli.status",
            vec!["ee", "status", "--json"],
        )),
        ServeEndpoint::Doctor => Ok(read_only_cli_dispatch(
            ServeEndpoint::Doctor,
            "cli.doctor",
            vec!["ee", "doctor", "--json"],
        )),
        ServeEndpoint::Search => {
            let query = require_single_query_value(request, "q", "/v1/search")?;
            Ok(ServeDispatchPlan {
                endpoint: ServeEndpoint::Search,
                handler_surface: "cli.search",
                cli_argv: vec![
                    "ee".to_owned(),
                    "search".to_owned(),
                    query,
                    "--json".to_owned(),
                ],
                payload_schema: "ee.response.v2",
                mutable: false,
                sse_stream: false,
            })
        }
        ServeEndpoint::Context => {
            let task = require_single_query_value(request, "task", "/v1/context")?;
            Ok(ServeDispatchPlan {
                endpoint: ServeEndpoint::Context,
                handler_surface: "cli.context",
                cli_argv: vec![
                    "ee".to_owned(),
                    "context".to_owned(),
                    task,
                    "--json".to_owned(),
                ],
                payload_schema: "ee.response.v2",
                mutable: false,
                sse_stream: false,
            })
        }
        ServeEndpoint::Why => {
            let memory_id = request
                .path
                .strip_prefix("/v1/why/")
                .filter(|value| !value.trim().is_empty() && !value.contains('/'))
                .ok_or_else(|| {
                    serve_usage_error(
                        "GET /v1/why/{memory_id} requires exactly one memory ID path segment.",
                    )
                })?;
            Ok(ServeDispatchPlan {
                endpoint: ServeEndpoint::Why,
                handler_surface: "cli.why",
                cli_argv: vec![
                    "ee".to_owned(),
                    "why".to_owned(),
                    memory_id.to_owned(),
                    "--json".to_owned(),
                ],
                payload_schema: "ee.response.v2",
                mutable: false,
                sse_stream: false,
            })
        }
        ServeEndpoint::SwarmBrief => Ok(read_only_cli_dispatch(
            ServeEndpoint::SwarmBrief,
            "cli.swarm.brief",
            vec!["ee", "swarm", "brief", "--json"],
        )),
        ServeEndpoint::DurableWrite => Ok(ServeDispatchPlan {
            endpoint: ServeEndpoint::DurableWrite,
            handler_surface: "serve.durable_write_placeholder",
            cli_argv: Vec::new(),
            payload_schema: "ee.response.v2",
            mutable: true,
            sse_stream: false,
        }),
        ServeEndpoint::Events => Ok(ServeDispatchPlan {
            endpoint: ServeEndpoint::Events,
            handler_surface: "serve.sse.events",
            cli_argv: Vec::new(),
            payload_schema: "ee.response.v2",
            mutable: false,
            sse_stream: true,
        }),
        ServeEndpoint::Unknown => Err(serve_usage_error(format!(
            "No ee serve v2 endpoint is registered for {} {}.",
            request.method, request.path
        ))),
    }
}

pub fn parse_serve_http_request(
    bytes: &[u8],
    limits: &ServeLimits,
) -> Result<ServeHttpRequest, DomainError> {
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| serve_usage_error("HTTP request is missing the CRLF header terminator."))?;
    if header_end > limits.max_header_bytes {
        return Err(serve_usage_error(format!(
            "HTTP request headers exceed the {} byte limit.",
            limits.max_header_bytes
        )));
    }
    let header_text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| serve_usage_error("HTTP request headers must be valid UTF-8."))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| serve_usage_error("HTTP request line is missing."))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| serve_usage_error("HTTP method is missing."))?;
    let target = parts
        .next()
        .ok_or_else(|| serve_usage_error("HTTP request target is missing."))?;
    let version = parts
        .next()
        .ok_or_else(|| serve_usage_error("HTTP version is missing."))?;
    if parts.next().is_some() || version != "HTTP/1.1" {
        return Err(serve_usage_error(
            "Only narrow HTTP/1.1 request lines are supported.",
        ));
    }
    if !matches!(method, "GET" | "POST") {
        return Err(serve_usage_error(
            "Only GET and POST are supported by ee serve v2.",
        ));
    }
    if !target.starts_with('/') {
        return Err(serve_usage_error(
            "HTTP request target must be an absolute path.",
        ));
    }

    let mut headers = BTreeMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(serve_usage_error(
                "HTTP header line is missing ':' separator.",
            ));
        };
        let normalized_name = name.trim().to_ascii_lowercase();
        if normalized_name.is_empty() {
            return Err(serve_usage_error("HTTP header name must not be empty."));
        }
        headers.insert(normalized_name, value.trim().to_owned());
    }

    let body = &bytes[header_end + 4..];
    let declared_body_bytes = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| serve_usage_error("Content-Length must be a non-negative integer."))?,
        None => 0,
    };
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        return Err(serve_usage_error(
            "Chunked uploads are not accepted by the first ee serve v2 slice.",
        ));
    }
    if declared_body_bytes > limits.max_body_bytes {
        return Err(serve_usage_error(format!(
            "HTTP request body exceeds the {} byte limit.",
            limits.max_body_bytes
        )));
    }
    if method == "POST" && !headers.contains_key("content-length") {
        return Err(serve_usage_error(
            "POST requests must include an explicit Content-Length.",
        ));
    }
    if body.len() < declared_body_bytes {
        return Err(serve_usage_error(
            "HTTP request body is shorter than Content-Length.",
        ));
    }
    if body.len() > declared_body_bytes {
        return Err(serve_usage_error(
            "HTTP request body contains bytes beyond Content-Length; keepalive is not supported.",
        ));
    }

    let (path, query) = parse_serve_target(target)?;
    let endpoint = serve_endpoint_for(method, &path);
    Ok(ServeHttpRequest {
        method: method.to_owned(),
        target: target.to_owned(),
        path,
        endpoint,
        query,
        headers,
        body_bytes: declared_body_bytes,
    })
}

#[must_use]
pub fn serve_auth_state(request: &ServeHttpRequest, token: Option<&str>) -> &'static str {
    if !request.endpoint.auth_required() {
        return "not_required";
    }
    let posture = serve_token_posture(token);
    if posture.state != "configured" {
        return posture.state;
    }
    let Some(configured_token) = token else {
        return "missing";
    };
    match request.headers.get("authorization") {
        Some(value) if value == &format!("Bearer {configured_token}") => "accepted",
        Some(_) => "rejected",
        None => "missing",
    }
}

#[must_use]
pub fn serve_auth_failure_envelope(
    request_id: &str,
    request: &ServeHttpRequest,
    auth_state: &'static str,
    elapsed_ms: u64,
) -> JsonValue {
    let error = DomainError::PolicyDeniedWithDetails {
        message: "ee serve requires a valid bearer token before endpoint dispatch.".to_owned(),
        repair: Some("Set EE_SERVE_TOKEN and send Authorization: Bearer <token>.".to_owned()),
        details_json: json!({
            "recovery": [
                {
                    "priority": 1,
                    "kind": "configure",
                    "command": "export EE_SERVE_TOKEN=<256-bit-random-token>"
                }
            ],
            "authState": auth_state,
            "tokenMaterialExposed": false
        })
        .to_string(),
    };
    let payload: JsonValue = serde_json::from_str(&crate::output::error_response_json(&error))
        .unwrap_or_else(|_| json!({"schema": "ee.error.v2"}));
    json!({
        "schema": SERVE_ENDPOINT_SCHEMA_V1,
        "request": serve_request_metadata_json(request_id, request, auth_state),
        "response": {
            "statusCode": 401,
            "payloadSchema": "ee.error.v2",
            "payload": payload,
            "elapsedMs": elapsed_ms,
            "degradedCodes": [serve_auth_degraded_code(auth_state)],
            "volatileTransportFields": ["request.requestId", "response.elapsedMs"]
        }
    })
}

#[must_use]
pub fn render_serve_sse_event(event_kind: &str, terminal: bool, payload: &JsonValue) -> String {
    let wrapped_payload = if matches!(
        payload.get("schema").and_then(JsonValue::as_str),
        Some("ee.response.v2" | "ee.error.v2")
    ) {
        payload.clone()
    } else {
        json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": payload,
            "degraded": []
        })
    };
    let payload_schema = match wrapped_payload.get("schema").and_then(JsonValue::as_str) {
        Some("ee.error.v2") => "ee.error.v2",
        _ => "ee.response.v2",
    };
    let event_payload = json!({
        "schema": SERVE_ENDPOINT_SCHEMA_V1,
        "request": {
            "requestId": "sse-stream",
            "method": "GET",
            "path": "/v1/events",
            "endpoint": ServeEndpoint::Events.as_str(),
            "cliEquivalent": ServeEndpoint::Events.cli_equivalent(),
            "auth": {
                "required": true,
                "state": "accepted",
                "tokenMaterialExposed": false
            },
            "bodyBytes": 0,
            "query": {},
            "contentLengthRequired": false,
            "chunkedUploadAccepted": false
        },
        "response": {
            "statusCode": if payload_schema == "ee.error.v2" { 500 } else { 200 },
            "payloadSchema": payload_schema,
            "payload": wrapped_payload,
            "elapsedMs": 0,
            "degradedCodes": [],
            "volatileTransportFields": ["request.requestId", "response.elapsedMs"]
        },
        "sse": {
            "readOnly": true,
            "eventKind": event_kind,
            "terminal": terminal,
            "eventBufferRemaining": 0
        }
    });
    format!("event: {event_kind}\ndata: {event_payload}\n\n")
}

#[must_use]
pub fn render_serve_http_json_response(status_code: u16, payload: &JsonValue) -> String {
    let body = payload.to_string();
    render_serve_http_response(
        status_code,
        "application/json; charset=utf-8",
        Some(body.len()),
        &body,
    )
}

#[must_use]
pub fn render_serve_http_sse_response(first_frame: &str) -> String {
    render_serve_http_response(200, "text/event-stream; charset=utf-8", None, first_frame)
}

#[must_use]
pub fn render_serve_transport_exchange(
    request_id: &str,
    request_bytes: &[u8],
    limits: &ServeLimits,
    token: Option<&str>,
    elapsed_ms: u64,
) -> String {
    let request = match parse_serve_http_request(request_bytes, limits) {
        Ok(request) => request,
        Err(error) => return render_serve_http_json_response(400, &serve_error_payload(&error)),
    };

    let auth_state = serve_auth_state(&request, token);
    if auth_state != "accepted" {
        return render_serve_http_json_response(
            401,
            &serve_auth_failure_envelope(request_id, &request, auth_state, elapsed_ms),
        );
    }

    let plan = match serve_dispatch_plan(&request) {
        Ok(plan) => plan,
        Err(error) => {
            let status_code = if request.endpoint == ServeEndpoint::Unknown {
                404
            } else {
                400
            };
            return render_serve_http_json_response(
                status_code,
                &serve_error_exchange_envelope(
                    request_id,
                    &request,
                    auth_state,
                    status_code,
                    &error,
                    elapsed_ms,
                ),
            );
        }
    };

    if plan.sse_stream {
        let frame = render_serve_sse_event(
            "header",
            false,
            &serve_dispatch_payload_json(&plan, "transport_only"),
        );
        return render_serve_http_sse_response(&frame);
    }

    render_serve_http_json_response(
        200,
        &serve_dispatch_exchange_envelope(request_id, &request, auth_state, 200, &plan, elapsed_ms),
    )
}

fn render_serve_http_response(
    status_code: u16,
    content_type: &str,
    content_length: Option<usize>,
    body: &str,
) -> String {
    let mut response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {content_type}\r\n",
        status_code,
        serve_http_reason_phrase(status_code)
    );
    if let Some(content_length) = content_length {
        response.push_str(&format!("Content-Length: {content_length}\r\n"));
    }
    response.push_str("Cache-Control: no-store\r\nConnection: close\r\n\r\n");
    response.push_str(body);
    response
}

fn serve_http_reason_phrase(status_code: u16) -> &'static str {
    match status_code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ServeTokenPosture {
    state: &'static str,
    repair: Option<&'static str>,
}

fn serve_token_posture(token: Option<&str>) -> ServeTokenPosture {
    match token {
        None | Some("") => ServeTokenPosture {
            state: "missing",
            repair: Some("Set EE_SERVE_TOKEN to at least 32 random bytes before serving HTTP."),
        },
        Some(value) if value.as_bytes().len().saturating_mul(8) < MIN_SERVE_TOKEN_BITS => {
            ServeTokenPosture {
                state: "weak",
                repair: Some("Use at least 32 random bytes of bearer-token material."),
            }
        }
        Some(_) => ServeTokenPosture {
            state: "configured",
            repair: None,
        },
    }
}

fn serve_startup_degraded(token_posture: &ServeTokenPosture) -> Vec<JsonValue> {
    match token_posture.state {
        "configured" => Vec::new(),
        "weak" => vec![json!({
            "code": "serve_token_weak",
            "severity": "high",
            "message": "EE_SERVE_TOKEN is present but below the 256-bit minimum.",
            "repair": token_posture.repair
        })],
        _ => vec![json!({
            "code": "serve_token_missing",
            "severity": "high",
            "message": "EE_SERVE_TOKEN is required before the localhost HTTP adapter accepts requests.",
            "repair": token_posture.repair
        })],
    }
}

fn serve_dispatch_exchange_envelope(
    request_id: &str,
    request: &ServeHttpRequest,
    auth_state: &'static str,
    status_code: u16,
    plan: &ServeDispatchPlan,
    elapsed_ms: u64,
) -> JsonValue {
    json!({
        "schema": SERVE_ENDPOINT_SCHEMA_V1,
        "request": serve_request_metadata_json(request_id, request, auth_state),
        "response": {
            "statusCode": status_code,
            "payloadSchema": "ee.response.v2",
            "payload": serve_dispatch_payload_json(plan, "not_started"),
            "elapsedMs": elapsed_ms,
            "degradedCodes": [],
            "volatileTransportFields": ["request.requestId", "response.elapsedMs"]
        }
    })
}

fn serve_error_exchange_envelope(
    request_id: &str,
    request: &ServeHttpRequest,
    auth_state: &'static str,
    status_code: u16,
    error: &DomainError,
    elapsed_ms: u64,
) -> JsonValue {
    json!({
        "schema": SERVE_ENDPOINT_SCHEMA_V1,
        "request": serve_request_metadata_json(request_id, request, auth_state),
        "response": {
            "statusCode": status_code,
            "payloadSchema": "ee.error.v2",
            "payload": serve_error_payload(error),
            "elapsedMs": elapsed_ms,
            "degradedCodes": [error.code()],
            "volatileTransportFields": ["request.requestId", "response.elapsedMs"]
        }
    })
}

fn serve_dispatch_payload_json(plan: &ServeDispatchPlan, execution: &'static str) -> JsonValue {
    json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "execution": execution,
            "executionBoundary": "serve_transport_adapter",
            "businessLogicExecuted": false,
            "dispatchPlan": plan.to_json()
        },
        "degraded": []
    })
}

fn serve_error_payload(error: &DomainError) -> JsonValue {
    serde_json::from_str(&crate::output::error_response_json(error))
        .unwrap_or_else(|_| json!({"schema": "ee.error.v2"}))
}

fn read_only_cli_dispatch(
    endpoint: ServeEndpoint,
    handler_surface: &'static str,
    argv: Vec<&'static str>,
) -> ServeDispatchPlan {
    ServeDispatchPlan {
        endpoint,
        handler_surface,
        cli_argv: argv.into_iter().map(str::to_owned).collect(),
        payload_schema: "ee.response.v2",
        mutable: false,
        sse_stream: false,
    }
}

fn require_single_query_value(
    request: &ServeHttpRequest,
    name: &str,
    endpoint_path: &str,
) -> Result<String, DomainError> {
    match request.query.get(name).map(Vec::as_slice) {
        Some([value]) if !value.trim().is_empty() => Ok(value.clone()),
        Some([_]) => Err(serve_usage_error(format!(
            "{endpoint_path} requires a non-empty `{name}` query parameter."
        ))),
        Some(_) => Err(serve_usage_error(format!(
            "{endpoint_path} requires exactly one `{name}` query parameter."
        ))),
        None => Err(serve_usage_error(format!(
            "{endpoint_path} requires a `{name}` query parameter."
        ))),
    }
}

fn serve_endpoint_catalog_json() -> Vec<JsonValue> {
    [
        ("GET", "/v1/status", ServeEndpoint::Status),
        ("GET", "/v1/doctor", ServeEndpoint::Doctor),
        ("GET", "/v1/search", ServeEndpoint::Search),
        ("GET", "/v1/context", ServeEndpoint::Context),
        ("GET", "/v1/why/{memory_id}", ServeEndpoint::Why),
        ("GET", "/v1/swarm/brief", ServeEndpoint::SwarmBrief),
        ("POST", "/v1/durable-write", ServeEndpoint::DurableWrite),
        ("GET", "/v1/events", ServeEndpoint::Events),
    ]
    .into_iter()
    .map(|(method, path, endpoint)| {
        json!({
            "method": method,
            "path": path,
            "endpoint": endpoint.as_str(),
            "cliEquivalent": endpoint.cli_equivalent(),
            "mutable": endpoint.mutable(),
            "authRequired": endpoint.auth_required()
        })
    })
    .collect()
}

fn serve_request_metadata_json(
    request_id: &str,
    request: &ServeHttpRequest,
    auth_state: &'static str,
) -> JsonValue {
    json!({
        "requestId": request_id,
        "method": request.method,
        "path": request.path,
        "endpoint": request.endpoint.as_str(),
        "cliEquivalent": request.endpoint.cli_equivalent(),
        "auth": {
            "required": request.endpoint.auth_required(),
            "state": auth_state,
            "tokenMaterialExposed": false
        },
        "bodyBytes": request.body_bytes,
        "query": request.query,
        "contentLengthRequired": request.method == "POST",
        "chunkedUploadAccepted": false
    })
}

fn serve_auth_degraded_code(auth_state: &'static str) -> &'static str {
    match auth_state {
        "weak" => "serve_auth_weak_token",
        "rejected" => "serve_auth_rejected",
        _ => "serve_auth_missing",
    }
}

fn serve_host_is_loopback(host: &str) -> Result<bool, DomainError> {
    let ip: IpAddr = host.parse().map_err(|_| {
        serve_usage_error(format!(
            "ee serve host '{host}' is not a supported numeric IP address."
        ))
    })?;
    Ok(ip.is_loopback())
}

fn serve_endpoint_for(method: &str, path: &str) -> ServeEndpoint {
    match (method, path) {
        ("GET", "/v1/status") => ServeEndpoint::Status,
        ("GET", "/v1/doctor") => ServeEndpoint::Doctor,
        ("GET", "/v1/search") => ServeEndpoint::Search,
        ("GET", "/v1/context") => ServeEndpoint::Context,
        ("GET", "/v1/swarm/brief") => ServeEndpoint::SwarmBrief,
        ("POST", "/v1/durable-write") => ServeEndpoint::DurableWrite,
        ("GET", "/v1/events") => ServeEndpoint::Events,
        ("GET", path) if path.starts_with("/v1/why/") => ServeEndpoint::Why,
        _ => ServeEndpoint::Unknown,
    }
}

fn parse_serve_target(
    target: &str,
) -> Result<(String, BTreeMap<String, Vec<String>>), DomainError> {
    let (path, query_raw) = target.split_once('?').unwrap_or((target, ""));
    let mut query = BTreeMap::<String, Vec<String>>::new();
    for pair in query_raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        query
            .entry(percent_decode_query_component(key)?)
            .or_default()
            .push(percent_decode_query_component(value)?);
    }
    Ok((path.to_owned(), query))
}

fn percent_decode_query_component(value: &str) -> Result<String, DomainError> {
    let mut output = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(' ');
                index += 1;
            }
            b'%' => {
                if index + 2 >= bytes.len() {
                    return Err(serve_usage_error("Percent escape in query is truncated."));
                }
                let hi = hex_digit(bytes[index + 1])?;
                let lo = hex_digit(bytes[index + 2])?;
                output.push(char::from((hi << 4) | lo));
                index += 3;
            }
            byte => {
                output.push(char::from(byte));
                index += 1;
            }
        }
    }
    Ok(output)
}

fn hex_digit(byte: u8) -> Result<u8, DomainError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(serve_usage_error(
            "Percent escape in query contains a non-hex digit.",
        )),
    }
}

fn serve_usage_error(message: impl Into<String>) -> DomainError {
    DomainError::Usage {
        message: message.into(),
        repair: Some(
            "Use narrow HTTP/1.1 requests documented by `ee schema export ee.serve.endpoint.v1`."
                .to_owned(),
        ),
    }
}

fn serve_policy_error(
    code: &'static str,
    message: String,
    repair: &'static str,
    details: JsonValue,
) -> DomainError {
    DomainError::PolicyDeniedWithDetails {
        message,
        repair: Some(repair.to_owned()),
        details_json: json!({
            "code": code,
            "serve": details
        })
        .to_string(),
    }
}

#[derive(Clone, Debug)]
pub struct DaemonRunPlan {
    pub run_id: String,
    pub table_path: PathBuf,
    pub rows: Vec<DaemonJobRow>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonJobRow {
    pub schema: String,
    pub row_id: String,
    pub run_id: String,
    pub daemon_job_key: String,
    pub runner_job_id: String,
    pub tick: u32,
    pub job_type: String,
    pub status: String,
    pub outcome: Option<String>,
    pub workspace: String,
    pub write_owner_id: String,
    pub reason: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub recorded_at: String,
    pub duration_ms: Option<u64>,
    pub items_processed: Option<u64>,
    pub error: Option<String>,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub recovered_from_orphan: bool,
    pub recovery_reason: Option<String>,
}

impl DaemonJobRow {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| {
            json!({
                "schema": DAEMON_JOB_ROW_SCHEMA_V1,
                "rowId": self.row_id,
                "daemonJobKey": self.daemon_job_key,
                "status": self.status,
            })
        })
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "running")
    }
}

#[derive(Clone, Debug)]
pub struct DaemonRecoveryReport {
    pub workspace: String,
    pub table_path: PathBuf,
    pub recovered_at: String,
    pub scanned_rows: usize,
    pub open_jobs_cancelled: usize,
    pub recovered_rows: Vec<DaemonJobRow>,
}

impl DaemonRecoveryReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": DAEMON_RECOVERY_SCHEMA_V1,
            "workspace": self.workspace,
            "tablePath": self.table_path.display().to_string(),
            "recoveredAt": self.recovered_at,
            "scannedRows": self.scanned_rows,
            "openJobsCancelled": self.open_jobs_cancelled,
            "recoveredRows": self
                .recovered_rows
                .iter()
                .map(DaemonJobRow::data_json)
                .collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct DaemonStatusReport {
    pub workspace: String,
    pub requested_job_types: Vec<JobType>,
    pub table_path: PathBuf,
    pub row_count: usize,
    pub open_job_count: usize,
    pub recent_outcomes: Vec<DaemonJobRow>,
}

impl DaemonStatusReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let spool_config = crate::core::WriteSpoolConfig::default();
        let degraded = daemon_status_degraded();
        json!({
            "schema": DAEMON_STATUS_SCHEMA_V1,
            "command": "daemon status",
            "workspace": self.workspace,
            "running": self.open_job_count > 0,
            "daemonized": false,
            "foregroundAvailable": true,
            "backgroundAvailable": false,
            "supervisor": "asupersync_foreground",
            "jobTypes": self
                .requested_job_types
                .iter()
                .map(|job_type| job_type.as_str())
                .collect::<Vec<_>>(),
            "writeOwner": {
                "schema": crate::core::WRITE_OWNER_STATUS_SCHEMA_V1,
                "identity": DAEMON_WRITE_OWNER_IDENTITY,
                "mode": "single_process_foreground",
                "spool": {
                    "schema": crate::core::WRITE_SPOOL_STATUS_SCHEMA_V1,
                    "backpressureSchema": crate::core::WRITE_SPOOL_BACKPRESSURE_SCHEMA_V1,
                    "backpressureCode": crate::core::WRITE_SPOOL_BACKPRESSURE_CODE,
                    "maxPending": spool_config.max_pending,
                    "maxBatchSize": spool_config.max_batch_size,
                    "maxPendingBytes": spool_config.max_pending_bytes,
                    "maxQueueAgeMs": spool_config.max_queue_age_ms,
                }
            },
            "durable": {
                "schema": DAEMON_JOB_TABLE_SCHEMA_V1,
                "tablePath": self.table_path.display().to_string(),
                "rowCount": self.row_count,
                "openJobCount": self.open_job_count,
                "recentOutcomeCount": self.recent_outcomes.len(),
            },
            "recentOutcomes": self
                .recent_outcomes
                .iter()
                .map(DaemonJobRow::data_json)
                .collect::<Vec<_>>(),
            "recovery": {
                "schema": DAEMON_RECOVERY_SCHEMA_V1,
                "openJobsEligibleForCancellation": self.open_job_count,
                "repair": "Start ee daemon --foreground --once --json to recover orphaned pending/running daemon jobs."
            },
            "capabilityGap": {
                "code": "daemon_background_mode_unimplemented",
                "capabilitiesCommand": "ee capabilities --json"
            },
            "degraded": degraded,
        })
    }
}

fn daemon_status_degraded() -> Vec<AggregatedDegradation> {
    aggregate_daemon_status_degraded([daemon_background_mode_degradation_input()])
}

fn aggregate_daemon_status_degraded<I>(entries: I) -> Vec<AggregatedDegradation>
where
    I: IntoIterator<Item = DegradationAggregationInput>,
{
    aggregate_degraded_entries(entries)
}

fn daemon_background_mode_degradation_input() -> DegradationAggregationInput {
    DegradationAggregationInput::new(
        "daemon_status",
        "daemon_background_mode_unimplemented",
        "low",
        "Only bounded foreground daemon mode is available; background daemonization is not implemented.",
        "Run `ee daemon --foreground --once --json` for bounded maintenance.",
    )
}

#[must_use]
pub fn daemon_job_table_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("daemon-jobs.jsonl")
}

pub fn record_daemon_foreground_start(
    workspace_path: &Path,
    options: &DaemonForegroundOptions,
) -> Result<DaemonRunPlan, String> {
    trace_serve_localhost("input", 0, &[]);
    let table_path = daemon_job_table_path(workspace_path);
    if options.dry_run {
        trace_serve_localhost("response", 0, &[]);
        return Ok(DaemonRunPlan {
            run_id: "dry-run".to_owned(),
            table_path,
            rows: Vec::new(),
        });
    }

    let recorded_at = Utc::now().to_rfc3339();
    let run_id = daemon_run_id(workspace_path, &recorded_at);
    let mut rows = Vec::new();
    for tick in 1..=options.tick_limit {
        for (offset, job_type) in options.job_types.iter().enumerate() {
            let runner_job_id = runner_job_id(offset);
            rows.push(DaemonJobRow {
                schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(),
                row_id: row_id(&run_id, tick, &runner_job_id, "planned"),
                run_id: run_id.clone(),
                daemon_job_key: daemon_job_key(&run_id, tick, &runner_job_id),
                runner_job_id,
                tick,
                job_type: job_type.as_str().to_owned(),
                status: if tick == 1 { "running" } else { "pending" }.to_owned(),
                outcome: None,
                workspace: workspace_path.to_string_lossy().into_owned(),
                write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
                reason: format!("daemon foreground tick {tick} planned"),
                started_at: Some(recorded_at.clone()),
                completed_at: None,
                recorded_at: recorded_at.clone(),
                duration_ms: None,
                items_processed: None,
                error: None,
                dry_run: false,
                durable_mutation: false,
                recovered_from_orphan: false,
                recovery_reason: None,
            });
        }
    }

    trace_serve_localhost("persistence", 0, &[]);
    append_daemon_job_rows(&table_path, &rows)?;
    trace_serve_localhost("response", 0, &[]);
    Ok(DaemonRunPlan {
        run_id,
        table_path,
        rows,
    })
}

pub fn record_daemon_foreground_report(
    workspace_path: &Path,
    report: &DaemonForegroundReport,
    run_id: &str,
) -> Result<Vec<DaemonJobRow>, String> {
    trace_serve_localhost("input", 0, &[]);
    if report.dry_run || run_id == "dry-run" {
        trace_serve_localhost("response", 0, &[]);
        return Ok(Vec::new());
    }

    let mut rows = Vec::new();
    for tick in &report.ticks {
        for result in &tick.report.results {
            rows.push(row_from_result(
                workspace_path,
                run_id,
                tick.tick,
                &tick.started_at,
                &tick.completed_at,
                result,
            ));
        }
    }

    trace_serve_localhost("persistence", 0, &[]);
    append_daemon_job_rows(&daemon_job_table_path(workspace_path), &rows)?;
    trace_serve_localhost("response", 0, &[]);
    Ok(rows)
}

pub fn recover_orphaned_daemon_jobs(
    workspace_path: &Path,
    reason: &str,
) -> Result<DaemonRecoveryReport, String> {
    trace_serve_localhost("input", 0, &[]);
    let table_path = daemon_job_table_path(workspace_path);
    let rows = load_daemon_job_rows(workspace_path)?;
    let latest = latest_daemon_rows(&rows);
    let recovered_at = Utc::now().to_rfc3339();
    let mut recovered_rows = Vec::new();

    for row in latest.into_iter().filter(DaemonJobRow::is_open) {
        recovered_rows.push(DaemonJobRow {
            schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(),
            row_id: row_id(
                &row.run_id,
                row.tick,
                &row.runner_job_id,
                "recovered-cancelled",
            ),
            run_id: row.run_id,
            daemon_job_key: row.daemon_job_key,
            runner_job_id: row.runner_job_id,
            tick: row.tick,
            job_type: row.job_type,
            status: "cancelled".to_owned(),
            outcome: Some("cancelled".to_owned()),
            workspace: row.workspace,
            write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
            reason: "daemon restart recovery".to_owned(),
            started_at: row.started_at,
            completed_at: Some(recovered_at.clone()),
            recorded_at: recovered_at.clone(),
            duration_ms: None,
            items_processed: None,
            error: Some(reason.to_owned()),
            dry_run: row.dry_run,
            durable_mutation: false,
            recovered_from_orphan: true,
            recovery_reason: Some(reason.to_owned()),
        });
    }

    if !recovered_rows.is_empty() {
        trace_serve_localhost("persistence", 0, &[]);
        append_daemon_job_rows(&table_path, &recovered_rows)?;
    }

    trace_serve_localhost("response", 0, &[]);
    Ok(DaemonRecoveryReport {
        workspace: workspace_path.to_string_lossy().into_owned(),
        table_path,
        recovered_at,
        scanned_rows: rows.len(),
        open_jobs_cancelled: recovered_rows.len(),
        recovered_rows,
    })
}

pub fn daemon_status_report(
    workspace_path: &Path,
    requested_job_types: &[JobType],
    recent_limit: usize,
) -> Result<DaemonStatusReport, String> {
    trace_serve_localhost("input", 0, &[]);
    let rows = load_daemon_job_rows(workspace_path)?;
    let mut latest = latest_daemon_rows(&rows);
    latest.sort_by(|left, right| {
        right
            .recorded_at
            .cmp(&left.recorded_at)
            .then_with(|| left.daemon_job_key.cmp(&right.daemon_job_key))
    });
    let open_job_count = latest.iter().filter(|row| row.is_open()).count();
    latest.truncate(recent_limit);
    trace_serve_localhost("response", 0, &[]);
    Ok(DaemonStatusReport {
        workspace: workspace_path.to_string_lossy().into_owned(),
        requested_job_types: requested_job_types.to_vec(),
        table_path: daemon_job_table_path(workspace_path),
        row_count: rows.len(),
        open_job_count,
        recent_outcomes: latest,
    })
}

pub fn load_daemon_job_rows(workspace_path: &Path) -> Result<Vec<DaemonJobRow>, String> {
    let table_path = daemon_job_table_path(workspace_path);
    if !daemon_job_table_path_is_regular_file(&table_path, "read")? {
        return Ok(Vec::new());
    }
    let file = open_daemon_job_table_for_read(&table_path)
        .map_err(|error| format!("Failed to open daemon job table: {error}"))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| format!("Failed to read daemon job row: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let row = serde_json::from_str::<DaemonJobRow>(&line).map_err(|error| {
            format!(
                "Failed to parse daemon job row {} in {}: {error}",
                index + 1,
                table_path.display()
            )
        })?;
        rows.push(row);
    }
    Ok(rows)
}

fn append_daemon_job_rows(table_path: &Path, rows: &[DaemonJobRow]) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    ensure_daemon_job_table_path_is_not_symlink(table_path)?;
    let parent = table_path
        .parent()
        .ok_or_else(|| "Daemon job table path has no parent directory".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create daemon job table directory: {error}"))?;
    ensure_daemon_job_table_path_is_not_symlink(table_path)?;
    if daemon_job_table_path_exists_as_non_regular_file(table_path)? {
        return Err(format!(
            "Refusing to append daemon job table '{}': path is not a regular file",
            table_path.display()
        ));
    }
    let mut file = open_daemon_job_table_for_append(table_path)
        .map_err(|error| format!("Failed to open daemon job table for append: {error}"))?;

    let mut buffer = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut buffer, row)
            .map_err(|error| format!("Failed to serialize daemon job row: {error}"))?;
        buffer.push(b'\n');
    }

    file.write_all(&buffer)
        .map_err(|error| format!("Failed to write daemon job rows: {error}"))?;

    file.sync_all()
        .map_err(|error| format!("Failed to sync daemon job table: {error}"))
}

fn open_daemon_job_table_for_read(table_path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_daemon_job_table_open_options(&mut options);
    options.open(table_path)
}

fn open_daemon_job_table_for_append(table_path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    configure_daemon_job_table_open_options(&mut options);
    options.open(table_path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_daemon_job_table_open_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_daemon_job_table_open_options(_options: &mut OpenOptions) {}

fn ensure_daemon_job_table_path_is_not_symlink(table_path: &Path) -> Result<(), String> {
    if let Some(symlink_path) = first_existing_symlink_component(table_path)? {
        return Err(format!(
            "Refusing to access daemon job table '{}': path traverses symbolic link '{}'",
            table_path.display(),
            symlink_path.display()
        ));
    }
    Ok(())
}

fn daemon_job_table_path_is_regular_file(
    table_path: &Path,
    operation: &str,
) -> Result<bool, String> {
    ensure_daemon_job_table_path_is_not_symlink(table_path)?;
    match fs::symlink_metadata(table_path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Refusing to {operation} daemon job table '{}': path is not a regular file",
            table_path.display()
        )),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(format!(
            "Failed to inspect daemon job table '{}': {error}",
            table_path.display()
        )),
    }
}

fn daemon_job_table_path_exists_as_non_regular_file(table_path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(table_path) {
        Ok(metadata) => Ok(!metadata.file_type().is_file()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(format!(
            "Failed to inspect daemon job table '{}': {error}",
            table_path.display()
        )),
    }
}

fn first_existing_symlink_component(path: &Path) -> Result<Option<PathBuf>, String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(format!(
                    "Failed to inspect daemon job table path component '{}': {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(None)
}

fn latest_daemon_rows(rows: &[DaemonJobRow]) -> Vec<DaemonJobRow> {
    let mut by_key = BTreeMap::new();
    for row in rows {
        by_key.insert(row.daemon_job_key.clone(), row.clone());
    }
    by_key.into_values().collect()
}

fn row_from_result(
    workspace_path: &Path,
    run_id: &str,
    tick: u32,
    tick_started_at: &str,
    tick_completed_at: &str,
    result: &JobRunResult,
) -> DaemonJobRow {
    let outcome = result.outcome.as_str();
    DaemonJobRow {
        schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(),
        row_id: row_id(run_id, tick, &result.job_id, outcome),
        run_id: run_id.to_owned(),
        daemon_job_key: daemon_job_key(run_id, tick, &result.job_id),
        runner_job_id: result.job_id.clone(),
        tick,
        job_type: result.job_type.as_str().to_owned(),
        status: outcome.to_owned(),
        outcome: Some(outcome.to_owned()),
        workspace: workspace_path.to_string_lossy().into_owned(),
        write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
        reason: format!("daemon foreground tick {tick} completed"),
        started_at: Some(tick_started_at.to_owned()),
        completed_at: Some(tick_completed_at.to_owned()),
        recorded_at: Utc::now().to_rfc3339(),
        duration_ms: Some(result.duration_ms),
        items_processed: result.items_processed,
        error: result.error.clone(),
        dry_run: result.dry_run,
        durable_mutation: result
            .details
            .as_ref()
            .and_then(|details| details.get("durableMutation"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        recovered_from_orphan: false,
        recovery_reason: None,
    }
}

fn daemon_run_id(workspace_path: &Path, recorded_at: &str) -> String {
    let input = format!("{}|{recorded_at}", workspace_path.display());
    let digest = blake3::hash(input.as_bytes()).to_hex().to_string();
    format!("daemon-run-{}", &digest[..16])
}

fn daemon_job_key(run_id: &str, tick: u32, runner_job_id: &str) -> String {
    format!("{run_id}:tick-{tick:06}:{runner_job_id}")
}

fn runner_job_id(offset: usize) -> String {
    format!("job-{:06}", offset.saturating_add(1))
}

fn row_id(run_id: &str, tick: u32, runner_job_id: &str, phase: &str) -> String {
    let input = format!(
        "{run_id}|{tick}|{runner_job_id}|{phase}|{}",
        Utc::now().to_rfc3339()
    );
    let digest = blake3::hash(input.as_bytes()).to_hex().to_string();
    format!("daemon-row-{}", &digest[..20])
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

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

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn parse_request(raw: &str) -> Result<ServeHttpRequest, String> {
        parse_serve_http_request(raw.as_bytes(), &ServeLimits::default())
            .map_err(|error| error.to_string())
    }

    fn plan_request(raw: &str) -> Result<ServeDispatchPlan, String> {
        let request = parse_request(raw)?;
        serve_dispatch_plan(&request).map_err(|error| error.to_string())
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

    #[test]
    fn serve_startup_report_marks_loopback_ready_without_exposing_token() -> TestResult {
        let token = "01234567890123456789012345678901";
        let report = serve_startup_report_json(&ServeStartupOptions::default(), Some(token))
            .map_err(|error| error.to_string())?;

        ensure(
            report["schema"].as_str(),
            Some(SERVE_STARTUP_SCHEMA_V1),
            "startup schema",
        )?;
        ensure(
            report["bind"]["host"].as_str(),
            Some(DEFAULT_SERVE_HOST),
            "default bind host",
        )?;
        ensure(
            report["bind"]["loopbackOnly"].as_bool(),
            Some(true),
            "default bind is loopback",
        )?;
        ensure(
            report["tokenPosture"]["state"].as_str(),
            Some("configured"),
            "token posture",
        )?;
        ensure(
            report["tokenPosture"]["tokenMaterialExposed"].as_bool(),
            Some(false),
            "token exposure",
        )?;
        ensure(
            report["readiness"]["canAcceptConnections"].as_bool(),
            Some(true),
            "startup readiness",
        )?;
        ensure(
            report["endpoints"].as_array().is_some_and(|endpoints| {
                endpoints.iter().all(|endpoint| {
                    endpoint["authRequired"].as_bool() == Some(true)
                        && !endpoint.to_string().contains(token)
                })
            }),
            true,
            "endpoint catalog auth posture",
        )?;
        ensure(
            report.to_string().contains(token),
            false,
            "startup report must not expose token material",
        )
    }

    #[test]
    fn serve_startup_report_requires_non_loopback_opt_in_and_token() -> TestResult {
        let non_loopback = ServeStartupOptions {
            host: "0.0.0.0".to_owned(),
            ..ServeStartupOptions::default()
        };
        let opt_in = ServeStartupOptions {
            allow_non_loopback: true,
            ..non_loopback.clone()
        };

        let opt_in_error = match serve_startup_report_json(
            &non_loopback,
            Some("01234567890123456789012345678901"),
        ) {
            Ok(report) => {
                return Err(format!(
                    "non-loopback without opt-in should fail, got {report}"
                ));
            }
            Err(error) => error,
        };
        ensure(
            opt_in_error.code(),
            "policy_denied",
            "non-loopback opt-in error code",
        )?;

        let token_error = match serve_startup_report_json(&opt_in, Some("short-token")) {
            Ok(report) => {
                return Err(format!(
                    "non-loopback with weak token should fail, got {report}"
                ));
            }
            Err(error) => error,
        };
        ensure(
            token_error.code(),
            "policy_denied",
            "non-loopback token error code",
        )
    }

    #[test]
    fn serve_listener_bind_allows_loopback_with_token_without_exposing_token() -> TestResult {
        let token = "01234567890123456789012345678901";
        let options = ServeStartupOptions {
            port: 0,
            ..ServeStartupOptions::default()
        };
        let binding =
            bind_serve_listener(&options, Some(token)).map_err(|error| error.to_string())?;
        let local_addr = binding
            .listener
            .local_addr()
            .map_err(|error| error.to_string())?;

        ensure(local_addr.ip().is_loopback(), true, "listener loopback")?;
        ensure(
            binding.metadata["schema"].as_str(),
            Some(SERVE_STARTUP_SCHEMA_V1),
            "binding schema",
        )?;
        ensure(
            binding.metadata["listener"]["requestedHost"].as_str(),
            Some(DEFAULT_SERVE_HOST),
            "requested host",
        )?;
        ensure(
            binding.metadata["listener"]["requestedPort"].as_u64(),
            Some(0),
            "requested port",
        )?;
        ensure(
            binding.metadata["listener"]["boundPort"].as_u64(),
            Some(u64::from(local_addr.port())),
            "bound port",
        )?;
        ensure(
            binding.metadata["listener"]["nonblocking"].as_bool(),
            Some(true),
            "nonblocking listener",
        )?;
        ensure(
            binding.metadata["startup"]["readiness"]["canAcceptConnections"].as_bool(),
            Some(true),
            "startup can accept",
        )?;
        ensure(
            binding.metadata.to_string().contains(token),
            false,
            "binding metadata must not expose token material",
        )
    }

    #[test]
    fn serve_listener_bind_refuses_policy_failures_before_socket_bind() -> TestResult {
        let missing_token_error = match bind_serve_listener(&ServeStartupOptions::default(), None) {
            Ok(binding) => {
                return Err(format!(
                    "missing token should fail before bind, got {:?}",
                    binding.metadata
                ));
            }
            Err(error) => error,
        };
        ensure(
            missing_token_error.code(),
            "policy_denied",
            "missing token error code",
        )?;

        let non_loopback = ServeStartupOptions {
            host: "0.0.0.0".to_owned(),
            port: 0,
            ..ServeStartupOptions::default()
        };
        let non_loopback_error =
            match bind_serve_listener(&non_loopback, Some("01234567890123456789012345678901")) {
                Ok(binding) => {
                    return Err(format!(
                        "non-loopback should fail before bind, got {:?}",
                        binding.metadata
                    ));
                }
                Err(error) => error,
            };
        ensure(
            non_loopback_error.code(),
            "policy_denied",
            "non-loopback error code",
        )
    }

    #[test]
    fn serve_http_parser_maps_search_query_and_bearer_auth() -> TestResult {
        let token = "01234567890123456789012345678901";
        let request = parse_serve_http_request(
            format!(
                "GET /v1/search?q=release+check&tag=rust%2Bcli HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
            )
            .as_bytes(),
            &ServeLimits::default(),
        )
        .map_err(|error| error.to_string())?;

        ensure(request.method.as_str(), "GET", "request method")?;
        ensure(request.endpoint, ServeEndpoint::Search, "mapped endpoint")?;
        ensure(
            request.query.get("q").cloned(),
            Some(vec!["release check".to_owned()]),
            "decoded query",
        )?;
        ensure(
            request.query.get("tag").cloned(),
            Some(vec!["rust+cli".to_owned()]),
            "percent-decoded query",
        )?;
        ensure(
            serve_auth_state(&request, Some(token)),
            "accepted",
            "bearer auth state",
        )
    }

    #[test]
    fn serve_dispatch_plan_maps_read_only_cli_surfaces() -> TestResult {
        let status = plan_request("GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
        ensure(status.handler_surface, "cli.status", "status handler")?;
        ensure(
            status.cli_argv,
            argv(&["ee", "status", "--json"]),
            "status argv",
        )?;

        let search =
            plan_request("GET /v1/search?q=release+check HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
        ensure(search.handler_surface, "cli.search", "search handler")?;
        ensure(
            search.cli_argv,
            argv(&["ee", "search", "release check", "--json"]),
            "search argv",
        )?;

        let context = plan_request(
            "GET /v1/context?task=prepare+release HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )?;
        ensure(context.handler_surface, "cli.context", "context handler")?;
        ensure(
            context.cli_argv,
            argv(&["ee", "context", "prepare release", "--json"]),
            "context argv",
        )?;

        let why = plan_request(
            "GET /v1/why/mem_00000000000000000000000001 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )?;
        ensure(why.handler_surface, "cli.why", "why handler")?;
        ensure(
            why.cli_argv,
            argv(&["ee", "why", "mem_00000000000000000000000001", "--json"]),
            "why argv",
        )?;
        ensure(
            why.to_json()["handlerSurface"].as_str(),
            Some("cli.why"),
            "dispatch json handler",
        )?;

        let brief = plan_request("GET /v1/swarm/brief HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
        ensure(
            brief.cli_argv,
            argv(&["ee", "swarm", "brief", "--json"]),
            "swarm brief argv",
        )
    }

    #[test]
    fn serve_dispatch_plan_bounds_non_cli_surfaces() -> TestResult {
        let durable = plan_request(
            "POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}",
        )?;
        ensure(
            durable.handler_surface,
            "serve.durable_write_placeholder",
            "durable handler",
        )?;
        ensure(durable.mutable, true, "durable write is mutable")?;
        ensure(
            durable.cli_argv.is_empty(),
            true,
            "durable write has no CLI argv",
        )?;

        let events = plan_request("GET /v1/events HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
        ensure(events.handler_surface, "serve.sse.events", "events handler")?;
        ensure(events.mutable, false, "events are read-only")?;
        ensure(events.sse_stream, true, "events are SSE")?;
        ensure(events.cli_argv.is_empty(), true, "events have no CLI argv")
    }

    #[test]
    fn serve_dispatch_plan_rejects_unknown_or_ambiguous_intents() -> TestResult {
        let unknown = parse_request("GET /v1/missing HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
        let unknown_error = match serve_dispatch_plan(&unknown) {
            Ok(plan) => return Err(format!("unknown endpoint should fail, got {plan:?}")),
            Err(error) => error,
        };
        ensure(unknown_error.code(), "usage", "unknown endpoint error")?;

        for raw in [
            "GET /v1/search HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "GET /v1/search?q=one&q=two HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "GET /v1/context?task=+ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "GET /v1/why/mem_1/extra HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        ] {
            let request = parse_request(raw)?;
            let error = match serve_dispatch_plan(&request) {
                Ok(plan) => return Err(format!("ambiguous intent should fail, got {plan:?}")),
                Err(error) => error,
            };
            ensure(error.code(), "usage", "ambiguous intent error")?;
        }

        Ok(())
    }

    #[test]
    fn serve_http_parser_rejects_keepalive_body_bytes() -> TestResult {
        let error = match parse_serve_http_request(
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\nabc",
            &ServeLimits::default(),
        ) {
            Ok(request) => return Err(format!("extra body bytes should fail, got {request:?}")),
            Err(error) => error,
        };
        ensure(error.code(), "usage", "extra body byte error code")
    }

    #[test]
    fn serve_auth_failure_envelope_is_error_v2_before_dispatch() -> TestResult {
        let request = parse_serve_http_request(
            b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            &ServeLimits::default(),
        )
        .map_err(|error| error.to_string())?;
        let auth_state = serve_auth_state(&request, Some("01234567890123456789012345678901"));
        let envelope = serve_auth_failure_envelope("req-1", &request, auth_state, 7);

        ensure(
            envelope["schema"].as_str(),
            Some(SERVE_ENDPOINT_SCHEMA_V1),
            "endpoint schema",
        )?;
        ensure(
            envelope["request"]["auth"]["state"].as_str(),
            Some("missing"),
            "auth state",
        )?;
        ensure(
            envelope["response"]["statusCode"].as_u64(),
            Some(401),
            "http status",
        )?;
        ensure(
            envelope["response"]["payload"]["schema"].as_str(),
            Some("ee.error.v2"),
            "wrapped error schema",
        )?;
        ensure(
            envelope
                .to_string()
                .contains("01234567890123456789012345678901"),
            false,
            "auth failure must not expose token material",
        )
    }

    #[test]
    fn serve_sse_event_is_read_only_endpoint_envelope() -> TestResult {
        let frame = render_serve_sse_event("complete", true, &json!({"ok": true}));
        ensure(
            frame.starts_with("event: complete\n"),
            true,
            "sse event line",
        )?;
        let data_line = frame
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .ok_or_else(|| "missing sse data line".to_owned())?;
        let event: JsonValue =
            serde_json::from_str(data_line).map_err(|error| error.to_string())?;

        ensure(
            event["schema"].as_str(),
            Some(SERVE_ENDPOINT_SCHEMA_V1),
            "sse endpoint schema",
        )?;
        ensure(
            event["request"]["endpoint"].as_str(),
            Some("events"),
            "sse endpoint",
        )?;
        ensure(
            event["sse"]["readOnly"].as_bool(),
            Some(true),
            "sse read-only",
        )?;
        ensure(
            event["sse"]["terminal"].as_bool(),
            Some(true),
            "sse terminal",
        )?;
        ensure(
            event["response"]["payload"]["schema"].as_str(),
            Some("ee.response.v2"),
            "sse wrapped response schema",
        )?;
        ensure(
            event["response"]["payload"]["data"]["ok"].as_bool(),
            Some(true),
            "sse wrapped data",
        )
    }

    #[test]
    fn serve_http_json_response_sets_close_headers_and_exact_length() -> TestResult {
        let payload = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {"ok": true},
            "degraded": []
        });
        let response = render_serve_http_json_response(200, &payload);
        ensure(
            response.starts_with("HTTP/1.1 200 OK\r\n"),
            true,
            "status line",
        )?;

        let (headers, body) = split_http_response(&response)?;
        ensure(
            header_value(headers, "Content-Type"),
            Some("application/json; charset=utf-8"),
            "json content type",
        )?;
        ensure(
            header_value(headers, "Cache-Control"),
            Some("no-store"),
            "cache control",
        )?;
        ensure(
            header_value(headers, "Connection"),
            Some("close"),
            "connection close",
        )?;
        let content_length = header_value(headers, "Content-Length")
            .ok_or_else(|| "missing JSON content length".to_owned())?
            .parse::<usize>()
            .map_err(|error| error.to_string())?;
        ensure(
            content_length,
            body.len(),
            "content length must match body bytes",
        )?;
        let body_json: JsonValue = serde_json::from_str(body).map_err(|error| error.to_string())?;
        ensure(
            body_json["schema"].as_str(),
            Some("ee.response.v2"),
            "body schema",
        )
    }

    #[test]
    fn serve_http_json_response_preserves_auth_failure_without_token_material() -> TestResult {
        let token = "01234567890123456789012345678901";
        let request = parse_request("GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
        let auth_state = serve_auth_state(&request, Some(token));
        let payload = serve_auth_failure_envelope("req-1", &request, auth_state, 3);
        let response = render_serve_http_json_response(401, &payload);

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
        let (headers, body) = split_http_response(&response)?;
        ensure(
            header_value(headers, "Content-Length").is_some(),
            true,
            "auth response content length",
        )?;
        let body_json: JsonValue = serde_json::from_str(body).map_err(|error| error.to_string())?;
        ensure(
            body_json["response"]["statusCode"].as_u64(),
            Some(401),
            "auth payload status",
        )
    }

    #[test]
    fn serve_http_sse_response_is_streaming_and_unbuffered_by_contract() -> TestResult {
        let frame = render_serve_sse_event("complete", true, &json!({"ok": true}));
        let response = render_serve_http_sse_response(&frame);
        ensure(
            response.starts_with("HTTP/1.1 200 OK\r\n"),
            true,
            "sse status line",
        )?;
        ensure(response.ends_with(&frame), true, "sse frame body")?;

        let (headers, body) = split_http_response(&response)?;
        ensure(
            header_value(headers, "Content-Type"),
            Some("text/event-stream; charset=utf-8"),
            "sse content type",
        )?;
        ensure(
            header_value(headers, "Content-Length").is_none(),
            true,
            "sse stream omits content length",
        )?;
        ensure(
            body.starts_with("event: complete\n"),
            true,
            "sse event body",
        )
    }

    #[test]
    fn serve_transport_exchange_returns_dispatch_envelope_without_executing_business_logic()
    -> TestResult {
        let token = "01234567890123456789012345678901";
        let raw = format!(
            "GET /v1/search?q=release+check HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        let response = render_serve_transport_exchange(
            "req-transport-1",
            raw.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            11,
        );

        ensure(
            response.starts_with("HTTP/1.1 200 OK\r\n"),
            true,
            "transport status line",
        )?;
        let (_, body) = split_http_response(&response)?;
        let envelope: JsonValue = serde_json::from_str(body).map_err(|error| error.to_string())?;
        ensure(
            envelope["schema"].as_str(),
            Some(SERVE_ENDPOINT_SCHEMA_V1),
            "transport envelope schema",
        )?;
        ensure(
            envelope["request"]["auth"]["state"].as_str(),
            Some("accepted"),
            "accepted auth state",
        )?;
        ensure(
            envelope["response"]["payload"]["data"]["businessLogicExecuted"].as_bool(),
            Some(false),
            "business logic boundary",
        )?;
        ensure(
            envelope["response"]["payload"]["data"]["dispatchPlan"]["handlerSurface"].as_str(),
            Some("cli.search"),
            "dispatch handler",
        )?;
        ensure(
            envelope["response"]["payload"]["data"]["dispatchPlan"]["cliArgv"]
                .as_array()
                .is_some_and(|argv| {
                    argv.iter().map(JsonValue::as_str).collect::<Vec<_>>()
                        == vec![
                            Some("ee"),
                            Some("search"),
                            Some("release check"),
                            Some("--json"),
                        ]
                }),
            true,
            "dispatch argv",
        )
    }

    #[test]
    fn serve_transport_exchange_rejects_missing_auth_before_dispatch() -> TestResult {
        let token = "01234567890123456789012345678901";
        let response = render_serve_transport_exchange(
            "req-transport-auth",
            b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            &ServeLimits::default(),
            Some(token),
            5,
        );

        ensure(
            response.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
            true,
            "missing auth status line",
        )?;
        ensure(
            response.contains(token),
            false,
            "transport auth response must not expose token",
        )?;
        let (_, body) = split_http_response(&response)?;
        let envelope: JsonValue = serde_json::from_str(body).map_err(|error| error.to_string())?;
        ensure(
            envelope["request"]["auth"]["state"].as_str(),
            Some("missing"),
            "missing auth state",
        )
    }

    #[test]
    fn serve_transport_exchange_maps_parse_and_unknown_endpoint_errors() -> TestResult {
        let token = "01234567890123456789012345678901";
        let parse_response = render_serve_transport_exchange(
            "req-parse",
            b"GET /v1/status HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n",
            &ServeLimits::default(),
            Some(token),
            0,
        );
        ensure(
            parse_response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            true,
            "parse error status",
        )?;
        let (_, parse_body) = split_http_response(&parse_response)?;
        let parse_payload: JsonValue =
            serde_json::from_str(parse_body).map_err(|error| error.to_string())?;
        ensure(
            parse_payload["schema"].as_str(),
            Some("ee.error.v2"),
            "parse error schema",
        )?;

        let raw_unknown = format!(
            "GET /v1/missing HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        let unknown_response = render_serve_transport_exchange(
            "req-unknown",
            raw_unknown.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            2,
        );
        ensure(
            unknown_response.starts_with("HTTP/1.1 404 Not Found\r\n"),
            true,
            "unknown endpoint status",
        )?;
        let (_, unknown_body) = split_http_response(&unknown_response)?;
        let unknown_envelope: JsonValue =
            serde_json::from_str(unknown_body).map_err(|error| error.to_string())?;
        ensure(
            unknown_envelope["schema"].as_str(),
            Some(SERVE_ENDPOINT_SCHEMA_V1),
            "unknown endpoint envelope",
        )?;
        ensure(
            unknown_envelope["request"]["endpoint"].as_str(),
            Some("unknown"),
            "unknown endpoint metadata",
        )?;
        ensure(
            unknown_envelope["response"]["payload"]["schema"].as_str(),
            Some("ee.error.v2"),
            "unknown wrapped error",
        )
    }

    #[test]
    fn serve_transport_exchange_returns_sse_header_frame_for_events() -> TestResult {
        let token = "01234567890123456789012345678901";
        let raw = format!(
            "GET /v1/events HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        let response = render_serve_transport_exchange(
            "req-events",
            raw.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            1,
        );

        let (headers, body) = split_http_response(&response)?;
        ensure(
            header_value(headers, "Content-Type"),
            Some("text/event-stream; charset=utf-8"),
            "transport sse content type",
        )?;
        ensure(
            header_value(headers, "Content-Length").is_none(),
            true,
            "transport sse no content length",
        )?;
        ensure(
            body.starts_with("event: header\n"),
            true,
            "transport sse header event",
        )?;
        let data_line = body
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .ok_or_else(|| "missing transport sse data line".to_owned())?;
        let event: JsonValue =
            serde_json::from_str(data_line).map_err(|error| error.to_string())?;
        ensure(
            event["response"]["payload"]["data"]["dispatchPlan"]["endpoint"].as_str(),
            Some("events"),
            "events dispatch plan",
        )
    }

    #[test]
    fn daemon_foreground_persists_rows_and_status_reports_write_owner() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut options = DaemonForegroundOptions::new(temp.path().to_string_lossy().into_owned());
        options.interval_ms = 0;
        options.job_types = vec![JobType::HealthCheck];

        let plan = record_daemon_foreground_start(temp.path(), &options)?;
        ensure(plan.rows.len(), 1, "planned rows")?;

        let report = crate::steward::run_daemon_foreground(&options)?;
        let terminal_rows = record_daemon_foreground_report(temp.path(), &report, &plan.run_id)?;
        ensure(terminal_rows.len(), 1, "terminal rows")?;

        let rows = load_daemon_job_rows(temp.path())?;
        ensure(rows.len(), 2, "persisted row count")?;

        let status = daemon_status_report(temp.path(), &[JobType::HealthCheck], 5)?;
        ensure(status.open_job_count, 0, "open jobs")?;
        ensure(status.row_count, 2, "status row count")?;
        let json = status.data_json();
        ensure(
            json["writeOwner"]["identity"].as_str(),
            Some(DAEMON_WRITE_OWNER_IDENTITY),
            "write owner identity",
        )?;
        ensure(
            json["writeOwner"]["spool"]["backpressureCode"].as_str(),
            Some(crate::core::WRITE_SPOOL_BACKPRESSURE_CODE),
            "backpressure code",
        )?;
        ensure(
            json["recentOutcomes"][0]["status"].as_str(),
            Some("success"),
            "recent terminal status",
        )
    }

    #[test]
    fn daemon_recovery_cancels_orphaned_planned_jobs() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut options = DaemonForegroundOptions::new(temp.path().to_string_lossy().into_owned());
        options.interval_ms = 0;
        options.job_types = vec![JobType::HealthCheck];

        let _plan = record_daemon_foreground_start(temp.path(), &options)?;
        let before = daemon_status_report(temp.path(), &[JobType::HealthCheck], 5)?;
        ensure(before.open_job_count, 1, "open before recovery")?;

        let recovery = recover_orphaned_daemon_jobs(temp.path(), "simulated daemon restart")?;
        ensure(recovery.open_jobs_cancelled, 1, "cancelled orphan count")?;
        ensure(recovery.scanned_rows, 1, "recovery scanned rows")?;

        let after = daemon_status_report(temp.path(), &[JobType::HealthCheck], 5)?;
        ensure(after.open_job_count, 0, "open after recovery")?;
        ensure(after.row_count, 2, "rows after recovery")?;
        let json = after.data_json();
        ensure(
            json["recentOutcomes"][0]["status"].as_str(),
            Some("cancelled"),
            "cancelled status",
        )?;
        ensure(
            json["recentOutcomes"][0]["recoveredFromOrphan"].as_bool(),
            Some(true),
            "recovered marker",
        )
    }

    #[test]
    fn daemon_status_handles_missing_table_without_mutation() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let status = daemon_status_report(temp.path(), &[JobType::DecaySweep], 5)?;
        ensure(status.row_count, 0, "row count")?;
        ensure(status.open_job_count, 0, "open job count")?;
        ensure(
            daemon_job_table_path(temp.path()).exists(),
            false,
            "status must not create table",
        )?;
        ensure(
            status.data_json()["running"].as_bool(),
            Some(false),
            "running flag",
        )
    }

    #[test]
    fn daemon_status_degraded_entries_are_aggregated() -> TestResult {
        let degraded = aggregate_daemon_status_degraded([
            daemon_background_mode_degradation_input(),
            daemon_background_mode_degradation_input(),
        ]);
        let value = serde_json::to_value(&degraded).map_err(|error| error.to_string())?;

        ensure(degraded.len(), 1, "aggregated daemon degraded count")?;
        ensure(
            value[0]["code"].as_str(),
            Some("daemon_background_mode_unimplemented"),
            "daemon degraded code",
        )?;
        ensure(
            value[0]["severity"].as_str(),
            Some("low"),
            "daemon severity",
        )?;
        ensure(
            value[0]["sources"].clone(),
            json!(["daemon_status"]),
            "daemon degraded source",
        )
    }

    #[test]
    fn daemon_job_rows_distinguish_missing_table_from_malformed_table() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let table_path = daemon_job_table_path(temp.path());

        let missing_rows = load_daemon_job_rows(temp.path())?;
        ensure(missing_rows.len(), 0, "missing table rows")?;
        ensure(table_path.exists(), false, "missing table remains absent")?;

        let parent = table_path
            .parent()
            .ok_or_else(|| format!("missing parent for {}", table_path.display()))?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        fs::write(&table_path, "not-json\n").map_err(|error| error.to_string())?;

        let error = match load_daemon_job_rows(temp.path()) {
            Ok(rows) => {
                return Err(format!(
                    "malformed daemon job table should fail, got {rows:?}"
                ));
            }
            Err(error) => error,
        };
        ensure(
            error.contains("Failed to parse daemon job row 1"),
            true,
            "malformed table parse error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn daemon_job_table_rejects_symlinked_ee_directory_before_write() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_ee = temp.path().join("real-ee");
        fs::create_dir_all(&real_ee).map_err(|error| error.to_string())?;
        symlink(&real_ee, temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let mut options = DaemonForegroundOptions::new(temp.path().to_string_lossy().into_owned());
        options.interval_ms = 0;
        options.job_types = vec![JobType::HealthCheck];

        let error = match record_daemon_foreground_start(temp.path(), &options) {
            Ok(plan) => return Err(format!("symlinked .ee directory should fail, got {plan:?}")),
            Err(error) => error,
        };
        ensure(
            error.contains("path traverses symbolic link"),
            true,
            "symlinked .ee rejection",
        )?;
        ensure(
            real_ee.join("daemon-jobs.jsonl").exists(),
            false,
            "daemon job table must not be written through symlinked .ee",
        )
    }

    #[cfg(unix)]
    #[test]
    fn daemon_job_table_rejects_symlinked_table_before_read_or_write() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let ee_dir = temp.path().join(".ee");
        fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let target = temp.path().join("outside-daemon-jobs.jsonl");
        fs::write(&target, "").map_err(|error| error.to_string())?;
        symlink(&target, daemon_job_table_path(temp.path())).map_err(|error| error.to_string())?;

        let read_error = match load_daemon_job_rows(temp.path()) {
            Ok(rows) => return Err(format!("symlinked table read should fail, got {rows:?}")),
            Err(error) => error,
        };
        ensure(
            read_error.contains("path traverses symbolic link"),
            true,
            "symlinked table read rejection",
        )?;

        let mut options = DaemonForegroundOptions::new(temp.path().to_string_lossy().into_owned());
        options.interval_ms = 0;
        options.job_types = vec![JobType::HealthCheck];
        let write_error = match record_daemon_foreground_start(temp.path(), &options) {
            Ok(plan) => return Err(format!("symlinked table write should fail, got {plan:?}")),
            Err(error) => error,
        };
        ensure(
            write_error.contains("path traverses symbolic link"),
            true,
            "symlinked table write rejection",
        )?;
        ensure(
            fs::read_to_string(&target).map_err(|error| error.to_string())?,
            String::new(),
            "symlink target must not receive daemon rows",
        )
    }

    #[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
    #[test]
    fn daemon_job_table_open_helpers_reject_symlinked_final_path() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let table_path = daemon_job_table_path(temp.path());
        let parent = table_path
            .parent()
            .ok_or_else(|| format!("missing parent for {}", table_path.display()))?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let outside_table = temp.path().join("outside-daemon-jobs.jsonl");
        fs::write(&outside_table, "").map_err(|error| error.to_string())?;
        symlink(&outside_table, &table_path).map_err(|error| error.to_string())?;

        match open_daemon_job_table_for_read(&table_path) {
            Ok(_) => return Err("read open should reject symlinked daemon job table".to_owned()),
            Err(_) => {}
        }
        match open_daemon_job_table_for_append(&table_path) {
            Ok(_) => return Err("append open should reject symlinked daemon job table".to_owned()),
            Err(_) => {}
        }
        ensure(
            fs::read_to_string(&outside_table).map_err(|error| error.to_string())?,
            String::new(),
            "symlink target must not receive daemon rows",
        )
    }

    #[test]
    fn daemon_job_table_rejects_non_regular_table_before_read_or_write() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let table_path = daemon_job_table_path(temp.path());
        let parent = table_path
            .parent()
            .ok_or_else(|| format!("missing parent for {}", table_path.display()))?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        fs::create_dir(&table_path).map_err(|error| error.to_string())?;

        let read_error = match load_daemon_job_rows(temp.path()) {
            Ok(rows) => {
                return Err(format!(
                    "non-regular daemon job table should fail, got {rows:?}"
                ));
            }
            Err(error) => error,
        };
        ensure(
            read_error.contains("path is not a regular file"),
            true,
            "non-regular table read rejection",
        )?;

        let mut options = DaemonForegroundOptions::new(temp.path().to_string_lossy().into_owned());
        options.interval_ms = 0;
        options.job_types = vec![JobType::HealthCheck];
        let write_error = match record_daemon_foreground_start(temp.path(), &options) {
            Ok(plan) => {
                return Err(format!(
                    "non-regular daemon job table should fail, got {plan:?}"
                ));
            }
            Err(error) => error,
        };
        ensure(
            write_error.contains("path is not a regular file"),
            true,
            "non-regular table write rejection",
        )
    }
}
