//! Accepted-side Unix responder broker for authenticated mesh sessions.
//!
//! This is the first bounded production slice of T2.2
//! (`bd-tc-epic-qzk7o.3.3`). It owns one real Asupersync TCP listener,
//! verifies the bind address and every kernel-observed peer through the
//! Tailscale LocalAPI, admits pre-authentication work under global/source
//! bounds, selects only an exact pre-registered route, opens existing key
//! storage without creating it, and hands the socket to T2.1's source-coupled
//! authenticated-session acceptor.
//!
//! This slice deliberately does not run application hello, anti-entropy, or
//! synchronization. Route registration/control-channel ownership, network-map
//! rebind supervision, durable audit persistence, and grant-target migration
//! remain later T2.2 slices.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::future::Future;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::net::Shutdown;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use asupersync::Cx;
#[cfg(unix)]
use asupersync::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use asupersync::net::TcpListener;
#[cfg(unix)]
use asupersync::net::unix::UnixStream;
#[cfg(unix)]
use asupersync::time::{BudgetTimeExt, timeout, wall_now};
use serde::{Deserialize, Serialize};

use crate::config::{EnvVar, parse_env_bool_flag, read_env_var};
use crate::mesh::bootstrap_envelope::BootstrapAdmission;
pub use crate::mesh::key_store::MESH_KEY_STORE_UNAVAILABLE_CODE;
use crate::mesh::key_store::{KeyStoreError, MeshKeyStore, PairKeyClass};
use crate::mesh::transport_session::{
    AcceptedSessionConfig, AcceptedSourceAttestation, AuthenticatedTransportSession,
    HandshakeObservations, ResolvedAcceptedRoute, ResponderExpectations, SessionCapabilities,
    SessionChannelError, SessionChannelLimits, UntrustedRouteSelectors,
    accept_authenticated_session_with,
};

pub const RESPONDER_BROKER_STATUS_SCHEMA_V1: &str = "ee.mesh.responder_broker.status.v1";
pub const RESPONDER_BROKER_AUDIT_SCHEMA_V1: &str = "ee.mesh.responder_broker.audit.v1";
pub const MESH_RESPONDER_ROUTE_UNAVAILABLE_CODE: &str = "mesh_responder_route_unavailable";
pub const MESH_BOOTSTRAP_IDENTITY_UNVERIFIED_CODE: &str = "mesh_bootstrap_identity_unverified";
pub const MESH_RESPONDER_PORT_CONFLICT_CODE: &str = "mesh_responder_port_conflict";

#[cfg(unix)]
const LOCAL_API_MAX_RESPONSE_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const LOCAL_API_MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_RECENT_BROKER_AUDIT_EVENTS: usize = 128;

pub type TailscaleLocalApiFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ResponderBrokerError>> + 'a>>;

/// Minimal verified local identity needed to validate one listener bind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTailscaleIdentity {
    pub stable_id: String,
    pub current_node_pubkey: String,
}

/// Minimal accepted-peer identity returned by LocalAPI WhoIs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhoIsIdentity {
    pub stable_id: String,
    pub current_node_pubkey: String,
}

/// Narrow conformance seam around the two read-only LocalAPI calls the broker
/// needs. Production uses [`TailscaleLocalApiClient`]; tests may host a fake
/// LocalAPI socket while exercising the same broker path.
pub trait TailscaleLocalApi: Send + Sync {
    fn verify_local_address<'a>(
        &'a self,
        cx: &'a Cx,
        address: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, LocalTailscaleIdentity>;

    fn who_is<'a>(
        &'a self,
        cx: &'a Cx,
        source: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, WhoIsIdentity>;
}

/// Real Tailscale LocalAPI client over tailscaled's Unix-domain socket.
#[derive(Clone, Debug)]
pub struct TailscaleLocalApiClient {
    socket_path: PathBuf,
    io_timeout: Duration,
}

impl TailscaleLocalApiClient {
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>, io_timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            io_timeout,
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    #[cfg(unix)]
    async fn request_json(&self, cx: &Cx, endpoint: &str) -> Result<Vec<u8>, ResponderBrokerError> {
        checkpoint(cx, "tailscale localapi")?;
        if self.io_timeout.is_zero()
            || !endpoint.starts_with("/localapi/v0/")
            || endpoint.bytes().any(|byte| byte == b'\r' || byte == b'\n')
        {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        let mut stream =
            await_local_api_io(cx, self.io_timeout, UnixStream::connect(&self.socket_path)).await?;
        let request = format!(
            "GET {endpoint} HTTP/1.1\r\nHost: local-tailscaled.sock\r\nConnection: close\r\n\r\n"
        );
        await_local_api_io(
            cx,
            self.io_timeout,
            AsyncWriteExt::write_all(&mut stream, request.as_bytes()),
        )
        .await?;
        await_local_api_io(cx, self.io_timeout, AsyncWriteExt::shutdown(&mut stream)).await?;

        let mut response = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = await_local_api_io(
                cx,
                self.io_timeout,
                AsyncReadExt::read(&mut stream, &mut chunk),
            )
            .await?;
            if read == 0 {
                break;
            }
            if response.len().saturating_add(read) > LOCAL_API_MAX_RESPONSE_BYTES {
                return Err(ResponderBrokerError::WhoIsUnverified);
            }
            response.extend_from_slice(&chunk[..read]);
        }
        parse_local_api_response(&response)
    }
}

impl TailscaleLocalApi for TailscaleLocalApiClient {
    fn verify_local_address<'a>(
        &'a self,
        cx: &'a Cx,
        address: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, LocalTailscaleIdentity> {
        Box::pin(async move {
            #[cfg(not(unix))]
            {
                let _ = (cx, address, &self.socket_path, self.io_timeout);
                return Err(ResponderBrokerError::PlatformUnsupported);
            }
            #[cfg(unix)]
            {
                let body = self.request_json(cx, "/localapi/v0/status").await?;
                let status: LocalApiStatus = serde_json::from_slice(&body)
                    .map_err(|_| ResponderBrokerError::WhoIsUnverified)?;
                let local = status.local.ok_or(ResponderBrokerError::WhoIsUnverified)?;
                let address_verified = local
                    .tailscale_ips
                    .iter()
                    .filter_map(|value| value.parse::<IpAddr>().ok())
                    .any(|ip| ip == address.ip());
                if !address_verified
                    || !valid_identity(&local.stable_id)
                    || !valid_node_key(&local.current_node_pubkey)
                {
                    return Err(ResponderBrokerError::WhoIsUnverified);
                }
                Ok(LocalTailscaleIdentity {
                    stable_id: local.stable_id,
                    current_node_pubkey: local.current_node_pubkey,
                })
            }
        })
    }

    fn who_is<'a>(
        &'a self,
        cx: &'a Cx,
        source: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, WhoIsIdentity> {
        Box::pin(async move {
            #[cfg(not(unix))]
            {
                let _ = (cx, source, &self.socket_path, self.io_timeout);
                return Err(ResponderBrokerError::PlatformUnsupported);
            }
            #[cfg(unix)]
            {
                if source.ip().is_unspecified() {
                    return Err(ResponderBrokerError::WhoIsUnverified);
                }
                let encoded_source = percent_encode_query(&source.to_string());
                let endpoint = format!("/localapi/v0/whois?addr={encoded_source}&proto=tcp");
                let body = self.request_json(cx, &endpoint).await?;
                let response: LocalApiWhoIsResponse = serde_json::from_slice(&body)
                    .map_err(|_| ResponderBrokerError::WhoIsUnverified)?;
                let node = response.node.ok_or(ResponderBrokerError::WhoIsUnverified)?;
                let source_matches_node = node
                    .addresses
                    .iter()
                    .filter_map(|prefix| prefix.split('/').next())
                    .filter_map(|ip| ip.parse::<IpAddr>().ok())
                    .any(|ip| ip == source.ip());
                if !source_matches_node
                    || !valid_identity(&node.stable_id)
                    || !valid_node_key(&node.current_node_pubkey)
                {
                    return Err(ResponderBrokerError::WhoIsUnverified);
                }
                Ok(WhoIsIdentity {
                    stable_id: node.stable_id,
                    current_node_pubkey: node.current_node_pubkey,
                })
            }
        })
    }
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
struct LocalApiStatus {
    #[serde(rename = "Self")]
    local: Option<LocalApiStatusNode>,
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
struct LocalApiStatusNode {
    #[serde(rename = "ID")]
    stable_id: String,
    #[serde(rename = "PublicKey")]
    current_node_pubkey: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
struct LocalApiWhoIsResponse {
    #[serde(rename = "Node")]
    node: Option<LocalApiWhoIsNode>,
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
struct LocalApiWhoIsNode {
    #[serde(rename = "StableID")]
    stable_id: String,
    #[serde(rename = "Key")]
    current_node_pubkey: String,
    #[serde(rename = "Addresses", default)]
    addresses: Vec<String>,
}

/// One locally registered responder route. No network message can introduce
/// or modify the workspace path or peer handle.
#[derive(Clone, Debug)]
pub struct RegisteredResponderRoute {
    pub workspace_path: PathBuf,
    pub peer_handle: String,
    pub committed_port: u16,
    pub expectations: ResponderExpectations,
    pub responder_node_pubkey: String,
    pub capabilities: SessionCapabilities,
    pub limits: SessionChannelLimits,
}

impl RegisteredResponderRoute {
    fn validate(&self) -> Result<(), ResponderBrokerError> {
        if !self.workspace_path.is_absolute()
            || self.peer_handle.trim().is_empty()
            || self.peer_handle.len() > 256
            || self.committed_port < 1024
            || !valid_identity(&self.expectations.team_id)
            || !valid_identity(&self.expectations.tailnet_id)
            || !valid_identity(&self.expectations.responder_workspace_id)
            || !valid_identity(&self.expectations.responder_stable_id)
            || !valid_identity(&self.expectations.initiator_stable_id)
            || !valid_node_key(&self.responder_node_pubkey)
            || self.expectations.pair_key_generation == 0
        {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        Ok(())
    }
}

/// Exact in-memory route table supplied by the local owner. Construction
/// rejects duplicates and mixed listener identity/port posture.
#[derive(Clone, Debug)]
pub struct ResponderRouteRegistry {
    routes: BTreeMap<RouteKey, RegisteredResponderRoute>,
    committed_port: u16,
    tailnet_id: String,
    responder_stable_id: String,
    responder_node_pubkey: String,
    limits: SessionChannelLimits,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RouteKey {
    team_id: String,
    target_workspace_id: String,
    pair_key_generation: u64,
}

impl ResponderRouteRegistry {
    pub fn new(
        routes: impl IntoIterator<Item = RegisteredResponderRoute>,
    ) -> Result<Self, ResponderBrokerError> {
        let mut iter = routes.into_iter();
        let first = iter
            .next()
            .ok_or(ResponderBrokerError::InvalidConfiguration)?;
        first.validate()?;
        let committed_port = first.committed_port;
        let tailnet_id = first.expectations.tailnet_id.clone();
        let responder_stable_id = first.expectations.responder_stable_id.clone();
        let responder_node_pubkey = first.responder_node_pubkey.clone();
        let limits = first.limits;
        let mut registry = Self {
            routes: BTreeMap::new(),
            committed_port,
            tailnet_id,
            responder_stable_id,
            responder_node_pubkey,
            limits,
        };
        registry.insert_checked(first)?;
        for route in iter {
            route.validate()?;
            if route.committed_port != registry.committed_port
                || route.expectations.tailnet_id != registry.tailnet_id
                || route.expectations.responder_stable_id != registry.responder_stable_id
                || route.responder_node_pubkey != registry.responder_node_pubkey
                || route.limits != registry.limits
            {
                return Err(ResponderBrokerError::InvalidConfiguration);
            }
            registry.insert_checked(route)?;
        }
        Ok(registry)
    }

    fn insert_checked(
        &mut self,
        route: RegisteredResponderRoute,
    ) -> Result<(), ResponderBrokerError> {
        let key = RouteKey {
            team_id: route.expectations.team_id.clone(),
            target_workspace_id: route.expectations.responder_workspace_id.clone(),
            pair_key_generation: route.expectations.pair_key_generation,
        };
        if self.routes.insert(key, route).is_some() {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        Ok(())
    }

    fn resolve(&self, selectors: &UntrustedRouteSelectors) -> Option<&RegisteredResponderRoute> {
        self.routes.get(&RouteKey {
            team_id: selectors.team_id.clone(),
            target_workspace_id: selectors.responder_workspace_id.clone(),
            pair_key_generation: selectors.pair_key_generation,
        })
    }

    #[must_use]
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

/// Pre-authentication concurrency and fixed-window rate bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreAuthAdmissionLimits {
    pub max_global_inflight: usize,
    pub max_source_inflight: usize,
    pub window_ms: u64,
    pub max_source_per_window: u32,
    pub max_global_per_window: u32,
    pub max_tracked_sources: usize,
}

impl Default for PreAuthAdmissionLimits {
    fn default() -> Self {
        Self {
            max_global_inflight: 64,
            max_source_inflight: 8,
            window_ms: 60_000,
            max_source_per_window: 8,
            max_global_per_window: 64,
            max_tracked_sources: 1024,
        }
    }
}

impl PreAuthAdmissionLimits {
    fn validate(self) -> Result<Self, ResponderBrokerError> {
        if self.max_global_inflight == 0
            || self.max_source_inflight == 0
            || self.max_source_inflight > self.max_global_inflight
            || self.window_ms == 0
            || self.max_source_per_window == 0
            || self.max_global_per_window == 0
            || self.max_tracked_sources == 0
        {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        Ok(self)
    }
}

#[derive(Debug)]
struct AdmissionState {
    limits: PreAuthAdmissionLimits,
    inflight_global: usize,
    inflight_by_source: BTreeMap<IpAddr, usize>,
    rate: BootstrapAdmission,
}

#[derive(Debug)]
struct PreAuthPermit {
    state: Arc<Mutex<AdmissionState>>,
    source: IpAddr,
}

impl Drop for PreAuthPermit {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.inflight_global = state.inflight_global.saturating_sub(1);
        if let Some(count) = state.inflight_by_source.get_mut(&self.source) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.inflight_by_source.remove(&self.source);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponderBrokerState {
    Listening,
    Shutdown,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponderBrokerStatus {
    pub schema: &'static str,
    pub state: ResponderBrokerState,
    pub bound_address: Option<String>,
    pub registered_routes: usize,
    pub preauth_inflight: usize,
    pub accepted_connections: u64,
    pub authenticated_sessions: u64,
    pub rejected_connections: u64,
    pub last_error_code: Option<&'static str>,
    pub application_hello_performed: bool,
    pub anti_entropy_performed: bool,
    pub synchronized: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponderBrokerAuditEvent {
    pub schema: &'static str,
    pub action: &'static str,
    pub outcome: &'static str,
    pub code: Option<&'static str>,
    pub route_selected: bool,
    pub authenticated: bool,
}

#[derive(Debug)]
struct RuntimeStatus {
    state: ResponderBrokerState,
    accepted_connections: u64,
    authenticated_sessions: u64,
    rejected_connections: u64,
    last_error_code: Option<&'static str>,
    recent_audit: VecDeque<ResponderBrokerAuditEvent>,
}

impl RuntimeStatus {
    fn listening() -> Self {
        Self {
            state: ResponderBrokerState::Listening,
            accepted_connections: 0,
            authenticated_sessions: 0,
            rejected_connections: 0,
            last_error_code: None,
            recent_audit: VecDeque::new(),
        }
    }

    fn record(
        &mut self,
        outcome: &'static str,
        code: Option<&'static str>,
        route_selected: bool,
        authenticated: bool,
        rejected_connection: bool,
    ) {
        if authenticated {
            self.authenticated_sessions = self.authenticated_sessions.saturating_add(1);
        } else if rejected_connection {
            self.rejected_connections = self.rejected_connections.saturating_add(1);
        }
        if code.is_some() {
            self.last_error_code = code;
        }
        if self.recent_audit.len() == MAX_RECENT_BROKER_AUDIT_EVENTS {
            self.recent_audit.pop_front();
        }
        self.recent_audit.push_back(ResponderBrokerAuditEvent {
            schema: RESPONDER_BROKER_AUDIT_SCHEMA_V1,
            action: "mesh.responder.accept",
            outcome,
            code,
            route_selected,
            authenticated,
        });
    }
}

/// Structured, metadata-safe accepted-side error surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponderBrokerError {
    PlatformUnsupported,
    InvalidConfiguration,
    PortConflict,
    TransportUnavailable,
    Cancelled,
    AdmissionLimited,
    WhoIsUnavailable,
    WhoIsUnverified,
    RouteUnavailable,
    KeyStoreUnavailable,
    PairingRequired,
    Session(SessionChannelError),
}

impl ResponderBrokerError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PortConflict => MESH_RESPONDER_PORT_CONFLICT_CODE,
            Self::WhoIsUnavailable | Self::WhoIsUnverified => {
                MESH_BOOTSTRAP_IDENTITY_UNVERIFIED_CODE
            }
            Self::RouteUnavailable => MESH_RESPONDER_ROUTE_UNAVAILABLE_CODE,
            Self::KeyStoreUnavailable => MESH_KEY_STORE_UNAVAILABLE_CODE,
            Self::PairingRequired => "mesh_frame_auth_failed",
            Self::Session(error) => error.degraded_code(),
            Self::PlatformUnsupported
            | Self::InvalidConfiguration
            | Self::TransportUnavailable
            | Self::Cancelled
            | Self::AdmissionLimited => "mesh_transport_unreachable",
        }
    }

    #[must_use]
    pub fn severity(&self) -> &'static str {
        match self {
            Self::WhoIsUnavailable
            | Self::WhoIsUnverified
            | Self::RouteUnavailable
            | Self::KeyStoreUnavailable
            | Self::PairingRequired => "high",
            Self::Session(error)
                if matches!(
                    error.degraded_code(),
                    "mesh_frame_auth_failed"
                        | "mesh_frame_target_mismatch"
                        | "mesh_frame_replay_rejected"
                ) =>
            {
                "high"
            }
            Self::PortConflict => "high",
            Self::PlatformUnsupported
            | Self::InvalidConfiguration
            | Self::TransportUnavailable
            | Self::Cancelled
            | Self::AdmissionLimited
            | Self::Session(_) => "warning",
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::PlatformUnsupported => {
                "Inbound mesh responder transport is unsupported on this platform".to_owned()
            }
            Self::InvalidConfiguration => {
                "Mesh responder refused invalid local listener or route configuration".to_owned()
            }
            Self::PortConflict => {
                "Mesh responder could not acquire the committed listener port".to_owned()
            }
            Self::TransportUnavailable => {
                "Mesh responder transport is unavailable on the verified local address".to_owned()
            }
            Self::Cancelled => "Mesh responder accept operation was cancelled".to_owned(),
            Self::AdmissionLimited => {
                "Mesh responder declined pre-authentication work at its bounded admission limit"
                    .to_owned()
            }
            Self::WhoIsUnavailable => {
                "Mesh responder could not verify the accepted source identity through Tailscale WhoIs"
                    .to_owned()
            }
            Self::WhoIsUnverified => {
                "Mesh responder could not verify the accepted source identity through Tailscale WhoIs"
                    .to_owned()
            }
            Self::RouteUnavailable => {
                "Mesh responder has no exact validated route for the authenticated target"
                    .to_owned()
            }
            Self::KeyStoreUnavailable => {
                "Mesh responder could not open the existing hardened pair-key store".to_owned()
            }
            Self::PairingRequired => {
                "Mesh responder route is not paired for authenticated transport".to_owned()
            }
            Self::Session(error) => error.message(),
        }
    }

    #[must_use]
    pub const fn repair(&self) -> &'static str {
        match self {
            Self::PlatformUnsupported => {
                "Use a Unix responder host; Windows remains client-only and fail-closed in v1."
            }
            Self::PortConflict => {
                "Stop the conflicting responder owner or align the user-scoped broker on the committed port."
            }
            Self::WhoIsUnavailable | Self::WhoIsUnverified => {
                "Restore the local Tailscale daemon and verify the peer remains enrolled on the expected tailnet."
            }
            Self::RouteUnavailable | Self::InvalidConfiguration => {
                "Re-register the exact local team/workspace route with the user-scoped responder owner."
            }
            Self::KeyStoreUnavailable => {
                "Repair owner-only mesh key-store permissions or re-pair without creating inbound credentials."
            }
            Self::PairingRequired => {
                "Pair this enrolled peer before retrying authenticated mesh transport."
            }
            Self::TransportUnavailable
            | Self::Cancelled
            | Self::AdmissionLimited
            | Self::Session(_) => {
                "Verify the enrolled peer endpoint and retry under a live bounded mesh operation."
            }
        }
    }
}

impl fmt::Display for ResponderBrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for ResponderBrokerError {}

/// Single listener owner for the bounded accepted-side production path.
pub struct ResponderBroker<A> {
    #[cfg(unix)]
    listener: Option<TcpListener>,
    local_api: A,
    routes: Arc<ResponderRouteRegistry>,
    admission: Arc<Mutex<AdmissionState>>,
    runtime: Arc<Mutex<RuntimeStatus>>,
    bound_address: SocketAddr,
    started_at: Instant,
}

impl<A: TailscaleLocalApi> ResponderBroker<A> {
    pub async fn bind(
        cx: &Cx,
        address: SocketAddr,
        local_api: A,
        routes: ResponderRouteRegistry,
        admission_limits: PreAuthAdmissionLimits,
    ) -> Result<Self, ResponderBrokerError> {
        #[cfg(not(unix))]
        {
            let _ = (cx, address, local_api, routes, admission_limits);
            return Err(ResponderBrokerError::PlatformUnsupported);
        }
        #[cfg(unix)]
        {
            checkpoint(cx, "responder bind")?;
            refuse_if_transport_disabled()?;
            let admission_limits = admission_limits.validate()?;
            if address.ip().is_unspecified()
                || address.port() < 1024
                || address.port() != routes.committed_port
            {
                return Err(ResponderBrokerError::InvalidConfiguration);
            }
            let local_identity = local_api.verify_local_address(cx, address).await?;
            if local_identity.stable_id != routes.responder_stable_id
                || local_identity.current_node_pubkey != routes.responder_node_pubkey
            {
                return Err(ResponderBrokerError::WhoIsUnverified);
            }
            let _ambient = Cx::set_current(Some(cx.clone()));
            let listener = TcpListener::bind(address).await.map_err(|error| {
                if error.kind() == io::ErrorKind::AddrInUse {
                    ResponderBrokerError::PortConflict
                } else {
                    ResponderBrokerError::TransportUnavailable
                }
            })?;
            let bound_address = listener
                .local_addr()
                .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
            Ok(Self {
                listener: Some(listener),
                local_api,
                routes: Arc::new(routes),
                admission: Arc::new(Mutex::new(AdmissionState {
                    limits: admission_limits,
                    inflight_global: 0,
                    inflight_by_source: BTreeMap::new(),
                    rate: BootstrapAdmission::with_limits(
                        admission_limits.window_ms,
                        admission_limits.max_source_per_window,
                        admission_limits.max_global_per_window,
                        admission_limits.max_tracked_sources,
                    ),
                })),
                runtime: Arc::new(Mutex::new(RuntimeStatus::listening())),
                bound_address,
                started_at: Instant::now(),
            })
        }
    }

    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.bound_address
    }

    /// Accept and authenticate exactly one session. The kernel source is
    /// admitted before reading `session_open`, then passed unchanged to the
    /// LocalAPI WhoIs query and T2.1's source-coupling check.
    pub async fn accept_authenticated(
        &self,
        cx: &Cx,
    ) -> Result<AuthenticatedTransportSession, ResponderBrokerError> {
        #[cfg(not(unix))]
        {
            let _ = cx;
            return Err(ResponderBrokerError::PlatformUnsupported);
        }
        #[cfg(unix)]
        {
            checkpoint(cx, "responder accept")?;
            let listener = self
                .listener
                .as_ref()
                .ok_or(ResponderBrokerError::TransportUnavailable)?;
            let _ambient = Cx::set_current(Some(cx.clone()));
            let (stream, kernel_source) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    let broker_error = if error.kind() == io::ErrorKind::Interrupted {
                        ResponderBrokerError::Cancelled
                    } else {
                        ResponderBrokerError::TransportUnavailable
                    };
                    self.record_error(&broker_error, false, false);
                    return Err(broker_error);
                }
            };
            {
                let mut runtime = self
                    .runtime
                    .lock()
                    .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
                runtime.accepted_connections = runtime.accepted_connections.saturating_add(1);
            }
            let permit = match self.admit(kernel_source.ip()) {
                Ok(permit) => permit,
                Err(error) => {
                    let _ = stream.shutdown(Shutdown::Both);
                    self.record_error(&error, false, true);
                    return Err(error);
                }
            };
            let routes = Arc::clone(&self.routes);
            let local_api = &self.local_api;
            let resolution_error = Arc::new(Mutex::new(None));
            let error_slot = Arc::clone(&resolution_error);
            let route_selected = Arc::new(AtomicBool::new(false));
            let selected_slot = Arc::clone(&route_selected);
            let limits = self.routes.limits;
            let accepted = accept_authenticated_session_with(
                cx,
                stream,
                limits,
                move |route_cx, observed_source, selectors| {
                    let error_slot = Arc::clone(&error_slot);
                    async move {
                        match resolve_route(
                            &route_cx,
                            local_api,
                            &routes,
                            observed_source,
                            &selectors,
                            permit,
                            &selected_slot,
                        )
                        .await
                        {
                            Ok(route) => Ok(route),
                            Err(error) => {
                                if let Ok(mut slot) = error_slot.lock() {
                                    *slot = Some(error);
                                }
                                Err(SessionChannelError::Authentication {
                                    message: "accepted responder route could not be verified"
                                        .to_owned(),
                                })
                            }
                        }
                    }
                },
            )
            .await;
            match accepted {
                Ok(session) => {
                    self.record_success();
                    Ok(session)
                }
                Err(session_error) => {
                    let broker_error = resolution_error
                        .lock()
                        .ok()
                        .and_then(|mut slot| slot.take())
                        .unwrap_or_else(|| ResponderBrokerError::Session(session_error));
                    self.record_error(&broker_error, route_selected.load(Ordering::Acquire), true);
                    Err(broker_error)
                }
            }
        }
    }

    fn admit(&self, source: IpAddr) -> Result<PreAuthPermit, ResponderBrokerError> {
        let mut state = self
            .admission
            .lock()
            .map_err(|_| ResponderBrokerError::AdmissionLimited)?;
        if state.inflight_global >= state.limits.max_global_inflight {
            return Err(ResponderBrokerError::AdmissionLimited);
        }
        let source_inflight = state.inflight_by_source.get(&source).copied().unwrap_or(0);
        if source_inflight >= state.limits.max_source_inflight {
            return Err(ResponderBrokerError::AdmissionLimited);
        }
        if source_inflight == 0
            && state.inflight_by_source.len() >= state.limits.max_tracked_sources
        {
            return Err(ResponderBrokerError::AdmissionLimited);
        }
        let now_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        state
            .rate
            .admit(&source.to_string(), now_ms)
            .map_err(|_| ResponderBrokerError::AdmissionLimited)?;
        state.inflight_global = state.inflight_global.saturating_add(1);
        state
            .inflight_by_source
            .entry(source)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        drop(state);
        Ok(PreAuthPermit {
            state: Arc::clone(&self.admission),
            source,
        })
    }

    fn record_success(&self) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.record("accepted", None, true, true, false);
        }
    }

    fn record_error(
        &self,
        error: &ResponderBrokerError,
        route_selected: bool,
        rejected_connection: bool,
    ) {
        if let Ok(mut runtime) = self.runtime.lock() {
            let outcome = if matches!(error, ResponderBrokerError::Cancelled) {
                "cancelled"
            } else {
                "rejected"
            };
            runtime.record(
                outcome,
                Some(error.code()),
                route_selected,
                false,
                rejected_connection,
            );
        }
    }

    #[must_use]
    pub fn status(&self) -> ResponderBrokerStatus {
        let (state, accepted, authenticated, rejected, last_error) = self
            .runtime
            .lock()
            .map(|runtime| {
                (
                    runtime.state,
                    runtime.accepted_connections,
                    runtime.authenticated_sessions,
                    runtime.rejected_connections,
                    runtime.last_error_code,
                )
            })
            .unwrap_or((
                ResponderBrokerState::Shutdown,
                0,
                0,
                0,
                Some("mesh_transport_unreachable"),
            ));
        let preauth_inflight = self
            .admission
            .lock()
            .map(|admission| admission.inflight_global)
            .unwrap_or(0);
        ResponderBrokerStatus {
            schema: RESPONDER_BROKER_STATUS_SCHEMA_V1,
            state,
            bound_address: (state == ResponderBrokerState::Listening)
                .then(|| self.bound_address.to_string()),
            registered_routes: self.routes.route_count(),
            preauth_inflight,
            accepted_connections: accepted,
            authenticated_sessions: authenticated,
            rejected_connections: rejected,
            last_error_code: last_error,
            application_hello_performed: false,
            anti_entropy_performed: false,
            synchronized: false,
        }
    }

    #[must_use]
    pub fn recent_audit_events(&self) -> Vec<ResponderBrokerAuditEvent> {
        self.runtime
            .lock()
            .map(|runtime| runtime.recent_audit.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Stop future accepts and drop the listener. An in-flight accept is
    /// stopped by cancelling its `Cx`; callers then invoke this after join.
    pub fn shutdown(&mut self) {
        #[cfg(unix)]
        {
            self.listener.take();
        }
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.state = if cfg!(unix) {
                ResponderBrokerState::Shutdown
            } else {
                ResponderBrokerState::Unsupported
            };
        }
    }
}

async fn resolve_route<A: TailscaleLocalApi>(
    cx: &Cx,
    local_api: &A,
    routes: &ResponderRouteRegistry,
    source: SocketAddr,
    selectors: &UntrustedRouteSelectors,
    permit: PreAuthPermit,
    route_selected: &AtomicBool,
) -> Result<ResolvedAcceptedRoute<PreAuthPermit>, ResponderBrokerError> {
    let route = routes
        .resolve(selectors)
        .ok_or(ResponderBrokerError::RouteUnavailable)?;
    route_selected.store(true, Ordering::Release);
    let who_is = local_api.who_is(cx, source).await?;
    if who_is.stable_id != route.expectations.initiator_stable_id {
        return Err(ResponderBrokerError::WhoIsUnverified);
    }
    let store = MeshKeyStore::open_existing(&route.workspace_path)
        .map_err(map_key_store_error)?
        .ok_or(ResponderBrokerError::KeyStoreUnavailable)?;
    let pair_record = store
        .load_pair_key(&route.peer_handle, PairKeyClass::Current)
        .map_err(map_key_store_error)?
        .ok_or(ResponderBrokerError::PairingRequired)?;
    let source_attestation = AcceptedSourceAttestation::from_local_whois(
        source.ip(),
        route.expectations.tailnet_id.clone(),
        who_is.stable_id,
        who_is.current_node_pubkey.clone(),
    )
    .map_err(ResponderBrokerError::Session)?;
    Ok(ResolvedAcceptedRoute::new(
        AcceptedSessionConfig {
            expectations: route.expectations.clone(),
            pair_key: pair_record.key,
            observations: HandshakeObservations {
                initiator_node_pubkey: who_is.current_node_pubkey,
                responder_node_pubkey: route.responder_node_pubkey.clone(),
            },
            capabilities: route.capabilities.clone(),
            limits: route.limits,
        },
        source_attestation,
        permit,
    ))
}

fn map_key_store_error(_error: KeyStoreError) -> ResponderBrokerError {
    ResponderBrokerError::KeyStoreUnavailable
}

fn valid_identity(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 256
        && trimmed == value
        && !value.chars().any(char::is_control)
}

fn valid_node_key(value: &str) -> bool {
    valid_identity(value)
        && value
            .strip_prefix("nodekey:")
            .is_some_and(|key| !key.is_empty() && !key.chars().any(char::is_whitespace))
}

#[cfg(unix)]
fn percent_encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(&mut encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(unix)]
fn parse_local_api_response(response: &[u8]) -> Result<Vec<u8>, ResponderBrokerError> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(ResponderBrokerError::WhoIsUnverified)?;
    if header_end > LOCAL_API_MAX_HEADER_BYTES {
        return Err(ResponderBrokerError::WhoIsUnverified);
    }
    let header = std::str::from_utf8(&response[..header_end])
        .map_err(|_| ResponderBrokerError::WhoIsUnverified)?;
    let mut lines = header.split("\r\n");
    let status = lines.next().ok_or(ResponderBrokerError::WhoIsUnverified)?;
    if !status.starts_with("HTTP/1.1 200 ") && status != "HTTP/1.1 200" {
        return Err(ResponderBrokerError::WhoIsUnavailable);
    }
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(ResponderBrokerError::WhoIsUnverified);
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(ResponderBrokerError::WhoIsUnverified);
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| ResponderBrokerError::WhoIsUnverified)?;
            if content_length.replace(parsed).is_some() {
                return Err(ResponderBrokerError::WhoIsUnverified);
            }
        }
    }
    let body = &response[header_end + 4..];
    if content_length.is_some_and(|expected| expected != body.len()) {
        return Err(ResponderBrokerError::WhoIsUnverified);
    }
    Ok(body.to_vec())
}

#[cfg(unix)]
async fn await_local_api_io<T, F>(
    cx: &Cx,
    duration: Duration,
    future: F,
) -> Result<T, ResponderBrokerError>
where
    F: Future<Output = io::Result<T>>,
{
    checkpoint(cx, "tailscale localapi")?;
    let now = wall_now();
    let effective = cx
        .budget()
        .remaining_duration(now)
        .map_or(duration, |remaining| remaining.min(duration));
    if effective.is_zero() {
        return Err(ResponderBrokerError::Cancelled);
    }
    let _ambient = Cx::set_current(Some(cx.clone()));
    match timeout(now, effective, future).await {
        Ok(Ok(value)) => {
            checkpoint(cx, "tailscale localapi")?;
            Ok(value)
        }
        Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {
            Err(ResponderBrokerError::Cancelled)
        }
        Ok(Err(_)) => Err(ResponderBrokerError::WhoIsUnavailable),
        Err(_) => Err(ResponderBrokerError::WhoIsUnavailable),
    }
}

fn checkpoint(cx: &Cx, _phase: &'static str) -> Result<(), ResponderBrokerError> {
    cx.checkpoint().map_err(|_| ResponderBrokerError::Cancelled)
}

fn refuse_if_transport_disabled() -> Result<(), ResponderBrokerError> {
    let Some(raw) = read_env_var(EnvVar::MeshTransportDisabled) else {
        return Ok(());
    };
    match parse_env_bool_flag(&raw) {
        Some(true) => Err(ResponderBrokerError::Session(
            SessionChannelError::TransportDisabled,
        )),
        Some(false) => Ok(()),
        None => Err(ResponderBrokerError::Session(
            SessionChannelError::InvalidConfiguration {
                variable: EnvVar::MeshTransportDisabled.name(),
            },
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(path: PathBuf, port: u16) -> RegisteredResponderRoute {
        RegisteredResponderRoute {
            workspace_path: path,
            peer_handle: "peer_0123456789abcdef0123456789abcdef".to_owned(),
            committed_port: port,
            expectations: ResponderExpectations {
                team_id: "team-a".to_owned(),
                tailnet_id: "tailnet-a".to_owned(),
                responder_node_id: "node-responder".to_owned(),
                responder_workspace_id: "workspace-responder".to_owned(),
                responder_stable_id: "stable-responder".to_owned(),
                initiator_node_id: "node-initiator".to_owned(),
                initiator_stable_id: "stable-initiator".to_owned(),
                pair_key_generation: 1,
            },
            responder_node_pubkey: "nodekey:responder-current".to_owned(),
            capabilities: SessionCapabilities::base(),
            limits: SessionChannelLimits::default(),
        }
    }

    #[test]
    fn registry_rejects_duplicate_and_mixed_listener_routes() {
        let path = PathBuf::from("/tmp/ee-responder-broker-unit");
        let first = route(path.clone(), 41888);
        let duplicate = first.clone();
        assert!(matches!(
            ResponderRouteRegistry::new([first, duplicate]),
            Err(ResponderBrokerError::InvalidConfiguration)
        ));

        let first = route(path.clone(), 41888);
        let mut mixed = route(path, 41889);
        mixed.expectations.team_id = "team-b".to_owned();
        assert!(matches!(
            ResponderRouteRegistry::new([first, mixed]),
            Err(ResponderBrokerError::InvalidConfiguration)
        ));

        let mut empty_node_key = route(PathBuf::from("/tmp/ee-responder-broker-unit"), 41888);
        empty_node_key.responder_node_pubkey = "nodekey:".to_owned();
        assert!(matches!(
            ResponderRouteRegistry::new([empty_node_key]),
            Err(ResponderBrokerError::InvalidConfiguration)
        ));
    }

    #[test]
    fn errors_are_structured_and_do_not_echo_paths_or_routes() {
        for (error, expected_code, expected_severity) in [
            (
                ResponderBrokerError::WhoIsUnverified,
                MESH_BOOTSTRAP_IDENTITY_UNVERIFIED_CODE,
                "high",
            ),
            (
                ResponderBrokerError::RouteUnavailable,
                MESH_RESPONDER_ROUTE_UNAVAILABLE_CODE,
                "high",
            ),
            (
                ResponderBrokerError::KeyStoreUnavailable,
                MESH_KEY_STORE_UNAVAILABLE_CODE,
                "high",
            ),
            (
                ResponderBrokerError::PortConflict,
                MESH_RESPONDER_PORT_CONFLICT_CODE,
                "high",
            ),
            (
                ResponderBrokerError::PlatformUnsupported,
                "mesh_transport_unreachable",
                "warning",
            ),
        ] {
            let message = error.message();
            assert!(!message.contains('/'));
            assert!(!message.contains("team-a"));
            assert!(!message.contains("workspace-responder"));
            assert!(!error.repair().is_empty());
            assert_eq!(error.code(), expected_code);
            assert_eq!(error.severity(), expected_severity);
        }
    }

    #[test]
    fn status_never_claims_unperformed_protocol_work() {
        let status = ResponderBrokerStatus {
            schema: RESPONDER_BROKER_STATUS_SCHEMA_V1,
            state: ResponderBrokerState::Shutdown,
            bound_address: None,
            registered_routes: 1,
            preauth_inflight: 0,
            accepted_connections: 1,
            authenticated_sessions: 1,
            rejected_connections: 0,
            last_error_code: None,
            application_hello_performed: false,
            anti_entropy_performed: false,
            synchronized: false,
        };
        let value = serde_json::to_value(status).expect("serialize status");
        assert_eq!(value["applicationHelloPerformed"], false);
        assert_eq!(value["antiEntropyPerformed"], false);
        assert_eq!(value["synchronized"], false);
    }
}
