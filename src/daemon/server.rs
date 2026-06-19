//! Unix-domain socket accept loop + per-connection dispatcher
//! (bd-oja31 skeleton). Wraps the framing in
//! [`super::protocol`] with the seed dispatch table for
//! `ee.daemon.capabilities`, `ee.daemon.echo`,
//! `ee.daemon.shutdown`, and the workspace-bound
//! `ee.daemon.context` pack path.
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

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};

use crate::config::env_registry::{self, EnvVar};
use crate::core::context::{
    ContextPackError, ContextPackOptions, ContextPackOutputOptionOverrides,
    ContextPackOutputOptions, attach_pack_dna_to_context_response,
    run_context_pack_with_performance_controlled,
};
use crate::core::search::SearchSourceMode;
use crate::models::{MemoryScope, QueryFilters, RedactionLevel};
use crate::output::{ContextJsonRenderOptions, render_context_response_json_with_options};
use crate::pack::{ContextPackProfile, DEFAULT_COORDINATION_STALE_AFTER_MS, PackResourceProfile};
use crate::search::SpeedMode;

use super::protocol::{
    DaemonRequest, DaemonResponse, FrameReadError, read_request, write_response,
};
use super::{
    DAEMON_DEFAULT_RPC_TIMEOUT, DAEMON_MAX_INFLIGHT, DAEMON_METHOD_UNAUTHORIZED_CODE,
    DAEMON_OVERLOADED_CODE, DAEMON_PEER_UNAUTHORIZED_CODE, DAEMON_SETSOCKOPT_FAILED_CODE,
    DaemonStartError, current_euid,
};

/// Method dispatch name for the round-trip integrity check.
pub const METHOD_ECHO: &str = "ee.daemon.echo";

/// Method dispatch name for daemon protocol discovery.
pub const METHOD_CAPABILITIES: &str = "ee.daemon.capabilities";

/// Method dispatch name for graceful daemon shutdown.
pub const METHOD_SHUTDOWN: &str = "ee.daemon.shutdown";

/// Error code returned when the diagnostic echo method is not enabled.
pub const DAEMON_ECHO_DISABLED_CODE: &str = "daemon_echo_disabled";

/// Method dispatch name for the warm-loaded `ee pack` path.
pub const METHOD_CONTEXT: &str = "ee.daemon.context";

/// Error code returned when `ee.daemon.context` params cannot be
/// mapped to the canonical pack request shape.
pub const DAEMON_CONTEXT_PARAMS_INVALID_CODE: &str = "daemon_context_params_invalid";

/// Error code returned when `ee.daemon.context` refuses a request
/// before pack execution because its explicit deadline/budget has
/// already expired.
pub const DAEMON_CONTEXT_DEADLINE_EXCEEDED_CODE: &str = "daemon_context_deadline_exceeded";

/// Error code returned when the canonical pack path fails before it can
/// produce an `ee.response.v2` envelope.
pub const DAEMON_CONTEXT_EXECUTION_FAILED_CODE: &str = "daemon_context_execution_failed";

/// Method dispatch name for the live in-process contention telemetry
/// snapshot. Because the handler runs INSIDE the long-lived daemon process,
/// the group-commit / singleflight counters it returns reflect real
/// accumulated load — coalescing that a one-shot `ee` CLI invocation
/// (single-writer fallback, process-local atomics reset per process) can
/// never observe. Surfaced to operators via `ee diag contention --use-daemon`.
/// bd-d67os.12.
pub const METHOD_TELEMETRY: &str = "ee.daemon.telemetry";

/// Error code returned when the daemon fails to serialize its live
/// contention telemetry snapshot into the `ee.diag.contention.v1` result
/// payload. Non-fatal to the daemon; the client falls back to the
/// in-process snapshot path. bd-d67os.12.
pub const DAEMON_TELEMETRY_ENCODE_FAILED_CODE: &str = "daemon_telemetry_encode_failed";

/// Method dispatch name for routing a durable memory write through the
/// daemon. Inc 1 (bd-wx6ou.2) executes the write via the existing direct
/// `remember_memory` path (no actor yet) so daemon-routed writes are
/// byte-identical to `ee remember`; later increments coalesce them through a
/// long-lived group-commit actor. Workspace-scoped (`SameUidWorkspace`) like
/// `ee.daemon.context`, because a write mutates one specific workspace.
pub const METHOD_WRITE: &str = "ee.daemon.write";

/// Error code returned when `ee.daemon.write` params cannot be mapped to a
/// remember request shape (missing content, bad workspace path, etc.).
pub const DAEMON_WRITE_PARAMS_INVALID_CODE: &str = "daemon_write_params_invalid";

/// Method dispatch name for routing a durable journal write through the daemon
/// (Inc 4, bd-wx6ou.5). Like `ee.daemon.write` but for journal entries: the
/// long-lived actor coalesces a batch of journal writes into one
/// transaction/fsync (Inc 3). Workspace-scoped (`SameUidWorkspace`).
pub const METHOD_WRITE_JOURNAL: &str = "ee.daemon.write_journal";

/// Error code returned when `ee.daemon.write_journal` params cannot be decoded
/// into a journal write request shape.
pub const DAEMON_JOURNAL_PARAMS_INVALID_CODE: &str = "daemon_journal_params_invalid";

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
const DAEMON_WORKER_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_SCHEDULER_JOIN_TIMEOUT: Duration = Duration::from_millis(750);

/// Per-daemon dispatch policy that is resolved at daemon start and
/// then shared by every accepted connection. Connection-level peer
/// credentials still gate local UID; this policy gates method-specific
/// workspace authority inside dispatch. bd-3mbao.
// Eq/PartialEq were derived but unused; dropped because the optional
// `write_router` (an asupersync runtime + write handle) is not comparable.
#[derive(Clone, Debug, Default)]
pub struct DaemonDispatchPolicy {
    bound_workspace_id: Option<String>,
    /// Set once at daemon start when the workspace is bound and the long-lived
    /// write-owner actor is hosted (Inc 2, bd-wx6ou.3). Carries the shared
    /// runtime + a clone of the actor's submit handle to `dispatch_write`
    /// without threading a new parameter through the accept/connection path,
    /// which already carries `Arc<DaemonDispatchPolicy>`. `None` ⇒ the
    /// in-process direct write path (Inc 1) is used.
    write_router: Option<DaemonWriteRouter>,
}

impl DaemonDispatchPolicy {
    /// Bind workspace-scoped daemon methods to one workspace id.
    #[must_use]
    pub fn for_workspace(workspace_id: impl Into<String>) -> Self {
        Self {
            bound_workspace_id: Some(workspace_id.into()),
            write_router: None,
        }
    }

    fn bound_workspace_id(&self) -> Option<&str> {
        self.bound_workspace_id.as_deref()
    }

    fn write_router(&self) -> Option<&DaemonWriteRouter> {
        self.write_router.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonAuthority {
    SameUid,
    SameUidWorkspace,
}

impl DaemonAuthority {
    const fn as_wire_label(self) -> &'static str {
        match self {
            Self::SameUid => "same_uid",
            Self::SameUidWorkspace => "same_uid_workspace",
        }
    }
}

#[derive(Debug)]
struct DaemonSocketPublishLock {
    _file: File,
}

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

#[allow(clippy::expect_used)]
impl Drop for InflightPermit {
    fn drop(&mut self) {
        let mut current = self
            .pool
            .inflight
            .lock()
            .expect("daemon inflight mutex must not be poisoned");
        *current = current.saturating_sub(1);
        if *current == 0 {
            self.pool.idle.notify_all();
        }
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
    idle: Condvar,
}

#[allow(clippy::expect_used)]
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
            idle: Condvar::new(),
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

    fn wait_until_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut current = self
            .inflight
            .lock()
            .expect("daemon inflight mutex must not be poisoned");
        while *current > 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next, result) = self
                .idle
                .wait_timeout(current, remaining)
                .expect("daemon inflight condvar must not be poisoned");
            current = next;
            if result.timed_out() && *current > 0 {
                return false;
            }
        }
        true
    }
}

#[derive(Debug)]
enum SchedulerThreadExit {
    Returned,
    Panicked,
}

#[derive(Debug)]
struct SchedulerThreadHandle {
    join: JoinHandle<()>,
    done_rx: mpsc::Receiver<SchedulerThreadExit>,
}

enum SchedulerJoinOutcome {
    Joined(io::Result<()>),
    StillRunning(SchedulerThreadHandle),
}

impl SchedulerThreadHandle {
    fn join_with_timeout(self, timeout: Duration) -> SchedulerJoinOutcome {
        match self.done_rx.recv_timeout(timeout) {
            Ok(SchedulerThreadExit::Returned) => SchedulerJoinOutcome::Joined(
                self.join
                    .join()
                    .map_err(|_| io::Error::other("daemon scheduler thread panicked")),
            ),
            Ok(SchedulerThreadExit::Panicked) => {
                let _ = self.join.join();
                SchedulerJoinOutcome::Joined(Err(io::Error::other(
                    "daemon scheduler thread panicked",
                )))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => SchedulerJoinOutcome::StillRunning(self),
            Err(mpsc::RecvTimeoutError::Disconnected) => SchedulerJoinOutcome::Joined(
                self.join
                    .join()
                    .map_err(|_| io::Error::other("daemon scheduler thread panicked")),
            ),
        }
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
pub struct DaemonServerHandle {
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    pool: Arc<InflightPool>,
    accept_thread: Option<JoinHandle<()>>,
    /// Background steward scheduler thread (bd-2ohzq). `None` when the
    /// daemon was started without a bound workspace (e.g. via the bare
    /// `start_server` entry point). The thread runs until `shutdown` fires.
    scheduler_thread: Option<SchedulerThreadHandle>,
    /// Once-guard for accept-loop and socket teardown. The first
    /// shutdown call stops the listener and unlinks the socket; later
    /// calls skip that irreversible section but may still wait for a
    /// previously timed-out worker drain.
    shutdown_done: AtomicBool,
    /// Set only after all per-connection worker permits have dropped.
    /// Kept separate from `shutdown_done` so a timed-out explicit
    /// shutdown does not make later calls falsely report success.
    workers_drained: AtomicBool,
    /// The retained clone of the write-owner actor's submit handle (Inc 2,
    /// bd-wx6ou.3). Connection threads use clones carried in the dispatch
    /// policy's `DaemonWriteRouter`; this one is dropped LAST in shutdown so the
    /// actor's mpsc closes (→ `recv` returns `Disconnected` → the actor loop
    /// breaks) only after the accept loop and connection workers have drained.
    /// `None` when the daemon was started without a bound workspace.
    write_handle: Option<crate::core::write_owner::WriteHandle>,
    /// Join handle for the long-lived write-owner actor task. Joined during
    /// shutdown after the write handle drops, so the final batch commits before
    /// the runtime is torn down.
    write_owner_task: Option<asupersync::runtime::JoinHandle<()>>,
    /// The asupersync runtime that drives the actor task and connection-thread
    /// `block_on` submits. Dropped last (joins its worker threads). Shared as
    /// `Arc` with the dispatch policy's `DaemonWriteRouter`.
    write_runtime: Option<Arc<asupersync::runtime::Runtime>>,
}

impl std::fmt::Debug for DaemonServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The write runtime/actor handles have no meaningful Debug; summarize
        // whether the actor is hosted rather than recursing into them.
        f.debug_struct("DaemonServerHandle")
            .field("socket_path", &self.socket_path)
            .field("accept_thread", &self.accept_thread.is_some())
            .field("scheduler_thread", &self.scheduler_thread.is_some())
            .field("write_actor_hosted", &self.write_handle.is_some())
            .finish_non_exhaustive()
    }
}

impl DaemonServerHandle {
    /// Return the bound socket path for status surfaces / tests.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Whether any daemon control path has requested shutdown. The
    /// foreground CLI process uses this to turn an RPC shutdown request
    /// into an explicit [`DaemonServerHandle::shutdown`] call so the
    /// accept loop, background scheduler, and socket teardown all join
    /// on the owning thread.
    #[must_use]
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Signal the accept loop to stop and wait for it to drain. Also
    /// removes the socket file from the filesystem so a subsequent
    /// `start_server` call against the same path does not need a
    /// manual cleanup step.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.shutdown_with_worker_drain_timeout(DAEMON_WORKER_DRAIN_TIMEOUT)
    }

    fn shutdown_with_worker_drain_timeout(
        &mut self,
        worker_drain_timeout: Duration,
    ) -> io::Result<()> {
        if self.workers_drained.load(Ordering::Acquire) && self.scheduler_thread.is_none() {
            return Ok(());
        }

        self.shutdown.store(true, Ordering::SeqCst);
        let first_teardown = !self.shutdown_done.swap(true, Ordering::AcqRel);
        let unlink_result = if first_teardown {
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
            SocketBroker::new(self.socket_path.clone()).remove_owned_socket_file()
        } else {
            Ok(())
        };
        let scheduler_result = self.join_scheduler_with_timeout();

        let workers_drained = self.pool.wait_until_idle(worker_drain_timeout);
        if workers_drained {
            self.workers_drained.store(true, Ordering::Release);
        }

        // Tear down the write-owner actor (Inc 2, bd-wx6ou.3) AFTER the accept
        // loop joined and connection workers drained: by now the dispatch
        // policy's router (a WriteHandle clone) is dropped, so dropping the
        // retained handle closes the actor's mpsc, its `recv` returns
        // Disconnected, and `run_group_commit` returns. Joining the actor task
        // ensures the final write commits before the runtime is dropped (which
        // joins its worker threads). The `is_finished` guard plus `catch_unwind`
        // keep a force-cancelled/panicked actor from hanging or unwinding
        // shutdown. (TODO Inc 7: bound `block_on` with a timeout so a write
        // wedged inside `remember_memory` cannot stall shutdown.)
        self.write_handle.take();
        if let (Some(runtime), Some(task)) =
            (self.write_runtime.as_ref(), self.write_owner_task.take())
            && !task.is_finished()
        {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = runtime.block_on(task);
            }));
        }
        self.write_runtime.take();

        unlink_result?;
        scheduler_result?;
        if !workers_drained {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "daemon worker threads did not drain within {}ms",
                    worker_drain_timeout.as_millis()
                ),
            ));
        }
        Ok(())
    }

    fn join_scheduler_with_timeout(&mut self) -> io::Result<()> {
        let Some(handle) = self.scheduler_thread.take() else {
            return Ok(());
        };
        match handle.join_with_timeout(DAEMON_SCHEDULER_JOIN_TIMEOUT) {
            SchedulerJoinOutcome::Joined(result) => result,
            SchedulerJoinOutcome::StillRunning(handle) => {
                self.scheduler_thread = Some(handle);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "daemon scheduler thread did not stop within {}ms after shutdown",
                        DAEMON_SCHEDULER_JOIN_TIMEOUT.as_millis()
                    ),
                ))
            }
        }
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

#[derive(Debug)]
/// Owns daemon UDS publication and guarded cleanup invariants.
struct SocketBroker {
    socket_path: PathBuf,
}

impl SocketBroker {
    fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn publish_listener(
        &self,
    ) -> Result<(UnixListener, DaemonSocketPublishLock), DaemonStartError> {
        self.ensure_private_parent()?;
        let publish_lock = self.acquire_publish_lock()?;
        self.refuse_non_socket_or_live_existing()?;
        let listener = self.bind_secured_temp_listener()?;
        Ok((listener, publish_lock))
    }

    fn ensure_private_parent(&self) -> Result<(), DaemonStartError> {
        if let Some(parent) = self.socket_path.parent()
            && !parent.as_os_str().is_empty()
        {
            // The parent directory must be a same-user private boundary:
            // it contains the publish lock, temp socket, and canonical
            // socket path. `DirBuilder` applies 0o700 only to components it
            // creates, so validate the resulting parent before opening the
            // start lock or binding a socket inside it.
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|source| DaemonStartError::SocketDirCreate {
                    path: parent.to_path_buf(),
                    source,
                })?;
            Self::validate_socket_parent(parent)?;
        }
        Ok(())
    }

    fn acquire_publish_lock(&self) -> Result<DaemonSocketPublishLock, DaemonStartError> {
        let lock_path = self.socket_publish_lock_path();
        let file = Self::open_daemon_socket_lock_file(&lock_path).map_err(|source| {
            DaemonStartError::Bind {
                path: self.socket_path.clone(),
                source,
            }
        })?;
        flock(&file, FlockOperation::LockExclusive).map_err(|source| DaemonStartError::Bind {
            path: self.socket_path.clone(),
            source: io::Error::from(source),
        })?;
        Ok(DaemonSocketPublishLock { _file: file })
    }

    fn refuse_non_socket_or_live_existing(&self) -> Result<(), DaemonStartError> {
        // Refuse to clobber a non-socket file already occupying the
        // canonical path, but do NOT pre-`remove_file` it. The former
        // stat -> remove_file -> bind sequence had two TOCTOU windows: an
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
        match fs::symlink_metadata(&self.socket_path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() {
                    return Err(DaemonStartError::SocketPathOccupied {
                        path: self.socket_path.clone(),
                    });
                }
                if self.existing_socket_accepts_connection() {
                    return Err(DaemonStartError::AlreadyRunning {
                        path: self.socket_path.clone(),
                    });
                }
                // A dead socket from a prior daemon: the `rename` below
                // atomically replaces it, so there is nothing to unlink.
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(DaemonStartError::Bind {
                    path: self.socket_path.clone(),
                    source,
                });
            }
        }
        Ok(())
    }

    fn bind_secured_temp_listener(&self) -> Result<UnixListener, DaemonStartError> {
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
        let tmp_path = self.temp_bind_path();
        // Clear any temp left by a crashed prior attempt that happened to
        // reuse this pid+counter; best-effort, the bind below is the
        // authoritative step.
        let _ = fs::remove_file(&tmp_path);

        let listener = UnixListener::bind(&tmp_path).map_err(|source| DaemonStartError::Bind {
            path: self.socket_path.clone(),
            source,
        })?;

        // Tighten the temp socket to mode 0o600 before it is published.
        // `UnixListener::bind` honours the process umask (typically 0o022
        // -> mode 0o755): world-connectable on every Unix host. Without
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
                path: self.socket_path.clone(),
                source,
            });
        }

        // Atomically publish the secured socket at the canonical path.
        // `rename(2)` is atomic and replaces a stale socket left by a
        // prior daemon in a single step.
        if let Err(source) = fs::rename(&tmp_path, &self.socket_path) {
            // Publish failed (e.g. cross-device move, or the parent dir
            // was removed underneath us). Drop the temp socket so it does
            // not linger, then surface the failure.
            let _ = fs::remove_file(&tmp_path);
            return Err(DaemonStartError::Bind {
                path: self.socket_path.clone(),
                source,
            });
        }

        Ok(listener)
    }

    fn remove_owned_socket_file(&self) -> io::Result<()> {
        match fs::symlink_metadata(&self.socket_path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "refusing to remove non-socket daemon path {}",
                            self.socket_path.display()
                        ),
                    ));
                }
                let euid = current_euid();
                if metadata.uid() != euid {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "refusing to remove daemon socket {} owned by uid {}, current uid {euid}",
                            self.socket_path.display(),
                            metadata.uid()
                        ),
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }

        match fs::remove_file(&self.socket_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Construct a per-attempt temporary socket path next to `socket_path`,
    /// of the form `<socket>.tmp.<pid>.<counter>`. The pid plus a
    /// process-global monotonic counter guarantees the path is unique to
    /// this bind attempt, so two concurrent [`start_server`] calls — in the
    /// same process or across processes — never collide on the temp name
    /// and the subsequent `rename(2)` is always a clean atomic publish.
    /// Sentinel: bd-3ik2d atomic-rename.
    fn temp_bind_path(&self) -> PathBuf {
        static TEMP_BIND_COUNTER: AtomicU64 = AtomicU64::new(0);
        let suffix = format!(
            ".tmp.{}.{}",
            std::process::id(),
            TEMP_BIND_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let mut file_name = self
            .socket_path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();
        file_name.push(&suffix);
        let mut tmp_path = self.socket_path.clone();
        tmp_path.set_file_name(file_name);
        tmp_path
    }

    fn socket_publish_lock_path(&self) -> PathBuf {
        let mut file_name = self
            .socket_path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("daemon.sock"));
        file_name.push(".start.lock");
        let mut lock_path = self.socket_path.clone();
        lock_path.set_file_name(file_name);
        lock_path
    }

    fn validate_socket_parent(parent: &Path) -> Result<(), DaemonStartError> {
        let metadata =
            fs::symlink_metadata(parent).map_err(|source| DaemonStartError::SocketDirCreate {
                path: parent.to_path_buf(),
                source,
            })?;
        if !metadata.file_type().is_dir() {
            return Err(DaemonStartError::InsecureSocketParent {
                path: parent.to_path_buf(),
                reason: "parent is not a real directory".to_owned(),
            });
        }

        let euid = current_euid();
        if metadata.uid() != euid {
            return Err(DaemonStartError::InsecureSocketParent {
                path: parent.to_path_buf(),
                reason: format!(
                    "parent is owned by uid {}, not current uid {euid}",
                    metadata.uid()
                ),
            });
        }

        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(DaemonStartError::InsecureSocketParent {
                path: parent.to_path_buf(),
                reason: format!(
                    "parent mode 0o{mode:o} grants group or other access; expected 0o700 or stricter"
                ),
            });
        }

        Ok(())
    }

    fn open_daemon_socket_lock_file(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        Self::configure_daemon_socket_lock_options(&mut options);
        options.open(path)
    }

    fn configure_daemon_socket_lock_options(options: &mut OpenOptions) {
        use std::os::unix::fs::OpenOptionsExt;

        options
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }

    fn existing_socket_accepts_connection(&self) -> bool {
        UnixStream::connect(&self.socket_path).is_ok()
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
    start_server_with_dispatch_policy(socket_path, DaemonDispatchPolicy::default())
}

/// Bind a UDS at `socket_path` for a daemon scoped to `workspace_id`.
/// Workspace-bound methods (currently `ee.daemon.context`) must carry
/// the same workspace id in their request envelope or dispatch refuses
/// them with `daemon_method_unauthorized`.
pub fn start_server_for_workspace(
    socket_path: impl Into<PathBuf>,
    workspace_id: impl Into<String>,
) -> Result<DaemonServerHandle, DaemonStartError> {
    start_server_with_dispatch_policy(
        socket_path,
        DaemonDispatchPolicy::for_workspace(workspace_id),
    )
}

fn start_server_with_dispatch_policy(
    socket_path: impl Into<PathBuf>,
    mut dispatch_policy: DaemonDispatchPolicy,
) -> Result<DaemonServerHandle, DaemonStartError> {
    let broker = SocketBroker::new(socket_path);
    let (listener, _publish_lock) = broker.publish_listener()?;
    let socket_path = broker.socket_path().to_path_buf();

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_in_thread = Arc::clone(&shutdown);
    let listener_path_in_thread = socket_path.clone();
    let pool = InflightPool::new(configured_max_inflight());
    let pool_in_thread = Arc::clone(&pool);

    // Host the long-lived write-owner actor when bound to a workspace (Inc 2,
    // bd-wx6ou.3): a current_thread asupersync runtime drives the actor task;
    // a `DaemonWriteRouter` (shared runtime + handle clone) is stashed in the
    // dispatch policy so `dispatch_write` routes through it. The accept and
    // connection paths already carry `Arc<DaemonDispatchPolicy>`, so no extra
    // parameter threading is needed.
    let (write_runtime, write_handle, write_owner_task) =
        if dispatch_policy.bound_workspace_id.is_some() {
            let runtime = Arc::new(crate::core::build_cli_runtime().map_err(|error| {
                DaemonStartError::Bind {
                    path: socket_path.clone(),
                    source: io::Error::other(format!(
                        "failed to build daemon write runtime: {error}"
                    )),
                }
            })?);
            let (owner, write_handle) = crate::core::write_owner::WriteOwner::new(
                crate::core::write_owner::DEFAULT_CHANNEL_CAPACITY,
            );
            let owner_task = runtime
                .handle()
                .try_spawn(async move {
                    let Some(cx) = asupersync::Cx::current() else {
                        return;
                    };
                    // Inc 3: enable group-commit so the actor accumulates a
                    // batch; a homogeneous ee.daemon.journal batch coalesces
                    // into ONE transaction/fsync (execute_journal_batch), while
                    // remember ops (which open their own connection) stay per-op.
                    let write_config = crate::core::write_owner::WriteHotPathConfig {
                        enabled: true,
                        group_commit_max_rows: 64,
                        group_commit_max_us: 1_000,
                        ..crate::core::write_owner::WriteHotPathConfig::default()
                    };
                    let _ = owner
                        .run_group_commit(&cx, write_config, |operations| {
                            if batch_is_all_journal(operations) {
                                execute_journal_batch(operations)
                            } else {
                                Ok(operations
                                    .iter()
                                    .map(execute_write_operation)
                                    .collect::<Vec<_>>())
                            }
                        })
                        .await;
                })
                .map_err(|error| DaemonStartError::Bind {
                    path: socket_path.clone(),
                    source: io::Error::other(format!("failed to spawn daemon write actor: {error}")),
                })?;
            dispatch_policy.write_router = Some(DaemonWriteRouter {
                runtime: Arc::clone(&runtime),
                handle: write_handle.clone(),
            });
            (Some(runtime), Some(write_handle), Some(owner_task))
        } else {
            (None, None, None)
        };

    let dispatch_policy = Arc::new(dispatch_policy);
    let dispatch_policy_in_thread = Arc::clone(&dispatch_policy);
    let (accept_ready_tx, accept_ready_rx) = mpsc::channel();

    let accept_thread = thread::Builder::new()
        .name("ee-daemon-accept".to_owned())
        .spawn(move || {
            let _ = accept_ready_tx.send(());
            run_accept_loop(
                listener,
                listener_path_in_thread,
                shutdown_in_thread,
                pool_in_thread,
                dispatch_policy_in_thread,
            );
        })
        .map_err(|source| DaemonStartError::Bind {
            path: socket_path.clone(),
            source,
        })?;
    if let Err(source) = accept_ready_rx.recv() {
        let _ = accept_thread.join();
        return Err(DaemonStartError::Bind {
            path: socket_path,
            source: io::Error::other(format!(
                "daemon accept thread did not report readiness: {source}"
            )),
        });
    }

    // Spawn the background steward scheduler when the daemon is bound to a
    // workspace (bd-2ohzq). Uses the same `shutdown` signal as the accept
    // loop so a single `DaemonServerHandle::shutdown()` call stops both.
    let scheduler_thread = dispatch_policy
        .bound_workspace_id
        .as_deref()
        .and_then(|workspace| {
            let scheduler_shutdown = Arc::clone(&shutdown);
            let workspace = workspace.to_owned();
            let (done_tx, done_rx) = mpsc::channel();
            let join = thread::Builder::new()
                .name("ee-daemon-steward".to_owned())
                .spawn(move || {
                    let exit = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::steward::run_daemon_background_scheduler(
                            &workspace,
                            scheduler_shutdown,
                        );
                    })) {
                        Ok(()) => SchedulerThreadExit::Returned,
                        Err(_) => SchedulerThreadExit::Panicked,
                    };
                    let _ = done_tx.send(exit);
                })
                .ok()?;
            Some(SchedulerThreadHandle { join, done_rx })
        });

    Ok(DaemonServerHandle {
        socket_path,
        shutdown,
        pool,
        accept_thread: Some(accept_thread),
        scheduler_thread,
        shutdown_done: AtomicBool::new(false),
        workers_drained: AtomicBool::new(false),
        write_handle,
        write_owner_task,
        write_runtime,
    })
}

trait ConnectionWorkerSpawner {
    fn spawn_connection_worker(
        &self,
        stream: UnixStream,
        shutdown: Arc<AtomicBool>,
        dispatch_policy: Arc<DaemonDispatchPolicy>,
        metrics: Arc<dyn super::metrics::DaemonMetricsCollector>,
        permit: InflightPermit,
    ) -> io::Result<JoinHandle<()>>;
}

#[derive(Clone, Copy)]
struct ThreadConnectionWorkerSpawner;

impl ConnectionWorkerSpawner for ThreadConnectionWorkerSpawner {
    fn spawn_connection_worker(
        &self,
        stream: UnixStream,
        shutdown: Arc<AtomicBool>,
        dispatch_policy: Arc<DaemonDispatchPolicy>,
        metrics: Arc<dyn super::metrics::DaemonMetricsCollector>,
        permit: InflightPermit,
    ) -> io::Result<JoinHandle<()>> {
        thread::Builder::new()
            .name("ee-daemon-conn".to_owned())
            .spawn(move || {
                // Permit is held for the lifetime of the worker; on drop
                // the counter decrements and the next accept can proceed.
                handle_connection(stream, shutdown, dispatch_policy, metrics);
                drop(permit);
            })
    }
}

fn run_accept_loop(
    listener: UnixListener,
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    pool: Arc<InflightPool>,
    dispatch_policy: Arc<DaemonDispatchPolicy>,
) {
    let metrics: Arc<dyn super::metrics::DaemonMetricsCollector> =
        Arc::new(super::metrics::NoopMetricsCollector);
    run_accept_loop_with_spawner(
        listener,
        socket_path,
        shutdown,
        pool,
        dispatch_policy,
        metrics,
        ThreadConnectionWorkerSpawner,
    );
}

fn run_accept_loop_with_spawner<S>(
    listener: UnixListener,
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    pool: Arc<InflightPool>,
    dispatch_policy: Arc<DaemonDispatchPolicy>,
    metrics: Arc<dyn super::metrics::DaemonMetricsCollector>,
    spawner: S,
) where
    S: ConnectionWorkerSpawner,
{
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
                            let worker_shutdown = Arc::clone(&shutdown);
                            let worker_policy = Arc::clone(&dispatch_policy);
                            let worker_metrics = Arc::clone(&metrics);
                            let spawn_result = spawner.spawn_connection_worker(
                                worker_stream,
                                worker_shutdown,
                                worker_policy,
                                worker_metrics,
                                permit,
                            );
                            if let Err(error) = spawn_result {
                                // Thread spawn itself failed (resource
                                // exhaustion). The failed closure drops the
                                // permit immediately; keep the original stream
                                // available so the client still receives the
                                // bounded-pool refusal envelope.
                                metrics.record_worker_spawn_failure(error.kind());
                                let mut rejected = stream;
                                write_overloaded_response(&mut rejected);
                            }
                        }
                        Err(error) => {
                            // Duplicating the stream failed before the worker
                            // owned a descriptor. Release the permit and refuse
                            // the client with the same overloaded envelope; the
                            // daemon is unable to service this connection.
                            metrics.record_stream_clone_failure(error.kind());
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
                trace_daemon_accept_loop_terminated(&socket_path, &error);
                metrics.record_accept_loop_terminated(error.kind());
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
    let response = daemon_shutting_down_response("<shutdown>", "<unknown>", None);
    let _ = write_response(stream, &response);
}

fn daemon_shutting_down_response(
    request_id: impl Into<String>,
    agent_id: impl Into<String>,
    workspace_id: Option<String>,
) -> DaemonResponse {
    DaemonResponse::err(
        request_id,
        agent_id,
        workspace_id,
        super::DAEMON_SHUTTING_DOWN_CODE,
        "daemon is shutting down and is no longer accepting connections; retry against a \
         fresh daemon or fall back to the in-process CLI path.",
    )
    .with_degraded(super::DAEMON_SHUTTING_DOWN_CODE)
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

#[must_use]
fn frame_error_closes_without_response(error: &FrameReadError) -> bool {
    match error {
        FrameReadError::Eof | FrameReadError::Truncated { .. } => true,
        FrameReadError::Io(source) => matches!(
            source.kind(),
            io::ErrorKind::TimedOut
                | io::ErrorKind::WouldBlock
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
        ),
        FrameReadError::TooLarge { .. } | FrameReadError::Decode(_) => false,
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

fn trace_daemon_accept_loop_terminated(socket_path: &Path, error: &io::Error) {
    let io_error_kind = io_error_kind_label(error.kind());
    tracing::warn!(
        workspace_id = "daemon-rpc",
        request_id = "<accept-loop>",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-n0o5m"),
        surface = "daemon_rpc",
        phase = "accept_loop",
        event = "ee.daemon.accept_loop_terminated",
        socket_path = %socket_path.display(),
        io_error_kind,
        "ee.daemon.accept_loop_terminated"
    );
}

fn io_error_kind_label(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::ConnectionRefused => "connection_refused",
        io::ErrorKind::ConnectionReset => "connection_reset",
        io::ErrorKind::ConnectionAborted => "connection_aborted",
        io::ErrorKind::NotConnected => "not_connected",
        io::ErrorKind::AddrInUse => "addr_in_use",
        io::ErrorKind::AddrNotAvailable => "addr_not_available",
        io::ErrorKind::BrokenPipe => "broken_pipe",
        io::ErrorKind::AlreadyExists => "already_exists",
        io::ErrorKind::WouldBlock => "would_block",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::WriteZero => "write_zero",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::UnexpectedEof => "unexpected_eof",
        io::ErrorKind::OutOfMemory => "out_of_memory",
        _ => "other",
    }
}

fn handle_connection(
    mut stream: UnixStream,
    shutdown: Arc<AtomicBool>,
    dispatch_policy: Arc<DaemonDispatchPolicy>,
    metrics: Arc<dyn super::metrics::DaemonMetricsCollector>,
) {
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
        Err(error) if frame_error_closes_without_response(&error) => return,
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

    if shutdown.load(Ordering::SeqCst) {
        let response = daemon_shutting_down_response(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
        );
        let degraded: Vec<&str> = response.degraded_codes.iter().map(String::as_str).collect();
        trace_daemon_rpc_dispatch(
            &request.request_id,
            &request.method,
            peer,
            elapsed_ms_since(started),
            super::DAEMON_SHUTTING_DOWN_CODE,
            &degraded,
            false,
            false,
        );
        let _ = write_response(&mut stream, &response);
        return;
    }

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
        super::metrics::instrument_dispatch(&request.method, metrics.as_ref(), || {
            dispatch_with_policy_and_shutdown(&request, dispatch_policy.as_ref(), shutdown.as_ref())
        })
    }));
    let response = match dispatched {
        Ok(response) => response,
        Err(payload) => {
            metrics.record_handler_panic(&request.method);
            build_panic_response(&request, payload.as_ref())
        }
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

fn dispatch_with_policy_and_shutdown(
    request: &DaemonRequest,
    policy: &DaemonDispatchPolicy,
    shutdown: &AtomicBool,
) -> DaemonResponse {
    dispatch_with_echo_policy_and_workspace(
        request,
        daemon_echo_enabled(),
        policy.bound_workspace_id(),
        shutdown,
        policy.write_router(),
    )
}

fn dispatch_with_echo_policy(request: &DaemonRequest, echo_enabled: bool) -> DaemonResponse {
    let shutdown = AtomicBool::new(false);
    dispatch_with_echo_policy_and_workspace(request, echo_enabled, None, &shutdown, None)
}

fn dispatch_with_echo_policy_and_workspace(
    request: &DaemonRequest,
    echo_enabled: bool,
    bound_workspace_id: Option<&str>,
    shutdown: &AtomicBool,
    write_router: Option<&DaemonWriteRouter>,
) -> DaemonResponse {
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

    let Some(authority) = daemon_method_authority(&request.method) else {
        let other = request.method.as_str();
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_UNKNOWN_METHOD_CODE,
            format!("unknown daemon method `{other}`"),
        );
    };
    if let Err(response) = authorize_daemon_method(request, authority, bound_workspace_id) {
        return response;
    }

    match request.method.as_str() {
        METHOD_CAPABILITIES => DaemonResponse::ok(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            daemon_capabilities_result(),
        ),
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
        METHOD_SHUTDOWN => {
            shutdown.store(true, Ordering::SeqCst);
            DaemonResponse::ok(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
                serde_json::json!({
                    "schema": "ee.daemon.shutdown.v1",
                    "accepted": true
                }),
            )
        }
        METHOD_CONTEXT => dispatch_context(request, shutdown),
        METHOD_TELEMETRY => dispatch_telemetry(request),
        METHOD_WRITE => dispatch_write(request, write_router),
        METHOD_WRITE_JOURNAL => dispatch_journal(request, write_router),
        _ => unreachable!("registered daemon methods are handled above"),
    }
}

/// Dispatch `ee.daemon.telemetry`: snapshot the daemon process's live,
/// process-global contention telemetry and return it as a serialized
/// `ee.diag.contention.v1` report. Running inside the daemon process is the
/// whole point — the group-commit counters here reflect real coalescing
/// accumulated across many writes, which a one-shot CLI invocation (a
/// single-writer fallback with its own zeroed atomics) cannot see. Sources
/// that require a live actor/pool handle (write-owner queue depth/wait,
/// read-pool stats, ledger lock-wait percentiles) are left `None` and surface
/// as `unavailableSources` in the report; they are wired in a follow-up once
/// their process-global accessors are threaded through. bd-d67os.12.
fn dispatch_telemetry(request: &DaemonRequest) -> DaemonResponse {
    let group_commit = crate::core::write_owner::write_group_commit_telemetry(None);
    let singleflight = crate::core::singleflight::singleflight_posture_report();
    let inputs = crate::core::contention::ContentionInputs {
        singleflight: Some(singleflight),
        group_commit: Some((&group_commit).into()),
        ..crate::core::contention::ContentionInputs::default()
    };
    let report = crate::core::contention::build_contention_report(&inputs);
    match serde_json::to_value(&report) {
        Ok(result) => DaemonResponse::ok(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            result,
        ),
        Err(error) => DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_TELEMETRY_ENCODE_FAILED_CODE,
            format!("failed to serialize contention telemetry: {error}"),
        ),
    }
}

/// Per-connection carrier for the write-routing path (Inc 2, bd-wx6ou.3): the
/// daemon's shared asupersync runtime plus a clone of the long-lived
/// write-owner actor's submit handle. Cloned into each connection worker so
/// `dispatch_write` can submit a `WriteOperation` and `block_on` the resulting
/// `WriteResult`. `Runtime` is `Send + Sync` (it wraps `Arc<RuntimeInner>`), so
/// `Arc<Runtime>` shares safely across the accept/connection threads.
#[derive(Clone)]
struct DaemonWriteRouter {
    runtime: Arc<asupersync::runtime::Runtime>,
    handle: crate::core::write_owner::WriteHandle,
}

impl std::fmt::Debug for DaemonWriteRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The runtime and write handle have no meaningful Debug; this exists
        // only so `DaemonDispatchPolicy` can keep deriving Debug.
        f.debug_struct("DaemonWriteRouter").finish_non_exhaustive()
    }
}

/// Parsed `ee.daemon.write` params: the inputs `remember_memory` needs, carried
/// over the socket so the daemon executes a write byte-identical to
/// `ee remember`. Owns its strings so `RememberMemoryOptions` can borrow them.
///
/// Serde is derived (snake_case) so these params can ride through the
/// long-lived write-owner actor as a `WriteOperation::Custom` payload (Inc 2,
/// bd-wx6ou.3): the low-level `WriteOperation::MemoryCreate` variant lacks the
/// `confidence`/`workflow_id`/`auto_link`/`propose_candidates` fields a faithful
/// `ee remember` needs, so we carry the whole owned params object instead.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct DaemonWriteParams {
    workspace_path: PathBuf,
    content: String,
    level: String,
    kind: String,
    tags: Option<String>,
    confidence: f32,
    source: Option<String>,
    workflow_id: Option<String>,
    auto_link: bool,
    propose_candidates: bool,
}

impl DaemonWriteParams {
    fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "`params` must be a JSON object for ee.daemon.write".to_string())?;
        #[allow(clippy::cast_possible_truncation)]
        let confidence = object
            .get("confidence")
            .and_then(serde_json::Value::as_f64)
            .map_or(0.8_f32, |value| value as f32);
        Ok(Self {
            workspace_path: required_path_any(
                object,
                &["workspacePath", "workspace_path", "workspace"],
            )?,
            content: required_string_any(object, &["content"])?,
            level: optional_string_any(object, &["level"])?
                .unwrap_or_else(|| "episodic".to_string()),
            kind: optional_string_any(object, &["kind"])?.unwrap_or_else(|| "fact".to_string()),
            tags: optional_string_any(object, &["tags"])?,
            confidence,
            source: optional_string_any(object, &["source"])?,
            workflow_id: optional_string_any(object, &["workflow", "workflowId", "workflow_id"])?,
            auto_link: optional_bool_any(object, &["autoLink", "auto_link"])?.unwrap_or(true),
            propose_candidates: optional_bool_any(object, &["proposeCandidates", "propose_candidates"])?
                .unwrap_or(true),
        })
    }

    fn options(&self) -> crate::core::memory::RememberMemoryOptions<'_> {
        crate::core::memory::RememberMemoryOptions {
            workspace_path: &self.workspace_path,
            database_path: None,
            content: &self.content,
            workflow_id: self.workflow_id.as_deref(),
            level: &self.level,
            kind: &self.kind,
            tags: self.tags.as_deref(),
            confidence: self.confidence,
            source: self.source.as_deref(),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: self.auto_link,
            propose_candidates: self.propose_candidates,
        }
    }

    /// `operation_type` tag for the `WriteOperation::Custom` that carries these
    /// params through the write-owner actor (Inc 2).
    const ACTOR_OPERATION_TYPE: &'static str = "ee.daemon.remember";

    /// Serialize into the `WriteOperation::Custom` payload submitted to the
    /// actor. Round-trips with [`DaemonWriteParams::from_payload`].
    fn to_payload(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Reconstruct from a `WriteOperation::Custom` payload inside the actor's
    /// `process_batch`. Errors map to a domain write failure for that op.
    fn from_payload(payload: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(payload.clone()).map_err(|error| error.to_string())
    }
}

/// Dispatch `ee.daemon.write`: execute a durable memory write. Inc 1 routes it
/// straight through `remember_memory` (no actor), so the result is identical to
/// `ee remember`. RPC-level failures (bad params) become a `DaemonResponse::err`;
/// a domain-level write failure becomes an `ok` response carrying
/// `{success:false, error:{code,message}}` so the client can distinguish "the
/// daemon could not accept this" from "the write itself failed". bd-wx6ou.2.
fn dispatch_write(
    request: &DaemonRequest,
    write_router: Option<&DaemonWriteRouter>,
) -> DaemonResponse {
    let params = match DaemonWriteParams::from_value(&request.params) {
        Ok(params) => params,
        Err(message) => {
            return DaemonResponse::err(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
                DAEMON_WRITE_PARAMS_INVALID_CODE,
                message,
            );
        }
    };
    // When the long-lived write-owner actor is hosted (bound workspace), route
    // the write through it so it can coalesce with siblings (Inc 2 wires the
    // path; real batching is Inc 3). Otherwise fall through to the in-process
    // direct path (Inc 1), which is the source of truth and is never removed.
    if let Some(router) = write_router {
        return dispatch_write_via_actor(request, &params, router);
    }
    let result = match crate::core::memory::remember_memory(&params.options()) {
        Ok(report) => serde_json::json!({
            "schema": "ee.daemon.write.v1",
            "success": true,
            "entityId": report.memory_id.to_string(),
        }),
        Err(error) => serde_json::json!({
            "schema": "ee.daemon.write.v1",
            "success": false,
            "error": {
                "code": error.code(),
                "message": error.message(),
            },
        }),
    };
    DaemonResponse::ok(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
        result,
    )
}

/// Route a parsed write through the long-lived write-owner actor and block the
/// connection thread until the (eventually coalesced) `WriteResult` returns.
/// The connection thread is a plain std::thread, so `runtime.block_on` parks it
/// while the actor task (on the runtime's worker thread) runs `process_batch`
/// and sends on the oneshot — no deadlock (distinct threads). bd-wx6ou.3.
fn dispatch_write_via_actor(
    request: &DaemonRequest,
    params: &DaemonWriteParams,
    router: &DaemonWriteRouter,
) -> DaemonResponse {
    let operation = crate::core::write_owner::WriteOperation::Custom {
        operation_type: DaemonWriteParams::ACTOR_OPERATION_TYPE.to_string(),
        payload: params.to_payload(),
    };
    submit_op_via_actor(request, router, operation)
}

/// Submit a prepared write op to the long-lived actor and block the connection
/// thread on the (eventually coalesced) `WriteResult`, mapping it to the
/// `ee.daemon.write.v1` response shape. Shared by the remember (Inc 2) and
/// journal (Inc 4) daemon write paths. bd-wx6ou.
fn submit_op_via_actor(
    request: &DaemonRequest,
    router: &DaemonWriteRouter,
    operation: crate::core::write_owner::WriteOperation,
) -> DaemonResponse {
    use crate::core::write_owner::WriteResult;
    let Some(mut receiver) = router.handle.try_submit(operation) else {
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            super::DAEMON_OVERLOADED_CODE,
            "write-owner actor queue is saturated; retry shortly",
        )
        .with_degraded(super::DAEMON_OVERLOADED_CODE);
    };
    let write_result = router.runtime.block_on(async {
        let cx = asupersync::Cx::current().expect("Runtime::block_on installs an ambient Cx");
        receiver.recv(&cx).await
    });
    let result = match write_result {
        Ok(WriteResult::Success { entity_id }) => serde_json::json!({
            "schema": "ee.daemon.write.v1",
            "success": true,
            "entityId": entity_id,
        }),
        Ok(WriteResult::Failed { error }) => serde_json::json!({
            "schema": "ee.daemon.write.v1",
            "success": false,
            "error": { "code": error.code(), "message": error.message() },
        }),
        Ok(WriteResult::Shutdown) => serde_json::json!({
            "schema": "ee.daemon.write.v1",
            "success": false,
            "error": {
                "code": super::DAEMON_SHUTTING_DOWN_CODE,
                "message": "write-owner actor is shutting down",
            },
        }),
        Err(_) => serde_json::json!({
            "schema": "ee.daemon.write.v1",
            "success": false,
            "error": {
                "code": "write_owner_unavailable",
                "message": "write-owner actor dropped the request before responding",
            },
        }),
    };
    DaemonResponse::ok(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
        result,
    )
}

/// Dispatch `ee.daemon.write_journal`: route a journal write through the actor
/// (coalesced with sibling journal writes) when the actor is hosted, else
/// execute it directly as a single-op batch (still durable, no coalescing). The
/// request params ARE the `DaemonJournalParams` payload, carried verbatim into
/// the `ee.daemon.journal` op. bd-wx6ou.5 (Inc 4 daemon half).
fn dispatch_journal(
    request: &DaemonRequest,
    write_router: Option<&DaemonWriteRouter>,
) -> DaemonResponse {
    use crate::core::write_owner::{WriteOperation, WriteResult};
    if let Err(message) = DaemonJournalParams::from_payload(&request.params) {
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_JOURNAL_PARAMS_INVALID_CODE,
            message,
        );
    }
    let operation = WriteOperation::Custom {
        operation_type: DaemonJournalParams::ACTOR_OPERATION_TYPE.to_string(),
        payload: request.params.clone(),
    };
    if let Some(router) = write_router {
        return submit_op_via_actor(request, router, operation);
    }
    // No actor (unbound daemon): execute the journal write directly as a
    // single-op batch — durable, just not coalesced.
    let result = match execute_journal_batch(std::slice::from_ref(&operation)) {
        Ok(mut results) => {
            let entity_id = match results.pop() {
                Some(WriteResult::Success { entity_id }) => entity_id,
                _ => None,
            };
            serde_json::json!({
                "schema": "ee.daemon.write.v1",
                "success": true,
                "entityId": entity_id,
            })
        }
        Err(error) => serde_json::json!({
            "schema": "ee.daemon.write.v1",
            "success": false,
            "error": { "code": error.code(), "message": error.message() },
        }),
    };
    DaemonResponse::ok(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
        result,
    )
}

/// Execute one `WriteOperation` inside the write-owner actor's `process_batch`
/// (Inc 2, bd-wx6ou.3). Only the `ee.daemon.remember` Custom op is supported —
/// the daemon write path carries `DaemonWriteParams` in the payload and runs
/// the same `remember_memory` the direct path uses, so a daemon-routed write is
/// byte-identical to `ee remember`. Unsupported ops fail that single request
/// (the actor maps a per-op error onto its `WriteResult`).
fn execute_write_operation(
    operation: &crate::core::write_owner::WriteOperation,
) -> crate::core::write_owner::WriteResult {
    use crate::core::write_owner::{WriteOperation, WriteResult};
    let WriteOperation::Custom {
        operation_type,
        payload,
    } = operation
    else {
        return WriteResult::Failed {
            error: crate::models::DomainError::Storage {
                message: format!(
                    "write-owner actor received an unsupported operation: {}",
                    operation.operation_type()
                ),
                repair: Some("only ee.daemon.remember is wired in this build".to_string()),
            },
        };
    };
    if operation_type != DaemonWriteParams::ACTOR_OPERATION_TYPE {
        return WriteResult::Failed {
            error: crate::models::DomainError::Storage {
                message: format!(
                    "write-owner actor received an unsupported custom op: {operation_type}"
                ),
                repair: Some("only ee.daemon.remember is wired in this build".to_string()),
            },
        };
    }
    let params = match DaemonWriteParams::from_payload(payload) {
        Ok(params) => params,
        Err(message) => {
            return WriteResult::Failed {
                error: crate::models::DomainError::Storage {
                    message: format!("daemon write payload decode failed: {message}"),
                    repair: Some("retry the write".to_string()),
                },
            };
        }
    };
    match crate::core::memory::remember_memory(&params.options()) {
        Ok(report) => WriteResult::Success {
            entity_id: Some(report.memory_id.to_string()),
        },
        Err(error) => WriteResult::Failed { error },
    }
}

/// Parsed `ee.daemon.journal` op payload (Inc 3, bd-wx6ou.4): the inputs a
/// journal write needs, carried over the socket. Owns its strings. Defines our
/// own payload (rather than reusing the direct-path `journal_append` shape) so
/// the daemon journal path does not depend on `core::journal` internals.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct DaemonJournalParams {
    workspace_path: PathBuf,
    workspace_id: String,
    /// Caller-supplied id for idempotency (Inc 4); generated when absent.
    entry_id: Option<String>,
    agent_name: Option<String>,
    session_key: Option<String>,
    kind: String,
    source: String,
    body: String,
    structured: Option<String>,
    redaction_report: String,
    instruction_risk: String,
}

impl DaemonJournalParams {
    /// `operation_type` tag for the `WriteOperation::Custom` carrying a journal
    /// write through the actor. Distinct from the direct-path `journal_append`.
    const ACTOR_OPERATION_TYPE: &'static str = "ee.daemon.journal";

    fn from_payload(payload: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(payload.clone()).map_err(|error| error.to_string())
    }

    fn database_path(&self) -> PathBuf {
        self.workspace_path.join(".ee").join("ee.db")
    }

    fn into_create_input(self) -> crate::db::CreateJournalEntryInput {
        crate::db::CreateJournalEntryInput {
            entry_id: self
                .entry_id
                .unwrap_or_else(crate::core::journal::generate_journal_entry_id),
            workspace_id: self.workspace_id,
            agent_name: self.agent_name,
            session_key: self.session_key,
            kind: self.kind,
            source: self.source,
            body: self.body,
            structured: self.structured,
            redaction_report: self.redaction_report,
            instruction_risk: self.instruction_risk,
        }
    }
}

/// True when every op in the batch is an `ee.daemon.journal` Custom op (so the
/// homogeneous batch can be coalesced into one transaction). bd-wx6ou.4.
fn batch_is_all_journal(operations: &[crate::core::write_owner::WriteOperation]) -> bool {
    use crate::core::write_owner::WriteOperation;
    !operations.is_empty()
        && operations.iter().all(|operation| {
            matches!(
                operation,
                WriteOperation::Custom { operation_type, .. }
                    if operation_type == DaemonJournalParams::ACTOR_OPERATION_TYPE
            )
        })
}

/// Execute a homogeneous batch of `ee.daemon.journal` ops in ONE transaction
/// (Inc 3, bd-wx6ou.4): open the workspace DB connection once, insert all N
/// journal entries inside a single `with_transaction` so the whole batch shares
/// ONE commit/fsync — the actual coalescing win. All ops target the daemon's
/// bound workspace, so one connection serves them. All-or-nothing: a failed
/// insert rolls back the batch and fails every request in it (the CLI falls
/// back to the direct path per Inc 4).
fn execute_journal_batch(
    operations: &[crate::core::write_owner::WriteOperation],
) -> Result<Vec<crate::core::write_owner::WriteResult>, crate::models::DomainError> {
    use crate::core::write_owner::{WriteOperation, WriteResult};
    let mut params = Vec::with_capacity(operations.len());
    for operation in operations {
        let WriteOperation::Custom { payload, .. } = operation else {
            return Err(crate::models::DomainError::Storage {
                message: "journal batch received a non-Custom operation".to_string(),
                repair: Some("only ee.daemon.journal ops are batched here".to_string()),
            });
        };
        params.push(DaemonJournalParams::from_payload(payload).map_err(|message| {
            crate::models::DomainError::Storage {
                message: format!("daemon journal payload decode failed: {message}"),
                repair: Some("retry the write".to_string()),
            }
        })?);
    }
    let Some(first) = params.first() else {
        return Ok(Vec::new());
    };
    let database_path = first.database_path();
    let connection =
        crate::db::DbConnection::open_file(&database_path).map_err(|error| {
            crate::models::DomainError::Storage {
                message: format!(
                    "daemon journal batch could not open {}: {error}",
                    database_path.display()
                ),
                repair: Some("ensure the workspace is initialized".to_string()),
            }
        })?;
    connection
        .with_transaction(|| {
            let mut out = Vec::with_capacity(params.len());
            for entry in params.drain(..) {
                let input = entry.into_create_input();
                let entry_id = input.entry_id.clone();
                connection.insert_journal_entry(&input)?;
                out.push(WriteResult::Success {
                    entity_id: Some(entry_id),
                });
            }
            Ok(out)
        })
        .map_err(|error| crate::models::DomainError::Storage {
            message: format!("daemon journal batch transaction failed: {error}"),
            repair: Some("retry the write".to_string()),
        })
}

#[derive(Clone, Debug, PartialEq)]
struct DaemonContextParams {
    query: String,
    workspace_path: PathBuf,
    database_path: Option<PathBuf>,
    index_dir: Option<PathBuf>,
    max_tokens: Option<u32>,
    candidate_pool: Option<u32>,
    max_results: Option<u32>,
    profile: Option<ContextPackProfile>,
    speed: SpeedMode,
    source_mode: SearchSourceMode,
    strict_source_mode: bool,
    pack_profile: crate::core::context::ContextPackOutputProfile,
    resource_profile: PackResourceProfile,
    no_coverage_fill: Option<bool>,
    no_rendered_text: Option<bool>,
    no_skipped: Option<bool>,
    no_meta: Option<bool>,
    include_non_affecting_degradations: Option<bool>,
    explain: bool,
    no_pack_dna: bool,
    read_only: bool,
    timeout_ms: Option<u64>,
}

impl DaemonContextParams {
    fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "`params` must be a JSON object for ee.daemon.context".to_string())?;
        let query = required_string_any(object, &["task", "query"])?;
        let workspace_path =
            required_path_any(object, &["workspacePath", "workspace_path", "workspace"])?;
        let profile = Some(
            optional_string_any(object, &["profile"])?
                .map(|value| parse_daemon_context_profile(&value))
                .transpose()?
                .unwrap_or(ContextPackProfile::Balanced),
        );
        let speed = optional_string_any(object, &["speed"])?
            .map(|value| parse_daemon_speed_mode(&value))
            .transpose()?
            .unwrap_or_default();
        let source_mode = optional_string_any(object, &["sourceMode", "source_mode"])?
            .map(|value| parse_daemon_source_mode(&value))
            .transpose()?
            .unwrap_or_default();
        let pack_profile = optional_string_any(object, &["packProfile", "pack_profile"])?
            .map(|value| parse_daemon_pack_output_profile(&value))
            .transpose()?
            .unwrap_or_default();
        let resource_profile =
            optional_string_any(object, &["resourceProfile", "resource_profile"])?
                .map(|value| {
                    value
                        .parse::<PackResourceProfile>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?
                .unwrap_or_default();

        Ok(Self {
            query,
            workspace_path,
            database_path: optional_path_any(
                object,
                &["databasePath", "database_path", "database"],
            )?,
            index_dir: optional_path_any(object, &["indexDir", "index_dir"])?,
            max_tokens: optional_u32_any(object, &["maxTokens", "max_tokens"])?,
            candidate_pool: optional_u32_any(object, &["candidatePool", "candidate_pool"])?,
            max_results: optional_u32_any(object, &["maxResults", "max_results"])?,
            profile,
            speed,
            source_mode,
            strict_source_mode: optional_bool_any(
                object,
                &["strictSourceMode", "strict_source_mode"],
            )?
            .unwrap_or(false),
            pack_profile,
            resource_profile,
            no_coverage_fill: optional_bool_any(object, &["noCoverageFill", "no_coverage_fill"])?,
            no_rendered_text: optional_bool_any(object, &["noRenderedText", "no_rendered_text"])?,
            no_skipped: optional_bool_any(object, &["noSkipped", "no_skipped"])?,
            no_meta: optional_bool_any(object, &["noMeta", "no_meta"])?,
            include_non_affecting_degradations: optional_bool_any(
                object,
                &[
                    "includeNonAffectingDegradations",
                    "include_non_affecting_degradations",
                ],
            )?,
            explain: optional_bool_any(object, &["explain"])?.unwrap_or(false),
            no_pack_dna: optional_bool_any(object, &["noPackDna", "no_pack_dna"])?.unwrap_or(false),
            read_only: optional_bool_any(object, &["readOnly", "read_only"])?.unwrap_or(false),
            timeout_ms: optional_u64_any(
                object,
                &["timeoutMs", "timeout_ms", "deadlineMs", "deadline_ms"],
            )?,
        })
    }

    fn output_options(&self) -> ContextPackOutputOptions {
        ContextPackOutputOptions::for_profile(self.pack_profile)
            .with_overrides(ContextPackOutputOptionOverrides {
                no_coverage_fill: self.no_coverage_fill,
                no_rendered_text: self.no_rendered_text,
                no_skipped: self.no_skipped,
                no_meta: self.no_meta,
                include_non_affecting_degradations: self.include_non_affecting_degradations,
            })
            .with_resource_profile(self.resource_profile)
    }

    fn context_options(&self) -> ContextPackOptions {
        ContextPackOptions {
            workspace_path: self.workspace_path.clone(),
            database_path: self.database_path.clone(),
            index_dir: self.index_dir.clone(),
            query: self.query.clone(),
            speed: self.speed,
            source_mode: self.source_mode,
            strict_source_mode: self.strict_source_mode,
            filters: QueryFilters::default(),
            profile: self.profile,
            max_tokens: self.max_tokens,
            candidate_pool: self.candidate_pool,
            max_results: self.max_results,
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            require_fresh_sentinels: false,
            relevance_floor: None,
            redaction_level: RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            output_options: self.output_options(),
            persist_pack: !self.read_only,
            baseline_write: None,
            // bd-1n0np.5.8: the daemon does not expose `--no-lod`; keep LOD
            // tiering on (the default), matching the one-shot CLI default.
            no_lod: false,
        }
    }
}

fn dispatch_context(request: &DaemonRequest, shutdown: &AtomicBool) -> DaemonResponse {
    if shutdown.load(Ordering::SeqCst) {
        return daemon_shutting_down_response(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
        );
    }
    let params = match DaemonContextParams::from_value(&request.params) {
        Ok(params) => params,
        Err(message) => {
            return DaemonResponse::err(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
                DAEMON_CONTEXT_PARAMS_INVALID_CODE,
                format!("invalid ee.daemon.context params: {message}"),
            );
        }
    };
    let context_started = Instant::now();
    if matches!(params.timeout_ms, Some(0)) {
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_CONTEXT_DEADLINE_EXCEEDED_CODE,
            "ee.daemon.context deadline expired before pack execution started.",
        );
    }

    let options = params.context_options();
    let deadline = params.timeout_ms.map(Duration::from_millis);
    let mut context_response = match run_context_pack_with_performance_controlled(
        &options,
        "pack",
        deadline,
        Some(shutdown),
    )
    .map(|run| run.response)
    {
        Ok(response) => response,
        Err(ContextPackError::DeadlineExceeded(error)) => {
            return DaemonResponse::err(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
                DAEMON_CONTEXT_DEADLINE_EXCEEDED_CODE,
                format!("ee.daemon.context deadline expired: {error}"),
            );
        }
        Err(ContextPackError::Cancelled(_)) => {
            return daemon_shutting_down_response(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
            );
        }
        Err(error) => {
            return DaemonResponse::err(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
                DAEMON_CONTEXT_EXECUTION_FAILED_CODE,
                format!("ee.daemon.context could not assemble the canonical pack: {error}"),
            );
        }
    };

    if shutdown.load(Ordering::SeqCst) {
        return daemon_shutting_down_response(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
        );
    }
    if daemon_context_deadline_expired(context_started, params.timeout_ms) {
        return daemon_context_deadline_response(
            request,
            "ee.daemon.context deadline expired after pack execution.",
        );
    }
    if params.explain && !params.no_pack_dna {
        let database_path = options
            .database_path
            .clone()
            .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
        attach_pack_dna_to_context_response(&database_path, &mut context_response);
    }
    if daemon_context_deadline_expired(context_started, params.timeout_ms) {
        return daemon_context_deadline_response(
            request,
            "ee.daemon.context deadline expired while attaching pack DNA.",
        );
    }

    let render_options = ContextJsonRenderOptions::from(options.output_options);
    let rendered = render_context_response_json_with_options(&context_response, render_options);
    if daemon_context_deadline_expired(context_started, params.timeout_ms) {
        return daemon_context_deadline_response(
            request,
            "ee.daemon.context deadline expired while rendering the response.",
        );
    }
    if rendered.len() > super::DAEMON_RESPONSE_MAX_BYTES {
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_CONTEXT_EXECUTION_FAILED_CODE,
            format!(
                "ee.daemon.context rendered {} bytes, exceeding the {}-byte daemon response cap; \
                 lower maxTokens or use the in-process CLI pack path.",
                rendered.len(),
                super::DAEMON_RESPONSE_MAX_BYTES
            ),
        );
    }
    let result = match serde_json::from_str::<serde_json::Value>(&rendered) {
        Ok(result) => result,
        Err(error) => {
            return DaemonResponse::err(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
                DAEMON_CONTEXT_EXECUTION_FAILED_CODE,
                format!("ee.daemon.context rendered invalid canonical JSON: {error}"),
            );
        }
    };
    let degraded_codes = result
        .pointer("/data/degraded")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("code").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut response = DaemonResponse::ok(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
        result,
    );
    for code in degraded_codes {
        response = response.with_degraded(code);
    }
    response
}

fn daemon_context_deadline_expired(started: Instant, timeout_ms: Option<u64>) -> bool {
    timeout_ms.is_some_and(|timeout_ms| started.elapsed() >= Duration::from_millis(timeout_ms))
}

fn daemon_context_deadline_response(
    request: &DaemonRequest,
    message: &'static str,
) -> DaemonResponse {
    DaemonResponse::err(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
        DAEMON_CONTEXT_DEADLINE_EXCEEDED_CODE,
        message,
    )
}

fn required_string_any(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<String, String> {
    let Some(value) = optional_string_any(object, keys)? else {
        return Err(format!("missing required field `{}`", keys[0]));
    };
    if value.trim().is_empty() {
        return Err(format!("field `{}` must not be blank", keys[0]));
    }
    Ok(value)
}

fn optional_string_any(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<Option<String>, String> {
    for key in keys {
        if let Some(value) = object.get(*key) {
            return value
                .as_str()
                .map(|text| Some(text.to_owned()))
                .ok_or_else(|| format!("field `{key}` must be a string"));
        }
    }
    Ok(None)
}

fn required_path_any(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<PathBuf, String> {
    let value = required_string_any(object, keys)?;
    Ok(PathBuf::from(value))
}

fn optional_path_any(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<Option<PathBuf>, String> {
    optional_string_any(object, keys).map(|value| value.map(PathBuf::from))
}

fn optional_bool_any(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<Option<bool>, String> {
    for key in keys {
        if let Some(value) = object.get(*key) {
            return value
                .as_bool()
                .map(Some)
                .ok_or_else(|| format!("field `{key}` must be a boolean"));
        }
    }
    Ok(None)
}

fn optional_u32_any(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<Option<u32>, String> {
    optional_u64_any(object, keys)?
        .map(|value| {
            u32::try_from(value).map_err(|_| format!("field `{}` must fit in u32", keys[0]))
        })
        .transpose()
}

fn optional_u64_any(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<Option<u64>, String> {
    for key in keys {
        if let Some(value) = object.get(*key) {
            return value
                .as_u64()
                .map(Some)
                .ok_or_else(|| format!("field `{key}` must be an unsigned integer"));
        }
    }
    Ok(None)
}

fn parse_daemon_context_profile(value: &str) -> Result<ContextPackProfile, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "compact" => Ok(ContextPackProfile::Compact),
        "balanced" => Ok(ContextPackProfile::Balanced),
        "grounding" => Ok(ContextPackProfile::Grounding),
        "orientation" => Ok(ContextPackProfile::Orientation),
        "thorough" => Ok(ContextPackProfile::Thorough),
        "submodular" => Ok(ContextPackProfile::Submodular),
        _ => Err(format!(
            "Invalid context profile `{value}`. Expected compact, balanced, grounding, orientation, thorough, or submodular."
        )),
    }
}

fn parse_daemon_speed_mode(value: &str) -> Result<SpeedMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "instant" => Ok(SpeedMode::Instant),
        "default" => Ok(SpeedMode::Default),
        "quality" => Ok(SpeedMode::Quality),
        _ => Err(format!(
            "Invalid speed mode `{value}`. Expected instant, default, or quality."
        )),
    }
}

fn parse_daemon_source_mode(value: &str) -> Result<SearchSourceMode, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "lexical_only" | "lexical" => Ok(SearchSourceMode::LexicalOnly),
        "semantic_only" | "semantic" => Ok(SearchSourceMode::SemanticOnly),
        "hybrid" => Ok(SearchSourceMode::Hybrid),
        _ => Err(format!(
            "Invalid source mode `{value}`. Expected lexical_only, semantic_only, or hybrid."
        )),
    }
}

fn parse_daemon_pack_output_profile(
    value: &str,
) -> Result<crate::core::context::ContextPackOutputProfile, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "lean" => Ok(crate::core::context::ContextPackOutputProfile::Lean),
        "standard" => Ok(crate::core::context::ContextPackOutputProfile::Standard),
        "verbose" => Ok(crate::core::context::ContextPackOutputProfile::Verbose),
        _ => Err(format!(
            "Invalid pack output profile `{value}`. Expected lean, standard, or verbose."
        )),
    }
}

fn daemon_method_authority(method: &str) -> Option<DaemonAuthority> {
    match method {
        METHOD_CAPABILITIES | METHOD_ECHO | METHOD_SHUTDOWN | METHOD_TELEMETRY => {
            Some(DaemonAuthority::SameUid)
        }
        METHOD_CONTEXT | METHOD_WRITE | METHOD_WRITE_JOURNAL => {
            Some(DaemonAuthority::SameUidWorkspace)
        }
        _ => None,
    }
}

fn authorize_daemon_method(
    request: &DaemonRequest,
    authority: DaemonAuthority,
    bound_workspace_id: Option<&str>,
) -> Result<(), DaemonResponse> {
    match authority {
        DaemonAuthority::SameUid => Ok(()),
        DaemonAuthority::SameUidWorkspace => {
            let workspace_id = request
                .workspace_id
                .as_deref()
                .map(str::trim)
                .filter(|workspace_id| !workspace_id.is_empty());
            let Some(workspace_id) = workspace_id else {
                return Err(method_unauthorized_response(
                    request,
                    "registered daemon method requires a non-empty workspace_id",
                ));
            };
            if let Some(bound_workspace_id) = bound_workspace_id
                && workspace_id != bound_workspace_id
            {
                return Err(method_unauthorized_response(
                    request,
                    "registered daemon method is not authorized for this daemon workspace",
                ));
            }
            Ok(())
        }
    }
}

fn method_unauthorized_response(request: &DaemonRequest, message: &'static str) -> DaemonResponse {
    DaemonResponse::err(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
        DAEMON_METHOD_UNAUTHORIZED_CODE,
        message,
    )
    .with_degraded(DAEMON_METHOD_UNAUTHORIZED_CODE)
}

#[allow(clippy::expect_used)]
fn daemon_capabilities_result() -> serde_json::Value {
    serde_json::json!({
        "protocol": "ee.daemon",
        "request_schemas": [super::DAEMON_REQUEST_SCHEMA_V1],
        "response_schemas": [super::DAEMON_RESPONSE_SCHEMA_V1],
        "methods": [
            METHOD_CAPABILITIES,
            METHOD_CONTEXT,
            METHOD_ECHO,
            METHOD_SHUTDOWN,
            METHOD_TELEMETRY,
            METHOD_WRITE,
            METHOD_WRITE_JOURNAL
        ],
        "authorization": {
            "ee.daemon.capabilities": daemon_method_authority(METHOD_CAPABILITIES).expect("registered method").as_wire_label(),
            "ee.daemon.context": daemon_method_authority(METHOD_CONTEXT).expect("registered method").as_wire_label(),
            "ee.daemon.echo": daemon_method_authority(METHOD_ECHO).expect("registered method").as_wire_label(),
            "ee.daemon.shutdown": daemon_method_authority(METHOD_SHUTDOWN).expect("registered method").as_wire_label(),
            "ee.daemon.telemetry": daemon_method_authority(METHOD_TELEMETRY).expect("registered method").as_wire_label(),
            "ee.daemon.write": daemon_method_authority(METHOD_WRITE).expect("registered method").as_wire_label(),
            "ee.daemon.write_journal": daemon_method_authority(METHOD_WRITE_JOURNAL).expect("registered method").as_wire_label()
        },
        "forward_compat": {
            "v1_unknown_fields": "rejected",
            "v1_unknown_methods": DAEMON_UNKNOWN_METHOD_CODE,
            "v2_migration": "Call ee.daemon.capabilities with ee.daemon.request.v1 before sending any non-v1 schema or method; downgrade to an advertised schema/method when absent."
        }
    })
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
    if response.schema != super::DAEMON_RESPONSE_SCHEMA_V1 {
        return Err(ClientError::ResponseSchemaMismatch {
            expected: super::DAEMON_RESPONSE_SCHEMA_V1,
            actual: response.schema,
        });
    }
    if response.request_id != request.request_id {
        return Err(ClientError::ResponseRequestIdMismatch {
            expected: request.request_id.clone(),
            actual: response.request_id,
        });
    }
    if response.agent_id != request.agent_id {
        return Err(ClientError::ResponseAgentIdMismatch {
            expected: request.agent_id.clone(),
            actual: response.agent_id,
        });
    }
    if response.workspace_id != request.workspace_id {
        return Err(ClientError::ResponseWorkspaceIdMismatch {
            expected: request.workspace_id.clone(),
            actual: response.workspace_id,
        });
    }
    Ok(response)
}

/// Errors that can occur on the client side of a UDS round-trip.
#[derive(Debug)]
pub enum ClientError {
    Connect(io::Error),
    Io(io::Error),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    RequestTooLarge {
        actual: usize,
    },
    ResponseTooLarge {
        announced: u32,
    },
    ResponseSchemaMismatch {
        expected: &'static str,
        actual: String,
    },
    ResponseRequestIdMismatch {
        expected: String,
        actual: String,
    },
    ResponseAgentIdMismatch {
        expected: String,
        actual: String,
    },
    ResponseWorkspaceIdMismatch {
        expected: Option<String>,
        actual: Option<String>,
    },
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
            Self::ResponseSchemaMismatch { expected, actual } => write!(
                formatter,
                "daemon response schema mismatch: expected {expected}, got {actual}"
            ),
            Self::ResponseRequestIdMismatch { expected, actual } => write!(
                formatter,
                "daemon response request_id mismatch: sent {expected}, got {actual}"
            ),
            Self::ResponseAgentIdMismatch { expected, actual } => write!(
                formatter,
                "daemon response agent_id mismatch: sent {expected}, got {actual}"
            ),
            Self::ResponseWorkspaceIdMismatch { expected, actual } => write!(
                formatter,
                "daemon response workspace_id mismatch: sent {}, got {}",
                expected.as_deref().unwrap_or("<absent>"),
                actual.as_deref().unwrap_or("<absent>")
            ),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(source) | Self::Io(source) => Some(source),
            Self::Encode(source) | Self::Decode(source) => Some(source),
            Self::RequestTooLarge { .. }
            | Self::ResponseTooLarge { .. }
            | Self::ResponseSchemaMismatch { .. }
            | Self::ResponseRequestIdMismatch { .. }
            | Self::ResponseAgentIdMismatch { .. }
            | Self::ResponseWorkspaceIdMismatch { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_AGENT_ID: &str = "agent-daemon-server-test";
    const TEST_WORKSPACE_ID: &str = "workspace-daemon-server-test";

    fn context_request(
        request_id: &'static str,
        agent_id: &'static str,
        params: serde_json::Value,
    ) -> DaemonRequest {
        let mut request = DaemonRequest::new(request_id, agent_id, METHOD_CONTEXT, params);
        request.workspace_id = Some(TEST_WORKSPACE_ID.to_owned());
        request
    }

    fn read_framed_daemon_response(stream: &mut UnixStream) -> DaemonResponse {
        use std::io::Read;

        let mut prefix = [0_u8; 4];
        stream
            .read_exact(&mut prefix)
            .expect("length prefix must arrive");
        let announced = u32::from_be_bytes(prefix) as usize;
        let mut body = vec![0_u8; announced];
        stream.read_exact(&mut body).expect("body must arrive");
        serde_json::from_slice(&body).expect("body must parse as daemon response")
    }

    fn client_round_trip_against_single_response(
        request: &DaemonRequest,
        response: DaemonResponse,
    ) -> Result<DaemonResponse, ClientError> {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-client-test.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind one-shot daemon socket");
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept one client");
            let _request = read_request(&mut stream).expect("client request frame must parse");
            write_response(&mut stream, &response).expect("response frame must write");
        });

        let result = client_round_trip(&socket_path, request);
        worker
            .join()
            .expect("one-shot daemon thread must not panic");
        result
    }

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
    fn client_round_trip_rejects_agent_id_mismatch() {
        let request = DaemonRequest::new(
            "req-agent-mismatch-001",
            TEST_AGENT_ID,
            METHOD_ECHO,
            serde_json::json!({}),
        );
        let response = DaemonResponse::err(
            request.request_id.clone(),
            "agent-spoofed",
            None,
            DAEMON_ECHO_DISABLED_CODE,
            "echo disabled",
        );

        let error = client_round_trip_against_single_response(&request, response)
            .expect_err("agent_id mismatch must be rejected");

        match error {
            ClientError::ResponseAgentIdMismatch { expected, actual } => {
                assert_eq!(expected, TEST_AGENT_ID);
                assert_eq!(actual, "agent-spoofed");
            }
            other => panic!("expected ResponseAgentIdMismatch, got {other:?}"),
        }
    }

    #[test]
    fn client_round_trip_rejects_workspace_id_mismatch() {
        let mut request = DaemonRequest::new(
            "req-workspace-mismatch-001",
            TEST_AGENT_ID,
            METHOD_CONTEXT,
            serde_json::json!({}),
        );
        request.workspace_id = Some(TEST_WORKSPACE_ID.to_owned());
        let response = DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            Some("workspace-spoofed".to_owned()),
            DAEMON_CONTEXT_PARAMS_INVALID_CODE,
            "invalid ee.daemon.context params",
        );

        let error = client_round_trip_against_single_response(&request, response)
            .expect_err("workspace_id mismatch must be rejected");

        match error {
            ClientError::ResponseWorkspaceIdMismatch { expected, actual } => {
                assert_eq!(expected.as_deref(), Some(TEST_WORKSPACE_ID));
                assert_eq!(actual.as_deref(), Some("workspace-spoofed"));
            }
            other => panic!("expected ResponseWorkspaceIdMismatch, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_context_rejects_missing_workspace_path() {
        let request = context_request(
            "req-ctx-001",
            TEST_AGENT_ID,
            serde_json::json!({"task": "ship daemon"}),
        );
        let response = dispatch(&request);
        assert!(response.result.is_none());
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_CONTEXT_PARAMS_INVALID_CODE);
        assert!(response.degraded_codes.is_empty());
    }

    #[test]
    fn dispatch_context_zero_timeout_fails_before_pack_execution() {
        let request = context_request(
            "req-ctx-timeout-001",
            TEST_AGENT_ID,
            serde_json::json!({
                "task": "ship daemon",
                "workspacePath": "/tmp/ee-daemon-context-timeout-test",
                "timeoutMs": 0
            }),
        );
        let response = dispatch(&request);
        assert!(response.result.is_none());
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_CONTEXT_DEADLINE_EXCEEDED_CODE);
        assert!(response.degraded_codes.is_empty());
    }

    #[test]
    fn dispatch_context_without_workspace_returns_method_unauthorized() {
        let request = DaemonRequest::new(
            "req-ctx-no-workspace-001",
            TEST_AGENT_ID,
            METHOD_CONTEXT,
            serde_json::json!({"task": "ship daemon"}),
        );
        let response = dispatch(&request);
        assert!(response.result.is_none());
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_METHOD_UNAUTHORIZED_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&DAEMON_METHOD_UNAUTHORIZED_CODE.to_owned())
        );
    }

    #[test]
    fn dispatch_context_blank_workspace_returns_method_unauthorized() {
        let mut request = DaemonRequest::new(
            "req-ctx-blank-workspace-001",
            TEST_AGENT_ID,
            METHOD_CONTEXT,
            serde_json::json!({"task": "ship daemon"}),
        );
        request.workspace_id = Some("  ".to_owned());
        let response = dispatch(&request);
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_METHOD_UNAUTHORIZED_CODE);
    }

    #[test]
    fn dispatch_context_workspace_mismatch_returns_method_unauthorized() {
        let mut request = context_request(
            "req-ctx-wrong-workspace-001",
            TEST_AGENT_ID,
            serde_json::json!({"task": "ship daemon"}),
        );
        request.workspace_id = Some("workspace-other".to_owned());
        let shutdown = AtomicBool::new(false);
        let response = dispatch_with_echo_policy_and_workspace(
            &request,
            false,
            Some(TEST_WORKSPACE_ID),
            &shutdown,
            None,
        );
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_METHOD_UNAUTHORIZED_CODE);
    }

    #[test]
    fn daemon_method_authority_classifies_seed_methods() {
        assert_eq!(
            daemon_method_authority(METHOD_CAPABILITIES),
            Some(DaemonAuthority::SameUid)
        );
        assert_eq!(
            daemon_method_authority(METHOD_ECHO),
            Some(DaemonAuthority::SameUid)
        );
        assert_eq!(
            daemon_method_authority(METHOD_SHUTDOWN),
            Some(DaemonAuthority::SameUid)
        );
        assert_eq!(
            daemon_method_authority(METHOD_CONTEXT),
            Some(DaemonAuthority::SameUidWorkspace)
        );
        assert_eq!(daemon_method_authority("ee.daemon.nope"), None);
    }

    #[test]
    fn dispatch_capabilities_advertises_strict_v1_migration_contract() {
        let request = DaemonRequest::new(
            "req-capabilities-001",
            TEST_AGENT_ID,
            METHOD_CAPABILITIES,
            serde_json::json!({}),
        );
        let response = dispatch(&request);
        assert!(response.error.is_none());
        assert!(response.degraded_codes.is_empty());

        let result = response
            .result
            .as_ref()
            .expect("capabilities returns result");
        assert_eq!(
            result.get("protocol").and_then(serde_json::Value::as_str),
            Some("ee.daemon")
        );
        assert_eq!(
            result.get("request_schemas"),
            Some(&serde_json::json!([super::super::DAEMON_REQUEST_SCHEMA_V1]))
        );
        assert_eq!(
            result.get("response_schemas"),
            Some(&serde_json::json!([
                super::super::DAEMON_RESPONSE_SCHEMA_V1
            ]))
        );
        assert_eq!(
            result.get("methods"),
            Some(&serde_json::json!([
                METHOD_CAPABILITIES,
                METHOD_CONTEXT,
                METHOD_ECHO,
                METHOD_SHUTDOWN,
                METHOD_TELEMETRY,
                METHOD_WRITE,
                METHOD_WRITE_JOURNAL
            ]))
        );
        assert_eq!(
            result
                .pointer("/authorization/ee.daemon.telemetry")
                .and_then(serde_json::Value::as_str),
            Some(DaemonAuthority::SameUid.as_wire_label())
        );
        assert_eq!(
            result
                .pointer("/authorization/ee.daemon.write_journal")
                .and_then(serde_json::Value::as_str),
            Some(DaemonAuthority::SameUidWorkspace.as_wire_label())
        );
        assert_eq!(
            result
                .pointer("/authorization/ee.daemon.write")
                .and_then(serde_json::Value::as_str),
            Some(DaemonAuthority::SameUidWorkspace.as_wire_label())
        );
        assert_eq!(
            result
                .pointer("/forward_compat/v1_unknown_fields")
                .and_then(serde_json::Value::as_str),
            Some("rejected")
        );
        assert_eq!(
            result
                .pointer("/forward_compat/v1_unknown_methods")
                .and_then(serde_json::Value::as_str),
            Some(DAEMON_UNKNOWN_METHOD_CODE)
        );
        assert!(
            result
                .pointer("/forward_compat/v2_migration")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|policy| policy.contains(METHOD_CAPABILITIES))
        );
        assert_eq!(
            result
                .pointer("/authorization/ee.daemon.context")
                .and_then(serde_json::Value::as_str),
            Some(DaemonAuthority::SameUidWorkspace.as_wire_label())
        );
        assert_eq!(
            result
                .pointer("/authorization/ee.daemon.shutdown")
                .and_then(serde_json::Value::as_str),
            Some(DaemonAuthority::SameUid.as_wire_label())
        );
    }

    #[test]
    fn dispatch_telemetry_returns_contention_report() {
        let request = DaemonRequest::new(
            "req-telemetry-001",
            TEST_AGENT_ID,
            METHOD_TELEMETRY,
            serde_json::json!({}),
        );
        let response = dispatch(&request);
        assert!(
            response.error.is_none(),
            "telemetry dispatch should succeed: {:?}",
            response.error
        );
        let result = response.result.as_ref().expect("telemetry returns result");
        assert_eq!(
            result.get("schemaTag").and_then(serde_json::Value::as_str),
            Some(crate::models::contention::CONTENTION_DIAG_SCHEMA_V1),
            "telemetry result is an ee.diag.contention.v1 report"
        );
        // The report always carries an overall posture, and the two
        // process-global sources the daemon snapshots (group-commit,
        // singleflight) are present rather than listed as unavailable.
        assert!(
            result.get("overallPosture").is_some(),
            "report carries an overall posture"
        );
        assert!(
            result.get("singleflight").is_some(),
            "singleflight posture is snapshotted in-process"
        );
        assert!(
            result.get("groupCommit").is_some(),
            "group-commit telemetry is snapshotted in-process"
        );
    }

    #[test]
    fn telemetry_method_is_same_uid_authorized() {
        assert_eq!(
            daemon_method_authority(METHOD_TELEMETRY),
            Some(DaemonAuthority::SameUid)
        );
    }

    #[test]
    fn daemon_write_params_from_value_parses_remember_inputs() {
        let value = serde_json::json!({
            "workspacePath": "/tmp/ws",
            "content": "remember this",
            "tags": "a,b",
            "confidence": 0.5,
        });
        let params = DaemonWriteParams::from_value(&value).expect("valid write params");
        assert_eq!(params.workspace_path, std::path::PathBuf::from("/tmp/ws"));
        assert_eq!(params.content, "remember this");
        assert_eq!(params.level, "episodic", "level defaults to episodic");
        assert_eq!(params.kind, "fact", "kind defaults to fact");
        assert_eq!(params.tags.as_deref(), Some("a,b"));
        assert!((params.confidence - 0.5).abs() < f32::EPSILON);
        assert!(params.auto_link, "auto_link defaults true for ee-remember parity");
        assert!(params.propose_candidates, "propose_candidates defaults true");
        // options() borrows the owned strings without panicking.
        assert_eq!(params.options().content, "remember this");
    }

    #[test]
    fn daemon_write_params_require_content_and_workspace() {
        let missing_content = serde_json::json!({ "workspacePath": "/tmp/ws" });
        assert!(DaemonWriteParams::from_value(&missing_content).is_err());
        let missing_workspace = serde_json::json!({ "content": "x" });
        assert!(DaemonWriteParams::from_value(&missing_workspace).is_err());
        let not_object = serde_json::json!("nope");
        assert!(DaemonWriteParams::from_value(&not_object).is_err());
    }

    #[test]
    fn write_method_is_same_uid_workspace_authorized() {
        assert_eq!(
            daemon_method_authority(METHOD_WRITE),
            Some(DaemonAuthority::SameUidWorkspace)
        );
    }

    #[test]
    fn write_journal_method_is_same_uid_workspace_authorized() {
        assert_eq!(
            daemon_method_authority(METHOD_WRITE_JOURNAL),
            Some(DaemonAuthority::SameUidWorkspace)
        );
    }

    #[test]
    fn dispatch_journal_rejects_invalid_params() {
        // No router (unbound) + bad params -> params-invalid RPC error, before
        // any DB work. Exercises the journal dispatch path without a workspace.
        let mut request = DaemonRequest::new(
            "req-journal-bad",
            TEST_AGENT_ID,
            METHOD_WRITE_JOURNAL,
            serde_json::json!({ "not": "journal params" }),
        );
        request.workspace_id = Some("journal-ws".to_string());
        let response = dispatch_journal(&request, None);
        let error = response.error.as_ref().expect("invalid params -> error");
        assert_eq!(error.code, DAEMON_JOURNAL_PARAMS_INVALID_CODE);
    }

    #[test]
    fn write_actor_hosting_round_trips_a_result_without_deadlock() {
        // Behavioral proof of the Inc 2 runtime hosting (bd-wx6ou.3): build the
        // same current_thread runtime + WriteOwner actor `start_server` hosts,
        // submit an op, and block_on the WriteResult from a different thread.
        // Uses an UNSUPPORTED Custom op so `execute_write_operation` returns
        // Failed immediately (no `remember_memory`, so no embedding hang in the
        // worker sandbox) — this still exercises spawn → submit → process_batch
        // → oneshot send → block_on(recv), which is where a runtime-lifecycle
        // deadlock would surface.
        use crate::core::write_owner::{
            DEFAULT_CHANNEL_CAPACITY, WriteHotPathConfig, WriteOperation, WriteOwner, WriteResult,
        };
        let runtime = crate::core::build_cli_runtime().expect("build runtime");
        let (owner, handle) = WriteOwner::new(DEFAULT_CHANNEL_CAPACITY);
        let task = runtime
            .handle()
            .try_spawn(async move {
                let Some(cx) = asupersync::Cx::current() else {
                    return;
                };
                let _ = owner
                    .run_group_commit(&cx, WriteHotPathConfig::default(), |ops| {
                        Ok(ops.iter().map(execute_write_operation).collect::<Vec<_>>())
                    })
                    .await;
            })
            .expect("spawn write actor");

        let operation = WriteOperation::Custom {
            operation_type: "ee.daemon.unsupported".to_string(),
            payload: serde_json::json!({}),
        };
        let mut receiver = handle.try_submit(operation).expect("submit accepted");
        let result = runtime.block_on(async {
            let cx = asupersync::Cx::current().expect("block_on installs an ambient Cx");
            receiver.recv(&cx).await
        });
        assert!(
            matches!(result, Ok(WriteResult::Failed { .. })),
            "an unsupported op round-trips as a Failed result"
        );

        // Clean shutdown: dropping the handle closes the mpsc so the actor's
        // recv returns Disconnected and the task completes; the join must not
        // hang.
        drop(handle);
        let _ = runtime.block_on(task);
    }

    #[test]
    fn dispatch_shutdown_sets_shutdown_latch() {
        let request = DaemonRequest::new(
            "req-shutdown-001",
            TEST_AGENT_ID,
            METHOD_SHUTDOWN,
            serde_json::json!({}),
        );
        let shutdown = AtomicBool::new(false);
        let response = dispatch_with_echo_policy_and_workspace(&request, false, None, &shutdown, None);

        assert!(response.error.is_none());
        assert!(shutdown.load(Ordering::SeqCst));
        assert_eq!(
            response
                .result
                .as_ref()
                .and_then(|value| value.pointer("/schema"))
                .and_then(serde_json::Value::as_str),
            Some("ee.daemon.shutdown.v1")
        );
        assert_eq!(
            response
                .result
                .as_ref()
                .and_then(|value| value.pointer("/accepted"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
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
    fn frame_transport_failures_close_without_decode_envelope() {
        assert!(frame_error_closes_without_response(&FrameReadError::Eof));
        assert!(frame_error_closes_without_response(
            &FrameReadError::Truncated {
                expected: 9,
                got: 2,
            }
        ));
        assert!(frame_error_closes_without_response(&FrameReadError::Io(
            io::Error::new(io::ErrorKind::TimedOut, "read timeout")
        )));
        assert!(frame_error_closes_without_response(&FrameReadError::Io(
            io::Error::new(io::ErrorKind::WouldBlock, "timeout on unix")
        )));
        assert!(frame_error_closes_without_response(&FrameReadError::Io(
            io::Error::new(io::ErrorKind::ConnectionReset, "peer reset")
        )));
        assert!(frame_error_closes_without_response(&FrameReadError::Io(
            io::Error::new(io::ErrorKind::UnexpectedEof, "short read")
        )));

        assert!(!frame_error_closes_without_response(
            &FrameReadError::TooLarge {
                announced: 9,
                max: 4,
            }
        ));
        assert!(!frame_error_closes_without_response(&FrameReadError::Io(
            io::Error::other("non-timeout read failure")
        )));
        let decode_err = serde_json::from_str::<DaemonRequest>("{").unwrap_err();
        assert!(!frame_error_closes_without_response(
            &FrameReadError::Decode(decode_err)
        ));
    }

    #[test]
    fn accept_loop_terminated_io_error_kind_labels_are_stable() {
        assert_eq!(
            io_error_kind_label(io::ErrorKind::Interrupted),
            "interrupted"
        );
        assert_eq!(io_error_kind_label(io::ErrorKind::AddrInUse), "addr_in_use");
        assert_eq!(io_error_kind_label(io::ErrorKind::TimedOut), "timed_out");
        assert_eq!(
            io_error_kind_label(io::ErrorKind::UnexpectedEof),
            "unexpected_eof"
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

    #[test]
    fn daemon_cancellation_drains_in_flight_workers_before_shutdown_reports_idle() {
        let pool = InflightPool::new(1);
        let permit = pool.try_acquire().expect("first permit must acquire");

        assert!(
            !pool.wait_until_idle(Duration::from_millis(1)),
            "pool with a held permit must not report idle"
        );

        drop(permit);

        assert!(
            pool.wait_until_idle(Duration::from_millis(50)),
            "pool must report idle after the last permit drops"
        );
    }

    #[test]
    fn daemon_shutdown_timeout_can_be_retried_after_workers_drain() {
        let pool = InflightPool::new(1);
        let permit = pool.try_acquire().expect("first permit must acquire");
        let mut handle = DaemonServerHandle {
            socket_path: std::env::temp_dir().join(format!(
                "ee-daemon-drain-test-{}-{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            )),
            shutdown: Arc::new(AtomicBool::new(false)),
            pool,
            accept_thread: None,
            scheduler_thread: None,
            shutdown_done: AtomicBool::new(false),
            workers_drained: AtomicBool::new(false),
            write_handle: None,
            write_owner_task: None,
            write_runtime: None,
        };

        let error = handle
            .shutdown_with_worker_drain_timeout(Duration::from_millis(1))
            .expect_err("held worker permit must make shutdown time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            handle.shutdown_done.load(Ordering::Acquire),
            "listener/socket teardown should not be retried after the first shutdown attempt"
        );
        assert!(
            !handle.workers_drained.load(Ordering::Acquire),
            "timed-out worker drain must not be recorded as success"
        );

        drop(permit);

        handle
            .shutdown_with_worker_drain_timeout(Duration::from_millis(50))
            .expect("shutdown retry should succeed once workers drain");
        assert!(
            handle.workers_drained.load(Ordering::Acquire),
            "successful retry must latch drained worker state"
        );
    }

    #[test]
    fn daemon_scheduler_join_timeout_can_be_retried_after_busy_task_exits() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let join = thread::spawn(move || {
            entered_tx.send(()).expect("notify scheduler entered");
            release_rx.recv().expect("wait for busy scheduler release");
            let _ = done_tx.send(SchedulerThreadExit::Returned);
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("scheduler test thread must enter busy section");

        let mut handle = DaemonServerHandle {
            socket_path: std::env::temp_dir().join(format!(
                "ee-daemon-scheduler-timeout-test-{}-{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            )),
            shutdown: Arc::new(AtomicBool::new(false)),
            pool: InflightPool::new(1),
            accept_thread: None,
            scheduler_thread: Some(SchedulerThreadHandle { join, done_rx }),
            shutdown_done: AtomicBool::new(false),
            workers_drained: AtomicBool::new(false),
            write_handle: None,
            write_owner_task: None,
            write_runtime: None,
        };

        let started = Instant::now();
        let error = handle
            .shutdown_with_worker_drain_timeout(Duration::from_millis(20))
            .expect_err("busy scheduler must make shutdown return a bounded timeout");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < DAEMON_SCHEDULER_JOIN_TIMEOUT + Duration::from_millis(500),
            "scheduler shutdown must be bounded; elapsed {:?}",
            started.elapsed()
        );
        assert!(
            handle.scheduler_thread.is_some(),
            "timed-out scheduler join must remain retryable"
        );

        release_tx.send(()).expect("release busy scheduler");
        handle
            .shutdown_with_worker_drain_timeout(Duration::from_millis(50))
            .expect("shutdown retry must join scheduler after busy task exits");
        assert!(
            handle.scheduler_thread.is_none(),
            "successful retry must consume scheduler join handle"
        );
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

    fn private_tempdir() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))
            .expect("make tempdir private");
        temp
    }

    #[test]
    fn start_server_then_echo_is_disabled_by_default() {
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-test.sock");
        let mut handle = start_server(&socket_path).expect("server must start");

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
        let temp = private_tempdir();
        let path = temp.path().join("not-a-socket");
        fs::write(&path, b"i am a regular file").expect("write");
        let error = start_server(&path).expect_err("must refuse non-socket");
        assert!(matches!(error, DaemonStartError::SocketPathOccupied { .. }));
        // The regular file must still exist after the refused start;
        // the daemon must not silently overwrite arbitrary paths.
        assert!(path.exists());
    }

    #[test]
    fn start_server_refuses_group_or_world_accessible_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("shared-parent");
        fs::create_dir(&parent).expect("create shared parent");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755))
            .expect("make parent group/other accessible");
        let socket_path = parent.join("ee-daemon.sock");

        let error = start_server(&socket_path).expect_err("must refuse unsafe parent");

        match error {
            DaemonStartError::InsecureSocketParent { path, reason } => {
                assert_eq!(path, parent);
                assert!(
                    reason.contains("group or other access"),
                    "unsafe-mode reason should name group/other access, got {reason}"
                );
            }
            other => panic!("expected InsecureSocketParent, got {other:?}"),
        }
    }

    #[test]
    fn start_server_refuses_symlink_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real_parent = temp.path().join("real-parent");
        let symlink_parent = temp.path().join("symlink-parent");
        fs::create_dir(&real_parent).expect("create real parent");
        std::os::unix::fs::symlink(&real_parent, &symlink_parent).expect("create parent symlink");
        let socket_path = symlink_parent.join("ee-daemon.sock");

        let error = start_server(&socket_path).expect_err("must refuse symlink parent");

        match error {
            DaemonStartError::InsecureSocketParent { path, reason } => {
                assert_eq!(path, symlink_parent);
                assert!(
                    reason.contains("not a real directory"),
                    "symlink-parent reason should name real-directory requirement, got {reason}"
                );
            }
            other => panic!("expected InsecureSocketParent, got {other:?}"),
        }
    }

    #[test]
    fn start_server_refuses_live_existing_socket() {
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-live.sock");
        let mut first = start_server(&socket_path).expect("first server must start");

        let error = start_server(&socket_path).expect_err("second start must refuse live socket");
        assert!(
            matches!(error, DaemonStartError::AlreadyRunning { .. }),
            "second start against a live daemon must return AlreadyRunning; got {error:?}",
        );

        let request = context_request(
            "req-live-existing-001",
            TEST_AGENT_ID,
            serde_json::json!({"still": "first"}),
        );
        let response =
            client_round_trip(first.socket_path(), &request).expect("first daemon remains live");
        assert_eq!(response.request_id, "req-live-existing-001");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        let error = response.error.as_ref().expect("context params error");
        assert_eq!(error.code, DAEMON_CONTEXT_PARAMS_INVALID_CODE);

        first.shutdown().expect("shutdown");
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
        let temp = private_tempdir();
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
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-peer.sock");
        let mut handle = start_server(&socket_path).expect("server must start");

        let request = context_request(
            "req-peer-001",
            TEST_AGENT_ID,
            serde_json::json!({"peer": "self"}),
        );
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.request_id, "req-peer-001");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        let error = response
            .error
            .as_ref()
            .expect("same-UID peer reaches dispatch");
        assert_eq!(error.code, DAEMON_CONTEXT_PARAMS_INVALID_CODE);

        handle.shutdown().expect("shutdown");
    }

    #[test]
    fn start_server_for_workspace_rejects_same_uid_context_for_wrong_workspace() {
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-workspace-auth.sock");
        let mut handle = start_server_for_workspace(&socket_path, TEST_WORKSPACE_ID)
            .expect("workspace-bound server must start");

        let mut request = context_request(
            "req-peer-wrong-workspace-001",
            TEST_AGENT_ID,
            serde_json::json!({"peer": "self"}),
        );
        request.workspace_id = Some("workspace-other".to_owned());
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.request_id, "req-peer-wrong-workspace-001");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        let error = response
            .error
            .as_ref()
            .expect("wrong workspace reaches method authorization");
        assert_eq!(error.code, DAEMON_METHOD_UNAUTHORIZED_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&DAEMON_METHOD_UNAUTHORIZED_CODE.to_owned())
        );

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

        let temp = private_tempdir();
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
        let request = context_request(
            "req-stale-001",
            TEST_AGENT_ID,
            serde_json::json!({"ping": 1}),
        );
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        let error = response.error.as_ref().expect("fresh socket dispatches");
        assert_eq!(error.code, DAEMON_CONTEXT_PARAMS_INVALID_CODE);

        handle.shutdown().expect("shutdown");
    }

    /// bd-3ik2d + bd-14dmn: two concurrent `start_server` calls on the
    /// same path must never leave the canonical path in a corrupt state
    /// or split-brain two live daemons. The publish lock serializes the
    /// inspect → bind-temp → chmod → rename window, then the follower
    /// probes the now-live canonical socket and refuses with
    /// `AlreadyRunning`.
    #[test]
    fn start_server_concurrent_binds_no_toctou() {
        use std::os::unix::fs::FileTypeExt;
        use std::sync::Barrier;

        let temp = private_tempdir();
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

        // Exactly one daemon owns the canonical socket. The follower
        // must not replace it and create a split-brain listener.
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        let already_running_count = results
            .iter()
            .filter(|result| matches!(result, Err(DaemonStartError::AlreadyRunning { .. })))
            .count();
        assert_eq!(ok_count, 1, "exactly one concurrent daemon start must win");
        assert_eq!(
            already_running_count, 1,
            "exactly one concurrent daemon start must refuse the live winner",
        );

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

        let mut handle = results
            .into_iter()
            .find_map(Result::ok)
            .expect("one daemon start must succeed");
        handle.shutdown().expect("shutdown winning daemon");
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

        let error = SocketBroker::new(path.clone())
            .remove_owned_socket_file()
            .expect_err("regular file must be refused");

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

        SocketBroker::new(socket_path.clone())
            .remove_owned_socket_file()
            .expect("owned socket cleanup must succeed");
        assert!(
            !socket_path.exists(),
            "owned socket file must be removed by guarded cleanup"
        );

        SocketBroker::new(socket_path.clone())
            .remove_owned_socket_file()
            .expect("absent path is already clean");
    }

    // ------------------------------------------------------------------
    // bd-2yg7d.3: adversarial SocketBroker lifecycle fixtures (ADR 0055).
    // These pin the broker's fail-closed publish/cleanup invariants
    // directly at the SocketBroker API so a refactor cannot silently
    // reopen a prior P0/P1 class without a named test failing.
    // ------------------------------------------------------------------

    /// Sample the canonical socket path in a tight loop until `stop`
    /// flips, recording every observation that violates the ADR 0055
    /// publish invariant: once the canonical path exists it must be a
    /// socket with mode 0o600. The chmod runs on the temp path BEFORE
    /// the atomic `rename(2)`, so correct code can never expose an
    /// insecure intermediate state at the canonical name — any recorded
    /// violation means the temp-bind + chmod + rename mechanism
    /// regressed. `allow_missing` covers fresh publishes where the
    /// canonical path legitimately does not exist until the rename
    /// lands; stale replacement must never let the path go missing.
    fn spawn_canonical_path_invariant_watcher(
        path: PathBuf,
        allow_missing: bool,
        stop: Arc<AtomicBool>,
    ) -> JoinHandle<(u64, Vec<String>)> {
        use std::os::unix::fs::FileTypeExt;

        thread::spawn(move || {
            let mut samples = 0_u64;
            let mut violations = Vec::new();
            while !stop.load(Ordering::Acquire) {
                samples += 1;
                let observed = match fs::symlink_metadata(&path) {
                    Ok(metadata) => {
                        if metadata.file_type().is_socket() {
                            let mode = metadata.permissions().mode() & 0o777;
                            (mode != 0o600).then(|| {
                                format!(
                                    "sample {samples}: canonical socket observed with mode 0o{mode:o}"
                                )
                            })
                        } else {
                            Some(format!(
                                "sample {samples}: canonical path exists as a non-socket"
                            ))
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (!allow_missing)
                        .then(|| format!("sample {samples}: canonical path observed missing")),
                    Err(error) => Some(format!("sample {samples}: stat failed: {error}")),
                };
                if let Some(violation) = observed
                    && violations.len() < 16
                {
                    violations.push(violation);
                }
                thread::yield_now();
            }
            (samples, violations)
        })
    }

    /// Accept the single pending connection on `listener` without
    /// risking an unbounded blocking `accept()` if the publish
    /// mechanism regressed. UDS `connect(2)` only succeeds once the
    /// connection is queued on the listener, so the bounded retry is a
    /// formality — `WouldBlock` persisting for the full deadline means
    /// the canonical path no longer routes to this listener.
    fn accept_pending_connection(listener: &UnixListener) -> UnixStream {
        listener
            .set_nonblocking(true)
            .expect("listener nonblocking");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => return stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "the published listener must receive the connection made against the \
                         canonical socket path (ADR 0055: the atomic rename must publish THIS \
                         listener, not leave a different socket at the canonical name)",
                    );
                    thread::yield_now();
                }
                Err(error) => panic!("accept on published listener failed: {error}"),
            }
        }
    }

    /// bd-2yg7d.3 (ADR 0055: never publish in a shared parent). The
    /// broker must refuse every parent mode that grants group or other
    /// access — including a /tmp-style 0o1777 sticky directory — and
    /// the refusal must fire BEFORE the lock file or temp socket is
    /// created, so the hostile directory never receives an ee artifact
    /// another uid could tamper with.
    #[test]
    fn socket_broker_publish_refuses_shared_parent_without_artifacts() {
        for mode in [0o777_u32, 0o1777, 0o770, 0o707, 0o750] {
            let temp = tempfile::tempdir().expect("tempdir");
            let parent = temp.path().join("shared-parent");
            fs::create_dir(&parent).expect("create shared parent");
            fs::set_permissions(&parent, fs::Permissions::from_mode(mode))
                .expect("loosen parent mode");

            let broker = SocketBroker::new(parent.join("ee-daemon.sock"));
            let error = broker.publish_listener().expect_err(
                "SocketBroker must refuse to publish into a group/other-accessible parent \
                 (ADR 0055 invariant: never publish in a shared parent)",
            );
            match error {
                DaemonStartError::InsecureSocketParent { path, reason } => {
                    assert_eq!(
                        path, parent,
                        "insecure-parent refusal must name the offending parent directory",
                    );
                    assert!(
                        reason.contains("group or other access"),
                        "refusal reason for parent mode 0o{mode:o} must name the group/other \
                         access grant; got {reason}",
                    );
                }
                other => panic!(
                    "parent mode 0o{mode:o} must be refused as InsecureSocketParent (ADR 0055: \
                     never publish in a shared parent); got {other:?}"
                ),
            }

            let leftovers: Vec<PathBuf> = fs::read_dir(&parent)
                .expect("read refused parent")
                .map(|entry| entry.expect("dir entry").path())
                .collect();
            assert!(
                leftovers.is_empty(),
                "refusing an insecure parent (mode 0o{mode:o}) must not leave lock/temp \
                 artifacts inside it; found {leftovers:?}",
            );
        }
    }

    /// bd-2yg7d.3 (ADR 0055: never overwrite a non-socket path). A
    /// regular file or directory squatting the canonical socket path
    /// must be refused as `SocketPathOccupied` and left untouched —
    /// publish must never repossess a path the operator pointed at
    /// real data.
    #[test]
    fn socket_broker_publish_refuses_non_socket_canonical_path() {
        // Regular file at the canonical path.
        {
            let temp = private_tempdir();
            let socket_path = temp.path().join("ee-daemon-occupied.sock");
            fs::write(&socket_path, b"operator data").expect("write squatting file");

            let error = SocketBroker::new(socket_path.clone())
                .publish_listener()
                .expect_err(
                    "a regular file at the canonical path must refuse publish (ADR 0055: never \
                     overwrite a non-socket path)",
                );
            match error {
                DaemonStartError::SocketPathOccupied { path } => assert_eq!(
                    path, socket_path,
                    "regular-file refusal must name the canonical path",
                ),
                other => panic!(
                    "a regular file at the canonical path must be SocketPathOccupied (ADR 0055: \
                     never overwrite a non-socket path); got {other:?}"
                ),
            }
            assert_eq!(
                fs::read(&socket_path).expect("squatting file must survive"),
                b"operator data",
                "refused publish must leave the squatting regular file byte-identical",
            );
        }

        // Directory at the canonical path.
        {
            let temp = private_tempdir();
            let socket_path = temp.path().join("ee-daemon-occupied-dir.sock");
            fs::create_dir(&socket_path).expect("create squatting directory");

            let error = SocketBroker::new(socket_path.clone())
                .publish_listener()
                .expect_err(
                    "a directory at the canonical path must refuse publish (ADR 0055: never \
                     overwrite a non-socket path)",
                );
            match error {
                DaemonStartError::SocketPathOccupied { path } => assert_eq!(
                    path, socket_path,
                    "directory refusal must name the canonical path",
                ),
                other => panic!(
                    "a directory at the canonical path must be SocketPathOccupied (ADR 0055: \
                     never overwrite a non-socket path); got {other:?}"
                ),
            }
            assert!(
                socket_path.is_dir(),
                "refused publish must leave the squatting directory in place",
            );
        }
    }

    /// bd-2yg7d.3 (ADR 0055: never overwrite a non-socket path). The
    /// occupancy check must classify the canonical path with
    /// `symlink_metadata` — the link itself, never its target — so a
    /// planted symlink cannot launder a "this is my stale socket"
    /// belief through whatever it points at.
    #[test]
    fn socket_broker_publish_refuses_symlink_at_canonical_path() {
        let temp = private_tempdir();
        let target = temp.path().join("symlink-target");
        fs::write(&target, b"victim bytes").expect("write symlink target");
        let socket_path = temp.path().join("ee-daemon-link.sock");
        std::os::unix::fs::symlink(&target, &socket_path).expect("plant symlink at canonical path");

        let error = SocketBroker::new(socket_path.clone())
            .publish_listener()
            .expect_err(
                "a symlink at the canonical path must refuse publish without following it \
                 (ADR 0055: never overwrite a non-socket path)",
            );
        assert!(
            matches!(error, DaemonStartError::SocketPathOccupied { .. }),
            "symlink refusal must be SocketPathOccupied (classified via symlink_metadata, never \
             the target); got {error:?}",
        );
        assert!(
            fs::symlink_metadata(&socket_path)
                .expect("symlink must survive refused publish")
                .file_type()
                .is_symlink(),
            "the planted symlink must remain at the canonical path after refusal",
        );
        assert_eq!(
            fs::read(&target).expect("symlink target must survive"),
            b"victim bytes",
            "refused publish must not write through or replace the symlink target",
        );
    }

    /// bd-2yg7d.3 (ADR 0055: stale socket replacement is temp-bind +
    /// atomic rename). Publishing over a dead socket left by a crashed
    /// daemon must succeed, must route the canonical path to the NEW
    /// listener, must consume the temp-bind artifact, and — because the
    /// replacement is a single `rename(2)` — must never expose a window
    /// where the canonical path is missing, a non-socket, or
    /// group/other-accessible.
    #[test]
    fn socket_broker_replaces_stale_socket_with_no_observable_gap() {
        use std::os::unix::fs::FileTypeExt;

        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-stale-gap.sock");
        {
            let stale = UnixListener::bind(&socket_path).expect("stale bind");
            // Pin the stale socket to 0o600 so every watcher sample has
            // exactly one expectation: an existing 0o600 socket, before,
            // during, and after the replacement.
            fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
                .expect("chmod stale socket");
            drop(stale);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let watcher =
            spawn_canonical_path_invariant_watcher(socket_path.clone(), false, Arc::clone(&stop));

        let broker = SocketBroker::new(socket_path.clone());
        let (listener, _publish_lock) = broker.publish_listener().expect(
            "publish over a dead stale socket must succeed via temp-bind + atomic rename \
             (ADR 0055 stale replacement)",
        );

        stop.store(true, Ordering::Release);
        let (samples, violations) = watcher.join().expect("watcher thread must not panic");
        assert!(
            samples > 0,
            "watcher must observe the canonical path at least once"
        );
        assert!(
            violations.is_empty(),
            "stale replacement must be atomic: no sample may show the canonical path missing, \
             non-socket, or insecure (ADR 0055 temp-bind + rename); observed {violations:?}",
        );

        // The canonical path must now route to the fresh listener; the
        // stale socket was dead, so a successful connect + accept proves
        // the rename published THIS listener.
        let _client = UnixStream::connect(&socket_path)
            .expect("replaced canonical socket must accept connections");
        let _server_side = accept_pending_connection(&listener);

        let metadata = fs::symlink_metadata(&socket_path).expect("published socket metadata");
        assert!(
            metadata.file_type().is_socket(),
            "canonical path must be a socket after stale replacement",
        );
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "replaced socket must be mode 0o600 (chmod-on-temp survives the rename; ADR 0055)",
        );

        let residue: Vec<String> = fs::read_dir(temp.path())
            .expect("read socket parent")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.contains(".tmp."))
            .collect();
        assert!(
            residue.is_empty(),
            "the temp-bind artifact must be consumed by the atomic rename, leaving no residue \
             next to the canonical socket (ADR 0055); found {residue:?}",
        );
    }

    /// bd-2yg7d.3 (ADR 0055: chmod-before-publish). On a fresh path the
    /// socket must already carry mode 0o600 by the time the canonical
    /// name exists at all: `UnixListener::bind` honours the umask
    /// (typically yielding 0o755), so a refactor that renamed first and
    /// chmodded second would expose a world-connectable socket at the
    /// canonical path. The watcher pins that no sample ever sees the
    /// canonical path as anything but absent or a 0o600 socket; the
    /// fixture lives in a fresh private tempdir, never global /tmp.
    #[test]
    fn socket_broker_fresh_publish_chmods_socket_before_canonical_publish() {
        use std::os::unix::fs::FileTypeExt;

        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-fresh-chmod.sock");

        let stop = Arc::new(AtomicBool::new(false));
        let watcher =
            spawn_canonical_path_invariant_watcher(socket_path.clone(), true, Arc::clone(&stop));

        let broker = SocketBroker::new(socket_path.clone());
        let (listener, _publish_lock) = broker
            .publish_listener()
            .expect("fresh publish in a private parent must succeed");

        // The instant publish_listener returns, the canonical path is
        // connectable — so it must ALREADY be 0o600.
        let metadata = fs::symlink_metadata(&socket_path).expect("published socket metadata");
        assert!(
            metadata.file_type().is_socket(),
            "canonical path must be a socket once published",
        );
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "the socket must receive mode 0o600 BEFORE it becomes connectable at the canonical \
             path (ADR 0055 chmod-before-publish; bd-3j0td)",
        );
        let _client = UnixStream::connect(&socket_path)
            .expect("published socket must be connectable by the owning uid");
        let _server_side = accept_pending_connection(&listener);

        stop.store(true, Ordering::Release);
        let (samples, violations) = watcher.join().expect("watcher thread must not panic");
        assert!(
            samples > 0,
            "watcher must sample the canonical path at least once"
        );
        assert!(
            violations.is_empty(),
            "during fresh publish the canonical path may only ever be observed absent or as a \
             0o600 socket — the chmod must precede the atomic rename (ADR 0055 \
             chmod-before-publish); observed {violations:?}",
        );
    }

    /// bd-2yg7d.3 (ADR 0055: temp-bind + atomic rename mechanism). The
    /// per-attempt temp path must live in the SAME parent directory as
    /// the canonical socket (same-directory `rename(2)` is what makes
    /// the publish atomic — a cross-directory temp could land on a
    /// different filesystem and would inherit a different privacy
    /// boundary), must extend the canonical file name, must embed the
    /// pid, and must be unique per attempt so concurrent publishes
    /// never collide.
    #[test]
    fn socket_broker_temp_bind_path_is_parent_local_and_unique() {
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-temp-name.sock");
        let broker = SocketBroker::new(socket_path.clone());

        let first = broker.temp_bind_path();
        let second = broker.temp_bind_path();

        assert_ne!(
            first, second,
            "temp bind paths must be unique per attempt (pid + monotonic counter) so two \
             concurrent publishes never collide and each rename is a clean atomic publish \
             (ADR 0055)",
        );
        for tmp_path in [&first, &second] {
            assert_eq!(
                tmp_path.parent(),
                socket_path.parent(),
                "temp bind path must stay inside the validated private parent of the canonical \
                 socket so the rename(2) publish is same-directory atomic (ADR 0055)",
            );
            let name = tmp_path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("temp bind file name must be valid UTF-8 in this fixture");
            assert!(
                name.starts_with("ee-daemon-temp-name.sock.tmp."),
                "temp bind name must extend the canonical socket name with a .tmp. suffix; \
                 got {name}",
            );
            assert!(
                name.contains(&format!(".tmp.{}.", std::process::id())),
                "temp bind name must embed the publishing pid for cross-process collision \
                 avoidance; got {name}",
            );
        }
    }

    /// bd-2yg7d.3 (ADR 0055: publish-lock path properties). The publish
    /// lock must be derived as `<socket>.start.lock` inside the same
    /// validated-private parent — a lock outside the 0o700 boundary
    /// could be squatted or flocked by another uid to wedge or race
    /// daemon starts — and the lock file itself must be a regular file
    /// with no group/other access, owned by the publishing euid.
    #[test]
    fn socket_broker_publish_lock_is_owner_only_sibling_file() {
        use std::os::unix::fs::MetadataExt;

        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-lock-props.sock");
        let broker = SocketBroker::new(socket_path.clone());

        let lock_path = broker.socket_publish_lock_path();
        assert_eq!(
            lock_path.parent(),
            socket_path.parent(),
            "publish lock must live inside the same validated private parent as the canonical \
             socket (ADR 0055 publish-lock properties)",
        );
        assert_eq!(
            lock_path.file_name().and_then(|name| name.to_str()),
            Some("ee-daemon-lock-props.sock.start.lock"),
            "publish lock name must be '<socket file name>.start.lock'",
        );

        let (_listener, publish_lock) = broker
            .publish_listener()
            .expect("publish in a private parent must succeed");
        let metadata = fs::symlink_metadata(&lock_path)
            .expect("publish lock file must exist after a successful publish");
        assert!(
            metadata.file_type().is_file(),
            "publish lock must be a regular file (O_NOFOLLOW open), not a socket/symlink/dir",
        );
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "publish lock mode 0o{mode:o} must grant no group/other access (created 0o600; \
             ADR 0055 publish-lock properties)",
        );
        assert_eq!(
            metadata.uid(),
            current_euid(),
            "publish lock must be owned by the publishing euid",
        );
        drop(publish_lock);
    }

    /// bd-2yg7d.3 (ADR 0055: publish-lock path properties). The lock
    /// file opens with `O_NOFOLLOW`: a symlink planted at the derived
    /// lock path must abort the publish (no socket appears) and must
    /// not open, create, or truncate whatever the link points at.
    #[test]
    fn socket_broker_publish_refuses_symlinked_lock_path() {
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-lock-link.sock");
        let broker = SocketBroker::new(socket_path.clone());

        let target = temp.path().join("lock-symlink-target");
        fs::write(&target, b"victim bytes").expect("write lock symlink target");
        std::os::unix::fs::symlink(&target, broker.socket_publish_lock_path())
            .expect("plant symlink at the publish-lock path");

        let error = broker.publish_listener().expect_err(
            "a symlinked publish-lock path must abort the publish (O_NOFOLLOW; ADR 0055 \
             publish-lock properties)",
        );
        assert!(
            matches!(error, DaemonStartError::Bind { .. }),
            "symlinked-lock refusal surfaces as the Bind error wrapping the O_NOFOLLOW open \
             failure; got {error:?}",
        );
        assert_eq!(
            fs::read(&target).expect("lock symlink target must survive"),
            b"victim bytes",
            "the publish-lock open must not write through the planted symlink",
        );
        assert!(
            !socket_path.exists(),
            "no socket may be published when the publish lock cannot be acquired safely",
        );
    }

    /// bd-2yg7d.3 (ADR 0055: never delete a non-socket during cleanup).
    /// A directory at the daemon socket path must be refused by
    /// `remove_owned_socket_file` and left on disk, exactly like the
    /// regular-file case pinned above.
    #[test]
    fn guarded_socket_unlink_refuses_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("daemon-sock-dir");
        fs::create_dir(&path).expect("create directory at socket path");

        let error = SocketBroker::new(path.clone())
            .remove_owned_socket_file()
            .expect_err(
                "cleanup must refuse a directory at the socket path (ADR 0055: never delete a \
                 non-socket during cleanup)",
            );
        assert_eq!(
            error.kind(),
            io::ErrorKind::InvalidInput,
            "non-socket cleanup refusal must be InvalidInput naming the path; got {error}",
        );
        assert!(
            path.is_dir(),
            "the directory must remain on disk after refused cleanup",
        );
    }

    /// bd-2yg7d.3 (ADR 0055: never delete a non-socket during cleanup).
    /// Cleanup classifies via `symlink_metadata`: a symlink pointing at
    /// a REAL socket is still a symlink, and removing it — or worse,
    /// following it — would let a planted link turn shutdown into an
    /// arbitrary unlink. Both the link and the socket behind it must
    /// survive.
    #[test]
    fn guarded_socket_unlink_refuses_symlink_to_socket() {
        use std::os::unix::fs::FileTypeExt;

        let temp = private_tempdir();
        let real_socket = temp.path().join("real-daemon.sock");
        let _listener = UnixListener::bind(&real_socket).expect("bind real socket");
        let link = temp.path().join("link-to-daemon.sock");
        std::os::unix::fs::symlink(&real_socket, &link).expect("plant symlink to socket");

        let error = SocketBroker::new(link.clone())
            .remove_owned_socket_file()
            .expect_err(
                "cleanup must refuse a symlink even when it points at a real socket (ADR 0055: \
                 never delete a non-socket during cleanup)",
            );
        assert_eq!(
            error.kind(),
            io::ErrorKind::InvalidInput,
            "symlink cleanup refusal must be InvalidInput; got {error}",
        );
        assert!(
            fs::symlink_metadata(&link)
                .expect("symlink must survive refused cleanup")
                .file_type()
                .is_symlink(),
            "the planted symlink must remain after refused cleanup",
        );
        assert!(
            fs::symlink_metadata(&real_socket)
                .expect("real socket must survive refused cleanup")
                .file_type()
                .is_socket(),
            "the socket behind the planted symlink must remain after refused cleanup",
        );
    }

    /// bd-2yg7d.3 (ADR 0055: never delete an other-owned file during
    /// cleanup). Creating a socket owned by a foreign uid requires
    /// euid 0, so this pin only exercises on a root runner (e.g. a
    /// container CI job) and skips gracefully elsewhere — mirroring how
    /// platform-gated coverage in this module degrades rather than
    /// asserting vacuously.
    #[test]
    fn guarded_socket_unlink_refuses_other_uid_socket_when_root() {
        if current_euid() != 0 {
            eprintln!(
                "skipping guarded_socket_unlink_refuses_other_uid_socket_when_root: \
                 chown to a foreign uid requires euid 0"
            );
            return;
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("foreign-owned.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind fixture socket");
        drop(listener);
        // uid 1 ("daemon" on Linux and macOS) is a stable foreign uid.
        std::os::unix::fs::chown(&socket_path, Some(1), None)
            .expect("chown fixture socket to a foreign uid");

        let error = SocketBroker::new(socket_path.clone())
            .remove_owned_socket_file()
            .expect_err(
                "cleanup must refuse a socket owned by another uid (ADR 0055: never delete an \
                 other-owned file during cleanup)",
            );
        assert_eq!(
            error.kind(),
            io::ErrorKind::PermissionDenied,
            "other-uid cleanup refusal must be PermissionDenied; got {error}",
        );
        assert!(
            error.to_string().contains("owned by uid"),
            "other-uid refusal must name the owning uid; got {error}",
        );
        assert!(
            socket_path.exists(),
            "the foreign-owned socket must remain on disk after refused cleanup",
        );
    }

    struct FailingConnectionWorkerSpawner;

    #[derive(Default)]
    struct CapturingDaemonMetricsCollector {
        worker_spawn_failures: Mutex<Vec<io::ErrorKind>>,
    }

    impl super::super::metrics::DaemonMetricsCollector for CapturingDaemonMetricsCollector {
        fn record_dispatch(
            &self,
            _method: &str,
            _outcome: super::super::metrics::DispatchOutcome,
            _elapsed: Duration,
        ) {
        }

        fn record_worker_spawn_failure(&self, kind: io::ErrorKind) {
            self.worker_spawn_failures
                .lock()
                .expect("metrics capture mutex must not be poisoned")
                .push(kind);
        }
    }

    impl ConnectionWorkerSpawner for FailingConnectionWorkerSpawner {
        fn spawn_connection_worker(
            &self,
            stream: UnixStream,
            shutdown: Arc<AtomicBool>,
            dispatch_policy: Arc<DaemonDispatchPolicy>,
            metrics: Arc<dyn super::super::metrics::DaemonMetricsCollector>,
            permit: InflightPermit,
        ) -> io::Result<JoinHandle<()>> {
            drop(stream);
            drop(shutdown);
            drop(dispatch_policy);
            drop(metrics);
            drop(permit);
            Err(io::Error::other("simulated pthread_create failure"))
        }
    }

    /// Regression test for bd-poxok. A real `thread::Builder::spawn`
    /// failure is host-resource dependent, so the accept loop exposes a
    /// test-only spawn seam and pins the observable contract: the client
    /// receives a framed `daemon_overloaded` response instead of a bare
    /// connection reset.
    #[test]
    fn accept_loop_spawn_failure_returns_overloaded_envelope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-spawn-fails.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind daemon socket");
        let shutdown = Arc::new(AtomicBool::new(false));
        let pool = InflightPool::new(1);
        let runner_shutdown = Arc::clone(&shutdown);
        let runner_pool = Arc::clone(&pool);
        let runner_socket_path = socket_path.clone();
        let metrics = Arc::new(CapturingDaemonMetricsCollector::default());
        let runner_metrics: Arc<dyn super::super::metrics::DaemonMetricsCollector> =
            metrics.clone();

        let runner = thread::spawn(move || {
            run_accept_loop_with_spawner(
                listener,
                runner_socket_path,
                runner_shutdown,
                runner_pool,
                Arc::new(DaemonDispatchPolicy::default()),
                runner_metrics,
                FailingConnectionWorkerSpawner,
            );
        });

        let mut client = UnixStream::connect(&socket_path).expect("client must connect");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("client read timeout");
        let response = read_framed_daemon_response(&mut client);
        let error = response.error.as_ref().expect("must carry an error");

        assert_eq!(response.request_id, "<overloaded>");
        assert_eq!(response.agent_id, "<unknown>");
        assert_eq!(error.code, DAEMON_OVERLOADED_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&DAEMON_OVERLOADED_CODE.to_owned()),
            "spawn failure envelope must surface daemon_overloaded in degraded[]",
        );

        shutdown.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&socket_path);
        runner.join().expect("accept loop thread must not panic");
        let failures = metrics
            .worker_spawn_failures
            .lock()
            .expect("metrics capture mutex must not be poisoned");
        assert_eq!(failures.as_slice(), &[io::ErrorKind::Other]);
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
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-idempotent.sock");
        let mut handle = start_server(&socket_path).expect("server must start");

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
        let (mut server_side, mut client_side) = UnixStream::pair().expect("socketpair");
        write_shutting_down_response(&mut server_side);
        drop(server_side);

        let response = read_framed_daemon_response(&mut client_side);
        let error = response.error.as_ref().expect("must carry an error");
        assert_eq!(error.code, crate::daemon::DAEMON_SHUTTING_DOWN_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&crate::daemon::DAEMON_SHUTTING_DOWN_CODE.to_owned()),
            "shutdown envelope must surface the code in degraded[]"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn handle_connection_refuses_parsed_request_after_shutdown_latch() {
        use std::io::Write;

        let (mut client_side, server_side) = UnixStream::pair().expect("socketpair");
        client_side
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("client read timeout");
        client_side
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("client write timeout");
        let shutdown = Arc::new(AtomicBool::new(true));

        let dispatch_policy = Arc::new(DaemonDispatchPolicy::for_workspace(
            "workspace-worker-shutdown",
        ));
        let metrics = Arc::new(super::super::metrics::NoopMetricsCollector);
        let worker = thread::spawn(move || {
            handle_connection(server_side, shutdown, dispatch_policy, metrics)
        });

        let mut request = DaemonRequest::new(
            "req-worker-shutdown",
            TEST_AGENT_ID,
            METHOD_CONTEXT,
            serde_json::json!({"task": "drain worker"}),
        );
        request.workspace_id = Some("workspace-worker-shutdown".to_owned());
        let body = serde_json::to_vec(&request).expect("request must encode");
        let length = u32::try_from(body.len()).expect("body length fits u32");
        client_side
            .write_all(&length.to_be_bytes())
            .expect("write length");
        client_side.write_all(&body).expect("write body");
        client_side.flush().expect("flush request");

        let response = read_framed_daemon_response(&mut client_side);
        worker.join().expect("worker thread must not panic");

        assert_eq!(response.request_id, "req-worker-shutdown");
        assert_eq!(response.agent_id, TEST_AGENT_ID);
        assert_eq!(
            response.workspace_id.as_deref(),
            Some("workspace-worker-shutdown")
        );
        let error = response.error.as_ref().expect("must carry an error");
        assert_eq!(error.code, crate::daemon::DAEMON_SHUTTING_DOWN_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&crate::daemon::DAEMON_SHUTTING_DOWN_CODE.to_owned()),
            "worker shutdown envelope must surface the code in degraded[]"
        );
    }
}
