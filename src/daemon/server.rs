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

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};

use crate::config::env_registry::{self, EnvVar};
use crate::core::context::{
    ContextPackError, ContextPackOptions, ContextPackOutputOptionOverrides,
    ContextPackOutputOptions, ContextSearchAdvisorySnapshot,
    attach_context_cached_search_advisories_for_delivery,
    attach_context_search_advisories_for_delivery, attach_pack_dna_to_context_response,
    run_context_pack_with_performance_controlled,
};
use crate::core::search::{
    PERFORMANCE_EXPLAIN_SCHEMA_V1, PERFORMANCE_FALLBACK_REDACTED_MESSAGE,
    SearchAdvisoryDeliveryReservation, SearchAdvisorySession, SearchAdvisorySettlement,
    SearchDedupMode, SearchOptions, SearchPerformanceTrace, SearchReport, SearchSourceMode,
    TypedMemoryFieldFilter, elapsed_timing_json, normalize_memory_kind_filter,
    run_search_with_performance_and_filters,
};
use crate::models::{MemoryScope, QueryFilters, RedactionLevel};
use crate::output::{ContextJsonRenderOptions, render_context_response_json_with_options};
use crate::pack::{ContextPackProfile, DEFAULT_COORDINATION_STALE_AFTER_MS, PackResourceProfile};
use crate::search::SpeedMode;

pub use super::protocol::{
    DAEMON_SEARCH_REQUEST_SCHEMA_V2, DAEMON_SEARCH_RESPONSE_SCHEMA_V3, METHOD_SEARCH,
};
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

/// Error code returned when `ee.daemon.search` params fail strict decoding.
pub const DAEMON_SEARCH_PARAMS_INVALID_CODE: &str = "daemon_search_params_invalid";

/// Error code returned when canonical search execution or response encoding fails.
pub const DAEMON_SEARCH_EXECUTION_FAILED_CODE: &str = "daemon_search_execution_failed";

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
/// database transaction boundary (Inc 3). Workspace-scoped
/// (`SameUidWorkspace`).
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
const SEARCH_ADVISORY_SETTLEMENT_RETRY_LIMIT: usize = 64;

/// Per-daemon dispatch policy that is resolved at daemon start and
/// then shared by every accepted connection. Connection-level peer
/// credentials still gate local UID; this policy gates method-specific
/// workspace authority inside dispatch. bd-3mbao.
// Eq/PartialEq were derived but unused; dropped because the optional
// `write_router` (an asupersync runtime + write handle) is not comparable.
#[derive(Clone, Debug, Default)]
pub struct DaemonDispatchPolicy {
    bound_workspace_id: Option<String>,
    /// Search advisories are emitted once per active workspace condition.
    search_advisory_session: Arc<Mutex<SearchAdvisorySession>>,
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
            ..Self::default()
        }
    }

    fn bound_workspace_id(&self) -> Option<&str> {
        self.bound_workspace_id.as_deref()
    }

    fn write_router(&self) -> Option<&DaemonWriteRouter> {
        self.write_router.as_ref()
    }

    fn search_advisory_session(&self) -> &Mutex<SearchAdvisorySession> {
        &self.search_advisory_session
    }
}

/// Own a provisional advisory emission until it is either attached to a
/// socket response or abandoned. Early returns and unwinds release the
/// reservation without consuming the once-per-active-episode advisory.
struct PendingSearchAdvisoryDelivery<'a> {
    session: &'a Mutex<SearchAdvisorySession>,
    reservation: Option<SearchAdvisoryDeliveryReservation>,
}

impl<'a> PendingSearchAdvisoryDelivery<'a> {
    fn new(session: &'a Mutex<SearchAdvisorySession>, workspace_id: &str) -> Self {
        let reservation = session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reserve_delivery(workspace_id);
        Self {
            session,
            reservation: Some(reservation),
        }
    }

    fn reservation_mut(&mut self) -> &mut SearchAdvisoryDeliveryReservation {
        self.reservation
            .as_mut()
            .expect("pending advisory delivery must retain its reservation")
    }

    fn finish(
        mut self,
        response: DaemonResponse,
        defer_until_socket_write: bool,
    ) -> DaemonResponse {
        let reservation = self
            .reservation
            .take()
            .expect("pending advisory delivery must retain its reservation");
        if !reservation.requires_settlement() {
            return response;
        }
        if defer_until_socket_write {
            return response.with_search_advisory_delivery(
                reservation.workspace_id(),
                reservation.token(),
                reservation.large_gap_capacity_busy(),
            );
        }
        settle_search_advisory_delivery(
            self.session,
            reservation.workspace_id(),
            reservation.token(),
            true,
            reservation.large_gap_capacity_busy(),
        );
        response
    }
}

impl Drop for PendingSearchAdvisoryDelivery<'_> {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        if reservation.requires_settlement() {
            settle_search_advisory_delivery(
                self.session,
                reservation.workspace_id(),
                reservation.token(),
                false,
                reservation.large_gap_capacity_busy(),
            );
        }
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
                runtime.block_on(task);
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
            Self::validate_socket_ancestor_chain(parent)?;
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
        // temp name carries a UUID v7 so concurrent and post-crash binds
        // do not reuse a predictable pathname, and each `rename` is a clean
        // atomic publish. Sentinel: bd-3ik2d atomic-rename.
        let tmp_path = self.temp_bind_path();
        self.bind_secured_temp_listener_at(&tmp_path)
    }

    fn bind_secured_temp_listener_at(
        &self,
        tmp_path: &Path,
    ) -> Result<UnixListener, DaemonStartError> {
        // Never unlink an unexpected pre-existing temp path. The randomized
        // UUID makes a collision vanishingly unlikely, while bind(2)'s
        // create-only behavior turns either a crash remnant or a planted
        // path into an explicit refusal instead of deleting somebody else's
        // file under a daemon-controlled name.

        let listener = UnixListener::bind(tmp_path).map_err(|source| DaemonStartError::Bind {
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
        if let Err(source) = fs::set_permissions(tmp_path, fs::Permissions::from_mode(0o600)) {
            // The temp socket is bound but world-open at this instant;
            // remove it before returning so a half-secured artifact does
            // not linger under the temp name.
            let _ = Self::remove_owned_socket_path(tmp_path);
            return Err(DaemonStartError::Bind {
                path: self.socket_path.clone(),
                source,
            });
        }

        // Atomically publish the secured socket at the canonical path.
        // `rename(2)` is atomic and replaces a stale socket left by a
        // prior daemon in a single step.
        if let Err(source) = fs::rename(tmp_path, &self.socket_path) {
            // Publish failed (e.g. cross-device move, or the parent dir
            // was removed underneath us). Drop the temp socket so it does
            // not linger, then surface the failure.
            let _ = Self::remove_owned_socket_path(tmp_path);
            return Err(DaemonStartError::Bind {
                path: self.socket_path.clone(),
                source,
            });
        }

        Ok(listener)
    }

    fn remove_owned_socket_file(&self) -> io::Result<()> {
        Self::remove_owned_socket_path(&self.socket_path)
    }

    fn remove_owned_socket_path(path: &Path) -> io::Result<()> {
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

    /// Construct a per-attempt temporary socket path next to `socket_path`,
    /// of the form `<socket>.tmp.<uuid-v7>`. UUID v7 combines process-local
    /// monotonicity with random bits, keeping concurrent and post-crash
    /// attempts distinct without ever deleting a colliding path.
    /// Sentinel: bd-3ik2d atomic-rename.
    fn temp_bind_path(&self) -> PathBuf {
        let suffix = format!(".tmp.{}", uuid::Uuid::now_v7().simple());
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

    fn validate_socket_ancestor_chain(parent: &Path) -> Result<(), DaemonStartError> {
        let euid = current_euid();
        let mut ancestors = parent.ancestors().collect::<Vec<_>>();
        ancestors.reverse();

        for ancestor in ancestors {
            let metadata = fs::symlink_metadata(ancestor).map_err(|source| {
                DaemonStartError::SocketDirCreate {
                    path: ancestor.to_path_buf(),
                    source,
                }
            })?;
            if metadata.file_type().is_symlink() {
                // System-owned compatibility links such as macOS `/var` ->
                // `/private/var` are stable because their containing parent
                // is checked earlier in this root-to-leaf walk. A link owned
                // by an unrelated uid is not a trustworthy socket ancestor.
                if !matches!(metadata.uid(), 0) && metadata.uid() != euid {
                    return Err(DaemonStartError::InsecureSocketParent {
                        path: ancestor.to_path_buf(),
                        reason: format!(
                            "ancestor symlink is owned by uid {}, not root or current uid {euid}",
                            metadata.uid()
                        ),
                    });
                }
                continue;
            }
            if !metadata.file_type().is_dir() {
                return Err(DaemonStartError::InsecureSocketParent {
                    path: ancestor.to_path_buf(),
                    reason: "ancestor is not a directory or trusted symlink".to_owned(),
                });
            }

            let mode = metadata.permissions().mode();
            let writable_by_other_principals = mode & 0o022 != 0;
            let sticky = mode & 0o1000 != 0;
            if writable_by_other_principals && !sticky {
                return Err(DaemonStartError::InsecureSocketParent {
                    path: ancestor.to_path_buf(),
                    reason: format!(
                        "ancestor mode 0o{:o} permits non-owner rename without sticky protection",
                        mode & 0o7777
                    ),
                });
            }
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
                    // Inc 3/5: enable group-commit so the actor accumulates a
                    // batch; transaction-coalescible daemon journal/outcome ops
                    // share one DB transaction boundary, while remember ops
                    // (which open their own connection) stay per-op.
                    let write_config = crate::core::write_owner::WriteHotPathConfig {
                        enabled: true,
                        group_commit_max_rows: 64,
                        group_commit_max_us: 1_000,
                        ..crate::core::write_owner::WriteHotPathConfig::default()
                    };
                    let _ = owner
                        .run_group_commit(&cx, write_config, |operations| {
                            if batch_is_daemon_txn_coalescible(operations) {
                                execute_daemon_txn_batch(operations)
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
                    source: io::Error::other(format!(
                        "failed to spawn daemon write actor: {error}"
                    )),
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
    let mut response = match dispatched {
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
    write_and_settle_daemon_response(&mut stream, dispatch_policy.as_ref(), &mut response);
}

fn write_and_settle_daemon_response(
    stream: &mut UnixStream,
    dispatch_policy: &DaemonDispatchPolicy,
    response: &mut DaemonResponse,
) -> bool {
    let delivered = write_response(stream, response).is_ok();
    settle_daemon_response_delivery(dispatch_policy, response, delivered);
    delivered
}

fn settle_daemon_response_delivery(
    dispatch_policy: &DaemonDispatchPolicy,
    response: &mut DaemonResponse,
    delivered: bool,
) {
    let Some(delivery) = response.take_delivery() else {
        return;
    };
    settle_search_advisory_delivery(
        dispatch_policy.search_advisory_session(),
        delivery.workspace_id(),
        delivery.search_advisory_token(),
        delivered,
        delivery.search_large_gap_capacity_busy(),
    );
}

fn settle_search_advisory_delivery(
    session: &Mutex<SearchAdvisorySession>,
    workspace_id: &str,
    token: u64,
    delivered: bool,
    large_gap_capacity_busy: bool,
) {
    for attempt in 0..=SEARCH_ADVISORY_SETTLEMENT_RETRY_LIMIT {
        let settlement = session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .settle_delivery(workspace_id, token, delivered, large_gap_capacity_busy);
        if settlement == SearchAdvisorySettlement::Complete {
            return;
        }
        if attempt == SEARCH_ADVISORY_SETTLEMENT_RETRY_LIMIT {
            // The advisory was already delivered fail-open. Leaving it
            // unconsumed can cause duplicate prose later, but can never hide a
            // first affected response under sustained capacity pressure.
            tracing::warn!(
                workspace_id,
                token,
                "search advisory capacity remained busy after bounded settlement retries"
            );
            return;
        }
        std::thread::yield_now();
    }
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

#[cfg(target_vendor = "apple")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    stream.peer_cred().map(|credentials| credentials.uid)
}

#[cfg(all(not(target_os = "linux"), not(target_vendor = "apple")))]
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
    dispatch_with_echo_policy_and_workspace_inner(
        request,
        daemon_echo_enabled(),
        policy.bound_workspace_id(),
        shutdown,
        policy.write_router(),
        policy.search_advisory_session(),
        true,
    )
}

fn dispatch_with_echo_policy(request: &DaemonRequest, echo_enabled: bool) -> DaemonResponse {
    let shutdown = AtomicBool::new(false);
    let search_advisory_session = Mutex::new(SearchAdvisorySession::default());
    dispatch_with_echo_policy_and_workspace(
        request,
        echo_enabled,
        None,
        &shutdown,
        None,
        &search_advisory_session,
    )
}

fn dispatch_with_echo_policy_and_workspace(
    request: &DaemonRequest,
    echo_enabled: bool,
    bound_workspace_id: Option<&str>,
    shutdown: &AtomicBool,
    write_router: Option<&DaemonWriteRouter>,
    search_advisory_session: &Mutex<SearchAdvisorySession>,
) -> DaemonResponse {
    dispatch_with_echo_policy_and_workspace_inner(
        request,
        echo_enabled,
        bound_workspace_id,
        shutdown,
        write_router,
        search_advisory_session,
        false,
    )
}

fn dispatch_with_echo_policy_and_workspace_inner(
    request: &DaemonRequest,
    echo_enabled: bool,
    bound_workspace_id: Option<&str>,
    shutdown: &AtomicBool,
    write_router: Option<&DaemonWriteRouter>,
    search_advisory_session: &Mutex<SearchAdvisorySession>,
    defer_advisory_until_socket_write: bool,
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
        METHOD_CONTEXT => dispatch_context(
            request,
            shutdown,
            search_advisory_session,
            defer_advisory_until_socket_write,
        ),
        METHOD_SEARCH => dispatch_search(
            request,
            shutdown,
            search_advisory_session,
            defer_advisory_until_socket_write,
        ),
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
    let flock_gate = crate::db::flock_gate_telemetry();
    let inputs = crate::core::contention::ContentionInputs {
        singleflight: Some(singleflight),
        group_commit: Some((&group_commit).into()),
        flock_gate: Some((&flock_gate).into()),
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

fn daemon_search_default_limit() -> u32 {
    10
}

fn deserialize_daemon_search_limit<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    let limit = json_number_to_u64(&value)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            <D::Error as serde::de::Error>::custom(
                "limit must be a mathematical integer between 0 and 4294967295",
            )
        })?;
    Ok(limit)
}

fn daemon_search_default_speed() -> String {
    "default".to_owned()
}

fn daemon_search_default_dedupe() -> String {
    "doc_id".to_owned()
}

fn daemon_search_default_source_mode() -> String {
    "hybrid".to_owned()
}

fn daemon_search_default_memory_scope() -> String {
    "swarm".to_owned()
}

/// Strict, method-specific request payload for [`METHOD_SEARCH`]. The daemon
/// envelope remains `ee.daemon.request.v1`; this nested schema lets clients
/// negotiate search semantics independently of the framing version.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DaemonSearchParams {
    schema: String,
    query: String,
    workspace_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    database_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_dir: Option<PathBuf>,
    #[serde(
        default = "daemon_search_default_limit",
        deserialize_with = "deserialize_daemon_search_limit"
    )]
    limit: u32,
    #[serde(default = "daemon_search_default_speed")]
    speed: String,
    #[serde(default)]
    explain: bool,
    #[serde(default)]
    explain_performance: bool,
    #[serde(default)]
    include_tombstoned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    #[serde(default)]
    include_expired: bool,
    #[serde(default)]
    include_future: bool,
    #[serde(default)]
    include_stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default)]
    field_filters: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relevance_floor: Option<f32>,
    #[serde(default = "daemon_search_default_dedupe")]
    dedupe: String,
    #[serde(default = "daemon_search_default_source_mode")]
    source_mode: String,
    #[serde(default)]
    strict_source_mode: bool,
    #[serde(default = "daemon_search_default_memory_scope")]
    memory_scope: String,
    #[serde(default)]
    strict_scope: bool,
}

impl DaemonSearchParams {
    /// Build the canonical daemon request from already-validated CLI options.
    #[must_use]
    pub fn from_search_options(
        options: &SearchOptions,
        kind: Option<&str>,
        field_filters: &[String],
        explain_performance: bool,
    ) -> Self {
        Self {
            schema: DAEMON_SEARCH_REQUEST_SCHEMA_V2.to_owned(),
            query: options.query.clone(),
            workspace_path: options.workspace_path.clone(),
            database_path: options.database_path.clone(),
            index_dir: options.index_dir.clone(),
            limit: options.limit,
            speed: options.speed.as_str().to_owned(),
            explain: options.explain,
            explain_performance,
            include_tombstoned: options.include_tombstoned,
            as_of: options.as_of.as_ref().map(chrono::DateTime::to_rfc3339),
            include_expired: options.include_expired,
            include_future: options.include_future,
            include_stale: options.include_stale,
            kind: kind.map(str::to_owned),
            field_filters: field_filters.to_vec(),
            relevance_floor: options.relevance_floor,
            dedupe: options.dedup_mode.as_str().to_owned(),
            source_mode: options.source_mode.as_str().to_owned(),
            strict_source_mode: options.strict_source_mode,
            memory_scope: options.memory_scope.as_str().to_owned(),
            strict_scope: options.strict_scope,
        }
    }

    fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value.clone())
            .map_err(|_| "params do not match ee.daemon.search.request.v2".to_owned())
    }

    fn into_search_parts(
        self,
        authorized_workspace_id: &str,
    ) -> Result<
        (
            SearchOptions,
            Option<String>,
            Vec<TypedMemoryFieldFilter>,
            bool,
        ),
        String,
    > {
        if self.schema != DAEMON_SEARCH_REQUEST_SCHEMA_V2 {
            return Err(format!(
                "field `schema` must equal `{DAEMON_SEARCH_REQUEST_SCHEMA_V2}`"
            ));
        }
        if self.query.trim().is_empty() {
            return Err("field `query` must not be blank".to_owned());
        }
        if self
            .relevance_floor
            .is_some_and(|floor| !floor.is_finite() || !(0.0..=1.0).contains(&floor))
        {
            return Err("field `relevanceFloor` must be between 0.0 and 1.0".to_owned());
        }
        let speed = parse_daemon_speed_mode(&self.speed)?;
        let source_mode = parse_daemon_source_mode(&self.source_mode)?;
        let dedup_mode = match self.dedupe.as_str() {
            "doc_id" => SearchDedupMode::DocId,
            "mi" => SearchDedupMode::MutualInformation,
            _ => return Err("field `dedupe` must be `doc_id` or `mi`".to_owned()),
        };
        let memory_scope = MemoryScope::parse(&self.memory_scope)
            .filter(|scope| scope.as_str() == self.memory_scope)
            .ok_or_else(|| {
                "field `memoryScope` must be self, team, global, workspace, verified, or swarm"
                    .to_owned()
            })?;
        let as_of = self
            .as_of
            .as_deref()
            .map(chrono::DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| "field `asOf` must be an RFC3339 timestamp".to_owned())?
            .map(|value| value.with_timezone(&chrono::Utc));
        let kind = self
            .kind
            .as_deref()
            .map(normalize_memory_kind_filter)
            .transpose()?;
        let field_filters = self
            .field_filters
            .iter()
            .map(|raw| TypedMemoryFieldFilter::parse(raw))
            .collect::<Result<Vec<_>, _>>()?;
        let workspace_path = canonical_workspace_path(&self.workspace_path, "workspacePath")?;
        let authorized_workspace =
            canonical_workspace_path(Path::new(authorized_workspace_id), "workspace_id")?;
        if workspace_path != authorized_workspace {
            return Err(
                "field `workspacePath` must identify the authorized envelope `workspace_id`"
                    .to_owned(),
            );
        }
        let database_path = self
            .database_path
            .as_deref()
            .map(|path| canonical_contained_path(&workspace_path, path, "databasePath"))
            .transpose()?;
        let index_dir = self
            .index_dir
            .as_deref()
            .map(|path| canonical_contained_path(&workspace_path, path, "indexDir"))
            .transpose()?;
        Ok((
            SearchOptions {
                workspace_path,
                database_path,
                index_dir,
                query: self.query,
                limit: self.limit,
                speed,
                explain: self.explain,
                as_of,
                include_tombstoned: self.include_tombstoned,
                include_expired: self.include_expired,
                include_future: self.include_future,
                include_stale: self.include_stale,
                relevance_floor: self.relevance_floor,
                dedup_mode,
                source_mode,
                strict_source_mode: self.strict_source_mode,
                memory_scope,
                strict_scope: self.strict_scope,
            },
            kind,
            field_filters,
            self.explain_performance,
        ))
    }
}

fn canonical_workspace_path(path: &Path, field: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("field `{field}` must be an absolute path"));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("field `{field}` could not be canonicalized: {error}"))?;
    if !canonical.is_dir() {
        return Err(format!("field `{field}` must identify a directory"));
    }
    Ok(canonical)
}

fn canonical_contained_path(
    canonical_workspace: &Path,
    path: &Path,
    field: &str,
) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("field `{field}` must be an absolute path"));
    }
    let canonical = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
                return Err(format!(
                    "field `{field}` must not identify a dangling symbolic link"
                ));
            }
            let parent = path
                .parent()
                .ok_or_else(|| format!("field `{field}` has no parent directory"))?;
            let file_name = path
                .file_name()
                .ok_or_else(|| format!("field `{field}` has no final path component"))?;
            fs::canonicalize(parent)
                .map_err(|parent_error| {
                    format!("field `{field}` parent could not be canonicalized: {parent_error}")
                })?
                .join(file_name)
        }
        Err(error) => {
            return Err(format!(
                "field `{field}` could not be canonicalized: {error}"
            ));
        }
    };
    if !canonical.starts_with(canonical_workspace) {
        return Err(format!(
            "field `{field}` must remain inside the canonical workspace"
        ));
    }
    Ok(canonical)
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DaemonSearchReuseContract {
    daemon_process: String,
    default_search_embedder: String,
    search_index: String,
}

impl Default for DaemonSearchReuseContract {
    fn default() -> Self {
        Self {
            daemon_process: "long_lived".to_owned(),
            default_search_embedder: "process_scoped".to_owned(),
            search_index: "per_request".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DaemonSearchTimingMeasurement {
    elapsed_ms: f64,
    elapsed_ms_bucket: String,
    nondeterministic: bool,
}

impl DaemonSearchTimingMeasurement {
    fn from_duration(duration: Duration) -> Self {
        let elapsed_ms = duration.as_secs_f64() * 1_000.0;
        let rendered = elapsed_timing_json(elapsed_ms);
        Self {
            elapsed_ms: rendered
                .get("elapsedMs")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(elapsed_ms),
            elapsed_ms_bucket: rendered
                .get("elapsedMsBucket")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("gte_1000ms")
                .to_owned(),
            nondeterministic: true,
        }
    }

    fn validate(&self, field: &str) -> Result<(), String> {
        if !self.elapsed_ms.is_finite() || self.elapsed_ms < 0.0 {
            return Err(format!(
                "timing `{field}.elapsedMs` must be finite and non-negative"
            ));
        }
        if !matches!(
            self.elapsed_ms_bucket.as_str(),
            "lt_1ms" | "1_9ms" | "10_49ms" | "50_99ms" | "100_499ms" | "500_999ms" | "gte_1000ms"
        ) {
            return Err(format!("timing `{field}.elapsedMsBucket` drifted"));
        }
        if !self.nondeterministic {
            return Err(format!(
                "timing `{field}` must declare nondeterministic=true"
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DaemonSearchTiming {
    daemon_total: DaemonSearchTimingMeasurement,
    embedder_preparation: Option<DaemonSearchTimingMeasurement>,
    index_open: Option<DaemonSearchTimingMeasurement>,
    query: Option<DaemonSearchTimingMeasurement>,
}

impl DaemonSearchTiming {
    fn from_trace(daemon_total: Duration, trace: &SearchPerformanceTrace) -> Self {
        Self {
            daemon_total: DaemonSearchTimingMeasurement::from_duration(daemon_total),
            embedder_preparation: aggregate_search_timings(trace, &["search::embedderPrepare"]),
            index_open: aggregate_search_timings(
                trace,
                &[
                    "searchSync::lexicalOpen",
                    "searchSync::twoTierOpen",
                    "searchSync::attachLexical",
                ],
            ),
            query: aggregate_search_timings(
                trace,
                &["searchSync::lexicalSearch", "searchSync::searchCollect"],
            ),
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.daemon_total.validate("daemonTotal")?;
        for (field, timing) in [
            ("embedderPreparation", self.embedder_preparation.as_ref()),
            ("indexOpen", self.index_open.as_ref()),
            ("query", self.query.as_ref()),
        ] {
            if let Some(timing) = timing {
                timing.validate(field)?;
            }
        }
        Ok(())
    }
}

fn aggregate_search_timings(
    trace: &SearchPerformanceTrace,
    names: &[&str],
) -> Option<DaemonSearchTimingMeasurement> {
    let durations = trace
        .timings()
        .filter(|(name, _)| names.contains(name))
        .map(|(_, elapsed)| elapsed)
        .collect::<Vec<_>>();
    (!durations.is_empty()).then(|| {
        DaemonSearchTimingMeasurement::from_duration(
            durations
                .into_iter()
                .fold(Duration::ZERO, Duration::saturating_add),
        )
    })
}

/// Strict method-specific success payload returned by [`METHOD_SEARCH`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DaemonSearchResult {
    schema: String,
    response: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    performance: Option<serde_json::Value>,
    human: String,
    reuse_contract: DaemonSearchReuseContract,
    timing: DaemonSearchTiming,
}

/// Validated public renderings plus daemon-only performance diagnostics.
///
/// Ordinary search output uses `response` and `human`. The explicit
/// `--explain-performance` surface also consumes the exact negotiated reuse
/// contract and timing objects instead of discarding them after validation.
#[derive(Clone, Debug, PartialEq)]
pub struct DaemonSearchRenderings {
    pub response: serde_json::Value,
    pub performance: Option<serde_json::Value>,
    pub human: String,
    pub reuse_contract: serde_json::Value,
    pub timing: serde_json::Value,
}

// Keep this in one-to-one correspondence with the 39 `$defs/uint64` references
// in ee.daemon.search.response.v3. Optional performance paths are skipped when
// the negotiated response omits that block.
const DAEMON_SEARCH_UINT64_INSTANCE_POINTERS: &[&str] = &[
    "/response/data/resultCount",
    "/response/data/rerank/topK",
    "/response/data/rerank/rerankScoreCount",
    "/response/data/rerank/advisorySummary/distinctCount",
    "/response/data/rerank/advisorySummary/emittedCount",
    "/response/data/rerank/advisorySummary/suppressedCount",
    "/response/data/rerank/advisorySummary/sessionOccurrenceCount",
    "/response/data/rerank/advisorySummary/sessionSuppressedCount",
    "/performance/data/query/lengthBytes",
    "/performance/data/queryPlan/requestedLimit",
    "/performance/data/queryPlan/candidateBudget",
    "/performance/data/dbReads/indexStatusChecks",
    "/performance/data/dbReads/memoryReads",
    "/performance/data/dbReads/tagReads",
    "/performance/data/dbReads/artifactLinkReads",
    "/performance/data/profileRuntime/budgets/search/candidateLimit",
    "/performance/data/profileRuntime/budgets/search/concurrentIndexReaders",
    "/performance/data/profileRuntime/budgets/pack/maxTokens",
    "/performance/data/profileRuntime/budgets/pack/maxCandidateMemories",
    "/performance/data/profileRuntime/budgets/cache/memoryCapMb",
    "/performance/data/profileRuntime/budgets/cache/entryCap",
    "/performance/data/profileRuntime/budgets/cache/hotsetPrewarmLimit",
    "/performance/data/profileRuntime/budgets/writeSpool/queueCap",
    "/performance/data/profileRuntime/budgets/writeSpool/batchCap",
    "/performance/data/profileRuntime/budgets/writeSpool/retryBudget",
    "/performance/data/profileRuntime/budgets/steward/maintenanceWindowMs",
    "/performance/data/profileRuntime/budgets/steward/graphRefreshBudget",
    "/performance/data/search/returnedHits",
    "/performance/data/search/sourceCounts/lexical",
    "/performance/data/search/sourceCounts/semanticFast",
    "/performance/data/search/sourceCounts/semanticQuality",
    "/performance/data/search/sourceCounts/hybrid",
    "/performance/data/search/sourceCounts/reranked",
    "/performance/data/search/fieldCoverage/fastScoreCount",
    "/performance/data/search/fieldCoverage/qualityScoreCount",
    "/performance/data/search/fieldCoverage/lexicalScoreCount",
    "/performance/data/search/fieldCoverage/rerankScoreCount",
    "/performance/data/search/fieldCoverage/metadataCount",
    "/performance/data/search/fieldCoverage/explanationCount",
];

fn parse_json_decimal_exponent(raw: &str) -> Option<i64> {
    let (negative, digits) = raw
        .strip_prefix('-')
        .map_or((false, raw), |digits| (true, digits));
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let magnitude = digits.bytes().fold(0_i64, |value, byte| {
        value
            .saturating_mul(10)
            .saturating_add(i64::from(byte - b'0'))
    });
    Some(if negative {
        magnitude.saturating_neg()
    } else {
        magnitude
    })
}

fn rendered_json_number_to_u64(rendered: &str) -> Option<u64> {
    let (negative, rendered) = rendered
        .strip_prefix('-')
        .map_or((false, rendered), |unsigned| (true, unsigned));
    let (mantissa, exponent) =
        rendered
            .find(['e', 'E'])
            .map_or(Some((rendered, 0_i64)), |index| {
                parse_json_decimal_exponent(&rendered[index + 1..])
                    .map(|exponent| (&rendered[..index], exponent))
            })?;
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let digits = whole.bytes().chain(fraction.bytes()).collect::<Vec<_>>();
    if digits.iter().all(|byte| *byte == b'0') {
        return Some(0);
    }
    if negative {
        return None;
    }

    let fraction_len = i64::try_from(fraction.len()).ok()?;
    let scale = exponent.saturating_sub(fraction_len);
    let integer_digits = if scale < 0 {
        let fractional_digits = usize::try_from(scale.saturating_neg()).ok()?;
        if fractional_digits >= digits.len() {
            return None;
        }
        let integer_len = digits.len() - fractional_digits;
        if digits[integer_len..].iter().any(|byte| *byte != b'0') {
            return None;
        }
        &digits[..integer_len]
    } else {
        &digits[..]
    };
    let mut value = 0_u64;
    for byte in integer_digits.iter().skip_while(|byte| **byte == b'0') {
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(*byte - b'0'))?;
    }
    if scale > 0 {
        let trailing_zeros = usize::try_from(scale).ok()?;
        if trailing_zeros > 20 {
            return None;
        }
        for _ in 0..trailing_zeros {
            value = value.checked_mul(10)?;
        }
    }
    Some(value)
}

// Draft 2020-12's `integer` type is mathematical, not lexical: `1.0` is an
// integer. Serde's arbitrary-precision number representation preserves the raw
// decimal token so values near u64::MAX never pass through f64 rounding.
fn json_number_to_u64(value: &serde_json::Value) -> Option<u64> {
    let number = value.as_number()?;
    number
        .as_u64()
        .or_else(|| rendered_json_number_to_u64(&number.to_string()))
}

fn canonicalize_daemon_search_uint64s(value: &mut serde_json::Value) -> Result<(), String> {
    for pointer in DAEMON_SEARCH_UINT64_INSTANCE_POINTERS {
        let Some(field) = value.pointer_mut(pointer) else {
            continue;
        };
        let unsigned = json_number_to_u64(field)
            .ok_or_else(|| format!("daemon search unsigned field `{pointer}` is not a uint64"))?;
        *field = serde_json::Value::from(unsigned);
    }
    Ok(())
}

impl DaemonSearchResult {
    #[cfg(test)]
    fn from_report(
        report: &SearchReport,
        explain: bool,
        workspace_id: &str,
        advisory_session: &mut SearchAdvisorySession,
        timing: DaemonSearchTiming,
        performance: Option<serde_json::Value>,
    ) -> Self {
        Self::from_report_inner(
            report,
            explain,
            workspace_id,
            advisory_session,
            None,
            timing,
            performance,
        )
    }

    fn from_report_for_delivery(
        report: &SearchReport,
        explain: bool,
        workspace_id: &str,
        advisory_session: &mut SearchAdvisorySession,
        reservation: &mut SearchAdvisoryDeliveryReservation,
        timing: DaemonSearchTiming,
        performance: Option<serde_json::Value>,
    ) -> Self {
        Self::from_report_inner(
            report,
            explain,
            workspace_id,
            advisory_session,
            Some(reservation),
            timing,
            performance,
        )
    }

    fn from_report_inner(
        report: &SearchReport,
        explain: bool,
        workspace_id: &str,
        advisory_session: &mut SearchAdvisorySession,
        reservation: Option<&mut SearchAdvisoryDeliveryReservation>,
        timing: DaemonSearchTiming,
        performance: Option<serde_json::Value>,
    ) -> Self {
        let mut data = match reservation {
            Some(reservation) => report.data_json_with_advisory_delivery_reservation(
                advisory_session,
                workspace_id,
                reservation,
            ),
            None => {
                report.data_json_with_advisory_session_for_workspace(advisory_session, workspace_id)
            }
        };
        let human = daemon_search_human_summary(report, &data);
        if explain && let Some(object) = data.as_object_mut() {
            object.insert(
                "resultPath".to_owned(),
                serde_json::Value::String("data.results".to_owned()),
            );
        }
        let degraded = crate::output::response_degraded_from_data(&data);
        let response = serde_json::json!({
            "schema": crate::models::RESPONSE_SCHEMA_V2,
            "success": true,
            "data": data,
            "degraded": degraded,
        });
        Self {
            schema: DAEMON_SEARCH_RESPONSE_SCHEMA_V3.to_owned(),
            response,
            performance,
            human,
            reuse_contract: DaemonSearchReuseContract::default(),
            timing,
        }
    }

    /// Decode and validate the complete method-specific response contract.
    pub fn from_value(mut value: serde_json::Value) -> Result<Self, String> {
        canonicalize_daemon_search_uint64s(&mut value)?;
        let result: Self = serde_json::from_value(value)
            .map_err(|_| "result does not match ee.daemon.search.response.v3".to_owned())?;
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != DAEMON_SEARCH_RESPONSE_SCHEMA_V3 {
            return Err(format!(
                "result schema must equal `{DAEMON_SEARCH_RESPONSE_SCHEMA_V3}`"
            ));
        }
        if self.reuse_contract != DaemonSearchReuseContract::default() {
            return Err("daemon search reuse contract drifted".to_owned());
        }
        let response = self
            .response
            .as_object()
            .ok_or_else(|| "daemon search response must be an object".to_owned())?;
        if response.len() != 4
            || !["schema", "success", "data", "degraded"]
                .iter()
                .all(|field| response.contains_key(*field))
        {
            return Err("canonical daemon search response field set drifted".to_owned());
        }
        if response.get("schema").and_then(serde_json::Value::as_str)
            != Some(crate::models::RESPONSE_SCHEMA_V2)
        {
            return Err("canonical daemon search response schema drifted".to_owned());
        }
        if response.get("success").and_then(serde_json::Value::as_bool) != Some(true)
            || !response
                .get("data")
                .is_some_and(serde_json::Value::is_object)
            || !response
                .get("degraded")
                .is_some_and(serde_json::Value::is_array)
        {
            return Err("canonical daemon search response shape drifted".to_owned());
        }
        validate_canonical_degradations(
            response
                .get("degraded")
                .ok_or_else(|| "canonical daemon search degraded list missing".to_owned())?,
        )?;
        validate_canonical_search_data(
            response
                .get("data")
                .ok_or_else(|| "canonical daemon search data missing".to_owned())?,
        )?;
        if let Some(performance) = self.performance.as_ref() {
            validate_search_performance_explain(performance)?;
        }
        self.timing.validate()
    }

    /// Split the validated payload into canonical renderings and diagnostics.
    pub fn into_renderings(self) -> Result<DaemonSearchRenderings, String> {
        Ok(DaemonSearchRenderings {
            response: self.response,
            performance: self.performance,
            human: self.human,
            reuse_contract: serde_json::to_value(self.reuse_contract).map_err(|error| {
                format!("daemon search reuse contract could not encode: {error}")
            })?,
            timing: serde_json::to_value(self.timing)
                .map_err(|error| format!("daemon search timing could not encode: {error}"))?,
        })
    }
}

fn validate_search_performance_explain(value: &serde_json::Value) -> Result<(), String> {
    validate_exact_object_fields(
        value,
        "daemon search performance",
        &["schema", "success", "data"],
        &[],
    )?;
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some(PERFORMANCE_EXPLAIN_SCHEMA_V1)
        || value.get("success").and_then(serde_json::Value::as_bool) != Some(true)
    {
        return Err("daemon search performance envelope drifted".to_owned());
    }
    let data = value
        .get("data")
        .ok_or_else(|| "daemon search performance data missing".to_owned())?;
    validate_exact_object_fields(
        data,
        "daemon search performance data",
        &[
            "command",
            "query",
            "queryPlan",
            "profileRuntime",
            "dbReads",
            "search",
            "timings",
            "pack",
            "cache",
            "graph",
            "fallbacks",
            "redaction",
        ],
        &[],
    )?;
    if data.get("command").and_then(serde_json::Value::as_str) != Some("search") {
        return Err("daemon search performance command drifted".to_owned());
    }
    validate_search_performance_query(
        data.get("query")
            .ok_or_else(|| "daemon search performance query missing".to_owned())?,
    )?;
    validate_search_performance_query_plan(
        data.get("queryPlan")
            .ok_or_else(|| "daemon search performance queryPlan missing".to_owned())?,
    )?;
    validate_search_performance_runtime_profile(
        data.get("profileRuntime")
            .ok_or_else(|| "daemon search performance profileRuntime missing".to_owned())?,
    )?;
    validate_search_performance_db_reads(
        data.get("dbReads")
            .ok_or_else(|| "daemon search performance dbReads missing".to_owned())?,
    )?;
    validate_search_performance_search(
        data.get("search")
            .ok_or_else(|| "daemon search performance search missing".to_owned())?,
    )?;
    validate_search_performance_timings(
        data.get("timings")
            .ok_or_else(|| "daemon search performance timings missing".to_owned())?,
        "daemon search performance timings",
    )?;
    for field in ["pack", "cache", "graph"] {
        validate_search_performance_not_used(
            data.get(field)
                .ok_or_else(|| format!("daemon search performance {field} missing"))?,
            field,
        )?;
    }
    let fallbacks = data
        .get("fallbacks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "daemon search performance fallbacks must be an array".to_owned())?;
    for (index, fallback) in fallbacks.iter().enumerate() {
        validate_exact_object_fields(
            fallback,
            &format!("daemon search performance fallback {index}"),
            &["code", "severity", "message", "sources"],
            &[],
        )?;
        if !fallback
            .get("code")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|code| !code.is_empty())
            || !matches!(
                fallback.get("severity").and_then(serde_json::Value::as_str),
                Some("info" | "low" | "warning" | "medium" | "high" | "critical")
            )
            || fallback.get("message").and_then(serde_json::Value::as_str)
                != Some(PERFORMANCE_FALLBACK_REDACTED_MESSAGE)
            || fallback.get("sources") != Some(&serde_json::json!(["search"]))
        {
            return Err(format!(
                "daemon search performance fallback {index} field types or values drifted"
            ));
        }
    }
    validate_search_performance_redaction(
        data.get("redaction")
            .ok_or_else(|| "daemon search performance redaction missing".to_owned())?,
    )?;
    Ok(())
}

fn validate_search_performance_query(value: &serde_json::Value) -> Result<(), String> {
    validate_exact_object_fields(
        value,
        "daemon search performance query",
        &["textIncluded", "lengthBytes", "fingerprint"],
        &[],
    )?;
    if value
        .get("textIncluded")
        .and_then(serde_json::Value::as_bool)
        != Some(false)
        || !value.get("lengthBytes").is_some_and(is_json_unsigned)
        || !value
            .get("fingerprint")
            .and_then(serde_json::Value::as_str)
            .is_some_and(valid_blake3_fingerprint)
    {
        return Err("daemon search performance query field types or values drifted".to_owned());
    }
    Ok(())
}

fn valid_blake3_fingerprint(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn validate_search_performance_query_plan(value: &serde_json::Value) -> Result<(), String> {
    const REQUIRED: &[&str] = &[
        "retrievalMode",
        "requestedLimit",
        "candidateBudget",
        "usesEmbeddings",
        "scoreExplanationsRequested",
        "sourceModeRequested",
        "sourceModeApplied",
        "strictSourceMode",
        "fallbackApplied",
        "memoryScope",
        "strictScope",
    ];
    validate_exact_object_fields(value, "daemon search performance queryPlan", REQUIRED, &[])?;
    let string_is = |field: &str, allowed: &[&str]| {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|actual| allowed.contains(&actual))
    };
    if !string_is("retrievalMode", &["instant", "default", "quality"])
        || !string_is(
            "sourceModeRequested",
            &["lexical_only", "semantic_only", "hybrid"],
        )
        || !string_is(
            "sourceModeApplied",
            &["lexical_only", "semantic_only", "hybrid"],
        )
        || !string_is(
            "memoryScope",
            &["self", "team", "global", "workspace", "verified", "swarm"],
        )
        || !["requestedLimit", "candidateBudget"]
            .iter()
            .all(|field| value.get(*field).is_some_and(is_json_unsigned))
        || ![
            "usesEmbeddings",
            "scoreExplanationsRequested",
            "strictSourceMode",
            "fallbackApplied",
            "strictScope",
        ]
        .iter()
        .all(|field| value.get(*field).is_some_and(serde_json::Value::is_boolean))
    {
        return Err("daemon search performance queryPlan field types or values drifted".to_owned());
    }
    Ok(())
}

fn validate_search_performance_runtime_profile(value: &serde_json::Value) -> Result<(), String> {
    validate_exact_object_fields(
        value,
        "daemon search performance profileRuntime",
        &["schema", "activeProfile", "source", "budgets"],
        &[],
    )?;
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some(crate::core::profile::RUNTIME_PROFILE_SCHEMA_V1)
        || !matches!(
            value
                .get("activeProfile")
                .and_then(serde_json::Value::as_str),
            Some("constrained" | "portable" | "workstation" | "swarm")
        )
        || !value
            .get("source")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|source| !source.is_empty())
    {
        return Err(
            "daemon search performance profileRuntime field types or values drifted".to_owned(),
        );
    }
    let budgets = value
        .get("budgets")
        .ok_or_else(|| "daemon search performance profileRuntime.budgets missing".to_owned())?;
    validate_exact_object_fields(
        budgets,
        "daemon search performance profileRuntime.budgets",
        &[
            "search",
            "pack",
            "cache",
            "writeSpool",
            "steward",
            "verification",
            "diagnostics",
        ],
        &[],
    )?;
    validate_performance_unsigned_object(
        budgets.get("cache"),
        "daemon search performance profileRuntime.budgets.cache",
        &["memoryCapMb", "entryCap", "hotsetPrewarmLimit"],
    )?;
    validate_performance_unsigned_object(
        budgets.get("writeSpool"),
        "daemon search performance profileRuntime.budgets.writeSpool",
        &["queueCap", "batchCap", "retryBudget"],
    )?;

    let search = required_performance_object(
        budgets.get("search"),
        "daemon search performance profileRuntime.budgets.search",
        &[
            "candidateLimit",
            "concurrentIndexReaders",
            "staleIndexTolerance",
        ],
    )?;
    if !["candidateLimit", "concurrentIndexReaders"]
        .iter()
        .all(|field| search.get(*field).is_some_and(is_json_unsigned))
        || !matches!(
            search
                .get("staleIndexTolerance")
                .and_then(serde_json::Value::as_str),
            Some("strict" | "repair_hint")
        )
    {
        return Err("daemon search performance profileRuntime.budgets.search drifted".to_owned());
    }

    let pack = required_performance_object(
        budgets.get("pack"),
        "daemon search performance profileRuntime.budgets.pack",
        &["maxTokens", "maxCandidateMemories", "explanationVerbosity"],
    )?;
    if !["maxTokens", "maxCandidateMemories"]
        .iter()
        .all(|field| pack.get(*field).is_some_and(is_json_unsigned))
        || !matches!(
            pack.get("explanationVerbosity")
                .and_then(serde_json::Value::as_str),
            Some("standard" | "full")
        )
    {
        return Err("daemon search performance profileRuntime.budgets.pack drifted".to_owned());
    }

    let steward = required_performance_object(
        budgets.get("steward"),
        "daemon search performance profileRuntime.budgets.steward",
        &["maintenanceWindowMs", "graphRefreshBudget", "daemonPrewarm"],
    )?;
    if !["maintenanceWindowMs", "graphRefreshBudget"]
        .iter()
        .all(|field| steward.get(*field).is_some_and(is_json_unsigned))
        || !steward
            .get("daemonPrewarm")
            .is_some_and(serde_json::Value::is_boolean)
    {
        return Err("daemon search performance profileRuntime.budgets.steward drifted".to_owned());
    }

    let verification = required_performance_object(
        budgets.get("verification"),
        "daemon search performance profileRuntime.budgets.verification",
        &[
            "recipe",
            "targetDirPosture",
            "timeoutClass",
            "heavyStrategy",
        ],
    )?;
    for (field, allowed) in [
        ("recipe", &["quick", "workspace", "full"][..]),
        ("targetDirPosture", &["shared", "isolated"][..]),
        ("timeoutClass", &["short", "standard", "extended"][..]),
        (
            "heavyStrategy",
            &["manual", "rch_preferred", "rch_default"][..],
        ),
    ] {
        if !verification
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| allowed.contains(&value))
        {
            return Err(format!(
                "daemon search performance profileRuntime.budgets.verification.{field} drifted"
            ));
        }
    }

    let diagnostics = required_performance_object(
        budgets.get("diagnostics"),
        "daemon search performance profileRuntime.budgets.diagnostics",
        &["supportBundleProfile", "redaction"],
    )?;
    if !diagnostics
        .get("supportBundleProfile")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| ["minimal", "standard", "full"].contains(&value))
        || !diagnostics
            .get("redaction")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| ["strict", "policy_applied"].contains(&value))
    {
        return Err(
            "daemon search performance profileRuntime.budgets.diagnostics drifted".to_owned(),
        );
    }
    Ok(())
}

fn validate_search_performance_search(value: &serde_json::Value) -> Result<(), String> {
    const FIELDS: &[&str] = &[
        "status",
        "returnedHits",
        "sourceCounts",
        "scoreDistribution",
        "fieldCoverage",
        "errors",
        "elapsed",
        "timings",
    ];
    validate_exact_object_fields(value, "daemon search performance search", FIELDS, &[])?;
    if !matches!(
        value.get("status").and_then(serde_json::Value::as_str),
        Some("success" | "no_results" | "index_not_found" | "index_error")
    ) || !value.get("returnedHits").is_some_and(is_json_unsigned)
        || !value
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|errors| errors.iter().all(serde_json::Value::is_string))
    {
        return Err("daemon search performance search field types or values drifted".to_owned());
    }
    validate_performance_unsigned_object(
        value.get("sourceCounts"),
        "daemon search performance search.sourceCounts",
        &[
            "lexical",
            "semanticFast",
            "semanticQuality",
            "hybrid",
            "reranked",
        ],
    )?;
    validate_performance_optional_number_object(
        value.get("scoreDistribution"),
        "daemon search performance search.scoreDistribution",
        &["top", "min", "max", "mean"],
    )?;
    validate_performance_unsigned_object(
        value.get("fieldCoverage"),
        "daemon search performance search.fieldCoverage",
        &[
            "fastScoreCount",
            "qualityScoreCount",
            "lexicalScoreCount",
            "rerankScoreCount",
            "metadataCount",
            "explanationCount",
        ],
    )?;
    validate_search_performance_elapsed(
        value
            .get("elapsed")
            .ok_or_else(|| "daemon search performance search.elapsed missing".to_owned())?,
        "daemon search performance search.elapsed",
        false,
    )?;
    validate_search_performance_timings(
        value
            .get("timings")
            .ok_or_else(|| "daemon search performance search.timings missing".to_owned())?,
        "daemon search performance search.timings",
    )
}

fn validate_search_performance_timings(
    value: &serde_json::Value,
    context: &str,
) -> Result<(), String> {
    let timings = value
        .as_array()
        .ok_or_else(|| format!("{context} must be an array"))?;
    for (index, timing) in timings.iter().enumerate() {
        validate_search_performance_elapsed(timing, &format!("{context}[{index}]"), true)?;
    }
    Ok(())
}

fn validate_search_performance_elapsed(
    value: &serde_json::Value,
    context: &str,
    named: bool,
) -> Result<(), String> {
    let required = if named {
        &["elapsedMs", "elapsedMsBucket", "nondeterministic", "name"][..]
    } else {
        &["elapsedMs", "elapsedMsBucket", "nondeterministic"][..]
    };
    validate_exact_object_fields(value, context, required, &[])?;
    if !value
        .get("elapsedMs")
        .and_then(serde_json::Value::as_f64)
        .is_some_and(|elapsed| elapsed.is_finite() && elapsed >= 0.0)
        || !matches!(
            value
                .get("elapsedMsBucket")
                .and_then(serde_json::Value::as_str),
            Some(
                "lt_1ms"
                    | "1_9ms"
                    | "10_49ms"
                    | "50_99ms"
                    | "100_499ms"
                    | "500_999ms"
                    | "gte_1000ms"
            )
        )
        || value
            .get("nondeterministic")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || named
            && !value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| !name.is_empty())
    {
        return Err(format!("{context} field types or values drifted"));
    }
    Ok(())
}

fn required_performance_object<'a>(
    value: Option<&'a serde_json::Value>,
    context: &str,
    fields: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    let value = value.ok_or_else(|| format!("{context} missing"))?;
    validate_exact_object_fields(value, context, fields, &[])?;
    value
        .as_object()
        .ok_or_else(|| format!("{context} must be an object"))
}

fn validate_performance_unsigned_object(
    value: Option<&serde_json::Value>,
    context: &str,
    fields: &[&str],
) -> Result<(), String> {
    let object = required_performance_object(value, context, fields)?;
    if fields
        .iter()
        .all(|field| object.get(*field).is_some_and(is_json_unsigned))
    {
        Ok(())
    } else {
        Err(format!("{context} field types drifted"))
    }
}

fn validate_performance_optional_number_object(
    value: Option<&serde_json::Value>,
    context: &str,
    fields: &[&str],
) -> Result<(), String> {
    let object = required_performance_object(value, context, fields)?;
    if fields.iter().all(|field| {
        object.get(*field).is_some_and(|value| {
            value.is_null() || value.as_f64().is_some_and(|number| number.is_finite())
        })
    }) {
        Ok(())
    } else {
        Err(format!("{context} field types drifted"))
    }
}

fn is_json_unsigned(value: &serde_json::Value) -> bool {
    json_number_to_u64(value).is_some()
}

fn validate_search_performance_db_reads(value: &serde_json::Value) -> Result<(), String> {
    const FIELDS: &[&str] = &[
        "indexStatusChecks",
        "memoryReads",
        "tagReads",
        "artifactLinkReads",
    ];
    validate_exact_object_fields(value, "daemon search performance dbReads", FIELDS, &[])?;
    if FIELDS
        .iter()
        .all(|field| value.get(*field).is_some_and(is_json_unsigned))
    {
        Ok(())
    } else {
        Err("daemon search performance dbReads field types drifted".to_owned())
    }
}

fn validate_search_performance_not_used(
    value: &serde_json::Value,
    field: &str,
) -> Result<(), String> {
    let context = format!("daemon search performance {field}");
    validate_exact_object_fields(value, &context, &["status", "reason"], &[])?;
    if value.get("status").and_then(serde_json::Value::as_str) != Some("not_used")
        || !value
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|reason| !reason.is_empty())
    {
        return Err(format!("{context} field types or values drifted"));
    }
    Ok(())
}

fn validate_search_performance_redaction(value: &serde_json::Value) -> Result<(), String> {
    validate_exact_object_fields(
        value,
        "daemon search performance redaction",
        &["memoryContentIncluded", "queryTextIncluded", "safeFields"],
        &[],
    )?;
    if value
        .get("memoryContentIncluded")
        .and_then(serde_json::Value::as_bool)
        != Some(false)
        || value
            .get("queryTextIncluded")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || value.get("safeFields")
            != Some(&serde_json::json!([
                "counts",
                "elapsedMs",
                "elapsedMsBucket",
                "status",
                "fingerprints",
                "degradationCodes"
            ]))
    {
        return Err("daemon search performance redaction contract drifted".to_owned());
    }
    Ok(())
}

fn daemon_search_human_summary(report: &SearchReport, response_data: &serde_json::Value) -> String {
    let large_gap_active = response_data
        .pointer("/indexFreshness/largeGap")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let large_gap_advisory_emitted = response_data
        .get("degraded")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("code").and_then(serde_json::Value::as_str)
                    == Some("search_index_large_gap")
            })
        });
    if large_gap_advisory_emitted {
        return report.human_summary();
    }

    let mut visible_report = report.clone();
    visible_report.degraded.retain(|entry| {
        entry.code != "search_index_large_gap"
            && !(large_gap_active && entry.code == "search_index_stale")
    });
    visible_report.human_summary()
}

fn daemon_search_degraded_codes(result: &DaemonSearchResult) -> Vec<String> {
    result
        .response
        .get("degraded")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("code").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn validate_canonical_degradations(degraded: &serde_json::Value) -> Result<(), String> {
    const REQUIRED: &[&str] = &["code", "severity", "message"];
    const OPTIONAL: &[&str] = &["repair", "repairKind", "sources", "details"];
    let entries = degraded
        .as_array()
        .ok_or_else(|| "canonical daemon search degraded list must be an array".to_owned())?;
    for (index, entry) in entries.iter().enumerate() {
        let context = format!("canonical daemon search degraded[{index}]");
        validate_exact_object_fields(entry, &context, REQUIRED, OPTIONAL)?;
        let object = entry
            .as_object()
            .ok_or_else(|| format!("{context} must be an object"))?;
        if !["code", "message"]
            .iter()
            .all(|field| object.get(*field).is_some_and(serde_json::Value::is_string))
        {
            return Err(format!("{context} code and message must be strings"));
        }
        if !matches!(
            object.get("severity").and_then(serde_json::Value::as_str),
            Some("info" | "low" | "warning" | "medium" | "high" | "critical")
        ) {
            return Err(format!("{context} severity drifted"));
        }
        if object.get("repair").is_some_and(|value| !value.is_string())
            || object.get("repairKind").is_some_and(|value| {
                !matches!(
                    value.as_str(),
                    Some("actionable" | "template" | "placeholder" | "unknown" | "empty")
                )
            })
            || object.get("sources").is_some_and(|value| {
                !value
                    .as_array()
                    .is_some_and(|sources| sources.iter().all(serde_json::Value::is_string))
            })
            || object
                .get("details")
                .is_some_and(|value| !value.is_object())
        {
            return Err(format!("{context} optional field shape drifted"));
        }
    }
    Ok(())
}

fn validate_exact_object_fields(
    value: &serde_json::Value,
    context: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{context} must be an object"))?;
    if let Some(missing) = required.iter().find(|field| !object.contains_key(**field)) {
        return Err(format!("{context} is missing required field `{missing}`"));
    }
    if let Some(unknown) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()) && !optional.contains(&field.as_str()))
    {
        return Err(format!("{context} contains unknown field `{unknown}`"));
    }
    Ok(())
}

fn validate_canonical_search_data(data: &serde_json::Value) -> Result<(), String> {
    const REQUIRED: &[&str] = &[
        "command",
        "status",
        "embed_backend",
        "query",
        "request",
        "scopeStats",
        "results",
        "consensus",
        "conflicts",
        "resultCount",
        "elapsedMs",
        "metrics",
        "rerank",
        "profileRuntime",
        "errors",
        "degraded",
    ];
    validate_exact_object_fields(
        data,
        "canonical search data",
        REQUIRED,
        &["queryAssist", "resultPath"],
    )?;
    if data.get("command").and_then(serde_json::Value::as_str) != Some("search") {
        return Err("canonical search data command drifted".to_owned());
    }
    if !matches!(
        data.get("status").and_then(serde_json::Value::as_str),
        Some("success" | "no_results" | "index_not_found" | "index_error")
    ) {
        return Err("canonical search data status drifted".to_owned());
    }
    if !matches!(
        data.get("embed_backend")
            .and_then(serde_json::Value::as_str),
        Some("neural_local" | "hash_fallback")
    ) {
        return Err("canonical search embed_backend drifted".to_owned());
    }
    if !data.get("query").is_some_and(serde_json::Value::is_string)
        || !["request", "scopeStats", "metrics", "profileRuntime"]
            .iter()
            .all(|field| data.get(*field).is_some_and(serde_json::Value::is_object))
        || !["consensus", "conflicts", "errors", "degraded"]
            .iter()
            .all(|field| data.get(*field).is_some_and(serde_json::Value::is_array))
    {
        return Err("canonical search nested data shape drifted".to_owned());
    }
    validate_canonical_search_rerank(
        data.get("rerank")
            .ok_or_else(|| "canonical search rerank posture missing".to_owned())?,
    )?;
    let elapsed_ms = data
        .get("elapsedMs")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "canonical search elapsedMs must be a number".to_owned())?;
    if !elapsed_ms.is_finite() || elapsed_ms < 0.0 {
        return Err("canonical search elapsedMs must be finite and non-negative".to_owned());
    }
    let results = data
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "canonical search results must be an array".to_owned())?;
    let result_count = data
        .get("resultCount")
        .and_then(json_number_to_u64)
        .ok_or_else(|| "canonical search resultCount must be a non-negative integer".to_owned())?;
    if result_count != results.len() as u64 {
        return Err("canonical search resultCount does not match results length".to_owned());
    }
    for (index, result) in results.iter().enumerate() {
        validate_canonical_search_result(result, index)?;
    }
    if data
        .get("queryAssist")
        .is_some_and(|value| !value.is_object())
    {
        return Err("canonical search queryAssist must be an object".to_owned());
    }
    if data
        .get("resultPath")
        .is_some_and(|value| value.as_str() != Some("data.results"))
    {
        return Err("canonical search resultPath drifted".to_owned());
    }
    Ok(())
}

fn validate_canonical_search_rerank(value: &serde_json::Value) -> Result<(), String> {
    const REQUIRED: &[&str] = &[
        "schema",
        "mode",
        "configured",
        "topK",
        "rerankScoreCount",
        "scoreKind",
        "available",
        "degradedCode",
        "advisory",
        "advisorySummary",
    ];
    validate_exact_object_fields(value, "canonical search rerank posture", REQUIRED, &[])?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some("ee.rerank_posture.v1")
        || !matches!(
            value.get("mode").and_then(serde_json::Value::as_str),
            Some("reranked" | "fusion_only_degraded" | "fusion_only")
        )
        || !matches!(
            value.get("configured").and_then(serde_json::Value::as_str),
            Some("auto" | "off")
        )
        || !["topK", "rerankScoreCount"]
            .iter()
            .all(|field| value.get(*field).is_some_and(is_json_unsigned))
        || !matches!(
            value.get("scoreKind").and_then(serde_json::Value::as_str),
            Some("reranked" | "rrf_fused")
        )
        || !value
            .get("available")
            .is_some_and(serde_json::Value::is_boolean)
        || !value.get("degradedCode").is_some_and(|code| {
            code.is_null() || code.as_str().is_some_and(|code| !code.is_empty())
        })
    {
        return Err("canonical search rerank posture field types or values drifted".to_owned());
    }

    let advisory = value
        .get("advisory")
        .ok_or_else(|| "canonical search rerank advisory missing".to_owned())?;
    if !advisory.is_null() {
        validate_exact_object_fields(
            advisory,
            "canonical search rerank advisory",
            &[
                "code",
                "severity",
                "permanent",
                "message",
                "repair",
                "resolution",
            ],
            &[],
        )?;
        if !advisory
            .get("code")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|code| !code.is_empty())
            || !matches!(
                advisory.get("severity").and_then(serde_json::Value::as_str),
                Some("info" | "low" | "warning" | "medium" | "high" | "critical")
            )
            || !advisory
                .get("permanent")
                .is_some_and(serde_json::Value::is_boolean)
            || !advisory
                .get("message")
                .is_some_and(serde_json::Value::is_string)
            || !advisory
                .get("repair")
                .is_some_and(|repair| repair.is_null() || repair.is_string())
            || !matches!(
                advisory
                    .get("resolution")
                    .and_then(serde_json::Value::as_str),
                Some("automatic_repair_unavailable" | "retry_or_inspect_local_registry")
            )
        {
            return Err(
                "canonical search rerank advisory field types or values drifted".to_owned(),
            );
        }
    }

    let summary_value = value
        .get("advisorySummary")
        .ok_or_else(|| "canonical search rerank advisorySummary missing".to_owned())?;
    validate_exact_object_fields(
        summary_value,
        "canonical search rerank advisorySummary",
        &[
            "scope",
            "permanent",
            "distinctCount",
            "emittedCount",
            "suppressedCount",
            "sessionOccurrenceCount",
            "sessionSuppressedCount",
        ],
        &[],
    )?;
    let summary = summary_value
        .as_object()
        .ok_or_else(|| "canonical search rerank advisorySummary must be an object".to_owned())?;
    if !matches!(
        summary.get("scope").and_then(serde_json::Value::as_str),
        Some(
            crate::core::search::SEARCH_ADVISORY_SCOPE_INVOCATION
                | crate::core::search::SEARCH_ADVISORY_SCOPE_PROCESS
                | "response",
        )
    ) || !summary
        .get("permanent")
        .is_some_and(|permanent| permanent.is_null() || permanent.is_boolean())
        || ![
            "distinctCount",
            "emittedCount",
            "suppressedCount",
            "sessionOccurrenceCount",
            "sessionSuppressedCount",
        ]
        .iter()
        .all(|field| summary.get(*field).is_some_and(is_json_unsigned))
    {
        return Err(
            "canonical search rerank advisorySummary field types or values drifted".to_owned(),
        );
    }
    Ok(())
}

fn validate_canonical_search_result(
    result: &serde_json::Value,
    index: usize,
) -> Result<(), String> {
    const REQUIRED: &[&str] = &[
        "docId",
        "score",
        "relevanceScore",
        "scoreKind",
        "scoreInterval",
        "coverageGuarantee",
        "calibrated",
        "source",
        "why",
        "provenance",
    ];
    const OPTIONAL: &[&str] = &[
        "memoryId",
        "fastScore",
        "qualityScore",
        "lexicalScore",
        "rerankScore",
        "metadata",
        "driftHint",
        "meshProvenance",
        "meshTrustAdjustment",
        "content",
        "content_truncated",
        "contentRedacted",
        "redactions",
        "tombstoned",
        "tombstonedAt",
        "validFrom",
        "validTo",
        "validityStatus",
        "validityWindowKind",
        "explanation",
    ];
    let context = format!("canonical search result[{index}]");
    validate_exact_object_fields(result, &context, REQUIRED, OPTIONAL)?;
    if !["docId", "why"]
        .iter()
        .all(|field| result.get(*field).is_some_and(serde_json::Value::is_string))
        || !result
            .get("provenance")
            .is_some_and(serde_json::Value::is_array)
        || !result
            .get("calibrated")
            .is_some_and(serde_json::Value::is_boolean)
    {
        return Err(format!("{context} required field types drifted"));
    }
    for field in ["score", "relevanceScore"] {
        let value = result
            .get(field)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| format!("{context}.{field} must be a number"))?;
        if !value.is_finite() {
            return Err(format!("{context}.{field} must be finite"));
        }
    }
    let relevance = result["relevanceScore"].as_f64().unwrap_or_default();
    if !(0.0..=1.0).contains(&relevance) {
        return Err(format!("{context}.relevanceScore must be between 0 and 1"));
    }
    if !matches!(
        result.get("scoreKind").and_then(serde_json::Value::as_str),
        Some("unit_normalized" | "rrf_fused" | "reranked")
    ) || !matches!(
        result.get("source").and_then(serde_json::Value::as_str),
        Some("lexical" | "semantic_fast" | "semantic_quality" | "hybrid" | "reranked")
    ) {
        return Err(format!("{context} score/source vocabulary drifted"));
    }
    let interval = result
        .get("scoreInterval")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{context}.scoreInterval must be an array"))?;
    if interval.len() != 2
        || !interval
            .iter()
            .all(|value| value.as_f64().is_some_and(f64::is_finite))
    {
        return Err(format!(
            "{context}.scoreInterval must contain two finite numbers"
        ));
    }
    if !result.get("coverageGuarantee").is_some_and(|value| {
        value.is_null()
            || value
                .as_f64()
                .is_some_and(|number| number.is_finite() && (0.0..=1.0).contains(&number))
    }) {
        return Err(format!("{context}.coverageGuarantee drifted"));
    }
    Ok(())
}

fn dispatch_search(
    request: &DaemonRequest,
    shutdown: &AtomicBool,
    search_advisory_session: &Mutex<SearchAdvisorySession>,
    defer_advisory_until_socket_write: bool,
) -> DaemonResponse {
    let daemon_total_start = Instant::now();
    if shutdown.load(Ordering::SeqCst) {
        return daemon_shutting_down_response(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
        );
    }
    let params = match DaemonSearchParams::from_value(&request.params) {
        Ok(params) => params,
        Err(message) => return daemon_search_params_error(request, &message),
    };
    let Some(authorized_workspace_id) = request.workspace_id.as_deref() else {
        return daemon_search_params_error(
            request,
            "authorized envelope `workspace_id` is missing",
        );
    };
    let (options, kind, field_filters, explain_performance) =
        match params.into_search_parts(authorized_workspace_id) {
            Ok(parts) => parts,
            Err(message) => return daemon_search_params_error(request, &message),
        };
    let search_run =
        match run_search_with_performance_and_filters(&options, kind.as_deref(), &field_filters) {
            Ok(run) => run,
            Err(error) => {
                return DaemonResponse::err(
                    request.request_id.clone(),
                    request.agent_id.clone(),
                    request.workspace_id.clone(),
                    DAEMON_SEARCH_EXECUTION_FAILED_CODE,
                    format!("ee.daemon.search could not execute canonical search: {error}"),
                );
            }
        };
    let advisory_workspace_id =
        crate::core::workspace::stable_workspace_id(&options.workspace_path);
    let performance = explain_performance.then(|| {
        search_run.report.performance_explain_json_with_trace(
            options.speed,
            options.explain,
            &search_run.performance,
        )
    });
    let report = search_run.report;
    let mut pending_delivery =
        PendingSearchAdvisoryDelivery::new(search_advisory_session, &advisory_workspace_id);
    let timing =
        DaemonSearchTiming::from_trace(daemon_total_start.elapsed(), &search_run.performance);
    let mut method_result = {
        let mut advisory_session = search_advisory_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        DaemonSearchResult::from_report_for_delivery(
            &report,
            options.explain,
            &advisory_workspace_id,
            &mut advisory_session,
            pending_delivery.reservation_mut(),
            timing,
            performance,
        )
    };
    method_result.timing.daemon_total =
        DaemonSearchTimingMeasurement::from_duration(daemon_total_start.elapsed());
    let response_degraded_codes = daemon_search_degraded_codes(&method_result);
    let result = match serde_json::to_value(method_result) {
        Ok(result) => result,
        Err(error) => {
            return DaemonResponse::err(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
                DAEMON_SEARCH_EXECUTION_FAILED_CODE,
                format!("ee.daemon.search could not encode its response: {error}"),
            );
        }
    };
    let mut response = DaemonResponse::ok(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
        result,
    );
    for code in response_degraded_codes {
        response = response.with_degraded(code);
    }
    if !daemon_response_fits(&response, super::DAEMON_RESPONSE_MAX_BYTES) {
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_SEARCH_EXECUTION_FAILED_CODE,
            "ee.daemon.search response exceeded the daemon response cap; lower --limit or use canonical in-process search.",
        );
    }
    pending_delivery.finish(response, defer_advisory_until_socket_write)
}

fn daemon_response_fits(response: &DaemonResponse, max_bytes: usize) -> bool {
    serde_json::to_vec(response).is_ok_and(|encoded| encoded.len() <= max_bytes)
}

fn daemon_search_params_error(request: &DaemonRequest, message: &str) -> DaemonResponse {
    DaemonResponse::err(
        request.request_id.clone(),
        request.agent_id.clone(),
        request.workspace_id.clone(),
        DAEMON_SEARCH_PARAMS_INVALID_CODE,
        format!("invalid ee.daemon.search params: {message}"),
    )
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
            propose_candidates: optional_bool_any(
                object,
                &["proposeCandidates", "propose_candidates"],
            )?
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

    fn database_path(&self) -> PathBuf {
        self.workspace_path.join(".ee").join("ee.db")
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
        // Invariant: block_on always installs an ambient Cx for the closure.
        #[allow(clippy::expect_used)]
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

/// Parsed prevalidated outcome write payload for the daemon write-owner actor.
///
/// This is intentionally narrower than the public `ee outcome` CLI: callers
/// supply the already-normalized feedback row and audit id, and the actor owns
/// only the transaction/coalescing boundary. The ordinary CLI path continues to
/// do validation, quarantine decisions, and derived follow-up updates.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct DaemonOutcomeParams {
    workspace_path: PathBuf,
    event_id: String,
    workspace_id: String,
    target_type: String,
    target_id: String,
    signal: String,
    weight: f32,
    source_type: String,
    source_id: Option<String>,
    reason: Option<String>,
    evidence_json: Option<String>,
    session_id: Option<String>,
    actor: Option<String>,
    audit_id: String,
    details: Option<String>,
}

impl DaemonOutcomeParams {
    const ACTOR_OPERATION_TYPE: &'static str = "ee.daemon.outcome";

    fn from_payload(payload: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(payload.clone()).map_err(|error| error.to_string())
    }

    fn database_path(&self) -> PathBuf {
        self.workspace_path.join(".ee").join("ee.db")
    }

    fn into_parts(self) -> (String, crate::db::AuditedFeedbackEventInput, String) {
        let event_id = self.event_id;
        let audit_id = self.audit_id;
        let input = crate::db::AuditedFeedbackEventInput {
            event: crate::db::CreateFeedbackEventInput {
                workspace_id: self.workspace_id,
                target_type: self.target_type,
                target_id: self.target_id,
                signal: self.signal,
                weight: self.weight,
                source_type: self.source_type,
                source_id: self.source_id,
                reason: self.reason,
                evidence_json: self.evidence_json,
                session_id: self.session_id,
            },
            actor: self.actor,
            details: self.details,
        };
        (event_id, input, audit_id)
    }
}

enum DaemonTxnBatchEntry {
    Journal(DaemonJournalParams),
    Outcome(DaemonOutcomeParams),
    Remember(DaemonWriteParams),
}

enum PreparedDaemonTxnBatchEntry {
    Journal(DaemonJournalParams),
    Outcome(DaemonOutcomeParams),
    Remember(crate::core::memory::PreparedRememberTxnWrite),
}

impl DaemonTxnBatchEntry {
    fn database_path(&self) -> PathBuf {
        match self {
            Self::Journal(params) => params.database_path(),
            Self::Outcome(params) => params.database_path(),
            Self::Remember(params) => params.database_path(),
        }
    }
}

/// True when every op in the batch can share one daemon transaction.
fn batch_is_daemon_txn_coalescible(
    operations: &[crate::core::write_owner::WriteOperation],
) -> bool {
    use crate::core::write_owner::WriteOperation;
    !operations.is_empty()
        && operations.iter().all(|operation| {
            matches!(
                operation,
                WriteOperation::Custom { operation_type, .. }
                    if operation_type == DaemonJournalParams::ACTOR_OPERATION_TYPE
                        || operation_type == DaemonOutcomeParams::ACTOR_OPERATION_TYPE
                        || operation_type == DaemonWriteParams::ACTOR_OPERATION_TYPE
            )
        })
}

fn parse_daemon_txn_batch_entry(
    operation: &crate::core::write_owner::WriteOperation,
) -> Result<DaemonTxnBatchEntry, crate::models::DomainError> {
    use crate::core::write_owner::WriteOperation;
    let WriteOperation::Custom {
        operation_type,
        payload,
    } = operation
    else {
        return Err(crate::models::DomainError::Storage {
            message: "daemon transaction batch received a non-Custom operation".to_string(),
            repair: Some("only daemon journal/outcome/remember ops are batched here".to_string()),
        });
    };
    match operation_type.as_str() {
        DaemonJournalParams::ACTOR_OPERATION_TYPE => DaemonJournalParams::from_payload(payload)
            .map(DaemonTxnBatchEntry::Journal)
            .map_err(|message| crate::models::DomainError::Storage {
                message: format!("daemon journal payload decode failed: {message}"),
                repair: Some("retry the write".to_string()),
            }),
        DaemonOutcomeParams::ACTOR_OPERATION_TYPE => DaemonOutcomeParams::from_payload(payload)
            .map(DaemonTxnBatchEntry::Outcome)
            .map_err(|message| crate::models::DomainError::Storage {
                message: format!("daemon outcome payload decode failed: {message}"),
                repair: Some("retry the write".to_string()),
            }),
        DaemonWriteParams::ACTOR_OPERATION_TYPE => DaemonWriteParams::from_payload(payload)
            .map(DaemonTxnBatchEntry::Remember)
            .map_err(|message| crate::models::DomainError::Storage {
                message: format!("daemon remember payload decode failed: {message}"),
                repair: Some("retry the write".to_string()),
            }),
        _ => Err(crate::models::DomainError::Storage {
            message: format!("unsupported daemon transaction op: {operation_type}"),
            repair: Some("only daemon journal/outcome/remember ops are batched here".to_string()),
        }),
    }
}

/// Execute a homogeneous batch of `ee.daemon.journal` ops in ONE transaction
/// (Inc 3, bd-wx6ou.4): open the workspace DB connection once, insert all N
/// journal entries inside a single `with_transaction` so the whole batch shares
/// ONE commit boundary — one `FileWriteOwnerGuard` acquisition, one WAL commit
/// frame, one COMMIT round-trip — which is the coalescing win. (Under WAL
/// `synchronous=NORMAL` (`src/db/mod.rs:1709`), COMMIT does not fsync; the WAL
/// is synced only at checkpoint, so the saving is commit-boundary overhead, not
/// per-commit fsyncs. The daemon ACK is therefore app/process-crash-durable, not
/// power-loss-durable — see ADR 0077 and bd-d67os.16.) All ops target the
/// daemon's bound workspace, so one connection serves them. All-or-nothing: a
/// failed insert rolls back the batch and fails every request in it (the CLI
/// falls back to the direct path per Inc 4).
fn execute_journal_batch(
    operations: &[crate::core::write_owner::WriteOperation],
) -> Result<Vec<crate::core::write_owner::WriteResult>, crate::models::DomainError> {
    execute_daemon_txn_batch(operations)
}

fn execute_daemon_txn_batch(
    operations: &[crate::core::write_owner::WriteOperation],
) -> Result<Vec<crate::core::write_owner::WriteResult>, crate::models::DomainError> {
    use crate::core::write_owner::WriteResult;
    let mut entries = Vec::with_capacity(operations.len());
    for operation in operations {
        entries.push(parse_daemon_txn_batch_entry(operation)?);
    }
    let Some(first) = entries.first() else {
        return Ok(Vec::new());
    };
    let database_path = first.database_path();
    for entry in &entries {
        if entry.database_path() != database_path {
            return Err(crate::models::DomainError::Storage {
                message: "daemon transaction batch crossed database paths".to_string(),
                repair: Some("retry the writes separately".to_string()),
            });
        }
        if let DaemonTxnBatchEntry::Remember(params) = entry
            && let Some(error) = crate::core::memory::remember_level_kind_cross_wire_error(
                &params.level,
                &params.kind,
            )
        {
            return Err(error);
        }
    }
    // The daemon owns batching, not store creation. Preflight the exact path
    // before the create-capable database open so a queued write addressed at
    // a misspelled workspace cannot plant a new store as a side effect
    // (bd-workspace-miss-init-suggestion-sfjvq).
    crate::core::ensure_addressed_database_exists(&database_path)?;
    let connection = crate::db::DbConnection::open_file(&database_path).map_err(|error| {
        crate::models::DomainError::Storage {
            message: format!(
                "daemon transaction batch could not open {}: {error}",
                database_path.display()
            ),
            repair: Some("ensure the workspace is initialized".to_string()),
        }
    })?;
    let mut prepared_entries = Vec::with_capacity(entries.len());
    for entry in entries {
        match entry {
            DaemonTxnBatchEntry::Journal(entry) => {
                prepared_entries.push(PreparedDaemonTxnBatchEntry::Journal(entry));
            }
            DaemonTxnBatchEntry::Outcome(entry) => {
                prepared_entries.push(PreparedDaemonTxnBatchEntry::Outcome(entry));
            }
            DaemonTxnBatchEntry::Remember(entry) => {
                let write = crate::core::memory::prepare_remember_txn_write_for_connection(
                    &connection,
                    &entry.options(),
                    true,
                )?;
                prepared_entries.push(PreparedDaemonTxnBatchEntry::Remember(write));
            }
        }
    }
    let mut results = connection
        .with_transaction(|| {
            let mut out = Vec::with_capacity(prepared_entries.len());
            for entry in &prepared_entries {
                match entry {
                    PreparedDaemonTxnBatchEntry::Journal(entry) => {
                        crate::core::journal::ensure_workspace(
                            &connection,
                            &entry.workspace_id,
                            &entry.workspace_path,
                        )
                        .map_err(|error| {
                            crate::db::DbError::MalformedRow {
                                operation: crate::db::DbOperation::Execute,
                                message: error.message().to_string(),
                            }
                        })?;
                        let input = entry.clone().into_create_input();
                        let entry_id = input.entry_id.clone();
                        connection.insert_journal_entry(&input)?;
                        out.push(WriteResult::Success {
                            entity_id: Some(entry_id),
                        });
                    }
                    PreparedDaemonTxnBatchEntry::Outcome(entry) => {
                        let (event_id, input, audit_id) = entry.clone().into_parts();
                        crate::core::outcome::record_outcome_feedback_event_in_txn(
                            &connection,
                            &event_id,
                            &input,
                            audit_id,
                        )?;
                        out.push(WriteResult::Success {
                            entity_id: Some(event_id),
                        });
                    }
                    PreparedDaemonTxnBatchEntry::Remember(write) => {
                        crate::core::memory::record_prepared_remember_txn_write_in_txn(
                            &connection,
                            write,
                        )?;
                        out.push(WriteResult::Success {
                            entity_id: Some(write.memory_id().to_owned()),
                        });
                    }
                }
            }
            Ok(out)
        })
        .map_err(|error| crate::models::DomainError::Storage {
            message: format!("daemon transaction batch failed: {error}"),
            repair: Some("retry the write".to_string()),
        })?;

    let mut index_drains: BTreeMap<String, (String, PathBuf)> = BTreeMap::new();
    for (index, entry) in prepared_entries.into_iter().enumerate() {
        if let PreparedDaemonTxnBatchEntry::Remember(write) = entry {
            let index_dir = write.index_dir().to_path_buf();
            match crate::core::memory::finish_prepared_remember_txn_write(&connection, write) {
                Ok(report) => {
                    index_drains.insert(
                        report.workspace_id.clone(),
                        (report.workspace_id.clone(), index_dir),
                    );
                    results[index] = WriteResult::Success {
                        entity_id: Some(report.memory_id.to_string()),
                    };
                }
                Err(error) => {
                    results[index] = WriteResult::Failed { error };
                }
            }
        }
    }
    for (_, (workspace_id, index_dir)) in index_drains {
        match crate::core::memory::reconcile_pending_remember_index_jobs(
            &connection,
            &workspace_id,
            &index_dir,
        ) {
            Ok(None) => {}
            Ok(Some(report))
                if matches!(
                    report.outcome.as_str(),
                    "completed" | "completed_no_documents"
                ) => {}
            Ok(Some(report)) => {
                tracing::warn!(
                    target: "ee::daemon::write_owner",
                    workspace_id,
                    job_id = report.job_id.as_str(),
                    outcome = report.outcome.as_str(),
                    processing_mode = report.processing_mode.as_str(),
                    "daemon remember batch committed source rows while derived index reconciliation remained nonterminal"
                );
            }
            Err(error) => {
                // The transaction above is already durable. Rewriting every
                // successful journal/outcome/remember result as failed would
                // invite duplicate source writes on retry and falsely blame
                // unrelated operations for a derived-index problem. The
                // pending index jobs remain the recovery mechanism.
                tracing::warn!(
                    target: "ee::daemon::write_owner",
                    workspace_id,
                    error = %error,
                    "daemon remember batch committed source rows but could not reconcile the derived index; preserving durable write success"
                );
            }
        }
    }
    Ok(results)
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

    fn context_options(&self, authorized_workspace_id: &str) -> Result<ContextPackOptions, String> {
        let workspace_path = canonical_workspace_path(&self.workspace_path, "workspacePath")?;
        let authorized_workspace =
            canonical_workspace_path(Path::new(authorized_workspace_id), "workspace_id")?;
        if workspace_path != authorized_workspace {
            return Err(
                "field `workspacePath` must identify the authorized envelope `workspace_id`"
                    .to_owned(),
            );
        }
        let database_path = self
            .database_path
            .as_deref()
            .map(|path| canonical_contained_path(&workspace_path, path, "databasePath"))
            .transpose()?;
        let index_dir = self
            .index_dir
            .as_deref()
            .map(|path| canonical_contained_path(&workspace_path, path, "indexDir"))
            .transpose()?;
        Ok(ContextPackOptions {
            workspace_path,
            database_path,
            index_dir,
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
        })
    }
}

fn dispatch_context(
    request: &DaemonRequest,
    shutdown: &AtomicBool,
    search_advisory_session: &Mutex<SearchAdvisorySession>,
    defer_advisory_until_socket_write: bool,
) -> DaemonResponse {
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
    let Some(authorized_workspace_id) = request.workspace_id.as_deref() else {
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_CONTEXT_PARAMS_INVALID_CODE,
            "authorized envelope `workspace_id` is missing",
        );
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

    let options = match params.context_options(authorized_workspace_id) {
        Ok(options) => options,
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
    let advisory_workspace_id =
        crate::core::workspace::stable_workspace_id(&options.workspace_path);
    let deadline = params.timeout_ms.map(Duration::from_millis);
    let context_run = match run_context_pack_with_performance_controlled(
        &options,
        "pack",
        deadline,
        Some(shutdown),
    ) {
        Ok(run) => run,
        Err(ContextPackError::DeadlineExceeded(error)) => {
            return DaemonResponse::err(
                request.request_id.clone(),
                request.agent_id.clone(),
                request.workspace_id.clone(),
                DAEMON_CONTEXT_DEADLINE_EXCEEDED_CODE,
                format!(
                    "ee.daemon.context deadline expired: {}",
                    crate::core::outcome::cancel_message(&error)
                ),
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
    let mut context_response = context_run.response;
    let context_search_report = context_run.search_report;
    let context_search_advisory_snapshot = context_run.search_advisory_snapshot;

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
    let mut pending_delivery =
        PendingSearchAdvisoryDelivery::new(search_advisory_session, &advisory_workspace_id);
    let mut result = result;
    {
        let mut advisory_session = search_advisory_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        attach_daemon_context_search_advisories_for_delivery(
            &mut result,
            context_search_report.as_ref(),
            &context_search_advisory_snapshot,
            &mut advisory_session,
            &advisory_workspace_id,
            pending_delivery.reservation_mut(),
        );
    }
    if daemon_context_deadline_expired(context_started, params.timeout_ms) {
        return daemon_context_deadline_response(
            request,
            "ee.daemon.context deadline expired while finalizing degradations.",
        );
    }
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
    if !daemon_response_fits(&response, super::DAEMON_RESPONSE_MAX_BYTES) {
        return DaemonResponse::err(
            request.request_id.clone(),
            request.agent_id.clone(),
            request.workspace_id.clone(),
            DAEMON_CONTEXT_EXECUTION_FAILED_CODE,
            format!(
                "ee.daemon.context response exceeded the {}-byte daemon response cap; lower maxTokens or use the in-process CLI pack path.",
                super::DAEMON_RESPONSE_MAX_BYTES
            ),
        );
    }
    pending_delivery.finish(response, defer_advisory_until_socket_write)
}

fn attach_daemon_context_search_advisories_for_delivery(
    response: &mut serde_json::Value,
    search_report: Option<&SearchReport>,
    search_advisory_snapshot: &ContextSearchAdvisorySnapshot,
    session: &mut SearchAdvisorySession,
    workspace_id: &str,
    reservation: &mut SearchAdvisoryDeliveryReservation,
) {
    if let Some(search_report) = search_report {
        attach_context_search_advisories_for_delivery(
            response,
            search_report,
            session,
            workspace_id,
            reservation,
        );
    } else {
        attach_context_cached_search_advisories_for_delivery(
            response,
            search_advisory_snapshot,
            session,
            workspace_id,
            reservation,
        );
    }
}

fn filter_context_large_gap_advisory_for_delivery(
    response: &mut serde_json::Value,
    session: &mut SearchAdvisorySession,
    workspace_id: &str,
    reservation: &mut SearchAdvisoryDeliveryReservation,
) {
    filter_context_large_gap_advisory_inner(response, session, workspace_id, Some(reservation));
}

fn filter_context_large_gap_advisory_inner(
    response: &mut serde_json::Value,
    session: &mut SearchAdvisorySession,
    workspace_id: &str,
    reservation: Option<&mut SearchAdvisoryDeliveryReservation>,
) {
    const CODE: &str = "search_index_large_gap";
    let has_large_gap = ["/degraded", "/data/degraded"].into_iter().any(|pointer| {
        response
            .pointer(pointer)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry.get("code").and_then(serde_json::Value::as_str) == Some(CODE)
                })
            })
    });
    let emit_large_gap = match reservation {
        Some(reservation) => session.emit_large_gap_while_active_for_delivery(
            workspace_id,
            has_large_gap,
            reservation,
        ),
        None => session.emit_large_gap_while_active(workspace_id, has_large_gap),
    }
    .should_emit();
    if emit_large_gap {
        return;
    }
    if !has_large_gap {
        return;
    }
    for pointer in ["/degraded", "/data/degraded"] {
        if let Some(entries) = response
            .pointer_mut(pointer)
            .and_then(serde_json::Value::as_array_mut)
        {
            entries.retain(|entry| {
                entry.get("code").and_then(serde_json::Value::as_str) != Some(CODE)
            });
        }
    }
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
    match value {
        "instant" => Ok(SpeedMode::Instant),
        "default" => Ok(SpeedMode::Default),
        "quality" => Ok(SpeedMode::Quality),
        _ => Err(format!(
            "Invalid speed mode `{value}`. Expected instant, default, or quality."
        )),
    }
}

fn parse_daemon_source_mode(value: &str) -> Result<SearchSourceMode, String> {
    match value {
        "lexical_only" => Ok(SearchSourceMode::LexicalOnly),
        "semantic_only" => Ok(SearchSourceMode::SemanticOnly),
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
        METHOD_CONTEXT | METHOD_SEARCH | METHOD_WRITE | METHOD_WRITE_JOURNAL => {
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
                && !workspace_ids_match(workspace_id, bound_workspace_id)
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

fn workspace_ids_match(requested: &str, bound: &str) -> bool {
    if requested == bound {
        return true;
    }
    let requested = Path::new(requested);
    let bound = Path::new(bound);
    fs::canonicalize(requested)
        .ok()
        .zip(fs::canonicalize(bound).ok())
        .is_some_and(|(requested, bound)| requested == bound)
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
            METHOD_SEARCH,
            METHOD_SHUTDOWN,
            METHOD_TELEMETRY,
            METHOD_WRITE,
            METHOD_WRITE_JOURNAL
        ],
        "authorization": {
            "ee.daemon.capabilities": daemon_method_authority(METHOD_CAPABILITIES).expect("registered method").as_wire_label(),
            "ee.daemon.context": daemon_method_authority(METHOD_CONTEXT).expect("registered method").as_wire_label(),
            "ee.daemon.echo": daemon_method_authority(METHOD_ECHO).expect("registered method").as_wire_label(),
            "ee.daemon.search": daemon_method_authority(METHOD_SEARCH).expect("registered method").as_wire_label(),
            "ee.daemon.shutdown": daemon_method_authority(METHOD_SHUTDOWN).expect("registered method").as_wire_label(),
            "ee.daemon.telemetry": daemon_method_authority(METHOD_TELEMETRY).expect("registered method").as_wire_label(),
            "ee.daemon.write": daemon_method_authority(METHOD_WRITE).expect("registered method").as_wire_label(),
            "ee.daemon.write_journal": daemon_method_authority(METHOD_WRITE_JOURNAL).expect("registered method").as_wire_label()
        },
        "method_schemas": {
            "ee.daemon.search": {
                "request": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
                "response": DAEMON_SEARCH_RESPONSE_SCHEMA_V3
            }
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
    client_round_trip_before(
        socket_path,
        request,
        Instant::now() + DAEMON_DEFAULT_RPC_TIMEOUT,
    )
}

/// Apply a caller-owned cumulative deadline to one daemon round-trip's framed
/// I/O. The deadline may be shared across capability negotiation and the
/// method call so per-request socket timeouts cannot accidentally multiply it.
/// The CLI additionally bounds the whole worker attempt, including connect.
pub fn client_round_trip_before(
    socket_path: &Path,
    request: &DaemonRequest,
    deadline: Instant,
) -> Result<DaemonResponse, ClientError> {
    ensure_client_deadline(deadline)?;
    let mut stream = UnixStream::connect(socket_path).map_err(ClientError::Connect)?;
    set_client_deadline(&stream, deadline)?;

    let body = serde_json::to_vec(request).map_err(ClientError::Encode)?;
    use std::io::Write;
    let length = u32::try_from(body.len())
        .map_err(|_| ClientError::RequestTooLarge { actual: body.len() })?;
    set_client_deadline(&stream, deadline)?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(|error| client_io_error(error, deadline))?;
    set_client_deadline(&stream, deadline)?;
    stream
        .write_all(&body)
        .map_err(|error| client_io_error(error, deadline))?;
    set_client_deadline(&stream, deadline)?;
    stream
        .flush()
        .map_err(|error| client_io_error(error, deadline))?;

    // Read the response with the same frame shape.
    let mut response_prefix = [0_u8; 4];
    use std::io::Read;
    set_client_deadline(&stream, deadline)?;
    stream
        .read_exact(&mut response_prefix)
        .map_err(|error| client_io_error(error, deadline))?;
    let announced = u32::from_be_bytes(response_prefix);
    let announced_usize =
        usize::try_from(announced).map_err(|_| ClientError::ResponseTooLarge { announced })?;
    if announced_usize > super::DAEMON_RESPONSE_MAX_BYTES {
        return Err(ClientError::ResponseTooLarge { announced });
    }
    let mut buffer = vec![0_u8; announced_usize];
    set_client_deadline(&stream, deadline)?;
    stream
        .read_exact(&mut buffer)
        .map_err(|error| client_io_error(error, deadline))?;
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

fn ensure_client_deadline(deadline: Instant) -> Result<Duration, ClientError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(ClientError::DeadlineExceeded)
}

fn set_client_deadline(stream: &UnixStream, deadline: Instant) -> Result<(), ClientError> {
    let remaining = ensure_client_deadline(deadline)?;
    stream
        .set_read_timeout(Some(remaining))
        .map_err(ClientError::Io)?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(ClientError::Io)
}

fn client_io_error(error: io::Error, deadline: Instant) -> ClientError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) || Instant::now() >= deadline
    {
        ClientError::DeadlineExceeded
    } else {
        ClientError::Io(error)
    }
}

/// Errors that can occur on the client side of a UDS round-trip.
#[derive(Debug)]
pub enum ClientError {
    DeadlineExceeded,
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
            Self::DeadlineExceeded => write!(formatter, "daemon round-trip deadline exceeded"),
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
            Self::DeadlineExceeded => None,
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
    use crate::core::search::MAX_SEARCH_ADVISORY_WORKSPACES;
    use crate::daemon::protocol::DaemonResponseDelivery;

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

    fn search_request(params: serde_json::Value) -> DaemonRequest {
        let mut request = DaemonRequest::new(
            "req-search-contract-001",
            TEST_AGENT_ID,
            METHOD_SEARCH,
            params,
        );
        request.workspace_id = Some("/tmp/ee-daemon-search-contract".to_owned());
        request
    }

    #[test]
    fn daemon_response_cap_rejects_undeliverable_candidate() {
        let response = DaemonResponse::ok(
            "req-cap-001",
            TEST_AGENT_ID,
            Some(TEST_WORKSPACE_ID.to_owned()),
            serde_json::json!({"payload": "too large for a one-byte cap"}),
        );

        assert!(!daemon_response_fits(&response, 1));
        assert!(daemon_response_fits(&response, usize::MAX));
    }

    fn permanent_reranker_advisory_report() -> SearchReport {
        SearchReport {
            index_freshness: None,
            status: crate::core::search::SearchStatus::NoResults,
            embed_backend: crate::models::EmbedBackend::HashFallback,
            query: "release".to_owned(),
            requested_limit: 10,
            results: Vec::new(),
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: vec![crate::core::search::SearchDegradation {
                code: "rerank_model_unavailable".to_owned(),
                severity: "warning".to_owned(),
                message: crate::core::search::RERANK_MODEL_UNAVAILABLE_ADVISORY.to_owned(),
                repair: None,
            }],
            runtime_profile: crate::core::profile::RuntimeProfileReport::for_profile(
                crate::core::profile::OperatingProfile::Workstation,
                "daemon_advisory_delivery_test",
            ),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            relevance_floor_applied: Some(0.0),
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: crate::models::MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        }
    }

    fn stale_index_advisory_report(db_generation: u64, index_generation: u64) -> SearchReport {
        let mut report = permanent_reranker_advisory_report();
        report.index_freshness = Some(crate::core::search::SearchIndexFreshness {
            stale: true,
            db_generation: Some(db_generation),
            index_generation: Some(index_generation),
            generation_gap: Some(db_generation.saturating_sub(index_generation)),
            large_gap: db_generation.saturating_sub(index_generation)
                > crate::core::search::SEARCH_INDEX_LARGE_GAP_THRESHOLD,
        });
        report.degraded = vec![
            crate::core::search::SearchDegradation {
                code: "search_index_stale".to_owned(),
                severity: "medium".to_owned(),
                message: format!(
                    "Search index is stale. Database generation is {db_generation}; index generation is {index_generation}."
                ),
                repair: Some("ee index rebuild --workspace .".to_owned()),
            },
            crate::core::search::SearchDegradation {
                code: "search_index_large_gap".to_owned(),
                severity: "medium".to_owned(),
                message: format!(
                    "Search index generation gap is {}; automatic read repair was skipped.",
                    db_generation.saturating_sub(index_generation)
                ),
                repair: Some("ee index rebuild --workspace .".to_owned()),
            },
        ];
        report
    }

    fn advisory_delivery_candidate(
        report: &SearchReport,
        policy: &DaemonDispatchPolicy,
        workspace_id: &str,
    ) -> (DaemonResponse, serde_json::Value) {
        let mut pending =
            PendingSearchAdvisoryDelivery::new(policy.search_advisory_session(), workspace_id);
        let result = {
            let mut session = policy
                .search_advisory_session()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            serde_json::to_value(DaemonSearchResult::from_report_for_delivery(
                report,
                false,
                workspace_id,
                &mut session,
                pending.reservation_mut(),
                DaemonSearchTiming::from_trace(
                    Duration::from_millis(1),
                    &SearchPerformanceTrace::default(),
                ),
                None,
            ))
            .expect("encode method result")
        };
        let response = DaemonResponse::ok(
            "req-advisory-delivery",
            TEST_AGENT_ID,
            Some(workspace_id.to_owned()),
            result.clone(),
        );
        (pending.finish(response, true), result)
    }

    fn context_advisory_delivery_candidate(
        report: &SearchReport,
        policy: &DaemonDispatchPolicy,
        workspace_id: &str,
    ) -> (DaemonResponse, serde_json::Value) {
        let mut pending =
            PendingSearchAdvisoryDelivery::new(policy.search_advisory_session(), workspace_id);
        let mut result = serde_json::json!({
            "schema": crate::models::RESPONSE_SCHEMA_V2,
            "success": true,
            "data": {"command": "pack", "degraded": []},
            "degraded": [],
        });
        {
            let mut session = policy
                .search_advisory_session()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            attach_context_search_advisories_for_delivery(
                &mut result,
                report,
                &mut session,
                workspace_id,
                pending.reservation_mut(),
            );
        }
        let response = DaemonResponse::ok(
            "req-context-advisory-delivery",
            TEST_AGENT_ID,
            Some(workspace_id.to_owned()),
            result.clone(),
        );
        (pending.finish(response, true), result)
    }

    fn cached_context_advisory_delivery_candidate(
        snapshot: &ContextSearchAdvisorySnapshot,
        policy: &DaemonDispatchPolicy,
        workspace_id: &str,
    ) -> (DaemonResponse, serde_json::Value) {
        let mut pending =
            PendingSearchAdvisoryDelivery::new(policy.search_advisory_session(), workspace_id);
        let mut result = serde_json::json!({
            "schema": crate::models::RESPONSE_SCHEMA_V2,
            "success": true,
            "data": {"command": "pack", "degraded": []},
            "degraded": [],
        });
        {
            let mut session = policy
                .search_advisory_session()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            attach_daemon_context_search_advisories_for_delivery(
                &mut result,
                None,
                snapshot,
                &mut session,
                workspace_id,
                pending.reservation_mut(),
            );
        }
        let response = DaemonResponse::ok(
            "req-cached-context-advisory-delivery",
            TEST_AGENT_ID,
            Some(workspace_id.to_owned()),
            result.clone(),
        );
        (pending.finish(response, true), result)
    }

    #[test]
    fn successful_socket_delivery_consumes_permanent_advisory() {
        let report = permanent_reranker_advisory_report();
        let policy = DaemonDispatchPolicy::for_workspace(TEST_WORKSPACE_ID);
        let (mut delivered_response, delivered_result) =
            advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert_eq!(
            delivered_result
                .pointer("/response/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable")
        );
        let (mut server_side, mut client_side) = UnixStream::pair().expect("socketpair");
        let reader = thread::spawn(move || read_framed_daemon_response(&mut client_side));
        assert!(write_and_settle_daemon_response(
            &mut server_side,
            &policy,
            &mut delivered_response,
        ));
        assert!(delivered_response.delivery.is_none());
        let wire_response = reader.join().expect("socket reader must not panic");
        assert_eq!(wire_response.result, Some(delivered_result));

        let (mut repeated_response, repeated_result) =
            advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert!(repeated_response.delivery.is_some());
        assert!(
            repeated_result
                .pointer("/response/data/rerank/advisory")
                .is_some_and(serde_json::Value::is_null)
        );
        assert_eq!(
            repeated_result
                .pointer("/response/data/rerank/advisorySummary/sessionSuppressedCount")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        settle_daemon_response_delivery(&policy, &mut repeated_response, true);
    }

    #[test]
    fn disconnected_socket_does_not_consume_permanent_advisory() {
        let report = permanent_reranker_advisory_report();
        let policy = DaemonDispatchPolicy::for_workspace(TEST_WORKSPACE_ID);
        let (mut failed_response, failed_result) =
            advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert!(failed_response.error.is_none());
        assert_eq!(
            failed_result
                .pointer("/response/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable")
        );
        let (mut server_side, client_side) = UnixStream::pair().expect("socketpair");
        drop(client_side);
        assert!(!write_and_settle_daemon_response(
            &mut server_side,
            &policy,
            &mut failed_response,
        ));
        assert!(failed_response.delivery.is_none());

        let (retry_response, retry_result) =
            advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert!(retry_response.delivery.is_some());
        assert_eq!(
            retry_result
                .pointer("/response/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable")
        );
    }

    #[test]
    fn disconnected_context_socket_does_not_consume_permanent_advisory() {
        let report = permanent_reranker_advisory_report();
        let policy = DaemonDispatchPolicy::for_workspace(TEST_WORKSPACE_ID);
        let (mut failed_response, failed_result) =
            context_advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert_eq!(
            failed_result
                .pointer("/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable")
        );
        let (mut server_side, client_side) = UnixStream::pair().expect("socketpair");
        drop(client_side);
        assert!(!write_and_settle_daemon_response(
            &mut server_side,
            &policy,
            &mut failed_response,
        ));

        let (retry_response, retry_result) =
            context_advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert!(retry_response.delivery.is_some());
        assert_eq!(
            retry_result
                .pointer("/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable")
        );
    }

    #[test]
    fn cached_context_snapshot_uses_socket_settlement_and_shared_once_ledger() {
        let report = permanent_reranker_advisory_report();
        let snapshot = ContextSearchAdvisorySnapshot::from_search_report(&report);
        let policy = DaemonDispatchPolicy::for_workspace(TEST_WORKSPACE_ID);
        let (mut failed_response, failed_result) = cached_context_advisory_delivery_candidate(
            &snapshot,
            &policy,
            TEST_WORKSPACE_ID,
        );
        assert_eq!(
            failed_result
                .pointer("/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable")
        );
        let (mut failed_server, failed_client) = UnixStream::pair().expect("socketpair");
        drop(failed_client);
        assert!(!write_and_settle_daemon_response(
            &mut failed_server,
            &policy,
            &mut failed_response,
        ));

        let (mut retry_response, retry_result) = cached_context_advisory_delivery_candidate(
            &snapshot,
            &policy,
            TEST_WORKSPACE_ID,
        );
        assert_eq!(
            retry_result
                .pointer("/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable"),
            "failed cached delivery must preserve the permanent advisory"
        );
        let (mut retry_server, mut retry_client) = UnixStream::pair().expect("socketpair");
        let retry_reader = thread::spawn(move || read_framed_daemon_response(&mut retry_client));
        assert!(write_and_settle_daemon_response(
            &mut retry_server,
            &policy,
            &mut retry_response,
        ));
        assert_eq!(
            retry_reader.join().expect("cached retry reader").result,
            Some(retry_result)
        );

        let (mut fresh_response, fresh_result) =
            context_advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert!(
            fresh_result
                .pointer("/data/rerank/advisory")
                .is_some_and(serde_json::Value::is_null),
            "fresh and cached context paths must share one process ledger"
        );
        settle_daemon_response_delivery(&policy, &mut fresh_response, true);
    }

    #[test]
    fn partial_socket_delivery_does_not_consume_permanent_advisory() {
        use std::io::Read;
        use std::net::Shutdown;

        let report = permanent_reranker_advisory_report();
        let policy = DaemonDispatchPolicy::for_workspace(TEST_WORKSPACE_ID);
        let (mut partial_response, _) =
            advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert!(partial_response.error.is_none());
        partial_response.result = Some(serde_json::json!({
            "payload": "x".repeat(3 * 1024 * 1024),
        }));
        let (mut server_side, mut client_side) = UnixStream::pair().expect("socketpair");
        server_side
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("server write timeout");
        let reader = thread::spawn(move || {
            let mut prefix = [0_u8; 4];
            client_side
                .read_exact(&mut prefix)
                .expect("length prefix must arrive");
            let announced = u32::from_be_bytes(prefix) as usize;
            let mut partial = [0_u8; 16 * 1024];
            client_side
                .read_exact(&mut partial)
                .expect("partial body must arrive");
            client_side
                .shutdown(Shutdown::Both)
                .expect("disconnect partial reader");
            (announced, partial.len())
        });

        assert!(!write_and_settle_daemon_response(
            &mut server_side,
            &policy,
            &mut partial_response,
        ));
        let (announced, observed) = reader.join().expect("partial reader must not panic");
        assert!(announced > observed, "fixture must disconnect mid-frame");
        assert!(partial_response.delivery.is_none());

        let (retry_response, retry_result) =
            advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert!(retry_response.delivery.is_some());
        assert_eq!(
            retry_result
                .pointer("/response/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable")
        );
    }

    #[test]
    fn concurrent_socket_capacity_fail_open_settles_all_first_advisories() {
        const WORKSPACES: usize = 65;

        let report = stale_index_advisory_report(200, 1);
        let policy = DaemonDispatchPolicy::default();
        let barrier = Arc::new(std::sync::Barrier::new(WORKSPACES + 1));
        let workers = (0..WORKSPACES)
            .map(|index| {
                let report = report.clone();
                let policy = policy.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let workspace_id = format!("capacity-workspace-{index:03}");
                    let (response, result) =
                        advisory_delivery_candidate(&report, &policy, &workspace_id);
                    (workspace_id, response, result)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let mut outcomes = workers
            .into_iter()
            .map(|worker| worker.join().expect("capacity reservation worker"))
            .collect::<Vec<_>>();

        for (_, response, result) in &outcomes {
            assert!(response.delivery.is_some());
            let codes = result["response"]["data"]["degraded"]
                .as_array()
                .expect("first response degraded array")
                .iter()
                .filter_map(|entry| entry["code"].as_str())
                .collect::<std::collections::BTreeSet<_>>();
            assert!(codes.contains("search_index_stale"));
            assert!(codes.contains("search_index_large_gap"));
            assert_eq!(
                result["response"]["data"]["indexFreshness"]["largeGap"],
                true
            );
        }

        let capacity_busy_indices = outcomes
            .iter()
            .enumerate()
            .filter_map(|(index, (_, response, _))| {
                response
                    .delivery
                    .as_ref()
                    .is_some_and(DaemonResponseDelivery::search_large_gap_capacity_busy)
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(capacity_busy_indices.len(), 1);
        let capacity_busy_index = capacity_busy_indices[0];
        let capacity_busy_workspace = outcomes[capacity_busy_index].0.clone();

        let release_index = outcomes
            .iter()
            .enumerate()
            .find(|(index, _)| *index != capacity_busy_index)
            .map(|(index, _)| index)
            .expect("one ordinary reservation to settle first");
        let (_, mut release_response, release_result) = outcomes.swap_remove(release_index);
        let (mut release_server, mut release_client) = UnixStream::pair().expect("socketpair");
        let release_reader =
            thread::spawn(move || read_framed_daemon_response(&mut release_client));
        assert!(write_and_settle_daemon_response(
            &mut release_server,
            &policy,
            &mut release_response,
        ));
        assert_eq!(
            release_reader.join().expect("release socket reader").result,
            Some(release_result)
        );

        let failed_workspace = outcomes
            .iter()
            .find(|(workspace_id, response, _)| {
                workspace_id != &capacity_busy_workspace
                    && response
                        .delivery
                        .as_ref()
                        .is_some_and(|delivery| !delivery.search_large_gap_capacity_busy())
            })
            .map(|(workspace_id, _, _)| workspace_id.clone())
            .expect("one ordinary reservation must exercise failed delivery");
        let delivery_workers = outcomes
            .into_iter()
            .map(|(workspace_id, mut response, expected_result)| {
                let policy = policy.clone();
                let failed_workspace = failed_workspace.clone();
                thread::spawn(move || {
                    let (mut server_side, mut client_side) =
                        UnixStream::pair().expect("socketpair");
                    if workspace_id == failed_workspace {
                        drop(client_side);
                        let delivered = write_and_settle_daemon_response(
                            &mut server_side,
                            &policy,
                            &mut response,
                        );
                        return (workspace_id, delivered);
                    }
                    let reader =
                        thread::spawn(move || read_framed_daemon_response(&mut client_side));
                    let delivered =
                        write_and_settle_daemon_response(&mut server_side, &policy, &mut response);
                    let wire = reader.join().expect("capacity socket reader");
                    assert_eq!(wire.result, Some(expected_result));
                    (workspace_id, delivered)
                })
            })
            .collect::<Vec<_>>();
        let deliveries = delivery_workers
            .into_iter()
            .map(|worker| worker.join().expect("capacity delivery worker"))
            .collect::<Vec<_>>();
        assert!(deliveries.iter().all(|(workspace_id, delivered)| {
            *delivered == (workspace_id != &failed_workspace)
        }));

        let (mut retry_response, retry_result) =
            advisory_delivery_candidate(&report, &policy, &failed_workspace);
        assert!(retry_response.delivery.is_some());
        assert!(
            retry_result["response"]["data"]["degraded"]
                .as_array()
                .expect("failed-delivery retry degraded array")
                .iter()
                .any(|entry| entry["code"] == "search_index_large_gap")
        );
        let (mut retry_server, mut retry_client) = UnixStream::pair().expect("socketpair");
        let retry_reader = thread::spawn(move || read_framed_daemon_response(&mut retry_client));
        assert!(write_and_settle_daemon_response(
            &mut retry_server,
            &policy,
            &mut retry_response,
        ));
        assert_eq!(
            retry_reader.join().expect("retry socket reader").result,
            Some(retry_result)
        );

        for workspace_id in [&capacity_busy_workspace, &failed_workspace] {
            let (mut repeated_response, repeated_result) =
                advisory_delivery_candidate(&report, &policy, workspace_id);
            assert!(repeated_response.delivery.is_some());
            assert!(
                repeated_result["response"]["data"]["degraded"]
                    .as_array()
                    .expect("settled workspace repeat degraded array")
                    .iter()
                    .all(|entry| {
                        !matches!(
                            entry["code"].as_str(),
                            Some("search_index_stale" | "search_index_large_gap")
                        )
                    })
            );
            settle_daemon_response_delivery(&policy, &mut repeated_response, true);
        }
        assert_eq!(
            policy
                .search_advisory_session()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tracked_workspace_count(),
            MAX_SEARCH_ADVISORY_WORKSPACES
        );
    }

    #[test]
    fn unbound_daemon_concurrent_workspace_reservations_share_one_process_winner() {
        const THREADS_PER_WORKSPACE: usize = 8;

        let report = permanent_reranker_advisory_report();
        let policy = DaemonDispatchPolicy::default();
        assert!(policy.bound_workspace_id().is_none());
        let barrier = Arc::new(std::sync::Barrier::new(THREADS_PER_WORKSPACE * 2 + 1));
        let progress_gate = Arc::new(std::sync::Barrier::new(3));
        let (progress_tx, progress_rx) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for workspace_id in ["workspace-a", "workspace-b"] {
            for worker_index in 0..THREADS_PER_WORKSPACE {
                let report = report.clone();
                let policy = policy.clone();
                let barrier = Arc::clone(&barrier);
                let progress_gate = Arc::clone(&progress_gate);
                let progress_tx = progress_tx.clone();
                workers.push(thread::spawn(move || {
                    barrier.wait();
                    let (response, result) =
                        advisory_delivery_candidate(&report, &policy, workspace_id);
                    if worker_index == 0 {
                        progress_tx
                            .send(workspace_id)
                            .expect("cross-workspace progress receiver must remain live");
                        progress_gate.wait();
                    }
                    (workspace_id, response, result)
                }));
            }
        }
        drop(progress_tx);
        barrier.wait();
        let progressed_workspaces = [
            progress_rx
                .recv()
                .expect("workspace-a or workspace-b must make reservation progress"),
            progress_rx
                .recv()
                .expect("both workspaces must make reservation progress"),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        progress_gate.wait();
        assert_eq!(
            progressed_workspaces,
            ["workspace-a", "workspace-b"].into_iter().collect(),
            "both workspaces must complete a reservation while the concurrent wave is still in flight"
        );
        let mut outcomes = workers
            .into_iter()
            .map(|worker| worker.join().expect("reservation worker must not panic"))
            .collect::<Vec<_>>();

        for workspace_id in ["workspace-a", "workspace-b"] {
            let workspace_outcomes = outcomes
                .iter()
                .filter(|(actual, _, _)| *actual == workspace_id)
                .collect::<Vec<_>>();
            assert_eq!(workspace_outcomes.len(), THREADS_PER_WORKSPACE);
            assert!(
                workspace_outcomes
                    .iter()
                    .all(|(_, response, _)| response.error.is_none())
            );
        }
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, _, result)| {
                    result
                        .pointer("/response/data/rerank/advisory/code")
                        .and_then(serde_json::Value::as_str)
                        == Some("rerank_model_unavailable")
                })
                .count(),
            1,
            "one process-wide permanent advisory identity must have one winner across all workspaces"
        );
        for (_, response, result) in &outcomes {
            assert!(
                response.delivery.is_some(),
                "every provisional occurrence must carry socket settlement"
            );
            let advisory = result
                .pointer("/response/data/rerank/advisory")
                .expect("rerank advisory field must remain present");
            if advisory.is_object() {
                assert_eq!(
                    advisory.get("code").and_then(serde_json::Value::as_str),
                    Some("rerank_model_unavailable"),
                    "the process-wide winner must own the permanent advisory"
                );
            } else {
                assert!(
                    advisory.is_null(),
                    "every competing process-wide nonwinner must carry rerank.advisory=null"
                );
            }
        }
        for (_, response, _) in &mut outcomes {
            settle_daemon_response_delivery(&policy, response, true);
        }

        let (mut repeated_response_a, repeated_a) =
            advisory_delivery_candidate(&report, &policy, "workspace-a");
        let (mut repeated_response_b, repeated_b) =
            advisory_delivery_candidate(&report, &policy, "workspace-b");
        for repeated in [&repeated_a, &repeated_b] {
            assert!(
                repeated
                    .pointer("/response/data/rerank/advisory")
                    .is_some_and(serde_json::Value::is_null)
            );
        }
        settle_daemon_response_delivery(&policy, &mut repeated_response_a, true);
        settle_daemon_response_delivery(&policy, &mut repeated_response_b, true);
    }

    #[test]
    fn same_workspace_concurrent_reservations_suppress_duplicates_without_errors() {
        const THREADS: usize = 16;

        let report = permanent_reranker_advisory_report();
        let policy = DaemonDispatchPolicy::for_workspace(TEST_WORKSPACE_ID);
        let barrier = Arc::new(std::sync::Barrier::new(THREADS + 1));
        let workers = (0..THREADS)
            .map(|_| {
                let report = report.clone();
                let policy = policy.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let mut outcomes = workers
            .into_iter()
            .map(|worker| worker.join().expect("reservation worker must not panic"))
            .collect::<Vec<_>>();
        assert!(
            outcomes
                .iter()
                .all(|(response, _)| response.error.is_none())
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, result)| {
                    result
                        .pointer("/response/data/rerank/advisory/code")
                        .and_then(serde_json::Value::as_str)
                        == Some("rerank_model_unavailable")
                })
                .count(),
            1,
            "only one thread may own the same-workspace delivery reservation"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|(response, _)| response.delivery.is_some())
                .count(),
            THREADS,
            "suppressed occurrences also require delivery settlement"
        );
        for (response, result) in &outcomes {
            assert!(response.delivery.is_some());
            let advisory = result
                .pointer("/response/data/rerank/advisory")
                .expect("rerank advisory field must remain present");
            if advisory.is_object() {
                assert_eq!(
                    advisory.get("code").and_then(serde_json::Value::as_str),
                    Some("rerank_model_unavailable"),
                    "the reservation winner must own the advisory"
                );
            } else {
                assert!(
                    advisory.is_null(),
                    "every competing same-workspace nonwinner must carry rerank.advisory=null"
                );
            }
        }
        let winner_index = outcomes
            .iter()
            .position(|(_, result)| {
                result
                    .pointer("/response/data/rerank/advisory/code")
                    .and_then(serde_json::Value::as_str)
                    == Some("rerank_model_unavailable")
            })
            .expect("one response must own the advisory reservation");
        for (index, (response, _)) in outcomes.iter_mut().enumerate() {
            settle_daemon_response_delivery(&policy, response, index != winner_index);
        }
        let (mut retry_response, retry_result) =
            advisory_delivery_candidate(&report, &policy, TEST_WORKSPACE_ID);
        assert_eq!(
            retry_result
                .pointer("/response/data/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable"),
            "a failed delivery must release the advisory for the next response"
        );
        settle_daemon_response_delivery(&policy, &mut retry_response, true);
    }

    #[test]
    fn large_gap_warning_is_once_but_daemon_structured_freshness_remains() {
        let first_report = stale_index_advisory_report(108, 1);
        let changed_generation_report = stale_index_advisory_report(112, 2);
        let policy = DaemonDispatchPolicy::for_workspace(TEST_WORKSPACE_ID);

        let (mut failed_response, failed_result) =
            advisory_delivery_candidate(&first_report, &policy, TEST_WORKSPACE_ID);
        assert!(
            failed_result
                .pointer("/response/data/degraded")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|entries| entries.iter().any(|entry| {
                    entry.get("code").and_then(serde_json::Value::as_str)
                        == Some("search_index_large_gap")
                }))
        );
        settle_daemon_response_delivery(&policy, &mut failed_response, false);

        let (mut delivered_response, delivered_result) =
            advisory_delivery_candidate(&first_report, &policy, TEST_WORKSPACE_ID);
        assert!(daemon_response_fits(&delivered_response, usize::MAX));
        assert!(
            delivered_result
                .get("human")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|human| {
                    human.contains("search_index_stale") && human.contains("search_index_large_gap")
                })
        );
        assert!(
            delivered_result
                .pointer("/response/degraded")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|entries| entries.iter().any(|entry| {
                    entry.get("code").and_then(serde_json::Value::as_str)
                        == Some("search_index_large_gap")
                }))
        );
        settle_daemon_response_delivery(&policy, &mut delivered_response, true);

        let (_, repeated_value) =
            advisory_delivery_candidate(&changed_generation_report, &policy, TEST_WORKSPACE_ID);
        let repeated_result = DaemonSearchResult::from_value(repeated_value)
            .expect("repeated result must remain a valid success response");
        assert!(daemon_search_degraded_codes(&repeated_result).is_empty());
        assert!(repeated_result.human.contains("No results for \"release\""));
        assert!(!repeated_result.human.contains("search_index_stale"));
        assert!(!repeated_result.human.contains("search_index_large_gap"));
        assert_eq!(repeated_result.response["degraded"], serde_json::json!([]));
        assert_eq!(
            repeated_result.response["data"]["indexFreshness"],
            serde_json::json!({
                "stale": true,
                "dbGeneration": 112,
                "indexGeneration": 2,
                "generationGap": 110,
                "largeGap": true,
            })
        );
    }

    #[test]
    fn daemon_human_renderer_keeps_small_gap_stale_warning() {
        let mut report = stale_index_advisory_report(2, 1);
        report
            .degraded
            .retain(|entry| entry.code != "search_index_large_gap");
        let response_data = report.data_json();

        assert_eq!(response_data["indexFreshness"]["largeGap"], false);
        let human = daemon_search_human_summary(&report, &response_data);
        assert!(human.contains("search_index_stale"));
        assert!(!human.contains("search_index_large_gap"));
    }

    #[test]
    fn context_large_gap_advisory_is_consumed_only_after_successful_delivery() {
        fn context_response() -> serde_json::Value {
            let degraded = serde_json::json!([
                {"code": "search_index_stale", "severity": "medium", "message": "stale"},
                {"code": "search_index_large_gap", "severity": "medium", "message": "large"}
            ]);
            serde_json::json!({
                "schema": crate::models::RESPONSE_SCHEMA_V2,
                "success": true,
                "data": {"degraded": degraded.clone()},
                "degraded": degraded,
            })
        }

        fn context_delivery_candidate(
            policy: &DaemonDispatchPolicy,
        ) -> (DaemonResponse, serde_json::Value) {
            let mut pending = PendingSearchAdvisoryDelivery::new(
                policy.search_advisory_session(),
                TEST_WORKSPACE_ID,
            );
            let mut result = context_response();
            {
                let mut session = policy
                    .search_advisory_session()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                filter_context_large_gap_advisory_for_delivery(
                    &mut result,
                    &mut session,
                    TEST_WORKSPACE_ID,
                    pending.reservation_mut(),
                );
            }
            let response = DaemonResponse::ok(
                "req-context-advisory-delivery",
                TEST_AGENT_ID,
                Some(TEST_WORKSPACE_ID.to_owned()),
                result.clone(),
            );
            (pending.finish(response, true), result)
        }

        let policy = DaemonDispatchPolicy::for_workspace(TEST_WORKSPACE_ID);
        let (mut failed_response, failed) = context_delivery_candidate(&policy);
        assert!(failed["degraded"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["code"] == "search_index_large_gap")
        }));
        settle_daemon_response_delivery(&policy, &mut failed_response, false);

        let (mut delivered_response, delivered) = context_delivery_candidate(&policy);
        assert!(delivered["degraded"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["code"] == "search_index_large_gap")
        }));
        settle_daemon_response_delivery(&policy, &mut delivered_response, true);

        let (_, repeated) = context_delivery_candidate(&policy);
        for pointer in ["/degraded", "/data/degraded"] {
            let codes = repeated
                .pointer(pointer)
                .and_then(serde_json::Value::as_array)
                .expect("degraded array")
                .iter()
                .filter_map(|entry| entry["code"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(codes, vec!["search_index_stale"]);
        }
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn apple_peer_uid_uses_safe_std_peer_credentials() {
        let (_client, server) = UnixStream::pair().expect("create UnixStream pair");
        assert_eq!(
            peer_uid(&server).expect("read Apple peer credentials"),
            current_euid()
        );
    }

    #[test]
    fn dispatch_search_rejects_method_schema_drift_and_unknown_fields() {
        for params in [
            serde_json::json!({
                "schema": "ee.daemon.search.request.v1",
                "query": "release",
                "workspacePath": "/tmp/ee-daemon-search-contract"
            }),
            serde_json::json!({
                "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
                "query": "release",
                "workspacePath": "/tmp/ee-daemon-search-contract",
                "unknownField": true
            }),
        ] {
            let response = dispatch(&search_request(params));
            assert!(response.result.is_none());
            assert_eq!(
                response.error.as_ref().map(|error| error.code.as_str()),
                Some(DAEMON_SEARCH_PARAMS_INVALID_CODE)
            );
        }
    }

    #[test]
    fn daemon_search_request_v2_carries_explicit_performance_bit() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let params = DaemonSearchParams::from_value(&serde_json::json!({
            "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
            "query": "release",
            "workspacePath": workspace.path(),
            "explainPerformance": true
        }))
        .expect("request v2 must accept its explicit performance bit");
        let (_, _, _, explain_performance) = params
            .into_search_parts(&workspace.path().display().to_string())
            .expect("valid request v2 must map to canonical search options");
        assert!(
            explain_performance,
            "performance request bit must survive strict wire decoding"
        );
    }

    #[test]
    fn daemon_search_request_v2_accepts_exact_integral_decimal_limits() {
        let workspace = tempfile::tempdir().expect("tempdir");
        for (raw, expected) in [
            ("1", 1_u32),
            ("1.0", 1_u32),
            ("1e0", 1_u32),
            ("4294967295.0", u32::MAX),
        ] {
            let mut value = serde_json::json!({
                "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
                "query": "release",
                "workspacePath": workspace.path()
            });
            value["limit"] = serde_json::from_str(raw).expect("exact numeric fixture");
            let params = DaemonSearchParams::from_value(&value)
                .unwrap_or_else(|error| panic!("limit {raw} must decode: {error}"));
            assert_eq!(params.limit, expected, "limit {raw}");
        }
    }

    #[test]
    fn daemon_search_request_v2_rejects_non_u32_limits() {
        let workspace = tempfile::tempdir().expect("tempdir");
        for raw in ["-1", "1.5", "4294967296", "1e400"] {
            let mut value = serde_json::json!({
                "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
                "query": "release",
                "workspacePath": workspace.path()
            });
            value["limit"] = serde_json::from_str(raw).expect("exact numeric fixture");
            assert!(
                DaemonSearchParams::from_value(&value).is_err(),
                "limit {raw} must be rejected"
            );
        }
    }

    #[test]
    fn daemon_search_request_v2_accepts_only_published_enum_spellings() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let workspace_id = workspace.path().display().to_string();
        let base = serde_json::json!({
            "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
            "query": "release",
            "workspacePath": workspace.path(),
            "speed": "default",
            "dedupe": "doc_id",
            "sourceMode": "hybrid",
            "memoryScope": "swarm"
        });

        for (field, canonical) in [
            ("speed", "quality"),
            ("dedupe", "mi"),
            ("sourceMode", "lexical_only"),
            ("memoryScope", "workspace"),
        ] {
            let mut value = base.clone();
            value[field] = serde_json::json!(canonical);
            DaemonSearchParams::from_value(&value)
                .expect("canonical request shape")
                .into_search_parts(&workspace_id)
                .unwrap_or_else(|error| panic!("canonical {field}={canonical:?}: {error}"));
        }

        for (field, noncanonical) in [
            ("speed", " Quality "),
            ("dedupe", "doc-id"),
            ("sourceMode", "lexical"),
            ("sourceMode", "Semantic_Only"),
            ("memoryScope", " SWARM "),
        ] {
            let mut value = base.clone();
            value[field] = serde_json::json!(noncanonical);
            let error = DaemonSearchParams::from_value(&value)
                .expect("string enum request shape")
                .into_search_parts(&workspace_id)
                .expect_err("noncanonical wire enum must be rejected");
            assert!(
                !error.is_empty(),
                "empty error for {field}={noncanonical:?}"
            );
        }
    }

    #[test]
    fn dispatch_search_binds_params_to_authorized_workspace() {
        let response = dispatch(&search_request(serde_json::json!({
            "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
            "query": "release",
            "workspacePath": "/tmp/a-different-workspace"
        })));
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some(DAEMON_SEARCH_PARAMS_INVALID_CODE)
        );
    }

    #[cfg(unix)]
    #[test]
    fn daemon_search_paths_are_canonical_and_workspace_contained() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let state_dir = workspace.join(".ee");
        fs::create_dir_all(&state_dir).expect("workspace state dir");
        let workspace_alias = temp.path().join("workspace-alias");
        std::os::unix::fs::symlink(&workspace, &workspace_alias).expect("workspace symlink");

        let params = DaemonSearchParams::from_value(&serde_json::json!({
            "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
            "query": "release",
            "workspacePath": workspace_alias,
            "databasePath": workspace.join(".ee/ee.db"),
            "indexDir": workspace.join(".ee/index")
        }))
        .expect("strict params");
        let (options, _, _, _) = params
            .into_search_parts(&workspace.display().to_string())
            .expect("canonical aliases inside the workspace are accepted");
        assert_eq!(
            options.workspace_path,
            fs::canonicalize(&workspace).unwrap()
        );
        assert_eq!(options.database_path, Some(workspace.join(".ee/ee.db")));
        assert_eq!(options.index_dir, Some(workspace.join(".ee/index")));
    }

    #[cfg(unix)]
    #[test]
    fn daemon_search_rejects_symlink_escape_for_database_and_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let state_dir = workspace.join(".ee");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&state_dir).expect("workspace state dir");
        fs::create_dir_all(&outside).expect("outside dir");
        let escape = state_dir.join("escape");
        std::os::unix::fs::symlink(&outside, &escape).expect("escape symlink");

        for (field, escaped_path) in [
            ("databasePath", escape.join("ee.db")),
            ("indexDir", escape.clone()),
        ] {
            let mut value = serde_json::json!({
                "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
                "query": "release",
                "workspacePath": workspace
            });
            value[field] = serde_json::json!(escaped_path);
            let params = DaemonSearchParams::from_value(&value).expect("strict params");
            let error = params
                .into_search_parts(&workspace.display().to_string())
                .expect_err("symlink escape must be rejected");
            assert!(error.contains("must remain inside the canonical workspace"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn daemon_search_rejects_dangling_final_symlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let state_dir = workspace.join(".ee");
        fs::create_dir_all(&state_dir).expect("workspace state dir");
        let dangling = state_dir.join("ee.db");
        std::os::unix::fs::symlink(temp.path().join("outside-missing.db"), &dangling)
            .expect("dangling database symlink");

        let params = DaemonSearchParams::from_value(&serde_json::json!({
            "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
            "query": "release",
            "workspacePath": workspace,
            "databasePath": dangling
        }))
        .expect("strict params");
        let error = params
            .into_search_parts(&workspace.display().to_string())
            .expect_err("dangling final symlink must be rejected");
        assert!(error.contains("must not identify a dangling symbolic link"));
    }

    #[test]
    fn daemon_search_result_rejects_schema_and_field_drift() {
        let canonical = serde_json::json!({
            "schema": crate::models::RESPONSE_SCHEMA_V2,
            "success": true,
            "data": {
                "command": "search",
                "status": "no_results",
                "embed_backend": "hash_fallback",
                "query": "release",
                "request": {},
                "scopeStats": {},
                "results": [],
                "consensus": [],
                "conflicts": [],
                "resultCount": 0,
                "elapsedMs": 1.0,
                "metrics": {},
                "rerank": {},
                "profileRuntime": {},
                "errors": [],
                "degraded": []
            },
            "degraded": []
        });
        let base = serde_json::json!({
            "schema": DAEMON_SEARCH_RESPONSE_SCHEMA_V3,
            "response": canonical.clone(),
            "human": "Search results\n",
            "reuseContract": {
                "daemonProcess": "long_lived",
                "defaultSearchEmbedder": "process_scoped",
                "searchIndex": "per_request"
            },
            "timing": {
                "daemonTotal": {
                    "elapsedMs": 2.0,
                    "elapsedMsBucket": "1_9ms",
                    "nondeterministic": true
                },
                "embedderPreparation": null,
                "indexOpen": null,
                "query": null
            }
        });
        let renderings = DaemonSearchResult::from_value(base.clone())
            .expect("valid daemon search result")
            .into_renderings()
            .expect("validated daemon search diagnostics must remain serializable");
        assert_eq!(
            renderings.reuse_contract, base["reuseContract"],
            "validated reuse contract must survive CLI rendering conversion"
        );
        assert_eq!(
            renderings.timing, base["timing"],
            "validated timing must survive CLI rendering conversion"
        );

        let mut degradation_drift = base.clone();
        degradation_drift["response"]["degraded"] = serde_json::json!([{
            "code": "rerank_model_unavailable",
            "severity": "info",
            "message": "reranker unavailable",
            "unexpected": true
        }]);
        assert!(DaemonSearchResult::from_value(degradation_drift).is_err());

        let mut wrong_schema = base.clone();
        wrong_schema["schema"] = serde_json::json!("ee.daemon.search.response.v1");
        assert!(DaemonSearchResult::from_value(wrong_schema).is_err());

        let mut unknown_field = base;
        unknown_field["unexpected"] = serde_json::json!(true);
        assert!(DaemonSearchResult::from_value(unknown_field).is_err());

        let mut nested_drift = serde_json::json!({
            "schema": DAEMON_SEARCH_RESPONSE_SCHEMA_V3,
            "response": canonical,
            "human": "Search results\n",
            "reuseContract": {
                "daemonProcess": "long_lived",
                "defaultSearchEmbedder": "process_scoped",
                "searchIndex": "per_request"
            },
            "timing": {
                "daemonTotal": {
                    "elapsedMs": 2.0,
                    "elapsedMsBucket": "1_9ms",
                    "nondeterministic": true
                },
                "embedderPreparation": null,
                "indexOpen": null,
                "query": null
            }
        });
        nested_drift["response"]["data"]["unknown"] = serde_json::json!(true);
        assert!(DaemonSearchResult::from_value(nested_drift).is_err());
    }

    #[test]
    fn daemon_search_uint64_conversion_is_exact_and_bounded() {
        assert_eq!(DAEMON_SEARCH_UINT64_INSTANCE_POINTERS.len(), 39);
        for (value, expected) in [
            (serde_json::json!(0), 0),
            (serde_json::json!(1.0), 1),
            (serde_json::json!(1e3), 1_000),
            (
                serde_json::json!(18_446_744_073_709_549_568.0),
                18_446_744_073_709_549_568,
            ),
            (serde_json::json!(u64::MAX), u64::MAX),
        ] {
            assert_eq!(json_number_to_u64(&value), Some(expected), "{value}");
        }

        for (raw, expected) in [
            ("18446744073709551615.0", u64::MAX),
            ("18446744073709551614.0", u64::MAX - 1),
            ("184467440737095516150e-1", u64::MAX),
        ] {
            let value: serde_json::Value =
                serde_json::from_str(raw).expect("exact decimal fixture must parse");
            assert_eq!(json_number_to_u64(&value), Some(expected), "{raw}");
        }

        let over_u64: serde_json::Value = serde_json::from_str("18446744073709551616")
            .expect("over-u64 JSON number must parse for contract validation");
        let over_u64_decimal: serde_json::Value = serde_json::from_str("18446744073709551616.0")
            .expect("over-u64 decimal fixture must parse");
        let near_boundary_fraction: serde_json::Value =
            serde_json::from_str("18446744073709551614.5")
                .expect("near-boundary fractional fixture must parse");
        for value in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            over_u64,
            over_u64_decimal,
            near_boundary_fraction,
            serde_json::json!("1"),
        ] {
            assert_eq!(json_number_to_u64(&value), None, "{value}");
        }
        for unrepresentable in ["NaN", "Infinity", "-Infinity"] {
            assert!(
                serde_json::from_str::<serde_json::Value>(unrepresentable).is_err(),
                "nonfinite JSON number must fail before uint64 conversion: {unrepresentable}"
            );
        }
        let huge_finite: serde_json::Value = serde_json::from_str("1e400")
            .expect("arbitrary-precision JSON must preserve huge finite numbers");
        assert_eq!(
            json_number_to_u64(&huge_finite),
            None,
            "finite values outside uint64 must fail conversion"
        );
    }

    #[test]
    fn daemon_search_performance_preserves_canonical_in_process_report() {
        let report = permanent_reranker_advisory_report();
        let trace = SearchPerformanceTrace::default();
        let canonical =
            report.performance_explain_json_with_trace(SpeedMode::Default, false, &trace);
        let timing = DaemonSearchTiming::from_trace(Duration::from_millis(2), &trace);
        let mut advisory_session = SearchAdvisorySession::default();
        let encoded = serde_json::to_value(DaemonSearchResult::from_report(
            &report,
            false,
            TEST_WORKSPACE_ID,
            &mut advisory_session,
            timing,
            Some(canonical.clone()),
        ))
        .expect("daemon search result must encode");

        assert_eq!(
            encoded["performance"], canonical,
            "daemon transport must carry the in-process canonical performance payload unchanged"
        );
        let renderings = DaemonSearchResult::from_value(encoded)
            .expect("canonical performance response must validate")
            .into_renderings()
            .expect("validated performance response must remain serializable");
        assert_eq!(
            renderings.performance.as_ref(),
            Some(&canonical),
            "client decoding must preserve every canonical performance field"
        );
    }

    #[test]
    fn daemon_search_performance_strict_v3_rejects_raw_text_and_type_drift() {
        let report = stale_index_advisory_report(108, 1);
        let trace = SearchPerformanceTrace::default();
        let canonical =
            report.performance_explain_json_with_trace(SpeedMode::Default, false, &trace);
        assert!(
            canonical["data"]["fallbacks"]
                .as_array()
                .is_some_and(|fallbacks| !fallbacks.is_empty()),
            "fixture must exercise the strict fallback item validator"
        );
        let timing = DaemonSearchTiming::from_trace(Duration::from_millis(2), &trace);
        let mut advisory_session = SearchAdvisorySession::default();
        let encoded = serde_json::to_value(DaemonSearchResult::from_report(
            &report,
            false,
            TEST_WORKSPACE_ID,
            &mut advisory_session,
            timing,
            Some(canonical),
        ))
        .expect("daemon search result must encode");
        DaemonSearchResult::from_value(encoded.clone())
            .expect("canonical strict-v3 performance payload must validate");

        let mut raw_text = encoded.clone();
        raw_text["performance"]["data"]["query"]["rawText"] = serde_json::json!("release secret");
        assert!(
            DaemonSearchResult::from_value(raw_text).is_err(),
            "strict v3 must reject raw query text even when all required fields remain"
        );

        let mut query_type_drift = encoded.clone();
        query_type_drift["performance"]["data"]["query"]["lengthBytes"] = serde_json::json!("7");
        assert!(DaemonSearchResult::from_value(query_type_drift).is_err());

        let mut plan_type_drift = encoded.clone();
        plan_type_drift["performance"]["data"]["queryPlan"]["usesEmbeddings"] =
            serde_json::json!("false");
        assert!(DaemonSearchResult::from_value(plan_type_drift).is_err());

        let mut read_type_drift = encoded.clone();
        read_type_drift["performance"]["data"]["dbReads"]["memoryReads"] = serde_json::json!(-1);
        assert!(DaemonSearchResult::from_value(read_type_drift).is_err());

        let mut profile_nested_drift = encoded.clone();
        profile_nested_drift["performance"]["data"]["profileRuntime"]["budgets"]["search"]["unexpected"] =
            serde_json::json!(1);
        assert!(DaemonSearchResult::from_value(profile_nested_drift).is_err());

        let mut search_type_drift = encoded.clone();
        search_type_drift["performance"]["data"]["search"]["returnedHits"] = serde_json::json!("1");
        assert!(DaemonSearchResult::from_value(search_type_drift).is_err());

        let mut timing_nested_drift = encoded.clone();
        timing_nested_drift["performance"]["data"]["search"]["elapsed"]["nondeterministic"] =
            serde_json::json!(false);
        assert!(DaemonSearchResult::from_value(timing_nested_drift).is_err());

        let mut fallback_type_drift = encoded.clone();
        fallback_type_drift["performance"]["data"]["fallbacks"][0]["sources"] =
            serde_json::json!([7]);
        assert!(DaemonSearchResult::from_value(fallback_type_drift).is_err());

        let mut redaction_value_drift = encoded;
        redaction_value_drift["performance"]["data"]["redaction"]["safeFields"] =
            serde_json::json!(["counts"]);
        assert!(DaemonSearchResult::from_value(redaction_value_drift).is_err());
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
        let search_advisory_session = Mutex::new(SearchAdvisorySession::default());
        let response = dispatch_with_echo_policy_and_workspace(
            &request,
            false,
            Some(TEST_WORKSPACE_ID),
            &shutdown,
            None,
            &search_advisory_session,
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
        assert_eq!(
            daemon_method_authority(METHOD_SEARCH),
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
                METHOD_SEARCH,
                METHOD_SHUTDOWN,
                METHOD_TELEMETRY,
                METHOD_WRITE,
                METHOD_WRITE_JOURNAL
            ]))
        );
        assert_eq!(
            result
                .pointer("/method_schemas/ee.daemon.search/request")
                .and_then(serde_json::Value::as_str),
            Some(DAEMON_SEARCH_REQUEST_SCHEMA_V2)
        );
        assert_eq!(
            result
                .pointer("/method_schemas/ee.daemon.search/response")
                .and_then(serde_json::Value::as_str),
            Some(DAEMON_SEARCH_RESPONSE_SCHEMA_V3)
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
        assert!(
            params.auto_link,
            "auto_link defaults true for ee-remember parity"
        );
        assert!(
            params.propose_candidates,
            "propose_candidates defaults true"
        );
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
    fn mixed_journal_outcome_batch_rolls_back_as_one_transaction() {
        let dir = tempfile::Builder::new()
            .prefix("daemon-mixed-batch")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let workspace_path = dir.path().canonicalize().expect("canonical workspace");
        let database_path = workspace_path.join(".ee").join("ee.db");
        std::fs::create_dir_all(workspace_path.join(".ee")).expect("create .ee");
        let workspace_id = "wsp_00000000000000000000009991".to_string();
        let memory_id = "mem_00000000000000000000009992".to_string();
        let connection = crate::db::DbConnection::open_file(&database_path).expect("open db");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                &workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("daemon mixed batch".to_string()),
                },
            )
            .expect("workspace");
        connection
            .insert_memory(
                &memory_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Use daemon mixed batch transaction tests.".to_string(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    provenance_uri: Some("test://daemon-mixed-batch".to_string()),
                    trust_class: "human_explicit".to_string(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("memory");

        let journal_id = crate::core::journal::generate_journal_entry_id();
        let journal = crate::core::write_owner::WriteOperation::Custom {
            operation_type: DaemonJournalParams::ACTOR_OPERATION_TYPE.to_string(),
            payload: serde_json::to_value(DaemonJournalParams {
                workspace_path: workspace_path.clone(),
                workspace_id: workspace_id.clone(),
                entry_id: Some(journal_id),
                agent_name: Some("daemon-test".to_string()),
                session_key: None,
                kind: "note".to_string(),
                source: "manual".to_string(),
                body: "journal row staged before failing outcome".to_string(),
                structured: None,
                redaction_report: "{}".to_string(),
                instruction_risk: "none".to_string(),
            })
            .expect("journal payload"),
        };
        let invalid_outcome = crate::core::write_owner::WriteOperation::Custom {
            operation_type: DaemonOutcomeParams::ACTOR_OPERATION_TYPE.to_string(),
            payload: serde_json::to_value(DaemonOutcomeParams {
                workspace_path: workspace_path.clone(),
                event_id: "bad-feedback-id".to_string(),
                workspace_id: workspace_id.clone(),
                target_type: "memory".to_string(),
                target_id: memory_id,
                signal: "helpful".to_string(),
                weight: 1.0,
                source_type: "outcome_observed".to_string(),
                source_id: Some("daemon-test".to_string()),
                reason: Some("force invalid event id rollback".to_string()),
                evidence_json: None,
                session_id: None,
                actor: Some("daemon-test".to_string()),
                audit_id: crate::db::generate_audit_id(),
                details: None,
            })
            .expect("outcome payload"),
        };

        let result = execute_daemon_txn_batch(&[journal, invalid_outcome]);
        assert!(result.is_err(), "invalid outcome id should fail the batch");
        let entries = connection
            .list_journal_entries(
                &workspace_id,
                &crate::db::JournalEntryListFilter {
                    limit: 10,
                    ..crate::db::JournalEntryListFilter::default()
                },
            )
            .expect("list journal entries");
        assert!(
            entries.is_empty(),
            "journal insert must roll back when a later outcome op fails"
        );
    }

    #[test]
    fn invalid_daemon_remember_batch_does_not_create_store() {
        let dir = tempfile::Builder::new()
            .prefix("daemon-invalid-remember-batch")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let workspace_path = dir.path().canonicalize().expect("canonical workspace");
        let store_dir = workspace_path.join(".ee");
        let database_path = store_dir.join("ee.db");
        let remember_op =
            |content: &str, kind: &str| crate::core::write_owner::WriteOperation::Custom {
                operation_type: DaemonWriteParams::ACTOR_OPERATION_TYPE.to_string(),
                payload: serde_json::to_value(DaemonWriteParams {
                    workspace_path: workspace_path.clone(),
                    content: content.to_string(),
                    level: "episodic".to_string(),
                    kind: kind.to_string(),
                    tags: None,
                    confidence: 0.8,
                    source: Some("manual://daemon-invalid-remember-batch".to_string()),
                    workflow_id: None,
                    auto_link: false,
                    propose_candidates: false,
                })
                .expect("remember payload"),
            };

        let result = execute_daemon_txn_batch(&[
            remember_op("Valid row before the invalid row.", "fact"),
            remember_op("Cross-wired row must reject the batch.", "semantic"),
        ]);
        let error = result.expect_err("cross-wired remember must reject the batch");
        assert_eq!(
            error.code(),
            crate::core::memory::REMEMBER_KIND_IS_LEVEL_CODE
        );
        assert!(
            !database_path.exists(),
            "invalid daemon remember batch must not create the database"
        );
        assert!(
            !store_dir.exists(),
            "invalid daemon remember batch must not create the store directory"
        );
    }

    #[test]
    fn valid_daemon_remember_batch_requires_an_initialized_store() {
        let dir = tempfile::Builder::new()
            .prefix("daemon-storeless-remember-batch")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let workspace_path = dir.path().join("addressed-but-uninitialized");
        let database_path = workspace_path.join(".ee").join("ee.db");
        let operation = crate::core::write_owner::WriteOperation::Custom {
            operation_type: DaemonWriteParams::ACTOR_OPERATION_TYPE.to_string(),
            payload: serde_json::to_value(DaemonWriteParams {
                workspace_path: workspace_path.clone(),
                content: "A valid daemon write must not initialize its own store.".to_string(),
                level: "episodic".to_string(),
                kind: "fact".to_string(),
                tags: None,
                confidence: 0.8,
                source: Some("manual://daemon-storeless-remember-batch".to_string()),
                workflow_id: None,
                auto_link: false,
                propose_candidates: false,
            })
            .expect("remember payload"),
        };

        let error = execute_daemon_txn_batch(&[operation])
            .expect_err("a daemon write must reject an uninitialized addressed store");
        assert!(matches!(
            error,
            crate::models::DomainError::WorkspaceStoreMissing { .. }
        ));
        assert!(!workspace_path.exists());
        assert!(!database_path.exists());
    }

    #[test]
    fn daemon_remember_batch_commits_memories_and_drains_index_jobs() {
        let dir = tempfile::Builder::new()
            .prefix("daemon-remember-batch")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let workspace_path = dir.path().canonicalize().expect("canonical workspace");
        let database_path = workspace_path.join(".ee").join("ee.db");
        std::fs::create_dir_all(workspace_path.join(".ee")).expect("create .ee");
        let initialized = crate::db::DbConnection::open_file(&database_path)
            .expect("initialize daemon fixture database");
        initialized
            .migrate()
            .expect("migrate daemon fixture database");
        initialized.close().expect("close daemon fixture database");

        let remember_op = |content: &str| crate::core::write_owner::WriteOperation::Custom {
            operation_type: DaemonWriteParams::ACTOR_OPERATION_TYPE.to_string(),
            payload: serde_json::to_value(DaemonWriteParams {
                workspace_path: workspace_path.clone(),
                content: content.to_string(),
                level: "procedural".to_string(),
                kind: "rule".to_string(),
                tags: Some("daemon-batch".to_string()),
                confidence: 0.8,
                source: Some("manual://daemon-remember-batch".to_string()),
                workflow_id: None,
                auto_link: false,
                propose_candidates: false,
            })
            .expect("remember payload"),
        };

        let results = execute_daemon_txn_batch(&[
            remember_op("Daemon remember batch row one."),
            remember_op("Daemon remember batch row two."),
        ])
        .expect("remember batch");
        assert_eq!(results.len(), 2);
        let memory_ids = results
            .into_iter()
            .map(|result| match result {
                crate::core::write_owner::WriteResult::Success {
                    entity_id: Some(id),
                } => id,
                other => panic!("expected successful memory result, got {other:?}"),
            })
            .collect::<Vec<_>>();

        let connection = crate::db::DbConnection::open_file(&database_path).expect("open db");
        let mut workspace_id = None;
        for memory_id in &memory_ids {
            let memory = connection
                .get_memory(memory_id)
                .expect("query memory")
                .expect("memory exists");
            workspace_id = Some(memory.workspace_id);
        }
        let workspace_id = workspace_id.expect("workspace id");
        let pending = connection
            .list_pending_search_index_jobs(&workspace_id, None)
            .expect("pending jobs");
        assert!(
            pending.is_empty(),
            "daemon remember batch should drain deferred memory index jobs"
        );
        let status =
            crate::core::index::get_index_status(&crate::core::index::IndexStatusOptions {
                workspace_path: workspace_path.clone(),
                database_path: Some(database_path.clone()),
                index_dir: None,
            })
            .expect("daemon batch index status");
        assert_eq!(status.health, crate::core::index::IndexHealth::Ready);
        assert_eq!(status.db_generation, status.index_generation);
        assert_eq!(
            status
                .index_document_counts
                .as_ref()
                .map(|counts| counts.memories),
            Some(2)
        );
        let search = crate::core::search::run_search_with_filters(
            &crate::core::search::SearchOptions {
                workspace_path,
                database_path: Some(database_path),
                index_dir: None,
                query: "Daemon remember batch row".to_owned(),
                limit: 10,
                speed: crate::search::SpeedMode::Instant,
                explain: false,
                as_of: None,
                include_tombstoned: false,
                include_expired: false,
                include_future: false,
                include_stale: false,
                relevance_floor: Some(0.0),
                dedup_mode: crate::core::search::SearchDedupMode::DocId,
                source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
                strict_source_mode: true,
                memory_scope: crate::models::MemoryScope::Workspace,
                strict_scope: false,
            },
            None,
            &[],
        )
        .expect("daemon batch search");
        let actual_ids = search
            .results
            .into_iter()
            .map(|hit| hit.doc_id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            actual_ids,
            memory_ids.into_iter().collect(),
            "daemon batch response IDs must be exactly searchable without a manual rebuild"
        );
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
        runtime.block_on(task);
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
        let search_advisory_session = Mutex::new(SearchAdvisorySession::default());
        let response = dispatch_with_echo_policy_and_workspace(
            &request,
            false,
            None,
            &shutdown,
            None,
            &search_advisory_session,
        );

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
        // Pinned remote verification can set TMPDIR to a long materialized
        // checkout path whose ancestors intentionally fail the daemon's
        // security policy (or whose socket names exceed SUN_LEN). Keep the
        // fixture under the canonical system temp root instead: /tmp is
        // sticky on Unix, while the fresh leaf remains owner-only.
        let temp_root = fs::canonicalize("/tmp").expect("canonical Unix temp root");
        let temp = tempfile::Builder::new()
            .prefix("ee-sb-")
            .tempdir_in(temp_root)
            .expect("private short socket tempdir");
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
    ) -> (
        JoinHandle<(u64, Vec<String>)>,
        Arc<std::sync::atomic::AtomicU64>,
    ) {
        use std::os::unix::fs::FileTypeExt;

        let observed_samples = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let watcher_samples = Arc::clone(&observed_samples);
        let watcher = thread::spawn(move || {
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
                watcher_samples.store(samples, Ordering::Release);
                thread::yield_now();
            }
            (samples, violations)
        });
        (watcher, observed_samples)
    }

    fn wait_for_canonical_path_watcher_sample(
        observed_samples: &std::sync::atomic::AtomicU64,
        previous_samples: u64,
    ) -> u64 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let samples = observed_samples.load(Ordering::Acquire);
            if samples > previous_samples {
                return samples;
            }
            assert!(
                Instant::now() < deadline,
                "canonical-path watcher must make bounded observation progress",
            );
            thread::yield_now();
        }
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

    #[test]
    fn socket_broker_publish_refuses_nonsticky_writable_ancestor() {
        let temp = private_tempdir();
        let shared_ancestor = temp.path().join("shared-ancestor");
        let private_parent = shared_ancestor.join("private-child");
        fs::create_dir_all(&private_parent).expect("create nested socket parent");
        fs::set_permissions(&shared_ancestor, fs::Permissions::from_mode(0o777))
            .expect("make ancestor replaceable by another uid");
        fs::set_permissions(&private_parent, fs::Permissions::from_mode(0o700))
            .expect("keep immediate parent private");

        let socket_path = private_parent.join("ee-daemon.sock");
        let error = SocketBroker::new(socket_path.clone())
            .publish_listener()
            .expect_err("a private leaf under a replaceable ancestor must be refused");
        match error {
            DaemonStartError::InsecureSocketParent { path, reason } => {
                assert_eq!(path, shared_ancestor);
                assert!(
                    reason.contains("non-owner rename without sticky protection"),
                    "ancestor refusal must explain the rename risk; got {reason}",
                );
            }
            other => panic!("writable ancestor must be an insecure-parent error; got {other:?}"),
        }
        assert!(
            !socket_path.exists(),
            "ancestor refusal must happen before the canonical socket is created",
        );
        assert!(
            !private_parent.join("ee-daemon.sock.start.lock").exists(),
            "ancestor refusal must happen before the publish lock is created",
        );
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
        let (watcher, observed_samples) =
            spawn_canonical_path_invariant_watcher(socket_path.clone(), false, Arc::clone(&stop));
        wait_for_canonical_path_watcher_sample(&observed_samples, 0);

        let broker = SocketBroker::new(socket_path.clone());
        let (listener, _publish_lock) = broker.publish_listener().expect(
            "publish over a dead stale socket must succeed via temp-bind + atomic rename \
             (ADR 0055 stale replacement)",
        );
        let after_publish = observed_samples.load(Ordering::Acquire);
        wait_for_canonical_path_watcher_sample(&observed_samples, after_publish);

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
        let (watcher, observed_samples) =
            spawn_canonical_path_invariant_watcher(socket_path.clone(), true, Arc::clone(&stop));
        wait_for_canonical_path_watcher_sample(&observed_samples, 0);

        let broker = SocketBroker::new(socket_path.clone());
        let (listener, _publish_lock) = broker
            .publish_listener()
            .expect("fresh publish in a private parent must succeed");
        let after_publish = observed_samples.load(Ordering::Acquire);
        wait_for_canonical_path_watcher_sample(&observed_samples, after_publish);

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
    /// a valid UUID v7, and must be unique per attempt so concurrent and
    /// post-crash publishes never collide.
    #[test]
    fn socket_broker_temp_bind_path_is_parent_local_and_unique() {
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon-temp-name.sock");
        let broker = SocketBroker::new(socket_path.clone());

        let first = broker.temp_bind_path();
        let second = broker.temp_bind_path();

        assert_ne!(
            first, second,
            "temp bind paths must be unique per attempt so concurrent publishes never collide \
             and each rename is a clean atomic publish (ADR 0055)",
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
            let uuid_suffix = name
                .strip_prefix("ee-daemon-temp-name.sock.tmp.")
                .expect("prefix was checked above");
            let parsed = uuid::Uuid::parse_str(uuid_suffix)
                .expect("temp bind suffix must be a syntactically valid UUID");
            assert_eq!(
                parsed.get_version_num(),
                7,
                "temp bind suffix must be UUID v7 for time-ordered uniqueness; got {name}",
            );
        }
    }

    #[test]
    fn socket_broker_temp_collision_never_unlinks_planted_regular_file() {
        let temp = private_tempdir();
        let socket_path = temp.path().join("ee-daemon.sock");
        let broker = SocketBroker::new(socket_path);
        let planted_path = broker.temp_bind_path();
        fs::write(&planted_path, b"operator bytes").expect("plant temp-path regular file");

        broker
            .bind_secured_temp_listener_at(&planted_path)
            .expect_err("bind must refuse a pre-existing temp path");
        assert_eq!(
            fs::read(&planted_path).expect("planted path must remain readable"),
            b"operator bytes",
            "temp collision handling must not unlink or overwrite a non-socket path",
        );
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
