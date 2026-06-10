# ADR 0055: Daemon SocketBroker lifecycle boundary

Status: Accepted
Date: 2026-06-09
Bead: bd-2yg7d.1

## Context

The optional daemon uses a local Unix-domain socket so future hot-mode reads can
reuse resident state without making daemon mode mandatory. The socket lifecycle
is security-sensitive: a bad path choice, a loose parent directory, a stale
socket race, or an unchecked peer can turn the daemon into a local confused
deputy.

The current implementation already contains several hardening fixes, but they
are spread across `src/daemon/mod.rs` and `src/daemon/server.rs`. Future source
work should not rediscover those decisions or accidentally move one invariant
without the others. The next daemon hardening slice therefore needs a named
SocketBroker boundary: one module owns path resolution, publication, liveness,
peer admission, and teardown invariants.

## Current Inventory

Source snapshot: 2026-06-09 on `main`.

| Responsibility | Current location | State |
| --- | --- | --- |
| Canonical socket path | `default_daemon_socket_path`, `default_daemon_socket_path_with`, and `runtime_dir_is_shared_tmp_root` in `src/daemon/mod.rs` | Hardened: Linux uses non-shared `XDG_RUNTIME_DIR`; macOS and shared-temp fallbacks use `${TMPDIR:-/tmp}/ee-${uid}/daemon.sock`. |
| Current effective UID | `current_euid` in `src/daemon/mod.rs` | Hardened on Unix through `rustix::process::geteuid`; non-Unix is platform-gated before UDS start. |
| Parent directory creation and validation | `start_server_with_dispatch_policy` plus `validate_socket_parent` in `src/daemon/server.rs` | Hardened: creates missing parents with `0o700`, then requires a real directory owned by the current UID with no group/other bits. |
| Start-time serialization | `acquire_socket_publish_lock`, `socket_publish_lock_path`, `open_daemon_socket_lock_file`, and `configure_daemon_socket_lock_options` in `src/daemon/server.rs` | Partially brokered: lock file uses `0o600` and `O_NOFOLLOW`, but lock naming and lifecycle are embedded in the server start path. |
| Existing canonical path check | `start_server_with_dispatch_policy` and `existing_socket_accepts_connection` in `src/daemon/server.rs` | Hardened for non-socket refusal and live-daemon detection; stale sockets are replaced by atomic publish rather than pre-unlinked. |
| Temporary bind path | `temp_bind_path` in `src/daemon/server.rs` | Hardened enough for same-process and cross-process collision avoidance through pid plus monotonic counter. |
| Secure publish | `start_server_with_dispatch_policy` in `src/daemon/server.rs` | Hardened: bind temp path, set `0o600`, then atomically `rename` to the canonical path. |
| Shutdown wake and accept-loop join | `DaemonServerHandle::shutdown_with_worker_drain_timeout` in `src/daemon/server.rs` | Partially brokered: wake-by-connect and accept-thread join live on the handle, while socket removal lives in a free helper. |
| Socket removal | `remove_owned_socket_file` in `src/daemon/server.rs` | Hardened: tolerate `NotFound`, require socket file type, require current UID ownership before `remove_file`. |
| Peer credential authorization | `handle_connection` and `peer_uid` in `src/daemon/server.rs` | Hardened on Linux through `SO_PEERCRED`; non-Linux Unix fails closed until a safe wrapper exists. |
| Per-connection deadlines | `handle_connection` in `src/daemon/server.rs` | Hardened: write/read timeout failures return `daemon_setsockopt_failed` and drop instead of running deadline-less reads. |
| Backpressure | `InflightPool`, `run_accept_loop_with_spawner`, and `write_overloaded_response` in `src/daemon/server.rs` | Hardened: bounded worker permits and immediate framed refusal instead of unbounded queueing. |

## Decision

Introduce SocketBroker as the owner of daemon UDS lifecycle invariants before
new daemon surfaces grow more methods. The broker boundary should be extracted
from existing behavior, not rewritten from scratch.

The broker owns:

- path resolution and parent-directory privacy validation;
- publish-lock path construction and lock acquisition;
- stale/live socket classification;
- temp-path generation, bind, chmod, and atomic publish;
- owned-socket removal during explicit shutdown and `Drop`;
- platform peer-credential lookup behind a small trait or function boundary.

The daemon server remains responsible for:

- request framing and response framing;
- dispatch policy and per-method authorization;
- worker admission, panic supervision, and metrics;
- per-connection read/write deadlines.

The public behavior must not soften during extraction. A SocketBroker refactor
must preserve these fail-closed rules:

- never publish in a shared parent;
- never overwrite a non-socket path;
- never accept a peer whose UID cannot be verified;
- never delete a non-socket or other-owned file during cleanup;
- never start local daemon mode on non-Unix targets.

## Follow-Up Beads

Follow-up implementation beads created from this inventory:

- `bd-2yg7d.2` — extract the SocketBroker publish/remove API while preserving
  the current path, parent, lock, temp-bind, chmod, rename, and unlink behavior.
- `bd-2yg7d.3` — add adversarial SocketBroker fixture coverage for non-socket
  paths, insecure parents, stale sockets, publish-lock paths, and cleanup
  refusal.

## Rejected Alternatives

1. **Leave lifecycle logic inside `server.rs`.** Rejected because the hardening
   invariants are already too interdependent. Adding more daemon methods while
   path, publish, peer, and teardown behavior remain scattered makes future
   review harder.

2. **Build a new daemon supervisor first.** Rejected for this slice. ADR 0053
   already pins the panic boundary, and the current risk is socket lifecycle
   drift rather than process supervision.

3. **Loosen cleanup to remove anything at the socket path.** Rejected. The
   current `remove_owned_socket_file` type and UID checks are intentional and
   must stay intact.

4. **Treat non-Linux Unix peer credentials as allowed.** Rejected. Until a safe
   wrapper is implemented for each platform, the daemon must fail closed rather
   than assume same-user access.

## Verification Hooks

The extraction bead must keep existing daemon tests green through RCH-only Rust
verification. Before remote Cargo proof is available, docs/static work can use:

- `rg` for the inventory symbols listed above;
- `git diff --check` for docs and tracker changes;
- `br sync --status --json`, `br doctor --json --no-db`, and
  `br dep cycles --json` for tracker health.

The follow-up source beads should add or preserve focused coverage for:

- default path resolution across `XDG_RUNTIME_DIR`, shared temp roots, and
  missing environment;
- parent-directory ownership and mode rejection;
- non-socket canonical path refusal;
- stale socket replacement through temp bind plus atomic rename;
- removal refusal for non-socket and other-owned paths;
- Linux peer UID acceptance/refusal and non-Linux fail-closed behavior.

## Consequences

Future daemon UDS hardening work now has a single vocabulary and migration
boundary. Agents should first decide whether a change belongs in SocketBroker
or in request dispatch; mixing the two in one bead should be treated as a scope
smell unless a test proves the boundary itself is wrong.
