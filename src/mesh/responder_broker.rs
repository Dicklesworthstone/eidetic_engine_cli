//! Accepted-side responder broker for authenticated mesh sessions.
//!
//! This T2.2 production path (`bd-tc-epic-qzk7o.3.3`) owns the complete
//! LocalAPI-reported Tailscale address set on one port, revalidates and rebinds
//! it, verifies every kernel-observed peer with WhoIs, resolves peer identity,
//! consent generation, and pair-key generation from durable local stores, and
//! hands the socket to T2.1's public source-coupled session acceptor.
//!
//! Unsigned bootstrap hello is answered on the same listener before
//! session_open. After hello or an authenticated session, the broker serves
//! one `ee.mesh.sync_round.v1` event-header batch from the origin store.
//! A same-EUID Unix-domain control channel lets another
//! local workspace register or unregister exact team routes without binding
//! a second TCP port. Durable lifecycle audit persistence and application
//! dispatch remain later T2.2 slices.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
#[cfg(unix)]
use std::fs;
use std::future::Future;
use std::io;
use std::net::Shutdown;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use asupersync::Cx;
use asupersync::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use asupersync::net::unix::{UnixListener, UnixStream};
use asupersync::net::{TcpListener, TcpStream};
use asupersync::time::BudgetTimeExt;
use asupersync::time::{sleep as asupersync_sleep, timeout, wall_now};
use serde::{Deserialize, Serialize};

use crate::config::{EnvVar, parse_env_bool_flag, read_env_var};
use crate::db::{
    DatabaseLocation, DbConnection, MeshPeerTransportIdentityError,
    ObserveMeshPeerTransportIdentityInput,
};
use crate::mesh::bootstrap_envelope::{
    BODY_FETCH_REQUEST_SCHEMA_V1, BODY_FETCH_RESPONSE_SCHEMA_V1, BOOTSTRAP_DECLINE_SCHEMA_V1,
    BOOTSTRAP_MAX_ENVELOPE_BYTES, BodyFetchRequest, BodyFetchResponse, BootstrapAdmission,
    BootstrapCapability, BootstrapDeclineV1, SYNC_ROUND_SCHEMA_V1, SyncRoundEvent,
    SyncRoundResponse, SyncRoundTip, decode_envelope, encode_envelope, parse_sync_round_request,
};
use crate::mesh::discovery_policy::{DiscoveryMode, EE_MESH_SERVICE_TAG, load_workspace_lists};
use crate::mesh::hello::{
    HelloOutcome, ResponderContext, decide_hello_response, parse_hello_request,
};
pub use crate::mesh::key_store::MESH_KEY_STORE_UNAVAILABLE_CODE;
use crate::mesh::key_store::{KeyStoreError, MeshKeyStore, PairKeyClass};
use crate::mesh::peer::{MeshPeerRecord, MeshPeerState};
use crate::mesh::transport_session::{
    AcceptedSessionConfig, AcceptedSourceAttestation, AuthenticatedTransportSession,
    FrameCapability, HandshakeObservations, ResolvedAcceptedRoute, ResponderExpectations,
    SessionCapabilities, SessionChannelError, SessionChannelLimits, SessionMessage,
    UntrustedRouteSelectors, accept_authenticated_session_with_open_bytes,
};

pub const RESPONDER_BROKER_STATUS_SCHEMA_V1: &str = "ee.mesh.responder_broker.status.v1";
pub const RESPONDER_BROKER_AUDIT_SCHEMA_V1: &str = "ee.mesh.responder_broker.audit.v1";
pub const MESH_RESPONDER_ROUTE_UNAVAILABLE_CODE: &str = "mesh_responder_route_unavailable";
pub const MESH_BOOTSTRAP_IDENTITY_UNVERIFIED_CODE: &str = "mesh_bootstrap_identity_unverified";
pub const MESH_RESPONDER_PORT_CONFLICT_CODE: &str = "mesh_responder_port_conflict";
pub const MESH_RESPONDER_IDENTITY_UPGRADE_REQUIRED_CODE: &str =
    "mesh_responder_identity_upgrade_required";
pub const RESPONDER_CONTROL_SCHEMA_V1: &str = "ee.mesh.responder_control.v1";
pub const RESPONDER_CONTROL_MAX_BYTES: usize = 8 * 1024;
const CONTROL_NONCE_HEX_LEN: usize = 16;

#[cfg(unix)]
const LOCAL_API_MAX_RESPONSE_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const LOCAL_API_MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_RECENT_BROKER_AUDIT_EVENTS: usize = 128;
const MIN_OWNER_REVALIDATE_INTERVAL: Duration = Duration::from_millis(100);
const MAX_OWNER_REVALIDATE_INTERVAL: Duration = Duration::from_secs(60);

pub type TailscaleLocalApiFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ResponderBrokerError>> + 'a>>;

/// Minimal verified local identity needed to validate one listener bind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTailscaleIdentity {
    pub stable_id: String,
    pub current_node_pubkey: String,
    pub tailnet_id: String,
}

/// Authoritative LocalAPI status snapshot used for full-address-set binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTailscaleStatus {
    pub identity: LocalTailscaleIdentity,
    pub addresses: Vec<IpAddr>,
}

/// Minimal accepted-peer identity returned by LocalAPI WhoIs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhoIsIdentity {
    pub stable_id: String,
    pub current_node_pubkey: String,
    pub login_name: Option<String>,
    pub display_name: Option<String>,
    pub user_id: Option<String>,
}

/// Narrow conformance seam around the two read-only LocalAPI calls the broker
/// needs. Production uses [`TailscaleLocalApiClient`]; tests may host a fake
/// LocalAPI socket while exercising the same broker path.
pub trait TailscaleLocalApi: Send + Sync {
    fn local_status<'a>(
        &'a self,
        _cx: &'a Cx,
    ) -> TailscaleLocalApiFuture<'a, LocalTailscaleStatus> {
        Box::pin(async { Err(ResponderBrokerError::InvalidConfiguration) })
    }

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

    /// Team-join TCP may bind loopback when tailscaled is not present.
    fn allows_loopback_bind(&self) -> bool {
        false
    }
}

impl<T: TailscaleLocalApi + ?Sized> TailscaleLocalApi for Arc<T> {
    fn local_status<'a>(&'a self, cx: &'a Cx) -> TailscaleLocalApiFuture<'a, LocalTailscaleStatus> {
        (**self).local_status(cx)
    }

    fn verify_local_address<'a>(
        &'a self,
        cx: &'a Cx,
        address: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, LocalTailscaleIdentity> {
        (**self).verify_local_address(cx, address)
    }

    fn who_is<'a>(
        &'a self,
        cx: &'a Cx,
        source: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, WhoIsIdentity> {
        (**self).who_is(cx, source)
    }

    fn allows_loopback_bind(&self) -> bool {
        (**self).allows_loopback_bind()
    }
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

    /// Select the first real tailscaled LocalAPI Unix socket present on this
    /// host. No CLI fallback is used for responder authority.
    #[must_use]
    pub fn discover(io_timeout: Duration) -> Option<Self> {
        let mut candidates = vec![
            PathBuf::from("/var/run/tailscale/tailscaled.sock"),
            PathBuf::from("/run/tailscale/tailscaled.sock"),
            PathBuf::from("/var/run/tailscaled.socket"),
        ];
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            candidates.push(
                home.join("Library/Containers/io.tailscale.ipn.macsys/Data/IPN/tailscaled.sock"),
            );
            candidates.push(
                home.join(
                    "Library/Group Containers/io.tailscale.ipn.macos/Data/IPN/tailscaled.sock",
                ),
            );
        }
        candidates
            .into_iter()
            .find(|candidate| local_api_socket_exists(candidate))
            .map(|socket_path| Self::new(socket_path, io_timeout))
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
    fn local_status<'a>(&'a self, cx: &'a Cx) -> TailscaleLocalApiFuture<'a, LocalTailscaleStatus> {
        Box::pin(async move {
            #[cfg(not(unix))]
            {
                let _ = (cx, &self.socket_path, self.io_timeout);
                return Err(ResponderBrokerError::PlatformUnsupported);
            }
            #[cfg(unix)]
            {
                let body = self.request_json(cx, "/localapi/v0/status").await?;
                parse_local_status(&body)
            }
        })
    }

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
                let status = self.local_status(cx).await?;
                if !status.addresses.contains(&address.ip()) {
                    return Err(ResponderBrokerError::WhoIsUnverified);
                }
                Ok(status.identity)
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
                    login_name: response
                        .user_profile
                        .as_ref()
                        .and_then(|profile| profile.login_name.clone())
                        .filter(|login| !login.trim().is_empty()),
                    display_name: response
                        .user_profile
                        .as_ref()
                        .and_then(|profile| profile.display_name.clone())
                        .filter(|name| !name.trim().is_empty()),
                    user_id: response.user_profile.as_ref().and_then(|profile| {
                        profile.id.as_ref().and_then(|value| {
                            value.as_str().map(str::to_owned).or_else(|| {
                                value
                                    .as_i64()
                                    .map(|id| id.to_string())
                                    .or_else(|| value.as_u64().map(|id| id.to_string()))
                            })
                        })
                    }),
                })
            }
        })
    }
}

/// WhoIs row for one team-join enrolled peer endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TeamJoinWhoIsPeer {
    ip: IpAddr,
    stable_id: String,
    current_node_pubkey: String,
}

/// LocalAPI stand-in for team-join TCP when tailscaled is absent.
#[derive(Clone, Debug)]
pub struct TeamJoinLocalApi {
    identity: LocalTailscaleIdentity,
    addresses: Vec<IpAddr>,
    peers: Vec<TeamJoinWhoIsPeer>,
}

impl TeamJoinLocalApi {
    /// Build from enrolled team-join peers only. Mixed tailnet peers fail.
    pub fn from_registrations(
        connection: &DbConnection,
        registrations: &[DurableResponderRegistration],
    ) -> Result<Self, ResponderBrokerError> {
        if registrations.is_empty() {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        let responder_node_id = registrations[0].responder_node_id.clone();
        if registrations
            .iter()
            .any(|registration| registration.responder_node_id != responder_node_id)
        {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        let mut peers = Vec::new();
        for registration in registrations {
            let peer = connection
                .get_mesh_peer(&registration.workspace_id, &registration.peer_handle)
                .map_err(|_| ResponderBrokerError::RouteUnavailable)?
                .filter(|peer| peer.enabled)
                .ok_or(ResponderBrokerError::RouteUnavailable)?;
            let policy = peer
                .policy_summary_json
                .as_deref()
                .ok_or(ResponderBrokerError::IdentityUpgradeRequired)
                .and_then(|json| {
                    serde_json::from_str::<MeshPeerRecord>(json)
                        .map_err(|_| ResponderBrokerError::IdentityUpgradeRequired)
                })?;
            if !crate::mesh::team::team_join_allows_ungranted_route(&policy) {
                return Err(ResponderBrokerError::RouteUnavailable);
            }
            let endpoint = peer_endpoint_for_whois(&policy.endpoint.endpoint)?;
            let current_node_pubkey = if policy.endpoint.tailscale_node_key.starts_with("nodekey:")
            {
                policy.endpoint.tailscale_node_key.clone()
            } else {
                format!("nodekey:{}", policy.endpoint.tailscale_node_key)
            };
            peers.push(TeamJoinWhoIsPeer {
                ip: endpoint.ip(),
                stable_id: format!("team-join-{}", peer.origin_node_id),
                current_node_pubkey,
            });
        }
        Ok(Self {
            identity: LocalTailscaleIdentity {
                stable_id: format!("team-join-{responder_node_id}"),
                current_node_pubkey: format!("nodekey:{responder_node_id}"),
                tailnet_id: crate::mesh::team::TEAM_JOIN_TAILNET_ID.to_owned(),
            },
            addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            peers,
        })
    }

    #[must_use]
    pub fn all_loopback(&self) -> bool {
        !self.peers.is_empty() && self.peers.iter().all(|peer| peer.ip.is_loopback())
    }

    #[must_use]
    pub fn identity_for_source(&self, source: SocketAddr) -> Option<WhoIsIdentity> {
        self.peers
            .iter()
            .find(|peer| peer.ip == source.ip())
            .map(|peer| WhoIsIdentity {
                stable_id: peer.stable_id.clone(),
                current_node_pubkey: peer.current_node_pubkey.clone(),
                login_name: None,
                display_name: None,
                user_id: None,
            })
    }
}

impl TailscaleLocalApi for TeamJoinLocalApi {
    fn local_status<'a>(
        &'a self,
        _cx: &'a Cx,
    ) -> TailscaleLocalApiFuture<'a, LocalTailscaleStatus> {
        Box::pin(async move {
            Ok(LocalTailscaleStatus {
                identity: self.identity.clone(),
                addresses: self.addresses.clone(),
            })
        })
    }

    fn verify_local_address<'a>(
        &'a self,
        _cx: &'a Cx,
        address: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, LocalTailscaleIdentity> {
        Box::pin(async move {
            if address.ip().is_loopback() || self.addresses.contains(&address.ip()) {
                Ok(self.identity.clone())
            } else {
                Err(ResponderBrokerError::WhoIsUnverified)
            }
        })
    }

    fn who_is<'a>(
        &'a self,
        _cx: &'a Cx,
        source: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, WhoIsIdentity> {
        Box::pin(async move {
            self.identity_for_source(source)
                .ok_or(ResponderBrokerError::WhoIsUnverified)
        })
    }

    fn allows_loopback_bind(&self) -> bool {
        true
    }
}

/// Production LocalAPI or the team-join loopback stand-in.
#[derive(Clone, Debug)]
pub enum InboundLocalApi {
    Tailscale(TailscaleLocalApiClient),
    TeamJoin(TeamJoinLocalApi),
}

impl InboundLocalApi {
    #[must_use]
    pub fn prefer(
        connection: &DbConnection,
        registrations: &[DurableResponderRegistration],
        localapi_socket: Option<&Path>,
    ) -> Option<Self> {
        if let Some(path) = localapi_socket {
            return Some(Self::Tailscale(TailscaleLocalApiClient::new(
                path,
                Duration::from_secs(2),
            )));
        }
        let team_join = TeamJoinLocalApi::from_registrations(connection, registrations).ok();
        if team_join
            .as_ref()
            .is_some_and(TeamJoinLocalApi::all_loopback)
        {
            return team_join.map(Self::TeamJoin);
        }
        if let Some(client) = TailscaleLocalApiClient::discover(Duration::from_secs(2)) {
            return Some(Self::Tailscale(client));
        }
        team_join.map(Self::TeamJoin)
    }

    #[must_use]
    pub const fn is_team_join(&self) -> bool {
        matches!(self, Self::TeamJoin(_))
    }
}

impl TailscaleLocalApi for InboundLocalApi {
    fn local_status<'a>(&'a self, cx: &'a Cx) -> TailscaleLocalApiFuture<'a, LocalTailscaleStatus> {
        match self {
            Self::Tailscale(client) => client.local_status(cx),
            Self::TeamJoin(api) => api.local_status(cx),
        }
    }

    fn verify_local_address<'a>(
        &'a self,
        cx: &'a Cx,
        address: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, LocalTailscaleIdentity> {
        match self {
            Self::Tailscale(client) => client.verify_local_address(cx, address),
            Self::TeamJoin(api) => api.verify_local_address(cx, address),
        }
    }

    fn who_is<'a>(
        &'a self,
        cx: &'a Cx,
        source: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, WhoIsIdentity> {
        match self {
            Self::Tailscale(client) => client.who_is(cx, source),
            Self::TeamJoin(api) => api.who_is(cx, source),
        }
    }

    fn allows_loopback_bind(&self) -> bool {
        match self {
            Self::Tailscale(_) => false,
            Self::TeamJoin(api) => api.allows_loopback_bind(),
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
struct LocalApiStatus {
    #[serde(rename = "BackendState")]
    backend_state: Option<String>,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Self")]
    local: Option<LocalApiStatusNode>,
    #[serde(rename = "CurrentTailnet")]
    current_tailnet: Option<LocalApiCurrentTailnet>,
    #[serde(rename = "MagicDNSSuffix")]
    magic_dns_suffix: Option<String>,
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
    #[serde(rename = "Tailnet")]
    tailnet_id: Option<String>,
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
struct LocalApiCurrentTailnet {
    #[serde(rename = "MagicDNSSuffix")]
    magic_dns_suffix: Option<String>,
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
struct LocalApiWhoIsResponse {
    #[serde(rename = "Node")]
    node: Option<LocalApiWhoIsNode>,
    #[serde(rename = "UserProfile")]
    user_profile: Option<LocalApiWhoIsUserProfile>,
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
struct LocalApiWhoIsUserProfile {
    #[serde(rename = "ID")]
    id: Option<serde_json::Value>,
    #[serde(rename = "LoginName")]
    login_name: Option<String>,
    #[serde(rename = "DisplayName")]
    display_name: Option<String>,
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

#[cfg(unix)]
fn parse_local_status(body: &[u8]) -> Result<LocalTailscaleStatus, ResponderBrokerError> {
    let status: LocalApiStatus =
        serde_json::from_slice(body).map_err(|_| ResponderBrokerError::WhoIsUnverified)?;
    if status.backend_state.as_deref() != Some("Running") {
        return Err(ResponderBrokerError::TransportUnavailable);
    }
    let local = status.local.ok_or(ResponderBrokerError::WhoIsUnverified)?;
    let tailnet_id = local
        .tailnet_id
        .or_else(|| {
            status
                .current_tailnet
                .and_then(|tailnet| tailnet.magic_dns_suffix)
        })
        .or(status.magic_dns_suffix)
        .filter(|value| valid_identity(value))
        .ok_or(ResponderBrokerError::WhoIsUnverified)?;
    let mut addresses = status
        .tailscale_ips
        .iter()
        .chain(&local.tailscale_ips)
        .filter_map(|value| value.parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_unspecified())
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(ResponderBrokerError::TransportUnavailable);
    }
    if !valid_identity(&local.stable_id) || !valid_node_key(&local.current_node_pubkey) {
        return Err(ResponderBrokerError::WhoIsUnverified);
    }
    Ok(LocalTailscaleStatus {
        identity: LocalTailscaleIdentity {
            stable_id: local.stable_id,
            current_node_pubkey: local.current_node_pubkey,
            tailnet_id,
        },
        addresses,
    })
}

#[cfg(unix)]
fn local_api_socket_exists(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;

    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn local_api_socket_exists(_path: &Path) -> bool {
    false
}

/// One locally registered responder route. No network message can introduce
/// or modify the workspace path or peer handle.
#[derive(Clone, Debug)]
pub struct RegisteredResponderRoute {
    pub workspace_path: PathBuf,
    /// Durable authority store used by production owner registrations. `None`
    /// is retained only for the lower-level transport conformance seam.
    pub database_path: Option<PathBuf>,
    pub peer_handle: String,
    pub committed_port: u16,
    pub expectations: ResponderExpectations,
    pub responder_node_pubkey: String,
    pub peer_transport_key_generation: u64,
    pub grant_generation: u64,
    pub capabilities: SessionCapabilities,
    pub limits: SessionChannelLimits,
}

/// Secret-free local registration request. Remote frames cannot supply any
/// of these fields; peer identity, grants, and pair-key generation are loaded
/// from their durable stores before the route enters the registry.
#[derive(Clone, Debug)]
pub struct DurableResponderRegistration {
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub workspace_id: String,
    pub team_id: String,
    pub responder_node_id: String,
    pub peer_handle: String,
    pub committed_port: u16,
    pub capabilities: SessionCapabilities,
    pub limits: SessionChannelLimits,
}

/// Enrolled team pair-key peers become durable inbound routes.
pub fn plan_team_responder_registrations(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
    database_path: &Path,
    committed_port: u16,
) -> Vec<DurableResponderRegistration> {
    let Ok(teams) = crate::mesh::team::load_local_teams(connection) else {
        return Vec::new();
    };
    let Ok(peers) = connection.list_mesh_peers(workspace_id) else {
        return Vec::new();
    };
    let mut registrations = Vec::new();
    for team in &teams {
        for peer in peers.iter().filter(|peer| peer.enabled) {
            if crate::mesh::team::team_pair_peer_handle(&team.team_id, &peer.origin_node_id)
                != peer.peer_id
            {
                continue;
            }
            registrations.push(DurableResponderRegistration {
                workspace_path: workspace_path.to_path_buf(),
                database_path: database_path.to_path_buf(),
                workspace_id: workspace_id.to_owned(),
                team_id: team.team_id.clone(),
                responder_node_id: team.origin_node_id.clone(),
                peer_handle: peer.peer_id.clone(),
                committed_port,
                capabilities: SessionCapabilities::base(),
                limits: SessionChannelLimits::default(),
            });
        }
    }
    registrations
}

/// Same-EUID control-channel operation. Network frames never carry these.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponderControlOp {
    Register,
    Unregister,
    Status,
}

/// Bounded local registration request. Paths are revalidated against the
/// caller's filesystem; they are never taken from a network peer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResponderControlRequest {
    pub schema: String,
    pub op: ResponderControlOp,
    pub nonce: String,
    pub workspace_id: String,
    pub team_id: String,
    pub responder_node_id: String,
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub peer_handles: Vec<String>,
    pub committed_port: u16,
}

/// Secret-free control-channel reply. Echoes the request nonce only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponderControlResponse {
    pub schema: String,
    pub ok: bool,
    pub nonce: String,
    pub code: Option<String>,
    pub message: Option<String>,
    pub bound_addresses: Vec<String>,
    pub registered_routes: usize,
}

/// Authoritative LocalAPI snapshot plus the exact durable route it resolved.
#[derive(Clone, Debug)]
pub struct ResolvedResponderRegistration {
    pub local_status: LocalTailscaleStatus,
    pub route: RegisteredResponderRoute,
}

/// Resolve one route from LocalAPI authority and durable local stores.
///
/// The only caller inputs are local registration scope. Stable node ids,
/// current node keys, key generations, and grant generations are never
/// accepted from the network or command line.
pub async fn resolve_durable_registration<A: TailscaleLocalApi>(
    cx: &Cx,
    local_api: &A,
    connection: &DbConnection,
    registration: &DurableResponderRegistration,
) -> Result<ResolvedResponderRegistration, ResponderBrokerError> {
    validate_durable_registration(connection, registration)?;
    let local_status = local_api.local_status(cx).await.map_err(|error| {
        if matches!(
            error,
            ResponderBrokerError::WhoIsUnavailable | ResponderBrokerError::TransportUnavailable
        ) {
            ResponderBrokerError::TransportUnavailable
        } else {
            error
        }
    })?;
    let peer = connection
        .get_mesh_peer(&registration.workspace_id, &registration.peer_handle)
        .map_err(|_| ResponderBrokerError::RouteUnavailable)?
        .filter(|peer| peer.enabled)
        .ok_or(ResponderBrokerError::RouteUnavailable)?;
    let policy = peer
        .policy_summary_json
        .as_deref()
        .ok_or(ResponderBrokerError::IdentityUpgradeRequired)
        .and_then(|json| {
            serde_json::from_str::<MeshPeerRecord>(json)
                .map_err(|_| ResponderBrokerError::IdentityUpgradeRequired)
        })?;
    if policy.peer_id != registration.peer_handle
        || policy.workspace_id != registration.workspace_id
        || policy.state != MeshPeerState::Active
        || !policy.handshake.granted
        || !policy.handshake.discovery_consent
        || !crate::mesh::team::team_join_tailnet_matches(
            &policy.endpoint.tailnet_id,
            &local_status.identity.tailnet_id,
        )
    {
        return Err(ResponderBrokerError::RouteUnavailable);
    }

    let endpoint = peer_endpoint_for_whois(&policy.endpoint.endpoint)?;
    let who_is = local_api.who_is(cx, endpoint).await?;
    let expected_key = &policy.endpoint.tailscale_node_key;
    let who_matches_key = who_is.current_node_pubkey == *expected_key
        || who_is.current_node_pubkey == format!("nodekey:{expected_key}");
    if peer.transport_identity.is_none() && !who_matches_key {
        return Err(ResponderBrokerError::WhoIsUnverified);
    }
    let peer = connection
        .observe_mesh_peer_transport_identity(&ObserveMeshPeerTransportIdentityInput {
            workspace_id: registration.workspace_id.clone(),
            peer_id: registration.peer_handle.clone(),
            tailnet_id: local_status.identity.tailnet_id.clone(),
            stable_node_id: who_is.stable_id,
            current_node_pubkey: who_is.current_node_pubkey,
            observed_at: None,
        })
        .map_err(map_peer_identity_error)?;
    let transport_identity = peer
        .transport_identity
        .ok_or(ResponderBrokerError::IdentityUpgradeRequired)?;
    let grant_generation = connection
        .get_mesh_lane_grant_state(&registration.workspace_id, &registration.peer_handle)
        .map_err(|_| ResponderBrokerError::RouteUnavailable)?
        .filter(|grant| grant.target_matches_current_peer && grant.grant_generation > 0)
        .map(|grant| grant.grant_generation)
        .or_else(|| crate::mesh::team::team_join_allows_ungranted_route(&policy).then_some(0))
        .ok_or(ResponderBrokerError::RouteUnavailable)?;
    let pair_record = MeshKeyStore::open_existing(&registration.workspace_path)
        .map_err(map_key_store_error)?
        .ok_or(ResponderBrokerError::KeyStoreUnavailable)?
        .load_pair_key(&registration.peer_handle, PairKeyClass::Current)
        .map_err(map_key_store_error)?
        .ok_or(ResponderBrokerError::PairingRequired)?;

    let route = RegisteredResponderRoute {
        workspace_path: registration.workspace_path.clone(),
        database_path: Some(registration.database_path.clone()),
        peer_handle: registration.peer_handle.clone(),
        committed_port: registration.committed_port,
        expectations: ResponderExpectations {
            team_id: registration.team_id.clone(),
            tailnet_id: local_status.identity.tailnet_id.clone(),
            responder_node_id: registration.responder_node_id.clone(),
            responder_workspace_id: registration.workspace_id.clone(),
            responder_stable_id: local_status.identity.stable_id.clone(),
            initiator_node_id: peer.origin_node_id,
            initiator_stable_id: transport_identity.stable_node_id,
            pair_key_generation: pair_record.generation.get(),
        },
        responder_node_pubkey: local_status.identity.current_node_pubkey.clone(),
        peer_transport_key_generation: transport_identity.key_generation,
        grant_generation,
        capabilities: registration.capabilities.clone(),
        limits: registration.limits,
    };
    route.validate()?;
    Ok(ResolvedResponderRegistration {
        local_status,
        route,
    })
}

fn validate_durable_registration(
    connection: &DbConnection,
    registration: &DurableResponderRegistration,
) -> Result<(), ResponderBrokerError> {
    if registration.committed_port < 1024
        || !valid_identity(&registration.team_id)
        || !valid_identity(&registration.workspace_id)
        || !valid_identity(&registration.responder_node_id)
        || !valid_opaque_peer_handle(&registration.peer_handle)
        || !registration.workspace_path.is_absolute()
        || !registration.database_path.is_absolute()
        || registration.workspace_path.canonicalize().ok().as_ref()
            != Some(&registration.workspace_path)
        || registration.database_path.canonicalize().ok().as_ref()
            != Some(&registration.database_path)
    {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    let connection_path = match connection.location() {
        DatabaseLocation::File(path) => path,
        DatabaseLocation::Memory => return Err(ResponderBrokerError::InvalidConfiguration),
    };
    if connection_path.canonicalize().ok().as_ref() != Some(&registration.database_path) {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    let workspace = connection
        .get_workspace(&registration.workspace_id)
        .map_err(|_| ResponderBrokerError::RouteUnavailable)?
        .ok_or(ResponderBrokerError::InvalidConfiguration)?;
    let stored_workspace_path = PathBuf::from(workspace.path);
    if !stored_workspace_path.is_absolute()
        || stored_workspace_path.canonicalize().ok().as_ref() != Some(&registration.workspace_path)
    {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    Ok(())
}

fn peer_endpoint_for_whois(endpoint: &str) -> Result<SocketAddr, ResponderBrokerError> {
    endpoint
        .parse::<SocketAddr>()
        .or_else(|_| endpoint.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, 1)))
        .map_err(|_| ResponderBrokerError::IdentityUpgradeRequired)
}

fn map_peer_identity_error(error: MeshPeerTransportIdentityError) -> ResponderBrokerError {
    match error {
        MeshPeerTransportIdentityError::StableIdentityMismatch
        | MeshPeerTransportIdentityError::AmbiguousStableIdentity => {
            ResponderBrokerError::WhoIsUnverified
        }
        MeshPeerTransportIdentityError::InvalidObservation => ResponderBrokerError::WhoIsUnverified,
        MeshPeerTransportIdentityError::PeerUnavailable
        | MeshPeerTransportIdentityError::AmbiguousGrantTarget
        | MeshPeerTransportIdentityError::RandomnessUnavailable
        | MeshPeerTransportIdentityError::GenerationExhausted
        | MeshPeerTransportIdentityError::Storage(_) => ResponderBrokerError::RouteUnavailable,
    }
}

impl RegisteredResponderRoute {
    fn validate(&self) -> Result<(), ResponderBrokerError> {
        if !self.workspace_path.is_absolute()
            || self
                .database_path
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
            || !valid_opaque_peer_handle(&self.peer_handle)
            || self.committed_port < 1024
            || !valid_identity(&self.expectations.team_id)
            || !valid_identity(&self.expectations.tailnet_id)
            || !valid_identity(&self.expectations.responder_workspace_id)
            || !valid_identity(&self.expectations.responder_stable_id)
            || !valid_durable_node_principal(&self.expectations.initiator_node_id)
            || !valid_identity(&self.expectations.initiator_stable_id)
            || !valid_node_key(&self.responder_node_pubkey)
            || self.expectations.pair_key_generation == 0
            || self.peer_transport_key_generation == 0
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
    initiator_stable_id: String,
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
            initiator_stable_id: route.expectations.initiator_stable_id.clone(),
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
            initiator_stable_id: selectors.initiator_stable_id.clone(),
            pair_key_generation: selectors.pair_key_generation,
        })
    }

    #[must_use]
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    fn workspace_ids(&self) -> Vec<String> {
        let mut ids = self
            .routes
            .values()
            .map(|route| route.expectations.responder_workspace_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    fn first_workspace_path(&self) -> Option<&Path> {
        self.routes
            .values()
            .next()
            .map(|route| route.workspace_path.as_path())
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
    started_at: Instant,
    inflight_global: usize,
    inflight_by_source: BTreeMap<IpAddr, usize>,
    rate: BootstrapAdmission,
    peers: BTreeMap<String, crate::mesh::admission::MeshPeerAdmissionState>,
}

impl AdmissionState {
    fn new(limits: PreAuthAdmissionLimits) -> Self {
        Self {
            limits,
            started_at: Instant::now(),
            inflight_global: 0,
            inflight_by_source: BTreeMap::new(),
            rate: BootstrapAdmission::with_limits(
                limits.window_ms,
                limits.max_source_per_window,
                limits.max_global_per_window,
                limits.max_tracked_sources,
            ),
            peers: BTreeMap::new(),
        }
    }
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
    application_hello_performed: bool,
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
            application_hello_performed: false,
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
    IdentityUpgradeRequired,
    RouteUnavailable,
    KeyStoreUnavailable,
    PairingRequired,
    Session(SessionChannelError),
    BootstrapHelloAnswered,
}

impl ResponderBrokerError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PortConflict => MESH_RESPONDER_PORT_CONFLICT_CODE,
            Self::WhoIsUnavailable | Self::WhoIsUnverified => {
                MESH_BOOTSTRAP_IDENTITY_UNVERIFIED_CODE
            }
            Self::IdentityUpgradeRequired => MESH_RESPONDER_IDENTITY_UPGRADE_REQUIRED_CODE,
            Self::RouteUnavailable => MESH_RESPONDER_ROUTE_UNAVAILABLE_CODE,
            Self::KeyStoreUnavailable => MESH_KEY_STORE_UNAVAILABLE_CODE,
            Self::PairingRequired => "mesh_frame_auth_failed",
            Self::Session(error) => error.degraded_code(),
            Self::BootstrapHelloAnswered => "mesh_transport_unreachable",
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
            | Self::IdentityUpgradeRequired
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
            | Self::BootstrapHelloAnswered
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
            Self::IdentityUpgradeRequired => {
                "Mesh responder peer requires an authoritative LocalAPI identity observation"
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
            Self::BootstrapHelloAnswered => {
                "Mesh responder answered an unsigned bootstrap hello".to_owned()
            }
        }
    }

    #[must_use]
    pub const fn repair(&self) -> &'static str {
        match self {
            Self::PlatformUnsupported => {
                "Use TeamJoin inbound (`ee mesh hello-responder run`) on this host, or Tailscale LocalAPI on Unix."
            }
            Self::PortConflict => {
                "Stop the conflicting responder owner or align the user-scoped broker on the committed port."
            }
            Self::WhoIsUnavailable | Self::WhoIsUnverified => {
                "Restore the local Tailscale daemon and verify the peer remains enrolled on the expected tailnet."
            }
            Self::IdentityUpgradeRequired => {
                "Start the responder while the enrolled peer endpoint is reachable through Tailscale LocalAPI."
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
            | Self::BootstrapHelloAnswered
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
    listener: Option<TcpListener>,
    local_api: A,
    routes: Arc<ResponderRouteRegistry>,
    admission: Arc<Mutex<AdmissionState>>,
    runtime: Arc<Mutex<RuntimeStatus>>,
    bound_address: SocketAddr,
}

impl<A: TailscaleLocalApi> ResponderBroker<A> {
    pub async fn bind(
        cx: &Cx,
        address: SocketAddr,
        local_api: A,
        routes: ResponderRouteRegistry,
        admission_limits: PreAuthAdmissionLimits,
    ) -> Result<Self, ResponderBrokerError> {
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
            || local_identity.tailnet_id != routes.tailnet_id
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
            admission: Arc::new(Mutex::new(AdmissionState::new(admission_limits))),
            runtime: Arc::new(Mutex::new(RuntimeStatus::listening())),
            bound_address,
        })
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
        let mut stream = stream;
        let first_packet = match read_asupersync_framed(cx, &mut stream, limits.io_timeout).await {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = stream.shutdown(Shutdown::Both);
                self.record_error(&error, false, true);
                return Err(error);
            }
        };
        if decode_envelope(&first_packet)
            .ok()
            .is_some_and(|envelope| envelope.capability == BootstrapCapability::Join)
        {
            if let Err(error) =
                answer_bootstrap_join(cx, &mut stream, &self.routes, &first_packet, kernel_source)
                    .await
            {
                let _ = stream.shutdown(Shutdown::Write);
                self.record_error(&error, false, true);
                return Err(error);
            }
            let _ = stream.shutdown(Shutdown::Write);
            if let Ok(mut runtime) = self.runtime.lock() {
                runtime.application_hello_performed = true;
                runtime.record("bootstrap_join", None, false, false, false);
            }
            return Err(ResponderBrokerError::BootstrapHelloAnswered);
        }
        if decode_envelope(&first_packet)
            .ok()
            .is_some_and(|envelope| envelope.capability == BootstrapCapability::Hello)
        {
            if let Err(error) =
                answer_bootstrap_hello(cx, &mut stream, &self.routes, &first_packet).await
            {
                let _ = stream.shutdown(Shutdown::Both);
                self.record_error(&error, false, true);
                return Err(error);
            }
            let _ = answer_sync_round(cx, &mut stream, &self.routes).await;
            let _ = stream.shutdown(Shutdown::Both);
            if let Ok(mut runtime) = self.runtime.lock() {
                runtime.application_hello_performed = true;
                runtime.record("bootstrap_hello", None, false, false, false);
            }
            return Err(ResponderBrokerError::BootstrapHelloAnswered);
        }
        let accepted = accept_authenticated_session_with_open_bytes(
            cx,
            stream,
            limits,
            first_packet,
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

    /// Accept one authenticated session, serve one EventFetch/Summary
    /// sync-round from the origin store, then close the session.
    pub async fn accept_authenticated_and_serve(
        &self,
        cx: &Cx,
    ) -> Result<(), ResponderBrokerError> {
        let mut session = self.accept_authenticated(cx).await?;
        let result =
            serve_authenticated_sync_round(cx, &mut session, &self.routes, &self.admission).await;
        session.close();
        result
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
        let now_ms = u64::try_from(state.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
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
        let (state, accepted, authenticated, rejected, last_error, hello_performed) = self
            .runtime
            .lock()
            .map(|runtime| {
                (
                    runtime.state,
                    runtime.accepted_connections,
                    runtime.authenticated_sessions,
                    runtime.rejected_connections,
                    runtime.last_error_code,
                    runtime.application_hello_performed,
                )
            })
            .unwrap_or((
                ResponderBrokerState::Shutdown,
                0,
                0,
                0,
                Some("mesh_transport_unreachable"),
                false,
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
            application_hello_performed: hello_performed,
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
        self.listener.take();
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.state = ResponderBrokerState::Shutdown;
        }
    }
}

/// One user-scoped owner for the complete LocalAPI address set.
///
/// The owner periodically re-reads status while serving. Address loss closes
/// every stale listener before retry; a changed address set or local node-key
/// rotation is rebound atomically from the owner's perspective. It never
/// binds loopback, wildcard, or a non-LocalAPI address.
pub struct ResponderBrokerOwner<A> {
    local_api: Arc<A>,
    routes: ResponderRouteRegistry,
    durable_registrations: Option<Vec<DurableResponderRegistration>>,
    admission_limits: PreAuthAdmissionLimits,
    admission: Arc<Mutex<AdmissionState>>,
    brokers: Vec<ResponderBroker<Arc<A>>>,
    bound_addresses: Vec<SocketAddr>,
    revalidate_interval: Duration,
    last_revalidated_at: Instant,
    #[cfg(unix)]
    control: Option<ResponderControlListener>,
}

impl<A: TailscaleLocalApi> ResponderBrokerOwner<A> {
    pub async fn start(
        cx: &Cx,
        local_api: A,
        routes: ResponderRouteRegistry,
        admission_limits: PreAuthAdmissionLimits,
        revalidate_interval: Duration,
    ) -> Result<Self, ResponderBrokerError> {
        if !(MIN_OWNER_REVALIDATE_INTERVAL..=MAX_OWNER_REVALIDATE_INTERVAL)
            .contains(&revalidate_interval)
        {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        let admission_limits = admission_limits.validate()?;
        let mut owner = Self {
            local_api: Arc::new(local_api),
            routes,
            durable_registrations: None,
            admission_limits,
            admission: Arc::new(Mutex::new(AdmissionState::new(admission_limits))),
            brokers: Vec::new(),
            bound_addresses: Vec::new(),
            revalidate_interval,
            last_revalidated_at: Instant::now(),
            #[cfg(unix)]
            control: None,
        };
        owner.reconcile(cx).await?;
        Ok(owner)
    }

    /// Start the production owner from durable local registration scope.
    /// Every reconciliation re-resolves LocalAPI identity, the peer principal
    /// and key generation, the lane-grant generation, and T2.1's public
    /// pair-key seam before retaining or rebinding listeners.
    pub async fn start_durable(
        cx: &Cx,
        local_api: A,
        registrations: Vec<DurableResponderRegistration>,
        admission_limits: PreAuthAdmissionLimits,
        revalidate_interval: Duration,
    ) -> Result<Self, ResponderBrokerError> {
        if registrations.is_empty() {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        let local_api = Arc::new(local_api);
        let routes = resolve_durable_route_registry(cx, local_api.as_ref(), &registrations).await?;
        let mut owner =
            Self::start_with_arc(cx, local_api, routes, admission_limits, revalidate_interval)
                .await?;
        owner.durable_registrations = Some(registrations);
        Ok(owner)
    }

    async fn start_with_arc(
        cx: &Cx,
        local_api: Arc<A>,
        routes: ResponderRouteRegistry,
        admission_limits: PreAuthAdmissionLimits,
        revalidate_interval: Duration,
    ) -> Result<Self, ResponderBrokerError> {
        if !(MIN_OWNER_REVALIDATE_INTERVAL..=MAX_OWNER_REVALIDATE_INTERVAL)
            .contains(&revalidate_interval)
        {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        let admission_limits = admission_limits.validate()?;
        let mut owner = Self {
            local_api,
            routes,
            durable_registrations: None,
            admission_limits,
            admission: Arc::new(Mutex::new(AdmissionState::new(admission_limits))),
            brokers: Vec::new(),
            bound_addresses: Vec::new(),
            revalidate_interval,
            last_revalidated_at: Instant::now(),
            #[cfg(unix)]
            control: None,
        };
        owner.reconcile(cx).await?;
        Ok(owner)
    }

    #[must_use]
    pub fn bound_addresses(&self) -> &[SocketAddr] {
        &self.bound_addresses
    }

    #[must_use]
    pub fn route_count(&self) -> usize {
        self.routes.route_count()
    }

    /// Return the currently bound pair, transport-key, and grant generations
    /// for one opaque peer handle. This is a secret-free owner diagnostic and
    /// proves reconciliation actually replaced stale durable authority.
    #[must_use]
    pub fn route_generations(&self, peer_handle: &str) -> Option<(u64, u64, u64)> {
        self.routes
            .routes
            .values()
            .find(|route| route.peer_handle == peer_handle)
            .map(|route| {
                (
                    route.expectations.pair_key_generation,
                    route.peer_transport_key_generation,
                    route.grant_generation,
                )
            })
    }

    /// Publish the same-EUID control socket used by another local workspace
    /// to register exact routes with this owner.
    #[cfg(unix)]
    pub fn listen_control(
        &mut self,
        socket_path: impl Into<PathBuf>,
    ) -> Result<(), ResponderBrokerError> {
        self.control = Some(ResponderControlListener::publish(socket_path.into())?);
        Ok(())
    }

    #[cfg(unix)]
    #[must_use]
    pub fn control_socket_path(&self) -> Option<&Path> {
        self.control
            .as_ref()
            .map(|control| control.socket_path.as_path())
    }

    /// Accept and answer one inbound connection, then return.
    ///
    /// Unsigned hello/join is a successful one-shot: the reply is written
    /// before `BootstrapHelloAnswered`. Authenticated EventFetch/Summary
    /// runs the same serve path as [`Self::serve_until_cancelled`].
    pub async fn serve_one(&self, cx: &Cx) -> Result<(), ResponderBrokerError> {
        let broker = self
            .brokers
            .first()
            .ok_or(ResponderBrokerError::TransportUnavailable)?;
        match broker.accept_authenticated_and_serve(cx).await {
            Ok(()) => Ok(()),
            Err(ResponderBrokerError::BootstrapHelloAnswered) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Serve authenticated transport sessions until the caller context is
    /// cancelled. Each accepted session answers one EventFetch/Summary
    /// sync-round from the registered origin store, then closes.
    pub async fn serve_until_cancelled(&mut self, cx: &Cx) -> Result<(), ResponderBrokerError> {
        let mut listener_index = 0_usize;
        loop {
            checkpoint(cx, "responder owner")?;
            if self.brokers.is_empty() {
                match self.reconcile(cx).await {
                    Ok(()) => {}
                    Err(ResponderBrokerError::Cancelled) => {
                        self.shutdown();
                        return Err(ResponderBrokerError::Cancelled);
                    }
                    Err(_) => {
                        asupersync_sleep(cx.now(), self.revalidate_interval).await;
                        continue;
                    }
                }
            }
            #[cfg(unix)]
            self.poll_control(cx).await;
            if self.brokers.is_empty() {
                asupersync_sleep(cx.now(), self.revalidate_interval).await;
                continue;
            }
            listener_index %= self.brokers.len();
            let broker = &self.brokers[listener_index];
            let now = wall_now();
            let accept_timed_out = match timeout(
                now,
                self.revalidate_interval,
                broker.accept_authenticated_and_serve(cx),
            )
            .await
            {
                Ok(Ok(())) => false,
                Ok(Err(ResponderBrokerError::Cancelled)) => {
                    self.shutdown();
                    return Err(ResponderBrokerError::Cancelled);
                }
                Ok(Err(_rejected)) => false,
                Err(_) => true,
            };
            if (accept_timed_out || self.last_revalidated_at.elapsed() >= self.revalidate_interval)
                && let Err(error) = self.reconcile(cx).await
            {
                self.shutdown();
                if matches!(error, ResponderBrokerError::Cancelled) {
                    return Err(error);
                }
                asupersync_sleep(cx.now(), self.revalidate_interval).await;
            }
            listener_index = listener_index.saturating_add(1);
        }
    }

    pub async fn reconcile(&mut self, cx: &Cx) -> Result<(), ResponderBrokerError> {
        let status = match self.local_api.local_status(cx).await {
            Ok(status) => status,
            Err(error) => {
                self.shutdown_listeners();
                return Err(
                    if matches!(
                        error,
                        ResponderBrokerError::WhoIsUnavailable
                            | ResponderBrokerError::TransportUnavailable
                    ) {
                        ResponderBrokerError::TransportUnavailable
                    } else {
                        error
                    },
                );
            }
        };
        let refreshed_routes = if let Some(registrations) = &self.durable_registrations {
            match resolve_durable_route_registry(cx, self.local_api.as_ref(), registrations).await {
                Ok(routes) => Some(routes),
                Err(error) => {
                    self.shutdown_listeners();
                    return Err(error);
                }
            }
        } else {
            None
        };
        let routes_changed = refreshed_routes
            .as_ref()
            .is_some_and(|routes| !same_route_authority(&self.routes, routes));
        if let Some(routes) = refreshed_routes {
            self.routes = routes;
        }
        if status.identity.stable_id != self.routes.responder_stable_id
            || status.identity.tailnet_id != self.routes.tailnet_id
        {
            self.shutdown_listeners();
            return Err(ResponderBrokerError::WhoIsUnverified);
        }
        let mut desired = status
            .addresses
            .iter()
            .copied()
            .map(|ip| SocketAddr::new(ip, self.routes.committed_port))
            .collect::<Vec<_>>();
        desired.sort();
        desired.dedup();
        if desired.is_empty()
            || desired.iter().any(|address| {
                address.ip().is_unspecified()
                    || (address.ip().is_loopback() && !self.local_api.allows_loopback_bind())
            })
        {
            self.shutdown_listeners();
            return Err(ResponderBrokerError::TransportUnavailable);
        }
        let key_changed = status.identity.current_node_pubkey != self.routes.responder_node_pubkey;
        if desired == self.bound_addresses && !key_changed && !routes_changed {
            self.last_revalidated_at = Instant::now();
            return Ok(());
        }

        self.shutdown_listeners();
        let mut routes = self.routes.clone();
        if key_changed {
            routes.responder_node_pubkey = status.identity.current_node_pubkey.clone();
            for route in routes.routes.values_mut() {
                route.responder_node_pubkey = status.identity.current_node_pubkey.clone();
            }
        }
        let mut brokers = Vec::with_capacity(desired.len());
        for address in &desired {
            match ResponderBroker::bind(
                cx,
                *address,
                Arc::clone(&self.local_api),
                routes.clone(),
                self.admission_limits,
            )
            .await
            {
                Ok(mut broker) => {
                    broker.admission = Arc::clone(&self.admission);
                    brokers.push(broker);
                }
                Err(error) => {
                    for broker in &mut brokers {
                        broker.shutdown();
                    }
                    return Err(error);
                }
            }
        }
        self.routes = routes;
        self.bound_addresses = desired;
        self.brokers = brokers;
        self.last_revalidated_at = Instant::now();
        Ok(())
    }

    pub fn shutdown(&mut self) {
        self.shutdown_listeners();
    }

    fn shutdown_listeners(&mut self) {
        for broker in &mut self.brokers {
            broker.shutdown();
        }
        self.brokers.clear();
        self.bound_addresses.clear();
    }

    #[cfg(unix)]
    async fn poll_control(&mut self, cx: &Cx) {
        let Some(control) = self.control.as_ref() else {
            return;
        };
        let accepted = match timeout(
            wall_now(),
            Duration::from_millis(50),
            control.listener.accept(),
        )
        .await
        {
            Ok(Ok((stream, _))) => stream,
            Ok(Err(_)) | Err(_) => return,
        };
        let (mut stream, request) = match read_control_request(cx, accepted).await {
            Ok(read) => read,
            Err(_) => return,
        };
        let response = self.dispatch_control(cx, request).await;
        let _ = write_control_response(&mut stream, &response).await;
    }

    #[cfg(unix)]
    async fn dispatch_control(
        &mut self,
        cx: &Cx,
        request: ResponderControlRequest,
    ) -> ResponderControlResponse {
        let nonce = request.nonce.clone();
        let outcome = match request.op {
            ResponderControlOp::Status => Ok(()),
            ResponderControlOp::Register => self.apply_control_register(cx, &request).await,
            ResponderControlOp::Unregister => self.apply_control_unregister(cx, &request).await,
        };
        match outcome {
            Ok(()) => ResponderControlResponse {
                schema: RESPONDER_CONTROL_SCHEMA_V1.to_owned(),
                ok: true,
                nonce,
                code: None,
                message: None,
                bound_addresses: self
                    .bound_addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                registered_routes: self.route_count(),
            },
            Err(error) => ResponderControlResponse {
                schema: RESPONDER_CONTROL_SCHEMA_V1.to_owned(),
                ok: false,
                nonce,
                code: Some(error.code().to_owned()),
                message: Some(error.message()),
                bound_addresses: self
                    .bound_addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                registered_routes: self.route_count(),
            },
        }
    }

    #[cfg(unix)]
    async fn apply_control_register(
        &mut self,
        cx: &Cx,
        request: &ResponderControlRequest,
    ) -> Result<(), ResponderBrokerError> {
        let incoming = materialize_control_registrations(request)?;
        let mut next = self.durable_registrations.clone().unwrap_or_default();
        if let Some(existing) = next.first()
            && existing.committed_port != request.committed_port
        {
            return Err(ResponderBrokerError::PortConflict);
        }
        for registration in incoming {
            next.retain(|row| {
                !(row.workspace_id == registration.workspace_id
                    && row.team_id == registration.team_id
                    && row.peer_handle == registration.peer_handle)
            });
            next.push(registration);
        }
        let routes = resolve_durable_route_registry(cx, self.local_api.as_ref(), &next).await?;
        self.durable_registrations = Some(next);
        self.routes = routes;
        self.reconcile(cx).await
    }

    #[cfg(unix)]
    async fn apply_control_unregister(
        &mut self,
        cx: &Cx,
        request: &ResponderControlRequest,
    ) -> Result<(), ResponderBrokerError> {
        let Some(mut next) = self.durable_registrations.clone() else {
            return Err(ResponderBrokerError::RouteUnavailable);
        };
        let before = next.len();
        next.retain(|row| {
            !(row.workspace_id == request.workspace_id
                && row.team_id == request.team_id
                && request.peer_handles.contains(&row.peer_handle))
        });
        if next.len() == before {
            return Err(ResponderBrokerError::RouteUnavailable);
        }
        if next.is_empty() {
            self.durable_registrations = Some(next);
            self.shutdown_listeners();
            return Ok(());
        }
        let routes = resolve_durable_route_registry(cx, self.local_api.as_ref(), &next).await?;
        self.durable_registrations = Some(next);
        self.routes = routes;
        self.reconcile(cx).await
    }
}

async fn resolve_durable_route_registry<A: TailscaleLocalApi>(
    cx: &Cx,
    local_api: &A,
    registrations: &[DurableResponderRegistration],
) -> Result<ResponderRouteRegistry, ResponderBrokerError> {
    let mut routes = Vec::with_capacity(registrations.len());
    for registration in registrations {
        let connection = DbConnection::open_file(&registration.database_path)
            .map_err(|_| ResponderBrokerError::RouteUnavailable)?;
        routes.push(
            resolve_durable_registration(cx, local_api, &connection, registration)
                .await?
                .route,
        );
    }
    ResponderRouteRegistry::new(routes)
}

fn same_route_authority(
    current: &ResponderRouteRegistry,
    refreshed: &ResponderRouteRegistry,
) -> bool {
    if current.committed_port != refreshed.committed_port
        || current.tailnet_id != refreshed.tailnet_id
        || current.responder_stable_id != refreshed.responder_stable_id
        || current.responder_node_pubkey != refreshed.responder_node_pubkey
        || current.routes.len() != refreshed.routes.len()
    {
        return false;
    }
    current.routes.iter().all(|(selectors, route)| {
        refreshed.routes.get(selectors).is_some_and(|candidate| {
            route.workspace_path == candidate.workspace_path
                && route.database_path == candidate.database_path
                && route.peer_handle == candidate.peer_handle
                && route.committed_port == candidate.committed_port
                && route.expectations == candidate.expectations
                && route.responder_node_pubkey == candidate.responder_node_pubkey
                && route.peer_transport_key_generation == candidate.peer_transport_key_generation
                && route.grant_generation == candidate.grant_generation
                && route.capabilities == candidate.capabilities
                && route.limits == candidate.limits
        })
    })
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
    refresh_durable_route_authority(route, &who_is)?;
    let pair_key = load_route_pair_key(route)?;
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
            pair_key,
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

fn refresh_durable_route_authority(
    route: &RegisteredResponderRoute,
    who_is: &WhoIsIdentity,
) -> Result<(), ResponderBrokerError> {
    let Some(database_path) = route.database_path.as_ref() else {
        return Ok(());
    };
    if !database_path.is_file() {
        return Err(ResponderBrokerError::RouteUnavailable);
    }
    let connection = DbConnection::open_file(database_path)
        .map_err(|_| ResponderBrokerError::RouteUnavailable)?;
    let peer = connection
        .observe_mesh_peer_transport_identity(&ObserveMeshPeerTransportIdentityInput {
            workspace_id: route.expectations.responder_workspace_id.clone(),
            peer_id: route.peer_handle.clone(),
            tailnet_id: route.expectations.tailnet_id.clone(),
            stable_node_id: who_is.stable_id.clone(),
            current_node_pubkey: who_is.current_node_pubkey.clone(),
            observed_at: None,
        })
        .map_err(map_peer_identity_error)?;
    if peer
        .transport_identity
        .as_ref()
        .is_none_or(|identity| identity.key_generation != route.peer_transport_key_generation)
    {
        return Err(ResponderBrokerError::WhoIsUnverified);
    }
    let grant = connection
        .get_mesh_lane_grant_state(
            &route.expectations.responder_workspace_id,
            &route.peer_handle,
        )
        .map_err(|_| ResponderBrokerError::RouteUnavailable)?;
    if route.grant_generation == 0 {
        if grant.is_some_and(|grant| grant.grant_generation > 0) {
            return Err(ResponderBrokerError::RouteUnavailable);
        }
        return Ok(());
    }
    let Some(grant) = grant.filter(|grant| {
        grant.target_matches_current_peer && grant.grant_generation == route.grant_generation
    }) else {
        return Err(ResponderBrokerError::RouteUnavailable);
    };
    if grant.target_adapter.peer_id != route.peer_handle
        || grant.target_adapter.origin_node_id != route.expectations.initiator_node_id
    {
        return Err(ResponderBrokerError::RouteUnavailable);
    }
    Ok(())
}

fn load_route_pair_key(
    route: &RegisteredResponderRoute,
) -> Result<crate::mesh::key_store::SecretBytes, ResponderBrokerError> {
    let store = MeshKeyStore::open_existing(&route.workspace_path)
        .map_err(map_key_store_error)?
        .ok_or(ResponderBrokerError::KeyStoreUnavailable)?;
    let pair_record = store
        .load_pair_key(&route.peer_handle, PairKeyClass::Current)
        .map_err(map_key_store_error)?
        .ok_or(ResponderBrokerError::PairingRequired)?;
    if pair_record.generation.get() != route.expectations.pair_key_generation {
        return Err(ResponderBrokerError::PairingRequired);
    }
    Ok(pair_record.key)
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

fn valid_opaque_peer_handle(value: &str) -> bool {
    value.strip_prefix("peer_").is_some_and(|opaque| {
        opaque.len() == 32
            && opaque
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_durable_node_principal(value: &str) -> bool {
    value.strip_prefix("node_").is_some_and(|opaque| {
        opaque.len() == 32
            && opaque
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// User-scoped control socket path. Mirrors the daemon partition:
/// private `XDG_RUNTIME_DIR/ee/mesh-responder.sock`, otherwise
/// `${TMPDIR:-/tmp}/ee-${uid}/mesh-responder.sock`.
#[must_use]
pub fn default_responder_control_socket_path() -> PathBuf {
    default_responder_control_socket_path_with(
        |key| std::env::var_os(key),
        current_responder_euid(),
    )
}

fn default_responder_control_socket_path_with(
    mut env_var: impl FnMut(&str) -> Option<std::ffi::OsString>,
    uid: u32,
) -> PathBuf {
    let tmp = env_var("TMPDIR").unwrap_or_else(|| "/tmp".into());
    if let Some(runtime_dir) = env_var("XDG_RUNTIME_DIR") {
        let runtime = Path::new(&runtime_dir);
        if !runtime.as_os_str().is_empty()
            && runtime != Path::new("/tmp")
            && runtime != Path::new("/var/tmp")
            && runtime != Path::new("/private/tmp")
        {
            return runtime.join("ee").join("mesh-responder.sock");
        }
    }
    Path::new(&tmp)
        .join(format!("ee-{uid}"))
        .join("mesh-responder.sock")
}

#[cfg(unix)]
fn current_responder_euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn current_responder_euid() -> u32 {
    0
}

/// Submit one same-EUID control request to a published owner socket.
#[cfg(unix)]
pub fn submit_responder_control_request(
    socket_path: &Path,
    request: &ResponderControlRequest,
) -> Result<ResponderControlResponse, ResponderBrokerError> {
    validate_control_request(request)?;
    refuse_insecure_socket_path(socket_path)?;
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path)
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    let peer = control_std_peer_uid(&stream)?;
    if peer != current_responder_euid() {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    write_control_frame(&mut stream, request)?;
    read_control_frame(&mut stream)
}

#[cfg(unix)]
struct ResponderControlListener {
    listener: UnixListener,
    socket_path: PathBuf,
}

#[cfg(unix)]
impl ResponderControlListener {
    fn publish(socket_path: PathBuf) -> Result<Self, ResponderBrokerError> {
        publish_control_socket(&socket_path)?;
        let std_listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .map_err(|_| ResponderBrokerError::PortConflict)?;
        if let Err(error) = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)) {
            let _ = remove_owned_socket(&socket_path);
            let _ = error;
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        // Bind-then-chmod is the same contract as the daemon: parent is 0700,
        // so no other UID can connect during the brief window.
        let listener = UnixListener::from_std(std_listener)
            .map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
        Ok(Self {
            listener,
            socket_path,
        })
    }
}

#[cfg(unix)]
impl Drop for ResponderControlListener {
    fn drop(&mut self) {
        let _ = remove_owned_socket(&self.socket_path);
    }
}

#[must_use]
pub fn responder_control_status_request() -> ResponderControlRequest {
    ResponderControlRequest {
        schema: RESPONDER_CONTROL_SCHEMA_V1.to_owned(),
        op: ResponderControlOp::Status,
        nonce: "0".repeat(CONTROL_NONCE_HEX_LEN),
        workspace_id: String::new(),
        team_id: String::new(),
        responder_node_id: String::new(),
        workspace_path: PathBuf::new(),
        database_path: PathBuf::new(),
        peer_handles: Vec::new(),
        committed_port: 0,
    }
}

fn validate_control_request(request: &ResponderControlRequest) -> Result<(), ResponderBrokerError> {
    if request.schema != RESPONDER_CONTROL_SCHEMA_V1
        || request.nonce.len() < CONTROL_NONCE_HEX_LEN
        || !request.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    if request.op == ResponderControlOp::Status {
        return Ok(());
    }
    if !valid_identity(&request.workspace_id)
        || !valid_identity(&request.team_id)
        || !valid_identity(&request.responder_node_id)
        || request.committed_port < 1024
        || request.peer_handles.is_empty()
        || request.peer_handles.len() > 32
        || !request
            .peer_handles
            .iter()
            .all(|handle| valid_opaque_peer_handle(handle))
    {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    revalidate_control_paths(request)
}

fn revalidate_control_paths(request: &ResponderControlRequest) -> Result<(), ResponderBrokerError> {
    let workspace = owner_safe_canonical_path(&request.workspace_path)?;
    let database = owner_safe_canonical_path(&request.database_path)?;
    if database != workspace.join(".ee").join("ee.db") {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    Ok(())
}

fn owner_safe_canonical_path(path: &Path) -> Result<PathBuf, ResponderBrokerError> {
    if !path.is_absolute() {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        return Err(ResponderBrokerError::PlatformUnsupported);
    }
    #[cfg(unix)]
    {
        let metadata =
            fs::symlink_metadata(path).map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
        if metadata.file_type().is_symlink() || metadata.uid() != current_responder_euid() {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
        if canonical != path {
            return Err(ResponderBrokerError::InvalidConfiguration);
        }
        Ok(canonical)
    }
}

fn materialize_control_registrations(
    request: &ResponderControlRequest,
) -> Result<Vec<DurableResponderRegistration>, ResponderBrokerError> {
    validate_control_request(request)?;
    let workspace_path = owner_safe_canonical_path(&request.workspace_path)?;
    let database_path = owner_safe_canonical_path(&request.database_path)?;
    Ok(request
        .peer_handles
        .iter()
        .map(|peer_handle| DurableResponderRegistration {
            workspace_path: workspace_path.clone(),
            database_path: database_path.clone(),
            workspace_id: request.workspace_id.clone(),
            team_id: request.team_id.clone(),
            responder_node_id: request.responder_node_id.clone(),
            peer_handle: peer_handle.clone(),
            committed_port: request.committed_port,
            capabilities: SessionCapabilities::base(),
            limits: SessionChannelLimits::default(),
        })
        .collect())
}

#[cfg(unix)]
fn publish_control_socket(socket_path: &Path) -> Result<(), ResponderBrokerError> {
    let parent = socket_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(ResponderBrokerError::InvalidConfiguration)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    let parent_meta =
        fs::symlink_metadata(parent).map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    if parent_meta.file_type().is_symlink()
        || !parent_meta.file_type().is_dir()
        || parent_meta.uid() != current_responder_euid()
        || parent_meta.mode() & 0o077 != 0
    {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    match fs::symlink_metadata(socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            let _ = remove_owned_socket(socket_path);
        }
        Ok(_) => return Err(ResponderBrokerError::PortConflict),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(ResponderBrokerError::InvalidConfiguration),
    }
    Ok(())
}

#[cfg(unix)]
fn remove_owned_socket(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to remove a non-socket control path",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn refuse_insecure_socket_path(path: &Path) -> Result<(), ResponderBrokerError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    if !metadata.file_type().is_socket() || metadata.uid() != current_responder_euid() {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    Ok(())
}

#[cfg(unix)]
fn control_std_peer_uid(
    stream: &std::os::unix::net::UnixStream,
) -> Result<u32, ResponderBrokerError> {
    #[cfg(target_os = "linux")]
    {
        rustix::net::sockopt::socket_peercred(stream)
            .map(|credentials| credentials.uid.as_raw())
            .map_err(|_| ResponderBrokerError::InvalidConfiguration)
    }
    #[cfg(target_vendor = "apple")]
    {
        stream
            .peer_cred()
            .map(|credentials| credentials.uid)
            .map_err(|_| ResponderBrokerError::InvalidConfiguration)
    }
    #[cfg(all(not(target_os = "linux"), not(target_vendor = "apple")))]
    {
        let _ = stream;
        Err(ResponderBrokerError::PlatformUnsupported)
    }
}

#[cfg(unix)]
async fn read_control_request(
    cx: &Cx,
    mut stream: UnixStream,
) -> Result<(UnixStream, ResponderControlRequest), ResponderBrokerError> {
    checkpoint(cx, "responder control")?;
    let peer = stream
        .peer_cred()
        .map(|credentials| credentials.uid)
        .map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    if peer != current_responder_euid() {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    let request = read_control_frame_async(cx, &mut stream).await?;
    validate_control_request(&request)?;
    Ok((stream, request))
}

#[cfg(unix)]
async fn write_control_response(
    stream: &mut UnixStream,
    response: &ResponderControlResponse,
) -> Result<(), ResponderBrokerError> {
    write_control_frame_async(stream, response).await
}

#[cfg(unix)]
fn write_control_frame<T: Serialize>(
    stream: &mut std::os::unix::net::UnixStream,
    value: &T,
) -> Result<(), ResponderBrokerError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    if bytes.len() > RESPONDER_CONTROL_MAX_BYTES {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    let len = u32::try_from(bytes.len()).map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    stream
        .write_all(&len.to_le_bytes())
        .and_then(|()| stream.write_all(&bytes))
        .map_err(|_| ResponderBrokerError::TransportUnavailable)
}

#[cfg(unix)]
fn read_control_frame<T: for<'de> Deserialize<'de>>(
    stream: &mut std::os::unix::net::UnixStream,
) -> Result<T, ResponderBrokerError> {
    let mut len_buf = [0_u8; 4];
    stream
        .read_exact(&mut len_buf)
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    let len = usize::try_from(u32::from_le_bytes(len_buf))
        .map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    if len == 0 || len > RESPONDER_CONTROL_MAX_BYTES {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    let mut bytes = vec![0_u8; len];
    stream
        .read_exact(&mut bytes)
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| ResponderBrokerError::InvalidConfiguration)
}

#[cfg(unix)]
async fn write_control_frame_async<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> Result<(), ResponderBrokerError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    if bytes.len() > RESPONDER_CONTROL_MAX_BYTES {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    let len = u32::try_from(bytes.len()).map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    AsyncWriteExt::write_all(stream, &len.to_le_bytes())
        .await
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    AsyncWriteExt::write_all(stream, &bytes)
        .await
        .map_err(|_| ResponderBrokerError::TransportUnavailable)
}

#[cfg(unix)]
async fn read_control_frame_async<T: for<'de> Deserialize<'de>>(
    cx: &Cx,
    stream: &mut UnixStream,
) -> Result<T, ResponderBrokerError> {
    checkpoint(cx, "responder control frame")?;
    let mut len_buf = [0_u8; 4];
    AsyncReadExt::read_exact(stream, &mut len_buf)
        .await
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    let len = usize::try_from(u32::from_le_bytes(len_buf))
        .map_err(|_| ResponderBrokerError::InvalidConfiguration)?;
    if len == 0 || len > RESPONDER_CONTROL_MAX_BYTES {
        return Err(ResponderBrokerError::InvalidConfiguration);
    }
    let mut bytes = vec![0_u8; len];
    AsyncReadExt::read_exact(stream, &mut bytes)
        .await
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| ResponderBrokerError::InvalidConfiguration)
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
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(ResponderBrokerError::WhoIsUnverified);
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            let tokens = value
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>();
            if tokens.len() != 1 || !tokens[0].eq_ignore_ascii_case("chunked") {
                return Err(ResponderBrokerError::WhoIsUnverified);
            }
            chunked = true;
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
    if chunked {
        if content_length.is_some() {
            return Err(ResponderBrokerError::WhoIsUnverified);
        }
        return decode_local_api_chunked_body(body);
    }
    if content_length.is_some_and(|expected| expected != body.len()) {
        return Err(ResponderBrokerError::WhoIsUnverified);
    }
    Ok(body.to_vec())
}

#[cfg(unix)]
fn decode_local_api_chunked_body(body: &[u8]) -> Result<Vec<u8>, ResponderBrokerError> {
    let mut decoded = Vec::new();
    let mut cursor = 0_usize;
    loop {
        let remaining = body
            .get(cursor..)
            .ok_or(ResponderBrokerError::WhoIsUnverified)?;
        let line_end = remaining
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or(ResponderBrokerError::WhoIsUnverified)?;
        let size_line = std::str::from_utf8(&remaining[..line_end])
            .map_err(|_| ResponderBrokerError::WhoIsUnverified)?;
        let size_token = size_line.split(';').next().unwrap_or_default().trim();
        let chunk_size = usize::from_str_radix(size_token, 16)
            .map_err(|_| ResponderBrokerError::WhoIsUnverified)?;
        cursor = cursor
            .checked_add(line_end + 2)
            .ok_or(ResponderBrokerError::WhoIsUnverified)?;
        if chunk_size == 0 {
            return Ok(decoded);
        }
        let chunk_end = cursor
            .checked_add(chunk_size)
            .ok_or(ResponderBrokerError::WhoIsUnverified)?;
        let chunk = body
            .get(cursor..chunk_end)
            .ok_or(ResponderBrokerError::WhoIsUnverified)?;
        if decoded.len().saturating_add(chunk.len()) > LOCAL_API_MAX_RESPONSE_BYTES {
            return Err(ResponderBrokerError::WhoIsUnverified);
        }
        decoded.extend_from_slice(chunk);
        if body.get(chunk_end..chunk_end + 2) != Some(b"\r\n") {
            return Err(ResponderBrokerError::WhoIsUnverified);
        }
        cursor = chunk_end + 2;
    }
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

async fn read_asupersync_framed(
    cx: &Cx,
    stream: &mut TcpStream,
    duration: Duration,
) -> Result<Vec<u8>, ResponderBrokerError> {
    checkpoint(cx, "bootstrap frame read")?;
    let now = wall_now();
    let effective = cx
        .budget()
        .remaining_duration(now)
        .map_or(duration, |remaining| remaining.min(duration));
    if effective.is_zero() {
        return Err(ResponderBrokerError::Cancelled);
    }
    let _ambient = Cx::set_current(Some(cx.clone()));
    let bytes = match timeout(now, effective, async {
        let mut prefix = [0_u8; 4];
        AsyncReadExt::read_exact(stream, &mut prefix).await?;
        let length = usize::try_from(u32::from_be_bytes(prefix)).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "bootstrap length does not fit usize",
            )
        })?;
        if length == 0 || length > BOOTSTRAP_MAX_ENVELOPE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bootstrap length {length} exceeds {BOOTSTRAP_MAX_ENVELOPE_BYTES}-byte cap"
                ),
            ));
        }
        let mut body = vec![0_u8; length];
        AsyncReadExt::read_exact(stream, &mut body).await?;
        Ok(body)
    })
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {
            return Err(ResponderBrokerError::Cancelled);
        }
        Ok(Err(_)) | Err(_) => return Err(ResponderBrokerError::TransportUnavailable),
    };
    checkpoint(cx, "bootstrap frame read")?;
    Ok(bytes)
}

async fn write_asupersync_framed(
    cx: &Cx,
    stream: &mut TcpStream,
    duration: Duration,
    bytes: &[u8],
) -> Result<(), ResponderBrokerError> {
    if bytes.len() > BOOTSTRAP_MAX_ENVELOPE_BYTES {
        return Err(ResponderBrokerError::TransportUnavailable);
    }
    checkpoint(cx, "bootstrap frame write")?;
    let prefix = u32::try_from(bytes.len())
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?
        .to_be_bytes();
    let now = wall_now();
    let effective = cx
        .budget()
        .remaining_duration(now)
        .map_or(duration, |remaining| remaining.min(duration));
    if effective.is_zero() {
        return Err(ResponderBrokerError::Cancelled);
    }
    let _ambient = Cx::set_current(Some(cx.clone()));
    match timeout(now, effective, async {
        AsyncWriteExt::write_all(stream, &prefix).await?;
        AsyncWriteExt::write_all(stream, bytes).await?;
        AsyncWriteExt::flush(stream).await
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {
            return Err(ResponderBrokerError::Cancelled);
        }
        Ok(Err(_)) | Err(_) => return Err(ResponderBrokerError::TransportUnavailable),
    }
    checkpoint(cx, "bootstrap frame write")?;
    Ok(())
}

async fn write_bootstrap_decline(
    cx: &Cx,
    stream: &mut TcpStream,
    duration: Duration,
    code: &str,
) -> Result<(), ResponderBrokerError> {
    let decline = BootstrapDeclineV1 {
        schema: BOOTSTRAP_DECLINE_SCHEMA_V1.to_owned(),
        code: code.to_owned(),
    };
    let bytes =
        serde_json::to_vec(&decline).map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    write_asupersync_framed(cx, stream, duration, &bytes).await
}

async fn answer_bootstrap_hello(
    cx: &Cx,
    stream: &mut TcpStream,
    routes: &ResponderRouteRegistry,
    first_packet: &[u8],
) -> Result<(), ResponderBrokerError> {
    checkpoint(cx, "bootstrap hello")?;
    let io_timeout = routes.limits.io_timeout;
    let envelope = match decode_envelope(first_packet) {
        Ok(envelope) => envelope,
        Err(error) => {
            return write_bootstrap_decline(cx, stream, io_timeout, error.decline_code()).await;
        }
    };
    if envelope.capability != BootstrapCapability::Hello {
        return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_unsupported_capability")
            .await;
    }
    let Some(request) = parse_hello_request(&envelope.payload) else {
        return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
    };
    let workspace_ids = routes.workspace_ids();
    let lists = routes
        .first_workspace_path()
        .and_then(|path| load_workspace_lists(path).ok())
        .unwrap_or_default();
    let advertised_tags = vec![EE_MESH_SERVICE_TAG.to_owned()];
    let capabilities = vec!["hello".to_owned()];
    let context = ResponderContext {
        mesh_enabled: true,
        tailscale_authenticated: true,
        shields_up: false,
        respond_mode: DiscoveryMode::from_env_respond(|_| {}),
        responder_node_key: &routes.responder_node_pubkey,
        responder_ee_version: env!("CARGO_PKG_VERSION"),
        responder_workspace_ids: &workspace_ids,
        responder_capabilities: &capabilities,
        responder_advertised_tags: &advertised_tags,
        respond_allowlist: &lists.respond_allowlist,
        denylist: &lists.denylist,
        rate_limited: false,
        elapsed_micros: 0,
    };
    let payload = match decide_hello_response(&request, &context) {
        HelloOutcome::Granted(response) => serde_json::to_value(response),
        HelloOutcome::Declined(error) => serde_json::to_value(error),
    }
    .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    let reply = encode_envelope(BootstrapCapability::Hello, payload)
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    write_asupersync_framed(cx, stream, io_timeout, &reply).await
}

async fn answer_bootstrap_join(
    cx: &Cx,
    stream: &mut TcpStream,
    routes: &ResponderRouteRegistry,
    first_packet: &[u8],
    joiner_addr: SocketAddr,
) -> Result<(), ResponderBrokerError> {
    checkpoint(cx, "bootstrap join")?;
    let io_timeout = routes.limits.io_timeout;
    let envelope = match decode_envelope(first_packet) {
        Ok(envelope) => envelope,
        Err(error) => {
            return write_bootstrap_decline(cx, stream, io_timeout, error.decline_code()).await;
        }
    };
    if envelope.capability != BootstrapCapability::Join {
        return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_unsupported_capability")
            .await;
    }
    let hello = match serde_json::from_value::<crate::mesh::team::TeamJoinHelloV1>(envelope.payload)
    {
        Ok(hello) if hello.schema == crate::mesh::team::TEAM_JOIN_HELLO_SCHEMA_V1 => hello,
        _ => {
            return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
        }
    };
    let Some(route) = routes.routes.values().next() else {
        return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
    };
    let Some(database_path) = route.database_path.clone() else {
        return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
    };
    let Ok(connection) = DbConnection::open_file(database_path) else {
        return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
    };
    let Ok(Some(invite)) = connection.get_team_pending_invite(&hello.invite_id) else {
        return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
    };
    let signer = match crate::mesh::origin_stream::Ed25519OriginSigner::load_or_create(
        &route.workspace_path,
        &invite.origin_node_id,
        &invite.created_at,
    ) {
        Ok(signer) => signer,
        Err(_) => {
            return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
        }
    };
    let challenge = match crate::mesh::team::sign_join_challenge(&signer, &invite, &hello) {
        Ok(challenge) => challenge,
        Err(_) => {
            return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
        }
    };
    let challenge_value =
        serde_json::to_value(&challenge).map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    let challenge_bytes = encode_envelope(BootstrapCapability::Join, challenge_value)
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    write_asupersync_framed(cx, stream, io_timeout, &challenge_bytes).await?;
    let prove_bytes = match read_asupersync_framed(cx, stream, io_timeout).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
        }
    };
    let prove_envelope = match decode_envelope(&prove_bytes) {
        Ok(envelope) => envelope,
        Err(_) => {
            return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
        }
    };
    let prove = match serde_json::from_value::<crate::mesh::team::TeamJoinProveV1>(
        prove_envelope.payload,
    ) {
        Ok(prove) if prove.schema == crate::mesh::team::TEAM_JOIN_PROVE_SCHEMA_V1 => prove,
        _ => {
            return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
        }
    };
    if prove.invite_id != hello.invite_id
        || prove.joiner_nonce != hello.joiner_nonce
        || prove.inviter_nonce != challenge.inviter_nonce
    {
        return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
    }
    let redeemed_at = chrono::Utc::now().to_rfc3339();
    let mut granted = match crate::mesh::team::redeem_team_invite(
        &connection,
        &prove.invite_id,
        &prove.secret,
        &redeemed_at,
    ) {
        Ok(granted) => granted,
        Err(_) => {
            return write_bootstrap_decline(cx, stream, io_timeout, "bootstrap_malformed").await;
        }
    };
    let pair = crate::mesh::team::derive_team_pair_key(
        &prove.secret,
        &granted.team_id,
        &prove.invite_id,
        &prove.joiner_node_id,
        &granted.origin_node_id,
        &prove.joiner_nonce,
        &prove.inviter_nonce,
    );
    granted.pair_confirmation = crate::mesh::team::pair_confirmation(&pair);
    if let Ok(workspaces) = connection.list_workspaces()
        && let Some(workspace) = workspaces.first()
    {
        let _ = crate::mesh::team::record_inviter_side_join_member(
            &connection,
            &workspace.id,
            &granted,
            &prove.joiner_node_id,
            &prove.joiner_display_name,
            &redeemed_at,
            Some(hello.joiner_verifying_key.as_str()).filter(|key| key.len() == 64),
        );
        let _ = crate::mesh::team::enroll_joiner_from_accept(
            &connection,
            &workspace.id,
            &granted.team_id,
            &prove.joiner_node_id,
            &prove.joiner_display_name,
            joiner_addr,
            &hello.joiner_workspace_id,
            hello.joiner_hello_port,
            &redeemed_at,
        );
    }
    let _ = crate::mesh::team::persist_pair_key(
        &route.workspace_path,
        &granted.team_id,
        &prove.joiner_node_id,
        &pair,
        &redeemed_at,
    );
    let payload =
        serde_json::to_value(&granted).map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    let reply = encode_envelope(BootstrapCapability::Join, payload)
        .map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    write_asupersync_framed(cx, stream, io_timeout, &reply).await
}

async fn answer_sync_round(
    cx: &Cx,
    stream: &mut TcpStream,
    routes: &ResponderRouteRegistry,
) -> Result<(), ResponderBrokerError> {
    let io_timeout = routes.limits.io_timeout;
    let Ok(bytes) = read_asupersync_framed(cx, stream, io_timeout).await else {
        return Ok(());
    };
    let Some(request) = parse_sync_round_request(&bytes) else {
        return Ok(());
    };
    let response = load_sync_round_response(routes, request.range_start_seq, request.max_events);
    let reply =
        serde_json::to_vec(&response).map_err(|_| ResponderBrokerError::TransportUnavailable)?;
    write_asupersync_framed(cx, stream, io_timeout, &reply).await
}

fn admit_authenticated_capability(
    _peer_id: &str,
    capability: &FrameCapability,
    payload: &serde_json::Value,
    peer_state: &mut crate::mesh::admission::MeshPeerAdmissionState,
) -> Result<crate::mesh::admission::MeshAdmissionDecision, ResponderBrokerError> {
    use crate::mesh::admission::{
        MeshAdmissionRequestKind, admit_authenticated_mesh_capability_with_state,
        record_authenticated_admission,
    };
    let kind = match capability {
        FrameCapability::BodyFetch => MeshAdmissionRequestKind::BodyFetch,
        FrameCapability::EventFetch => MeshAdmissionRequestKind::EventBatch,
        FrameCapability::Summary => MeshAdmissionRequestKind::TipAdvertise,
        FrameCapability::Extension(name) if name == "identity_attest" => {
            MeshAdmissionRequestKind::Hello
        }
        _ => MeshAdmissionRequestKind::Hello,
    };
    let payload_bytes = u64::try_from(payload.to_string().len()).unwrap_or(u64::MAX);
    let now_epoch_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    let event_count = if kind == MeshAdmissionRequestKind::EventBatch {
        serde_json::to_vec(payload)
            .ok()
            .and_then(|bytes| parse_sync_round_request(&bytes))
            .map(|parsed| parsed.max_events)
            .unwrap_or(0)
    } else {
        0
    };
    let body_fetch_bytes = if kind == MeshAdmissionRequestKind::BodyFetch {
        payload_bytes
    } else {
        0
    };
    let decision = admit_authenticated_mesh_capability_with_state(
        peer_state,
        kind,
        payload_bytes,
        event_count,
        body_fetch_bytes,
        now_epoch_ms,
    );
    record_authenticated_admission(peer_state, &decision, now_epoch_ms);
    if !decision.allowed() {
        return Err(ResponderBrokerError::AdmissionLimited);
    }
    Ok(decision)
}

async fn serve_authenticated_sync_round(
    cx: &Cx,
    session: &mut AuthenticatedTransportSession,
    routes: &ResponderRouteRegistry,
    admission: &Arc<Mutex<AdmissionState>>,
) -> Result<(), ResponderBrokerError> {
    let request = loop {
        let Some(request) = session
            .receive_request(cx)
            .await
            .map_err(ResponderBrokerError::Session)?
        else {
            return Ok(());
        };
        if matches!(
            request.capability,
            FrameCapability::Summary | FrameCapability::EventFetch | FrameCapability::BodyFetch
        ) || matches!(
            &request.capability,
            FrameCapability::Extension(name) if name == "identity_attest"
        ) {
            break request;
        }
    };
    let peer_id = session.binding().initiator_node_id.clone();
    {
        let mut state = admission
            .lock()
            .map_err(|_| ResponderBrokerError::AdmissionLimited)?;
        let peer = state
            .peers
            .entry(peer_id.clone())
            .or_insert_with(|| crate::mesh::admission::MeshPeerAdmissionState::new(&peer_id));
        admit_authenticated_capability(&peer_id, &request.capability, &request.payload, peer)?;
    }
    let payload = if matches!(
        &request.capability,
        FrameCapability::Extension(name) if name == "identity_attest"
    ) {
        let processed = session
            .process_request(cx, &request, async {
                load_identity_attest_response(routes, &request.payload)
            })
            .await
            .map_err(ResponderBrokerError::Session)?;
        serde_json::to_value(&processed).map_err(|_| ResponderBrokerError::TransportUnavailable)?
    } else if request.capability == FrameCapability::BodyFetch {
        let processed = session
            .process_request(cx, &request, async {
                load_body_fetch_response(routes, &request.payload)
            })
            .await
            .map_err(ResponderBrokerError::Session)?;
        serde_json::to_value(&processed).map_err(|_| ResponderBrokerError::TransportUnavailable)?
    } else {
        let (range_start_seq, max_events) = serde_json::to_vec(&request.payload)
            .ok()
            .and_then(|bytes| parse_sync_round_request(&bytes))
            .map(|parsed| (parsed.range_start_seq, parsed.max_events))
            .unwrap_or((0, 512));
        let processed = session
            .process_request(cx, &request, async {
                load_sync_round_response(routes, range_start_seq, max_events)
            })
            .await
            .map_err(ResponderBrokerError::Session)?;
        serde_json::to_value(&processed).map_err(|_| ResponderBrokerError::TransportUnavailable)?
    };
    let result = session
        .send_response(
            cx,
            SessionMessage {
                correlation_id: request.correlation_id,
                capability: request.capability,
                requested_budget_ms: request.requested_budget_ms,
                payload,
            },
        )
        .await
        .map_err(ResponderBrokerError::Session);
    if let Ok(mut state) = admission.lock()
        && let Some(peer) = state.peers.get_mut(&peer_id)
    {
        crate::mesh::admission::release_authenticated_admission(peer);
    }
    persist_authenticated_admission_snapshot(routes, admission);
    result
}

fn persist_authenticated_admission_snapshot(
    routes: &ResponderRouteRegistry,
    admission: &Arc<Mutex<AdmissionState>>,
) {
    let Some(database_path) = routes
        .routes
        .values()
        .find_map(|route| route.database_path.clone())
    else {
        return;
    };
    let Some(workspace_id) = routes.workspace_ids().into_iter().next() else {
        return;
    };
    let Ok(state) = admission.lock() else {
        return;
    };
    let peers = state.peers.values().cloned().collect::<Vec<_>>();
    drop(state);
    let Ok(connection) = DbConnection::open_file(database_path) else {
        return;
    };
    let _ = crate::mesh::team::persist_team_admission_states(
        &connection,
        &workspace_id,
        &peers,
        &chrono::Utc::now().to_rfc3339(),
    );
}

fn load_sync_round_response(
    routes: &ResponderRouteRegistry,
    range_start_seq: u64,
    max_events: u32,
) -> SyncRoundResponse {
    let workspace_id = routes
        .workspace_ids()
        .into_iter()
        .next()
        .unwrap_or_default();
    let (team_id, origin_node_id) = routes
        .routes
        .values()
        .next()
        .map(|route| {
            (
                route.expectations.team_id.clone(),
                route.expectations.responder_node_id.clone(),
            )
        })
        .unwrap_or_default();
    let mut events = Vec::new();
    if let Some(database_path) = routes
        .routes
        .values()
        .find_map(|route| route.database_path.clone())
        && let Ok(connection) = DbConnection::open_file_read_only(database_path)
    {
        let limit = max_events.max(1).min(512);
        if let Ok(rows) =
            connection.list_mesh_origin_events(&team_id, &origin_node_id, range_start_seq, limit)
        {
            events = rows
                .into_iter()
                .map(|row| {
                    let payload_json = crate::mesh::origin_stream::inbound_from_stored(&row)
                        .ok()
                        .and_then(|event| serde_json::to_string(&event).ok())
                        .unwrap_or(row.payload_json);
                    SyncRoundEvent {
                        origin_node_id: row.origin_node_id,
                        origin_workspace_id: workspace_id.clone(),
                        seq: row.seq,
                        event_hash: row.event_hash,
                        payload_json,
                    }
                })
                .collect();
        }
    }
    let last_seq = events.last().map_or(0, |event| event.seq);
    let tip_hash = events.last().map(|event| event.event_hash.clone());
    SyncRoundResponse {
        schema: SYNC_ROUND_SCHEMA_V1.to_owned(),
        tips: vec![SyncRoundTip {
            origin_node_id,
            origin_workspace_id: workspace_id,
            last_seq,
            tip_event_hash: tip_hash,
        }],
        events,
    }
}

fn load_identity_attest_response(
    routes: &ResponderRouteRegistry,
    payload: &serde_json::Value,
) -> crate::mesh::idp::IdentityAttestFrameV1 {
    let rejected = crate::mesh::idp::IdentityAttestFrameV1 {
        schema: crate::mesh::idp::IDENTITY_ATTEST_FRAME_SCHEMA_V1.to_owned(),
        team_id: String::new(),
        member_id: String::new(),
        subject: "rejected".to_owned(),
        email: None,
        matched_groups: Vec::new(),
        token_hash: "blake3:0000000000000000000000000000000000000000000000000000000000000000"
            .to_owned(),
        checked_at: String::new(),
    };
    let Some(database_path) = routes
        .routes
        .values()
        .find_map(|route| route.database_path.clone())
    else {
        return rejected;
    };
    let Ok(connection) = DbConnection::open_file(database_path) else {
        return rejected;
    };
    crate::mesh::team::apply_identity_attest_frame(&connection, payload).unwrap_or(rejected)
}

fn load_body_fetch_response(
    routes: &ResponderRouteRegistry,
    payload: &serde_json::Value,
) -> BodyFetchResponse {
    let request = serde_json::from_value::<BodyFetchRequest>(payload.clone()).ok();
    let key = request
        .as_ref()
        .map(|parsed| parsed.body_cache_key.as_str())
        .unwrap_or_default();
    if key.is_empty()
        || request
            .as_ref()
            .is_none_or(|parsed| parsed.schema != BODY_FETCH_REQUEST_SCHEMA_V1)
    {
        return BodyFetchResponse {
            schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
            body_cache_key: key.to_owned(),
            cache_status: "metadata_only".to_owned(),
            size_bytes: 0,
            body_hex: None,
            nonce_hex: None,
        };
    }
    let workspace_id = routes
        .workspace_ids()
        .into_iter()
        .next()
        .unwrap_or_default();
    let workspace_path = routes.first_workspace_path().map(Path::to_path_buf);
    let database_path = routes
        .routes
        .values()
        .find_map(|route| route.database_path.clone());
    let Some((workspace_path, database_path)) = workspace_path.zip(database_path) else {
        return BodyFetchResponse {
            schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
            body_cache_key: key.to_owned(),
            cache_status: "metadata_only".to_owned(),
            size_bytes: 0,
            body_hex: None,
            nonce_hex: None,
        };
    };
    let Ok(connection) = DbConnection::open_file_read_only(database_path) else {
        return BodyFetchResponse {
            schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
            body_cache_key: key.to_owned(),
            cache_status: "metadata_only".to_owned(),
            size_bytes: 0,
            body_hex: None,
            nonce_hex: None,
        };
    };
    if let Some(peer_id) = routes
        .routes
        .values()
        .next()
        .map(|route| route.peer_handle.clone())
        && !crate::mesh::team::body_lane_allows_fetch(&connection, &workspace_id, &peer_id)
    {
        return BodyFetchResponse {
            schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
            body_cache_key: key.to_owned(),
            cache_status: "metadata_only".to_owned(),
            size_bytes: 0,
            body_hex: None,
            nonce_hex: None,
        };
    }
    crate::mesh::team::fetch_local_team_body(&connection, &workspace_id, &workspace_path, key)
        .unwrap_or_else(|_| BodyFetchResponse {
            schema: BODY_FETCH_RESPONSE_SCHEMA_V1.to_owned(),
            body_cache_key: key.to_owned(),
            cache_status: "metadata_only".to_owned(),
            size_bytes: 0,
            body_hex: None,
            nonce_hex: None,
        })
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

    #[derive(Clone)]
    struct StaticLocalApi {
        status: LocalTailscaleStatus,
    }

    impl TailscaleLocalApi for StaticLocalApi {
        fn local_status<'a>(
            &'a self,
            _cx: &'a Cx,
        ) -> TailscaleLocalApiFuture<'a, LocalTailscaleStatus> {
            Box::pin(async move { Ok(self.status.clone()) })
        }

        fn verify_local_address<'a>(
            &'a self,
            _cx: &'a Cx,
            _address: SocketAddr,
        ) -> TailscaleLocalApiFuture<'a, LocalTailscaleIdentity> {
            Box::pin(async { Err(ResponderBrokerError::WhoIsUnverified) })
        }

        fn who_is<'a>(
            &'a self,
            _cx: &'a Cx,
            _source: SocketAddr,
        ) -> TailscaleLocalApiFuture<'a, WhoIsIdentity> {
            Box::pin(async { Err(ResponderBrokerError::WhoIsUnverified) })
        }
    }

    fn route(path: PathBuf, port: u16) -> RegisteredResponderRoute {
        RegisteredResponderRoute {
            workspace_path: path,
            database_path: None,
            peer_handle: "peer_0123456789abcdef0123456789abcdef".to_owned(),
            committed_port: port,
            expectations: ResponderExpectations {
                team_id: "team-a".to_owned(),
                tailnet_id: "tailnet-a".to_owned(),
                responder_node_id: "node-responder".to_owned(),
                responder_workspace_id: "workspace-responder".to_owned(),
                responder_stable_id: "stable-responder".to_owned(),
                initiator_node_id: "node_0123456789abcdef0123456789abcdef".to_owned(),
                initiator_stable_id: "stable-initiator".to_owned(),
                pair_key_generation: 1,
            },
            responder_node_pubkey: "nodekey:responder-current".to_owned(),
            peer_transport_key_generation: 1,
            grant_generation: 1,
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

        let mut guessable_handle = route(PathBuf::from("/tmp/ee-responder-broker-unit"), 41888);
        guessable_handle.peer_handle = "peer_7".to_owned();
        assert!(matches!(
            ResponderRouteRegistry::new([guessable_handle]),
            Err(ResponderBrokerError::InvalidConfiguration)
        ));
    }

    #[test]
    fn production_owner_rejects_loopback_status_before_any_listener_bind() {
        let registry = ResponderRouteRegistry::new([route(
            PathBuf::from("/tmp/ee-responder-owner-loopback-negative"),
            41888,
        )])
        .expect("valid local route registry");
        let result = crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
            ResponderBrokerOwner::start(
                &cx,
                StaticLocalApi {
                    status: LocalTailscaleStatus {
                        identity: LocalTailscaleIdentity {
                            stable_id: "stable-responder".to_owned(),
                            current_node_pubkey: "nodekey:responder-current".to_owned(),
                            tailnet_id: "tailnet-a".to_owned(),
                        },
                        addresses: vec![IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)],
                    },
                },
                registry,
                PreAuthAdmissionLimits::default(),
                Duration::from_secs(1),
            )
            .await
        });
        assert!(matches!(
            result,
            Ok(Err(ResponderBrokerError::TransportUnavailable))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn localapi_http_parser_accepts_bounded_chunked_json() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"ok\r\n7\r\n\":true}\r\n0\r\n\r\n";
        let body = parse_local_api_response(response).expect("decode bounded chunked response");
        assert_eq!(body, br#"{"ok":true}"#);
    }

    #[cfg(unix)]
    #[test]
    fn localapi_status_requires_running_backend_and_uses_complete_address_set() {
        let running = br#"{
            "BackendState":"Running",
            "TailscaleIPs":["100.64.0.1"],
            "Self":{
                "ID":"node-stable",
                "PublicKey":"nodekey:current",
                "TailscaleIPs":["fd7a:115c:a1e0::1"]
            },
            "CurrentTailnet":{"MagicDNSSuffix":"example.ts.net"}
        }"#;
        let status = parse_local_status(running).expect("running status");
        assert_eq!(
            status.addresses,
            vec![
                "100.64.0.1".parse::<IpAddr>().expect("v4"),
                "fd7a:115c:a1e0::1".parse::<IpAddr>().expect("v6"),
            ]
        );

        let stopped = br#"{
            "BackendState":"Stopped",
            "TailscaleIPs":["100.64.0.1"],
            "Self":{
                "ID":"node-stable",
                "PublicKey":"nodekey:current",
                "TailscaleIPs":["100.64.0.1"]
            },
            "CurrentTailnet":{"MagicDNSSuffix":"example.ts.net"}
        }"#;
        assert!(matches!(
            parse_local_status(stopped),
            Err(ResponderBrokerError::TransportUnavailable)
        ));
    }

    #[test]
    fn registry_multiplexes_same_target_for_distinct_initiator_stable_ids() {
        let path = PathBuf::from("/tmp/ee-responder-broker-multiplex-unit");
        let first = route(path.clone(), 41888);
        let mut second = route(path, 41888);
        second.peer_handle = "peer_fedcba9876543210fedcba9876543210".to_owned();
        second.expectations.initiator_node_id = "node_fedcba9876543210fedcba9876543210".to_owned();
        second.expectations.initiator_stable_id = "stable-initiator-b".to_owned();

        let registry = ResponderRouteRegistry::new([first, second]).expect(
            "distinct verified initiators may share one team, target, port, and generation",
        );
        assert_eq!(registry.route_count(), 2);
        let selected = registry
            .resolve(&UntrustedRouteSelectors {
                team_id: "team-a".to_owned(),
                responder_workspace_id: "workspace-responder".to_owned(),
                initiator_stable_id: "stable-initiator-b".to_owned(),
                pair_key_generation: 1,
            })
            .expect(
                "untrusted stable-id selector chooses only the local route later verified by WhoIs",
            );
        assert_eq!(
            selected.expectations.initiator_stable_id,
            "stable-initiator-b"
        );
    }

    #[test]
    fn durable_owner_authority_comparison_detects_generation_refreshes() {
        let path = PathBuf::from("/tmp/ee-responder-broker-refresh-unit");
        let current = ResponderRouteRegistry::new([route(path.clone(), 41888)])
            .expect("current route registry");
        let mut refreshed_route = route(path, 41888);
        refreshed_route.expectations.pair_key_generation = 2;
        refreshed_route.peer_transport_key_generation = 2;
        refreshed_route.grant_generation = 2;
        let refreshed =
            ResponderRouteRegistry::new([refreshed_route]).expect("refreshed route registry");
        assert!(!same_route_authority(&current, &refreshed));
        assert!(same_route_authority(&refreshed, &refreshed));
    }

    #[test]
    fn route_pair_key_generation_must_match_durable_record() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open key store");
        store
            .store_pair_key(
                "peer_0123456789abcdef0123456789abcdef",
                PairKeyClass::Current,
                std::num::NonZeroU64::new(2).expect("nonzero generation"),
                &crate::mesh::key_store::SecretBytes::new([7; 32]),
                "2026-08-08T00:00:00Z",
                false,
            )
            .expect("store generation-two key");
        let route = route(workspace.path().to_path_buf(), 41888);

        let error = load_route_pair_key(&route)
            .expect_err("route generation one must not consume durable generation two");
        assert!(matches!(error, ResponderBrokerError::PairingRequired));
    }

    #[test]
    fn authenticated_admission_rejects_oversized_body_fetch() {
        let oversize = serde_json::json!({
            "schema": "ee.mesh.body_fetch.request.v1",
            "bodyCacheKey": "x".repeat(600 * 1024),
        });
        let mut noisy = crate::mesh::admission::MeshPeerAdmissionState::new("peer-noisy");
        let error = admit_authenticated_capability(
            "peer-noisy",
            &FrameCapability::BodyFetch,
            &oversize,
            &mut noisy,
        )
        .expect_err("oversized body fetch");
        assert!(matches!(error, ResponderBrokerError::AdmissionLimited));
        assert!(noisy.malformed_frame_count > 0);
        let mut ok_peer = crate::mesh::admission::MeshPeerAdmissionState::new("peer-ok");
        let ok = admit_authenticated_capability(
            "peer-ok",
            &FrameCapability::Summary,
            &serde_json::json!({"schema": "ee.mesh.sync_round.v1"}),
            &mut ok_peer,
        )
        .expect("summary stays inside budget");
        assert!(ok.allowed());
        assert_eq!(ok_peer.in_flight_requests, 1);
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

    #[test]
    fn default_control_socket_follows_daemon_partition() {
        assert_eq!(
            default_responder_control_socket_path_with(
                |key| match key {
                    "XDG_RUNTIME_DIR" => Some("/run/user/1000".into()),
                    _ => None,
                },
                1000,
            ),
            PathBuf::from("/run/user/1000/ee/mesh-responder.sock")
        );
        assert_eq!(
            default_responder_control_socket_path_with(
                |key| match key {
                    "XDG_RUNTIME_DIR" => Some("/tmp".into()),
                    "TMPDIR" => Some("/var/tmp".into()),
                    _ => None,
                },
                501,
            ),
            PathBuf::from("/var/tmp/ee-501/mesh-responder.sock")
        );
    }

    #[test]
    fn status_control_request_skips_path_and_identity_gates() {
        assert!(validate_control_request(&responder_control_status_request()).is_ok());
    }

    #[test]
    fn control_request_rejects_network_shaped_fields_and_relative_paths() {
        let request = ResponderControlRequest {
            schema: RESPONDER_CONTROL_SCHEMA_V1.to_owned(),
            op: ResponderControlOp::Register,
            nonce: "0123456789abcdef".to_owned(),
            workspace_id: "wsp_test".to_owned(),
            team_id: "team_test".to_owned(),
            responder_node_id: "node_0123456789abcdef0123456789abcdef".to_owned(),
            workspace_path: PathBuf::from("relative/workspace"),
            database_path: PathBuf::from("relative/ee.db"),
            peer_handles: vec!["peer_0123456789abcdef0123456789abcdef".to_owned()],
            committed_port: 41888,
        };
        assert!(matches!(
            validate_control_request(&request),
            Err(ResponderBrokerError::InvalidConfiguration)
        ));
        let mut unknown = request.clone();
        unknown.schema = "ee.mesh.event.v1".to_owned();
        unknown.workspace_path = PathBuf::from("/tmp/ee-control-unit");
        unknown.database_path = PathBuf::from("/tmp/ee-control-unit/.ee/ee.db");
        assert!(matches!(
            validate_control_request(&unknown),
            Err(ResponderBrokerError::InvalidConfiguration)
        ));
    }
}
