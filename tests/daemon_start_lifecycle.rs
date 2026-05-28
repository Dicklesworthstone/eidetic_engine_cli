//! Lifecycle-honesty coverage for `ee daemon start` (bd-37o8k).
//!
//! The detached `ee daemon start` path used to emit `success:true` to
//! stdout while `std::mem::forget(handle)` + parent-process exit killed
//! the in-process accept thread, leaving an orphan socket file that
//! refused every connection. A multi-agent harness that branched on
//! `success == true` and then `connect(2)`'d to the printed socket path
//! got ECONNREFUSED — a tier-1 surface lie.
//!
//! The fix spawns the listener as a true detached child process and
//! polls the socket for connectability before emitting success. These
//! tests pin the corrected contract end-to-end against the real `ee`
//! binary:
//!
//! - `daemon_start_detached_socket_is_connectable_before_success`:
//!   after `ee daemon start` returns `success:true`, a UDS connect to
//!   the printed `socketPath` succeeds. This is the inverse of the bug.
//! - `daemon_start_emits_daemon_start_failed_on_unbindable_socket`:
//!   when the child can never bind (parent dir is a regular file), the
//!   envelope is `success:false` carrying the `daemon_start_failed`
//!   degraded code instead of a lie.
//!
//! Cfg-gated to Unix because `ee daemon start` is Unix-only.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// Run `ee daemon start` (detached) against the given socket path and
/// return the parsed `ee.response.v2` envelope. The parent CLI process
/// returns after its readiness probe resolves, so this blocks only for
/// as long as the probe takes (or its 5s deadline on failure).
fn run_daemon_start(socket_path: &Path) -> Result<Value, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["daemon", "start", "--socket"])
        .arg(socket_path)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("failed to run `ee daemon start`: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| {
            format!(
                "no JSON envelope on stdout; stdout={stdout:?} stderr={:?}",
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
    serde_json::from_str(line)
        .map_err(|error| format!("envelope is not valid JSON: {error}; line={line:?}"))
}

/// Best-effort teardown: kill any detached daemon child still listening
/// on `socket_path`. The child's argv contains the unique tempdir
/// socket path, so a `pkill -f` match is precise. Also unlinks the
/// socket file so the tempdir drop is clean.
fn teardown_daemon(socket_path: &Path) {
    let needle = socket_path.display().to_string();
    let _ = Command::new("pkill")
        .arg("-f")
        .arg(&needle)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if socket_path.exists() {
        let _ = fs::remove_file(socket_path);
    }
}

#[test]
fn daemon_start_detached_socket_is_connectable_before_success() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-lifecycle.sock");

    let envelope = run_daemon_start(&socket_path)?;
    let result: TestResult = (|| {
        ensure(
            envelope
                .pointer("/success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            format!("detached start must report success:true; got {envelope}"),
        )?;
        ensure(
            envelope
                .pointer("/data/foreground")
                .and_then(Value::as_bool)
                == Some(false),
            format!("detached start data.foreground must be false; got {envelope}"),
        )?;
        let reported = envelope
            .pointer("/data/socketPath")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("envelope missing data.socketPath; got {envelope}"))?;
        ensure(
            reported == socket_path.display().to_string(),
            format!(
                "reported socketPath must equal the requested path; got {reported}, want {}",
                socket_path.display()
            ),
        )?;

        // The core anti-regression: the success envelope promised a
        // live daemon, so a connect to the printed path must succeed
        // right now (not ECONNREFUSED against an orphan socket file).
        // A short retry budget tolerates probe/connect scheduling skew
        // on loaded CI hosts but stays far under the would-be-bug
        // signature (a connect that never succeeds).
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut connected = false;
        let mut last_err = None;
        while Instant::now() < deadline {
            match UnixStream::connect(&socket_path) {
                Ok(_stream) => {
                    connected = true;
                    break;
                }
                Err(error) => {
                    last_err = Some(error);
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        ensure(
            connected,
            format!(
                "UDS connect to the printed socketPath must succeed after success:true; \
                 last error: {last_err:?}"
            ),
        )?;
        Ok(())
    })();

    teardown_daemon(&socket_path);
    result
}

#[test]
fn daemon_start_emits_daemon_start_failed_on_unbindable_socket() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    // Make the socket's parent path a regular FILE so the child's
    // `create_dir_all` / bind can never succeed; the parent's readiness
    // probe observes the child exit and must emit the honest failure.
    let blocker = temp.path().join("not-a-dir");
    fs::write(&blocker, b"blocker").map_err(|error| format!("write blocker: {error}"))?;
    let socket_path = blocker.join("daemon.sock");

    let envelope = run_daemon_start(&socket_path)?;

    ensure(
        envelope.pointer("/success").and_then(Value::as_bool) == Some(false),
        format!("unbindable start must report success:false; got {envelope}"),
    )?;
    let codes: Vec<&str> = envelope
        .pointer("/degraded")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.pointer("/code").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    ensure(
        codes.contains(&"daemon_start_failed"),
        format!("unbindable start must carry daemon_start_failed; got degraded codes {codes:?}"),
    )?;
    Ok(())
}
