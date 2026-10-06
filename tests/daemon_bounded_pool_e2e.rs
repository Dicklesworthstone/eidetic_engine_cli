//! Regression test for bd-jnyui — the daemon accept loop must cap the
//! number of in-flight per-connection worker threads at
//! `DAEMON_MAX_INFLIGHT` (default 32) and refuse excess connections
//! with a framed `daemon_overloaded` response rather than spawning an
//! unbounded thread per accept.
//!
//! Threat model (local-only, but the actual swarm threat surface): a
//! single attacker who can reach the UDS opens connections faster than
//! the kernel can hand them to worker threads and announces max-sized
//! frames while sending nothing, pinning each worker for the full read
//! timeout. Before this fix `run_accept_loop` spawned a fresh
//! `std::thread::Builder` per accept, so ~2k connections committed
//! enough address space (~2 MiB stack + up to 4 MiB request buffer
//! each) to trip the OOM killer / macOS per-process thread limit and
//! wedge every other daemon-mode CLI on the box.
//!
//! The test opens 100 simultaneous connections against a daemon whose
//! cap is the default 32 and asserts:
//!   * the bounded pool never spawns more than 32 concurrent workers
//!     (measured as the count of connections the daemon *held* open —
//!     i.e. a worker is blocked reading the request frame we never
//!     send), and
//!   * the remaining 68 connections each receive a framed
//!     `daemon_overloaded` response (the connection is refused, not
//!     silently queued).
//!
//! Cfg-gated to Unix because the UDS server is Unix-only.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use ee::daemon::protocol::DaemonResponse;
use ee::daemon::{DAEMON_MAX_INFLIGHT, DAEMON_OVERLOADED_CODE, server::start_server};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// A daemon socket path that `start_server` will actually publish.
///
/// MEASURED on an RCH worker before this existed: `tempfile::tempdir()` put the socket under
/// the project sync root and `start_server` refused outright --
///
///   Refusing to publish daemon socket under insecure parent
///   /data/rch/eidetic_engine_cli/paired-HASH/.rch-tmp/.tmpEkg6gh: parent mode 0o775 grants
///   group or other access; expected 0o700 or stricter
///
/// The tempdir itself is 0o700, but the ancestor walk (`src/daemon/server.rs` ~:1037) checks
/// EVERY ancestor, and the worker's sync root is 0o775. The assembled path was ALSO 112 bytes
/// against a usable 107 on Linux, so both limits were violated; only the mode was reported,
/// because that check runs first. Fixing one without the other would have moved the error,
/// not removed it.
///
/// `sun_path` is 108 bytes on Linux and 104 on macOS. The SMALLER value is used deliberately
/// so this guard refuses a few bytes early on the Linux fleet and never wrongly admits a path
/// on either platform. Do not "correct" it to 108. The 37-byte allowance is `start_server`'s
/// atomic publication suffix -- it binds `socket_path` plus `format!(".tmp.{}",
/// uuid.simple())` first, then renames (`src/daemon/server.rs:970`), so a check against the
/// final path alone passes paths that are then refused.
fn secure_socket_path(file_name: &str) -> Result<PathBuf, String> {
    // Short on purpose: a nanosecond stamp would eat a fifth of the usable budget.
    static NEXT_SOCKET_DIR: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new("/tmp").join(format!(
        "eebp-{}-{}",
        std::process::id(),
        NEXT_SOCKET_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).map_err(|error| format!("fixture: create socket dir: {error}"))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("fixture: secure socket dir: {error}"))?;
    let path = dir.join(file_name);
    const PUBLISH_SUFFIX_BYTES: usize = 37;
    const SUN_PATH_BUDGET: usize = 104;
    let length = path.as_os_str().len();
    if length + PUBLISH_SUFFIX_BYTES >= SUN_PATH_BUDGET {
        return Err(format!(
            "fixture: socket path is {length} bytes and start_server appends \
             {PUBLISH_SUFFIX_BYTES} for atomic publication, exceeding sun_path \
             ({SUN_PATH_BUDGET}): {}",
            path.display()
        ));
    }
    Ok(path)
}

/// What a single probe connection observed from the daemon.
#[derive(Debug)]
enum Outcome {
    /// The daemon wrote a framed `daemon_overloaded` response: the
    /// accept loop refused the connection because the pool was full.
    Overloaded,
    /// The daemon accepted the connection and a worker is blocked
    /// reading the request frame we never send: this connection
    /// occupies one of the bounded worker slots.
    Held,
    /// The daemon wrote a framed response whose code was something
    /// other than `daemon_overloaded` (unexpected for a probe that
    /// never sends a request frame).
    OtherResponse(String),
    /// The connection could not be established or produced an
    /// unexpected I/O error.
    Error(String),
}

#[test]
fn daemon_accept_loop_caps_workers_and_rejects_excess_with_overloaded() -> TestResult {
    let socket_path = secure_socket_path("ee-daemon-overload.sock")?;

    // The default cap is exactly the value bd-jnyui pins (32); the
    // test deliberately does NOT set EE_DAEMON_MAX_INFLIGHT so it
    // exercises the production default and stays insensitive to env
    // state in the parallel test runner.
    let cap = DAEMON_MAX_INFLIGHT;
    let total = cap + 68; // 100 connections at the default cap.
    let expected_overloaded = total - cap; // 68.

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;
    // Let the accept thread reach `listener.accept()` before the flood.
    thread::sleep(Duration::from_millis(100));

    let barrier = Arc::new(Barrier::new(total + 1));
    let release = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Outcome>();

    let mut clients = Vec::with_capacity(total);
    for _ in 0..total {
        let socket_path = socket_path.clone();
        let barrier = Arc::clone(&barrier);
        let release = Arc::clone(&release);
        let tx = tx.clone();
        clients.push(thread::spawn(move || {
            let stream = UnixStream::connect(&socket_path);
            // Synchronize so every probe has issued its connect before
            // any probe starts reading; this saturates the accept
            // backlog so the bounded pool is the thing under test.
            barrier.wait();
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = tx.send(Outcome::Error(format!("connect: {error}")));
                    return;
                }
            };
            // Give the single-threaded accept loop time to drain the
            // whole backlog: by now the 32 accepted connections each
            // have a worker blocked on our (never-sent) request frame,
            // and the 68 refused connections each have a framed
            // daemon_overloaded response waiting to be read.
            thread::sleep(Duration::from_millis(300));
            stream.set_read_timeout(Some(Duration::from_secs(3))).ok();

            let mut prefix = [0_u8; 4];
            match stream.read_exact(&mut prefix) {
                Ok(()) => {
                    let announced = u32::from_be_bytes(prefix) as usize;
                    let mut body = vec![0_u8; announced];
                    match stream.read_exact(&mut body) {
                        Ok(()) => match serde_json::from_slice::<DaemonResponse>(&body) {
                            Ok(response) => {
                                let code = response
                                    .error
                                    .as_ref()
                                    .map(|error| error.code.clone())
                                    .unwrap_or_default();
                                if code == DAEMON_OVERLOADED_CODE {
                                    let _ = tx.send(Outcome::Overloaded);
                                } else {
                                    let _ = tx.send(Outcome::OtherResponse(code));
                                }
                            }
                            Err(error) => {
                                let _ = tx.send(Outcome::Error(format!("decode: {error}")));
                            }
                        },
                        Err(error) => {
                            let _ = tx.send(Outcome::Error(format!("read body: {error}")));
                        }
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    // No bytes within the window: the daemon accepted
                    // us and a worker is blocked reading the request
                    // frame we never send. This connection holds one
                    // bounded worker slot. Keep it open until the test
                    // releases us so the peak-concurrency measurement
                    // is stable, then drop the stream (the worker sees
                    // EOF and frees its permit).
                    let _ = tx.send(Outcome::Held);
                    while !release.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(20));
                    }
                }
                Err(error) => {
                    let _ = tx.send(Outcome::Error(format!("read prefix: {error}")));
                }
            }
        }));
    }

    // Release the barrier so every probe's connect has happened.
    barrier.wait();

    let mut overloaded = 0usize;
    let mut held = 0usize;
    let mut other = Vec::new();
    let mut errors = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    for _ in 0..total {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Outcome::Overloaded) => overloaded += 1,
            Ok(Outcome::Held) => held += 1,
            Ok(Outcome::OtherResponse(code)) => other.push(code),
            Ok(Outcome::Error(message)) => errors.push(message),
            Err(_) => break,
        }
    }

    // Let the held probes close so the worker threads drain their
    // permits, then shut the daemon down cleanly.
    release.store(true, Ordering::SeqCst);
    for client in clients {
        let _ = client.join();
    }
    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;

    ensure(
        errors.is_empty(),
        format!("probe connections reported errors: {errors:?}"),
    )?;
    ensure(
        other.is_empty(),
        format!("probes received non-overloaded response codes: {other:?}"),
    )?;
    // The core security invariant: the bounded pool never spawns more
    // than the cap. Held connections are a 1:1 proxy for live workers.
    ensure(
        held <= cap,
        format!("held {held} connections exceeds worker cap {cap} — bounded pool leaked slots"),
    )?;
    // Clean partition: every connection that was NOT held got an
    // explicit daemon_overloaded refusal (none were silently queued).
    ensure(
        overloaded + held == total,
        format!(
            "expected every connection accounted for: held={held} + overloaded={overloaded} != {total}"
        ),
    )?;
    // The deterministic nominal split at a 32-cap / 100-connection
    // flood: exactly 32 workers held, exactly 68 refusals.
    ensure(
        held == cap,
        format!("expected exactly {cap} held workers; got {held} (overloaded={overloaded})"),
    )?;
    ensure(
        overloaded == expected_overloaded,
        format!(
            "expected exactly {expected_overloaded} daemon_overloaded responses; got {overloaded} (held={held})"
        ),
    )?;
    Ok(())
}
