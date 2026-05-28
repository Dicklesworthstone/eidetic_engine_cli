//! Unix-domain socket accept loop + per-connection dispatcher
//! (bd-oja31 skeleton). Wraps the framing in
//! [`super::protocol`] with the seed dispatch table for
//! `ee.daemon.echo` and the `ee.daemon.context` stub.
//!
//! Threading: each accepted connection is dispatched onto a bounded
//! worker pool (capped at [`super::DAEMON_MAX_INFLIGHT`], overridable
//! via `EE_DAEMON_MAX_INFLIGHT`). When the pool is saturated the
//! accept loop refuses the new connection with a framed
//! `daemon_overloaded` response and closes it rather than queueing —
//! that keeps total daemon RSS amplification bounded at
//! `inflight × per-worker-footprint` rather than `unbounded × per-worker`.
//! A shutdown signal (an `Arc<AtomicBool>`) is checked between
//! accepts; the accept loop breaks on the next iteration when the
//! signal flips. A follow-up slice will wrap this with Asupersync
//! supervision; the wire framing and dispatch table are stable
//! across that refactor. See bd-jnyui for the bounded-pool fix.
//!
//! Platform: this module is `#[cfg(unix)]`; Windows builds skip it and
//! the CLI handler short-circuits with
//! [`super::DaemonStartError::PlatformUnsupported`].

#![cfg(unix)]

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use crate::config::env_registry::{self, EnvVar};

use super::protocol::{
    DaemonRequest, DaemonResponse, FrameReadError, FrameWriteError, read_request, write_response,
};
use super::{
    DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE, DAEMON_DEFAULT_RPC_TIMEOUT, DAEMON_MAX_INFLIGHT,
    DAEMON_OVERLOADED_CODE, DAEMON_PEER_UNAUTHORIZED_CODE, DaemonStartError, current_euid,
};

/// Method dispatch name for the round-trip integrity check.
pub const METHOD_ECHO: &str = "ee.daemon.echo";

/// Method dispatch name for the warm-loaded `ee context` stub.
pub const METHOD_CONTEXT: &str = "ee.daemon.context";

/// Error code returned when a request's `method` field does not match
/// any registered handler.
pub const DAEMON_UNKNOWN_METHOD_CODE: &str = "daemon_unknown_method";

/// Error code returned when the request envelope failed to decode
/// (malformed JSON, missing required field, schema mismatch).
pub const DAEMON_REQUEST_DECODE_FAILED_CODE: &str = "daemon_request_decode_failed";

/// Error code returned when the request envelope's `schema` field does
/// not match [`super::DAEMON_REQUEST_SCHEMA_V1`].
pub const DAEMON_REQUEST_SCHEMA_MISMATCH_CODE: &str = "daemon_request_schema_mismatch";

/// Error code returned when a connection-handler panics mid-dispatch.
/// [`handle_connection`] wraps the dispatch call in
/// `std::panic::catch_unwind` so the accept loop survives, the client
/// receives a structured `daemon_handler_panic` envelope instead of a
/// torn-down connection, and the panic message is sanitized before
/// being logged to the daemon's stderr — never written back on the
/// wire — so workspace ids, memory body bytes, or anything else that
/// happened to be on the stack at the panic site cannot leak through
/// the response. bd-b82q4.
pub const DAEMON_HANDLER_PANIC_CODE: &str = "daemon_handler_panic";

/// Hard cap on the byte length of a sanitized panic message written
/// to the daemon's stderr by [`handle_connection`]. The wire envelope
/// never carries panic details; this only bounds the log line so a
/// pathological panic payload (e.g. a `Display` impl that walked a
/// huge memory body) cannot blow out the journal.
const DAEMON_PANIC_LOG_MAX_BYTES: usize = 512;

/// Bounded-pool permit. A clone-on-acquire counter that decrements on
/// drop, used to cap the number of in-flight per-connection worker
/// threads. The cap defends against the bd-jnyui local-DoS vector:
/// before this gate, a single attacker could fork-bomb the daemon
/// because `run_accept_loop` spawned a new `std::thread::Builder` per
/// accept with no semaphore or queue depth.
#[derive(Debug)]
struct InflightPermit {
    pool: Arc<InflightPool>,
}

impl Drop for InflightPermit {
    fn drop(&mut self) {
        let mut current = self
            .pool
            .inflight
            .lock()
            .expect("daemon inflight mutex must not be poisoned");
        *current = current.saturating_sub(1);
    }
}

/// Shared, hand-rolled counting semaphore for the accept loop. The
/// counter is bumped on `try_acquire` (returning a permit) and
/// decremented when the permit is dropped — typically when the worker
/// thread exits.
#[derive(Debug)]
struct InflightPool {
    capacity: usize,
    inflight: Mutex<usize>,
}

impl InflightPool {
    fn new(capacity: usize) -> Arc<Self> {
        // A capacity of zero would refuse every connection and is never
        // useful in production. Clamp to one so an operator misreading
        // the env var still gets a serializable daemon rather than a
        // dead socket.
        let capacity = capacity.max(1);
        Arc::new(Self {
            capacity,
            inflight: Mutex::new(0),
        })
    }

    fn try_acquire(self: &Arc<Self>) -> Option<InflightPermit> {
        let mut current = self
            .inflight
            .lock()
            .expect("daemon inflight mutex must not be poisoned");
        if *current >= self.capacity {
            return None;
        }
        *current += 1;
        Some(InflightPermit {
            pool: Arc::clone(self),
        })
    }
}

/// Resolve the configured per-daemon worker cap. Reads
/// `EE_DAEMON_MAX_INFLIGHT` through the central env registry so the
/// override is reflected in `ee capabilities` alongside other tuning
/// knobs; a missing, unparseable, or zero value falls back to
/// [`super::DAEMON_MAX_INFLIGHT`].
fn configured_max_inflight() -> usize {
    env_registry::read(EnvVar::DaemonMaxInflight)
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DAEMON_MAX_INFLIGHT)
}

/// Handle returned by [`start_server`]. Holds the accept-loop thread
/// and the shutdown signal; dropping it does NOT stop the server
/// (callers must call [`DaemonServerHandle::shutdown`] explicitly so
/// the socket file is unlinked deterministically).
#[derive(Debug)]
pub struct DaemonServerHandle {
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl DaemonServerHandle {
    /// Return the bound socket path for status surfaces / tests.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Signal the accept loop to stop and wait for it to drain. Also
    /// removes the socket file from the filesystem so a subsequent
    /// `start_server` call against the same path does not need a
    /// manual cleanup step.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the accept loop by connecting to the socket from the
        // current process; the loop checks the shutdown flag between
        // accepts but blocks inside `accept()` itself. The connect-
        // and-immediately-drop pattern unblocks the listener cheaply.
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(handle) = self.accept_thread.take() {
            handle
                .join()
                .map_err(|_| io::Error::other("daemon accept thread panicked"))?;
        }
        if self.socket_path.exists() {
            fs::remove_file(&self.socket_path)?;
        }
        Ok(())
    }
}

impl Drop for DaemonServerHandle {
    fn drop(&mut self) {
        // Best-effort cleanup: signal shutdown and join the accept
        // thread so the socket file is unlinked even if the caller
        // forgot to call `shutdown`. Errors are intentionally
        // swallowed in Drop; the explicit `shutdown` method surfaces
        // them when callers care.
        let _ = self.shutdown();
    }
}

/// Bind a UDS at `socket_path` and spawn the accept loop. Returns a
/// handle the caller can use to query the bound path and to signal
/// shutdown. The accept loop runs until [`DaemonServerHandle::shutdown`]
/// is called or the handle is dropped.
///
/// The function refuses to overwrite a non-socket file at
/// `socket_path`; the operator must remove a stale path explicitly so
/// the daemon never silently truncates a regular file someone
/// accidentally pointed it at.
pub fn start_server(
    socket_path: impl Into<PathBuf>,
) -> Result<DaemonServerHandle, DaemonStartError> {
    let socket_path = socket_path.into();

    if let Some(parent) = socket_path.parent()
        && !parent.as_os_str().is_empty()
    {
        // The parent directory is created with mode 0o700 so it
        // partitions across local tenants on hosts that share TMPDIR
        // (the macOS / no-XDG_RUNTIME_DIR fallback). DirBuilder with
        // `recursive(true)` and `mode(0o700)` applies the mode to
        // every component it creates and is a no-op on directories
        // that already exist with another mode — that matches
        // `create_dir_all`'s ignore-existing semantics and avoids
        // tightening a pre-existing operator-managed directory
        // unexpectedly. Companion fix: bd-3j0td.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|source| DaemonStartError::SocketDirCreate {
                path: parent.to_path_buf(),
                source,
            })?;
    }

    match fs::symlink_metadata(&socket_path) {
        Ok(metadata) => {
            // The path exists. Only proceed if it is itself a socket
            // (a stale daemon left it behind); refuse anything else.
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if !metadata.file_type().is_socket() {
                    return Err(DaemonStartError::SocketPathOccupied { path: socket_path });
                }
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                return Err(DaemonStartError::PlatformUnsupported);
            }
            // Stale socket: unlink so bind succeeds.
            fs::remove_file(&socket_path).map_err(|source| DaemonStartError::Bind {
                path: socket_path.clone(),
                source,
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(DaemonStartError::Bind {
                path: socket_path,
                source,
            });
        }
    }

    let listener = UnixListener::bind(&socket_path).map_err(|source| DaemonStartError::Bind {
        path: socket_path.clone(),
        source,
    })?;

    // Tighten the socket file to mode 0o600 immediately after bind.
    // `UnixListener::bind` honours the process umask, which is
    // typically 0o022 → mode 0o755 on the resulting socket: world-
    // connectable on every Unix host. Without this chmod, any local
    // UID can `connect(2)` and reach the dispatch table — the
    // pre-fix attack surface documented in bd-3j0td. The chmod is
    // best-effort but failures are surfaced so an operator running
    // on a filesystem that rejects `chmod` (rare, but possible on
    // certain network mounts) sees the bind step error rather than
    // a silently world-open socket. Sentinel: bd-3j0td chmod-0600.
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        // The socket is bound but world-open at this instant; remove
        // it before returning so a half-secured artifact does not
        // linger. The remove is best-effort — the chmod failure is
        // the actionable error.
        let _ = fs::remove_file(&socket_path);
        DaemonStartError::Bind {
            path: socket_path.clone(),
            source,
        }
    })?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_in_thread = Arc::clone(&shutdown);
    let listener_path_in_thread = socket_path.clone();
    let pool = InflightPool::new(configured_max_inflight());

    let accept_thread = thread::Builder::new()
        .name("ee-daemon-accept".to_owned())
        .spawn(move || {
            run_accept_loop(listener, listener_path_in_thread, shutdown_in_thread, pool);
        })
        .map_err(|source| DaemonStartError::Bind {
            path: socket_path.clone(),
            source,
        })?;

    Ok(DaemonServerHandle {
        socket_path,
        shutdown,
        accept_thread: Some(accept_thread),
    })
}

fn run_accept_loop(
    listener: UnixListener,
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    pool: Arc<InflightPool>,
) {
    let _ = socket_path; // reserved for future tracing.
    for incoming in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match incoming {
            Ok(stream) => {
                if let Some(permit) = pool.try_acquire() {
                    let spawn_result = thread::Builder::new()
                        .name("ee-daemon-conn".to_owned())
                        .spawn(move || {
                            // Permit is held for the lifetime of the
                            // worker; on drop the counter decrements
                            // and the next accept can proceed.
                            handle_connection(stream);
                            drop(permit);
                        });
                    if let Err(error) = spawn_result {
                        // Thread spawn itself failed (resource
                        // exhaustion). Drop the permit immediately so
                        // the next accept does not see a stuck
                        // counter, and refuse the client with the
                        // same overloaded code — from the client's
                        // perspective the daemon is at capacity even
                        // if the cause was OS-level rather than the
                        // bounded pool itself.
                        let _ = error;
                        let mut rejected = stream;
                        write_overloaded_response(&mut rejected);
                    }
                } else {
                    // Pool saturated — refuse the connection with a
                    // framed daemon_overloaded response and close.
                    // Queueing was rejected as a fix because every
                    // queued connection still holds a UDS file
                    // descriptor and a per-peer 30s read timeout
                    // budget; backpressure must propagate to the
                    // client immediately so it can fall back to
                    // in-process execution.
                    let mut rejected = stream;
                    write_overloaded_response(&mut rejected);
                }
            }
            Err(error) => {
                // Accept errors that are not transient bring down the
                // accept loop; the next `start_server` call after the
                // operator fixes the cause will bind fresh.
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }
}

/// Write a framed `daemon_overloaded` response on a connection that
/// was accepted but for which no worker slot is available, then close.
/// The response uses the canonical envelope shape so consumers parse
/// it through the same `read_response` path as any other daemon
/// reply; we do not yet know the client's `request_id` (we never
/// read the request frame), so it is set to `"<overloaded>"`.
fn write_overloaded_response(stream: &mut UnixStream) {
    // Use a tight write timeout so a dead client cannot block the
    // accept loop. The accept loop is single-threaded; a slow client
    // here would amplify the very DoS the bounded pool defends
    // against.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let response = DaemonResponse::err(
        "<overloaded>",
        DAEMON_OVERLOADED_CODE,
        "daemon worker pool saturated; retry after existing connections drain or fall back \
         to the in-process CLI path.",
    )
    .with_degraded(DAEMON_OVERLOADED_CODE);
    let _ = write_response(stream, &response);
}

fn handle_connection(mut stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT));
    let _ = stream.set_write_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT));

    // Peer-credential gate (bd-3j0td). Even with the socket file
    // tightened to 0o600 — and the per-UID parent dir at 0o700 — the
    // accept side validates the peer's effective UID against the
    // daemon process's own UID before dispatching. This is defense
    // in depth: the dispatch table currently bypasses the canonical
    // CLI redaction pipeline (see bd-3uev6), so the auth gate IS the
    // cross-tenant exfil defense until redaction is wired through.
    // We do not read the request frame on refusal because doing so
    // would leak the request body's framed length back to a peer the
    // daemon is intentionally refusing. Sentinel: bd-3j0td getpeereid.
    match peer_uid(&stream) {
        Ok(peer) => {
            let own = current_euid();
            if peer != own {
                let response = DaemonResponse::err(
                    "<unauthorized>",
                    DAEMON_PEER_UNAUTHORIZED_CODE,
                    format!(
                        "daemon refuses peer uid {peer}; only uid {own} (the daemon owner) \
                         may connect to this socket"
                    ),
                )
                .with_degraded(DAEMON_PEER_UNAUTHORIZED_CODE);
                let _ = write_response(&mut stream, &response);
                return;
            }
        }
        Err(error) => {
            // getpeereid/SO_PEERCRED itself failed — treat this as a
            // hard refusal rather than a silent allow. The framed
            // response carries the same `daemon_peer_unauthorized`
            // code so the client cannot distinguish "you are the
            // wrong UID" from "we could not verify your UID"; the
            // bias is intentional, the daemon must close on auth
            // ambiguity.
            let response = DaemonResponse::err(
                "<unauthorized>",
                DAEMON_PEER_UNAUTHORIZED_CODE,
                format!("daemon could not verify peer credential: {error}"),
            )
            .with_degraded(DAEMON_PEER_UNAUTHORIZED_CODE);
            let _ = write_response(&mut stream, &response);
            return;
        }
    }

    // Skeleton: one request per accepted connection. A follow-up
    // multiplexing slice will loop here so a single client can run
    // many RPCs over the same socket; the framing already supports
    // that because each frame is self-contained.
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(FrameReadError::Eof) => return,
        Err(other) => {
            let response = DaemonResponse::err(
                "<unknown>",
                DAEMON_REQUEST_DECODE_FAILED_CODE,
                other.to_string(),
            );
            let _ = write_response(&mut stream, &response);
            return;
        }
    };

    // Panic supervision (bd-b82q4): if `dispatch` (or any method it
    // delegates to in a future warm-load slice) panics, `catch_unwind`
    // converts the unwinding panic into an `Err(payload)` value. We
    // log a sanitized one-line summary to stderr and return a
    // structured `daemon_handler_panic` envelope to the client. The
    // accept loop is unaffected — sibling connections continue to be
    // served. Critically, the panic payload is NEVER written on the
    // wire: a panic that traversed a `Display` for a memory body or a
    // workspace path could otherwise leak that content to the
    // cross-tenant client.
    //
    // `AssertUnwindSafe` is sound here because the only state
    // captured by the closure is a borrow of `request`; the
    // `DaemonResponse` value is produced fresh inside the closure and
    // the `UnixStream` is touched only after the closure returns.
    let request_id = request.request_id.clone();
    let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(&request)));
    let response = match dispatched {
        Ok(response) => response,
        Err(payload) => build_panic_response(&request_id, payload.as_ref()),
    };
    let _ = write_response(&mut stream, &response);
}

/// Construct the structured envelope returned to a client whose
/// connection-handler panicked, and log a sanitized one-line summary
/// of the panic to the daemon's stderr. The wire envelope carries a
/// fixed generic message — never the raw panic payload — so a panic
/// inside a `Display` impl that touched a memory body cannot leak
/// that content to the client. bd-b82q4.
fn build_panic_response(request_id: &str, payload: &(dyn std::any::Any + Send)) -> DaemonResponse {
    let raw = extract_panic_payload_str(payload);
    let sanitized = sanitize_panic_message(&raw);
    // Single-line stderr log, capped, so a hostile panic payload
    // cannot blow out the journal nor inject log-forging characters.
    eprintln!("ee daemon handler panicked: {sanitized}");
    DaemonResponse::err(
        request_id.to_owned(),
        DAEMON_HANDLER_PANIC_CODE,
        "daemon handler panicked; the accept loop is intact, retry the request or fall back \
         to the in-process CLI path.",
    )
    .with_degraded(DAEMON_HANDLER_PANIC_CODE)
}

/// Best-effort extraction of a printable `&str` from a panic payload.
/// `std::panic::catch_unwind` boxes the payload as `Box<dyn Any +
/// Send>`. The two payload types Rust ships out of the box are
/// `&'static str` (for `panic!("literal")`) and `String` (for
/// `panic!("{fmt}", x)`); anything else is logged as `<non-string
/// panic payload>` rather than `Debug`-formatted, because `Debug` on
/// an unknown type could itself touch sensitive state.
fn extract_panic_payload_str(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

/// Sanitize a panic message for the daemon's stderr log: drop ASCII
/// control characters (defends against log-line injection via
/// embedded `\r`, `\n`, escape sequences), and truncate to
/// [`DAEMON_PANIC_LOG_MAX_BYTES`]. The sanitized output is never
/// written back to the client on the wire — only logged.
fn sanitize_panic_message(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(DAEMON_PANIC_LOG_MAX_BYTES));
    for ch in raw.chars() {
        if out.len() + ch.len_utf8() > DAEMON_PANIC_LOG_MAX_BYTES {
            out.push_str("...");
            break;
        }
        if ch.is_ascii_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Read the peer's effective UID from a connected `UnixStream`. Linux
/// exposes this through the `SO_PEERCRED` socket option (returning a
/// `ucred` struct populated by the kernel at `connect`-time); macOS
/// and the BSDs ship the dedicated `getpeereid(2)` syscall. The
/// per-platform paths produce the same `u32` UID so the
/// [`handle_connection`] gate is platform-agnostic. Sentinel:
/// bd-3j0td peer_uid.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    // SAFETY: zeroed `ucred` is a valid initial value (the kernel
    // overwrites every field on success). `getsockopt` reads through
    // the `&mut len` pointer to determine the buffer size and then
    // populates the buffer; the cast obeys the C-side ABI.
    let mut ucred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut ucred).cast::<libc::c_void>(),
            &raw mut len,
        )
    };
    if ret == 0 {
        Ok(ucred.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
))]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `getpeereid` writes through the two out-pointers and
    // returns 0 on success. We pass `&raw mut gid` to satisfy the ABI
    // but gate only on UID equality, so the gid value is intentionally
    // unused after the call.
    let ret = unsafe { libc::getpeereid(fd, &raw mut uid, &raw mut gid) };
    if ret == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
)))]
fn peer_uid(_stream: &UnixStream) -> io::Result<u32> {
    Err(io::Error::other(
        "peer credential lookup is not implemented on this Unix variant; \
         add a platform branch before exposing the daemon here",
    ))
}

/// Pure dispatch: map a parsed [`DaemonRequest`] to a
/// [`DaemonResponse`]. Exposed for unit tests that exercise the
/// dispatch table without paying for a UDS round-trip.
#[must_use]
pub fn dispatch(request: &DaemonRequest) -> DaemonResponse {
    if request.schema != super::DAEMON_REQUEST_SCHEMA_V1 {
        return DaemonResponse::err(
            request.request_id.clone(),
            DAEMON_REQUEST_SCHEMA_MISMATCH_CODE,
            format!(
                "expected schema `{}`, got `{}`",
                super::DAEMON_REQUEST_SCHEMA_V1,
                request.schema,
            ),
        );
    }

    match request.method.as_str() {
        METHOD_ECHO => DaemonResponse::ok(request.request_id.clone(), request.params.clone()),
        METHOD_CONTEXT => DaemonResponse::err(
            request.request_id.clone(),
            DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE,
            "ee.daemon.context is a stub until the ANN warm-load slice ships; \
             the CLI client should fall back to the in-process `ee context` path.",
        )
        .with_degraded(DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE),
        other => DaemonResponse::err(
            request.request_id.clone(),
            DAEMON_UNKNOWN_METHOD_CODE,
            format!("unknown daemon method `{other}`"),
        ),
    }
}

/// Open a UDS client connection to a running daemon and send exactly
/// one request, returning the parsed response. The CLI client uses
/// this for `ee daemon status` round-trip checks and for any future
/// hot-path proxy calls.
pub fn client_round_trip(
    socket_path: &Path,
    request: &DaemonRequest,
) -> Result<DaemonResponse, ClientError> {
    let mut stream = UnixStream::connect(socket_path).map_err(ClientError::Connect)?;
    stream
        .set_read_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT))
        .map_err(ClientError::Io)?;
    stream
        .set_write_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT))
        .map_err(ClientError::Io)?;

    let body = serde_json::to_vec(request).map_err(ClientError::Encode)?;
    use std::io::Write;
    let length = u32::try_from(body.len())
        .map_err(|_| ClientError::RequestTooLarge { actual: body.len() })?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(ClientError::Io)?;
    stream.write_all(&body).map_err(ClientError::Io)?;
    stream.flush().map_err(ClientError::Io)?;

    // Read the response with the same frame shape.
    let mut response_prefix = [0_u8; 4];
    use std::io::Read;
    stream
        .read_exact(&mut response_prefix)
        .map_err(ClientError::Io)?;
    let announced = u32::from_be_bytes(response_prefix);
    let announced_usize =
        usize::try_from(announced).map_err(|_| ClientError::ResponseTooLarge { announced })?;
    if announced_usize > super::DAEMON_RESPONSE_MAX_BYTES {
        return Err(ClientError::ResponseTooLarge { announced });
    }
    let mut buffer = vec![0_u8; announced_usize];
    stream.read_exact(&mut buffer).map_err(ClientError::Io)?;
    let response: DaemonResponse = serde_json::from_slice(&buffer).map_err(ClientError::Decode)?;
    Ok(response)
}

/// Errors that can occur on the client side of a UDS round-trip.
#[derive(Debug)]
pub enum ClientError {
    Connect(io::Error),
    Io(io::Error),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    RequestTooLarge { actual: usize },
    ResponseTooLarge { announced: u32 },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(source) => {
                write!(formatter, "failed to connect to daemon socket: {source}")
            }
            Self::Io(source) => write!(formatter, "io error during daemon round-trip: {source}"),
            Self::Encode(source) => write!(formatter, "failed to encode daemon request: {source}"),
            Self::Decode(source) => write!(formatter, "failed to decode daemon response: {source}"),
            Self::RequestTooLarge { actual } => write!(
                formatter,
                "daemon request is {actual} bytes which exceeds the {}-byte cap",
                super::DAEMON_REQUEST_MAX_BYTES
            ),
            Self::ResponseTooLarge { announced } => write!(
                formatter,
                "daemon response announced {announced} bytes which exceeds the {}-byte cap",
                super::DAEMON_RESPONSE_MAX_BYTES
            ),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(source) | Self::Io(source) => Some(source),
            Self::Encode(source) | Self::Decode(source) => Some(source),
            Self::RequestTooLarge { .. } | Self::ResponseTooLarge { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_echo_returns_params_unchanged() {
        let params = serde_json::json!({"k": "v", "n": 7});
        let request = DaemonRequest::new("req-echo-001", METHOD_ECHO, params.clone());
        let response = dispatch(&request);
        assert_eq!(response.request_id, "req-echo-001");
        assert_eq!(response.result, Some(params));
        assert!(response.error.is_none());
        assert!(response.degraded_codes.is_empty());
    }

    #[test]
    fn dispatch_context_returns_warmload_not_yet_implemented() {
        let request = DaemonRequest::new(
            "req-ctx-001",
            METHOD_CONTEXT,
            serde_json::json!({"task": "ship daemon"}),
        );
        let response = dispatch(&request);
        assert!(response.result.is_none());
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE.to_owned())
        );
    }

    #[test]
    fn dispatch_unknown_method_returns_unknown_method_code() {
        let request = DaemonRequest::new("req-unk-001", "ee.daemon.nope", Value::Null);
        let response = dispatch(&request);
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_UNKNOWN_METHOD_CODE);
    }

    #[test]
    fn dispatch_schema_mismatch_returns_schema_mismatch_code() {
        let bogus = DaemonRequest {
            schema: "ee.daemon.request.v0_wrong".to_owned(),
            request_id: "req-schema-001".to_owned(),
            method: METHOD_ECHO.to_owned(),
            params: Value::Null,
        };
        let response = dispatch(&bogus);
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_REQUEST_SCHEMA_MISMATCH_CODE);
    }

    /// bd-b82q4: a connection-handler panic must surface to the client
    /// as a structured `daemon_handler_panic` envelope rather than a
    /// torn-down connection, and the accept loop must keep running.
    /// `dispatch` has no panicking method today, so this test exercises
    /// the exact `catch_unwind` + `build_panic_response` composition
    /// that `handle_connection` runs, pinning the contract before a
    /// warm-load method that *can* panic lands behind the dispatch
    /// table.
    #[test]
    fn handle_connection_panicking_method_returns_structured_envelope_not_connection_reset() {
        let request_id = "req-panic-001";
        let dispatched =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> DaemonResponse {
                panic!("simulated warm-load bounds-check failure");
            }));
        let response = match dispatched {
            Ok(response) => response,
            Err(payload) => build_panic_response(request_id, payload.as_ref()),
        };
        // The client gets a real, parseable envelope — not a reset.
        assert_eq!(response.request_id, request_id);
        let error = response
            .error
            .as_ref()
            .expect("panic must yield an error envelope");
        assert_eq!(error.code, DAEMON_HANDLER_PANIC_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&DAEMON_HANDLER_PANIC_CODE.to_owned())
        );
        // The wire message is the fixed generic string — the raw panic
        // payload must NOT leak onto the wire.
        assert!(
            error.message.contains("daemon handler panicked"),
            "envelope message should be the generic panic notice, got: {}",
            error.message
        );
        assert!(
            !error.message.contains("bounds-check"),
            "raw panic payload must not leak onto the wire, got: {}",
            error.message
        );
    }

    #[test]
    fn sanitize_panic_message_strips_control_chars_and_truncates() {
        // Control characters (here a CRLF and a TAB) collapse to spaces
        // so a hostile panic payload cannot forge extra log lines.
        let dirty = "line one\r\nline two\tend";
        let clean = sanitize_panic_message(dirty);
        assert_eq!(clean, "line one  line two end");
        assert!(!clean.contains('\n'));
        assert!(!clean.contains('\r'));
        assert!(!clean.contains('\t'));

        // Oversized payloads truncate with an ellipsis marker so the
        // log line stays bounded.
        let huge = "a".repeat(DAEMON_PANIC_LOG_MAX_BYTES * 2);
        let bounded = sanitize_panic_message(&huge);
        assert!(bounded.len() <= DAEMON_PANIC_LOG_MAX_BYTES + 3);
        assert!(bounded.ends_with("..."));
    }

    #[test]
    fn extract_panic_payload_str_handles_str_string_and_other() {
        let str_payload: Box<dyn std::any::Any + Send> = Box::new("static str panic");
        assert_eq!(
            extract_panic_payload_str(str_payload.as_ref()),
            "static str panic"
        );

        let string_payload: Box<dyn std::any::Any + Send> = Box::new(String::from("owned panic"));
        assert_eq!(
            extract_panic_payload_str(string_payload.as_ref()),
            "owned panic"
        );

        let other_payload: Box<dyn std::any::Any + Send> = Box::new(42_u32);
        assert_eq!(
            extract_panic_payload_str(other_payload.as_ref()),
            "<non-string panic payload>"
        );
    }

    #[test]
    fn start_server_then_client_round_trip_echo() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-test.sock");
        let mut handle = start_server(&socket_path).expect("server must start");
        // Give the accept thread a moment to enter the listen state.
        thread::sleep(Duration::from_millis(50));

        let request = DaemonRequest::new(
            "req-roundtrip-001",
            METHOD_ECHO,
            serde_json::json!({"ping": "pong"}),
        );
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.request_id, "req-roundtrip-001");
        assert_eq!(response.result, Some(serde_json::json!({"ping": "pong"})));

        handle.shutdown().expect("shutdown");
        // After shutdown, the socket file must be gone so a subsequent
        // start_server against the same path does not need manual
        // cleanup.
        assert!(!socket_path.exists(), "socket file must be unlinked");
    }

    #[test]
    fn start_server_refuses_non_socket_existing_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("not-a-socket");
        fs::write(&path, b"i am a regular file").expect("write");
        let error = start_server(&path).expect_err("must refuse non-socket");
        assert!(matches!(error, DaemonStartError::SocketPathOccupied { .. }));
        // The regular file must still exist after the refused start;
        // the daemon must not silently overwrite arbitrary paths.
        assert!(path.exists());
    }

    /// Regression test for bd-3j0td. The UDS file must land on disk
    /// with mode 0o600 — owner-rw only — so that no other local UID
    /// can `connect(2)` even when the parent directory's 0o700 gate
    /// is bypassed (custom XDG_RUNTIME_DIR, operator-set TMPDIR, etc.).
    /// A future refactor that loses the chmod step would re-open the
    /// world-connectable hole this pin defends against; sentinel
    /// string `metadata.mode() & 0o777 == 0o600` per the bd-3j0td
    /// proposed-fix bullet 4.
    #[test]
    fn start_server_socket_file_is_owner_rw_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-mode.sock");
        let mut handle = start_server(&socket_path).expect("server must start");
        // The chmod runs synchronously inside start_server before it
        // returns, so the mode is observable immediately. Read via
        // `symlink_metadata` so a follow-up refactor that swapped the
        // bound path for a symlink would surface as a test failure.
        let metadata = fs::symlink_metadata(handle.socket_path()).expect("socket metadata");
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(
            mode,
            0o600,
            "UDS socket file at {} must be mode 0o600 (owner-rw only) per bd-3j0td; \
             observed 0o{mode:o}. World-connectable sockets are the cross-tenant \
             exfil attack surface the chmod step defends.",
            handle.socket_path().display()
        );
        handle.shutdown().expect("shutdown");
    }

    /// Regression test for bd-3j0td peer-credential gate. A second
    /// connection from the same UID round-trips (the gate compares
    /// effective UIDs; both peer and daemon are this test process).
    /// The same-UID path landing here means the negative path —
    /// peer_uid != own — can only fire on a real cross-UID connection,
    /// which the test harness cannot create without elevated
    /// privileges; the structural pin is that the gate IS in the
    /// dispatch path and does not refuse the legitimate same-UID
    /// caller.
    #[test]
    fn handle_connection_admits_same_uid_peer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-peer.sock");
        let mut handle = start_server(&socket_path).expect("server must start");
        thread::sleep(Duration::from_millis(50));

        let request = DaemonRequest::new(
            "req-peer-001",
            METHOD_ECHO,
            serde_json::json!({"peer": "self"}),
        );
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.request_id, "req-peer-001");
        assert!(response.error.is_none(), "same-UID peer must be admitted");
        assert_eq!(response.result, Some(serde_json::json!({"peer": "self"})));

        handle.shutdown().expect("shutdown");
    }
}
