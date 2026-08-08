//! Real-loopback proof for bd-tc-epic-qzk7o.3.2.
//!
//! Every transport assertion crosses an OS TCP socket and the permitted path
//! uses only the public authenticated-session API. Raw peers are confined to
//! adversarial wire injection; there is no in-memory transport substitute.

use std::fmt::Write as _;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU64;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use asupersync::io::{AsyncReadExt, AsyncWriteExt};
use asupersync::net::{TcpListener, TcpStream};
use asupersync::{CancelKind, Cx};
use ee::mesh::key_store::SecretBytes;
use ee::mesh::transport_session::{
    AcceptedSessionConfig, AcceptedSourceAttestation, CAPABILITY_NEGOTIATION_SCHEMA_V1,
    EstablishedSession, FrameCapability, FrameDraft, FrameKind, HandshakeObservations,
    InitiatorHandshake, InitiatorSessionConfig, MAX_FRAME_BYTES, NegotiatedExtensions,
    ResolvedAcceptedRoute, ResponderExpectations, SessionBinding, SessionCapabilities,
    SessionChannelError, SessionChannelLimits, SessionCounters, SessionDirection, SessionMessage,
    TRANSPORT_FRAME_SCHEMA_V1, accept_authenticated_session_with, connect_authenticated_session,
    decode_frame, decode_session_confirm, decode_session_finish, decode_session_open,
    responder_accept_open, sign_frame, verify_frame,
};
use regex_lite::Regex;
use serde_json::json;

type TestResult<T = ()> = Result<T, String>;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn pair_key() -> SecretBytes {
    SecretBytes::new([0x5a; 32])
}

fn binding() -> SessionBinding {
    SessionBinding {
        team_id: "team-loopback".to_owned(),
        tailnet_id: "tailnet-loopback.ts.net".to_owned(),
        initiator_node_id: "node-init".to_owned(),
        responder_node_id: "node-resp".to_owned(),
        initiator_workspace_id: "ws-init".to_owned(),
        responder_workspace_id: "ws-resp".to_owned(),
        initiator_stable_id: "stable-init".to_owned(),
        responder_stable_id: "stable-resp".to_owned(),
        session_id: "session-loopback-0001".to_owned(),
    }
}

fn observations() -> HandshakeObservations {
    HandshakeObservations {
        initiator_node_pubkey: "nodekey:init-current".to_owned(),
        responder_node_pubkey: "nodekey:resp-current".to_owned(),
    }
}

fn limits() -> SessionChannelLimits {
    SessionChannelLimits {
        connect_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_secs(2),
        max_requested_budget_ms: 1_000,
        max_authenticated_frames: 4_096,
        max_authenticated_bytes: 64 * 1024 * 1024,
    }
}

fn initiator_config(custom_limits: SessionChannelLimits) -> InitiatorSessionConfig {
    InitiatorSessionConfig {
        local_address: "127.0.0.2:0".parse().expect("valid loopback source"),
        binding: binding(),
        pair_key: pair_key(),
        pair_key_generation: 7,
        observations: observations(),
        capabilities: SessionCapabilities::base(),
        limits: custom_limits,
    }
}

#[test]
fn accepted_route_is_inspected_before_auth_and_client_source_is_explicit() -> TestResult {
    let (address, server) = spawn_server(move |cx, stream| async move {
        let peer_address = stream.peer_addr().map_err(|error| error.to_string())?;
        let session = accept_authenticated_session_with(
            &cx,
            stream,
            limits(),
            move |_route_cx, observed_address, route| async move {
                if observed_address != peer_address
                    || route.team_id != "team-loopback"
                    || route.responder_workspace_id != "ws-resp"
                    || route.pair_key_generation != 7
                {
                    return Err(SessionChannelError::Authentication {
                        message: "bounded route selectors did not match the accepted socket"
                            .to_owned(),
                    });
                }
                resolved_route(limits(), observed_address.ip())
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        if session.binding().responder_workspace_id != "ws-resp" {
            return Err("authenticated route changed after local selection".to_owned());
        }
        Ok(peer_address)
    })?;

    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session.close();
        Ok(())
    })?;
    let peer_address = join_server(server)?;
    if peer_address.ip().to_string() != "127.0.0.2" {
        return Err(format!(
            "listener observed {peer_address}, expected explicitly bound 127.0.0.2 source"
        ));
    }
    Ok(())
}

#[test]
fn connect_refuses_wildcard_source_before_network_io() -> TestResult {
    run_runtime(|cx| async move {
        let mut config = initiator_config(limits());
        config.local_address = "0.0.0.0:0"
            .parse()
            .map_err(|error| format!("parse: {error}"))?;
        let remote = "127.0.0.1:9"
            .parse()
            .map_err(|error| format!("parse: {error}"))?;
        let error = connect_authenticated_session(&cx, remote, config)
            .await
            .expect_err("wildcard source must fail before connect");
        if !matches!(error, SessionChannelError::InvalidLimits { .. }) {
            return Err(format!("unexpected wildcard-source error: {error}"));
        }
        Ok(())
    })
}

#[test]
fn pending_route_cannot_widen_session_limits_after_open() -> TestResult {
    let (address, server) = spawn_server(move |cx, stream| async move {
        let error = accept_authenticated_session_with(
            &cx,
            stream,
            limits(),
            move |_route_cx, peer_address, _selectors| async move {
                let mut widened = limits();
                widened.max_authenticated_bytes = widened.max_authenticated_bytes.saturating_add(1);
                resolved_route(widened, peer_address.ip())
            },
        )
        .await
        .expect_err("route selection must not widen pending-open limits");
        Ok(error)
    })?;

    run_runtime(|cx| async move {
        connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .expect_err("server must close a limit-mismatched pending route");
        Ok(())
    })?;
    let error = join_server(server)?;
    if !matches!(error, SessionChannelError::InvalidLimits { .. }) {
        return Err(format!("unexpected pending-limit error: {error}"));
    }
    Ok(())
}

#[test]
fn accepted_source_attestation_must_match_kernel_peer_before_pair_key_auth() -> TestResult {
    let (address, server) = spawn_server(move |cx, stream| async move {
        let error = accept_authenticated_session_with(
            &cx,
            stream,
            limits(),
            move |_route_cx, _peer_address, _selectors| async move {
                resolved_route(
                    limits(),
                    "127.0.0.3"
                        .parse()
                        .map_err(|error| SessionChannelError::Authentication {
                            message: format!("invalid test source address: {error}"),
                        })?,
                )
            },
        )
        .await
        .expect_err("unrelated source attestation must fail before handshake");
        Ok(error)
    })?;

    run_runtime(|cx| async move {
        connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .expect_err("server must close a source-mismatched pending route");
        Ok(())
    })?;
    let error = join_server(server)?;
    if !matches!(error, SessionChannelError::Authentication { .. }) {
        return Err(format!("unexpected source-mismatch error: {error}"));
    }
    Ok(())
}

fn accepted_config(custom_limits: SessionChannelLimits) -> AcceptedSessionConfig {
    AcceptedSessionConfig {
        expectations: ResponderExpectations {
            team_id: "team-loopback".to_owned(),
            tailnet_id: "tailnet-loopback.ts.net".to_owned(),
            responder_node_id: "node-resp".to_owned(),
            responder_workspace_id: "ws-resp".to_owned(),
            responder_stable_id: "stable-resp".to_owned(),
            initiator_node_id: "node-init".to_owned(),
            initiator_stable_id: "stable-init".to_owned(),
            pair_key_generation: 7,
        },
        pair_key: pair_key(),
        observations: observations(),
        capabilities: SessionCapabilities::base(),
        limits: custom_limits,
    }
}

fn accepted_source(source_ip: IpAddr) -> Result<AcceptedSourceAttestation, SessionChannelError> {
    AcceptedSourceAttestation::from_local_whois(
        source_ip,
        "tailnet-loopback.ts.net",
        "stable-init",
        "nodekey:init-current",
    )
}

fn resolved_route(
    custom_limits: SessionChannelLimits,
    source_ip: IpAddr,
) -> Result<ResolvedAcceptedRoute<()>, SessionChannelError> {
    Ok(ResolvedAcceptedRoute::new(
        accepted_config(custom_limits),
        accepted_source(source_ip)?,
        (),
    ))
}

async fn accept_loopback_session(
    cx: &Cx,
    stream: TcpStream,
    custom_limits: SessionChannelLimits,
) -> Result<ee::mesh::transport_session::AuthenticatedTransportSession, SessionChannelError> {
    accept_authenticated_session_with(
        cx,
        stream,
        custom_limits,
        move |_route_cx, peer_address, _selectors| async move {
            resolved_route(custom_limits, peer_address.ip())
        },
    )
    .await
}

fn run_runtime<F, Fut, T>(operation: F) -> TestResult<T>
where
    F: FnOnce(Cx) -> Fut,
    Fut: Future<Output = TestResult<T>>,
{
    ee::core::run_cli_with_cx(TEST_TIMEOUT, operation)
        .map_err(|error| format!("asupersync runtime failed: {error}"))?
}

fn run_runtime_for<F, Fut, T>(timeout: Duration, operation: F) -> TestResult<T>
where
    F: FnOnce(Cx) -> Fut,
    Fut: Future<Output = TestResult<T>>,
{
    ee::core::run_cli_with_cx(timeout, operation)
        .map_err(|error| format!("asupersync runtime failed: {error}"))?
}

fn spawn_server<H, Fut, T>(
    handler: H,
) -> TestResult<(SocketAddr, thread::JoinHandle<TestResult<T>>)>
where
    H: FnOnce(Cx, TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = TestResult<T>> + 'static,
    T: Send + 'static,
{
    let (address_tx, address_rx) = mpsc::sync_channel(1);
    let join = thread::spawn(move || {
        run_runtime(|cx| async move {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|error| format!("bind loopback listener: {error}"))?;
            let address = listener
                .local_addr()
                .map_err(|error| format!("read loopback address: {error}"))?;
            address_tx
                .send(address)
                .map_err(|error| format!("publish loopback address: {error}"))?;
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|error| format!("accept loopback stream: {error}"))?;
            handler(cx, stream).await
        })
    });
    let address = address_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| format!("wait for loopback address: {error}"))?;
    Ok((address, join))
}

fn join_server<T>(join: thread::JoinHandle<TestResult<T>>) -> TestResult<T> {
    join.join()
        .map_err(|_| "loopback server thread panicked".to_owned())?
}

async fn write_packet(stream: &mut TcpStream, bytes: &[u8]) -> TestResult {
    let length = u32::try_from(bytes.len())
        .map_err(|_| "test packet exceeds u32".to_owned())?
        .to_be_bytes();
    stream
        .write_all(&length)
        .await
        .map_err(|error| format!("write prefix: {error}"))?;
    stream
        .write_all(bytes)
        .await
        .map_err(|error| format!("write body: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("flush packet: {error}"))
}

async fn write_json<T: serde::Serialize>(stream: &mut TcpStream, value: &T) -> TestResult {
    let bytes = serde_json::to_vec(value).map_err(|error| format!("encode JSON: {error}"))?;
    write_packet(stream, &bytes).await
}

async fn read_packet(stream: &mut TcpStream) -> TestResult<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .await
        .map_err(|error| format!("read prefix: {error}"))?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(format!("test peer refused {length}-byte packet"));
    }
    let mut bytes = vec![0_u8; length];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|error| format!("read body: {error}"))?;
    Ok(bytes)
}

fn negotiation_correlation() -> String {
    let digest = blake3::hash(binding().session_id.as_bytes()).to_hex();
    format!("session-capabilities-{}", &digest.as_str()[..24])
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

async fn raw_initiator(address: SocketAddr) -> TestResult<(TcpStream, EstablishedSession)> {
    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|error| format!("raw initiator connect: {error}"))?;
    let (state, open) = InitiatorHandshake::open(binding(), [0x11; 32], 7, observations())
        .map_err(|error| error.to_string())?;
    write_json(&mut stream, &open).await?;
    let confirm = decode_session_confirm(&read_packet(&mut stream).await?)
        .map_err(|error| error.to_string())?;
    let (finish, mut established) = state
        .finish(&pair_key(), &confirm)
        .map_err(|error| error.to_string())?;
    write_json(&mut stream, &finish).await?;

    let offer = sign_frame(
        &established.binding,
        &established.keys,
        FrameDraft {
            direction: SessionDirection::InitiatorToResponder,
            counter: 1,
            correlation_id: negotiation_correlation(),
            kind: FrameKind::Request,
            capability: FrameCapability::Hello,
            requested_budget_ms: 0,
            payload: json!({
                "schema": CAPABILITY_NEGOTIATION_SCHEMA_V1,
                "phase": "offer",
                "capabilities": ["body_fetch", "event_fetch", "hello", "summary"]
            }),
        },
    )
    .map_err(|error| error.to_string())?;
    write_json(&mut stream, &offer).await?;
    let response =
        decode_frame(&read_packet(&mut stream).await?).map_err(|error| error.to_string())?;
    verify_frame(
        &response,
        &established.binding,
        SessionDirection::ResponderToInitiator,
        &mut established.inbound,
        &established.keys,
        &NegotiatedExtensions::none(),
    )
    .map_err(|error| error.to_string())?;
    if response.kind != FrameKind::Response || response.correlation_id != negotiation_correlation()
    {
        return Err("raw initiator received invalid negotiation response".to_owned());
    }
    established.next_outbound = 2;
    Ok((stream, established))
}

async fn raw_responder(mut stream: TcpStream) -> TestResult<(TcpStream, EstablishedSession)> {
    let open =
        decode_session_open(&read_packet(&mut stream).await?).map_err(|error| error.to_string())?;
    let config = accepted_config(limits());
    let (pending, confirm) = responder_accept_open(
        &open,
        &config.expectations,
        [0x22; 32],
        config.observations,
        &config.pair_key,
    )
    .map_err(|error| error.to_string())?;
    write_json(&mut stream, &confirm).await?;
    let finish = decode_session_finish(&read_packet(&mut stream).await?)
        .map_err(|error| error.to_string())?;
    let mut established = pending
        .complete(&config.pair_key, &finish)
        .map_err(|error| error.to_string())?;
    let offer =
        decode_frame(&read_packet(&mut stream).await?).map_err(|error| error.to_string())?;
    verify_frame(
        &offer,
        &established.binding,
        SessionDirection::InitiatorToResponder,
        &mut established.inbound,
        &established.keys,
        &NegotiatedExtensions::none(),
    )
    .map_err(|error| error.to_string())?;
    let selection = sign_frame(
        &established.binding,
        &established.keys,
        FrameDraft {
            direction: SessionDirection::ResponderToInitiator,
            counter: 1,
            correlation_id: negotiation_correlation(),
            kind: FrameKind::Response,
            capability: FrameCapability::Hello,
            requested_budget_ms: 0,
            payload: json!({
                "schema": CAPABILITY_NEGOTIATION_SCHEMA_V1,
                "phase": "selection",
                "capabilities": ["body_fetch", "event_fetch", "hello", "summary"]
            }),
        },
    )
    .map_err(|error| error.to_string())?;
    write_json(&mut stream, &selection).await?;
    established.next_outbound = 2;
    Ok((stream, established))
}

fn request(correlation_id: &str, payload: serde_json::Value) -> SessionMessage {
    SessionMessage {
        correlation_id: correlation_id.to_owned(),
        capability: FrameCapability::Summary,
        requested_budget_ms: 250,
        payload,
    }
}

#[test]
fn real_loopback_public_api_exchanges_bidirectional_frames_and_half_closes() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, limits())
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("server-request", json!({"ask": "ack"})))
            .await
            .map_err(|error| error.to_string())?;
        let inbound = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("client half-closed before request")?;
        if inbound.payload != json!({"tip": 41}) {
            return Err("server received wrong authenticated payload".to_owned());
        }
        server_mutations.fetch_add(1, Ordering::SeqCst);
        session
            .send_response(&cx, request(&inbound.correlation_id, json!({"tip": 42})))
            .await
            .map_err(|error| error.to_string())?;
        let response = session
            .receive_response(&cx, "server-request")
            .await
            .map_err(|error| error.to_string())?;
        if response.payload != json!({"ack": true}) {
            return Err("server received wrong correlated response".to_owned());
        }
        session
            .shutdown_write(&cx)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    })?;

    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("client-request", json!({"tip": 41})))
            .await
            .map_err(|error| error.to_string())?;
        let inbound = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("server half-closed before its request")?;
        session
            .send_response(&cx, request(&inbound.correlation_id, json!({"ack": true})))
            .await
            .map_err(|error| error.to_string())?;
        let response = session
            .receive_response(&cx, "client-request")
            .await
            .map_err(|error| error.to_string())?;
        if response.payload != json!({"tip": 42}) {
            return Err("client received wrong correlated response".to_owned());
        }
        let eof = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?;
        if eof.is_some() {
            return Err("clean half-close produced an application request".to_owned());
        }
        Ok(())
    })?;
    join_server(server)?;
    if mutations.load(Ordering::SeqCst) != 1 {
        return Err("permitted request did not perform exactly one mutation".to_owned());
    }
    Ok(())
}

fn negotiated_usage() -> TestResult<ee::mesh::transport_session::AuthenticatedSessionUsage> {
    let (address, server) = spawn_server(move |cx, stream| async move {
        let session = accept_loopback_session(&cx, stream, limits())
            .await
            .map_err(|error| error.to_string())?;
        Ok(session.authenticated_usage())
    })?;
    let client_usage = run_runtime(|cx| async move {
        let session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        Ok(session.authenticated_usage())
    })?;
    let server_usage = join_server(server)?;
    if client_usage != server_usage || client_usage.frames != 2 || client_usage.wire_bytes == 0 {
        return Err(format!(
            "capability negotiation usage drifted: client={client_usage:?}, server={server_usage:?}"
        ));
    }
    Ok(client_usage)
}

#[test]
fn terminal_authenticated_frame_budget_closes_at_the_exact_boundary() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let mut server_limits = limits();
    server_limits.max_authenticated_frames = 2;
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, server_limits)
            .await
            .map_err(|error| error.to_string())?;
        if session.authenticated_usage().frames != 2 {
            return Err("negotiation did not consume the exact frame budget".to_owned());
        }
        let error = session
            .receive_request(&cx)
            .await
            .expect_err("the first application frame must exceed the terminal frame budget");
        if !matches!(
            error,
            SessionChannelError::SessionBudgetExhausted { resource: "frame" }
        ) {
            return Err(format!(
                "expected frame-budget exhaustion, observed {error:?}"
            ));
        }
        if server_mutations.load(Ordering::SeqCst) != 0 {
            return Err("over-frame-budget request mutated application state".to_owned());
        }
        Ok(())
    })?;
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("frame-budget", json!({"mutate": true})))
            .await
            .map_err(|error| error.to_string())
    })?;
    join_server(server)?;
    if mutations.load(Ordering::SeqCst) != 0 {
        return Err("terminal frame-budget proof observed application mutation".to_owned());
    }
    Ok(())
}

#[test]
fn terminal_authenticated_byte_budget_closes_at_the_exact_boundary() -> TestResult {
    let negotiated = negotiated_usage()?;
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let mut server_limits = limits();
    server_limits.max_authenticated_bytes = negotiated.wire_bytes;
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, server_limits)
            .await
            .map_err(|error| error.to_string())?;
        if session.authenticated_usage().wire_bytes != negotiated.wire_bytes {
            return Err("negotiation did not consume the exact byte budget".to_owned());
        }
        let error = session
            .receive_request(&cx)
            .await
            .expect_err("the first application frame must exceed the terminal byte budget");
        if !matches!(
            error,
            SessionChannelError::SessionBudgetExhausted { resource: "byte" }
        ) {
            return Err(format!(
                "expected byte-budget exhaustion, observed {error:?}"
            ));
        }
        if server_mutations.load(Ordering::SeqCst) != 0 {
            return Err("over-byte-budget request mutated application state".to_owned());
        }
        Ok(())
    })?;
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("byte-budget", json!({"mutate": true})))
            .await
            .map_err(|error| error.to_string())
    })?;
    join_server(server)?;
    if mutations.load(Ordering::SeqCst) != 0 {
        return Err("terminal byte-budget proof observed application mutation".to_owned());
    }
    Ok(())
}

#[test]
fn cumulative_authenticated_frame_budget_counts_both_directions() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let mut terminal_limits = limits();
    // Capability negotiation is two authenticated frames; the request and
    // response must consume the remaining two slots together.
    terminal_limits.max_authenticated_frames = 4;
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, terminal_limits)
            .await
            .map_err(|error| error.to_string())?;
        let request = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("peer half-closed before cumulative-frame request")?;
        server_mutations.fetch_add(1, Ordering::SeqCst);
        session
            .send_response(
                &cx,
                request(&request.correlation_id, json!({"accepted": true})),
            )
            .await
            .map_err(|error| error.to_string())?;
        if session.authenticated_usage().frames != 4 {
            return Err("request/response did not consume the shared frame budget".to_owned());
        }
        Ok(session.authenticated_usage())
    })?;
    let client_usage = run_runtime(|cx| async move {
        let mut session =
            connect_authenticated_session(&cx, address, initiator_config(terminal_limits))
                .await
                .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("cumulative-frame", json!({"mutate": true})))
            .await
            .map_err(|error| error.to_string())?;
        let response = session
            .receive_response(&cx, "cumulative-frame")
            .await
            .map_err(|error| error.to_string())?;
        if response.payload != json!({"accepted": true}) {
            return Err("cumulative-frame response drifted".to_owned());
        }
        let error = session
            .send_request(&cx, request("over-frame-budget", json!({"mutate": true})))
            .await
            .expect_err("the fifth authenticated frame must exceed the shared budget");
        if !matches!(
            error,
            SessionChannelError::SessionBudgetExhausted { resource: "frame" }
        ) {
            return Err(format!(
                "expected cumulative frame-budget exhaustion, observed {error:?}"
            ));
        }
        Ok(session.authenticated_usage())
    })?;
    let server_usage = join_server(server)?;
    if client_usage != server_usage || client_usage.frames != 4 {
        return Err(format!(
            "cumulative frame usage drifted: client={client_usage:?}, server={server_usage:?}"
        ));
    }
    if mutations.load(Ordering::SeqCst) != 1 {
        return Err("over-frame-budget request reached application processing".to_owned());
    }
    Ok(())
}

#[test]
fn cumulative_authenticated_byte_budget_counts_both_directions() -> TestResult {
    let baseline_usage = completed_exchange_usage(limits(), "cumulative-byte")?;
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let mut terminal_limits = limits();
    terminal_limits.max_authenticated_bytes = baseline_usage.wire_bytes;
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, terminal_limits)
            .await
            .map_err(|error| error.to_string())?;
        let request = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("peer half-closed before cumulative-byte request")?;
        server_mutations.fetch_add(1, Ordering::SeqCst);
        session
            .send_response(
                &cx,
                request(&request.correlation_id, json!({"accepted": true})),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(session.authenticated_usage())
    })?;
    let client_usage = run_runtime(|cx| async move {
        let mut session =
            connect_authenticated_session(&cx, address, initiator_config(terminal_limits))
                .await
                .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("cumulative-byte", json!({"mutate": true})))
            .await
            .map_err(|error| error.to_string())?;
        let response = session
            .receive_response(&cx, "cumulative-byte")
            .await
            .map_err(|error| error.to_string())?;
        if response.payload != json!({"accepted": true}) {
            return Err("cumulative-byte response drifted".to_owned());
        }
        let error = session
            .send_request(&cx, request("over-byte-budget", json!({"mutate": true})))
            .await
            .expect_err("the next authenticated frame must exceed the shared byte budget");
        if !matches!(
            error,
            SessionChannelError::SessionBudgetExhausted { resource: "byte" }
        ) {
            return Err(format!(
                "expected cumulative byte-budget exhaustion, observed {error:?}"
            ));
        }
        Ok(session.authenticated_usage())
    })?;
    let server_usage = join_server(server)?;
    if client_usage != server_usage || client_usage != baseline_usage {
        return Err(format!(
            "cumulative byte usage drifted: baseline={baseline_usage:?}, client={client_usage:?}, server={server_usage:?}"
        ));
    }
    if mutations.load(Ordering::SeqCst) != 1 {
        return Err("over-byte-budget request reached application processing".to_owned());
    }
    Ok(())
}

fn completed_exchange_usage(
    session_limits: SessionChannelLimits,
    correlation_id: &'static str,
) -> TestResult<ee::mesh::transport_session::AuthenticatedSessionUsage> {
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, session_limits)
            .await
            .map_err(|error| error.to_string())?;
        let request = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("peer half-closed before baseline exchange")?;
        session
            .send_response(
                &cx,
                request(&request.correlation_id, json!({"accepted": true})),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(session.authenticated_usage())
    })?;
    let client_usage = run_runtime(|cx| async move {
        let mut session =
            connect_authenticated_session(&cx, address, initiator_config(session_limits))
                .await
                .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request(correlation_id, json!({"mutate": true})))
            .await
            .map_err(|error| error.to_string())?;
        session
            .receive_response(&cx, correlation_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(session.authenticated_usage())
    })?;
    let server_usage = join_server(server)?;
    if client_usage != server_usage {
        return Err(format!(
            "baseline authenticated usage drifted: client={client_usage:?}, server={server_usage:?}"
        ));
    }
    Ok(client_usage)
}

#[test]
fn local_entropy_and_closed_channel_errors_are_transport_unreachable() -> TestResult {
    for (label, error) in [
        (
            "entropy failure",
            SessionChannelError::Randomness {
                message: "test-only CSPRNG failure".to_owned(),
            },
        ),
        ("locally closed channel", SessionChannelError::Closed),
    ] {
        if error.degraded_code() != "mesh_transport_unreachable" {
            return Err(format!(
                "{label} must be classified as transport-unreachable, got {}",
                error.degraded_code()
            ));
        }
    }
    Ok(())
}

#[test]
fn peer_half_close_with_outstanding_response_is_not_clean_eof() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, limits())
            .await
            .map_err(|error| error.to_string())?;
        let _request = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("client half-closed before outstanding-response proof")?;
        if server_mutations.load(Ordering::SeqCst) != 0 {
            return Err("half-close fixture mutated application state".to_owned());
        }
        session
            .shutdown_write(&cx)
            .await
            .map_err(|error| error.to_string())
    })?;
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("missing-response", json!({"mutate": false})))
            .await
            .map_err(|error| error.to_string())?;
        let error = session
            .receive_request(&cx)
            .await
            .expect_err("EOF with an outstanding response must not be clean");
        if error != SessionChannelError::UnexpectedHalfClose {
            return Err(format!(
                "expected unexpected half-close, observed {error:?}"
            ));
        }
        Ok(())
    })?;
    join_server(server)?;
    if mutations.load(Ordering::SeqCst) != 0 {
        return Err("outstanding-response half-close mutated application state".to_owned());
    }
    Ok(())
}

#[test]
fn real_loopback_retry_uses_fresh_frame_with_same_idempotency_key() -> TestResult {
    let (address, server) = spawn_server(move |_cx, stream| async move {
        let (mut stream, mut established) = raw_responder(stream).await?;
        let mut observed = Vec::new();
        for response_counter in [2_u64, 3] {
            let frame = decode_frame(&read_packet(&mut stream).await?)
                .map_err(|error| error.to_string())?;
            verify_frame(
                &frame,
                &established.binding,
                SessionDirection::InitiatorToResponder,
                &mut established.inbound,
                &established.keys,
                &NegotiatedExtensions::none(),
            )
            .map_err(|error| error.to_string())?;
            observed.push(frame.clone());
            let response = sign_frame(
                &established.binding,
                &established.keys,
                FrameDraft {
                    direction: SessionDirection::ResponderToInitiator,
                    counter: response_counter,
                    correlation_id: frame.correlation_id,
                    kind: FrameKind::Response,
                    capability: frame.capability,
                    requested_budget_ms: frame.requested_budget_ms,
                    payload: json!({"accepted": true}),
                },
            )
            .map_err(|error| error.to_string())?;
            write_json(&mut stream, &response).await?;
        }
        if observed[0].correlation_id != observed[1].correlation_id
            || observed[0].payload != observed[1].payload
        {
            return Err("retry changed its idempotency key or payload".to_owned());
        }
        if observed[0].counter + 1 != observed[1].counter || observed[0].mac == observed[1].mac {
            return Err(
                "retry replayed frame bytes instead of using the exact-next counter".to_owned(),
            );
        }
        Ok(())
    })?;
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        for _ in 0..2 {
            session
                .send_request(
                    &cx,
                    request("stable-idempotency-key", json!({"same": "operation"})),
                )
                .await
                .map_err(|error| error.to_string())?;
            let response = session
                .receive_response(&cx, "stable-idempotency-key")
                .await
                .map_err(|error| error.to_string())?;
            if response.payload != json!({"accepted": true}) {
                return Err("retry response drifted".to_owned());
            }
        }
        Ok(())
    })?;
    join_server(server)
}

#[derive(Clone, Copy, Debug)]
enum Attack {
    WrongMac,
    WrongBinding,
    WrongDirection,
    Duplicate,
    Skipped,
    Regressed,
    V1,
    Oversized,
    PartialPrefix,
    PartialBody,
    PayloadHash,
    UnnegotiatedCapability,
}

impl Attack {
    const fn expected_code(self) -> &'static str {
        match self {
            Self::WrongBinding | Self::WrongDirection => "mesh_frame_target_mismatch",
            Self::Duplicate | Self::Skipped | Self::Regressed => "mesh_frame_replay_rejected",
            Self::WrongMac
            | Self::V1
            | Self::Oversized
            | Self::PartialPrefix
            | Self::PartialBody
            | Self::PayloadHash
            | Self::UnnegotiatedCapability => "mesh_frame_auth_failed",
        }
    }
}

async fn inject_attack(
    attack: Attack,
    stream: &mut TcpStream,
    established: &EstablishedSession,
) -> TestResult {
    match attack {
        Attack::V1 => {
            return write_json(stream, &json!({"schema": TRANSPORT_FRAME_SCHEMA_V1})).await;
        }
        Attack::Oversized => {
            let prefix = u32::try_from(MAX_FRAME_BYTES + 1)
                .map_err(|_| "oversized proof prefix overflow".to_owned())?
                .to_be_bytes();
            stream
                .write_all(&prefix)
                .await
                .map_err(|error| format!("write oversized prefix: {error}"))?;
            return Ok(());
        }
        Attack::PartialPrefix => {
            stream
                .write_all(&[0, 0])
                .await
                .map_err(|error| format!("write partial prefix: {error}"))?;
            AsyncWriteExt::shutdown(stream)
                .await
                .map_err(|error| format!("half-close partial prefix: {error}"))?;
            return Ok(());
        }
        Attack::PartialBody => {
            stream
                .write_all(&16_u32.to_be_bytes())
                .await
                .map_err(|error| format!("write partial-body prefix: {error}"))?;
            stream
                .write_all(b"partial")
                .await
                .map_err(|error| format!("write partial body: {error}"))?;
            AsyncWriteExt::shutdown(stream)
                .await
                .map_err(|error| format!("half-close partial body: {error}"))?;
            return Ok(());
        }
        _ => {}
    }

    let (direction, counter) = match attack {
        Attack::WrongDirection => (SessionDirection::ResponderToInitiator, 2),
        Attack::Duplicate => (SessionDirection::InitiatorToResponder, 1),
        Attack::Skipped => (SessionDirection::InitiatorToResponder, 3),
        Attack::Regressed => (SessionDirection::InitiatorToResponder, 0),
        _ => (SessionDirection::InitiatorToResponder, 2),
    };
    let mut frame = sign_frame(
        &established.binding,
        &established.keys,
        FrameDraft {
            direction,
            counter,
            correlation_id: "attack-request".to_owned(),
            kind: FrameKind::Request,
            capability: if matches!(attack, Attack::UnnegotiatedCapability) {
                FrameCapability::Extension("not_negotiated".to_owned())
            } else {
                FrameCapability::Summary
            },
            requested_budget_ms: 250,
            payload: json!({"mutation": "forbidden"}),
        },
    )
    .map_err(|error| error.to_string())?;
    match attack {
        Attack::WrongMac => frame.mac = "00".repeat(32),
        Attack::WrongBinding => {
            frame.target_workspace_id.push_str("-wrong");
            frame.mac = hex_bytes(
                blake3::keyed_hash(
                    established
                        .keys
                        .for_direction(SessionDirection::InitiatorToResponder)
                        .as_bytes(),
                    &frame.mac_preimage(),
                )
                .as_bytes(),
            );
        }
        Attack::PayloadHash => frame.payload = json!({"mutation": "tampered"}),
        _ => {}
    }
    write_json(stream, &frame).await
}

fn run_attack(attack: Attack) -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, limits())
            .await
            .map_err(|error| error.to_string())?;
        let error = session
            .receive_request(&cx)
            .await
            .expect_err("forbidden wire input must fail before application dispatch");
        if error.degraded_code() != attack.expected_code() {
            return Err(format!(
                "{attack:?}: expected {}, observed {} ({error})",
                attack.expected_code(),
                error.degraded_code()
            ));
        }
        if server_mutations.load(Ordering::SeqCst) != 0 {
            return Err(format!(
                "{attack:?}: failure performed application mutation"
            ));
        }
        let closed = session.receive_request(&cx).await;
        if !matches!(closed, Err(SessionChannelError::Closed)) {
            return Err(format!("{attack:?}: session remained usable after failure"));
        }
        Ok(())
    })?;
    run_runtime(|_cx| async move {
        let (mut stream, established) = raw_initiator(address).await?;
        inject_attack(attack, &mut stream, &established).await
    })?;
    join_server(server)?;
    if mutations.load(Ordering::SeqCst) != 0 {
        return Err(format!(
            "{attack:?}: forbidden request mutated application state"
        ));
    }
    Ok(())
}

#[test]
fn real_loopback_rejects_mac_binding_direction_counters_sizes_prefix_and_v1() -> TestResult {
    for attack in [
        Attack::WrongMac,
        Attack::WrongBinding,
        Attack::WrongDirection,
        Attack::Duplicate,
        Attack::Skipped,
        Attack::Regressed,
        Attack::V1,
        Attack::Oversized,
        Attack::PartialPrefix,
        Attack::PartialBody,
        Attack::PayloadHash,
        Attack::UnnegotiatedCapability,
    ] {
        run_attack(attack)?;
    }
    Ok(())
}

#[test]
fn real_loopback_rejects_wrong_response_correlation_without_mutation() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let server = spawn_server(move |_cx, stream| async move {
        let (mut stream, mut established) = raw_responder(stream).await?;
        let request_frame =
            decode_frame(&read_packet(&mut stream).await?).map_err(|error| error.to_string())?;
        verify_frame(
            &request_frame,
            &established.binding,
            SessionDirection::InitiatorToResponder,
            &mut established.inbound,
            &established.keys,
            &NegotiatedExtensions::none(),
        )
        .map_err(|error| error.to_string())?;
        let wrong = sign_frame(
            &established.binding,
            &established.keys,
            FrameDraft {
                direction: SessionDirection::ResponderToInitiator,
                counter: 2,
                correlation_id: "near-identical-wrong-correlation".to_owned(),
                kind: FrameKind::Response,
                capability: request_frame.capability,
                requested_budget_ms: request_frame.requested_budget_ms,
                payload: json!({"mutation": "forbidden"}),
            },
        )
        .map_err(|error| error.to_string())?;
        write_json(&mut stream, &wrong).await
    })?;
    let (address, join) = server;
    let client_mutations = Arc::clone(&mutations);
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("expected-correlation", json!({"read": true})))
            .await
            .map_err(|error| error.to_string())?;
        let error = session
            .receive_response(&cx, "expected-correlation")
            .await
            .expect_err("wrong correlation must fail closed");
        if error.degraded_code() != "mesh_frame_auth_failed" {
            return Err(format!(
                "wrong correlation emitted {}",
                error.degraded_code()
            ));
        }
        if client_mutations.load(Ordering::SeqCst) != 0 {
            return Err("wrong correlation mutated application state".to_owned());
        }
        Ok(())
    })?;
    join_server(join)?;
    Ok(())
}

#[test]
fn real_loopback_rejects_response_capability_mismatch_on_receive() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let (address, join) = spawn_server(move |_cx, stream| async move {
        let (mut stream, mut established) = raw_responder(stream).await?;
        let request_frame =
            decode_frame(&read_packet(&mut stream).await?).map_err(|error| error.to_string())?;
        verify_frame(
            &request_frame,
            &established.binding,
            SessionDirection::InitiatorToResponder,
            &mut established.inbound,
            &established.keys,
            &NegotiatedExtensions::none(),
        )
        .map_err(|error| error.to_string())?;
        let mismatch = sign_frame(
            &established.binding,
            &established.keys,
            FrameDraft {
                direction: SessionDirection::ResponderToInitiator,
                counter: 2,
                correlation_id: request_frame.correlation_id,
                kind: FrameKind::Response,
                capability: FrameCapability::BodyFetch,
                requested_budget_ms: request_frame.requested_budget_ms,
                payload: json!({"mutation": "forbidden"}),
            },
        )
        .map_err(|error| error.to_string())?;
        write_json(&mut stream, &mismatch).await
    })?;
    let client_mutations = Arc::clone(&mutations);
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("capability-match", json!({"read": true})))
            .await
            .map_err(|error| error.to_string())?;
        let error = session
            .receive_response(&cx, "capability-match")
            .await
            .expect_err("BodyFetch cannot answer a Summary request");
        if error.degraded_code() != "mesh_frame_auth_failed" {
            return Err(format!(
                "capability mismatch emitted {}",
                error.degraded_code()
            ));
        }
        if client_mutations.load(Ordering::SeqCst) != 0 {
            return Err("capability-mismatched response mutated application state".to_owned());
        }
        Ok(())
    })?;
    join_server(join)
}

#[test]
fn real_loopback_rejects_response_capability_mismatch_on_send() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let (address, join) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, limits())
            .await
            .map_err(|error| error.to_string())?;
        let inbound = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("client half-closed before mismatch proof")?;
        let error = session
            .send_response(
                &cx,
                SessionMessage {
                    correlation_id: inbound.correlation_id,
                    capability: FrameCapability::BodyFetch,
                    requested_budget_ms: 250,
                    payload: json!({"mutation": "forbidden"}),
                },
            )
            .await
            .expect_err("responder must refuse capability-confused response");
        if error.degraded_code() != "mesh_frame_auth_failed" {
            return Err(format!("send mismatch emitted {}", error.degraded_code()));
        }
        if server_mutations.load(Ordering::SeqCst) != 0 {
            return Err("send-side mismatch mutated application state".to_owned());
        }
        Ok(())
    })?;
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(&cx, request("send-capability-match", json!({"read": true})))
            .await
            .map_err(|error| error.to_string())?;
        let _ = session.receive_response(&cx, "send-capability-match").await;
        Ok(())
    })?;
    join_server(join)
}

#[test]
fn deadlines_and_cancellation_fail_before_application_mutation() -> TestResult {
    let (address, server) = spawn_server(move |_cx, _stream| async move {
        asupersync::time::sleep(asupersync::time::wall_now(), Duration::from_millis(200)).await;
        Ok(())
    })?;
    let timeout_limits = SessionChannelLimits {
        io_timeout: Duration::from_millis(20),
        ..limits()
    };
    run_runtime(|cx| async move {
        let error = connect_authenticated_session(&cx, address, initiator_config(timeout_limits))
            .await
            .expect_err("silent peer must hit the handshake read deadline");
        if !matches!(error, SessionChannelError::Timeout { .. }) {
            return Err(format!("expected timeout, observed {error:?}"));
        }
        Ok(())
    })?;
    join_server(server)?;

    let (address, server) = spawn_server(move |_cx, _stream| async move {
        asupersync::time::sleep(asupersync::time::wall_now(), Duration::from_millis(200)).await;
        Ok(())
    })?;
    run_runtime(|cx| async move {
        let cancel = cx.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel.cancel_with(CancelKind::User, Some("cancel blocked mesh read"));
        });
        let error = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .expect_err("in-flight blocked handshake read must observe cancellation");
        canceller
            .join()
            .map_err(|_| "in-flight cancellation thread panicked".to_owned())?;
        if !matches!(error, SessionChannelError::Cancelled { .. }) {
            return Err(format!(
                "expected in-flight cancellation, observed {error:?}"
            ));
        }
        Ok(())
    })?;
    join_server(server)?;

    let (address, server) = spawn_server(move |_cx, _stream| async move {
        asupersync::time::sleep(asupersync::time::wall_now(), Duration::from_millis(200)).await;
        Ok(())
    })?;
    let started = std::time::Instant::now();
    run_runtime_for(Duration::from_millis(25), |cx| async move {
        let error = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .expect_err("caller Cx deadline must beat the two-second socket limit");
        if !matches!(
            error,
            SessionChannelError::Cancelled { .. } | SessionChannelError::Timeout { .. }
        ) {
            return Err(format!("expected clipped Cx deadline, observed {error:?}"));
        }
        Ok(())
    })?;
    if started.elapsed() >= Duration::from_millis(500) {
        return Err("caller Cx deadline did not clip the socket deadline".to_owned());
    }
    join_server(server)?;

    run_runtime(|cx| async move {
        cx.cancel_with(CancelKind::User, Some("loopback cancellation proof"));
        let error = connect_authenticated_session(
            &cx,
            "127.0.0.1:9"
                .parse()
                .map_err(|error| format!("parse address: {error}"))?,
            initiator_config(limits()),
        )
        .await
        .expect_err("cancelled context must refuse before connect");
        if !matches!(error, SessionChannelError::Cancelled { .. }) {
            return Err(format!("expected cancellation, observed {error:?}"));
        }
        Ok(())
    })
}

#[test]
fn authenticated_requested_budget_bounds_processing_without_mutation() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let server_mutations = Arc::clone(&mutations);
    let (address, server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, limits())
            .await
            .map_err(|error| error.to_string())?;
        let request = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("budget proof peer half-closed before request")?;
        let mutation = Arc::clone(&server_mutations);
        let error = session
            .process_request(&cx, &request, async move {
                asupersync::time::sleep(asupersync::time::wall_now(), Duration::from_millis(100))
                    .await;
                mutation.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .expect_err("authenticated 15ms request budget must cancel 100ms processing");
        if !matches!(error, SessionChannelError::Timeout { .. }) {
            return Err(format!("request budget emitted {error:?}"));
        }
        if server_mutations.load(Ordering::SeqCst) != 0 {
            return Err("over-budget request performed application mutation".to_owned());
        }
        Ok(())
    })?;
    run_runtime(|cx| async move {
        let mut session = connect_authenticated_session(&cx, address, initiator_config(limits()))
            .await
            .map_err(|error| error.to_string())?;
        session
            .send_request(
                &cx,
                SessionMessage {
                    correlation_id: "processing-budget".to_owned(),
                    capability: FrameCapability::Summary,
                    requested_budget_ms: 15,
                    payload: json!({"mutation": "only-after-success"}),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        asupersync::time::sleep(asupersync::time::wall_now(), Duration::from_millis(150)).await;
        Ok(())
    })?;
    join_server(server)?;
    if mutations.load(Ordering::SeqCst) != 0 {
        return Err("request budget failure changed application state".to_owned());
    }
    Ok(())
}

#[test]
fn kill_switch_refuses_connect_and_accepted_paths_before_authentication() -> TestResult {
    const CHILD_MARKER: &str = "EE_MESH_TRANSPORT_KILL_SWITCH_CHILD";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let output = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .arg("--exact")
            .arg("kill_switch_refuses_connect_and_accepted_paths_before_authentication")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("EE_MESH_TRANSPORT_DISABLED", "1")
            .output()
            .map_err(|error| format!("spawn kill-switch proof child: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "kill-switch proof child failed\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        return Ok(());
    }

    run_runtime(|cx| async move {
        let connect_error = connect_authenticated_session(
            &cx,
            "127.0.0.1:9"
                .parse()
                .map_err(|error| format!("parse address: {error}"))?,
            initiator_config(limits()),
        )
        .await
        .expect_err("kill switch must refuse connect before socket work");
        if connect_error != SessionChannelError::TransportDisabled {
            return Err(format!("unexpected connect refusal: {connect_error:?}"));
        }

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| format!("bind accepted-path proof: {error}"))?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let connector = thread::spawn(move || std::net::TcpStream::connect(address));
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        connector
            .join()
            .map_err(|_| "accepted-path connector panicked".to_owned())?
            .map_err(|error| format!("accepted-path connector failed: {error}"))?;
        let accepted_error = accept_loopback_session(&cx, stream, limits())
            .await
            .expect_err("kill switch must refuse accepted stream before authentication");
        if accepted_error != SessionChannelError::TransportDisabled {
            return Err(format!("unexpected accepted refusal: {accepted_error:?}"));
        }
        Ok(())
    })
}

#[test]
fn invalid_kill_switch_value_fails_closed_on_both_paths() -> TestResult {
    const CHILD_MARKER: &str = "EE_MESH_TRANSPORT_INVALID_SWITCH_CHILD";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let output = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .arg("--exact")
            .arg("invalid_kill_switch_value_fails_closed_on_both_paths")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("EE_MESH_TRANSPORT_DISABLED", "definitely-not-a-boolean")
            .output()
            .map_err(|error| format!("spawn invalid-switch proof child: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "invalid-switch proof child failed\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        return Ok(());
    }

    run_runtime(|cx| async move {
        let expected = SessionChannelError::InvalidConfiguration {
            variable: "EE_MESH_TRANSPORT_DISABLED",
        };
        let connect_error = connect_authenticated_session(
            &cx,
            "127.0.0.1:9"
                .parse()
                .map_err(|error| format!("parse address: {error}"))?,
            initiator_config(limits()),
        )
        .await
        .expect_err("invalid emergency switch must fail before connect");
        if connect_error != expected {
            return Err(format!(
                "unexpected invalid connect error: {connect_error:?}"
            ));
        }

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| format!("bind invalid accepted proof: {error}"))?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let connector = thread::spawn(move || std::net::TcpStream::connect(address));
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        connector
            .join()
            .map_err(|_| "invalid accepted connector panicked".to_owned())?
            .map_err(|error| format!("invalid accepted connector failed: {error}"))?;
        let accepted_error = accept_loopback_session(&cx, stream, limits())
            .await
            .expect_err("invalid emergency switch must fail before accepted authentication");
        if accepted_error != expected {
            return Err(format!(
                "unexpected invalid accepted error: {accepted_error:?}"
            ));
        }
        Ok(())
    })
}

#[test]
fn checked_max_counter_exhaustion_cannot_reuse_terminal_value() -> TestResult {
    let (address, server) = spawn_server(move |_cx, mut stream| async move {
        let established = raw_established_for_max();
        let mut counters = SessionCounters::expecting(NonZeroU64::MAX);
        let first =
            decode_frame(&read_packet(&mut stream).await?).map_err(|error| error.to_string())?;
        verify_frame(
            &first,
            &established.binding,
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &established.keys,
            &NegotiatedExtensions::none(),
        )
        .map_err(|error| format!("first MAX frame must verify: {error}"))?;
        let replay =
            decode_frame(&read_packet(&mut stream).await?).map_err(|error| error.to_string())?;
        let error = verify_frame(
            &replay,
            &established.binding,
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &established.keys,
            &NegotiatedExtensions::none(),
        )
        .expect_err("terminal MAX counter cannot be accepted twice");
        if error.degraded_code() != "mesh_frame_replay_rejected" {
            return Err(format!("MAX exhaustion emitted {}", error.degraded_code()));
        }
        Ok(())
    })?;
    run_runtime(|_cx| async move {
        let mut stream = TcpStream::connect(address)
            .await
            .map_err(|error| error.to_string())?;
        let established = raw_established_for_max();
        let frame = sign_frame(
            &established.binding,
            &established.keys,
            FrameDraft {
                direction: SessionDirection::InitiatorToResponder,
                counter: u64::MAX,
                correlation_id: "max-terminal".to_owned(),
                kind: FrameKind::Request,
                capability: FrameCapability::Summary,
                requested_budget_ms: 1,
                payload: json!({"terminal": true}),
            },
        )
        .map_err(|error| error.to_string())?;
        write_json(&mut stream, &frame).await?;
        write_json(&mut stream, &frame).await
    })?;
    join_server(server)
}

#[test]
fn fresh_responder_nonce_rejects_cross_session_frame_replay() -> TestResult {
    let mutations = Arc::new(AtomicUsize::new(0));
    let first_mutations = Arc::clone(&mutations);
    let (first_address, first_server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, limits())
            .await
            .map_err(|error| error.to_string())?;
        let request = session
            .receive_request(&cx)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("first session half-closed before replay fixture")?;
        if request.payload != json!({"replay": "fixture"}) {
            return Err("first session did not authenticate replay fixture".to_owned());
        }
        first_mutations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })?;
    let (old_frame, first_key) = run_runtime(|_cx| async move {
        let (mut stream, established) = raw_initiator(first_address).await?;
        let frame = sign_frame(
            &established.binding,
            &established.keys,
            FrameDraft {
                direction: SessionDirection::InitiatorToResponder,
                counter: 2,
                correlation_id: "cross-session-replay".to_owned(),
                kind: FrameKind::Request,
                capability: FrameCapability::Summary,
                requested_budget_ms: 250,
                payload: json!({"replay": "fixture"}),
            },
        )
        .map_err(|error| error.to_string())?;
        let encoded = serde_json::to_vec(&frame).map_err(|error| error.to_string())?;
        write_packet(&mut stream, &encoded).await?;
        Ok((encoded, *established.keys.initiator_to_responder.as_bytes()))
    })?;
    join_server(first_server)?;

    let second_mutations = Arc::clone(&mutations);
    let (second_address, second_server) = spawn_server(move |cx, stream| async move {
        let mut session = accept_loopback_session(&cx, stream, limits())
            .await
            .map_err(|error| error.to_string())?;
        let error = session
            .receive_request(&cx)
            .await
            .expect_err("prior-session frame must fail under fresh directional keys");
        if error.degraded_code() != "mesh_frame_auth_failed" {
            return Err(format!(
                "cross-session replay emitted {}",
                error.degraded_code()
            ));
        }
        if second_mutations.load(Ordering::SeqCst) != 1 {
            return Err("cross-session replay performed application mutation".to_owned());
        }
        Ok(())
    })?;
    let second_key = run_runtime(|_cx| async move {
        let (mut stream, established) = raw_initiator(second_address).await?;
        write_packet(&mut stream, &old_frame).await?;
        Ok(*established.keys.initiator_to_responder.as_bytes())
    })?;
    join_server(second_server)?;
    if first_key == second_key {
        return Err("fresh responder handshake reused a directional session key".to_owned());
    }
    if mutations.load(Ordering::SeqCst) != 1 {
        return Err("cross-session replay changed application mutation count".to_owned());
    }
    Ok(())
}

fn raw_established_for_max() -> EstablishedSession {
    let binding = binding();
    EstablishedSession {
        keys: ee::mesh::transport_session::derive_session_keys(
            &pair_key(),
            &binding,
            &[0x11; 32],
            &[0x22; 32],
        ),
        binding,
        inbound: SessionCounters::new(),
        next_outbound: 1,
    }
}

#[test]
fn transport_wire_schema_embedded_catalog_matches_documents() -> TestResult {
    let schemas = ee::mesh::transport_session::TRANSPORT_WIRE_SCHEMAS;
    if schemas.len() != 5 {
        return Err(format!(
            "expected five transport wire schemas, got {}",
            schemas.len()
        ));
    }
    for schema in schemas {
        let document: serde_json::Value = serde_json::from_str(schema.document)
            .map_err(|error| format!("{} is not JSON: {error}", schema.id))?;
        if document.get("$id").and_then(serde_json::Value::as_str) != Some(schema.id) {
            return Err(format!("{} embedded schema id drifted", schema.id));
        }
    }
    Ok(())
}

#[test]
fn transport_wire_schemas_are_publicly_registered_once() -> TestResult {
    let public = ee::output::public_schemas();
    for schema in ee::mesh::transport_session::TRANSPORT_WIRE_SCHEMAS {
        let matches = public.iter().filter(|entry| entry.id == schema.id).count();
        if matches != 1 {
            return Err(format!(
                "{} must appear exactly once in public_schemas, found {matches}",
                schema.id
            ));
        }
        let known_matches = ee::models::schema::KNOWN_SCHEMAS
            .iter()
            .filter(|known| **known == schema.id)
            .count();
        if known_matches != 1 {
            return Err(format!(
                "{} must appear exactly once in KNOWN_SCHEMAS, found {known_matches}",
                schema.id
            ));
        }

        let exported: serde_json::Value =
            serde_json::from_str(&ee::output::render_schema_export_json(Some(schema.id)))
                .map_err(|error| format!("{} export is not JSON: {error}", schema.id))?;
        if exported.get("$id").and_then(serde_json::Value::as_str) != Some(schema.id) {
            return Err(format!("{} public schema export drifted", schema.id));
        }
        let embedded: serde_json::Value = serde_json::from_str(schema.document)
            .map_err(|error| format!("{} embedded schema is not JSON: {error}", schema.id))?;
        if exported != embedded {
            return Err(format!(
                "{} public export differs from the transport-owned document",
                schema.id
            ));
        }
    }
    Ok(())
}

fn schema_required_fields(schema: &serde_json::Value) -> TestResult<Vec<&str>> {
    schema
        .pointer("/required")
        .and_then(serde_json::Value::as_array)
        .ok_or("schema required[] is missing".to_owned())?
        .iter()
        .map(|field| {
            field
                .as_str()
                .ok_or("required field is not a string".to_owned())
        })
        .collect()
}

fn validate_closed_object(schema: &serde_json::Value, instance: &serde_json::Value) -> TestResult {
    let object = instance
        .as_object()
        .ok_or("schema instance must be an object".to_owned())?;
    let properties = schema
        .pointer("/properties")
        .and_then(serde_json::Value::as_object)
        .ok_or("schema properties object is missing".to_owned())?;
    for required in schema_required_fields(schema)? {
        if !object.contains_key(required) {
            return Err(format!("instance is missing required field {required}"));
        }
    }
    if let Some(unexpected) = object.keys().find(|key| !properties.contains_key(*key)) {
        return Err(format!("instance contains additional field {unexpected}"));
    }
    Ok(())
}

fn schema_pattern_accepts(
    schema: &serde_json::Value,
    pointer: &str,
    value: &str,
) -> TestResult<bool> {
    let pattern = schema
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("schema pattern is missing at {pointer}"))?;
    Regex::new(pattern)
        .map_err(|error| format!("invalid schema pattern {pattern:?}: {error}"))
        .map(|regex| regex.is_match(value))
}

fn validate_capability_negotiation_instance(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
) -> TestResult {
    validate_closed_object(schema, instance)?;
    let expected_schema = schema
        .pointer("/properties/schema/const")
        .and_then(serde_json::Value::as_str)
        .ok_or("capability schema const is missing".to_owned())?;
    if instance
        .pointer("/schema")
        .and_then(serde_json::Value::as_str)
        != Some(expected_schema)
    {
        return Err("capability instance carries the wrong schema".to_owned());
    }
    let phase = instance
        .pointer("/phase")
        .and_then(serde_json::Value::as_str)
        .ok_or("capability phase is not a string".to_owned())?;
    let phases = schema
        .pointer("/properties/phase/enum")
        .and_then(serde_json::Value::as_array)
        .ok_or("capability phase enum is missing".to_owned())?;
    if !phases
        .iter()
        .any(|candidate| candidate.as_str() == Some(phase))
    {
        return Err(format!("capability phase {phase:?} is not allowed"));
    }
    let capabilities = instance
        .pointer("/capabilities")
        .and_then(serde_json::Value::as_array)
        .ok_or("capabilities is not an array".to_owned())?;
    let min = schema
        .pointer("/properties/capabilities/minItems")
        .and_then(serde_json::Value::as_u64)
        .ok_or("capability minItems is missing".to_owned())? as usize;
    let max = schema
        .pointer("/properties/capabilities/maxItems")
        .and_then(serde_json::Value::as_u64)
        .ok_or("capability maxItems is missing".to_owned())? as usize;
    if !(min..=max).contains(&capabilities.len()) {
        return Err("capability count is outside the schema bounds".to_owned());
    }
    let mut unique = std::collections::BTreeSet::new();
    for capability in capabilities {
        let token = capability
            .as_str()
            .ok_or("capability token is not a string".to_owned())?;
        if !schema_pattern_accepts(schema, "/properties/capabilities/items/pattern", token)? {
            return Err(format!(
                "capability token {token:?} violates the schema pattern"
            ));
        }
        if !unique.insert(token) {
            return Err(format!("capability token {token:?} is duplicated"));
        }
    }
    let required_token = schema
        .pointer("/properties/capabilities/contains/const")
        .and_then(serde_json::Value::as_str)
        .ok_or("capability contains.const is missing".to_owned())?;
    if !unique.contains(required_token) {
        return Err(format!(
            "capability set is missing required token {required_token:?}"
        ));
    }
    Ok(())
}

fn validate_frame_instance(schema: &serde_json::Value, instance: &serde_json::Value) -> TestResult {
    validate_closed_object(schema, instance)?;
    let expected_schema = schema
        .pointer("/properties/schema/const")
        .and_then(serde_json::Value::as_str)
        .ok_or("frame schema const is missing".to_owned())?;
    if instance
        .pointer("/schema")
        .and_then(serde_json::Value::as_str)
        != Some(expected_schema)
    {
        return Err("frame instance carries the wrong schema".to_owned());
    }
    for (field, pattern_pointer) in [
        ("capability", "/properties/capability/pattern"),
        ("payloadHash", "/properties/payloadHash/pattern"),
        ("mac", "/properties/mac/pattern"),
    ] {
        let value = instance
            .get(field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("frame field {field} is not a string"))?;
        if !schema_pattern_accepts(schema, pattern_pointer, value)? {
            return Err(format!("frame field {field} violates its schema pattern"));
        }
    }
    Ok(())
}

fn validate_handshake_instance(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
) -> TestResult {
    validate_closed_object(schema, instance)?;
    let expected_schema = schema
        .pointer("/properties/schema/const")
        .and_then(serde_json::Value::as_str)
        .ok_or("handshake schema const is missing".to_owned())?;
    if instance
        .pointer("/schema")
        .and_then(serde_json::Value::as_str)
        != Some(expected_schema)
    {
        return Err("handshake instance carries the wrong schema".to_owned());
    }
    let properties = schema
        .pointer("/properties")
        .and_then(serde_json::Value::as_object)
        .ok_or("handshake properties are missing".to_owned())?;
    for (field, field_schema) in properties {
        let Some(value) = instance.get(field) else {
            continue;
        };
        if let Some(pattern) = field_schema
            .get("pattern")
            .and_then(serde_json::Value::as_str)
        {
            let text = value
                .as_str()
                .ok_or_else(|| format!("handshake field {field} is not a string"))?;
            let regex = Regex::new(pattern)
                .map_err(|error| format!("invalid handshake pattern {pattern:?}: {error}"))?;
            if !regex.is_match(text) {
                return Err(format!("handshake field {field} violates its pattern"));
            }
        }
        if let Some(minimum) = field_schema
            .get("minimum")
            .and_then(serde_json::Value::as_u64)
            && value.as_u64().is_none_or(|number| number < minimum)
        {
            return Err(format!("handshake field {field} is below its minimum"));
        }
        if let Some(text) = value.as_str() {
            if let Some(minimum) = field_schema
                .get("minLength")
                .and_then(serde_json::Value::as_u64)
                && text.len() < minimum as usize
            {
                return Err(format!("handshake field {field} is too short"));
            }
            if let Some(maximum) = field_schema
                .get("maxLength")
                .and_then(serde_json::Value::as_u64)
                && text.len() > maximum as usize
            {
                return Err(format!("handshake field {field} is too long"));
            }
        }
    }
    Ok(())
}

#[test]
fn transport_wire_schemas_validate_valid_and_near_identical_invalid_instances() -> TestResult {
    let schemas = ee::mesh::transport_session::TRANSPORT_WIRE_SCHEMAS;
    let capability_schema = schemas
        .iter()
        .find(|schema| schema.id == CAPABILITY_NEGOTIATION_SCHEMA_V1)
        .ok_or("capability schema is not embedded".to_owned())?;
    let capability_schema: serde_json::Value =
        serde_json::from_str(capability_schema.document).map_err(|error| error.to_string())?;
    let valid_negotiation = json!({
        "schema": CAPABILITY_NEGOTIATION_SCHEMA_V1,
        "phase": "offer",
        "capabilities": ["hello", "summary"]
    });
    validate_capability_negotiation_instance(&capability_schema, &valid_negotiation)?;
    for invalid in [
        json!({
            "schema": CAPABILITY_NEGOTIATION_SCHEMA_V1,
            "phase": "offer",
            "capabilities": ["summary"]
        }),
        json!({
            "schema": CAPABILITY_NEGOTIATION_SCHEMA_V1,
            "phase": "offer",
            "capabilities": ["hello", "hello"]
        }),
        json!({
            "schema": CAPABILITY_NEGOTIATION_SCHEMA_V1,
            "phase": "offer",
            "capabilities": ["hello", "Bad-Token"]
        }),
    ] {
        if validate_capability_negotiation_instance(&capability_schema, &invalid).is_ok() {
            return Err(format!("invalid capability instance passed: {invalid}"));
        }
    }
    if SessionCapabilities::new(["summary".to_owned()]).is_ok()
        || SessionCapabilities::new(["hello".to_owned(), "hello".to_owned()]).is_ok()
        || SessionCapabilities::new(["hello".to_owned(), "Bad-Token".to_owned()]).is_ok()
    {
        return Err("runtime capability validation drifted from its schema".to_owned());
    }

    let frame_schema = schemas
        .iter()
        .find(|schema| schema.id == ee::mesh::transport_session::TRANSPORT_FRAME_SCHEMA_V2)
        .ok_or("frame-v2 schema is not embedded".to_owned())?;
    let frame_schema: serde_json::Value =
        serde_json::from_str(frame_schema.document).map_err(|error| error.to_string())?;
    let session_binding = binding();
    let keys = ee::mesh::transport_session::derive_session_keys(
        &pair_key(),
        &session_binding,
        &[0x11; 32],
        &[0x22; 32],
    );
    let frame = sign_frame(
        &session_binding,
        &keys,
        FrameDraft {
            direction: SessionDirection::InitiatorToResponder,
            counter: 1,
            correlation_id: "schema-frame".to_owned(),
            kind: FrameKind::Request,
            capability: FrameCapability::Summary,
            requested_budget_ms: 1,
            payload: json!({"valid": true}),
        },
    )
    .map_err(|error| error.to_string())?;
    let valid_frame = serde_json::to_value(frame).map_err(|error| error.to_string())?;
    validate_frame_instance(&frame_schema, &valid_frame)?;
    let mut invalid_frame = valid_frame.clone();
    invalid_frame["capability"] = json!("Bad-Token");
    if validate_frame_instance(&frame_schema, &invalid_frame).is_ok() {
        return Err("invalid frame capability token passed the wire schema".to_owned());
    }
    invalid_frame = valid_frame;
    invalid_frame
        .as_object_mut()
        .ok_or("valid frame is not an object".to_owned())?
        .remove("mac");
    if validate_frame_instance(&frame_schema, &invalid_frame).is_ok() {
        return Err("frame missing required MAC passed the wire schema".to_owned());
    }

    let (initiator, open) = InitiatorHandshake::open(binding(), [0x31; 32], 7, observations())
        .map_err(|error| error.to_string())?;
    let accepted = accepted_config(limits());
    let pair_key = pair_key();
    let (responder, confirm) = responder_accept_open(
        &open,
        &accepted.expectations,
        [0x42; 32],
        accepted.observations,
        &pair_key,
    )
    .map_err(|error| error.to_string())?;
    let (finish, _) = initiator
        .finish(&pair_key, &confirm)
        .map_err(|error| error.to_string())?;
    responder
        .complete(&pair_key, &finish)
        .map_err(|error| error.to_string())?;
    for (schema_id, valid) in [
        (
            ee::mesh::transport_session::SESSION_OPEN_SCHEMA_V1,
            serde_json::to_value(open).map_err(|error| error.to_string())?,
        ),
        (
            ee::mesh::transport_session::SESSION_CONFIRM_SCHEMA_V1,
            serde_json::to_value(confirm).map_err(|error| error.to_string())?,
        ),
        (
            ee::mesh::transport_session::SESSION_FINISH_SCHEMA_V1,
            serde_json::to_value(finish).map_err(|error| error.to_string())?,
        ),
    ] {
        let schema = schemas
            .iter()
            .find(|schema| schema.id == schema_id)
            .ok_or_else(|| format!("handshake schema {schema_id} is not embedded"))?;
        let schema: serde_json::Value =
            serde_json::from_str(schema.document).map_err(|error| error.to_string())?;
        validate_handshake_instance(&schema, &valid)?;
        let mut invalid = valid;
        let required = schema_required_fields(&schema)?
            .into_iter()
            .find(|field| *field != "schema")
            .ok_or_else(|| format!("handshake schema {schema_id} has no required payload field"))?;
        invalid
            .as_object_mut()
            .ok_or("handshake instance is not an object".to_owned())?
            .remove(required);
        if validate_handshake_instance(&schema, &invalid).is_ok() {
            return Err(format!(
                "handshake instance missing required {required} passed {schema_id}"
            ));
        }
    }
    Ok(())
}
