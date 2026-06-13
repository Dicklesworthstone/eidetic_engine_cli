use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::core::context::{
    ContextPackOptions, ContextPackOutputOptions, run_context_pack_with_performance,
};
use crate::core::doctor::DoctorReport;
use crate::core::memory::{RememberMemoryOptions, RememberMemoryReport, remember_memory};
use crate::core::search::{SearchDedupMode, SearchOptions, SearchSourceMode, run_search};
use crate::core::subscribe::{SubscribePollOptions, parse_subscribe_filter, poll_memory_deltas};
use crate::core::swarm_brief::{
    SwarmBriefCollectOptions, SwarmBriefSourceKind, SystemSwarmBriefCommandRunner,
    collect_swarm_brief,
};
use crate::core::why::{WhyOptions, explain_memory};
use crate::models::{DomainError, MemoryScope, QueryFilters, RESPONSE_SCHEMA_V2, RedactionLevel};
use crate::steward::{DaemonForegroundOptions, DaemonForegroundReport, JobRunResult, JobType};

pub const SUBSYSTEM: &str = "serve";
pub const DAEMON_JOB_TABLE_SCHEMA_V1: &str = "ee.steward.daemon_job_table.v1";
pub const DAEMON_JOB_ROW_SCHEMA_V1: &str = "ee.steward.daemon_job_row.v1";
/// Hard upper bound on the byte length of the daemon-job table read by
/// `load_daemon_job_rows`. The table at `.ee/daemon-jobs.jsonl` is an
/// append-only JSONL ledger that grows on every foreground daemon start
/// (`record_daemon_foreground_start` → `append_daemon_job_rows`). The
/// previous `BufReader::new(file).lines()` shape allocated each line into
/// a fresh `String` whose capacity grows to fit the line, so a peer-planted
/// (or runaway-emitter) multi-GB single line (no embedded `\n`) would
/// force a matching String pre-size and OOM the daemon read path.
/// 16 MiB matches `HANDOFF_FILE_MAX_BYTES` and the parallel cap on
/// `.ee/coordination-fallback-evidence.jsonl` in
/// `src/core/why.rs::COORDINATION_FALLBACK_LEDGER_MAX_BYTES`; daemon job
/// rows are small JSON records (a few hundred bytes each), so 16 MiB is
/// thousands of rows of headroom while still bounding worst-case
/// allocation. Truncation past the cap surfaces through the existing
/// `serde_json::from_str` error path, so callers see an explicit parse
/// failure instead of silent data loss.
const DAEMON_JOB_TABLE_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub const DAEMON_STATUS_SCHEMA_V1: &str = "ee.steward.daemon_status.v1";
pub const DAEMON_RECOVERY_SCHEMA_V1: &str = "ee.steward.daemon_recovery.v1";
pub const DAEMON_WRITE_OWNER_IDENTITY: &str = "ee-daemon-single-write-owner";
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
            Self::Context => Some("ee pack \"<task>\" --json"),
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
    pub body: Vec<u8>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeConnectionExchange {
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub response_status_line: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeAcceptedConnection {
    pub accept_attempts: usize,
    pub peer_addr: SocketAddr,
    pub exchange: ServeConnectionExchange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServeForegroundOnceReport {
    pub bound_addr: SocketAddr,
    pub listener_metadata: JsonValue,
    pub accepted: ServeAcceptedConnection,
}

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
                handler_surface: "cli.pack",
                cli_argv: vec![
                    "ee".to_owned(),
                    "pack".to_owned(),
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
            handler_surface: "serve.durable_write",
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
    // RFC 7230 §3.1.1:
    //   request-line = method SP request-target SP HTTP-version CRLF
    // SP is a single 0x20 — exactly two of them in the request line, no
    // leading/trailing/interior runs and no other whitespace characters.
    //
    // The previous parser used `split_whitespace`, which accepts ANY
    // Unicode whitespace (TAB 0x09, VT 0x0B, FF 0x0C, NEL, non-breaking
    // space, etc.) AND collapses runs of whitespace into a single
    // separator. That is a request-line whitespace-normalization mismatch
    // primitive in the same family as the chunked-in-TE smuggling vector
    // that 1b426516 closed: a strict upstream proxy that parses
    // `"GET\t/admin\tHTTP/1.1"` as malformed (rejected) while we parse it
    // as `("GET", "/admin", "HTTP/1.1")` — or, more dangerous, a permissive
    // proxy that interprets the tab as part of the request-target while we
    // strip it — produces a routing disagreement that lets an attacker
    // smuggle requests through whatever fronting layer is deployed in
    // front of ee serve v2. Defending the parser at the source closes the
    // class even before the proxy stance is decided.
    //
    // Use `split(' ')` and require exactly three non-empty segments. This
    // rejects:
    //   - tabs, vertical tab, form feed, NEL, etc. inside the request line
    //   - consecutive spaces ("GET  /  HTTP/1.1")
    //   - leading or trailing space ("  GET / HTTP/1.1 ")
    //   - a fourth segment after the version
    // while still accepting the canonical `METHOD SP target SP version`
    // shape that every existing test case uses.
    let parts: Vec<&str> = request_line.split(' ').collect();
    if parts.len() != 3 || parts.iter().any(|segment| segment.is_empty()) {
        return Err(serve_usage_error(
            "HTTP request line must be exactly `METHOD SP target SP version` with single-space separators.",
        ));
    }
    let method = parts[0];
    let target = parts[1];
    let version = parts[2];
    if version != "HTTP/1.1" {
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
        // Reject duplicate headers outright. RFC 7230 §3.3.3 bullet 4
        // already requires rejecting requests with multiple
        // Content-Length values (even when the values match, the
        // disagreement between an upstream proxy that keeps the first
        // and this parser that previously kept the last via
        // `BTreeMap::insert` is the classic CL.CL smuggling pair).
        // Multiple Transfer-Encoding rows have the same framing-
        // disagreement risk. The chunked-in-TE-list guard below only
        // fires when a *single* TE row names chunked; two distinct TE
        // rows like `Transfer-Encoding: identity` + `Transfer-Encoding:
        // chunked` would have collapsed to a last-wins value before
        // that guard ran. A v2 parser that does not implement RFC 7230
        // §3.2.2 comma-combining for repeated names should reject all
        // duplicates loudly instead of silently overwriting; the v2
        // surface ships with this defense from the first slice.
        if headers.contains_key(&normalized_name) {
            return Err(serve_usage_error(format!(
                "HTTP header `{normalized_name}` appears more than once; \
                 the ee serve v2 parser rejects duplicate header rows \
                 to close request-smuggling framing disagreements."
            )));
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
    // Reject any `Transfer-Encoding` codings list that names `chunked`,
    // not just the bare exact-match `chunked` token. RFC 7230 §3.3.3
    // declares that a request carrying BOTH Transfer-Encoding and
    // Content-Length is a request-smuggling indicator: a permissive
    // upstream proxy can parse a list like `chunked, identity` as
    // chunked and forward a CL-framed body to this parser, which would
    // otherwise read the declared Content-Length bytes and split the
    // next request boundary on attacker-chosen bytes (classic CL.TE
    // smuggling). The exact-match check at the previous shape only
    // caught `chunked` in isolation, so `chunked, identity`,
    // `identity, chunked`, and `CHUNKED, gzip` all slipped through to
    // Content-Length framing. ee serve v1 still defers the whole HTTP
    // surface, but the parser ships with v2 unchanged unless the
    // smuggling vector is closed now.
    //
    // Also strip the `;`-delimited transfer-parameter suffix from each
    // coding before the case-insensitive token match. RFC 7230 §4.1
    // declares that `chunked` itself has no parameters, but a
    // permissive upstream proxy can still accept `chunked; foo=bar`
    // and frame the body as chunked. Without this strip, an attacker
    // who controls header construction at the fronting proxy can
    // append a no-op parameter (`chunked; q=1`, `chunked;ext=v`) to
    // any coding-list entry and bypass the bare-string guard that
    // 1b426516 just landed — letting `parse_serve_http_request` fall
    // through to Content-Length framing and reintroducing the same
    // CL.TE smuggling pair that fix was meant to close.
    if headers.get("transfer-encoding").is_some_and(|value| {
        value.split(',').any(|encoding| {
            let token = encoding.split(';').next().unwrap_or("").trim();
            token.eq_ignore_ascii_case("chunked")
        })
    }) {
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
        body: body[..declared_body_bytes].to_vec(),
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
        None => "missing",
        // The expected header is `Bearer <token>` (case-sensitive on the
        // scheme, matching the prior format!-then-eq contract). Compare in
        // constant time so per-byte timing differences cannot recover the
        // configured bearer token byte-by-byte. The previous
        // `value == &format!("Bearer {configured_token}")` short-circuited
        // on the first mismatching byte; with `EE_SERVE_TOKEN` policy-required
        // to be at least 256 bits AND `--allow-non-loopback` already wired,
        // that timing channel is a real credential-recovery vector against
        // a remote attacker who can issue many requests and measure response
        // latency.
        Some(value) => {
            let expected = format!("Bearer {configured_token}");
            if constant_time_eq_bytes(value.as_bytes(), expected.as_bytes()) {
                "accepted"
            } else {
                "rejected"
            }
        }
    }
}

/// Constant-time byte-slice equality.
///
/// Runs in `O(max(a.len(), b.len()))` regardless of how soon a mismatch
/// appears, and folds the length difference into the accumulator so that
/// length-mismatched inputs also take constant time. Mirrors
/// `src/core/preflight_guard.rs:constant_time_eq_str` (private to that
/// module — duplicated here rather than re-exported to keep the
/// preflight_guard surface unchanged).
#[must_use]
fn constant_time_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    let max_len = a.len().max(b.len());
    let mut diff = a.len() ^ b.len();
    for index in 0..max_len {
        let x = a.get(index).copied().unwrap_or(0);
        let y = b.get(index).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    std::hint::black_box(diff) == 0
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
    let request = json!({
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
    });
    render_serve_sse_event_with_metadata(event_kind, terminal, payload, request, 0, 0)
}

fn render_serve_sse_event_for_request(
    event_kind: &str,
    terminal: bool,
    payload: &JsonValue,
    request_id: &str,
    request: &ServeHttpRequest,
    auth_state: &'static str,
    elapsed_ms: u64,
    event_buffer_remaining: usize,
) -> String {
    render_serve_sse_event_with_metadata(
        event_kind,
        terminal,
        payload,
        serve_request_metadata_json(request_id, request, auth_state),
        elapsed_ms,
        event_buffer_remaining,
    )
}

fn render_serve_sse_event_with_metadata(
    event_kind: &str,
    terminal: bool,
    payload: &JsonValue,
    request: JsonValue,
    elapsed_ms: u64,
    event_buffer_remaining: usize,
) -> String {
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
    // bd-1zoiw: derive the outer envelope's degradedCodes from the inner
    // wrapped payload's top-level `degraded[]` codes so terminal SSE frames
    // carrying ee.response.v2 / ee.error.v2 envelopes with non-empty
    // degradations surface those codes at the response-metadata level.
    // Older synthetic error fixtures placed the array at `error.degraded`;
    // keep accepting that shape as a compatibility fallback while the
    // canonical `ee.error.v2` schema and renderer use the top-level field.
    let degraded_codes = serve_payload_degraded_codes(&wrapped_payload, payload_schema);
    let event_payload = json!({
        "schema": SERVE_ENDPOINT_SCHEMA_V1,
        "request": request,
        "response": {
            "statusCode": if payload_schema == "ee.error.v2" { 500 } else { 200 },
            "payloadSchema": payload_schema,
            "payload": wrapped_payload,
            "elapsedMs": elapsed_ms,
            "degradedCodes": degraded_codes,
            "volatileTransportFields": ["request.requestId", "response.elapsedMs"]
        },
        "sse": {
            "readOnly": true,
            "eventKind": event_kind,
            "terminal": terminal,
            "eventBufferRemaining": event_buffer_remaining
        }
    });
    format!("event: {event_kind}\ndata: {event_payload}\n\n")
}

fn serve_payload_degraded_codes<'a>(payload: &'a JsonValue, payload_schema: &str) -> Vec<&'a str> {
    let mut degraded_codes = Vec::new();
    push_degraded_codes_from_value(&mut degraded_codes, payload.get("degraded"));
    if payload_schema == "ee.error.v2" {
        push_degraded_codes_from_value(&mut degraded_codes, payload.pointer("/error/degraded"));
    }
    degraded_codes
}

fn push_degraded_codes_from_value<'a>(
    degraded_codes: &mut Vec<&'a str>,
    degraded_value: Option<&'a JsonValue>,
) {
    let Some(entries) = degraded_value.and_then(JsonValue::as_array) else {
        return;
    };
    for code in entries
        .iter()
        .filter_map(|entry| entry.get("code").and_then(JsonValue::as_str))
    {
        if !degraded_codes.contains(&code) {
            degraded_codes.push(code);
        }
    }
}

fn serve_error_metadata_codes<'a>(payload: &'a JsonValue, error_code: &'a str) -> Vec<&'a str> {
    let mut codes = vec![error_code];
    push_degraded_codes_from_value(&mut codes, payload.get("degraded"));
    push_degraded_codes_from_value(&mut codes, payload.pointer("/error/degraded"));
    codes
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
    // bd-da9h1: endpoints that do not require auth (e.g. `Unknown` for
    // any unsupported /v1/* path) must reach the dispatch table so the
    // 404/not-found branch can answer them. Without this gate, every
    // unknown path returned 401 even when the bearer token was correct,
    // hiding endpoint-discovery errors behind an auth failure.
    if request.endpoint.auth_required() && auth_state != "accepted" {
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
        let frame = match serve_sse_payload_for_plan(&plan, &request, limits) {
            Ok(payload) => render_serve_sse_event_for_request(
                "complete", true, &payload, request_id, &request, auth_state, elapsed_ms, 0,
            ),
            Err(error) => render_serve_sse_event_for_request(
                "error",
                true,
                &serve_error_payload(&error),
                request_id,
                &request,
                auth_state,
                elapsed_ms,
                0,
            ),
        };
        return render_serve_http_sse_response(&frame);
    }

    let payload = match serve_dispatch_payload_for_plan(&plan, &request) {
        Ok(payload) => payload,
        Err(error) => {
            let status_code = serve_status_code_for_payload_error(&error);
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
    render_serve_http_json_response(
        200,
        &serve_dispatch_exchange_envelope(
            request_id, &request, auth_state, 200, &payload, elapsed_ms,
        ),
    )
}

pub fn serve_single_connection_exchange(
    mut stream: TcpStream,
    request_id: &str,
    limits: &ServeLimits,
    token: Option<&str>,
    elapsed_ms: u64,
) -> Result<ServeConnectionExchange, DomainError> {
    stream
        .set_read_timeout(Some(Duration::from_millis(
            limits.connection_read_timeout_ms,
        )))
        .map_err(|error| serve_transport_io_error("set read timeout", error))?;
    stream
        .set_write_timeout(Some(Duration::from_millis(
            limits.response_write_timeout_ms,
        )))
        .map_err(|error| serve_transport_io_error("set write timeout", error))?;

    let mut request_bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let total_read_limit = serve_total_read_limit(limits)?;
    loop {
        let read = stream
            .read(&mut buffer)
            .map_err(|error| serve_transport_io_error("read request", error))?;
        if read == 0 {
            break;
        }
        request_bytes.extend_from_slice(&buffer[..read]);
        if request_bytes.len() > total_read_limit {
            return Err(serve_usage_error(format!(
                "HTTP request exceeds the {total_read_limit} byte total read limit."
            )));
        }
        if serve_request_complete_len(&request_bytes, limits)?.is_some() {
            break;
        }
    }

    let response =
        render_serve_transport_exchange(request_id, &request_bytes, limits, token, elapsed_ms);
    stream
        .write_all(response.as_bytes())
        .map_err(|error| serve_transport_io_error("write response", error))?;
    stream
        .flush()
        .map_err(|error| serve_transport_io_error("flush response", error))?;
    let _ = stream.shutdown(Shutdown::Both);

    Ok(ServeConnectionExchange {
        request_bytes: request_bytes.len(),
        response_bytes: response.len(),
        response_status_line: response.lines().next().unwrap_or_default().to_owned(),
    })
}

pub fn serve_accept_once(
    listener: &TcpListener,
    request_id: &str,
    limits: &ServeLimits,
    token: Option<&str>,
    elapsed_ms: u64,
) -> Result<ServeAcceptedConnection, DomainError> {
    listener
        .set_nonblocking(true)
        .map_err(|error| serve_transport_io_error("set listener nonblocking mode", error))?;
    let timeout = Duration::from_millis(limits.connection_read_timeout_ms);
    let deadline = Instant::now() + timeout;
    let poll_interval = Duration::from_millis(10);
    let mut accept_attempts = 0_usize;

    loop {
        accept_attempts = accept_attempts.saturating_add(1);
        match listener.accept() {
            Ok((stream, peer_addr)) => {
                let exchange = serve_single_connection_exchange(
                    stream, request_id, limits, token, elapsed_ms,
                )?;
                return Ok(ServeAcceptedConnection {
                    accept_attempts,
                    peer_addr,
                    exchange,
                });
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(serve_transport_timeout_error(timeout));
                }
                let remaining = deadline.saturating_duration_since(now);
                std::thread::sleep(if remaining < poll_interval {
                    remaining
                } else {
                    poll_interval
                });
            }
            Err(error) => return Err(serve_transport_io_error("accept connection", error)),
        }
    }
}

pub fn serve_foreground_once<F>(
    options: &ServeStartupOptions,
    token: Option<&str>,
    request_id: &str,
    elapsed_ms: u64,
    on_bound: F,
) -> Result<ServeForegroundOnceReport, DomainError>
where
    F: FnOnce(&ServeListenerBinding) -> Result<(), DomainError>,
{
    let binding = bind_serve_listener(options, token)?;
    let bound_addr = binding
        .listener
        .local_addr()
        .map_err(|error| serve_transport_io_error("inspect listener local address", error))?;
    let listener_metadata = binding.metadata.clone();
    on_bound(&binding)?;
    let accepted = serve_accept_once(
        &binding.listener,
        request_id,
        &options.limits,
        token,
        elapsed_ms,
    )?;
    Ok(ServeForegroundOnceReport {
        bound_addr,
        listener_metadata,
        accepted,
    })
}

fn serve_request_complete_len(
    bytes: &[u8],
    limits: &ServeLimits,
) -> Result<Option<usize>, DomainError> {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        if bytes.len() > limits.max_header_bytes {
            return Err(serve_usage_error(format!(
                "HTTP request headers exceed the {} byte limit.",
                limits.max_header_bytes
            )));
        }
        return Ok(None);
    };
    if header_end > limits.max_header_bytes {
        return Err(serve_usage_error(format!(
            "HTTP request headers exceed the {} byte limit.",
            limits.max_header_bytes
        )));
    }

    let header_text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| serve_usage_error("HTTP request headers must be valid UTF-8."))?;
    // Reject duplicate Content-Length rows at the framing layer for the
    // same reason `parse_serve_http_request` rejects them at the parser
    // layer: a permissive upstream proxy may frame on the first value
    // while this loop previously kept the last (last-write-wins on the
    // `content_length` local). Two CL rows like `Content-Length: 100`
    // then `Content-Length: 50` would frame this side's socket read on
    // 50 bytes while a proxy framed on 100 — the classic CL.CL
    // smuggling shape. The parser-layer rejection at
    // `parse_serve_http_request` would catch the request afterwards,
    // but the framing layer is what decides how many bytes to consume
    // from the socket; aligning both layers ensures the bytes for any
    // smuggled trailing request never enter the buffer in the first
    // place. Same defense-in-depth pattern as the parser-layer check
    // at `src/serve.rs:567`.
    let mut content_length = 0_usize;
    let mut content_length_seen = false;
    for line in header_text.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            if content_length_seen {
                return Err(serve_usage_error(
                    "HTTP header `content-length` appears more than once; \
                     the ee serve v2 framing layer rejects duplicate \
                     Content-Length rows to close request-smuggling \
                     framing disagreements.",
                ));
            }
            content_length_seen = true;
            content_length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| serve_usage_error("Content-Length must be a non-negative integer."))?;
        }
    }
    if content_length > limits.max_body_bytes {
        return Err(serve_usage_error(format!(
            "HTTP request body exceeds the {} byte limit.",
            limits.max_body_bytes
        )));
    }

    let complete_len = serve_complete_request_len(header_end, content_length)?;
    if bytes.len() > complete_len {
        return Err(serve_usage_error(
            "HTTP request body contains bytes beyond Content-Length; keepalive is not supported.",
        ));
    }
    if bytes.len() == complete_len {
        Ok(Some(complete_len))
    } else {
        Ok(None)
    }
}

fn serve_total_read_limit(limits: &ServeLimits) -> Result<usize, DomainError> {
    serve_complete_request_len(limits.max_header_bytes, limits.max_body_bytes)
}

fn serve_complete_request_len(
    header_end: usize,
    content_length: usize,
) -> Result<usize, DomainError> {
    header_end
        .checked_add(4)
        .and_then(|header_bytes| header_bytes.checked_add(content_length))
        .ok_or_else(|| {
            serve_usage_error(
                "HTTP request size limit overflows usize; reduce serve header/body limits.",
            )
        })
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
        Some(value) if value.len().saturating_mul(8) < MIN_SERVE_TOKEN_BITS => ServeTokenPosture {
            state: "weak",
            repair: Some("Use at least 32 random bytes of bearer-token material."),
        },
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
    payload: &JsonValue,
    elapsed_ms: u64,
) -> JsonValue {
    // bd-2eiwy: surface inner ee.response.v2 degradations at the
    // transport-envelope metadata level, matching the SSE extraction path.
    let degraded_codes = serve_payload_degraded_codes(payload, "ee.response.v2");
    json!({
        "schema": SERVE_ENDPOINT_SCHEMA_V1,
        "request": serve_request_metadata_json(request_id, request, auth_state),
        "response": {
            "statusCode": status_code,
            "payloadSchema": "ee.response.v2",
            "payload": payload,
            "elapsedMs": elapsed_ms,
            "degradedCodes": degraded_codes,
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
    let payload = serve_error_payload(error);
    let degraded_codes = serve_error_metadata_codes(&payload, error.code());
    json!({
        "schema": SERVE_ENDPOINT_SCHEMA_V1,
        "request": serve_request_metadata_json(request_id, request, auth_state),
        "response": {
            "statusCode": status_code,
            "payloadSchema": "ee.error.v2",
            "payload": payload,
            "elapsedMs": elapsed_ms,
            "degradedCodes": degraded_codes,
            "volatileTransportFields": ["request.requestId", "response.elapsedMs"]
        }
    })
}

fn serve_dispatch_payload_for_plan(
    plan: &ServeDispatchPlan,
    request: &ServeHttpRequest,
) -> Result<JsonValue, DomainError> {
    match plan.endpoint {
        ServeEndpoint::Status => serve_status_payload_json(),
        ServeEndpoint::Doctor => serve_doctor_payload_json(),
        ServeEndpoint::Search => serve_search_payload_json(request),
        ServeEndpoint::Context => serve_context_payload_json(request),
        ServeEndpoint::Why => serve_why_payload_json(request),
        ServeEndpoint::SwarmBrief => serve_swarm_brief_payload_json(),
        ServeEndpoint::DurableWrite => serve_durable_write_payload_json(plan, request),
        ServeEndpoint::Events | ServeEndpoint::Unknown => Err(serve_usage_error(format!(
            "Endpoint `{}` is not a JSON dispatch endpoint.",
            plan.endpoint.as_str()
        ))),
    }
}

fn serve_sse_payload_for_plan(
    plan: &ServeDispatchPlan,
    request: &ServeHttpRequest,
    limits: &ServeLimits,
) -> Result<JsonValue, DomainError> {
    match plan.endpoint {
        ServeEndpoint::Events => serve_events_payload_json(plan, request, limits),
        _ => Err(serve_usage_error(format!(
            "Endpoint `{}` is not an SSE endpoint.",
            plan.endpoint.as_str()
        ))),
    }
}

fn serve_current_workspace_path() -> Result<PathBuf, DomainError> {
    std::env::current_dir().map_err(|error| DomainError::Configuration {
        message: format!("Failed to resolve current workspace for ee serve request: {error}"),
        repair: Some("Start `ee serve --foreground` from a readable workspace.".to_owned()),
    })
}

fn serve_response_payload_from_data(data: JsonValue, degraded: JsonValue) -> JsonValue {
    json!({
        "schema": RESPONSE_SCHEMA_V2,
        "success": true,
        "data": data,
        "degraded": degraded
    })
}

fn parse_rendered_response_json(
    raw: &str,
    surface: &'static str,
) -> Result<JsonValue, DomainError> {
    serde_json::from_str(raw).map_err(|error| DomainError::Storage {
        message: format!("Failed to parse {surface} JSON response rendered for ee serve: {error}"),
        repair: Some("Fix the response renderer before serving this endpoint.".to_owned()),
    })
}

fn serve_doctor_payload_json() -> Result<JsonValue, DomainError> {
    let workspace_path = serve_current_workspace_path()?;
    let report = DoctorReport::gather_for_workspace(&workspace_path);
    parse_rendered_response_json(&crate::output::render_doctor_json(&report), "doctor")
}

fn serve_search_payload_json(request: &ServeHttpRequest) -> Result<JsonValue, DomainError> {
    let workspace_path = serve_current_workspace_path()?;
    let query = require_single_query_value(request, "q", "/v1/search")?;
    let report = run_search(&SearchOptions {
        workspace_path,
        database_path: None,
        index_dir: None,
        query,
        limit: 10,
        speed: crate::search::SpeedMode::Default,
        explain: false,
        as_of: None,
        include_tombstoned: false,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        dedup_mode: SearchDedupMode::DocId,
        source_mode: SearchSourceMode::Hybrid,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
    })
    .map_err(|error| DomainError::SearchIndex {
        message: error.to_string(),
        repair: error.repair_hint().map(str::to_owned),
    })?;
    let data = report.data_json();
    let degraded = data.get("degraded").cloned().unwrap_or_else(|| json!([]));
    Ok(serve_response_payload_from_data(data, degraded))
}

fn serve_context_payload_json(request: &ServeHttpRequest) -> Result<JsonValue, DomainError> {
    let workspace_path = serve_current_workspace_path()?;
    let task = require_single_query_value(request, "task", "/v1/context")?;
    let output_options = ContextPackOutputOptions::default();
    let options = ContextPackOptions {
        workspace_path,
        database_path: None,
        index_dir: None,
        query: task,
        speed: crate::search::SpeedMode::Default,
        source_mode: SearchSourceMode::Hybrid,
        strict_source_mode: false,
        filters: QueryFilters::default(),
        profile: None,
        max_tokens: None,
        candidate_pool: None,
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        redaction_level: RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: 0,
        task_lens: None,
        require_fresh_sentinels: false,
        output_options,
        persist_pack: false,
        baseline_write: None,
        no_lod: false,
    };
    let response = run_context_pack_with_performance(&options, "pack")
        .map(|run| run.response)
        .map_err(serve_context_error_to_domain)?;
    parse_rendered_response_json(
        &crate::output::render_context_response_json(&response),
        "pack",
    )
}

fn serve_why_payload_json(request: &ServeHttpRequest) -> Result<JsonValue, DomainError> {
    let workspace_path = serve_current_workspace_path()?;
    let memory_id = request
        .path
        .strip_prefix("/v1/why/")
        .filter(|value| !value.trim().is_empty() && !value.contains('/'))
        .ok_or_else(|| {
            serve_usage_error(
                "GET /v1/why/{memory_id} requires exactly one memory ID path segment.",
            )
        })?;
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", database_path.display()),
            repair: Some("ee init --workspace .".to_owned()),
        });
    }
    let report = explain_memory(&WhyOptions {
        database_path: &database_path,
        memory_id,
        confidence_threshold: 0.5,
    });
    if let Some(error) = report.error.as_ref() {
        return Err(DomainError::Storage {
            message: error.clone(),
            repair: Some("ee init --workspace . --repair-plan".to_owned()),
        });
    }
    if !report.found {
        return Err(DomainError::NotFound {
            resource: "memory".to_owned(),
            id: memory_id.to_owned(),
            repair: Some("ee memory list".to_owned()),
        });
    }
    parse_rendered_response_json(&crate::output::render_why_json(&report), "why")
}

fn serve_swarm_brief_payload_json() -> Result<JsonValue, DomainError> {
    let workspace_path = serve_current_workspace_path()?;
    let mut options = SwarmBriefCollectOptions::for_workspace(workspace_path);
    options.enabled_sources.insert(SwarmBriefSourceKind::Rch);
    options.include_rch = true;
    let runner = SystemSwarmBriefCommandRunner;
    let report = collect_swarm_brief(&options, &runner);
    let degraded =
        serde_json::to_value(&report.degraded).map_err(|error| DomainError::Storage {
            message: format!("Failed to serialize swarm brief degraded entries: {error}"),
            repair: Some("Fix the swarm brief serializer before serving this endpoint.".to_owned()),
        })?;
    let data = serde_json::to_value(&report).map_err(|error| DomainError::Storage {
        message: format!("Failed to serialize swarm brief report: {error}"),
        repair: Some("Fix the swarm brief serializer before serving this endpoint.".to_owned()),
    })?;
    Ok(serve_response_payload_from_data(data, degraded))
}

fn serve_events_payload_json(
    plan: &ServeDispatchPlan,
    request: &ServeHttpRequest,
    limits: &ServeLimits,
) -> Result<JsonValue, DomainError> {
    let workspace_path = serve_current_workspace_path()?;
    let cursor = optional_query_u64(request, "cursor", "/v1/events")?.unwrap_or(0);
    let limit = optional_query_u32(request, "limit", "/v1/events")?.unwrap_or_else(|| {
        u32::try_from(limits.sse_event_buffer)
            .unwrap_or(u32::MAX)
            .max(1)
    });
    let filter = optional_single_query_value(request, "filter", "/v1/events")?;
    let filter = parse_subscribe_filter(filter.as_deref())?;
    let report = poll_memory_deltas(&SubscribePollOptions {
        workspace_path: &workspace_path,
        database_path: None,
        cursor,
        filter,
        limit,
    })?;
    let degraded =
        serde_json::to_value(&report.degraded).map_err(|error| DomainError::Storage {
            message: format!("Failed to serialize subscribe-poll degraded entries: {error}"),
            repair: Some("Fix the subscribe-poll serializer before serving /v1/events.".to_owned()),
        })?;
    let mut data = report.data_json();
    if let Some(data) = data.as_object_mut() {
        data.insert(
            "serve".to_owned(),
            json!({
                "executionBoundary": "serve_sse_events",
                "handlerSurface": plan.handler_surface,
                "readOnly": true,
                "terminalFrame": true,
                "dispatchPlan": plan.to_json()
            }),
        );
    }
    Ok(serve_response_payload_from_data(data, degraded))
}

fn serve_context_error_to_domain(error: crate::core::context::ContextPackError) -> DomainError {
    if error.is_policy_denied() {
        DomainError::PolicyDenied {
            message: error.to_string(),
            repair: error.repair_hint().map(str::to_owned),
        }
    } else {
        DomainError::Storage {
            message: error.to_string(),
            repair: error.repair_hint().map(str::to_owned),
        }
    }
}

fn serve_status_code_for_payload_error(error: &DomainError) -> u16 {
    match error {
        DomainError::Usage { .. }
        | DomainError::UsageWithDetails { .. }
        | DomainError::UsageCodeWithDetails { .. } => 400,
        DomainError::NotFound { .. } => 404,
        DomainError::PolicyDenied { .. } | DomainError::PolicyDeniedWithDetails { .. } => 403,
        _ => 500,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServeDurableWriteRequest {
    operation: String,
    workspace: PathBuf,
    content: Option<String>,
    level: Option<String>,
    kind: Option<String>,
    tags: Option<Vec<String>>,
    workflow: Option<String>,
    confidence: Option<f32>,
    source: Option<String>,
    allow_secret_mention: Option<bool>,
    valid_from: Option<String>,
    valid_to: Option<String>,
    dry_run: Option<bool>,
    auto_link: Option<bool>,
    propose_candidates: Option<bool>,
}

fn serve_durable_write_payload_json(
    plan: &ServeDispatchPlan,
    request: &ServeHttpRequest,
) -> Result<JsonValue, DomainError> {
    let durable_request = parse_serve_durable_write_request(request)?;
    match durable_request.operation.trim() {
        "remember" => serve_durable_write_remember_payload_json(plan, &durable_request),
        operation => Err(DomainError::UsageCodeWithDetails {
            code: "serve_durable_write_unsupported_operation",
            message: format!(
                "Unsupported /v1/durable-write operation `{operation}`; only `remember` is implemented."
            ),
            repair: Some(
                "POST JSON with {\"operation\":\"remember\",\"workspace\":\"/path\",\"content\":\"...\"}."
                    .to_owned(),
            ),
            details_json: json!({
                "supportedOperations": ["remember"],
                "endpoint": "/v1/durable-write"
            })
            .to_string(),
        }),
    }
}

fn parse_serve_durable_write_request(
    request: &ServeHttpRequest,
) -> Result<ServeDurableWriteRequest, DomainError> {
    if request.body.is_empty() {
        return Err(DomainError::UsageWithDetails {
            message: "/v1/durable-write requires a JSON request body.".to_owned(),
            repair: Some(
                "POST JSON with {\"operation\":\"remember\",\"workspace\":\"/path\",\"content\":\"...\"}."
                    .to_owned(),
            ),
            details_json: durable_write_request_contract_json().to_string(),
        });
    }
    serde_json::from_slice(&request.body).map_err(|error| DomainError::UsageWithDetails {
        message: format!("Failed to parse /v1/durable-write JSON body: {error}"),
        repair: Some(
            "POST JSON with {\"operation\":\"remember\",\"workspace\":\"/path\",\"content\":\"...\"}."
                .to_owned(),
        ),
        details_json: durable_write_request_contract_json().to_string(),
    })
}

fn serve_durable_write_remember_payload_json(
    plan: &ServeDispatchPlan,
    request: &ServeDurableWriteRequest,
) -> Result<JsonValue, DomainError> {
    let content = required_non_empty_durable_write_string(
        request.content.as_deref(),
        "content",
        "remember operations require non-empty `content`.",
    )?;
    if request.workspace.as_os_str().is_empty() {
        return Err(DomainError::Usage {
            message: "remember operations require a non-empty `workspace` path.".to_owned(),
            repair: Some(
                "Set `workspace` to the target workspace root for the memory write.".to_owned(),
            ),
        });
    }
    let level = optional_non_empty_durable_write_string(request.level.as_deref(), "episodic");
    let kind = optional_non_empty_durable_write_string(request.kind.as_deref(), "fact");
    let tags = normalize_durable_write_tags(request.tags.as_ref())?;
    let report = remember_memory(&RememberMemoryOptions {
        workspace_path: &request.workspace,
        database_path: None,
        content,
        workflow_id: request
            .workflow
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
        level,
        kind,
        tags: tags.as_deref(),
        confidence: request.confidence.unwrap_or(0.8),
        source: request
            .source
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
        allow_secret_mention: request.allow_secret_mention.unwrap_or(false),
        valid_from: request
            .valid_from
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
        valid_to: request
            .valid_to
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
        dry_run: request.dry_run.unwrap_or(false),
        auto_link: request.auto_link.unwrap_or(true),
        propose_candidates: request.propose_candidates.unwrap_or(true),
    })?;
    let degraded = remember_report_degraded_entries_json(&report);
    Ok(json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "execution": if report.dry_run { "dry_run" } else { "executed" },
            "executionBoundary": "serve_durable_write",
            "businessLogicExecuted": true,
            "operation": "remember",
            "handlerSurface": "serve.durable_write.remember",
            "dispatchPlan": plan.to_json(),
            "result": remember_report_summary_json(&report),
            "degraded": degraded.clone()
        },
        "degraded": degraded
    }))
}

fn durable_write_request_contract_json() -> JsonValue {
    json!({
        "endpoint": "/v1/durable-write",
        "supportedOperations": ["remember"],
        "remember": {
            "required": ["operation", "workspace", "content"],
            "optional": [
                "level",
                "kind",
                "tags",
                "workflow",
                "confidence",
                "source",
                "allowSecretMention",
                "validFrom",
                "validTo",
                "dryRun",
                "autoLink",
                "proposeCandidates"
            ]
        }
    })
}

fn required_non_empty_durable_write_string<'a>(
    value: Option<&'a str>,
    field: &str,
    message: &'static str,
) -> Result<&'a str, DomainError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DomainError::Usage {
            message: message.to_owned(),
            repair: Some(format!("Set `{field}` in the /v1/durable-write JSON body.")),
        })
}

fn optional_non_empty_durable_write_string<'a>(
    value: Option<&'a str>,
    default: &'static str,
) -> &'a str {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
}

fn normalize_durable_write_tags(tags: Option<&Vec<String>>) -> Result<Option<String>, DomainError> {
    let Some(tags) = tags else {
        return Ok(None);
    };
    let normalized = tags
        .iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    if normalized.is_empty() {
        return Err(DomainError::Usage {
            message: "`tags` must contain at least one non-empty string when provided.".to_owned(),
            repair: Some("Omit `tags` or provide non-empty tag strings.".to_owned()),
        });
    }
    Ok(Some(normalized.join(",")))
}

fn remember_report_summary_json(report: &RememberMemoryReport) -> JsonValue {
    json!({
        "command": "remember",
        "version": report.version,
        "memoryId": report.memory_id.to_string(),
        "workspaceId": &report.workspace_id,
        "workspacePath": report.workspace_path.display().to_string(),
        "databasePath": report.database_path.display().to_string(),
        "content": &report.content,
        "workflowId": &report.workflow_id,
        "level": report.level.as_str(),
        "kind": report.kind.as_str(),
        "confidence": report.confidence,
        "tags": &report.tags,
        "source": &report.source,
        "validFrom": &report.valid_from,
        "validTo": &report.valid_to,
        "validityStatus": &report.validity_status,
        "validityWindowKind": &report.validity_window_kind,
        "dryRun": report.dry_run,
        "persisted": report.persisted,
        "revisionNumber": report.revision_number,
        "revisionGroupId": &report.revision_group_id,
        "auditId": &report.audit_id,
        "indexJobId": &report.index_job_id,
        "indexStatus": &report.index_status,
        "effectIds": &report.effect_ids,
        "suggestedLinkStatus": &report.suggested_link_status,
        "autoLinkStatus": &report.auto_link_status,
        "curationCandidateStatus": &report.curation_candidate_status,
        "redactionStatus": &report.redaction_status,
        "policyBypassUsed": report.policy_bypass.is_some()
    })
}

fn remember_report_degraded_entries_json(report: &RememberMemoryReport) -> Vec<JsonValue> {
    let mut degraded = Vec::new();
    if let Some(bypass) = &report.policy_bypass {
        degraded.push(json!({
            "code": &bypass.code,
            "severity": &bypass.severity,
            "message": &bypass.message,
            "repair": &bypass.repair,
            "kind": &bypass.kind
        }));
    }
    degraded.extend(
        report
            .suggested_link_degradations
            .iter()
            .chain(report.auto_link_degradations.iter())
            .chain(report.curation_candidate_degradations.iter())
            .map(|degradation| {
                json!({
                    "code": &degradation.code,
                    "severity": &degradation.severity,
                    "message": &degradation.message,
                    "repair": &degradation.repair
                })
            }),
    );
    degraded
}

fn serve_status_payload_json() -> Result<JsonValue, DomainError> {
    let report = crate::core::status::StatusReport::gather();
    serde_json::from_str(&crate::output::render_status_json(&report)).map_err(|error| {
        DomainError::Storage {
            message: format!("Failed to serialize ee serve status endpoint payload: {error}"),
            repair: Some("Retry `ee status --json` or use direct CLI commands.".to_owned()),
        }
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
        Some([value]) if !is_effectively_empty_query_value(value) => Ok(value.clone()),
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

fn optional_single_query_value(
    request: &ServeHttpRequest,
    name: &str,
    endpoint_path: &str,
) -> Result<Option<String>, DomainError> {
    match request.query.get(name).map(Vec::as_slice) {
        Some([value]) if !is_effectively_empty_query_value(value) => Ok(Some(value.clone())),
        Some([_]) => Err(serve_usage_error(format!(
            "{endpoint_path} requires a non-empty `{name}` query parameter when provided."
        ))),
        Some(_) => Err(serve_usage_error(format!(
            "{endpoint_path} requires at most one `{name}` query parameter."
        ))),
        None => Ok(None),
    }
}

fn optional_query_u64(
    request: &ServeHttpRequest,
    name: &str,
    endpoint_path: &str,
) -> Result<Option<u64>, DomainError> {
    optional_single_query_value(request, name, endpoint_path)?
        .map(|raw| {
            raw.parse::<u64>().map_err(|error| {
                serve_usage_error(format!(
                    "{endpoint_path} query parameter `{name}` must be an unsigned integer: {error}."
                ))
            })
        })
        .transpose()
}

fn optional_query_u32(
    request: &ServeHttpRequest,
    name: &str,
    endpoint_path: &str,
) -> Result<Option<u32>, DomainError> {
    optional_single_query_value(request, name, endpoint_path)?
        .map(|raw| {
            raw.parse::<u32>().map_err(|error| {
                serve_usage_error(format!(
                    "{endpoint_path} query parameter `{name}` must be an unsigned integer: {error}."
                ))
            })
        })
        .transpose()
}

/// bd-2f09u: post-percent-decode emptiness check that treats control
/// bytes (NUL through 0x1F sans the whitespace subset already covered
/// by `char::is_whitespace`) as effectively empty. The prior
/// `value.trim().is_empty()` check only stripped `char::is_whitespace`,
/// so a client could submit `?q=%00`, `?q=%01%02`, or `?q=%00%20` and
/// pass the non-emptiness arm with a semantically-empty string.
fn is_effectively_empty_query_value(value: &str) -> bool {
    value.chars().all(|c| c.is_whitespace() || c.is_control())
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
    // bd-2ysyd: accumulate raw bytes and validate the result as UTF-8 in one
    // shot. The prior implementation pushed each decoded byte through
    // `char::from`, which interprets the byte as a Latin-1 code point and
    // mangles multi-byte UTF-8 sequences (e.g. `%E2%9C%93` → three Latin-1
    // chars instead of `✓`).
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' => {
                if index + 2 >= bytes.len() {
                    return Err(serve_usage_error("Percent escape in query is truncated."));
                }
                let hi = hex_digit(bytes[index + 1])?;
                let lo = hex_digit(bytes[index + 2])?;
                output.push((hi << 4) | lo);
                index += 3;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output)
        .map_err(|_| serve_usage_error("Percent-decoded query value is not valid UTF-8."))
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

fn serve_transport_io_error(action: &str, error: std::io::Error) -> DomainError {
    DomainError::Configuration {
        message: format!("Failed to {action} for ee serve connection: {error}"),
        repair: Some("Retry `ee serve --foreground` or use direct CLI commands.".to_owned()),
    }
}

fn serve_transport_timeout_error(timeout: Duration) -> DomainError {
    DomainError::Configuration {
        message: format!(
            "Timed out waiting for ee serve connection after {} ms.",
            timeout.as_millis()
        ),
        repair: Some("Retry `ee serve --foreground` or use direct CLI commands.".to_owned()),
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
            "degraded": [],
        })
    }
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
    // Cap the read at `DAEMON_JOB_TABLE_MAX_BYTES`. The previous
    // `BufReader::new(file)` shape was unbounded; a peer-planted multi-GB
    // single-line record (no embedded `\n`) would force the underlying
    // `String` line allocation to grow without limit and OOM the daemon
    // read path before any happy-path filter could reject it. Wrapping
    // the handle in `file.take(MAX)` bounds peak allocation while
    // preserving the streaming line-by-line shape. Same defense pattern
    // as the parallel ledger reader at
    // `src/core/why.rs::fetch_coordination_fallback_evidence` (b040cde7).
    let reader = BufReader::new(file.take(DAEMON_JOB_TABLE_MAX_BYTES));
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
    crate::core::path_safety::first_existing_symlink_component(path).map_err(|error| {
        format!(
            "Failed to inspect daemon job table path '{}': {error}",
            path.display()
        )
    })
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

    // bd-2ysyd: percent-decoded query values must round-trip multi-byte UTF-8.
    // The prior decoder pushed each decoded byte through `char::from`, which
    // mangled `%E2%9C%93` to three Latin-1 chars instead of `✓`.
    #[test]
    fn serve_http_parser_decodes_utf8_percent_escapes() -> TestResult {
        let request = parse_serve_http_request(
            "GET /v1/search?q=%E2%9C%93&tag=caf%C3%A9 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
                .as_bytes(),
            &ServeLimits::default(),
        )
        .map_err(|error| error.to_string())?;

        ensure(
            request.query.get("q").cloned(),
            Some(vec!["✓".to_owned()]),
            "decoded single-codepoint utf-8 query",
        )?;
        ensure(
            request.query.get("tag").cloned(),
            Some(vec!["café".to_owned()]),
            "decoded mixed ascii/utf-8 query",
        )?;

        let context = parse_serve_http_request(
            "GET /v1/context?task=na%C3%AFve+plan HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".as_bytes(),
            &ServeLimits::default(),
        )
        .map_err(|error| error.to_string())?;
        ensure(
            context.query.get("task").cloned(),
            Some(vec!["naïve plan".to_owned()]),
            "decoded utf-8 with space",
        )?;

        let bad_utf8 = parse_serve_http_request(
            "GET /v1/search?q=%FF%FE HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".as_bytes(),
            &ServeLimits::default(),
        );
        ensure(
            bad_utf8.is_err(),
            true,
            "invalid utf-8 percent sequence must be rejected",
        )
    }

    // bd-2f09u: query values that decode to only NUL / control bytes must
    // be treated as empty by require_single_query_value so an attacker
    // cannot bypass the empty-query rejection by submitting `?q=%00`,
    // `?q=%01%02`, or `?q=%00%20`. The decoder itself still accepts the
    // raw bytes (UTF-8 round-trip stays unchanged for legitimate
    // values); the emptiness check at the dispatch layer is what tightens.
    #[test]
    fn serve_dispatch_rejects_control_byte_only_query_value_as_empty() -> TestResult {
        let baseline_empty = plan_request("GET /v1/search?q= HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .err()
            .ok_or_else(|| "baseline empty `q` should be rejected".to_string())?;

        for raw in [
            "GET /v1/search?q=%00 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "GET /v1/search?q=%01%02 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "GET /v1/search?q=%00%20 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "GET /v1/search?q=%09%0A%0D HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        ] {
            let error = plan_request(raw)
                .err()
                .ok_or_else(|| format!("control-byte-only query `{raw}` should be rejected"))?;
            ensure(
                error.as_str(),
                baseline_empty.as_str(),
                "control-byte-only query must produce the same error as a literal empty `q=`",
            )?;
        }

        // Legitimate UTF-8 with embedded whitespace should still go
        // through unchanged — guard against the predicate over-rejecting.
        let healthy =
            plan_request("GET /v1/search?q=release+ci HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
        ensure(
            healthy.handler_surface,
            "cli.search",
            "legitimate non-empty query must still dispatch",
        )?;
        ensure(
            healthy.cli_argv,
            argv(&["ee", "search", "release ci", "--json"]),
            "legitimate non-empty query argv",
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
        ensure(context.handler_surface, "cli.pack", "context handler")?;
        ensure(
            context.cli_argv,
            argv(&["ee", "pack", "prepare release", "--json"]),
            "context argv",
        )?;

        let why = plan_request(
            "GET /v1/why/mem_00000000000000000000000001 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )?;
        ensure(why.handler_surface, "cli.why", "why handler")?;
        ensure(
            why.cli_argv.clone(),
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
            "serve.durable_write",
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
    fn serve_http_parser_rejects_chunked_in_te_codings_list() -> TestResult {
        // RFC 7230 §3.3.3 smuggling guard: a coding list like
        // `chunked, identity` or `identity, chunked` MUST be treated as
        // chunked (and therefore rejected by v1's no-chunked policy).
        // The previous exact-match check on `eq_ignore_ascii_case("chunked")`
        // accepted both shapes silently, letting a CL.TE smuggling pair
        // (CL framed for ee, chunked framed for the upstream proxy)
        // slip through. Lock the defense.
        let raws: [&[u8]; 4] = [
            // chunked first in list
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked, identity\r\nContent-Length: 0\r\n\r\n",
            // chunked last in list
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: identity, chunked\r\nContent-Length: 0\r\n\r\n",
            // case-insensitive, with internal whitespace
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding:  CHUNKED , gzip \r\nContent-Length: 0\r\n\r\n",
            // bare chunked (regression guard for the original exact match)
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n",
        ];
        for raw in raws {
            match parse_serve_http_request(raw, &ServeLimits::default()) {
                Ok(request) => {
                    return Err(format!(
                        "Transfer-Encoding with chunked-in-list must reject; got {request:?}",
                    ));
                }
                Err(error) => ensure(
                    error.code(),
                    "usage",
                    "chunked-in-TE-list rejection error code",
                )?,
            }
        }
        // Sanity check: a TE without `chunked` (e.g. `identity` alone)
        // must still be accepted — the smuggling guard targets `chunked`
        // specifically, not all TE values.
        parse_serve_http_request(
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: identity\r\nContent-Length: 0\r\n\r\n",
            &ServeLimits::default(),
        )
        .map_err(|error| format!("TE: identity must still parse; got {error}"))?;
        Ok(())
    }

    #[test]
    fn serve_http_parser_rejects_non_rfc7230_request_line_whitespace() -> TestResult {
        // RFC 7230 §3.1.1 fixes the request line to exactly
        //   `method SP request-target SP HTTP-version CRLF`
        // where SP is a single 0x20. The prior parser accepted ANY
        // Unicode whitespace via `split_whitespace`, opening a
        // request-line normalization mismatch — the same class of
        // smuggling vector 1b426516 closed for `Transfer-Encoding:
        // chunked, identity`. Lock the strict shape across the four
        // practical bypass families and confirm the canonical shape
        // still parses.
        let raws: &[&[u8]] = &[
            // tab between method and target
            b"GET\t/v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            // tab between target and version
            b"GET /v1/status\tHTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            // run of two spaces between method and target
            b"GET  /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            // run of two spaces between target and version
            b"GET /v1/status  HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            // leading space before method
            b" GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            // trailing space after version
            b"GET /v1/status HTTP/1.1 \r\nHost: 127.0.0.1\r\n\r\n",
            // vertical tab inside the request line
            b"GET\x0b/v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            // form feed inside the request line
            b"GET\x0c/v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            // fourth segment after the version
            b"GET /v1/status HTTP/1.1 extra\r\nHost: 127.0.0.1\r\n\r\n",
        ];
        for raw in raws {
            match parse_serve_http_request(raw, &ServeLimits::default()) {
                Ok(request) => {
                    return Err(format!(
                        "non-RFC7230 request-line whitespace must reject; got {request:?} for {:?}",
                        String::from_utf8_lossy(raw),
                    ));
                }
                Err(error) => ensure(error.code(), "usage", "request-line shape error code")?,
            }
        }

        // Canonical shape must still parse.
        parse_serve_http_request(
            b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            &ServeLimits::default(),
        )
        .map_err(|error| format!("canonical request line must still parse; got {error}"))?;
        Ok(())
    }

    #[test]
    fn serve_auth_bearer_compare_is_constant_time_shape() -> TestResult {
        // Regression guard for the bearer-token timing side channel.
        //
        // The pre-fix code computed
        //   `value == &format!("Bearer {configured_token}")`,
        // a short-circuit string compare that leaked the first mismatching
        // byte through response-latency analysis. EE_SERVE_TOKEN is policy-
        // required at >= 256 bits and `--allow-non-loopback` is wired, so
        // the leak is real-credential-recovery territory.
        //
        // This test does NOT measure wall-clock latency (timing assertions
        // are unstable under CI noise). It locks the structural contract:
        // (a) the helper is constant-time-shape — runs over `max_len`
        //     regardless of mismatch position;
        // (b) length-mismatched inputs are NOT shortcut to false by length
        //     check; the length difference XOR-folds into the accumulator;
        // (c) the public `serve_auth_state` accept/reject decision is
        //     unchanged from the prior `==` semantics.
        ensure(
            constant_time_eq_bytes(b"abc", b"abc"),
            true,
            "equal byte slices must compare equal",
        )?;
        ensure(
            constant_time_eq_bytes(b"abc", b"abd"),
            false,
            "single-byte diff must compare unequal",
        )?;
        ensure(
            constant_time_eq_bytes(b"abc", b""),
            false,
            "empty vs non-empty must compare unequal",
        )?;
        ensure(
            constant_time_eq_bytes(b"abc", b"abcd"),
            false,
            "prefix-only match must compare unequal",
        )?;
        ensure(
            constant_time_eq_bytes(b"abcd", b"abc"),
            false,
            "longer-vs-shorter match must compare unequal",
        )?;
        ensure(
            constant_time_eq_bytes(b"", b""),
            true,
            "empty-vs-empty must compare equal",
        )?;

        // End-to-end through serve_auth_state: bearer header must accept
        // the exact configured token AND reject any single-byte mutation
        // (early-bytes, mid, late) — locking the accept/reject decision
        // unchanged from the prior `==` semantics.
        let configured = "1234567890abcdef1234567890abcdef"; // 32 ASCII bytes
        let request_with_auth = |auth_header: &str| -> Result<ServeHttpRequest, String> {
            parse_serve_http_request(
                format!(
                    "GET /v1/search?q=x HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {auth_header}\r\n\r\n"
                )
                .as_bytes(),
                &ServeLimits::default(),
            )
            .map_err(|error| error.to_string())
        };

        let req_ok = request_with_auth(&format!("Bearer {configured}"))?;
        ensure(
            serve_auth_state(&req_ok, Some(configured)),
            "accepted",
            "exact bearer must be accepted",
        )?;

        // First byte differs.
        let req_first = request_with_auth("Bearer X234567890abcdef1234567890abcdef")?;
        ensure(
            serve_auth_state(&req_first, Some(configured)),
            "rejected",
            "first-byte mutation must be rejected",
        )?;

        // Last byte differs.
        let req_last = request_with_auth("Bearer 1234567890abcdef1234567890abcdeX")?;
        ensure(
            serve_auth_state(&req_last, Some(configured)),
            "rejected",
            "last-byte mutation must be rejected",
        )?;

        // Length mismatch — shorter.
        let req_short = request_with_auth("Bearer short")?;
        ensure(
            serve_auth_state(&req_short, Some(configured)),
            "rejected",
            "short bearer must be rejected",
        )?;

        // Length mismatch — longer.
        let req_long = request_with_auth(&format!("Bearer {configured}XYZ"))?;
        ensure(
            serve_auth_state(&req_long, Some(configured)),
            "rejected",
            "long bearer must be rejected",
        )?;

        // Wrong scheme casing — pre-fix accepted exact-case "Bearer "
        // only (via the format! left-hand side). Preserve that.
        let req_wrong_scheme = request_with_auth(&format!("bearer {configured}"))?;
        ensure(
            serve_auth_state(&req_wrong_scheme, Some(configured)),
            "rejected",
            "lowercase scheme must be rejected (matching prior contract)",
        )?;

        // Missing Authorization header is a separate "missing" state.
        let req_no_auth = parse_serve_http_request(
            b"GET /v1/search?q=x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            &ServeLimits::default(),
        )
        .map_err(|error| error.to_string())?;
        ensure(
            serve_auth_state(&req_no_auth, Some(configured)),
            "missing",
            "no Authorization header must report missing",
        )
    }

    #[test]
    fn serve_http_parser_rejects_chunked_with_transfer_extension_parameters() -> TestResult {
        // RFC 7230 §4.1 declares that `chunked` has no parameters, but
        // a permissive upstream proxy may still accept
        // `Transfer-Encoding: chunked; foo=bar` and frame the body as
        // chunked. The bare-string guard at 1b426516 compared the full
        // trimmed segment against the literal `chunked`, so any token
        // with a `;`-delimited parameter slipped through and let the
        // parser fall through to Content-Length framing — the same
        // CL.TE smuggling shape the prior fix was meant to close.
        //
        // Lock the param-strip extension across the four practical
        // bypass shapes: trailing param, parameterized chunked inside
        // a list, leading-CHUNKED with whitespace around the `;`, and
        // a bare `chunked;abc` with no equals-sign value.
        let raws: [&[u8]; 4] = [
            // trailing parameter on bare chunked
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked; foo=bar\r\nContent-Length: 0\r\n\r\n",
            // parameterized chunked at end of coding list
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: gzip, chunked; q=1\r\nContent-Length: 0\r\n\r\n",
            // case-insensitive + whitespace around `;`, parameterized chunked at head of list
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: CHUNKED ; ext=v, identity\r\nContent-Length: 0\r\n\r\n",
            // parameter without equals sign
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: identity, chunked;abc\r\nContent-Length: 0\r\n\r\n",
        ];
        for raw in raws {
            match parse_serve_http_request(raw, &ServeLimits::default()) {
                Ok(request) => {
                    return Err(format!(
                        "Transfer-Encoding with parameterized chunked must reject; got {request:?}",
                    ));
                }
                Err(error) => ensure(
                    error.code(),
                    "usage",
                    "parameterized-chunked rejection error code",
                )?,
            }
        }
        // Sanity check: a parameterized NON-chunked coding (e.g.
        // `gzip; q=0.5`) must still parse — the param-strip extension
        // targets `chunked` specifically, not all parameterized codings.
        parse_serve_http_request(
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: gzip; q=0.5\r\nContent-Length: 0\r\n\r\n",
            &ServeLimits::default(),
        )
        .map_err(|error| format!("TE: gzip; q=0.5 must still parse; got {error}"))?;
        Ok(())
    }

    #[test]
    fn serve_http_parser_rejects_duplicate_headers() -> TestResult {
        // RFC 7230 §3.3.3 bullet 4 smuggling guard: a request that
        // repeats Content-Length MUST be rejected regardless of
        // whether the duplicate values agree, because an upstream
        // proxy may parse the first while this parser previously kept
        // the last (BTreeMap::insert last-wins). The same disagreement
        // hazard applies to repeated Transfer-Encoding rows — the
        // chunked-in-TE-list guard only fires when a *single* row
        // names chunked, so a paired `identity` + `chunked` would have
        // collapsed to whichever survived BTreeMap::insert before the
        // smuggling check ran. The v2 parser rejects all duplicate
        // header rows, not only Content-Length / Transfer-Encoding;
        // the v2 surface does not implement RFC 7230 §3.2.2 list
        // combining for repeated names, so collapsing them silently
        // would lose information without any matching defense.
        let raws: [&[u8]; 5] = [
            // Two differing Content-Length rows — classic CL.CL split
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 100\r\nContent-Length: 50\r\n\r\n",
            // Two matching Content-Length rows — still a framing-
            // disagreement risk against permissive proxies
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            // Case-insensitive duplicate (normalized_name lowercases)
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\ncontent-length: 0\r\nContent-Length: 0\r\n\r\n",
            // Paired Transfer-Encoding rows would have bypassed the
            // chunked-in-TE-list check via last-wins overwrite
            b"POST /v1/durable-write HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: identity\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n",
            // Generic duplicate header (Host) — the v2 surface does
            // not silently collapse repeats
            b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nHost: attacker.example\r\n\r\n",
        ];
        for raw in raws {
            match parse_serve_http_request(raw, &ServeLimits::default()) {
                Ok(request) => {
                    return Err(format!("duplicate header row must reject; got {request:?}",));
                }
                Err(error) => ensure(
                    error.code(),
                    "usage",
                    "duplicate-header rejection error code",
                )?,
            }
        }
        Ok(())
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

    // bd-1zoiw: when the inner payload is a real ee.response.v2 envelope
    // with non-empty `degraded[]` entries (the shape pack-stream trailers
    // and cancellation frames will surface), the outer endpoint
    // envelope's `response.degradedCodes` MUST mirror the inner codes
    // instead of staying hard-coded to `[]`. Mirrors the auth-failure
    // envelope path that already populates the field.
    #[test]
    fn serve_sse_event_mirrors_inner_response_v2_degraded_codes() -> TestResult {
        let inner = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {"ok": true},
            "degraded": [
                {"code": "pack_assembly_slow", "severity": "low", "message": "synthetic"},
                {"code": "semantic_disabled", "severity": "info", "message": "synthetic"}
            ]
        });
        let frame = render_serve_sse_event("trailer", true, &inner);
        let data_line = frame
            .lines()
            .find(|line| line.starts_with("data: "))
            .ok_or_else(|| "missing data line".to_string())?;
        let event: serde_json::Value = serde_json::from_str(&data_line["data: ".len()..])
            .map_err(|error| error.to_string())?;
        let codes = event["response"]["degradedCodes"]
            .as_array()
            .ok_or_else(|| "degradedCodes must be an array".to_string())?
            .iter()
            .filter_map(|entry| entry.as_str())
            .collect::<Vec<_>>();
        ensure(codes.len(), 2, "mirror count")?;
        ensure(
            codes.contains(&"pack_assembly_slow"),
            true,
            "pack_assembly_slow mirrored",
        )?;
        ensure(
            codes.contains(&"semantic_disabled"),
            true,
            "semantic_disabled mirrored",
        )?;
        // Statuscode stays 200 because the inner envelope is not an error.
        ensure(
            event["response"]["statusCode"].as_u64(),
            Some(200),
            "statusCode stays 200 for ee.response.v2 with degraded[]",
        )
    }

    // Canonical ee.error.v2 renderers place degradation entries at the
    // top-level `degraded[]` field, matching docs/schemas/ee.error.v2.json.
    // The SSE endpoint envelope must mirror that field too, not only the
    // success-envelope `degraded[]` shape.
    #[test]
    fn serve_sse_event_mirrors_canonical_error_v2_top_level_degraded_codes() -> TestResult {
        let error = DomainError::Storage {
            message: "advisory lock timeout while waiting for workspace write lock".to_owned(),
            repair: Some(
                "ee diag advisory-lock --workspace . --resource-type workspace --json".to_owned(),
            ),
        };
        let inner: JsonValue = serde_json::from_str(&crate::output::error_response_json(&error))
            .map_err(|error| error.to_string())?;
        ensure(
            inner["degraded"][0]["code"].as_str(),
            Some(crate::models::degradation::ADVISORY_LOCK_TIMEOUT_CODE),
            "canonical error degraded code",
        )?;

        let frame = render_serve_sse_event("error", true, &inner);
        let data_line = frame
            .lines()
            .find(|line| line.starts_with("data: "))
            .ok_or_else(|| "missing data line".to_string())?;
        let event: serde_json::Value = serde_json::from_str(&data_line["data: ".len()..])
            .map_err(|error| error.to_string())?;
        let codes = event["response"]["degradedCodes"]
            .as_array()
            .ok_or_else(|| "degradedCodes must be an array".to_string())?
            .iter()
            .filter_map(|entry| entry.as_str())
            .collect::<Vec<_>>();
        ensure(
            codes,
            vec![crate::models::degradation::ADVISORY_LOCK_TIMEOUT_CODE],
            "canonical error.v2 mirror",
        )?;
        ensure(
            event["response"]["statusCode"].as_u64(),
            Some(500),
            "statusCode flips to 500 for ee.error.v2",
        )
    }

    // Older synthetic error envelopes placed degradation entries under
    // `error.degraded`. Keep accepting that shape as a compatibility
    // fallback so existing non-canonical producer fixtures still surface
    // their codes at the endpoint-envelope layer.
    #[test]
    fn serve_sse_event_accepts_nested_error_v2_degraded_codes() -> TestResult {
        let inner = json!({
            "schema": "ee.error.v2",
            "success": false,
            "error": {
                "code": "usage",
                "severity": "low",
                "message": "synthetic failure",
                "degraded": [
                    {"code": "synthetic_inner", "severity": "warning", "message": "x"}
                ]
            }
        });
        let frame = render_serve_sse_event("error", true, &inner);
        let data_line = frame
            .lines()
            .find(|line| line.starts_with("data: "))
            .ok_or_else(|| "missing data line".to_string())?;
        let event: serde_json::Value = serde_json::from_str(&data_line["data: ".len()..])
            .map_err(|error| error.to_string())?;
        let codes = event["response"]["degradedCodes"]
            .as_array()
            .ok_or_else(|| "degradedCodes must be an array".to_string())?
            .iter()
            .filter_map(|entry| entry.as_str())
            .collect::<Vec<_>>();
        ensure(codes, vec!["synthetic_inner"], "error.v2 mirror")?;
        ensure(
            event["response"]["statusCode"].as_u64(),
            Some(500),
            "statusCode flips to 500 for ee.error.v2",
        )
    }

    // bd-1zoiw: sanity-pin the empty-degraded path so the mirror
    // helper's fallback (no degraded[] in either ee.response.v2 or
    // wrapped data payload) reliably yields `[]`. Without this pin
    // the positive mirror assertions above could pass for the wrong
    // reason (e.g. the helper accidentally falls back to the wrapped
    // payload's data field instead of the degraded field).
    #[test]
    fn serve_sse_event_emits_empty_degraded_codes_when_inner_payload_has_none() -> TestResult {
        let frame = render_serve_sse_event("complete", true, &json!({"ok": true}));
        let data_line = frame
            .lines()
            .find(|line| line.starts_with("data: "))
            .ok_or_else(|| "missing data line".to_string())?;
        let event: serde_json::Value = serde_json::from_str(&data_line["data: ".len()..])
            .map_err(|error| error.to_string())?;
        ensure(
            event["response"]["degradedCodes"].as_array().map(Vec::len),
            Some(0),
            "empty degradedCodes when no inner degraded entries",
        )
    }

    // bd-2eiwy: the non-SSE dispatch envelope for /v1/status (and any
    // future endpoint whose ee.response.v2 payload carries degradations)
    // MUST surface inner `degraded[]` codes at the outer
    // `response.degradedCodes` level. The /v1/status caller chain runs
    // through serve_dispatch_payload_for_plan → serve_status_payload_json
    // → render_status_json which can produce a real status report with
    // codes like `index_stale`, `embed_model_unavailable`,
    // `search_index_stale`, etc. Before this fix the outer envelope
    // hard-coded `degradedCodes: []` so operators triaging /v1/status by
    // reading the response-metadata field saw zero signal.
    #[test]
    fn serve_dispatch_exchange_envelope_mirrors_inner_payload_degraded_codes() -> TestResult {
        let request = parse_serve_http_request(
            b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            &ServeLimits::default(),
        )
        .map_err(|error| error.to_string())?;
        let payload = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {
                "schema": "ee.status.v1",
                "ok": true
            },
            "degraded": [
                {"code": "index_stale", "severity": "medium", "message": "synthetic"},
                {"code": "embed_model_unavailable", "severity": "warning", "message": "synthetic"}
            ]
        });
        let envelope = serve_dispatch_exchange_envelope(
            "req-bd-2eiwy",
            &request,
            "not_required",
            200,
            &payload,
            42,
        );
        let codes = envelope["response"]["degradedCodes"]
            .as_array()
            .ok_or_else(|| "degradedCodes must be an array".to_string())?
            .iter()
            .filter_map(|entry| entry.as_str())
            .collect::<Vec<_>>();
        ensure(codes.len(), 2, "mirror count")?;
        ensure(codes.contains(&"index_stale"), true, "index_stale surfaced")?;
        ensure(
            codes.contains(&"embed_model_unavailable"),
            true,
            "embed_model_unavailable surfaced",
        )?;
        ensure(
            envelope["response"]["statusCode"].as_u64(),
            Some(200),
            "statusCode unchanged by mirror",
        )?;
        ensure(
            envelope["response"]["payloadSchema"].as_str(),
            Some("ee.response.v2"),
            "payloadSchema unchanged",
        )
    }

    // bd-2eiwy: empty-input sanity-pin — when the inner payload has no
    // `degraded[]` array (or an empty one), the outer envelope's
    // degradedCodes must be `[]`. Without this pin, a future drift in
    // the mirror helper that incorrectly fell back to `data` or another
    // field would not be caught by the positive test above.
    #[test]
    fn serve_dispatch_exchange_envelope_emits_empty_degraded_codes_when_inner_empty() -> TestResult
    {
        let request = parse_serve_http_request(
            b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            &ServeLimits::default(),
        )
        .map_err(|error| error.to_string())?;
        let payload_with_empty = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {"ok": true},
            "degraded": []
        });
        let envelope_a = serve_dispatch_exchange_envelope(
            "req-empty-arr",
            &request,
            "not_required",
            200,
            &payload_with_empty,
            7,
        );
        ensure(
            envelope_a["response"]["degradedCodes"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "empty `degraded: []` propagates to empty degradedCodes",
        )?;
        let payload_without_degraded = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {"ok": true}
        });
        let envelope_b = serve_dispatch_exchange_envelope(
            "req-missing-field",
            &request,
            "not_required",
            200,
            &payload_without_degraded,
            7,
        );
        ensure(
            envelope_b["response"]["degradedCodes"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "missing `degraded` field yields empty degradedCodes",
        )
    }

    #[test]
    fn serve_error_exchange_envelope_includes_canonical_error_degraded_codes() -> TestResult {
        let request = parse_serve_http_request(
            b"GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            &ServeLimits::default(),
        )
        .map_err(|error| error.to_string())?;
        let error = DomainError::Storage {
            message: "advisory lock timeout while waiting for workspace write lock".to_owned(),
            repair: Some(
                "ee diag advisory-lock --workspace . --resource-type workspace --json".to_owned(),
            ),
        };

        let envelope = serve_error_exchange_envelope(
            "req-error-degraded",
            &request,
            "accepted",
            503,
            &error,
            9,
        );
        let codes = envelope["response"]["degradedCodes"]
            .as_array()
            .ok_or_else(|| "degradedCodes must be an array".to_string())?
            .iter()
            .filter_map(|entry| entry.as_str())
            .collect::<Vec<_>>();

        ensure(codes.contains(&"storage"), true, "domain error code kept")?;
        ensure(
            codes.contains(&crate::models::degradation::ADVISORY_LOCK_TIMEOUT_CODE),
            true,
            "canonical error degraded code surfaced",
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
    fn serve_transport_exchange_executes_search_endpoint_or_returns_real_search_error() -> TestResult
    {
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
        let payload = &envelope["response"]["payload"];
        if response.starts_with("HTTP/1.1 200 OK\r\n") {
            ensure(
                payload["schema"].as_str(),
                Some("ee.response.v2"),
                "search response schema",
            )?;
            ensure(
                payload["data"]["command"].as_str(),
                Some("search"),
                "search command",
            )?;
            ensure(
                payload["data"]["query"].as_str(),
                Some("release check"),
                "search query",
            )?;
        } else if response.starts_with("HTTP/1.1 500 Internal Server Error\r\n") {
            ensure(
                payload["schema"].as_str(),
                Some("ee.error.v2"),
                "search error schema",
            )?;
            ensure(
                payload["error"]["code"].as_str(),
                Some("search_index"),
                "search error code",
            )?;
        } else {
            return Err(format!(
                "search endpoint expected 200 or real search-index error, got {response}"
            ));
        }
        ensure(
            payload["data"]["businessLogicExecuted"].is_null(),
            true,
            "search endpoint must not return the old dispatch-plan-only stub payload",
        )
    }

    #[test]
    fn serve_transport_exchange_executes_status_endpoint_payload() -> TestResult {
        let token = "01234567890123456789012345678901";
        let raw = format!(
            "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        let response = render_serve_transport_exchange(
            "req-status-exec",
            raw.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            17,
        );

        ensure(
            response.starts_with("HTTP/1.1 200 OK\r\n"),
            true,
            "status endpoint status line",
        )?;
        ensure(
            response.contains(token),
            false,
            "status endpoint response must not expose token",
        )?;
        let (_, body) = split_http_response(&response)?;
        let envelope: JsonValue = serde_json::from_str(body).map_err(|error| error.to_string())?;
        ensure(
            envelope["schema"].as_str(),
            Some(SERVE_ENDPOINT_SCHEMA_V1),
            "status transport envelope schema",
        )?;
        ensure(
            envelope["response"]["payload"]["schema"].as_str(),
            Some("ee.response.v2"),
            "status payload schema",
        )?;
        ensure(
            envelope["response"]["payload"]["success"].as_bool(),
            Some(true),
            "status payload success",
        )?;
        ensure(
            envelope["response"]["payload"]["data"]["command"].as_str(),
            Some("status"),
            "status payload command",
        )?;
        ensure(
            envelope["response"]["payload"]["data"]["runtime"]["engine"].as_str(),
            Some("asupersync"),
            "status runtime engine",
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

    // bd-da9h1: unsupported /v1/* paths must answer 404 regardless of auth
    // posture — the previous gate routed every Unknown request through 401
    // because serve_auth_state returns "not_required" for endpoints that
    // declare auth_required()=false, but the gate only accepted "accepted".
    #[test]
    fn serve_transport_exchange_unknown_endpoint_returns_404_without_auth() -> TestResult {
        let raw_no_token_no_header = "GET /v1/nope HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".to_owned();
        let response_no_token = render_serve_transport_exchange(
            "req-unknown-noauth",
            raw_no_token_no_header.as_bytes(),
            &ServeLimits::default(),
            None,
            3,
        );
        ensure(
            response_no_token.starts_with("HTTP/1.1 404 Not Found\r\n"),
            true,
            "unknown endpoint must 404 when server has no token configured",
        )?;
        let (_, body_no_token) = split_http_response(&response_no_token)?;
        let envelope_no_token: JsonValue =
            serde_json::from_str(body_no_token).map_err(|error| error.to_string())?;
        ensure(
            envelope_no_token["schema"].as_str(),
            Some(SERVE_ENDPOINT_SCHEMA_V1),
            "unknown 404 envelope schema (no server token)",
        )?;
        ensure(
            envelope_no_token["request"]["endpoint"].as_str(),
            Some("unknown"),
            "unknown 404 endpoint metadata (no server token)",
        )?;
        ensure(
            envelope_no_token["response"]["payload"]["schema"].as_str(),
            Some("ee.error.v2"),
            "unknown 404 wraps error envelope (no server token)",
        )?;

        let token = "01234567890123456789012345678901";
        let raw_token_no_header = "GET /v1/nope HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".to_owned();
        let response_no_header = render_serve_transport_exchange(
            "req-unknown-no-header",
            raw_token_no_header.as_bytes(),
            &ServeLimits::default(),
            Some(token),
            4,
        );
        ensure(
            response_no_header.starts_with("HTTP/1.1 404 Not Found\r\n"),
            true,
            "unknown endpoint must 404 when server has token but client sends no header",
        )?;
        let (_, body_no_header) = split_http_response(&response_no_header)?;
        let envelope_no_header: JsonValue =
            serde_json::from_str(body_no_header).map_err(|error| error.to_string())?;
        ensure(
            envelope_no_header["request"]["endpoint"].as_str(),
            Some("unknown"),
            "unknown 404 endpoint metadata (server token, no client header)",
        )
    }

    #[test]
    fn serve_transport_exchange_returns_terminal_events_sse_frame() -> TestResult {
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
        let event_is_terminal =
            body.starts_with("event: complete\n") || body.starts_with("event: error\n");
        ensure(event_is_terminal, true, "transport sse terminal event")?;
        let data_line = body
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .ok_or_else(|| "missing transport sse data line".to_owned())?;
        let event: JsonValue =
            serde_json::from_str(data_line).map_err(|error| error.to_string())?;
        ensure(
            event["sse"]["terminal"].as_bool(),
            Some(true),
            "events terminal frame",
        )?;
        let payload = &event["response"]["payload"];
        match event["sse"]["eventKind"].as_str() {
            Some("complete") => {
                ensure(
                    payload["schema"].as_str(),
                    Some("ee.response.v2"),
                    "events response schema",
                )?;
                ensure(
                    payload["data"]["command"].as_str(),
                    Some("subscribe poll"),
                    "events command",
                )?;
                ensure(
                    payload["data"]["serve"]["dispatchPlan"]["endpoint"].as_str(),
                    Some("events"),
                    "events dispatch plan",
                )
            }
            Some("error") => {
                ensure(
                    payload["schema"].as_str(),
                    Some("ee.error.v2"),
                    "events error schema",
                )?;
                ensure(
                    payload["error"]["code"].as_str(),
                    Some("storage"),
                    "events storage error",
                )
            }
            other => Err(format!("unexpected events SSE kind {other:?}: {event}")),
        }
    }

    #[test]
    fn serve_single_connection_exchange_round_trips_real_loopback_stream() -> TestResult {
        let token = "01234567890123456789012345678901";
        let server_token = token.to_owned();
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let addr = listener.local_addr().map_err(|error| error.to_string())?;
        let server = std::thread::spawn(move || -> Result<ServeConnectionExchange, String> {
            let (stream, _) = listener.accept().map_err(|error| error.to_string())?;
            serve_single_connection_exchange(
                stream,
                "req-loopback",
                &ServeLimits::default(),
                Some(server_token.as_str()),
                7,
            )
            .map_err(|error| error.to_string())
        });

        let mut client = std::net::TcpStream::connect(addr).map_err(|error| error.to_string())?;
        let request = format!(
            "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        client
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        client
            .shutdown(std::net::Shutdown::Write)
            .map_err(|error| error.to_string())?;
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .map_err(|error| error.to_string())?;
        let exchange = server
            .join()
            .map_err(|_| "server thread panicked".to_owned())??;

        ensure(
            exchange.response_status_line.as_str(),
            "HTTP/1.1 200 OK",
            "exchange status line",
        )?;
        ensure(
            response.starts_with("HTTP/1.1 200 OK\r\n"),
            true,
            "client status line",
        )?;
        ensure(
            response.contains(token),
            false,
            "single connection response must not expose token",
        )?;
        let (headers, body) = split_http_response(&response)?;
        ensure(
            header_value(headers, "Connection"),
            Some("close"),
            "connection close",
        )?;
        let envelope: JsonValue = serde_json::from_str(body).map_err(|error| error.to_string())?;
        ensure(
            envelope["schema"].as_str(),
            Some(SERVE_ENDPOINT_SCHEMA_V1),
            "connection envelope schema",
        )?;
        ensure(
            envelope["request"]["endpoint"].as_str(),
            Some("status"),
            "connection endpoint",
        )?;
        ensure(
            envelope["response"]["payload"]["data"]["command"].as_str(),
            Some("status"),
            "connection status payload",
        )?;
        ensure(
            exchange.request_bytes,
            request.len(),
            "exchange request bytes",
        )?;
        ensure(
            exchange.response_bytes,
            response.len(),
            "exchange response bytes",
        )
    }

    #[test]
    fn serve_total_read_limit_overflow_fails_closed() -> TestResult {
        let limits = ServeLimits {
            max_header_bytes: usize::MAX,
            max_body_bytes: 1,
            ..ServeLimits::default()
        };
        let error = serve_total_read_limit(&limits).expect_err("overflow must fail closed");

        ensure(
            error.to_string().contains("overflows usize"),
            true,
            "overflow error text",
        )
    }

    #[test]
    fn serve_request_complete_len_overflow_fails_closed() -> TestResult {
        let request = b"POST /v1/status HTTP/1.1\r\nContent-Length: 18446744073709551615\r\n\r\n";
        let limits = ServeLimits {
            max_body_bytes: usize::MAX,
            ..ServeLimits::default()
        };
        let error = serve_request_complete_len(request, &limits)
            .expect_err("overflowing complete length must fail closed");

        ensure(
            error.to_string().contains("overflows usize"),
            true,
            "overflow error text",
        )
    }

    #[test]
    fn serve_accept_once_round_trips_bound_listener_connection() -> TestResult {
        let token = "01234567890123456789012345678901";
        let server_token = token.to_owned();
        let limits = ServeLimits {
            connection_read_timeout_ms: 1_000,
            ..ServeLimits::default()
        };
        let options = ServeStartupOptions {
            port: 0,
            limits: limits.clone(),
            ..ServeStartupOptions::default()
        };
        let binding =
            bind_serve_listener(&options, Some(token)).map_err(|error| error.to_string())?;
        let addr = binding
            .listener
            .local_addr()
            .map_err(|error| error.to_string())?;
        let server = std::thread::spawn(move || -> Result<ServeAcceptedConnection, String> {
            serve_accept_once(
                &binding.listener,
                "req-accept-once",
                &limits,
                Some(server_token.as_str()),
                13,
            )
            .map_err(|error| error.to_string())
        });

        let mut client = std::net::TcpStream::connect(addr).map_err(|error| error.to_string())?;
        let request = format!(
            "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        client
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        client
            .shutdown(std::net::Shutdown::Write)
            .map_err(|error| error.to_string())?;
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .map_err(|error| error.to_string())?;
        let accepted = server
            .join()
            .map_err(|_| "server thread panicked".to_owned())??;

        ensure(
            accepted.peer_addr.ip().is_loopback(),
            true,
            "accepted peer loopback",
        )?;
        ensure(
            accepted.accept_attempts >= 1,
            true,
            "accept attempts recorded",
        )?;
        ensure(
            accepted.exchange.response_status_line.as_str(),
            "HTTP/1.1 200 OK",
            "accepted status line",
        )?;
        let (headers, body) = split_http_response(&response)?;
        ensure(
            header_value(headers, "Connection"),
            Some("close"),
            "accepted connection close",
        )?;
        let envelope: JsonValue = serde_json::from_str(body).map_err(|error| error.to_string())?;
        ensure(
            envelope["request"]["endpoint"].as_str(),
            Some("status"),
            "accepted endpoint",
        )?;
        ensure(
            accepted.exchange.response_bytes,
            response.len(),
            "accepted response bytes",
        )
    }

    #[test]
    fn serve_accept_once_times_out_without_pending_connection() -> TestResult {
        let token = "01234567890123456789012345678901";
        let limits = ServeLimits {
            connection_read_timeout_ms: 0,
            ..ServeLimits::default()
        };
        let options = ServeStartupOptions {
            port: 0,
            limits: limits.clone(),
            ..ServeStartupOptions::default()
        };
        let binding =
            bind_serve_listener(&options, Some(token)).map_err(|error| error.to_string())?;
        let error =
            match serve_accept_once(&binding.listener, "req-timeout", &limits, Some(token), 0) {
                Ok(accepted) => {
                    return Err(format!(
                        "empty accept queue should time out, got {accepted:?}"
                    ));
                }
                Err(error) => error,
            };

        ensure(
            error.to_string().contains("Timed out waiting"),
            true,
            "accept timeout error",
        )
    }

    #[test]
    fn serve_foreground_once_binds_then_accepts_single_connection() -> TestResult {
        let token = "01234567890123456789012345678901";
        let server_token = token.to_owned();
        let limits = ServeLimits {
            connection_read_timeout_ms: 1_000,
            ..ServeLimits::default()
        };
        let options = ServeStartupOptions {
            port: 0,
            limits,
            ..ServeStartupOptions::default()
        };
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || -> Result<ServeForegroundOnceReport, String> {
            serve_foreground_once(
                &options,
                Some(server_token.as_str()),
                "req-foreground-once",
                21,
                |binding| {
                    let addr = binding.listener.local_addr().map_err(|error| {
                        serve_transport_io_error("inspect test listener local address", error)
                    })?;
                    addr_tx
                        .send(addr)
                        .map_err(|error| DomainError::Configuration {
                            message: format!(
                                "Failed to share ee serve test bound address: {error}"
                            ),
                            repair: Some("Retry the serve foreground-once test.".to_owned()),
                        })
                },
            )
            .map_err(|error| error.to_string())
        });

        let addr = addr_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|error| error.to_string())?;
        let mut client = std::net::TcpStream::connect(addr).map_err(|error| error.to_string())?;
        let request = format!(
            "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\r\n"
        );
        client
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        client
            .shutdown(std::net::Shutdown::Write)
            .map_err(|error| error.to_string())?;
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .map_err(|error| error.to_string())?;
        let report = server
            .join()
            .map_err(|_| "server thread panicked".to_owned())??;

        ensure(report.bound_addr, addr, "foreground bound addr")?;
        ensure(
            report.listener_metadata["schema"].as_str(),
            Some(SERVE_STARTUP_SCHEMA_V1),
            "foreground listener schema",
        )?;
        ensure(
            report.listener_metadata.to_string().contains(token),
            false,
            "foreground metadata does not expose token",
        )?;
        ensure(
            report.accepted.peer_addr.ip().is_loopback(),
            true,
            "foreground peer loopback",
        )?;
        ensure(
            report.accepted.exchange.response_status_line.as_str(),
            "HTTP/1.1 200 OK",
            "foreground status line",
        )?;
        let (_, body) = split_http_response(&response)?;
        let envelope: JsonValue = serde_json::from_str(body).map_err(|error| error.to_string())?;
        ensure(
            envelope["request"]["endpoint"].as_str(),
            Some("status"),
            "foreground endpoint",
        )?;
        ensure(
            report.accepted.exchange.response_bytes,
            response.len(),
            "foreground response bytes",
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
