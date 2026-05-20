use std::net::{IpAddr, Ipv4Addr};

use ee::config::EnvVar;
use ee::db::audit_actions;
use ee::mesh::hello_responder::{
    DEFAULT_HELLO_RESPONDER_PORT, HELLO_RESPONDER_CRASHED_RESTARTED_EVENT,
    HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1, HELLO_RESPONDER_NO_TAILSCALE_IP_CODE,
    HELLO_RESPONDER_NOT_RUNNING_CODE, HELLO_RESPONDER_STARTED_EVENT,
    HELLO_RESPONDER_STATUS_SCHEMA_V1, HELLO_RESPONDER_STOPPED_EVENT, HelloResponderAdmission,
    HelloResponderLifecycleEventKind, HelloResponderRateLimiter, HelloResponderRuntimeInput,
    HelloResponderStatusReport, lifecycle_audit, validate_tailnet_header,
};

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
