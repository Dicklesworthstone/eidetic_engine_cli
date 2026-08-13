//! SRR6.46.12 hello responder lifecycle and status helpers.
//!
//! The request handler in [`crate::mesh::hello`] stays pure. This module owns
//! the foreground/daemon lifecycle contract around that handler: env-gated
//! config, operator status, restart/audit event names, rate limiting, and
//! redaction-safe request admission helpers.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};

use serde::Serialize;

use crate::config::{EnvVar, read_env_var};
use crate::db::{CreateAuditInput, DbConnection, generate_audit_id};
use crate::models::DomainError;

pub const HELLO_RESPONDER_STATUS_SCHEMA_V1: &str = "ee.mesh.hello_responder.status.v1";
pub const HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1: &str =
    "ee.mesh.hello_responder.lifecycle_audit.v1";

pub const DEFAULT_HELLO_RESPONDER_PORT: u16 = 41888;
pub const DEFAULT_HELLO_RESPONDER_RATE_LIMIT_PER_PEER: u32 = 16;
pub const DEFAULT_HELLO_RESPONDER_RATE_WINDOW_SECONDS: u64 = 60;

pub const HELLO_RESPONDER_NOT_RUNNING_CODE: &str = "hello_responder_not_running";
pub const HELLO_RESPONDER_PORT_IN_USE_CODE: &str = "hello_responder_port_in_use";
pub const HELLO_RESPONDER_NO_TAILSCALE_IP_CODE: &str = "hello_responder_no_tailscale_ip";
pub const HELLO_RESPONDER_CRASH_LOOP_CODE: &str = "hello_responder_crash_loop";
pub const HELLO_RESPONDER_RATE_LIMITED_STORM_CODE: &str = "hello_responder_rate_limited_storm";

pub const HELLO_RESPONDER_STARTED_EVENT: &str = "mesh.hello_responder_started";
pub const HELLO_RESPONDER_STOPPED_EVENT: &str = "mesh.hello_responder_stopped";
pub const HELLO_RESPONDER_CRASHED_RESTARTED_EVENT: &str = "mesh.hello_responder_crashed_restarted";

const CRASH_LOOP_THRESHOLD_24H: u32 = 3;
const RATE_LIMITED_STORM_THRESHOLD_1H: u64 = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelloResponderRuntimeInput {
    pub mesh_enabled: bool,
    pub responder_disabled: bool,
    pub configured_port: u16,
    pub tailscale_ip: Option<IpAddr>,
    pub running: bool,
    pub port_in_use: bool,
    pub accepted_requests_1h: u64,
    pub denied_requests_1h: u64,
    pub rate_limited_requests_1h: u64,
    pub last_request_at: Option<String>,
    pub last_restart_at: Option<String>,
    pub crash_count_24h: u32,
}

impl HelloResponderRuntimeInput {
    #[must_use]
    pub fn new(mesh_enabled: bool) -> Self {
        Self {
            mesh_enabled,
            responder_disabled: false,
            configured_port: DEFAULT_HELLO_RESPONDER_PORT,
            tailscale_ip: None,
            running: false,
            port_in_use: false,
            accepted_requests_1h: 0,
            denied_requests_1h: 0,
            rate_limited_requests_1h: 0,
            last_request_at: None,
            last_restart_at: None,
            crash_count_24h: 0,
        }
    }

    #[must_use]
    pub fn listen_address(&self) -> Option<String> {
        self.tailscale_ip
            .map(|ip| SocketAddr::new(ip, self.configured_port).to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResponderDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

impl HelloResponderDegradation {
    #[must_use]
    pub fn not_running() -> Self {
        Self {
            code: HELLO_RESPONDER_NOT_RUNNING_CODE,
            severity: "medium",
            message: "Mesh is enabled but the hello responder lifecycle job is not running."
                .to_owned(),
            repair: "Run `ee mesh hello-responder run --help` and start the user-scoped foreground owner, or disable the responder with EE_MESH_HELLO_RESPONDER_DISABLED=1."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn port_in_use(port: u16) -> Self {
        Self {
            code: HELLO_RESPONDER_PORT_IN_USE_CODE,
            severity: "medium",
            message: format!("The hello responder could not bind port {port}."),
            repair: "Free the port or set EE_MESH_HELLO_PORT to an unused port.".to_owned(),
        }
    }

    #[must_use]
    pub fn no_tailscale_ip() -> Self {
        Self {
            code: HELLO_RESPONDER_NO_TAILSCALE_IP_CODE,
            severity: "medium",
            message:
                "Mesh is enabled but no self Tailscale IP is available for the hello responder."
                    .to_owned(),
            repair: "Authenticate Tailscale, then retry the user-scoped `ee mesh hello-responder run` owner."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn crash_loop(crash_count_24h: u32) -> Self {
        Self {
            code: HELLO_RESPONDER_CRASH_LOOP_CODE,
            severity: "high",
            message: format!(
                "The hello responder restarted {crash_count_24h} times in the last 24 hours."
            ),
            repair: "Inspect recent `mesh.hello_responder_crashed_restarted` audit rows before re-enabling mesh discovery."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn rate_limited_storm(rate_limited_requests_1h: u64) -> Self {
        Self {
            code: HELLO_RESPONDER_RATE_LIMITED_STORM_CODE,
            severity: "warning",
            message: format!(
                "The hello responder rate-limited {rate_limited_requests_1h} requests in the last hour."
            ),
            repair: "Inspect the requesting peers and discovery policy before raising rate limits."
                .to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResponderStatusReport {
    pub schema: &'static str,
    pub running: bool,
    pub listen_address: Option<String>,
    pub accepted_requests_1h: u64,
    pub denied_requests_1h: u64,
    pub rate_limited_requests_1h: u64,
    pub last_request_at: Option<String>,
    pub last_restart_at: Option<String>,
    pub crash_count_24h: u32,
    pub degraded: Vec<HelloResponderDegradation>,
}

impl HelloResponderStatusReport {
    #[must_use]
    pub fn from_runtime(input: &HelloResponderRuntimeInput) -> Self {
        let mut degraded = Vec::new();
        if input.mesh_enabled && !input.responder_disabled {
            if input.tailscale_ip.is_none() {
                degraded.push(HelloResponderDegradation::no_tailscale_ip());
            }
            if input.port_in_use {
                degraded.push(HelloResponderDegradation::port_in_use(
                    input.configured_port,
                ));
            }
            if !input.running {
                degraded.push(HelloResponderDegradation::not_running());
            }
        }
        if input.crash_count_24h >= CRASH_LOOP_THRESHOLD_24H {
            degraded.push(HelloResponderDegradation::crash_loop(input.crash_count_24h));
        }
        if input.rate_limited_requests_1h >= RATE_LIMITED_STORM_THRESHOLD_1H {
            degraded.push(HelloResponderDegradation::rate_limited_storm(
                input.rate_limited_requests_1h,
            ));
        }

        Self {
            schema: HELLO_RESPONDER_STATUS_SCHEMA_V1,
            running: input.running,
            listen_address: input.listen_address(),
            accepted_requests_1h: input.accepted_requests_1h,
            denied_requests_1h: input.denied_requests_1h,
            rate_limited_requests_1h: input.rate_limited_requests_1h,
            last_request_at: input.last_request_at.clone(),
            last_restart_at: input.last_restart_at.clone(),
            crash_count_24h: input.crash_count_24h,
            degraded,
        }
    }

    pub fn from_environment(mesh_enabled: bool) -> Result<Self, HelloResponderConfigError> {
        let mut input = HelloResponderRuntimeInput::new(mesh_enabled);
        if let Some(value) = read_env_var(EnvVar::MeshHelloResponderDisabled) {
            input.responder_disabled = parse_env_bool(EnvVar::MeshHelloResponderDisabled, &value)?;
        }
        if let Some(value) = read_env_var(EnvVar::MeshHelloPort) {
            input.configured_port = parse_port(&value)?;
        }
        Ok(Self::from_runtime(&input))
    }

    /// Overlay live owner posture from the same-EUID control channel.
    pub fn apply_live_owner(&mut self, running: bool, listen_address: Option<String>) {
        self.running = running;
        if listen_address.is_some() {
            self.listen_address = listen_address;
        }
        if self.running {
            self.degraded.retain(|item| {
                item.code != HELLO_RESPONDER_NOT_RUNNING_CODE
                    && (self.listen_address.is_none()
                        || item.code != HELLO_RESPONDER_NO_TAILSCALE_IP_CODE)
            });
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelloResponderConfigError {
    variable: &'static str,
    value: String,
    expected: &'static str,
}

impl HelloResponderConfigError {
    fn invalid(variable: EnvVar, value: &str, expected: &'static str) -> Self {
        Self {
            variable: variable.name(),
            value: value.to_owned(),
            expected,
        }
    }
}

impl fmt::Display for HelloResponderConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} has invalid value {:?}; expected {}",
            self.variable, self.value, self.expected
        )
    }
}

impl std::error::Error for HelloResponderConfigError {}

fn parse_env_bool(variable: EnvVar, value: &str) -> Result<bool, HelloResponderConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(HelloResponderConfigError::invalid(
            variable,
            value,
            "true/false, yes/no, on/off, or 1/0",
        )),
    }
}

fn parse_port(value: &str) -> Result<u16, HelloResponderConfigError> {
    let parsed = value.trim().parse::<u16>().map_err(|_| {
        HelloResponderConfigError::invalid(EnvVar::MeshHelloPort, value, "integer port 1..=65535")
    })?;
    if parsed == 0 {
        return Err(HelloResponderConfigError::invalid(
            EnvVar::MeshHelloPort,
            value,
            "integer port 1..=65535",
        ));
    }
    Ok(parsed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RateBucket {
    window_start_epoch_seconds: u64,
    request_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelloResponderRateLimiter {
    limit_per_peer: u32,
    window_seconds: u64,
    buckets: BTreeMap<String, RateBucket>,
}

impl Default for HelloResponderRateLimiter {
    fn default() -> Self {
        Self::new(
            DEFAULT_HELLO_RESPONDER_RATE_LIMIT_PER_PEER,
            DEFAULT_HELLO_RESPONDER_RATE_WINDOW_SECONDS,
        )
    }
}

impl HelloResponderRateLimiter {
    #[must_use]
    pub fn new(limit_per_peer: u32, window_seconds: u64) -> Self {
        Self {
            limit_per_peer,
            window_seconds: window_seconds.max(1),
            buckets: BTreeMap::new(),
        }
    }

    pub fn admit(
        &mut self,
        peer_node_key: &str,
        now_epoch_seconds: u64,
    ) -> HelloResponderAdmission {
        let bucket = self
            .buckets
            .entry(peer_node_key.to_owned())
            .or_insert(RateBucket {
                window_start_epoch_seconds: now_epoch_seconds,
                request_count: 0,
            });
        if now_epoch_seconds.saturating_sub(bucket.window_start_epoch_seconds)
            >= self.window_seconds
        {
            bucket.window_start_epoch_seconds = now_epoch_seconds;
            bucket.request_count = 0;
        }
        if bucket.request_count >= self.limit_per_peer {
            return HelloResponderAdmission::RateLimited {
                retry_after_seconds: self.window_seconds.saturating_sub(
                    now_epoch_seconds.saturating_sub(bucket.window_start_epoch_seconds),
                ),
            };
        }
        bucket.request_count = bucket.request_count.saturating_add(1);
        HelloResponderAdmission::Accepted {
            remaining_in_window: self.limit_per_peer.saturating_sub(bucket.request_count),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HelloResponderAdmission {
    Accepted { remaining_in_window: u32 },
    RateLimited { retry_after_seconds: u64 },
}

impl HelloResponderAdmission {
    #[must_use]
    pub const fn allowed(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelloResponderRequestDenial {
    pub code: &'static str,
    pub message: String,
}

pub fn validate_tailnet_header(
    expected_tailnet_id: &str,
    received_tailnet_id: Option<&str>,
) -> Result<(), HelloResponderRequestDenial> {
    match received_tailnet_id {
        Some(received) if received == expected_tailnet_id => Ok(()),
        Some(received) => Err(HelloResponderRequestDenial {
            code: "tailnet_mismatch",
            message: format!(
                "hello requester tailnet {received:?} does not match expected tailnet {expected_tailnet_id:?}"
            ),
        }),
        None => Err(HelloResponderRequestDenial {
            code: "tailnet_header_missing",
            message: "hello requester did not provide a tailnet identity header".to_owned(),
        }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelloResponderLifecycleEventKind {
    Started,
    Stopped,
    CrashedRestarted,
}

impl HelloResponderLifecycleEventKind {
    #[must_use]
    pub const fn audit_event_type(self) -> &'static str {
        match self {
            Self::Started => HELLO_RESPONDER_STARTED_EVENT,
            Self::Stopped => HELLO_RESPONDER_STOPPED_EVENT,
            Self::CrashedRestarted => HELLO_RESPONDER_CRASHED_RESTARTED_EVENT,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResponderLifecycleAudit {
    pub schema: &'static str,
    pub event_type: &'static str,
    pub listen_address: Option<String>,
    pub crash_count_24h: u32,
}

#[must_use]
pub fn lifecycle_audit(
    kind: HelloResponderLifecycleEventKind,
    status: &HelloResponderStatusReport,
) -> HelloResponderLifecycleAudit {
    HelloResponderLifecycleAudit {
        schema: HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1,
        event_type: kind.audit_event_type(),
        listen_address: status.listen_address.clone(),
        crash_count_24h: status.crash_count_24h,
    }
}

/// Persist a redaction-safe lifecycle row on the workspace audit chain.
pub fn persist_lifecycle_audit(
    connection: &DbConnection,
    workspace_id: &str,
    kind: HelloResponderLifecycleEventKind,
    status: &HelloResponderStatusReport,
) -> Result<String, DomainError> {
    let audit = lifecycle_audit(kind, status);
    let details = serde_json::to_string(&audit).map_err(|error| DomainError::Storage {
        message: format!("Failed to serialize hello-responder lifecycle audit: {error}"),
        repair: Some("Retry after the workspace store is writable.".to_owned()),
    })?;
    let audit_id = generate_audit_id();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some("ee-mesh-responder".to_owned()),
                action: audit.event_type.to_owned(),
                target_type: Some("mesh_hello_responder".to_owned()),
                target_id: Some(workspace_id.to_owned()),
                details: Some(details),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to persist hello-responder lifecycle audit: {error}"),
            repair: Some("Check that the workspace database is writable and retry.".to_owned()),
        })?;
    Ok(audit_id)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn status_reports_enabled_not_running_with_no_tailscale_ip() {
        let input = HelloResponderRuntimeInput::new(true);
        let report = HelloResponderStatusReport::from_runtime(&input);

        assert!(!report.running);
        assert_eq!(report.listen_address, None);
        assert!(
            report
                .degraded
                .iter()
                .any(|item| item.code == HELLO_RESPONDER_NO_TAILSCALE_IP_CODE)
        );
        assert!(
            report
                .degraded
                .iter()
                .any(|item| item.code == HELLO_RESPONDER_NOT_RUNNING_CODE)
        );
    }

    #[test]
    fn status_uses_configured_tailnet_bind_address() {
        let mut input = HelloResponderRuntimeInput::new(true);
        input.tailscale_ip = Some(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 8)));
        input.configured_port = 41889;
        input.running = true;

        let report = HelloResponderStatusReport::from_runtime(&input);

        assert_eq!(report.listen_address.as_deref(), Some("100.64.0.8:41889"));
        assert!(report.degraded.is_empty());
    }

    #[test]
    fn live_owner_overlay_clears_not_running_and_keeps_listen_address() {
        let report =
            HelloResponderStatusReport::from_runtime(&HelloResponderRuntimeInput::new(true));
        assert!(!report.running);
        let mut live = report;
        live.apply_live_owner(true, Some("100.64.0.8:41888".to_owned()));
        assert!(live.running);
        assert_eq!(live.listen_address.as_deref(), Some("100.64.0.8:41888"));
        assert!(
            live.degraded
                .iter()
                .all(|item| item.code != HELLO_RESPONDER_NOT_RUNNING_CODE
                    && item.code != HELLO_RESPONDER_NO_TAILSCALE_IP_CODE)
        );
    }

    #[test]
    fn disabled_responder_does_not_emit_not_running() {
        let mut input = HelloResponderRuntimeInput::new(true);
        input.responder_disabled = true;

        let report = HelloResponderStatusReport::from_runtime(&input);

        assert!(report.degraded.is_empty());
    }

    #[test]
    fn rate_limiter_allows_sixteen_requests_per_peer_per_window() {
        let mut limiter = HelloResponderRateLimiter::default();

        for remaining in (0..DEFAULT_HELLO_RESPONDER_RATE_LIMIT_PER_PEER).rev() {
            assert_eq!(
                limiter.admit("node-a", 10),
                HelloResponderAdmission::Accepted {
                    remaining_in_window: remaining
                }
            );
        }

        assert_eq!(
            limiter.admit("node-a", 10),
            HelloResponderAdmission::RateLimited {
                retry_after_seconds: DEFAULT_HELLO_RESPONDER_RATE_WINDOW_SECONDS
            }
        );
        assert!(limiter.admit("node-b", 10).allowed());
        assert!(limiter.admit("node-a", 70).allowed());
    }

    #[test]
    fn tailnet_header_mismatch_is_denied() {
        let denied = validate_tailnet_header("tailnet-1", Some("tailnet-2")).unwrap_err();
        assert_eq!(denied.code, "tailnet_mismatch");
    }

    #[test]
    fn lifecycle_audit_uses_required_event_type() {
        let status =
            HelloResponderStatusReport::from_runtime(&HelloResponderRuntimeInput::new(false));
        let audit = lifecycle_audit(HelloResponderLifecycleEventKind::CrashedRestarted, &status);

        assert_eq!(audit.schema, HELLO_RESPONDER_LIFECYCLE_AUDIT_SCHEMA_V1);
        assert_eq!(audit.event_type, HELLO_RESPONDER_CRASHED_RESTARTED_EVENT);
    }
}
