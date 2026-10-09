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

use std::future::{Future, poll_fn};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::num::NonZeroU64;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use asupersync::{CancelKind, Cx};
use ee::config::MeshLane;
use ee::db::{
    CreateMeshOriginEventInput, CreateWorkspaceInput, DbConnection, InsertTeamMemberInput,
    MeshLaneGrantMutationInput, MeshLaneGrantTargetAdapter, UpsertMeshBodyCacheMetadataInput,
    UpsertMeshPeerInput,
};
use ee::mesh::bootstrap_envelope::{
    BOOTSTRAP_DECLINE_SCHEMA_V1, BootstrapCapability, BootstrapDeclineV1, SyncRoundRequest,
    decode_envelope, encode_envelope, exchange_bootstrap_hello, exchange_live_mesh_round,
};
use ee::mesh::foreground_cli::{
    contact_authenticated_body_fetch, contact_authenticated_identity_attest,
    contact_authenticated_mesh_peer,
};
use ee::mesh::hello::{build_request, parse_hello_response, serialize_within_budget};
use ee::mesh::idp::{IDENTITY_ATTEST_FRAME_SCHEMA_V1, IdentityAttestFrameV1};
use ee::mesh::key_store::{MeshKeyStore, PairKeyClass, SecretBytes, SecureLocalDir};
use ee::mesh::peer::{
    MeshPeerCapabilityProfile, MeshPeerEndpoint, MeshPeerEnrollInput, MeshPeerHandshake,
    build_peer_origin_node_id, enroll_peer,
};
use ee::mesh::responder_broker::{
    DurableResponderRegistration, LocalTailscaleIdentity, LocalTailscaleStatus,
    MESH_KEY_STORE_UNAVAILABLE_CODE, PreAuthAdmissionLimits, RegisteredResponderRoute,
    ResponderBroker, ResponderBrokerError, ResponderBrokerOwner, ResponderBrokerState,
    ResponderControlOp, ResponderControlRequest, ResponderRouteRegistry, TailscaleLocalApi,
    TailscaleLocalApiClient, TailscaleLocalApiFuture, WhoIsIdentity,
    default_responder_control_socket_path, submit_responder_control_request,
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

fn is_random_node_principal(value: &str) -> bool {
    value.strip_prefix("node_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn sqlite_text_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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
        initiator_node_id: "node_0123456789abcdef0123456789abcdef".to_owned(),
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
            initiator_node_id: "node_0123456789abcdef0123456789abcdef".to_owned(),
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
    run_runtime_with(TEST_TIMEOUT, operation)
}

fn run_runtime_with<F, Fut, T>(budget: Duration, operation: F) -> TestResult<T>
where
    F: FnOnce(Cx) -> Fut,
    Fut: Future<Output = TestResult<T>>,
{
    ee::core::run_cli_with_cx(budget, operation)
        .map_err(|error| format!("asupersync runtime failed: {error}"))?
}

/// Keep the regression watchdog outside the runtime under test. An async
/// timeout around the old broken accept would itself re-poll the socket and
/// accidentally deliver the cancellation that the product failed to observe.
fn lifecycle_test_child(test_name: &str) -> TestResult<bool> {
    const CHILD_MARKER: &str = "EE_MESH_RESPONDER_LIFECYCLE_TEST_CHILD";
    if std::env::var(CHILD_MARKER).as_deref() == Ok(test_name) {
        return Ok(true);
    }
    let mut child = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, test_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("start lifecycle regression {test_name}: {error}"))?;
    let started = Instant::now();
    let watchdog = Duration::from_secs(45);
    loop {
        if child
            .try_wait()
            .map_err(|error| format!("poll lifecycle regression {test_name}: {error}"))?
            .is_some()
        {
            let output = child
                .wait_with_output()
                .map_err(|error| format!("join lifecycle regression {test_name}: {error}"))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !output.status.success()
                || !stdout.contains(&format!("test {test_name} ... ok"))
                || !stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
            {
                return Err(format!(
                    "lifecycle regression {test_name} did not pass exactly one test: {}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    stdout,
                    String::from_utf8_lossy(&output.stderr),
                ));
            }
            return Ok(false);
        }
        if started.elapsed() >= watchdog {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .map_err(|error| format!("reap timed-out lifecycle regression: {error}"))?;
            return Err(format!(
                "lifecycle regression {test_name} did not complete within {watchdog:?}; \
                 stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

async fn notify_first_pending<F: Future>(future: F, ready: mpsc::SyncSender<()>) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut ready = Some(ready);
    poll_fn(|cx| {
        let result = future.as_mut().poll(cx);
        if result.is_pending()
            && let Some(ready) = ready.take()
        {
            let _ = ready.send(());
        }
        result
    })
    .await
}

/// The owner normally refuses loopback Tailscale addresses. This documented
/// LocalAPI seam permits the wire-real fixture without weakening production
/// address validation. WhoIs may be delayed and a status failure explicitly
/// injected; successful authority reads still use the real API client.
struct LoopbackLocalApi {
    client: TailscaleLocalApiClient,
    who_is_delay: Duration,
    status_failure: Option<Arc<LocalApiStatusFailure>>,
}

#[derive(Default)]
struct LocalApiStatusFailure {
    requested: AtomicBool,
    observed: AtomicBool,
}

impl TailscaleLocalApi for LoopbackLocalApi {
    fn local_status<'a>(&'a self, cx: &'a Cx) -> TailscaleLocalApiFuture<'a, LocalTailscaleStatus> {
        if let Some(failure) = &self.status_failure
            && failure.requested.load(Ordering::Acquire)
        {
            failure.observed.store(true, Ordering::Release);
            return Box::pin(async { Err(ResponderBrokerError::WhoIsUnavailable) });
        }
        self.client.local_status(cx)
    }

    fn verify_local_address<'a>(
        &'a self,
        cx: &'a Cx,
        address: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, LocalTailscaleIdentity> {
        self.client.verify_local_address(cx, address)
    }

    fn who_is<'a>(
        &'a self,
        cx: &'a Cx,
        source: SocketAddr,
    ) -> TailscaleLocalApiFuture<'a, WhoIsIdentity> {
        Box::pin(async move {
            if !self.who_is_delay.is_zero() {
                asupersync::time::sleep(cx.now(), self.who_is_delay).await;
            }
            self.client.who_is(cx, source).await
        })
    }

    fn allows_loopback_bind(&self) -> bool {
        true
    }
}

struct FakeLocalApi {
    socket_path: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    join: thread::JoinHandle<TestResult>,
    stop: Option<Arc<AtomicBool>>,
}

impl FakeLocalApi {
    fn spawn(dir: &Path, expected_requests: usize) -> TestResult<Self> {
        Self::spawn_with_tailnets(dir, expected_requests, None, Some("tailnet-loopback"))
    }

    fn spawn_until_stopped(dir: &Path) -> TestResult<Self> {
        Self::spawn_wire(dir, None, None, Some("tailnet-loopback"))
    }

    fn spawn_with_tailnets(
        dir: &Path,
        expected_requests: usize,
        self_tailnet: Option<&str>,
        current_tailnet: Option<&str>,
    ) -> TestResult<Self> {
        Self::spawn_wire(dir, Some(expected_requests), self_tailnet, current_tailnet)
    }

    fn spawn_wire(
        dir: &Path,
        expected_requests: Option<usize>,
        self_tailnet: Option<&str>,
        current_tailnet: Option<&str>,
    ) -> TestResult<Self> {
        let socket_path = dir.join("tailscaled.sock");
        let listener = UnixListener::bind(&socket_path)
            .map_err(|error| format!("bind fake localapi: {error}"))?;
        listener
            .set_nonblocking(expected_requests.is_none())
            .map_err(|error| format!("set fake localapi accept mode: {error}"))?;
        let stop = expected_requests
            .is_none()
            .then(|| Arc::new(AtomicBool::new(false)));
        let server_stop = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&requests);
        let self_tailnet = self_tailnet.map(str::to_owned);
        let current_tailnet = current_tailnet.map(str::to_owned);
        let join = thread::spawn(move || {
            let mut served = 0;
            while expected_requests.is_none_or(|expected| served < expected)
                && !server_stop
                    .as_ref()
                    .is_some_and(|stop| stop.load(Ordering::Acquire))
            {
                let (mut stream, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error)
                        if expected_requests.is_none()
                            && error.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(format!("accept fake localapi: {error}")),
                };
                stream
                    .set_nonblocking(false)
                    .map_err(|error| format!("set fake localapi stream mode: {error}"))?;
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
                served += 1;
            }
            Ok(())
        });
        Ok(Self {
            socket_path,
            requests,
            join,
            stop,
        })
    }

    fn finish(self) -> TestResult<Vec<String>> {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Release);
        }
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
                stable_node_id: None,
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
        let target_adapter = MeshLaneGrantTargetAdapter::new(&peer.peer_id, origin_node_id.clone());
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
        connection
            .execute_raw(&format!(
                "UPDATE mesh_peers
                    SET transport_tailnet_id = {},
                        transport_stable_node_id = {},
                        transport_current_node_pubkey = {},
                        transport_key_generation = 1
                  WHERE workspace_id = {} AND peer_id = {}",
                sqlite_text_literal(&status.identity.tailnet_id),
                sqlite_text_literal(&who_is.stable_id),
                sqlite_text_literal(&who_is.current_node_pubkey),
                sqlite_text_literal(workspace_id),
                sqlite_text_literal(&peer.peer_id),
            ))
            .map_err(|error| format!("plant pre-random bound peer state: {error}"))?;
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
        let peer_id = peer.peer_id.clone();
        let registration = DurableResponderRegistration {
            workspace_path,
            database_path: database_path
                .canonicalize()
                .map_err(|error| format!("canonicalize real responder database: {error}"))?,
            workspace_id: workspace_id.to_owned(),
            team_id: "team-real-responder-proof".to_owned(),
            responder_node_id: "node-real-responder-proof".to_owned(),
            peer_handle: peer_id.clone(),
            committed_port: port,
            capabilities: SessionCapabilities::base(),
            limits: session_limits(),
        };
        let mut owner = ResponderBrokerOwner::start_durable(
            &cx,
            client,
            vec![registration],
            PreAuthAdmissionLimits::default(),
            Duration::from_millis(250),
        )
        .await
        .map_err(|error| format!("start real responder owner: {error}"))?;
        let migrated_peer = connection
            .get_mesh_peer(workspace_id, &peer_id)
            .map_err(|error| format!("reload migrated real responder peer: {error}"))?
            .ok_or_else(|| "migrated real responder peer disappeared".to_owned())?;
        if migrated_peer.origin_node_id == origin_node_id
            || !is_random_node_principal(&migrated_peer.origin_node_id)
        {
            return Err("durable owner did not heal the already-bound legacy principal through real LocalAPI".to_owned());
        }
        let migrated_grant = connection
            .get_mesh_lane_grant_state(workspace_id, &peer_id)
            .map_err(|error| format!("reload migrated real responder grant: {error}"))?
            .ok_or_else(|| "migrated real responder grant disappeared".to_owned())?;
        if migrated_grant.target_adapter.origin_node_id != migrated_peer.origin_node_id
            || migrated_grant.grant_generation != 1
        {
            return Err(
                "durable owner did not migrate the already-bound grant target exactly once"
                    .to_owned(),
            );
        }
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
        if owner.route_generations(&peer_id) != Some((1, 1, 1)) {
            return Err(format!(
                "real durable owner started with unexpected route generations: {:?}",
                owner.route_generations(&peer_id)
            ));
        }
        let current_peer = connection
            .get_mesh_peer(workspace_id, &peer_id)
            .map_err(|error| format!("reload real responder peer: {error}"))?
            .ok_or_else(|| "real responder peer disappeared".to_owned())?;
        connection
            .revoke_mesh_lane_with_effect(
                &MeshLaneGrantMutationInput {
                    workspace_id: workspace_id.to_owned(),
                    peer_id: peer_id.clone(),
                    target_adapter: MeshLaneGrantTargetAdapter::new(
                        &peer_id,
                        current_peer.origin_node_id,
                    ),
                    material_lane: MeshLane::Metadata,
                    expected_generation: 1,
                    approval_config_digest: None,
                    updated_at: Some("2026-08-09T00:02:00Z".to_owned()),
                },
                |_| Ok::<(), String>(()),
            )
            .map_err(|error| format!("revoke real responder lane: {error}"))?;
        store
            .store_pair_key(
                &peer_id,
                PairKeyClass::Current,
                NonZeroU64::new(2).expect("real refresh generation is nonzero"),
                &pair_key(),
                "2026-08-09T00:02:00Z",
                true,
            )
            .map_err(|error| format!("rotate real responder pair key: {error}"))?;
        owner
            .reconcile(&cx)
            .await
            .map_err(|error| format!("refresh real durable responder authority: {error}"))?;
        if owner.route_generations(&peer_id) != Some((2, 1, 2)) {
            return Err(format!(
                "real durable owner did not refresh pair/grant generations: {:?}",
                owner.route_generations(&peer_id)
            ));
        }
        owner.shutdown();
        Ok(())
    })
}

#[test]
fn durable_registration_rejects_cross_workspace_database_and_key_store_mixtures() -> TestResult {
    let workspace_a_dir = tempfile::tempdir().map_err(|error| format!("workspace a: {error}"))?;
    let workspace_b_dir = tempfile::tempdir().map_err(|error| format!("workspace b: {error}"))?;
    let workspace_a = workspace_a_dir
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace a: {error}"))?;
    let workspace_b = workspace_b_dir
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace b: {error}"))?;
    std::fs::create_dir(workspace_a.join(".ee"))
        .map_err(|error| format!("create workspace a state dir: {error}"))?;
    std::fs::create_dir(workspace_b.join(".ee"))
        .map_err(|error| format!("create workspace b state dir: {error}"))?;
    std::fs::set_permissions(
        workspace_a.join(".ee"),
        std::fs::Permissions::from_mode(0o700),
    )
    .map_err(|error| format!("secure workspace a state dir: {error}"))?;
    std::fs::set_permissions(
        workspace_b.join(".ee"),
        std::fs::Permissions::from_mode(0o700),
    )
    .map_err(|error| format!("secure workspace b state dir: {error}"))?;
    let database_a = workspace_a.join(".ee/ee.db");
    let database_b = workspace_b.join(".ee/ee.db");
    let connection_a = DbConnection::open_file(&database_a)
        .map_err(|error| format!("open workspace a database: {error}"))?;
    connection_a
        .migrate()
        .map_err(|error| format!("migrate workspace a database: {error}"))?;
    connection_a
        .insert_workspace(
            "wsp_crossworkspacea00000000000",
            &CreateWorkspaceInput {
                path: workspace_a.display().to_string(),
                name: Some("workspace a".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace a row: {error}"))?;
    let connection_b = DbConnection::open_file(&database_b)
        .map_err(|error| format!("open workspace b database: {error}"))?;
    connection_b
        .migrate()
        .map_err(|error| format!("migrate workspace b database: {error}"))?;
    connection_b
        .insert_workspace(
            "wsp_crossworkspaceb00000000000",
            &CreateWorkspaceInput {
                path: workspace_b.display().to_string(),
                name: Some("workspace b".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace b row: {error}"))?;
    MeshKeyStore::open_or_create(&workspace_b)
        .map_err(|error| format!("open wrong-workspace key store: {error}"))?
        .store_pair_key(
            PEER_HANDLE,
            PairKeyClass::Current,
            NonZeroU64::new(1).expect("test generation is nonzero"),
            &pair_key(),
            CREATED_AT,
            false,
        )
        .map_err(|error| format!("seed wrong-workspace pair key: {error}"))?;
    let database_a = database_a
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace a database: {error}"))?;
    let database_b = database_b
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace b database: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 0)?;
    let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
    run_runtime(|cx| async move {
        let base = DurableResponderRegistration {
            workspace_path: workspace_a.clone(),
            database_path: database_a.clone(),
            workspace_id: "wsp_crossworkspacea00000000000".to_owned(),
            team_id: "team-cross-workspace".to_owned(),
            responder_node_id: "node-cross-workspace".to_owned(),
            peer_handle: PEER_HANDLE.to_owned(),
            committed_port: 41888,
            capabilities: SessionCapabilities::base(),
            limits: session_limits(),
        };
        let wrong_workspace_path = DurableResponderRegistration {
            workspace_path: workspace_b,
            ..base.clone()
        };
        let wrong_path_error = ee::mesh::responder_broker::resolve_durable_registration(
            &cx,
            &client,
            &connection_a,
            &wrong_workspace_path,
        )
        .await
        .expect_err("DB workspace row must reject another workspace's key-store root");
        if !matches!(wrong_path_error, ResponderBrokerError::InvalidConfiguration) {
            return Err(format!(
                "cross-workspace key-store mixture returned {wrong_path_error:?}"
            ));
        }
        let wrong_database_path = DurableResponderRegistration {
            database_path: database_b,
            ..base
        };
        let wrong_database_error = ee::mesh::responder_broker::resolve_durable_registration(
            &cx,
            &client,
            &connection_a,
            &wrong_database_path,
        )
        .await
        .expect_err("live DB connection must match the registered database path");
        if !matches!(
            wrong_database_error,
            ResponderBrokerError::InvalidConfiguration
        ) {
            return Err(format!(
                "cross-workspace database mixture returned {wrong_database_error:?}"
            ));
        }
        Ok(())
    })?;
    let requests = fake.finish()?;
    if !requests.is_empty() {
        return Err(format!(
            "invalid local workspace mixtures reached LocalAPI: {requests:?}"
        ));
    }
    Ok(())
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
    if !lifecycle_test_child(
        "production_broker_path_attests_exact_source_and_authenticates_preexisting_key",
    )? {
        return Ok(());
    }
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
    let (pending_tx, pending_rx) = mpsc::sync_channel(1);
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
            let mut session = notify_first_pending(broker.accept_authenticated(&cx), pending_tx)
                .await
                .map_err(|error| error.to_string())?;
            if session.binding().responder_workspace_id != "workspace-responder" {
                return Err("broker authenticated the wrong local route".to_owned());
            }
            session.close();
            let listening = broker.status();
            if listening.accepted_connections != 1
                || listening.authenticated_sessions != 1
                || listening.rejected_connections != 0
                || listening.preauth_inflight != 0
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
    pending_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| format!("wait for the idle broker accept: {error}"))?;
    // Cross several cancellation-check slices before a peer exists. The
    // one-shot API must keep waiting, without admitting or rejecting anything.
    thread::sleep(Duration::from_millis(250));
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
    if !lifecycle_test_child("blocked_accept_observes_cancellation_then_shutdown")? {
        return Ok(());
    }
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
        let (pending_tx, pending_rx) = mpsc::sync_channel(1);
        let canceller = thread::spawn(move || {
            pending_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| format!("accept never became pending: {error}"))?;
            let cancelled_at = Instant::now();
            cancel.cancel_with(CancelKind::Shutdown, Some("stop mesh responder"));
            Ok::<_, String>(cancelled_at)
        });
        let error = notify_first_pending(broker.accept_authenticated(&cx), pending_tx)
            .await
            .expect_err("cancelled accept must stop");
        let cancelled_at = canceller
            .join()
            .map_err(|_| "accept canceller panicked".to_owned())??;
        if cancelled_at.elapsed() >= Duration::from_secs(1) {
            return Err(format!(
                "idle accept did not observe cancellation promptly: {:?}",
                cancelled_at.elapsed()
            ));
        }
        if !matches!(error, ResponderBrokerError::Cancelled) {
            return Err(format!("unexpected cancelled-accept error: {error:?}"));
        }
        let status = broker.status();
        let audit = broker.recent_audit_events();
        if status.accepted_connections != 0
            || status.rejected_connections != 0
            || status.preauth_inflight != 0
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
        let rebound = StdTcpListener::bind(bind_address)
            .map_err(|error| format!("cancelled broker retained its listener: {error}"))?;
        drop(rebound);
        fake.finish()?;
        Ok(())
    })
}

#[test]
fn blocked_accept_observes_request_deadline_without_a_peer() -> TestResult {
    if !lifecycle_test_child("blocked_accept_observes_request_deadline_without_a_peer")? {
        return Ok(());
    }
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 1)?;
    let port = available_nonprivileged_port()?;
    let bind_address = SocketAddr::from(([127, 0, 0, 1], port));
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    let runtime = ee::core::build_cli_runtime()
        .map_err(|error| format!("build deadline regression runtime: {error}"))?;
    let setup_cx = runtime.request_cx_with_budget(asupersync::Budget::INFINITE);
    runtime.block_on(async {
        let _ambient = Cx::set_current(Some(setup_cx.clone()));
        let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
        let mut broker = ResponderBroker::bind(
            &setup_cx,
            bind_address,
            client,
            registry,
            PreAuthAdmissionLimits::default(),
        )
        .await
        .map_err(|error| error.to_string())?;
        // Mint the bounded context after setup, using the same production
        // runtime drivers. A test-only Cx without a reactor would hide the bug.
        let cx =
            runtime.request_cx_with_budget(setup_cx.budget_for_timeout(Duration::from_millis(175)));
        let started = Instant::now();
        let error = broker
            .accept_authenticated(&cx)
            .await
            .expect_err("idle accept must respect the request deadline");
        let elapsed = started.elapsed();
        if !matches!(error, ResponderBrokerError::Cancelled)
            || elapsed < Duration::from_millis(100)
            || elapsed >= Duration::from_secs(1)
            || cx.checkpoint().is_ok()
            || !cx.cancelled_by(CancelKind::Deadline)
        {
            return Err(format!(
                "idle deadline result was not bounded cancellation: {error:?}, {elapsed:?}"
            ));
        }
        let status = broker.status();
        let audit = broker.recent_audit_events();
        if status.accepted_connections != 0
            || status.rejected_connections != 0
            || status.preauth_inflight != 0
            || audit.len() != 1
            || audit[0].outcome != "cancelled"
            || audit[0].authenticated
            || audit[0].route_selected
        {
            return Err(format!(
                "deadline fabricated connection work: {status:?} {audit:?}"
            ));
        }
        broker.shutdown();
        let rebound = StdTcpListener::bind(bind_address)
            .map_err(|error| format!("deadline left the listener bound: {error}"))?;
        drop(rebound);
        fake.finish()?;
        Ok(())
    })
}

#[test]
fn cancelled_owner_releases_tcp_and_control_before_drop() -> TestResult {
    if !lifecycle_test_child("cancelled_owner_releases_tcp_and_control_before_drop")? {
        return Ok(());
    }
    for cancel_after_pending in [false, true] {
        let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
        let local_api_dir =
            tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
        let fake = FakeLocalApi::spawn_until_stopped(local_api_dir.path())?;
        let control_path = local_api_dir.path().join("responder.sock");
        let port = available_nonprivileged_port()?;
        let bind_address = SocketAddr::from(([127, 0, 0, 1], port));
        let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
            .map_err(|error| error.to_string())?;
        run_runtime(|cx| async move {
            let mut owner = ResponderBrokerOwner::start(
                &cx,
                LoopbackLocalApi {
                    client: TailscaleLocalApiClient::new(
                        fake.socket_path.clone(),
                        LOCAL_API_TIMEOUT,
                    ),
                    who_is_delay: Duration::ZERO,
                    status_failure: None,
                },
                registry,
                PreAuthAdmissionLimits::default(),
                Duration::from_secs(60),
            )
            .await
            .map_err(|error| format!("start loopback owner: {error}"))?;
            owner
                .listen_control(&control_path)
                .map_err(|error| error.to_string())?;
            let (result, cancelled_at) = if cancel_after_pending {
                let cancel = cx.clone();
                let (pending_tx, pending_rx) = mpsc::sync_channel(1);
                let canceller = thread::spawn(move || {
                    pending_rx
                        .recv_timeout(Duration::from_secs(3))
                        .map_err(|error| format!("owner never became pending: {error}"))?;
                    let cancelled_at = Instant::now();
                    cancel.cancel_with(CancelKind::Shutdown, Some("stop idle owner"));
                    Ok::<_, String>(cancelled_at)
                });
                let result =
                    notify_first_pending(owner.serve_until_cancelled(&cx), pending_tx).await;
                let cancelled_at = canceller
                    .join()
                    .map_err(|_| "owner canceller panicked".to_owned())??;
                (result, cancelled_at)
            } else {
                let cancelled_at = Instant::now();
                cx.cancel_with(CancelKind::Shutdown, Some("stop owner before serving"));
                (owner.serve_until_cancelled(&cx).await, cancelled_at)
            };
            if !matches!(result, Err(ResponderBrokerError::Cancelled))
                || cancelled_at.elapsed() >= Duration::from_secs(1)
                || !owner.bound_addresses().is_empty()
                || owner.control_socket_path().is_some()
            {
                return Err(format!(
                    "retained owner did not stop (after_pending={cancel_after_pending}): \
                     {result:?}, {:?}, {:?}, {:?}",
                    cancelled_at.elapsed(),
                    owner.bound_addresses(),
                    owner.control_socket_path()
                ));
            }
            let tcp = StdTcpListener::bind(bind_address)
                .map_err(|error| format!("stopped owner retained TCP listener: {error}"))?;
            let control = UnixListener::bind(&control_path)
                .map_err(|error| format!("stopped owner retained control publication: {error}"))?;
            // Both rebinds happened while owner is still alive.
            drop(tcp);
            drop(control);
            let requests = fake.finish()?;
            if requests.len() < 2 || requests.iter().any(|request| request.contains("/whois?")) {
                return Err(format!(
                    "idle owner unexpectedly performed peer work: {requests:?}"
                ));
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn owner_retains_control_during_localapi_failure_and_cancels_retry_sleep() -> TestResult {
    if !lifecycle_test_child(
        "owner_retains_control_during_localapi_failure_and_cancels_retry_sleep",
    )? {
        return Ok(());
    }
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn_until_stopped(local_api_dir.path())?;
    let control_path = local_api_dir.path().join("responder.sock");
    let port = available_nonprivileged_port()?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    let failure = Arc::new(LocalApiStatusFailure::default());
    run_runtime(|cx| async move {
        let mut owner = ResponderBrokerOwner::start(
            &cx,
            LoopbackLocalApi {
                client: TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT),
                who_is_delay: Duration::ZERO,
                status_failure: Some(Arc::clone(&failure)),
            },
            registry,
            PreAuthAdmissionLimits::default(),
            Duration::from_secs(60),
        )
        .await
        .map_err(|error| format!("start revalidation owner: {error}"))?;
        owner
            .listen_control(&control_path)
            .map_err(|error| error.to_string())?;
        failure.requested.store(true, Ordering::Release);
        let cancel = cx.clone();
        let control_at_failure = control_path.clone();
        let (pending_tx, pending_rx) = mpsc::sync_channel(1);
        let canceller = thread::spawn(move || {
            pending_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| format!("owner never entered the failed-status retry: {error}"))?;
            // A filesystem check cannot wake the parked network future. In
            // particular, do not connect a peer to make cancellation progress.
            let control_retained = control_at_failure.exists();
            let cancelled_at = Instant::now();
            cancel.cancel_with(CancelKind::Shutdown, Some("stop LocalAPI retry"));
            Ok::<_, String>((control_retained, cancelled_at))
        });
        let result = {
            let mut work = std::pin::pin!(owner.serve_until_cancelled(&cx));
            let mut pending_tx = Some(pending_tx);
            poll_fn(|task_cx| {
                let result = work.as_mut().poll(task_cx);
                if result.is_pending()
                    && failure.observed.load(Ordering::Acquire)
                    && let Some(pending_tx) = pending_tx.take()
                {
                    let _ = pending_tx.send(());
                }
                result
            })
            .await
        };
        let (control_retained, cancelled_at) = canceller
            .join()
            .map_err(|_| "retry canceller panicked".to_owned())??;
        if !control_retained
            || !matches!(result, Err(ResponderBrokerError::Cancelled))
            || cancelled_at.elapsed() >= Duration::from_secs(1)
            || !owner.bound_addresses().is_empty()
            || owner.control_socket_path().is_some()
        {
            return Err(format!(
                "failed-status retry lost control or did not stop promptly: \
                 control_retained={control_retained}, {result:?}, {:?}",
                cancelled_at.elapsed()
            ));
        }
        let tcp = StdTcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
            .map_err(|error| format!("failed-status owner retained TCP listener: {error}"))?;
        let control = UnixListener::bind(&control_path)
            .map_err(|error| format!("failed-status owner retained control endpoint: {error}"))?;
        drop(tcp);
        drop(control);
        fake.finish()?;
        Ok(())
    })
}

#[test]
fn abandoned_control_frame_does_not_block_owner_cancellation() -> TestResult {
    if !lifecycle_test_child("abandoned_control_frame_does_not_block_owner_cancellation")? {
        return Ok(());
    }
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn_until_stopped(local_api_dir.path())?;
    let control_path = local_api_dir.path().join("responder.sock");
    let port = available_nonprivileged_port()?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    run_runtime(|cx| async move {
        let mut owner = ResponderBrokerOwner::start(
            &cx,
            LoopbackLocalApi {
                client: TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT),
                who_is_delay: Duration::ZERO,
                status_failure: None,
            },
            registry,
            PreAuthAdmissionLimits::default(),
            Duration::from_secs(60),
        )
        .await
        .map_err(|error| format!("start control owner: {error}"))?;
        owner
            .listen_control(&control_path)
            .map_err(|error| error.to_string())?;
        let mut abandoned = std::os::unix::net::UnixStream::connect(&control_path)
            .map_err(|error| format!("connect abandoned control client: {error}"))?;
        abandoned
            .write_all(&[32, 0])
            .map_err(|error| format!("write partial control prefix: {error}"))?;
        let cancel = cx.clone();
        let (pending_tx, pending_rx) = mpsc::sync_channel(1);
        let canceller = thread::spawn(move || {
            pending_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| format!("partial control read never became pending: {error}"))?;
            let cancelled_at = Instant::now();
            cancel.cancel_with(CancelKind::Shutdown, Some("stop abandoned control client"));
            Ok::<_, String>(cancelled_at)
        });
        let result = notify_first_pending(owner.serve_until_cancelled(&cx), pending_tx).await;
        let cancelled_at = canceller
            .join()
            .map_err(|_| "control canceller panicked".to_owned())??;
        if !matches!(result, Err(ResponderBrokerError::Cancelled))
            || cancelled_at.elapsed() >= Duration::from_secs(1)
            || !owner.bound_addresses().is_empty()
            || owner.control_socket_path().is_some()
        {
            return Err(format!(
                "abandoned control client blocked shutdown: {result:?}"
            ));
        }
        abandoned
            .set_read_timeout(Some(Duration::from_secs(1)))
            .map_err(|error| format!("bound abandoned control read: {error}"))?;
        let mut byte = [0_u8; 1];
        let read = abandoned.read(&mut byte).map_err(|error| {
            format!("cancelled owner did not close the control client: {error}")
        })?;
        if read != 0 {
            return Err("cancelled owner fabricated a response to a partial request".to_owned());
        }
        let tcp = StdTcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
            .map_err(|error| format!("cancelled control owner retained TCP listener: {error}"))?;
        let control = UnixListener::bind(&control_path)
            .map_err(|error| format!("cancelled control owner retained its endpoint: {error}"))?;
        drop(tcp);
        drop(control);
        fake.finish()?;
        Ok(())
    })
}

#[test]
fn owner_control_preserves_partial_frames_across_cancellation_ticks() -> TestResult {
    if !lifecycle_test_child("owner_control_preserves_partial_frames_across_cancellation_ticks")? {
        return Ok(());
    }
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn_until_stopped(local_api_dir.path())?;
    let control_path = local_api_dir.path().join("responder.sock");
    let port = available_nonprivileged_port()?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    run_runtime(|cx| async move {
        let mut owner = ResponderBrokerOwner::start(
            &cx,
            LoopbackLocalApi {
                client: TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT),
                who_is_delay: Duration::ZERO,
                status_failure: None,
            },
            registry,
            PreAuthAdmissionLimits::default(),
            Duration::from_secs(60),
        )
        .await
        .map_err(|error| format!("start partial-frame owner: {error}"))?;
        owner
            .listen_control(&control_path)
            .map_err(|error| error.to_string())?;
        let request = ee::mesh::responder_broker::responder_control_status_request();
        let bytes = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        let prefix = u32::try_from(bytes.len())
            .map_err(|error| error.to_string())?
            .to_le_bytes();
        let mut stream = std::os::unix::net::UnixStream::connect(&control_path)
            .map_err(|error| format!("connect partial-frame client: {error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(|error| format!("bound partial-frame response: {error}"))?;
        stream
            .write_all(&prefix[..2])
            .map_err(|error| error.to_string())?;
        let cancel = cx.clone();
        let (pending_tx, pending_rx) = mpsc::sync_channel(1);
        let client = thread::spawn(move || {
            let result = (|| {
                pending_rx
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|error| format!("partial prefix never became pending: {error}"))?;
                thread::sleep(Duration::from_millis(150));
                stream
                    .write_all(&prefix[2..])
                    .map_err(|error| error.to_string())?;
                let split = bytes.len() / 2;
                stream
                    .write_all(&bytes[..split])
                    .map_err(|error| error.to_string())?;
                thread::sleep(Duration::from_millis(150));
                stream
                    .write_all(&bytes[split..])
                    .map_err(|error| error.to_string())?;
                let mut length = [0_u8; 4];
                stream
                    .read_exact(&mut length)
                    .map_err(|error| error.to_string())?;
                let length = usize::try_from(u32::from_le_bytes(length))
                    .map_err(|error| error.to_string())?;
                if length == 0 || length > ee::mesh::responder_broker::RESPONDER_CONTROL_MAX_BYTES {
                    return Err(format!(
                        "partial-frame response has invalid length: {length}"
                    ));
                }
                let mut reply = vec![0_u8; length];
                stream
                    .read_exact(&mut reply)
                    .map_err(|error| error.to_string())?;
                let reply: serde_json::Value =
                    serde_json::from_slice(&reply).map_err(|error| error.to_string())?;
                if reply["ok"] != true
                    || reply["nonce"] != request.nonce
                    || reply["registeredRoutes"] != 1
                {
                    return Err(format!(
                        "partial control frame lost state across timer slices: {reply}"
                    ));
                }
                Ok(())
            })();
            cancel.cancel_with(
                CancelKind::Shutdown,
                Some("partial control response complete"),
            );
            result
        });
        let result = notify_first_pending(owner.serve_until_cancelled(&cx), pending_tx).await;
        let client = client
            .join()
            .map_err(|_| "partial-frame client panicked".to_owned())?;
        if !matches!(result, Err(ResponderBrokerError::Cancelled)) {
            return Err(format!("partial-frame owner did not stop: {result:?}"));
        }
        client?;
        fake.finish()?;
        Ok(())
    })
}

#[test]
fn expired_control_operation_allows_the_next_status_request() -> TestResult {
    if !lifecycle_test_child("expired_control_operation_allows_the_next_status_request")? {
        return Ok(());
    }
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn_until_stopped(local_api_dir.path())?;
    let control_path = local_api_dir.path().join("responder.sock");
    let port = available_nonprivileged_port()?;
    let registry = ResponderRouteRegistry::new([route(workspace.path().to_path_buf(), port)])
        .map_err(|error| error.to_string())?;
    run_runtime(|cx| async move {
        let mut owner = ResponderBrokerOwner::start(
            &cx,
            LoopbackLocalApi {
                client: TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT),
                who_is_delay: Duration::ZERO,
                status_failure: None,
            },
            registry,
            PreAuthAdmissionLimits::default(),
            Duration::from_secs(60),
        )
        .await
        .map_err(|error| format!("start control-expiry owner: {error}"))?;
        owner
            .listen_control(&control_path)
            .map_err(|error| error.to_string())?;
        let mut abandoned = std::os::unix::net::UnixStream::connect(&control_path)
            .map_err(|error| format!("connect expiring control client: {error}"))?;
        abandoned
            .set_read_timeout(Some(Duration::from_secs(7)))
            .map_err(|error| format!("bound expired control read: {error}"))?;
        abandoned
            .write_all(&[32, 0])
            .map_err(|error| format!("write abandoned control prefix: {error}"))?;
        let cancel = cx.clone();
        let (pending_tx, pending_rx) = mpsc::sync_channel(1);
        let client = thread::spawn(move || {
            let result = (|| {
                pending_rx
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|error| format!("abandoned request never became pending: {error}"))?;
                let started = Instant::now();
                let mut byte = [0_u8; 1];
                let read = abandoned.read(&mut byte).map_err(|error| {
                    format!("owner did not expire abandoned control I/O: {error}")
                })?;
                if read != 0 || started.elapsed() < Duration::from_secs(4) {
                    return Err(format!(
                        "control request did not reach its own operation deadline: \
                         read={read}, elapsed={:?}",
                        started.elapsed()
                    ));
                }
                let request = ee::mesh::responder_broker::responder_control_status_request();
                let response = submit_responder_control_request(&control_path, &request)
                    .map_err(|error| format!("following status request failed: {error}"))?;
                if !response.ok
                    || response.nonce != request.nonce
                    || response.registered_routes != 1
                {
                    return Err(format!(
                        "owner did not serve the following status request: {response:?}"
                    ));
                }
                Ok(())
            })();
            cancel.cancel_with(
                CancelKind::Shutdown,
                Some("control expiry regression complete"),
            );
            result
        });
        let result = notify_first_pending(owner.serve_until_cancelled(&cx), pending_tx).await;
        let client = client
            .join()
            .map_err(|_| "control-expiry client panicked".to_owned())?;
        if !matches!(result, Err(ResponderBrokerError::Cancelled))
            || !owner.bound_addresses().is_empty()
            || owner.control_socket_path().is_some()
        {
            return Err(format!(
                "control-expiry owner did not stop cleanly: {result:?}"
            ));
        }
        client?;
        fake.finish()?;
        Ok(())
    })
}

#[test]
fn same_euid_control_register_rejects_relative_paths_before_connect() -> TestResult {
    let missing_socket = tempfile::tempdir()
        .map_err(|error| format!("temp control dir: {error}"))?
        .path()
        .join("missing-mesh-responder.sock");
    let relative = ResponderControlRequest {
        schema: ee::mesh::responder_broker::RESPONDER_CONTROL_SCHEMA_V1.to_owned(),
        op: ResponderControlOp::Register,
        nonce: "0123456789abcdef".to_owned(),
        workspace_id: "wsp_control".to_owned(),
        team_id: "team_control".to_owned(),
        responder_node_id: "node_0123456789abcdef0123456789abcdef".to_owned(),
        workspace_path: PathBuf::from("relative/workspace"),
        database_path: PathBuf::from("relative/ee.db"),
        peer_handles: vec![PEER_HANDLE.to_owned()],
        committed_port: 41888,
    };
    let error = submit_responder_control_request(&missing_socket, &relative)
        .expect_err("relative paths must fail before they reach the owner");
    if !matches!(error, ResponderBrokerError::InvalidConfiguration) {
        return Err(format!("unexpected relative-path control error: {error:?}"));
    }
    let _ = default_responder_control_socket_path();
    Ok(())
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

#[test]
fn production_broker_answers_unsigned_bootstrap_hello_on_the_same_port() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 1)?;
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
            let broker = ResponderBroker::bind(
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
            let error = match broker.accept_authenticated(&cx).await {
                Ok(_) => {
                    return Err(
                        "bootstrap hello was treated as an authenticated session".to_owned()
                    );
                }
                Err(error) => error,
            };
            if !matches!(error, ResponderBrokerError::BootstrapHelloAnswered) {
                return Err(format!("unexpected hello accept error: {error:?}"));
            }
            let status = broker.status();
            if !status.application_hello_performed || status.authenticated_sessions != 0 {
                return Err(format!("hello status overstated work: {status:?}"));
            }
            fake.finish()?;
            Ok(())
        })
    });
    let address = address_rx
        .recv_timeout(TEST_TIMEOUT)
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    let request = build_request(
        "hello-loopback",
        "nodekey:initiator",
        env!("CARGO_PKG_VERSION"),
        vec!["workspace-initiator".to_owned()],
        vec!["hello".to_owned()],
        Vec::new(),
    );
    let payload_bytes =
        serialize_within_budget(&request).map_err(|error| format!("serialize hello: {error}"))?;
    let payload = serde_json::from_slice(&payload_bytes)
        .map_err(|error| format!("hello payload json: {error}"))?;
    let reply = exchange_bootstrap_hello(address, Duration::from_secs(2), payload)
        .map_err(|error| format!("bootstrap hello exchange: {error}"))?;
    let response = parse_hello_response(&reply)
        .ok_or_else(|| format!("bootstrap hello reply was not a hello response: {reply}"))?;
    if response.responder_node_key != "nodekey:responder-current" {
        return Err(format!(
            "hello answered with unexpected responder identity: {}",
            response.responder_node_key
        ));
    }
    if !response.discovery_consent {
        return Err("hello response declined discovery consent".to_owned());
    }
    server
        .join()
        .map_err(|_| "hello broker thread panicked".to_owned())??;
    Ok(())
}

const LOOPBACK_ORIGIN_EVENT_ID: &str = "mesh_oevt_loopbackorigin000000000001";
const LOOPBACK_ORIGIN_EVENT_HASH: &str =
    "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

const LOOPBACK_RESPONDER_WORKSPACE_ID: &str = "wsp_responderloopback000000001";

fn origin_capable_expectations() -> ResponderExpectations {
    let mut expected = expectations();
    expected.team_id = "team_loopback".to_owned();
    expected.responder_node_id = "node_responder".to_owned();
    expected.responder_workspace_id = LOOPBACK_RESPONDER_WORKSPACE_ID.to_owned();
    expected
}

fn origin_capable_session_limits() -> SessionChannelLimits {
    SessionChannelLimits {
        connect_timeout: TEST_TIMEOUT,
        io_timeout: TEST_TIMEOUT,
        max_requested_budget_ms: 10_000,
        max_authenticated_frames: 128,
        max_authenticated_bytes: 1024 * 1024,
    }
}

fn origin_capable_initiator_config() -> InitiatorSessionConfig {
    let mut config = initiator_config();
    config.binding.team_id = "team_loopback".to_owned();
    config.binding.responder_node_id = "node_responder".to_owned();
    config.binding.responder_workspace_id = LOOPBACK_RESPONDER_WORKSPACE_ID.to_owned();
    config.limits = origin_capable_session_limits();
    config
}

fn seed_loopback_origin_event(database_path: &Path) -> TestResult {
    let connection = DbConnection::open_file(database_path)
        .map_err(|error| format!("open loopback origin db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate loopback origin db: {error}"))?;
    connection
        .append_mesh_origin_event(&CreateMeshOriginEventInput {
            event_id: LOOPBACK_ORIGIN_EVENT_ID.to_owned(),
            team_id: "team_loopback".to_owned(),
            origin_node_id: "node_responder".to_owned(),
            signing_key_generation: 1,
            seq: 0,
            prev_event_hash: None,
            event_hash: LOOPBACK_ORIGIN_EVENT_HASH.to_owned(),
            signature: "sig-loopback".to_owned(),
            payload_schema: "ee.mesh.memory_event.v1".to_owned(),
            payload_json: r#"{"operation":"create","logicalMemoryId":"mem_loopbackorigin0001"}"#
                .to_owned(),
            required_features_json: "[]".to_owned(),
            produced_at: CREATED_AT.to_owned(),
            body_nonce_hex: None,
        })
        .map_err(|error| format!("append loopback origin event: {error}"))?;
    Ok(())
}

fn seed_loopback_route_authority(database_path: &Path) -> TestResult {
    let connection = DbConnection::open_file(database_path)
        .map_err(|error| format!("open loopback authority db: {error}"))?;
    connection
        .insert_workspace(
            LOOPBACK_RESPONDER_WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: database_path
                    .parent()
                    .unwrap_or(database_path)
                    .display()
                    .to_string(),
                name: Some("loopback responder".to_owned()),
            },
        )
        .map_err(|error| format!("insert loopback workspace: {error}"))?;
    let initiator_node_id = origin_capable_expectations().initiator_node_id;
    connection
        .upsert_mesh_peer(&UpsertMeshPeerInput {
            workspace_id: LOOPBACK_RESPONDER_WORKSPACE_ID.to_owned(),
            peer_id: PEER_HANDLE.to_owned(),
            origin_node_id: initiator_node_id.clone(),
            display_name: Some("loopback-initiator".to_owned()),
            policy_summary_json: None,
            enabled: true,
            last_seen_at: Some(CREATED_AT.to_owned()),
        })
        .map_err(|error| format!("upsert loopback peer: {error}"))?;
    connection
        .apply_mesh_lane_grant_with_effect(
            &MeshLaneGrantMutationInput {
                workspace_id: LOOPBACK_RESPONDER_WORKSPACE_ID.to_owned(),
                peer_id: PEER_HANDLE.to_owned(),
                target_adapter: MeshLaneGrantTargetAdapter::new(PEER_HANDLE, initiator_node_id),
                material_lane: MeshLane::Metadata,
                expected_generation: 0,
                approval_config_digest: Some(format!("blake3:{}", "a".repeat(64))),
                updated_at: Some(CREATED_AT.to_owned()),
            },
            |_| Ok::<(), String>(()),
        )
        .map_err(|error| format!("grant loopback lane: {error}"))?;
    connection
        .execute_raw(&format!(
            "UPDATE mesh_peers
                SET transport_tailnet_id = {},
                    transport_stable_node_id = {},
                    transport_current_node_pubkey = {},
                    transport_key_generation = 1
              WHERE workspace_id = {} AND peer_id = {}",
            sqlite_text_literal("tailnet-loopback"),
            sqlite_text_literal("stable-initiator"),
            sqlite_text_literal("nodekey:initiator-current"),
            sqlite_text_literal(LOOPBACK_RESPONDER_WORKSPACE_ID),
            sqlite_text_literal(PEER_HANDLE),
        ))
        .map_err(|error| format!("plant loopback transport identity: {error}"))?;
    Ok(())
}

fn route_with_database(
    workspace_path: PathBuf,
    database_path: PathBuf,
    port: u16,
) -> RegisteredResponderRoute {
    let mut registered = route(workspace_path, port);
    registered.database_path = Some(database_path);
    registered.expectations = origin_capable_expectations();
    registered.limits = origin_capable_session_limits();
    registered
}

#[test]
fn production_broker_returns_origin_event_batch_after_unsigned_hello() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let database_path = workspace.path().join("ee.db");
    seed_loopback_origin_event(&database_path)?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 1)?;
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route_with_database(
        workspace.path().to_path_buf(),
        database_path,
        port,
    )])
    .map_err(|error| error.to_string())?;
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        run_runtime(|cx| async move {
            let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
            let broker = ResponderBroker::bind(
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
            let error = match broker.accept_authenticated(&cx).await {
                Ok(_) => {
                    return Err("unsigned hello was treated as an authenticated session".to_owned());
                }
                Err(error) => error,
            };
            if !matches!(error, ResponderBrokerError::BootstrapHelloAnswered) {
                return Err(format!("unexpected hello accept error: {error:?}"));
            }
            fake.finish()?;
            Ok(())
        })
    });
    let address = address_rx
        .recv_timeout(TEST_TIMEOUT)
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    let request = build_request(
        "hello-sync-loopback",
        "nodekey:initiator",
        env!("CARGO_PKG_VERSION"),
        vec!["workspace-initiator".to_owned()],
        vec!["hello".to_owned(), "sync".to_owned()],
        Vec::new(),
    );
    let payload_bytes =
        serialize_within_budget(&request).map_err(|error| format!("serialize hello: {error}"))?;
    let payload = serde_json::from_slice(&payload_bytes)
        .map_err(|error| format!("hello payload json: {error}"))?;
    let (_hello, sync) = exchange_live_mesh_round(
        address,
        TEST_TIMEOUT,
        payload,
        &SyncRoundRequest::new(Vec::new(), 0, 8),
    )
    .map_err(|error| format!("live mesh round: {error}"))?;
    if sync.events.len() != 1 || sync.events[0].event_hash != LOOPBACK_ORIGIN_EVENT_HASH {
        return Err(format!(
            "unsigned hello did not return origin batch: {sync:?}"
        ));
    }
    server
        .join()
        .map_err(|_| "hello+sync broker thread panicked".to_owned())??;
    Ok(())
}

#[test]
fn production_broker_serves_authenticated_event_fetch_from_origin_store() -> TestResult {
    if !lifecycle_test_child(
        "production_broker_serves_authenticated_event_fetch_from_origin_store",
    )? {
        return Ok(());
    }
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
    let database_path = workspace.path().join("ee.db");
    seed_loopback_origin_event(&database_path)?;
    seed_loopback_route_authority(&database_path)?;

    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn_until_stopped(local_api_dir.path())?;
    let control_path = local_api_dir.path().join("responder.sock");
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route_with_database(
        workspace.path().to_path_buf(),
        database_path,
        port,
    )])
    .map_err(|error| error.to_string())?;
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let result = run_runtime_with(Duration::from_secs(30), |cx| async move {
            let mut owner = ResponderBrokerOwner::start(
                &cx,
                LoopbackLocalApi {
                    client: TailscaleLocalApiClient::new(
                        fake.socket_path.clone(),
                        LOCAL_API_TIMEOUT,
                    ),
                    // This delay happens after TCP accept, while real
                    // authentication is waiting for its WhoIs authority. A
                    // control-poll timeout must not discard the handshake.
                    who_is_delay: Duration::from_millis(250),
                    status_failure: None,
                },
                registry,
                PreAuthAdmissionLimits::default(),
                Duration::from_secs(60),
            )
            .await
            .map_err(|error| error.to_string())?;
            owner
                .listen_control(&control_path)
                .map_err(|error| error.to_string())?;
            address_tx
                .send((bind_address, cx.clone()))
                .map_err(|error| format!("publish broker address: {error}"))?;
            let result = owner.serve_until_cancelled(&cx).await;
            if !matches!(result, Err(ResponderBrokerError::Cancelled))
                || !owner.bound_addresses().is_empty()
                || owner.control_socket_path().is_some()
            {
                return Err(format!("authenticated owner did not shut down: {result:?}"));
            }
            let requests = fake.finish()?;
            if requests
                .iter()
                .filter(|request| request.contains("/whois?"))
                .count()
                != 1
            {
                return Err(format!(
                    "slow session did not verify exactly one real peer: {requests:?}"
                ));
            }
            Ok(())
        });
        result
    });
    let (address, cancel) = address_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    let started = Instant::now();
    let client = run_runtime_with(Duration::from_secs(30), |cx| async move {
        contact_authenticated_mesh_peer(
            &cx,
            address,
            origin_capable_initiator_config(),
            &SyncRoundRequest::new(Vec::new(), 0, 8),
        )
        .await
        .map_err(|error| format!("authenticated EventFetch client: {error}"))
    });
    cancel.cancel_with(
        CancelKind::Shutdown,
        Some("authenticated EventFetch complete"),
    );
    let server = server
        .join()
        .map_err(|_| "authenticated serve thread panicked".to_owned())?;
    let sync = match (client, server) {
        (Ok(sync), Ok(())) => sync,
        (client, server) => {
            return Err(format!(
                "authenticated EventFetch failed client={client:?} server={server:?}"
            ));
        }
    };
    if started.elapsed() < Duration::from_millis(200) {
        return Err("slow-handshake fixture did not cross the 50 ms owner poll budget".to_owned());
    }
    if sync.events.len() != 1 || sync.events[0].event_hash != LOOPBACK_ORIGIN_EVENT_HASH {
        return Err(format!(
            "authenticated EventFetch did not return origin batch: {sync:?}"
        ));
    }
    Ok(())
}

fn seed_loopback_body_cache(workspace_path: &Path, database_path: &Path) -> TestResult {
    let connection = DbConnection::open_file(database_path)
        .map_err(|error| format!("open loopback body db: {error}"))?;
    let initiator_node_id = origin_capable_expectations().initiator_node_id;
    connection
        .apply_mesh_lane_grant_with_effect(
            &MeshLaneGrantMutationInput {
                workspace_id: LOOPBACK_RESPONDER_WORKSPACE_ID.to_owned(),
                peer_id: PEER_HANDLE.to_owned(),
                target_adapter: MeshLaneGrantTargetAdapter::new(PEER_HANDLE, initiator_node_id),
                material_lane: MeshLane::Body,
                expected_generation: 1,
                approval_config_digest: Some(ee::mesh::lane_grant::approval_config_digest(b"")),
                updated_at: Some(CREATED_AT.to_owned()),
            },
            |_| Ok::<(), String>(()),
        )
        .map_err(|error| format!("grant loopback body lane: {error}"))?;
    let body = b"secret-body-payload";
    connection
        .upsert_mesh_body_cache_metadata(&UpsertMeshBodyCacheMetadataInput {
            workspace_id: LOOPBACK_RESPONDER_WORKSPACE_ID.to_owned(),
            body_cache_key: "body_loopback1".to_owned(),
            origin_node_id: "node_responder".to_owned(),
            origin_workspace_id: LOOPBACK_RESPONDER_WORKSPACE_ID.to_owned(),
            logical_memory_id: "mem_loopbackorigin0001".to_owned(),
            content_hash: format!("blake3:{}", "ab".repeat(32)),
            body_ref_json: None,
            preview_hash: None,
            size_bytes: Some(19),
            cache_status: "available".to_owned(),
            local_body_hash: Some(format!("blake3:{}", blake3::hash(body).to_hex())),
            cached_at: Some(CREATED_AT.to_owned()),
            expires_at: None,
        })
        .map_err(|error| format!("upsert loopback body cache: {error}"))?;
    let cache_dir = workspace_path.join(".ee").join("mesh-body-cache");
    let cache = SecureLocalDir::open_or_create(workspace_path, &cache_dir)
        .map_err(|error| format!("open loopback body cache dir: {error}"))?;
    cache
        .write_replace("body_loopback1", body)
        .map_err(|error| format!("write loopback body: {error}"))?;
    Ok(())
}

#[test]
fn production_broker_serves_authenticated_body_fetch_from_local_cache() -> TestResult {
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
    let database_path = workspace.path().join("ee.db");
    seed_loopback_origin_event(&database_path)?;
    seed_loopback_route_authority(&database_path)?;
    seed_loopback_body_cache(workspace.path(), &database_path)?;

    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 2)?;
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let mut registered = route_with_database(workspace.path().to_path_buf(), database_path, port);
    registered.grant_generation = 2;
    let registry = ResponderRouteRegistry::new([registered]).map_err(|error| error.to_string())?;
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let result = run_runtime_with(Duration::from_secs(30), |cx| async move {
            let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
            let broker = ResponderBroker::bind(
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
            broker
                .accept_authenticated_and_serve(&cx)
                .await
                .map_err(|error| format!("authenticated serve: {error}"))?;
            fake.finish()?;
            Ok(())
        });
        result
    });
    let address = address_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    let client = run_runtime_with(Duration::from_secs(30), |cx| async move {
        contact_authenticated_body_fetch(
            &cx,
            address,
            origin_capable_initiator_config(),
            "body_loopback1",
        )
        .await
        .map_err(|error| format!("authenticated BodyFetch client: {error}"))
    });
    let server = server
        .join()
        .map_err(|_| "authenticated body serve thread panicked".to_owned())?;
    let fetched = match (client, server) {
        (Ok(fetched), Ok(())) => fetched,
        (client, server) => {
            return Err(format!(
                "authenticated BodyFetch failed client={client:?} server={server:?}"
            ));
        }
    };
    if fetched.cache_status != "available" || fetched.body_hex.is_none() {
        return Err(format!(
            "BodyFetch did not return available body: {fetched:?}"
        ));
    }
    let expected = b"secret-body-payload"
        .iter()
        .fold(String::new(), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        });
    if fetched.body_hex.as_deref() != Some(expected.as_str()) {
        return Err(format!("BodyFetch hex mismatch: {fetched:?}"));
    }
    Ok(())
}

#[test]
fn production_broker_refreshes_locally_verified_identity_without_bearer() -> TestResult {
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
    let database_path = workspace.path().join("ee.db");
    seed_loopback_origin_event(&database_path)?;
    seed_loopback_route_authority(&database_path)?;
    let (team_id, member_id) = {
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("open attest db: {error}"))?;
        ee::mesh::team::create_local_team_with_store(
            &connection,
            LOOPBACK_RESPONDER_WORKSPACE_ID,
            "Analysts",
            CREATED_AT,
            Some(workspace.path()),
        )
        .map_err(|error| format!("create attest team: {error}"))?;
        let self_member = connection
            .list_all_team_members()
            .map_err(|error| format!("list attest members: {error}"))?
            .into_iter()
            .next()
            .ok_or_else(|| "attest team has no member".to_owned())?;
        let member_id = format!("mbr_{}", "e".repeat(32));
        connection
            .insert_team_member(&InsertTeamMemberInput {
                member_id: member_id.clone(),
                team_id: self_member.team_id.clone(),
                workspace_id: LOOPBACK_RESPONDER_WORKSPACE_ID.to_owned(),
                display_name: "Alice".to_owned(),
                state: "active".to_owned(),
                is_self: false,
                origin_node_id: origin_capable_expectations().initiator_node_id,
                bound_via: "member_added_node".to_owned(),
                joined_at: CREATED_AT.to_owned(),
            })
            .map_err(|error| format!("insert remote attest member: {error}"))?;
        ee::mesh::team::record_member_tailnet_identity(
            &connection,
            &member_id,
            "alice@acme.com",
            Some("user-1"),
            "2026-08-13T21:59:00Z",
        )
        .map_err(|error| format!("seed locally verified identity: {error}"))?;
        (self_member.team_id, member_id)
    };

    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 2)?;
    let port = available_nonprivileged_port()?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let mut registered =
        route_with_database(workspace.path().to_path_buf(), database_path.clone(), port);
    registered.expectations.team_id.clone_from(&team_id);
    let registry = ResponderRouteRegistry::new([registered]).map_err(|error| error.to_string())?;
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let result = run_runtime_with(Duration::from_secs(30), |cx| async move {
            let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
            let broker = ResponderBroker::bind(
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
            broker
                .accept_authenticated_and_serve(&cx)
                .await
                .map_err(|error| format!("authenticated serve: {error}"))?;
            fake.finish()?;
            Ok(())
        });
        result
    });
    let address = address_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    let frame = IdentityAttestFrameV1 {
        schema: IDENTITY_ATTEST_FRAME_SCHEMA_V1.to_owned(),
        team_id: team_id.clone(),
        member_id: member_id.clone(),
        subject: "user-1".to_owned(),
        email: Some("alice@acme.com".to_owned()),
        matched_groups: vec!["eng".to_owned()],
        token_hash: "blake3:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
            .to_owned(),
        checked_at: "2026-08-13T22:00:00Z".to_owned(),
    };
    let mut initiator = origin_capable_initiator_config();
    initiator.binding.team_id.clone_from(&team_id);
    let client = run_runtime_with(Duration::from_secs(30), |cx| async move {
        contact_authenticated_identity_attest(&cx, address, initiator, &frame)
            .await
            .map_err(|error| format!("authenticated identity_attest client: {error}"))
    });
    let server = server
        .join()
        .map_err(|_| "authenticated identity attest serve thread panicked".to_owned())?;
    let applied = match (client, server) {
        (Ok(applied), Ok(())) => applied,
        (client, server) => {
            return Err(format!(
                "authenticated identity_attest failed client={client:?} server={server:?}"
            ));
        }
    };
    if applied.subject != "user-1" || applied.member_id != member_id {
        return Err(format!("identity_attest did not apply: {applied:?}"));
    }
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("reopen attest db: {error}"))?;
    let identity = connection
        .get_team_member_identity(&member_id)
        .map_err(|error| format!("load attest identity: {error}"))?
        .ok_or_else(|| "identity row missing after live attest".to_owned())?;
    if identity.login != "alice@acme.com" {
        return Err(format!("live attest stored unexpected login: {identity:?}"));
    }
    let _ = team_id;
    Ok(())
}

const JOIN_INVITER_WORKSPACE_ID: &str = "wsp_inviteloopback000000000001";
const JOIN_JOINER_WORKSPACE_ID: &str = "wsp_joinloopback00000000000001";

fn write_test_framed(stream: &mut std::net::TcpStream, bytes: &[u8]) -> TestResult {
    let prefix = u32::try_from(bytes.len())
        .map_err(|_| "bootstrap frame length does not fit u32".to_owned())?
        .to_be_bytes();
    stream
        .write_all(&prefix)
        .and_then(|()| stream.write_all(bytes))
        .and_then(|()| stream.flush())
        .map_err(|error| format!("write framed join: {error}"))
}

fn read_test_framed(stream: &mut std::net::TcpStream) -> TestResult<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .map_err(|error| format!("read framed join prefix: {error}"))?;
    let length = usize::try_from(u32::from_be_bytes(prefix))
        .map_err(|_| "framed join length does not fit usize".to_owned())?;
    let mut bytes = vec![0_u8; length];
    stream
        .read_exact(&mut bytes)
        .map_err(|error| format!("read framed join: {error}"))?;
    Ok(bytes)
}

fn seed_inviter_team_and_invite(database_path: &Path, endpoint: &str) -> TestResult<String> {
    let connection = DbConnection::open_file(database_path)
        .map_err(|error| format!("open inviter team db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate inviter team db: {error}"))?;
    connection
        .insert_workspace(
            JOIN_INVITER_WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: database_path
                    .parent()
                    .unwrap_or(database_path)
                    .display()
                    .to_string(),
                name: Some("inviter".to_owned()),
            },
        )
        .map_err(|error| format!("insert inviter workspace: {error}"))?;
    let workspace_path = database_path.parent().unwrap_or(database_path);
    ee::mesh::team::create_local_team_with_store(
        &connection,
        JOIN_INVITER_WORKSPACE_ID,
        "Analysts",
        CREATED_AT,
        Some(workspace_path),
    )
    .map_err(|error| format!("create inviter team: {error}"))?;
    let minted = ee::mesh::team::mint_team_invite_with_store(
        &connection,
        endpoint,
        CREATED_AT,
        "9999-12-31T23:59:59Z",
        Some(workspace_path),
    )
    .map_err(|error| format!("mint inviter invite: {error}"))?;
    Ok(minted.invite_code)
}

#[test]
fn production_broker_answers_unsigned_bootstrap_join_and_persists_team_joined() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let database_path = workspace.path().join("ee.db");
    let port = available_nonprivileged_port()?;
    let invite_code = seed_inviter_team_and_invite(&database_path, &format!("127.0.0.1:{port}"))?;
    let joiner = DbConnection::open_memory().map_err(|error| format!("open joiner db: {error}"))?;
    joiner
        .migrate()
        .map_err(|error| format!("migrate joiner db: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 1)?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route_with_database(
        workspace.path().to_path_buf(),
        database_path.clone(),
        port,
    )])
    .map_err(|error| error.to_string())?;
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        run_runtime_with(Duration::from_secs(60), |cx| async move {
            let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
            let broker = ResponderBroker::bind(
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
            let error = match broker.accept_authenticated(&cx).await {
                Ok(_) => {
                    return Err("bootstrap join was treated as an authenticated session".to_owned());
                }
                Err(error) => error,
            };
            if !matches!(error, ResponderBrokerError::BootstrapHelloAnswered) {
                return Err(format!("unexpected join accept error: {error:?}"));
            }
            fake.finish()?;
            Ok(())
        })
    });
    let _address = address_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    let client = ee::mesh::team::join_team_with_code(
        &joiner,
        JOIN_JOINER_WORKSPACE_ID,
        &invite_code,
        "Priya",
        "2026-08-13T04:00:00Z",
        Duration::from_secs(30),
    );
    let server = server
        .join()
        .map_err(|_| "join broker thread panicked".to_owned())?;
    let report = match (client, server) {
        (Ok(report), Ok(())) => report,
        (client, server) => {
            return Err(format!(
                "live join failed client={client:?} server={server:?}"
            ));
        }
    };
    if !report.joined {
        return Err("live join did not persist teamJoined".to_owned());
    }
    let status = ee::mesh::team::local_team_status(&joiner)
        .map_err(|error| format!("joiner status: {error}"))?;
    if status.team_count != 1
        || status.members.len() != 2
        || !status.members.iter().any(|member| member.is_self)
    {
        return Err(format!("joiner status missing self member: {status:?}"));
    }
    let inviter = DbConnection::open_file(&database_path)
        .map_err(|error| format!("reopen inviter db: {error}"))?;
    let inviter_status = ee::mesh::team::local_team_status(&inviter)
        .map_err(|error| format!("inviter status: {error}"))?;
    if inviter_status.members.len() != 2
        || !inviter_status
            .members
            .iter()
            .any(|member| !member.is_self && member.display_name == "Priya")
    {
        return Err(format!(
            "inviter did not record joining member: {inviter_status:?}"
        ));
    }
    Ok(())
}

#[test]
fn production_broker_declines_bootstrap_join_with_wrong_secret() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("temp workspace: {error}"))?;
    let database_path = workspace.path().join("ee.db");
    let port = available_nonprivileged_port()?;
    let invite_code = seed_inviter_team_and_invite(&database_path, &format!("127.0.0.1:{port}"))?;
    let parsed = ee::mesh::team::parse_team_invite_code(&invite_code)
        .map_err(|error| format!("parse invite: {error}"))?;
    let local_api_dir = tempfile::tempdir().map_err(|error| format!("temp localapi: {error}"))?;
    let fake = FakeLocalApi::spawn(local_api_dir.path(), 1)?;
    let bind_address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|error| format!("parse bind address: {error}"))?;
    let registry = ResponderRouteRegistry::new([route_with_database(
        workspace.path().to_path_buf(),
        database_path,
        port,
    )])
    .map_err(|error| error.to_string())?;
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        run_runtime(|cx| async move {
            let client = TailscaleLocalApiClient::new(fake.socket_path.clone(), LOCAL_API_TIMEOUT);
            let broker = ResponderBroker::bind(
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
            let error = match broker.accept_authenticated(&cx).await {
                Ok(_) => {
                    return Err("wrong-secret join was treated as authenticated".to_owned());
                }
                Err(error) => error,
            };
            if !matches!(error, ResponderBrokerError::BootstrapHelloAnswered) {
                return Err(format!("unexpected wrong-secret accept error: {error:?}"));
            }
            fake.finish()?;
            Ok(())
        })
    });
    let address = address_rx
        .recv_timeout(TEST_TIMEOUT)
        .map_err(|error| format!("wait for broker bind: {error}"))?;
    let hello = ee::mesh::team::TeamJoinHelloV1 {
        schema: ee::mesh::team::TEAM_JOIN_HELLO_SCHEMA_V1.to_owned(),
        invite_id: parsed.invite_id.clone(),
        joiner_node_id: "node_ffffffffffffffffffffffffffffffff".to_owned(),
        joiner_display_name: "attacker".to_owned(),
        joiner_nonce: "aa".repeat(16),
        joiner_verifying_key: String::new(),
        joiner_workspace_id: String::new(),
        joiner_hello_port: 0,
    };
    let mut stream = std::net::TcpStream::connect_timeout(&address, TEST_TIMEOUT)
        .map_err(|error| format!("wrong-secret connect: {error}"))?;
    stream
        .set_read_timeout(Some(TEST_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(TEST_TIMEOUT)))
        .map_err(|error| format!("wrong-secret timeout: {error}"))?;
    write_test_framed(
        &mut stream,
        &encode_envelope(
            BootstrapCapability::Join,
            serde_json::to_value(&hello)
                .map_err(|error| format!("serialize join hello: {error}"))?,
        )
        .map_err(|error| format!("encode join hello: {error}"))?,
    )?;
    let challenge_bytes = read_test_framed(&mut stream)?;
    let challenge_envelope = decode_envelope(&challenge_bytes)
        .map_err(|error| format!("decode join challenge: {error}"))?;
    let challenge =
        serde_json::from_value::<ee::mesh::team::TeamJoinChallengeV1>(challenge_envelope.payload)
            .map_err(|error| format!("parse join challenge: {error}"))?;
    let prove = ee::mesh::team::TeamJoinProveV1 {
        schema: ee::mesh::team::TEAM_JOIN_PROVE_SCHEMA_V1.to_owned(),
        invite_id: parsed.invite_id,
        secret: "ffffffffffffffffffffffffffffffff".to_owned(),
        joiner_node_id: hello.joiner_node_id,
        joiner_display_name: hello.joiner_display_name,
        joiner_nonce: hello.joiner_nonce,
        inviter_nonce: challenge.inviter_nonce,
    };
    write_test_framed(
        &mut stream,
        &encode_envelope(
            BootstrapCapability::Join,
            serde_json::to_value(&prove)
                .map_err(|error| format!("serialize join prove: {error}"))?,
        )
        .map_err(|error| format!("encode join prove: {error}"))?,
    )?;
    let reply = read_test_framed(&mut stream)?;
    let declined = serde_json::from_slice::<BootstrapDeclineV1>(&reply)
        .ok()
        .is_some_and(|decline| decline.schema == BOOTSTRAP_DECLINE_SCHEMA_V1);
    if !declined {
        return Err(format!(
            "wrong-secret join was not declined: {}",
            String::from_utf8_lossy(&reply)
        ));
    }
    server
        .join()
        .map_err(|_| "wrong-secret join broker thread panicked".to_owned())??;
    Ok(())
}
