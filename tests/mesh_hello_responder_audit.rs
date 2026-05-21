use std::collections::BTreeSet;
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use ee::config::EnvVar;
use ee::db::audit_actions;
use ee::mesh::discovery_policy::DiscoveryMode;
use ee::mesh::hello::{
    HELLO_ERROR_SCHEMA_V1, HELLO_PROTOCOL_VERSION_MAJOR, HELLO_REQUEST_SCHEMA_V1,
    HELLO_RESPONSE_SCHEMA_V1, HelloErrorCode, HelloRequest, ResponderContext,
    assert_no_responder_metadata_leak, decide_hello_response,
};
use ee::mesh::hello_responder::{
    DEFAULT_HELLO_RESPONDER_PORT, HELLO_RESPONDER_CRASHED_RESTARTED_EVENT,
    HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1, HELLO_RESPONDER_NO_TAILSCALE_IP_CODE,
    HELLO_RESPONDER_NOT_RUNNING_CODE, HELLO_RESPONDER_STARTED_EVENT,
    HELLO_RESPONDER_STATUS_SCHEMA_V1, HELLO_RESPONDER_STOPPED_EVENT, HelloResponderAdmission,
    HelloResponderLifecycleEventKind, HelloResponderRateLimiter, HelloResponderRuntimeInput,
    HelloResponderStatusReport, lifecycle_audit, validate_tailnet_header,
};
use serde_json::Value;

const MESH_HELLO_FIXTURES: &[&str] = &[
    "tests/fixtures/mesh_hello/consent_denied.json",
    "tests/fixtures/mesh_hello/consent_granted.json",
    "tests/fixtures/mesh_hello/decline_no_metadata_leak.json",
    "tests/fixtures/mesh_hello/mesh_disabled.json",
    "tests/fixtures/mesh_hello/shields_up_decline.json",
    "tests/fixtures/mesh_hello/unauth_decline.json",
    "tests/fixtures/mesh_hello/unknown_fields.json",
    "tests/fixtures/mesh_hello/version_skew_major.json",
    "tests/fixtures/mesh_hello/version_skew_minor.json",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn value_at<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a Value, String> {
    value
        .pointer(pointer)
        .ok_or_else(|| format!("{context}: missing {pointer}"))
}

fn string_at(value: &Value, pointer: &str, context: &str) -> Result<String, String> {
    value_at(value, pointer, context)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{context}: {pointer} must be a string"))
}

fn bool_at(value: &Value, pointer: &str, context: &str) -> Result<bool, String> {
    value_at(value, pointer, context)?
        .as_bool()
        .ok_or_else(|| format!("{context}: {pointer} must be a bool"))
}

fn string_array_at(value: &Value, pointer: &str, context: &str) -> Result<Vec<String>, String> {
    value_at(value, pointer, context)?
        .as_array()
        .ok_or_else(|| format!("{context}: {pointer} must be an array"))?
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context}: {pointer}[{idx}] must be a string"))
        })
        .collect()
}

fn fixture_request(value: &Value, context: &str) -> Result<HelloRequest, String> {
    let schema = string_at(value, "/request/schema", context)?;
    if schema != HELLO_REQUEST_SCHEMA_V1 {
        return Err(format!(
            "{context}: request schema {schema} != {HELLO_REQUEST_SCHEMA_V1}"
        ));
    }
    Ok(HelloRequest {
        schema: HELLO_REQUEST_SCHEMA_V1,
        request_id: string_at(value, "/request/requestId", context)?,
        requester_node_key: string_at(value, "/request/requesterNodeKey", context)?,
        requester_ee_version: string_at(value, "/request/requesterEeVersion", context)?,
        requester_ee_protocol_version: string_at(
            value,
            "/request/requesterEeProtocolVersion",
            context,
        )?,
        requester_workspace_ids: string_array_at(value, "/request/requesterWorkspaceIds", context)?,
        requester_capabilities: string_array_at(value, "/request/requesterCapabilities", context)?,
        requester_advertised_tags: string_array_at(
            value,
            "/request/requesterAdvertisedTags",
            context,
        )?,
    })
}

fn expected_response_array(
    value: &Value,
    pointer: &str,
    fallback: &[String],
    context: &str,
) -> Result<Vec<String>, String> {
    if value.pointer(pointer).is_some() {
        string_array_at(value, pointer, context)
    } else {
        Ok(fallback.to_vec())
    }
}

fn assert_expected_outcome(
    value: &Value,
    request: &HelloRequest,
    context: &str,
) -> Result<(), String> {
    let expected_kind = string_at(value, "/expected/kind", context)?;
    let responder_tags =
        string_array_at(value, "/responderContext/responderAdvertisedTags", context)?;
    let responder_workspace_ids = expected_response_array(
        value,
        "/expected/response/responderWorkspaceIds",
        &request.requester_workspace_ids,
        context,
    )?;
    let responder_capabilities = expected_response_array(
        value,
        "/expected/response/responderCapabilities",
        &["discovery".to_owned()],
        context,
    )?;
    let respond_allowlist = BTreeSet::new();
    let denylist = BTreeSet::new();
    let respond_mode: DiscoveryMode =
        serde_json::from_value(value_at(value, "/responderContext/respondMode", context)?.clone())
            .map_err(|error| format!("{context}: parse respondMode: {error}"))?;
    let ctx = ResponderContext {
        mesh_enabled: bool_at(value, "/responderContext/meshEnabled", context)?,
        tailscale_authenticated: bool_at(
            value,
            "/responderContext/tailscaleAuthenticated",
            context,
        )?,
        shields_up: bool_at(value, "/responderContext/shieldsUp", context)?,
        respond_mode,
        responder_node_key: "nodekey:responder",
        responder_ee_version: "0.2.0",
        responder_workspace_ids: &responder_workspace_ids,
        responder_capabilities: &responder_capabilities,
        responder_advertised_tags: &responder_tags,
        respond_allowlist: &respond_allowlist,
        denylist: &denylist,
        rate_limited: bool_at(value, "/responderContext/rateLimited", context)?,
        elapsed_micros: 42,
    };

    let outcome = decide_hello_response(request, &ctx);
    match expected_kind.as_str() {
        "granted" => {
            let response = outcome
                .response()
                .ok_or_else(|| format!("{context}: expected granted outcome"))?;
            let actual = serde_json::to_value(response)
                .map_err(|error| format!("{context}: serialize response: {error}"))?;
            let expected = value_at(value, "/expected/response", context)?;
            if actual != *expected {
                return Err(format!(
                    "{context}: granted response mismatch\nactual={actual}\nexpected={expected}"
                ));
            }
            if response.schema != HELLO_RESPONSE_SCHEMA_V1 {
                return Err(format!("{context}: response schema drifted"));
            }
        }
        "declined" => {
            let error = outcome
                .error()
                .ok_or_else(|| format!("{context}: expected declined outcome"))?;
            assert_no_responder_metadata_leak(error)
                .map_err(|field| format!("{context}: decline leaked {field}"))?;
            let actual = serde_json::to_value(error)
                .map_err(|ser_error| format!("{context}: serialize error: {ser_error}"))?;
            let expected = value_at(value, "/expected/error", context)?;
            if actual != *expected {
                return Err(format!(
                    "{context}: declined error mismatch\nactual={actual}\nexpected={expected}"
                ));
            }
            if error.schema != HELLO_ERROR_SCHEMA_V1 {
                return Err(format!("{context}: error schema drifted"));
            }
        }
        other => return Err(format!("{context}: unknown expected kind {other}")),
    }
    Ok(())
}

#[test]
fn hello_responder_status_is_schema_pinned_and_redaction_safe() -> Result<(), String> {
    let mut input = HelloResponderRuntimeInput::new(true);
    input.tailscale_ip = Some(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9)));
    input.running = true;
    input.accepted_requests_1h = 7;

    let report = HelloResponderStatusReport::from_runtime(&input);
    let value = serde_json::to_value(&report).map_err(|error| error.to_string())?;

    assert_eq!(value["schema"], HELLO_RESPONDER_STATUS_SCHEMA_V1);
    assert_eq!(value["listenAddress"], "100.64.0.9:41888");
    assert_eq!(value["acceptedRequests1h"], 7);
    assert_eq!(value.get("tailscaleNodeKey"), None);
    assert_eq!(value.get("tailnetId"), None);
    assert_eq!(report.degraded, Vec::new());
    Ok(())
}

#[test]
fn hello_responder_enabled_without_daemon_emits_status_degradations() {
    let report = HelloResponderStatusReport::from_runtime(&HelloResponderRuntimeInput::new(true));
    let codes: Vec<&str> = report.degraded.iter().map(|item| item.code).collect();

    assert!(codes.contains(&HELLO_RESPONDER_NO_TAILSCALE_IP_CODE));
    assert!(codes.contains(&HELLO_RESPONDER_NOT_RUNNING_CODE));
}

#[test]
fn hello_responder_env_vars_are_registered_with_defaults() {
    assert!(EnvVar::all().contains(&EnvVar::MeshHelloPort));
    assert!(EnvVar::all().contains(&EnvVar::MeshHelloResponderDisabled));
    assert_eq!(EnvVar::MeshHelloPort.default_value(), Some("41888"));
    assert_eq!(DEFAULT_HELLO_RESPONDER_PORT, 41888);
    assert_eq!(
        EnvVar::MeshHelloResponderDisabled.default_value(),
        Some("false")
    );
    assert_eq!(EnvVar::MeshHelloPort.category(), "mesh");
}

#[test]
fn mesh_hello_fixtures_replay_against_runtime_handler() -> Result<(), String> {
    let mut saw_minor_skew = false;
    let mut saw_major_skew = false;

    for fixture_path in MESH_HELLO_FIXTURES {
        let value = read_json(fixture_path)?;
        let scenario = string_at(&value, "/scenario", fixture_path)?;
        let request = fixture_request(&value, fixture_path)?;

        if scenario == "version_skew_minor" {
            saw_minor_skew = true;
            let requester_major = request
                .requester_ee_protocol_version
                .split('.')
                .next()
                .and_then(|part| part.parse::<u32>().ok())
                .ok_or_else(|| format!("{fixture_path}: version_skew_minor lacks major"))?;
            assert_eq!(requester_major, HELLO_PROTOCOL_VERSION_MAJOR);
            assert_eq!(
                string_at(&value, "/expected/kind", fixture_path)?,
                "granted"
            );
        }

        if scenario == "version_skew_major" {
            saw_major_skew = true;
            let requester_major = request
                .requester_ee_protocol_version
                .split('.')
                .next()
                .and_then(|part| part.parse::<u32>().ok())
                .ok_or_else(|| format!("{fixture_path}: version_skew_major lacks major"))?;
            assert_ne!(requester_major, HELLO_PROTOCOL_VERSION_MAJOR);
            assert_eq!(
                string_at(&value, "/expected/error/code", fixture_path)?,
                HelloErrorCode::UnsupportedProtocolVersion.as_str()
            );
        }

        assert_expected_outcome(&value, &request, fixture_path)?;
    }

    assert!(
        saw_minor_skew,
        "mesh hello fixtures must include same-major minor skew coverage"
    );
    assert!(
        saw_major_skew,
        "mesh hello fixtures must include cross-major decline coverage"
    );
    Ok(())
}

#[test]
fn hello_responder_rate_limit_is_per_peer_and_windowed() {
    let mut limiter = HelloResponderRateLimiter::default();

    for _ in 0..16 {
        assert!(limiter.admit("node-a", 100).allowed());
    }
    assert_eq!(
        limiter.admit("node-a", 100),
        HelloResponderAdmission::RateLimited {
            retry_after_seconds: 60
        }
    );
    assert!(limiter.admit("node-b", 100).allowed());
    assert!(limiter.admit("node-a", 160).allowed());
}

#[test]
fn hello_responder_tailnet_header_is_required_and_exact() {
    assert!(validate_tailnet_header("tailnet-a", Some("tailnet-a")).is_ok());
    assert_eq!(
        validate_tailnet_header("tailnet-a", Some("tailnet-b"))
            .unwrap_err()
            .code,
        "tailnet_mismatch"
    );
    assert_eq!(
        validate_tailnet_header("tailnet-a", None).unwrap_err().code,
        "tailnet_header_missing"
    );
}

#[test]
fn lifecycle_audit_events_match_db_action_constants() {
    assert_eq!(
        audit_actions::MESH_HELLO_RESPONDER_STARTED,
        HELLO_RESPONDER_STARTED_EVENT
    );
    assert_eq!(
        audit_actions::MESH_HELLO_RESPONDER_STOPPED,
        HELLO_RESPONDER_STOPPED_EVENT
    );
    assert_eq!(
        audit_actions::MESH_HELLO_RESPONDER_CRASHED_RESTARTED,
        HELLO_RESPONDER_CRASHED_RESTARTED_EVENT
    );

    let mut input = HelloResponderRuntimeInput::new(true);
    input.crash_count_24h = 3;
    let status = HelloResponderStatusReport::from_runtime(&input);
    let audit = lifecycle_audit(HelloResponderLifecycleEventKind::Started, &status);
    assert_eq!(audit.schema, HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1);
    assert_eq!(audit.event_type, HELLO_RESPONDER_STARTED_EVENT);
}
