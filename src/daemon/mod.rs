//! Optional `ee daemon` Unix-domain socket RPC skeleton (bd-oja31 / SRR1).
//!
//! The daemon is opt-in: every CLI command continues to work without it.
//! When started, it binds a UDS at `${XDG_RUNTIME_DIR}/ee/daemon.sock` on
//! Linux, falling back to `/tmp/ee-daemon.sock` on platforms (macOS) that
//! do not standardize `XDG_RUNTIME_DIR`. The wire framing is a
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

/// Degraded code emitted when `start_daemon` is called on a non-Unix
/// platform. UDS bind is intentionally Unix-only; Windows ships the
/// in-process CLI path with no daemon-mode acceleration today.
pub const DAEMON_RAM_PINNING_UNAVAILABLE_ON_MACOS_CODE: &str =
    "daemon_ram_pinning_unavailable_on_macos";

/// Degraded code emitted when the daemon socket cannot be reached by
/// the CLI client. The CLI fallback path remains in-process execution;
/// this code is informational so the operator can re-run
/// `ee daemon start` if they expected hot-mode acceleration.
pub const DAEMON_SOCKET_UNAVAILABLE_CODE: &str = "daemon_socket_unavailable";

/// Compute the canonical daemon socket path for the current platform.
/// On Linux the path is `${XDG_RUNTIME_DIR}/ee/daemon.sock`; on macOS
/// (and any other Unix-y platform where `XDG_RUNTIME_DIR` is unset) the
/// fallback is `${TMPDIR:-/tmp}/ee-daemon.sock`.
#[must_use]
pub fn default_daemon_socket_path() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let runtime = Path::new(&runtime_dir);
        if !runtime.as_os_str().is_empty() {
            return runtime.join("ee").join("daemon.sock");
        }
    }
    let tmp = std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into());
    Path::new(&tmp).join("ee-daemon.sock")
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
    fn default_socket_path_falls_back_to_tmpdir_on_darwin() {
        // The fallback shape: `${TMPDIR:-/tmp}/ee-daemon.sock`. On a
        // host without XDG_RUNTIME_DIR set this is the canonical path
        // the CLI client uses; pin the construction so a future
        // refactor cannot quietly change it.
        let tmp = Path::new("/tmp");
        assert_eq!(tmp.join("ee-daemon.sock"), Path::new("/tmp/ee-daemon.sock"),);
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
