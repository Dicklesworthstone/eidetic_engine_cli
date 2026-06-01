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
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::config::env_registry::{self, EnvVar};

use super::protocol::{
    DaemonRequest, DaemonResponse, FrameReadError, read_request, write_response,
};
use super::{
    DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE, DAEMON_DEFAULT_RPC_TIMEOUT, DAEMON_MAX_INFLIGHT,
    DAEMON_OVERLOADED_CODE, DAEMON_PEER_UNAUTHORIZED_CODE, DAEMON_SETSOCKOPT_FAILED_CODE,
    DaemonStartError, current_euid,
};

/// Method dispatch name for the round-trip integrity check.
pub const METHOD_ECHO: &str = "ee.daemon.echo";

/// Error code returned when the diagnostic echo method is not enabled.
pub const DAEMON_ECHO_DISABLED_CODE: &str = "daemon_echo_disabled";

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

/// Whether the diagnostic echo method should be exposed. The method
/// reflects caller-supplied content, so production daemons keep it off
/// unless a local diagnostic run explicitly opts in.
fn daemon_echo_enabled() -> bool {
    env_registry::read_or_default(EnvVar::DaemonEnableEcho)
        .is_some_and(|value| daemon_echo_env_value_truthy(&value))
}

fn daemon_echo_env_value_truthy(value: &str) -> bool {
    let value = value.trim();
    value == "1"
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("yes")
        || value.eq_ignore_ascii_case("on")
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
    /// Once-guard for [`DaemonServerHandle::shutdown`]. The first call
    /// performs the real teardown; any later call — typically the
    /// `Drop` impl running after an explicit `shutdown()` — observes
    /// this latch and returns `Ok(())` without re-touching the socket
    /// file. Distinct from the `shutdown` signal field above, which
    /// tells the accept loop to stop; this tracks whether the teardown
    /// *body* has already run. bd-wj6v9.
    shutdown_done: AtomicBool,
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
        // Once-guard (bd-wj6v9): the first call performs the real
        // teardown; a second call — typically `Drop` running after an
        // explicit `shutdown()` — observes the latch and returns Ok
        // without re-entering the teardown. Before this guard, the
        // second pass relied on `accept_thread.take()` already being
        // `None` and the socket already gone, and any residual
        // `remove_file` could surface a misleading ENOENT.
        if self.shutdown_done.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
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
        // Idempotent guarded unlink (bd-wj6v9, bd-2z3e8): tolerate
        // `NotFound`, but do not let shutdown delete an arbitrary
        // regular file if the socket path was swapped underneath us.
        remove_owned_socket_file(&self.socket_path)
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

fn remove_owned_socket_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "refusing to remove non-socket daemon path {}",
                        path.display()
                    ),
                ));
            }
            let euid = current_euid();
            if metadata.uid() != euid {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to remove daemon socket {} owned by uid {}, current uid {euid}",
                        path.display(),
                        metadata.uid()
                    ),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }

    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
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

    // Refuse to clobber a non-socket file already occupying the
    // canonical path, but do NOT pre-`remove_file` it. The former
    // stat → remove_file → bind sequence had two TOCTOU windows: an
    // attacker with write access to the parent directory could swap
    // the socket for a regular file between the `is_socket()` check
    // and the `remove_file` (falsifying the daemon's "this is my
    // stale socket" belief), or recreate the path between
    // `remove_file` and `bind`. We instead bind a per-attempt temp
    // path and `rename(2)` it onto the canonical name atomically
    // (below); `rename` replaces any existing socket cleanly with no
    // unlink step, collapsing both windows into one operation. The
    // parent directory is per-UID 0o700 (bd-3j0td), so no other UID
    // can race us inside it. Sentinel: bd-3ik2d atomic-rename.
    match fs::symlink_metadata(&socket_path) {
        Ok(metadata) => {
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
            // A stale socket from a prior daemon: the `rename` below
            // atomically replaces it, so there is nothing to unlink.
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(DaemonStartError::Bind {
                path: socket_path,
                source,
            });
        }
    }

    // Bind a per-attempt temporary path inside the same (per-UID
    // 0o700) parent directory, tighten its mode to 0o600 BEFORE it is
    // ever visible at the canonical name, then `rename(2)` it into
    // place. Because the chmod happens on the temp path, the canonical
    // path never exists in a world-connectable (0o755) state for even
    // an instant — a strict improvement over chmod-after-bind. The
    // temp name carries the pid plus a process-global counter so two
    // concurrent binds (same process or across processes) never
    // collide on the temp path and each `rename` is a clean atomic
    // publish. Sentinel: bd-3ik2d atomic-rename.
    let tmp_path = temp_bind_path(&socket_path);
    // Clear any temp left by a crashed prior attempt that happened to
    // reuse this pid+counter; best-effort, the bind below is the
    // authoritative step.
    let _ = fs::remove_file(&tmp_path);

    let listener = UnixListener::bind(&tmp_path).map_err(|source| DaemonStartError::Bind {
        path: socket_path.clone(),
        source,
    })?;

    // Tighten the temp socket to mode 0o600 before it is published.
    // `UnixListener::bind` honours the process umask (typically 0o022
    // → mode 0o755): world-connectable on every Unix host. Without
    // this chmod, any local UID could `connect(2)` and reach the
    // dispatch table — the attack surface documented in bd-3j0td. The
    // chmod failure is surfaced (not swallowed) so an operator on a
    // filesystem that rejects `chmod` sees the bind step error rather
    // than a silently world-open socket. Sentinel: bd-3j0td chmod-0600.
    if let Err(source) = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600)) {
        // The temp socket is bound but world-open at this instant;
        // remove it before returning so a half-secured artifact does
        // not linger under the temp name.
        let _ = fs::remove_file(&tmp_path);
        return Err(DaemonStartError::Bind {
            path: socket_path,
            source,
        });
    }

    // Atomically publish the secured socket at the canonical path.
    // `rename(2)` is atomic and replaces a stale socket left by a
    // prior daemon in a single step.
    if let Err(source) = fs::rename(&tmp_path, &socket_path) {
        // Publish failed (e.g. cross-device move, or the parent dir
        // was removed underneath us). Drop the temp socket so it does
        // not linger, then surface the failure.
        let _ = fs::remove_file(&tmp_path);
        return Err(DaemonStartError::Bind {
            path: socket_path,
            source,
        });
    }

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
        shutdown_done: AtomicBool::new(false),
    })
}

/// Construct a per-attempt temporary socket path next to `socket_path`,
/// of the form `<socket>.tmp.<pid>.<counter>`. The pid plus a
/// process-global monotonic counter guarantees the path is unique to
/// this bind attempt, so two concurrent [`start_server`] calls — in the
/// same process or across processes — never collide on the temp name
/// and the subsequent `rename(2)` is always a clean atomic publish.
/// Sentinel: bd-3ik2d atomic-rename.
fn temp_bind_path(socket_path: &Path) -> PathBuf {
    static TEMP_BIND_COUNTER: AtomicU64 = AtomicU64::new(0);
    let suffix = format!(
        ".tmp.{}.{}",
        std::process::id(),
        TEMP_BIND_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let mut file_name = socket_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    file_name.push(&suffix);
    let mut tmp_path = socket_path.to_path_buf();
    tmp_path.set_file_name(file_name);
    tmp_path
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
            // We've been signalled to stop, but `incoming` may still be
            // a freshly accepted connection: the shutdown wake itself
            // connects to unblock `accept`, and a legitimate client can
            // race in between the shutdown signal and the listener
            // teardown. Hand that peer a framed `daemon_shutting_down`
            // envelope before we stop, rather than dropping the stream
            // and leaving the client to interpret a bare connection
            // reset. The wake connection (which drops its end without
            // reading) just sees the best-effort write fail, harmlessly.
            // bd-36dp2.
            if let Ok(mut stream) = incoming {
                write_shutting_down_response(&mut stream);
            }
            break;
        }
        match incoming {
            Ok(stream) => {
                if let Some(permit) = pool.try_acquire() {
                    match stream.try_clone() {
                        Ok(worker_stream) => {
                            let spawn_result = thread::Builder::new()
                                .name("ee-daemon-conn".to_owned())
                                .spawn(move || {
                                    // Permit is held for the lifetime of the
                                    // worker; on drop the counter decrements
                                    // and the next accept can proceed.
                                    handle_connection(worker_stream);
                                    drop(permit);
                                });
                            if let Err(error) = spawn_result {
                                // Thread spawn itself failed (resource
                                // exhaustion). The failed closure drops the
                                // permit immediately; keep the original stream
                                // available so the client still receives the
                                // bounded-pool refusal envelope.
                                let _ = error;
                                let mut rejected = stream;
                                write_overloaded_response(&mut rejected);
                            }
                        }
                        Err(error) => {
                            // Duplicating the stream failed before the worker
                            // owned a descriptor. Release the permit and refuse
                            // the client with the same overloaded envelope; the
                            // daemon is unable to service this connection.
                            let _ = error;
                            drop(permit);
                            let mut rejected = stream;
                            write_overloaded_response(&mut rejected);
                        }
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
/// read the request frame), so it is set to `"<overloaded>"`. The
/// caller's `agent_id` is likewise unknown at this refusal point.
fn write_overloaded_response(stream: &mut UnixStream) {
    // Use a tight write timeout so a dead client cannot block the
    // accept loop. The accept loop is single-threaded; a slow client
    // here would amplify the very DoS the bounded pool defends
    // against.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let response = DaemonResponse::err(
        "<overloaded>",
        "<unknown>",
        None,
        DAEMON_OVERLOADED_CODE,
        "daemon worker pool saturated; retry after existing connections drain or fall back \
         to the in-process CLI path.",
    )
    .with_degraded(DAEMON_OVERLOADED_CODE);
    let _ = write_response(stream, &response);
}

/// Write a framed `daemon_shutting_down` response on a connection that
/// was accepted after the shutdown latch flipped, then let the caller
/// drop it. Mirrors [`write_overloaded_response`]: a tight write
/// timeout keeps a dead peer from stalling the (single-threaded) accept
/// loop on its way out, and we never learned the client's `request_id`
/// or `agent_id` (we never read a request frame), so they are set to
/// sentinels.
/// bd-36dp2.
fn write_shutting_down_response(stream: &mut UnixStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let response = DaemonResponse::err(
        "<shutdown>",
        "<unknown>",
        None,
        super::DAEMON_SHUTTING_DOWN_CODE,
        "daemon is shutting down and is no longer accepting connections; retry against a \
         fresh daemon or fall back to the in-process CLI path.",
    )
    .with_degraded(super::DAEMON_SHUTTING_DOWN_CODE);
    let _ = write_response(stream, &response);
}

/// Wall-clock milliseconds since `started`, saturating at `u64::MAX`
/// per the tracing-field convention
/// (docs/observability/tracing_field_convention.md).
fn elapsed_ms_since(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Classify a dispatch [`DaemonResponse`] into the observability tuple
/// bd-qwlzu records: the outcome code (`"ok"` or the wire error code)
/// plus the `schema_mismatch` / `unknown_method` protocol-shape booleans
/// the audit stream branches on. Pure so the per-method attribution
/// mapping is unit-testable without a live socket. The `result` /
/// `params` payload is deliberately NOT inspected — payload redaction is
/// bd-3uev6's surface; this reads only the stable error code.
#[must_use]
fn classify_dispatch_response(response: &DaemonResponse) -> (&str, bool, bool) {
    match response.error.as_ref() {
        None => ("ok", false, false),
        Some(error) => (
            error.code.as_str(),
            error.code == DAEMON_REQUEST_SCHEMA_MISMATCH_CODE,
            error.code == DAEMON_UNKNOWN_METHOD_CODE,
        ),
    }
}

/// Stable, leak-free category for a frame read failure. Returns a fixed
/// discriminant string rather than the error `Display` so a decode
/// failure can be recorded in the audit stream WITHOUT echoing any
/// attacker-controlled frame bytes into the log (orthogonal to the
/// wire-side reflection tracked by bd-3uev6).
#[must_use]
fn frame_error_kind(error: &FrameReadError) -> &'static str {
    match error {
        FrameReadError::Eof => "eof",
        FrameReadError::TooLarge { .. } => "too_large",
        FrameReadError::Truncated { .. } => "truncated",
        FrameReadError::Io(_) => "io",
        FrameReadError::Decode(_) => "decode",
    }
}

/// Emit one structured `ee.daemon.rpc` tracing event for a completed
/// dispatch (bd-qwlzu). Fields follow the canonical tracing-field
/// convention (workspace_id / request_id / bead_id / surface / phase /
/// elapsed_ms / degraded_codes) plus daemon-specific `method` + `peer_uid`
/// attribution and the `schema_mismatch` / `unknown_method` booleans.
/// This is the per-RPC audit trail the daemon dispatch path previously
/// lacked entirely.
fn trace_daemon_rpc_dispatch(
    request_id: &str,
    method: &str,
    peer: u32,
    elapsed_ms: u64,
    code: &str,
    degraded_codes: &[&str],
    schema_mismatch: bool,
    unknown_method: bool,
) {
    tracing::info!(
        workspace_id = "daemon-rpc",
        request_id,
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-qwlzu"),
        surface = "daemon_rpc",
        phase = "dispatch",
        elapsed_ms,
        method,
        peer_uid = peer,
        code,
        degraded_codes = ?degraded_codes,
        schema_mismatch,
        unknown_method,
        "ee.daemon.rpc dispatch outcome"
    );
}

/// Emit a `warn`-level `ee.daemon.rpc` event for a peer-credential
/// refusal (bd-qwlzu × bd-3j0td). The `SO_PEERCRED` / `getpeereid` gate
/// writes a refusal envelope on the wire but, before this, left no
/// server-side record — cross-UID probing was invisible to operators.
/// `peer` is `-1` when the credential lookup itself failed (the UID
/// could not be verified).
fn trace_daemon_rpc_unauthorized(peer: i64, elapsed_ms: u64, reason: &str) {
    let degraded = [DAEMON_PEER_UNAUTHORIZED_CODE];
    tracing::warn!(
        workspace_id = "daemon-rpc",
        request_id = "<unauthorized>",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-qwlzu"),
        surface = "daemon_rpc",
        phase = "dependency_check",
        elapsed_ms,
        method = "<unauthenticated>",
        peer_uid = peer,
        code = DAEMON_PEER_UNAUTHORIZED_CODE,
        degraded_codes = ?degraded,
        reason,
        "ee.daemon.rpc peer authorization refused"
    );
}

/// Emit a `warn`-level `ee.daemon.rpc` event for a frame that failed to
/// decode (bd-qwlzu). Carries only the stable `frame_error_kind`
/// category — never the raw bytes — so the audit stream cannot itself
/// become the leak channel bd-3uev6 warns about.
fn trace_daemon_rpc_decode_failure(peer: u32, elapsed_ms: u64, kind: &str) {
    let degraded = [DAEMON_REQUEST_DECODE_FAILED_CODE];
    tracing::warn!(
        workspace_id = "daemon-rpc",
        request_id = "<undecoded>",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-qwlzu"),
        surface = "daemon_rpc",
        phase = "input",
        elapsed_ms,
        method = "<undecoded>",
        peer_uid = peer,
        code = DAEMON_REQUEST_DECODE_FAILED_CODE,
        degraded_codes = ?degraded,
        frame_error_kind = kind,
        "ee.daemon.rpc request decode failed"
    );
}

fn handle_connection(mut stream: UnixStream) {
    // Install the per-connection deadlines BEFORE any read. The read
    // timeout is the only backstop that stops a half-open peer from
    // pinning this worker thread forever; if `setsockopt` fails (low
    // memory, a seccomp filter that blocks SO_RCVTIMEO/SO_SNDTIMEO,
    // certain BSD kernel modes) we MUST NOT fall through into a
    // deadline-less `read_request`. The pre-fix code discarded these
    // errors with `let _ =`, leaving exactly that permanent-hang path.
    // Set the write deadline first so the refusal envelope below cannot
    // itself block on a wedged client. Sentinel: bd-3pnno setsockopt.
    if let Err(error) = stream.set_write_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT)) {
        // No write deadline could be installed. We still attempt a
        // best-effort framed refusal — a freshly accepted UDS send
        // buffer almost always accepts a small envelope without
        // blocking — then drop. `write_response`'s own error is
        // swallowed; the connection closes either way and the worker
        // exits instead of hanging.
        let response = DaemonResponse::err(
            "<setsockopt>",
            "<unknown>",
            None,
            DAEMON_SETSOCKOPT_FAILED_CODE,
            format!("daemon could not set the connection write timeout: {error}"),
        )
        .with_degraded(DAEMON_SETSOCKOPT_FAILED_CODE);
        let _ = write_response(&mut stream, &response);
        return;
    }
    if let Err(error) = stream.set_read_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT)) {
        // The critical case: without a read deadline, `read_request`
        // could block forever on a peer that opened the connection and
        // stopped sending. Refuse with a framed envelope and drop so
        // the worker thread is reclaimed.
        let response = DaemonResponse::err(
            "<setsockopt>",
            "<unknown>",
            None,
            DAEMON_SETSOCKOPT_FAILED_CODE,
            format!("daemon could not set the connection read timeout: {error}"),
        )
        .with_degraded(DAEMON_SETSOCKOPT_FAILED_CODE);
        let _ = write_response(&mut stream, &response);
        return;
    }

    let started = std::time::Instant::now();

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
    let peer = match peer_uid(&stream) {
        Ok(peer) => {
            let own = current_euid();
            if peer != own {
                let response = DaemonResponse::err(
                    "<unauthorized>",
                    "<unknown>",
                    None,
                    DAEMON_PEER_UNAUTHORIZED_CODE,
                    format!(
                        "daemon refuses peer uid {peer}; only uid {own} (the daemon owner) \
                         may connect to this socket"
                    ),
                )
                .with_degraded(DAEMON_PEER_UNAUTHORIZED_CODE);
                trace_daemon_rpc_unauthorized(
                    i64::from(peer),
                    elapsed_ms_since(started),
                    "peer uid does not match daemon owner uid",
                );
                let _ = write_response(&mut stream, &response);
                return;
            }
            peer
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
                "<unknown>",
                None,
                DAEMON_PEER_UNAUTHORIZED_CODE,
                format!("daemon could not verify peer credential: {error}"),
            )
            .with_degraded(DAEMON_PEER_UNAUTHORIZED_CODE);
            trace_daemon_rpc_unauthorized(
                -1,
                elapsed_ms_since(started),
                "peer credential lookup failed",
            );
            let _ = write_response(&mut stream, &response);
            return;
        }
    };

    // Skeleton: one request per accepted connection. A follow-up
    // multiplexing slice will loop here so a single client can run
    // many RPCs over the same socket; the framing already supports
    // that because each frame is self-contained.
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(FrameReadError::Eof) => return,
        Err(other) => {
            let kind = frame_error_kind(&other);
            // bd-3uev6: the wire message is a FIXED string. `other.to_string()`
            // for `FrameReadError::Decode` embeds an attacker-controlled snippet
            // of the input near the parse failure — a log-injection vector once
            // these envelopes reach the flight recorder / obs pipeline. The full
            // diagnostic stays server-side in the structured `frame_error_kind`
            // tracing event below; the peer sent the bytes and does not need them
            // reflected.
            let response = DaemonResponse::err(
                "<unknown>",
                "<unknown>",
                None,
                DAEMON_REQUEST_DECODE_FAILED_CODE,
                "request body failed to decode",
            );
            trace_daemon_rpc_decode_failure(peer, elapsed_ms_since(started), kind);
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
    //
    // Metrics seam (bd-3vkyp): the live dispatch goes through
    // `instrument_dispatch` with the zero-cost `NoopMetricsCollector`,
    // so a perf-investigation build turns on per-method counters /
    // histograms by swapping the collector at THIS call site — no edit
    // to `dispatch` or any of its match arms, hence no recompile of the
    // hot dispatch table.
    let request_id = request.request_id.clone();
    let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::metrics::instrument_dispatch(
            &request.method,
            &super::metrics::NoopMetricsCollector,
            || dispatch(&request),
        )
    }));
    let response = match dispatched {
        Ok(response) => response,
        Err(payload) => build_panic_response(&request, payload.as_ref()),
    };
    let (code, schema_mismatch, unknown_method) = classify_dispatch_response(&response);
    let degraded: Vec<&str> = response.degraded_codes.iter().map(String::as_str).collect();
    trace_daemon_rpc_dispatch(
        &request_id,
        &request.method,
        peer,
        elapsed_ms_since(started),
        code,
        &degraded,
        schema_mismatch,
        unknown_method,
    );
    let _ = write_response(&mut stream, &response);
}

/// Construct the structured envelope returned to a client whose
/// connection-handler panicked, and log a sanitized one-line summary
/// of the panic to the daemon's stderr. The wire envelope carries a
/// fixed generic message — never the raw panic payload — so a panic
/// inside a `Display` impl that touched a memory body cannot leak
/// that content to the client. bd-b82q4.
fn build_panic_response(
    request: &DaemonRequest,
    payload: &(dyn std::any::Any + Send),
) -> DaemonResponse {
    let raw = extract_panic_payload_str(payload);
    let sanitized = sanitize_panic_message(&raw);
    // Single-line stderr log, capped, so a hostile panic payload
    // cannot blow out the journal nor inject log-forging characters.
    eprintln!("ee daemon handler panicked: {sanitized}");
    DaemonResponse::err(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
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

/// Read the peer's effective UID from a connected `UnixStream`. Linux exposes
/// this through Rustix's safe `SO_PEERCRED` wrapper. Other Unix targets fail
/// closed until a safe platform wrapper is wired in, so the daemon never
/// bypasses auth just because credential lookup is unavailable. Sentinel:
/// bd-3j0td peer_uid.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    rustix::net::sockopt::socket_peercred(stream)
        .map(|credentials| credentials.uid.as_raw())
        .map_err(io::Error::from)
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(_stream: &UnixStream) -> io::Result<u32> {
    Err(io::Error::other(
        "safe peer credential lookup is not implemented on this Unix target; \
         refusing daemon peer rather than bypassing authorization",
    ))
}

/// Pure dispatch: map a parsed [`DaemonRequest`] to a
/// [`DaemonResponse`]. Exposed for unit tests that exercise the
/// dispatch table without paying for a UDS round-trip.
#[must_use]
pub fn dispatch(request: &DaemonRequest) -> DaemonResponse {
    dispatch_with_echo_policy(request, daemon_echo_enabled())
}

fn dispatch_with_echo_policy(request: &DaemonRequest, echo_enabled: bool) -> DaemonResponse {
    if request.schema != super::DAEMON_REQUEST_SCHEMA_V1 {
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_REQUEST_SCHEMA_MISMATCH_CODE,
            format!(
                "expected schema `{}`, got `{}`",
                super::DAEMON_REQUEST_SCHEMA_V1,
                request.schema,
            ),
        );
    }

    match request.method.as_str() {
        // bd-3uev6: echo is the one public dispatch method that returns
        // caller-supplied content, so it MUST route through the same
        // canonical redaction pipeline every other content-bearing
        // surface uses (core::outcome, support_bundle, mcp). Reflecting
        // `params` verbatim would make the socket a redaction-bypassing
        // round-trip oracle. The integrity contract becomes "echo returns
        // the redaction-stable form of what you sent", and only when the
        // diagnostic method is explicitly enabled for the local daemon.
        METHOD_ECHO if echo_enabled => DaemonResponse::ok(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            crate::core::support_bundle::redact_json_value(&request.params),
        ),
        METHOD_ECHO => DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_ECHO_DISABLED_CODE,
            "ee.daemon.echo is disabled by default; set EE_DAEMON_ENABLE_ECHO=1 for local diagnostics.",
        ),
        METHOD_CONTEXT => DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE,
            "ee.daemon.context is a stub until the ANN warm-load slice ships; \
             the CLI client should fall back to the in-process `ee context` path.",
        )
        .with_degraded(DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE),
        other => DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
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

    const TEST_AGENT_ID: &str = "agent-daemon-server-test";

    #[test]
    fn daemon_echo_env_value_truthy_accepts_only_explicit_true_values() {
        for value in ["1", "true", "TRUE", "yes", "YES", "on", "ON", " true "] {
            assert!(
                daemon_echo_env_value_truthy(value),
                "{value:?} should enable daemon echo"
            );
        }
        for value in ["", "0", "false", "no", "off", "enabled", "please"] {
            assert!(
                !daemon_echo_env_value_truthy(value),
                "{value:?} should not enable daemon echo"
            );
        }
    }

    #[test]
    fn dispatch_echo_disabled_returns_error_by_default() {
        let request = DaemonRequest::new(
            "req-echo-disabled-001",
            TEST_AGENT_ID,
            METHOD_ECHO,
            serde_json::json!({"k": "v", "n": 7}),
        );
        let response = dispatch_with_echo_policy(&request, false);
        assert_eq!(response.request_id, "req-echo-disabled-001");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        assert!(response.result.is_none());
        let error = response.error.as_ref().expect("echo must be disabled");
        assert_eq!(error.code, DAEMON_ECHO_DISABLED_CODE);
    }

    #[test]
    fn dispatch_echo_enabled_returns_benign_params_unchanged() {
        let params = serde_json::json!({"k": "v", "n": 7});
        let request =
            DaemonRequest::new("req-echo-001", TEST_AGENT_ID, METHOD_ECHO, params.clone());
        let response = dispatch_with_echo_policy(&request, true);
        assert_eq!(response.request_id, "req-echo-001");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        assert_eq!(response.result, Some(params));
        assert!(response.error.is_none());
        assert!(response.degraded_codes.is_empty());
    }

    // bd-3uev6: echo must route params through the canonical redaction
    // pipeline (core::support_bundle::redact_json_value) so the dispatch
    // table is not a reflection oracle that bypasses every other
    // content-bearing surface's redaction. Benign params round-trip
    // unchanged (pinned above); secret-shaped values must NOT.
    #[test]
    fn dispatch_echo_redacts_secret_shaped_params() {
        let secret = "sk_live_abcdefghijklmnopqrstuvwxyz0123456789";
        let request = DaemonRequest::new(
            "req-echo-redact-001",
            TEST_AGENT_ID,
            METHOD_ECHO,
            serde_json::json!({"token": secret, "note": "hello"}),
        );
        let response = dispatch_with_echo_policy(&request, true);
        assert!(response.error.is_none());
        let result = response.result.expect("echo returns a result");
        let serialized = result.to_string();
        assert!(
            !serialized.contains("sk_live_abcdefghijklmnopqrstuvwxyz"),
            "echo must redact secret-shaped params via redact_json_value; leaked: {serialized}"
        );
        assert_eq!(
            result.get("note").and_then(serde_json::Value::as_str),
            Some("hello"),
            "non-secret fields must round-trip unchanged"
        );
    }

    #[test]
    fn dispatch_context_returns_warmload_not_yet_implemented() {
        let request = DaemonRequest::new(
            "req-ctx-001",
            TEST_AGENT_ID,
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
        let request = DaemonRequest::new(
            "req-unk-001",
            TEST_AGENT_ID,
            "ee.daemon.nope",
            serde_json::Value::Null,
        );
        let response = dispatch(&request);
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_UNKNOWN_METHOD_CODE);
    }

    // bd-qwlzu: the dispatch-observability classifiers feed the
    // `ee.daemon.rpc` audit event. Pin the code+boolean mapping so the
    // per-method attribution cannot silently drift.
    #[test]
    fn classify_dispatch_response_ok_has_no_error_flags() {
        let response = DaemonResponse::ok("r", TEST_AGENT_ID, None, serde_json::json!({"x": 1}));
        assert_eq!(classify_dispatch_response(&response), ("ok", false, false));
    }

    #[test]
    fn classify_dispatch_response_schema_mismatch_sets_only_schema_flag() {
        let bogus = DaemonRequest {
            schema: "ee.daemon.request.v0_wrong".to_owned(),
            request_id: "r".to_owned(),
            agent_id: TEST_AGENT_ID.to_owned(),
            workspace_id: None,
            method: METHOD_ECHO.to_owned(),
            params: serde_json::Value::Null,
        };
        let response = dispatch(&bogus);
        let (code, schema_mismatch, unknown_method) = classify_dispatch_response(&response);
        assert_eq!(code, DAEMON_REQUEST_SCHEMA_MISMATCH_CODE);
        assert!(schema_mismatch);
        assert!(!unknown_method);
    }

    #[test]
    fn classify_dispatch_response_unknown_method_sets_only_unknown_flag() {
        let request = DaemonRequest::new(
            "r",
            TEST_AGENT_ID,
            "ee.daemon.nope",
            serde_json::Value::Null,
        );
        let response = dispatch(&request);
        let (code, schema_mismatch, unknown_method) = classify_dispatch_response(&response);
        assert_eq!(code, DAEMON_UNKNOWN_METHOD_CODE);
        assert!(!schema_mismatch);
        assert!(unknown_method);
    }

    #[test]
    fn frame_error_kind_categories_are_stable_and_leak_free() {
        assert_eq!(frame_error_kind(&FrameReadError::Eof), "eof");
        assert_eq!(
            frame_error_kind(&FrameReadError::TooLarge {
                announced: 9,
                max: 4
            }),
            "too_large"
        );
        assert_eq!(
            frame_error_kind(&FrameReadError::Truncated {
                expected: 9,
                got: 2
            }),
            "truncated"
        );
        assert_eq!(
            frame_error_kind(&FrameReadError::Io(std::io::Error::other("boom"))),
            "io"
        );
        let decode_err = serde_json::from_str::<DaemonRequest>("{").unwrap_err();
        assert_eq!(
            frame_error_kind(&FrameReadError::Decode(decode_err)),
            "decode"
        );
    }

    #[test]
    fn dispatch_schema_mismatch_returns_schema_mismatch_code() {
        let bogus = DaemonRequest {
            schema: "ee.daemon.request.v0_wrong".to_owned(),
            request_id: "req-schema-001".to_owned(),
            agent_id: TEST_AGENT_ID.to_owned(),
            workspace_id: None,
            method: METHOD_ECHO.to_owned(),
            params: serde_json::Value::Null,
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
        let request = DaemonRequest::new(
            request_id,
            TEST_AGENT_ID,
            METHOD_CONTEXT,
            serde_json::json!({"task": "panic"}),
        );
        let dispatched =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> DaemonResponse {
                panic!("simulated warm-load bounds-check failure");
            }));
        let response = match dispatched {
            Ok(response) => response,
            Err(payload) => build_panic_response(&request, payload.as_ref()),
        };
        // The client gets a real, parseable envelope — not a reset.
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.agent_id, TEST_AGENT_ID);
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
    fn start_server_then_echo_is_disabled_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-test.sock");
        let mut handle = start_server(&socket_path).expect("server must start");
        // Give the accept thread a moment to enter the listen state.
        thread::sleep(Duration::from_millis(50));

        let request = DaemonRequest::new(
            "req-roundtrip-001",
            TEST_AGENT_ID,
            METHOD_ECHO,
            serde_json::json!({"ping": "pong"}),
        );
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.request_id, "req-roundtrip-001");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        assert!(response.result.is_none());
        let error = response.error.as_ref().expect("echo must be disabled");
        assert_eq!(error.code, DAEMON_ECHO_DISABLED_CODE);

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
            TEST_AGENT_ID,
            METHOD_CONTEXT,
            serde_json::json!({"peer": "self"}),
        );
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.request_id, "req-peer-001");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        let error = response
            .error
            .as_ref()
            .expect("same-UID peer reaches dispatch");
        assert_eq!(error.code, DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE);

        handle.shutdown().expect("shutdown");
    }

    /// bd-3ik2d: a stale socket left by a crashed prior daemon must be
    /// replaced atomically. The fix binds a temp path and `rename(2)`s
    /// it over the stale socket — there is no `remove_file` step that a
    /// racing attacker could exploit (the former TOCTOU window). Here we
    /// leave a dead socket file behind (bind, then drop the listener
    /// without unlinking) and assert the next `start_server` publishes a
    /// fresh, live, 0o600 socket over it.
    #[test]
    fn start_server_replaces_stale_socket_atomically() {
        use std::os::unix::fs::FileTypeExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-stale.sock");

        // Simulate a crashed daemon: bind then drop the listener WITHOUT
        // unlinking, leaving a dead socket file on disk.
        {
            let stale = UnixListener::bind(&socket_path).expect("stale bind");
            drop(stale);
        }
        assert!(
            fs::symlink_metadata(&socket_path)
                .expect("stale socket must remain on disk")
                .file_type()
                .is_socket(),
            "precondition: a stale socket file occupies the path",
        );

        let mut handle = start_server(&socket_path).expect("server must replace stale socket");
        thread::sleep(Duration::from_millis(50));

        let metadata = fs::symlink_metadata(handle.socket_path()).expect("socket metadata");
        assert!(
            metadata.file_type().is_socket(),
            "published path must be a socket"
        );
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "replaced socket must be 0o600 (bd-3j0td invariant preserved across rename)",
        );
        let request = DaemonRequest::new(
            "req-stale-001",
            TEST_AGENT_ID,
            METHOD_CONTEXT,
            serde_json::json!({"ping": 1}),
        );
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        let error = response.error.as_ref().expect("fresh socket dispatches");
        assert_eq!(error.code, DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE);

        handle.shutdown().expect("shutdown");
    }

    /// bd-3ik2d: two concurrent `start_server` calls on the same path
    /// must never leave the canonical path in a corrupt state. The old
    /// stat → remove_file → bind sequence could unlink a peer's fresh
    /// socket or leave the path momentarily absent / a regular file (the
    /// TOCTOU window). The atomic bind-temp → chmod → rename path
    /// guarantees the canonical name always resolves to a valid 0o600
    /// socket regardless of who wins the race, and neither bind errors.
    #[test]
    fn start_server_concurrent_binds_no_toctou() {
        use std::os::unix::fs::FileTypeExt;
        use std::sync::Barrier;

        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-race.sock");

        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let path = socket_path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    start_server(&path)
                })
            })
            .collect();

        let results: Vec<_> = threads
            .into_iter()
            .map(|t| t.join().expect("bind thread must not panic"))
            .collect();

        // The atomic rename replaces rather than racing on remove_file,
        // so both concurrent binds succeed. If someone reintroduces the
        // stat → remove → bind window, one bind will observe EADDRINUSE
        // or unlink the other's socket and this count drops below two.
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok_count, 2, "both concurrent atomic binds must succeed");

        // The canonical path always resolves to a live 0o600 socket —
        // never absent, never a regular file, never world-open.
        let metadata = fs::symlink_metadata(&socket_path).expect("canonical path must exist");
        assert!(
            metadata.file_type().is_socket(),
            "canonical path must be a socket after a concurrent-bind race",
        );
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "canonical socket must be 0o600 after a concurrent-bind race",
        );

        // The losing bind's listener is orphaned: the winner's `rename`
        // replaced the canonical path, so the loser's accept loop can no
        // longer be woken (shutdown wakes by connecting to the canonical
        // name, which now points at the winner). Joining it would
        // deadlock, so we intentionally leak both handles and let the
        // tempdir reap the socket file. The leak is bounded to this test
        // process.
        for result in results {
            if let Ok(handle) = result {
                std::mem::forget(handle);
            }
        }
    }

    /// bd-2z3e8: shutdown cleanup must never turn a swapped daemon
    /// socket path into an arbitrary-file unlink. The helper is tested
    /// directly so the test can simulate hostile path state without
    /// deadlocking the accept thread's wakeup connection.
    #[test]
    fn guarded_socket_unlink_refuses_regular_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("not-a-daemon-socket");
        fs::write(&path, b"operator data").expect("write regular file");

        let error = remove_owned_socket_file(&path).expect_err("regular file must be refused");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            path.exists(),
            "regular file must remain on disk after refused daemon cleanup"
        );
    }

    #[test]
    fn guarded_socket_unlink_removes_owned_socket_and_tolerates_absent_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("owned.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");
        drop(listener);

        remove_owned_socket_file(&socket_path).expect("owned socket cleanup must succeed");
        assert!(
            !socket_path.exists(),
            "owned socket file must be removed by guarded cleanup"
        );

        remove_owned_socket_file(&socket_path).expect("absent path is already clean");
    }

    /// Regression test for bd-wj6v9. A second `shutdown()` call (the
    /// explicit-then-`Drop` pattern, or any repeated invocation) must
    /// be a clean `Ok(())` no-op rather than surfacing a misleading
    /// `ENOENT` from re-unlinking an already-removed socket. The
    /// once-guard short-circuits the second pass; the idempotent
    /// `remove_file` (NotFound-tolerant) backstops the case where the
    /// socket vanished out from under the first pass.
    #[test]
    fn shutdown_is_idempotent_across_repeated_calls() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-idempotent.sock");
        let mut handle = start_server(&socket_path).expect("server must start");
        thread::sleep(Duration::from_millis(50));

        handle.shutdown().expect("first shutdown must succeed");
        assert!(
            !socket_path.exists(),
            "socket file must be unlinked after the first shutdown"
        );
        // Second explicit call: pre-bd-wj6v9 this re-entered the
        // teardown; it must now be a guarded no-op returning Ok.
        handle
            .shutdown()
            .expect("second shutdown must be an idempotent no-op, not ENOENT");
        // A third call (mirroring the implicit `Drop`-after-explicit
        // path) must also be Ok; `Drop` itself runs at end of scope.
        handle
            .shutdown()
            .expect("third shutdown must also be a no-op");
    }

    /// Regression test for bd-36dp2. A connection accepted while the
    /// daemon is shutting down must receive a framed
    /// `daemon_shutting_down` envelope rather than being dropped
    /// silently (which the client would observe as a bare connection
    /// reset). We exercise the envelope writer directly over a
    /// `UnixStream` pair — deterministic, no race against the accept
    /// loop — and assert the wire frame parses to the expected code.
    #[test]
    fn write_shutting_down_response_emits_framed_daemon_shutting_down_envelope() {
        use std::io::Read;
        let (mut server_side, mut client_side) = UnixStream::pair().expect("socketpair");
        write_shutting_down_response(&mut server_side);
        drop(server_side);

        let mut prefix = [0_u8; 4];
        client_side
            .read_exact(&mut prefix)
            .expect("length prefix must arrive");
        let announced = u32::from_be_bytes(prefix) as usize;
        let mut body = vec![0_u8; announced];
        client_side
            .read_exact(&mut body)
            .expect("framed body must arrive");
        let response: DaemonResponse =
            serde_json::from_slice(&body).expect("frame must parse as DaemonResponse");
        let error = response.error.as_ref().expect("must carry an error");
        assert_eq!(error.code, crate::daemon::DAEMON_SHUTTING_DOWN_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&crate::daemon::DAEMON_SHUTTING_DOWN_CODE.to_owned()),
            "shutdown envelope must surface the code in degraded[]"
        );
    }
}
