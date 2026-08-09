//! Supplemental wire/session conformance for the bounded T2.2 broker.
//!
//! The responder binds and accepts through the real Asupersync TCP listener,
//! calls a fake-but-wire-real Tailscale LocalAPI Unix socket for status and
//! exact-source WhoIs, opens a preprovisioned hardened key store, and delegates
//! the accepted stream to the public T2.1 authenticated-session path.
//! Fake LocalAPI coverage is not evidence of real Tailscale authority; the
//! opt-in test below exercises status and WhoIs against a real local daemon.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::future::Future;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::num::NonZeroU64;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use asupersync::{CancelKind, Cx};
use ee::config::MeshLane;
use ee::db::{
    CreateWorkspaceInput, DbConnection, MeshLaneGrantMutationInput, MeshLaneGrantTargetAdapter,
    UpsertMeshPeerInput,
};
use ee::mesh::key_store::{MeshKeyStore, PairKeyClass, SecretBytes};
use ee::mesh::peer::{
    MeshPeerCapabilityProfile, MeshPeerEndpoint, MeshPeerEnrollInput, MeshPeerHandshake,
    build_peer_origin_node_id, enroll_peer,
};
use ee::mesh::responder_broker::{
    DurableResponderRegistration, MESH_KEY_STORE_UNAVAILABLE_CODE, PreAuthAdmissionLimits,
    RegisteredResponderRoute, ResponderBroker, ResponderBrokerError, ResponderBrokerOwner,
    ResponderBrokerState, ResponderRouteRegistry, TailscaleLocalApi, TailscaleLocalApiClient,
    resolve_durable_registration,
};
use ee::mesh::transport_session::{
    HandshakeObservations, InitiatorSessionConfig, ResponderExpectations, SessionBinding,
    SessionCapabilities, SessionChannelLimits, connect_authenticated_session,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, String>;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_API_TIMEOUT: Duration = Duration::from_secs(2);
const PEER_HANDLE: &str = "peer_0123456789abcdef0123456789abcdef";
const CREATED_AT: &str = "2026-08-08T00:00:00Z";

fn pair_key() -> SecretBytes {
    SecretBytes::new([0x5a; 32])
}

fn session_limits() -> SessionChannelLimits {
    SessionChannelLimits {
        connect_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_secs(2),
        max_requested_budget_ms: 1_000,
        max_authenticated_frames: 128,
        max_authenticated_bytes: 1024 * 1024,
    }
}

fn expectations() -> ResponderExpectations {
    ResponderExpectations {
        team_id: "team-loopback".to_owned(),
        tailnet_id: "tailnet-loopback".to_owned(),
        responder_node_id: "node-responder".to_owned(),
        responder_workspace_id: "workspace-responder".to_owned(),
        responder_stable_id: "stable-responder".to_owned(),
        initiator_node_id: "node-initiator".to_owned(),
        initiator_stable_id: "stable-initiator".to_owned(),
        pair_key_generation: 7,
    }
}

fn route(workspace_path: PathBuf, port: u16) -> RegisteredResponderRoute {
    RegisteredResponderRoute {
        workspace_path,
        database_path: None,
        peer_handle: PEER_HANDLE.to_owned(),
        committed_port: port,
        expectations: expectations(),
        responder_node_pubkey: "nodekey:responder-current".to_owned(),
        peer_transport_key_generation: 1,
        grant_generation: 1,
        capabilities: SessionCapabilities::base(),
        limits: session_limits(),
    }
}

fn initiator_config() -> InitiatorSessionConfig {
    InitiatorSessionConfig {
        local_address: "127.0.0.2:0".parse().expect("loopback source"),
        binding: SessionBinding {
            team_id: "team-loopback".to_owned(),
            tailnet_id: "tailnet-loopback".to_owned(),
            initiator_node_id: "node-initiator".to_owned(),
            responder_node_id: "node-responder".to_owned(),
            initiator_workspace_id: "workspace-initiator".to_owned(),
            responder_workspace_id: "workspace-responder".to_owned(),
            initiator_stable_id: "stable-initiator".to_owned(),
            responder_stable_id: "stable-responder".to_owned(),
            session_id: "replaced-by-connect".to_owned(),
        },
        pair_key: pair_key(),
        pair_key_generation: 7,
        observations: HandshakeObservations {
            initiator_node_pubkey: "nodekey:initiator-current".to_owned(),
            responder_node_pubkey: "nodekey:responder-current".to_owned(),
        },
        capabilities: SessionCapabilities::base(),
        limits: session_limits(),
    }
}

fn available_nonprivileged_port() -> TestResult<u16> {
    let listener = StdTcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("select loopback port: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("read loopback port: {error}"))?
        .port();
    if port < 1024 {
        return Err(format!("OS selected privileged test port {port}"));
    }
    Ok(port)
}

fn run_runtime<F, Fut, T>(operation: F) -> TestResult<T>
where
    F: FnOnce(Cx) -> Fut,
    Fut: Future<Output = TestResult<T>>,
{
    ee::core::run_cli_with_cx(TEST_TIMEOUT, operation)
        .map_err(|error| format!("asupersync runtime failed: {error}"))?
}

struct FakeLocalApi {
    socket_path: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    join: thread::JoinHandle<TestResult>,
}

impl FakeLocalApi {
    fn spawn(dir: &Path, expected_requests: usize) -> TestResult<Self> {
        Self::spawn_with_tailnets(dir, expected_requests, None, Some("tailnet-loopback"))
    }

    fn spawn_with_tailnets(
        dir: &Path,
        expected_requests: usize,
        self_tailnet: Option<&str>,
        current_tailnet: Option<&str>,
    ) -> TestResult<Self> {
        let socket_path = dir.join("tailscaled.sock");
        let listener = UnixListener::bind(&socket_path)
            .map_err(|error| format!("bind fake localapi: {error}"))?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&requests);
        let self_tailnet = self_tailnet.map(str::to_owned);
        let current_tailnet = current_tailnet.map(str::to_owned);
        let join = thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener
                    .accept()
                    .map_err(|error| format!("accept fake localapi: {error}"))?;
                let request = read_http_request(&mut stream)?;
                observed
                    .lock()
                    .map_err(|_| "fake localapi request log poisoned".to_owned())?
                    .push(request.clone());
                let body = if request.starts_with("GET /localapi/v0/status ") {
                    json!({
                        "BackendState": "Running",
                        "TailscaleIPs": ["127.0.0.1"],
                        "Self": {
                            "ID": "stable-responder",
                            "PublicKey": "nodekey:responder-current",
                            "TailscaleIPs": ["127.0.0.1"],
                            "Tailnet": self_tailnet
                        },
                        "CurrentTailnet": {
                            "MagicDNSSuffix": current_tailnet
                        }
                    })
                } else if request.starts_with("GET /localapi/v0/whois?") {
                    json!({
                        "Node": {
                            "StableID": "stable-initiator",
                            "Key": "nodekey:initiator-current",
                            "Addresses": ["127.0.0.2/32"]
                        },
                        "UserProfile": {
                            "ID": 1,
                            "LoginName": "member@example.test",
                            "DisplayName": "Loopback Member"
                        }
                    })
                } else {
                    return Err(format!("unexpected fake localapi request: {request}"));
                };
                write_http_response(
                    &mut stream,
                    &serde_json::to_vec(&body).map_err(|e| e.to_string())?,
                )?;
            }
            Ok(())
        });
        Ok(Self {
            socket_path,
            requests,
            join,
        })
    }

    fn finish(self) -> TestResult<Vec<String>> {
        self.join
            .join()
            .map_err(|_| "fake localapi thread panicked".to_owned())??;
        Arc::try_unwrap(self.requests)
            .map_err(|_| "fake localapi request log still shared".to_owned())?
            .into_inner()
            .map_err(|_| "fake localapi request log poisoned".to_owned())
    }
}

fn read_http_request(stream: &mut std::os::unix::net::UnixStream) -> TestResult<String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| format!("set fake localapi read timeout: {error}"))?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    while bytes.len() <= 16 * 1024 {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("read fake localapi request: {error}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if bytes.len() > 16 * 1024 {
        return Err("fake localapi request exceeded bound".to_owned());
    }
    String::from_utf8(bytes).map_err(|_| "fake localapi request was not utf8".to_owned())
}

fn write_http_response(stream: &mut std::os::unix::net::UnixStream, body: &[u8]) -> TestResult {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|error| format!("write fake localapi response: {error}"))
}

#[test]
fn real_tailscale_localapi_binds_status_and_whois_to_kernel_source() -> TestResult {
    if std::env::var("EE_E2E_REAL_TAILSCALE").ok().as_deref() != Some("1") {
        return Ok(());
    }
    run_runtime(|cx| async move {
        let client = TailscaleLocalApiClient::discover(LOCAL_API_TIMEOUT)
            .ok_or("real tailscaled LocalAPI socket was not found")?;
        let status = client
            .local_status(&cx)
            .await
            .map_err(|error| format!("real LocalAPI status failed: {error}"))?;
        let bind_ip = status
            .addresses
            .first()
            .copied()
            .ok_or("real LocalAPI status returned no Tailscale address")?;
        let listener = StdTcpListener::bind(SocketAddr::new(bind_ip, 0))
            .map_err(|error| format!("bind real Tailscale self-test listener: {error}"))?;
        let target = listener
            .local_addr()
            .map_err(|error| format!("read real Tailscale listener address: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("bound real Tailscale listener polling: {error}"))?;
        let connector = thread::spawn(move || {
            std::net::TcpStream::connect_timeout(&target, Duration::from_secs(2))
                .map_err(|error| format!("connect real Tailscale self-test: {error}"))
        });
        let accept_deadline = std::time::Instant::now() + Duration::from_secs(2);
        let (accepted, kernel_source) = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= accept_deadline {
                        return Err("real Tailscale self-test accept timed out".to_owned());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Err(format!("accept real Tailscale self-test: {error}"));
                }
            }
        };
        drop(accepted);
        drop(
            connector
                .join()
                .map_err(|_| "real Tailscale connector panicked".to_owned())??,
        );
        if kernel_source.ip() != bind_ip {
            return Err(format!(
                "kernel source {} did not use LocalAPI Tailscale address {bind_ip}",
                kernel_source.ip()
            ));
        }
        let who_is = client
            .who_is(&cx, kernel_source)
            .await
            .map_err(|error| format!("real LocalAPI WhoIs failed: {error}"))?;
        if who_is.stable_id != status.identity.stable_id
            || who_is.current_node_pubkey != status.identity.current_node_pubkey
        {
            return Err("real WhoIs identity did not match real LocalAPI self status".to_owned());
        }
        let workspace = tempfile::tempdir()
            .map_err(|error| format!("create real responder workspace: {error}"))?;
        let workspace_path = workspace
            .path()
            .canonicalize()
            .map_err(|error| format!("canonicalize real responder workspace: {error}"))?;
        let ee_dir = workspace_path.join(".ee");
        std::fs::create_dir(&ee_dir)
            .map_err(|error| format!("create real responder state dir: {error}"))?;
        let database_path = ee_dir.join("ee.db");
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("open real responder database: {error}"))?;
        connection
            .migrate()
            .map_err(|error| format!("migrate real responder database: {error}"))?;
        let workspace_id = "wsp_real_responder_0000000001";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("real responder proof".to_owned()),
                },
            )
            .map_err(|error| format!("insert real responder workspace: {error}"))?;
        let peer_report = enroll_peer(MeshPeerEnrollInput {
            workspace_id: workspace_id.to_owned(),
            alias: "real-tailscale-self".to_owned(),
            endpoint: MeshPeerEndpoint {
                tailscale_node_key: status.identity.current_node_pubkey.clone(),
                tailnet_id: status.identity.tailnet_id.clone(),
                tailnet_display_name: None,
                endpoint: bind_ip.to_string(),
                magic_dns_name: None,
            },
            capability_profile: MeshPeerCapabilityProfile::MetadataOnly,
            handshake: MeshPeerHandshake::granted(
                "real-responder-proof",
                "1.0",
                status.identity.current_node_pubkey.clone(),
                vec!["mesh:metadata".to_owned()],
            ),
            public_key_fingerprint: "blake3:real-responder-proof".to_owned(),
            now: CREATED_AT.to_owned(),
            explicit_human_consent: true,
        });
        let peer = peer_report
            .peer
            .ok_or_else(|| format!("compose real responder peer: {}", peer_report.message))?;
        let origin_node_id = build_peer_origin_node_id(&peer.endpoint.tailscale_node_key);
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: workspace_id.to_owned(),
                peer_id: peer.peer_id.clone(),
                origin_node_id: origin_node_id.clone(),
                display_name: Some(peer.alias.clone()),
                policy_summary_json: Some(
                    serde_json::to_string(&peer)
                        .map_err(|error| format!("encode real responder peer: {error}"))?,
                ),
                enabled: true,
                last_seen_at: Some(CREATED_AT.to_owned()),
            })
            .map_err(|error| format!("persist real responder peer: {error}"))?;
        let target_adapter = MeshLaneGrantTargetAdapter::new(&peer.peer_id, origin_node_id);
        connection
            .apply_mesh_lane_grant_with_effect(
                &MeshLaneGrantMutationInput {
                    workspace_id: workspace_id.to_owned(),
                    peer_id: peer.peer_id.clone(),
                    target_adapter,
                    material_lane: MeshLane::Metadata,
                    expected_generation: 0,
                    approval_config_digest: Some(format!("blake3:{}", "a".repeat(64))),
                    updated_at: Some(CREATED_AT.to_owned()),
                },
                |_| Ok::<(), String>(()),
            )
            .map_err(|error| format!("grant real responder lane: {error}"))?;
        let store = MeshKeyStore::open_or_create(&workspace_path)
            .map_err(|error| format!("create real responder key store: {error}"))?;
        store
            .store_pair_key(
                &peer.peer_id,
                PairKeyClass::Current,
                NonZeroU64::new(1).expect("real proof generation is nonzero"),
                &pair_key(),
                CREATED_AT,
                false,
            )
            .map_err(|error| format!("store real responder pair key: {error}"))?;

        let port = available_nonprivileged_port()?;
        let resolved = resolve_durable_registration(
            &cx,
            &client,
            &connection,
            &DurableResponderRegistration {
                workspace_path,
                database_path: database_path
                    .canonicalize()
                    .map_err(|error| format!("canonicalize real responder database: {error}"))?,
                workspace_id: workspace_id.to_owned(),
                team_id: "team-real-responder-proof".to_owned(),
                responder_node_id: "node-real-responder-proof".to_owned(),
                peer_handle: peer.peer_id,
                committed_port: port,
                capabilities: SessionCapabilities::base(),
                limits: session_limits(),
            },
        )
        .await
        .map_err(|error| format!("resolve real durable responder route: {error}"))?;
        let registry = ResponderRouteRegistry::new([resolved.route])
            .map_err(|error| format!("register real responder route: {error}"))?;
        let mut owner = ResponderBrokerOwner::start(
            &cx,
            client,
            registry,
            PreAuthAdmissionLimits::default(),
            Duration::from_millis(250),
        )
        .await
        .map_err(|error| format!("start real responder owner: {error}"))?;
        let expected_addresses = status
            .addresses
            .iter()
            .map(|ip| SocketAddr::new(*ip, port))
            .collect::<Vec<_>>();
        if owner.bound_addresses() != expected_addresses {
            return Err(format!(
                "real owner bound {:?}, expected full LocalAPI set {expected_addresses:?}",
                owner.bound_addresses()
            ));
        }
        owner.shutdown();
        Ok(())
    })
}

#[test]
fn production_broker_rejects_status_tailnet_mismatch_before_listen_or_pair_auth() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn_with_tailnets(
        local_api_dir.path(),
        1,
        Some("tailnet-wrong"),
        Some("tailnet-loopback"),
    )?;
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    let requests = run_runtime(|cx| async move {
        let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
        let error = match ResponderBroker::bind(
            &cx,
            bind_address,
            client,
            registry,
            PreAuthAdmissionLimits::default(),
        )
        .await
        {
            Ok(mut broker) => {
                broker.shutdown();
                return Err("wrong-tailnet status allowed a responder broker bind".to_owned());
            }
            Err(error) => error,
        };
        if !matches!(error, ResponderBrokerError::WhoIsUnverified) {
            return Err(format!("unexpected wrong-tailnet error: {error:?}"));
        }
        fake.finish()
    })?;
    if requests.len() != 1 || !requests[0].starts_with("GET /localapi/v0/status ") {
        return Err(format!(
            "wrong-tailnet bind performed work beyond status attestation: {requests:?}"
        ));
    }
    if workspace.path().join(".ee/keys/mesh").exists() {
        return Err("wrong-tailnet bind reached or created pair-key storage".to_owned());
    }
    let rebound = StdTcpListener::bind(bind_address)
        .map_err(|error| format!("wrong-tailnet bind left broker socket bound: {error}"))?;
    drop(rebound);
    Ok(())
}

#[test]
fn production_broker_path_attests_exact_source_and_authenticates_preexisting_key() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let store = MeshKeyStore::open_or_create(workspace.path())
        .map_err(|error| format!("preprovision key store: {error}"))?;
    store
        .store_pair_key(
            PEER_HANDLE,
            PairKeyClass::Current,
            NonZeroU64::new(7).expect("test generation is nonzero"),
            &pair_key(),
            CREATED_AT,
            false,
        )
        .map_err(|error| format!("preprovision pair key: {error}"))?;

    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 2)?;
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        run_runtime(|cx| async move {
            let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
            let mut broker = ResponderBroker::bind(
                &cx,
                bind_address,
                client,
                registry,
                PreAuthAdmissionLimits::default(),
            )
            .await
            .map_err(|error| error.to_string())?;
            address_tx
                .send(broker.local_addr())
                .map_err(|error| format!("publish broker address: {error}"))?;
            let mut session = broker
                .accept_authenticated(&cx)
                .await
                .map_err(|error| error.to_string())?;
            if session.binding().responder_workspace_id != "workspace-responder" {
                return Err("broker authenticated the wrong local route".to_owned());
            }
            session.close();
            let listening = broker.status();
            if listening.authenticated_sessions != 1
                || listening.application_hello_performed
                || listening.anti_entropy_performed
                || listening.synchronized
            {
                return Err(format!("broker status overstated work: {listening:?}"));
            }
            let audit = broker.recent_audit_events();
            if audit.len() != 1
                || audit[0].outcome != "accepted"
                || !audit[0].authenticated
                || !audit[0].route_selected
            {
                return Err(format!("unexpected broker audit: {audit:?}"));
            }
            broker.shutdown();
            if broker.status().state != ResponderBrokerState::Shutdown {
                return Err("broker shutdown status did not converge".to_owned());
            }
            fake.finish()
        })
    });
    let address = address_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config())
            .await
            .map_err(|error| error.to_string())?;
        session.close();
        Ok(())
    })?;
    let requests = server
        .join()
        .map_err(|_| "broker server thread panicked".to_owned())??;
    if requests.len() != 2 {
        return Err(format!(
            "expected status + WhoIs requests, got {requests:?}"
        ));
    }
    if requests[0]
        != "GET /localapi/v0/status HTTP/1.1\r\nHost: local-tailscaled.sock\r\nConnection: close\r\n\r\n"
    {
        return Err(format!("unexpected status request: {:?}", requests[0]));
    }
    let who_is_line = requests[1].lines().next().unwrap_or_default();
    let source_port = who_is_line
        .strip_prefix("GET /localapi/v0/whois?addr=127.0.0.2%3A")
        .and_then(|value| value.strip_suffix("&proto=tcp HTTP/1.1"))
        .and_then(|value| value.parse::<u16>().ok());
    if source_port.is_none_or(|port| port == 0) {
        return Err(format!(
            "WhoIs did not carry the kernel source address: {who_is_line}"
        ));
    }
    Ok(())
}

#[test]
fn inbound_missing_store_is_noncreating_and_rate_limited_fail_closed() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 2)?;
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let workspace_path = workspace.path().to_path_buf();
    let server = thread::spawn(move || {
        run_runtime(|cx| async move {
            let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
            let broker = ResponderBroker::bind(
                &cx,
                bind_address,
                client,
                registry,
                PreAuthAdmissionLimits {
                    max_source_per_window: 1,
                    max_global_per_window: 1,
                    ..PreAuthAdmissionLimits::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
            address_tx
                .send(broker.local_addr())
                .map_err(|error| format!("publish broker address: {error}"))?;
            let first = broker
                .accept_authenticated(&cx)
                .await
                .expect_err("missing store must reject inbound authentication");
            if first.code() != MESH_KEY_STORE_UNAVAILABLE_CODE {
                return Err(format!("unexpected missing-store error: {first:?}"));
            }
            let second = broker
                .accept_authenticated(&cx)
                .await
                .expect_err("second source attempt in the window must be rate-limited");
            if !matches!(second, ResponderBrokerError::AdmissionLimited) {
                return Err(format!("unexpected admission error: {second:?}"));
            }
            fake.finish()
        })
    });
    let address = address_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    for _ in 0..2 {
        let _ = run_runtime(|cx| async move {
            let _ = connect_authenticated_session(&cx, address, initiator_config()).await;
            Ok(())
        });
    }
    server
        .join()
        .map_err(|_| "broker server thread panicked".to_owned())??;
    if workspace_path.join(".ee/keys/mesh").exists() {
        return Err("inbound broker created a missing key store".to_owned());
    }
    Ok(())
}

#[test]
fn blocked_accept_observes_cancellation_then_shutdown() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 1)?;
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    run_runtime(|cx| async move {
        let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
        let mut broker = ResponderBroker::bind(
            &cx,
            bind_address,
            client,
            registry,
            PreAuthAdmissionLimits::default(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let cancel = cx.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel.cancel_with(CancelKind::Shutdown, Some("stop mesh responder"));
        });
        let error = broker
            .accept_authenticated(&cx)
            .await
            .expect_err("cancelled accept must stop");
        canceller
            .join()
            .map_err(|_| "accept canceller panicked".to_owned())?;
        if !matches!(error, ResponderBrokerError::Cancelled) {
            return Err(format!("unexpected cancelled-accept error: {error:?}"));
        }
        let status = broker.status();
        let audit = broker.recent_audit_events();
        if status.accepted_connections != 0
            || status.rejected_connections != 0
            || audit.len() != 1
            || audit[0].outcome != "cancelled"
            || audit[0].route_selected
            || audit[0].authenticated
        {
            return Err(format!(
                "cancelled accept was misreported as connection work: {status:?} {audit:?}"
            ));
        }
        broker.shutdown();
        fake.finish()?;
        Ok(())
    })
}

#[test]
fn mesh_transport_kill_switch_binds_no_broker_socket() -> TestResult {
    const CHILD_MARKER: &str = "EE_MESH_RESPONDER_KILL_SWITCH_CHILD";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let output = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .arg("--exact")
            .arg("mesh_transport_kill_switch_binds_no_broker_socket")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("EE_MESH_TRANSPORT_DISABLED", "1")
            .output()
            .map_err(|error| format!("spawn broker kill-switch proof child: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "broker kill-switch proof child failed\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        return Ok(());
    }

    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    run_runtime(|cx| async move {
        let client = TailscaleLocalApiClient::new(
            workspace
                .path()
                .join("tailscaled-must-not-be-contacted.sock"),
            LOCAL_API_TIMEOUT,
        );
        let error = match ResponderBroker::bind(
            &cx,
            bind_address,
            client,
            registry,
            PreAuthAdmissionLimits::default(),
        )
        .await
        {
            Ok(mut broker) => {
                broker.shutdown();
                return Err("kill switch allowed a responder broker bind".to_owned());
            }
            Err(error) => error,
        };
        if !matches!(
            error,
            ResponderBrokerError::Session(
                ee::mesh::transport_session::SessionChannelError::TransportDisabled
            )
        ) {
            return Err(format!("unexpected broker kill-switch error: {error:?}"));
        }
        Ok(())
    })?;
    let rebound = StdTcpListener::bind(bind_address)
        .map_err(|error| format!("kill switch left broker socket bound: {error}"))?;
    drop(rebound);
    Ok(())
}
