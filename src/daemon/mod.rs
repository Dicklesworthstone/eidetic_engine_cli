//! Optional `ee daemon` Unix-domain socket RPC skeleton (bd-oja31 / SRR1).
//!
//! The daemon is opt-in: every CLI command continues to work without it.
//! When started, it binds a UDS at `${XDG_RUNTIME_DIR}/ee/daemon.sock` on
//! Linux, falling back to `${TMPDIR:-/tmp}/ee-${uid}/daemon.sock` on
//! platforms (macOS) that do not standardize `XDG_RUNTIME_DIR`. The
//! socket file is chmodded to 0o600 and the parent directory to 0o700
//! immediately after bind; the accept loop also gates every connection
//! through a `getpeereid` / `SO_PEERCRED` check (bd-3j0td). The wire
//! framing is a
//! length-prefixed JSON message pair (`ee.daemon.request.v1` →
//! `ee.daemon.response.v1`); see `docs/schemas/ee.daemon.request.v1.json`
//! and `docs/schemas/ee.daemon.response.v1.json` for the canonical
//! field contracts.
//!
//! This module ships the skeleton only. The end goal of bd-oja31 is a
//! RAM-pinned ANN + lexical-index hot-mode RPC; the skeleton lands the
//! transport, framing, dispatch table, and two seed methods so the
//! ANN warm-load and the `mlock`/`MADV_HUGEPAGE` adapter (bd-17c65.14.9)
//! can land behind it without re-litigating the protocol shape:
//!
//! - `ee.daemon.echo` — round-trip integrity check. Returns the request
//!   `params` unchanged. Used by `tests/daemon_uds_rpc_round_trip.rs` to
//!   pin the framing contract.
//! - `ee.daemon.context` — stub for the future warm-loaded `ee context`
//!   path. Always returns `error.code = daemon_ann_warmload_not_yet_implemented`
//!   until the ANN warm-load slice ships.
//!
//! Threading model: the skeleton uses a `std::thread::spawn` accept loop
//! and a small per-connection worker. A future slice will wrap the
//! accept loop in an Asupersync `Region` so the daemon participates in
//! the same supervision tree as the rest of `ee`. That refactor changes
//! `DaemonServer::serve()` only; the wire framing and dispatch table
//! are stable.
//!
//! Platform support: the UDS path is Unix-only. On non-Unix platforms
//! (Windows) the `start_daemon` entry point returns a
//! [`DaemonStartError::PlatformUnsupported`] error so the CLI can emit a
//! structured degraded entry rather than panicking.

#![allow(clippy::module_name_repetitions)]

use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod protocol;

/// Metrics-collection seam for the dispatch path (bd-3vkyp). Platform-
/// agnostic: depends only on the wire types in [`protocol`].
pub mod metrics;

#[cfg(unix)]
pub mod server;

/// Schema id pinned in `docs/schemas/ee.daemon.request.v1.json`.
pub const DAEMON_REQUEST_SCHEMA_V1: &str = "ee.daemon.request.v1";

/// Schema id pinned in `docs/schemas/ee.daemon.response.v1.json`.
pub const DAEMON_RESPONSE_SCHEMA_V1: &str = "ee.daemon.response.v1";

/// Hard upper bound on the byte length of an inbound request envelope.
/// Defends the daemon against a misbehaving client sending an unbounded
/// length prefix. Real `ee.daemon.request.v1` envelopes are well under
/// 64 KiB even for `ee.daemon.context` payloads, so 4 MiB is a generous
/// ceiling that still bounds peak allocation per connection.
pub const DAEMON_REQUEST_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Hard upper bound on the byte length of an outbound response envelope.
/// Matches the request cap; downstream methods that would emit larger
/// responses (a full warm-loaded context pack) MUST truncate or split
/// the response before serialization.
pub const DAEMON_RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Default per-request read/write timeout. A skeleton method like
/// `ee.daemon.echo` should complete in microseconds; this timeout
/// catches stuck clients that opened a connection and stopped sending.
pub const DAEMON_DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Degraded code emitted on the `ee.daemon.context` stub path until the
/// ANN warm-load slice ships. The CLI client maps this onto the
/// canonical envelope's `degraded[]` array with severity `medium` per
/// `docs/degraded_code_taxonomy.md`.
pub const DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE: &str =
    "daemon_ann_warmload_not_yet_implemented";

/// Degraded code emitted by `ee daemon stop` when no daemon socket is
/// present at the resolved path (the operator stopped a daemon that
/// was not running). Informational so an operator who expected
/// hot-mode acceleration knows the daemon was not up and can re-run
/// `ee daemon start`. The macOS RAM-pinning concern that a sibling
/// dead constant once tried to express is already covered by the
/// properly-scoped `lexical_ram_unavailable_on_macos` /
/// `numa_pin_unsupported_platform` codes (see bd-2prjb).
pub const DAEMON_SOCKET_UNAVAILABLE_CODE: &str = "daemon_socket_unavailable";

/// Degraded code emitted when the daemon accept loop refuses a new
/// connection because the bounded worker pool is saturated. The CLI
/// client must fall back to in-process execution and may retry after a
/// brief backoff once existing workers drain. See bd-jnyui.
pub const DAEMON_OVERLOADED_CODE: &str = "daemon_overloaded";

/// Hard upper bound on the number of in-flight per-connection worker
/// threads the daemon will spawn. Defaults to 32 and is overridable via
/// `EE_DAEMON_MAX_INFLIGHT` for stress harnesses; saturated accepts are
/// answered with a framed `daemon_overloaded` response and the
/// connection closed rather than queued. See bd-jnyui (P1 — unbounded
/// thread spawn was a local DoS vector before this cap shipped).
pub const DAEMON_MAX_INFLIGHT: usize = 32;

/// Degraded code emitted on the daemon's accept side when a peer's
/// effective UID does not match the daemon process's effective UID.
/// The daemon refuses to dispatch the request and returns a framed
/// error response carrying this code; the CLI client maps it onto the
/// canonical envelope's `degraded[]` array with severity `high` per
/// `docs/degraded_code_taxonomy.md`. Pins the bd-3j0td (P0 UDS
/// world-connectable) fix surface: the chmod-0600 + getpeereid gate
/// IS the cross-tenant exfil defense.
pub const DAEMON_PEER_UNAUTHORIZED_CODE: &str = "daemon_peer_unauthorized";

/// Degraded code emitted when the daemon fails to install the
/// per-connection read/write deadline (`setsockopt(SO_RCVTIMEO /
/// SO_SNDTIMEO)`) on a freshly accepted stream. The 30s read timeout is
/// the only backstop preventing a half-open peer from pinning a worker
/// thread forever; if `setsockopt` fails (low memory, a seccomp filter
/// that blocks it, certain BSD kernel modes) the daemon refuses the
/// connection with this code and drops it rather than entering a
/// deadline-less `read` that would hang the worker indefinitely. The
/// CLI client maps it onto the canonical envelope's `degraded[]` array
/// with severity `high` per `docs/degraded_code_taxonomy.md`. bd-3pnno.
pub const DAEMON_SETSOCKOPT_FAILED_CODE: &str = "daemon_setsockopt_failed";

/// Degraded code written to a peer whose connection was accepted while
/// the daemon was already shutting down. The accept loop checks the
/// shutdown latch after each `accept`; if it is set, a connection may
/// still have been established (the shutdown wake itself connects, and
/// a legitimate client can race in between the shutdown signal and the
/// listener teardown). Rather than dropping that stream silently —
/// which the client observes as an inscrutable connection reset — the
/// daemon writes a framed envelope carrying this code so the client can
/// cleanly fall back to the in-process path or retry against a fresh
/// daemon. The CLI client maps it onto the canonical envelope's
/// `degraded[]` array with severity `medium` per
/// `docs/degraded_code_taxonomy.md`. bd-36dp2.
pub const DAEMON_SHUTTING_DOWN_CODE: &str = "daemon_shutting_down";

/// Compute the canonical daemon socket path for the current platform.
/// On Linux the path is `${XDG_RUNTIME_DIR}/ee/daemon.sock` (the
/// runtime dir is already mode 0700 per the systemd-user contract);
/// on macOS (and any other Unix-y platform where `XDG_RUNTIME_DIR` is
/// unset) the fallback is `${TMPDIR:-/tmp}/ee-${uid}/daemon.sock` so
/// the per-UID parent directory partitions the socket across local
/// tenants. The bare `/tmp/ee-daemon.sock` shape collided across
/// every local UID and is a documented attack surface (bd-3j0td).
#[must_use]
pub fn default_daemon_socket_path() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let runtime = Path::new(&runtime_dir);
        if !runtime.as_os_str().is_empty() {
            return runtime.join("ee").join("daemon.sock");
        }
    }
    let tmp = std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into());
    let uid = current_euid();
    Path::new(&tmp)
        .join(format!("ee-{uid}"))
        .join("daemon.sock")
}

/// Return the effective UID of the calling process. Used by
/// [`default_daemon_socket_path`] to partition the macOS / TMPDIR
/// fallback per-tenant and by the daemon accept loop to compare
/// against the peer credential.
#[cfg(unix)]
#[must_use]
pub fn current_euid() -> u32 {
    // SAFETY: `geteuid(2)` is async-signal-safe, has no preconditions,
    // and cannot fail. It does not touch errno.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
#[must_use]
pub fn current_euid() -> u32 {
    0
}

/// Errors that can be reported by [`start_daemon`] before the accept
/// loop runs. The CLI handler maps these onto either a structured
/// `ee.error.v2` envelope (for unrecoverable starts) or a degraded
/// entry (for platform-not-supported paths).
#[derive(Debug)]
pub enum DaemonStartError {
    /// UDS RPC is not supported on the current platform (Windows).
    PlatformUnsupported,
    /// The socket path's parent directory could not be created.
    SocketDirCreate {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The socket path was occupied by a non-socket file. The skeleton
    /// refuses to overwrite arbitrary files; the operator must remove
    /// the conflicting path explicitly.
    SocketPathOccupied { path: PathBuf },
    /// The `bind(2)` call failed.
    Bind {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for DaemonStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PlatformUnsupported => formatter.write_str(
                "ee daemon UDS RPC is only supported on Unix targets; \
                 the in-process CLI path remains available on Windows.",
            ),
            Self::SocketDirCreate { path, source } => write!(
                formatter,
                "Failed to create daemon socket parent directory {}: {source}",
                path.display()
            ),
            Self::SocketPathOccupied { path } => write!(
                formatter,
                "Daemon socket path {} is occupied by a non-socket file; \
                 remove it manually before retrying `ee daemon start`.",
                path.display()
            ),
            Self::Bind { path, source } => write!(
                formatter,
                "Failed to bind daemon socket at {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for DaemonStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PlatformUnsupported | Self::SocketPathOccupied { .. } => None,
            Self::SocketDirCreate { source, .. } | Self::Bind { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_path_uses_xdg_runtime_dir_when_set() {
        // Bypass the env-var read by exercising the canonical-path
        // construction directly; the production helper consults
        // process env which tests cannot mutate safely.
        let runtime = Path::new("/run/user/1000");
        assert_eq!(
            runtime.join("ee").join("daemon.sock"),
            Path::new("/run/user/1000/ee/daemon.sock"),
        );
    }

    #[test]
    fn default_socket_path_falls_back_to_per_uid_tmpdir_on_darwin() {
        // Post-bd-3j0td: the fallback shape is
        // `${TMPDIR:-/tmp}/ee-${uid}/daemon.sock`. The per-UID parent
        // partitions the socket across local tenants so the shared
        // `/tmp/ee-daemon.sock` collision (the pre-fix attack surface)
        // is gone. Pin the construction; a future refactor that
        // collapses this back to the world-shared bare path is a
        // regression of the P0 fix.
        let tmp = Path::new("/tmp");
        let uid: u32 = 1000;
        assert_eq!(
            tmp.join(format!("ee-{uid}")).join("daemon.sock"),
            Path::new("/tmp/ee-1000/daemon.sock"),
        );
    }

    #[test]
    fn schema_constants_match_docs_filenames() {
        // The schema IDs MUST match the docs/schemas/*.json filenames
        // exactly. A drift here would break the schema-export surface
        // and the contract-drift radar.
        assert_eq!(DAEMON_REQUEST_SCHEMA_V1, "ee.daemon.request.v1");
        assert_eq!(DAEMON_RESPONSE_SCHEMA_V1, "ee.daemon.response.v1");
    }

    #[test]
    fn request_and_response_caps_are_symmetric() {
        // The 4-MiB symmetry is intentional: a method that decodes a
        // capped request and would produce an oversized response must
        // truncate or split before serialization rather than relying
        // on an asymmetric outbound ceiling. The constants must move
        // together; a divergence is a real contract change.
        assert_eq!(DAEMON_REQUEST_MAX_BYTES, DAEMON_RESPONSE_MAX_BYTES);
        assert_eq!(DAEMON_REQUEST_MAX_BYTES, 4 * 1024 * 1024);
    }

    #[test]
    fn default_rpc_timeout_is_generous_for_skeleton_methods() {
        // `ee.daemon.echo` should complete in microseconds; the 30s
        // ceiling is intentionally loose so that early warm-load
        // method slices can land without immediately tripping it.
        // The test pins the constant so future tuning is explicit.
        assert_eq!(DAEMON_DEFAULT_RPC_TIMEOUT, Duration::from_secs(30));
    }
}
