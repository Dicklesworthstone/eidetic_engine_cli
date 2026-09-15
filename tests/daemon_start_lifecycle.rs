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
//! - `daemon_start_reports_daemon_already_running_when_socket_is_live`:
//!   a repeated detached start against a live daemon refuses with an
//!   `ee.error.v2` / `daemon_already_running` envelope instead of
//!   replacing the first daemon's socket path.
//!
//! Cfg-gated to Unix because `ee daemon start` is Unix-only.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
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
        .args(["daemon", "start", "--json", "--socket"])
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

fn run_daemon_stop(socket_path: &Path) -> Result<Value, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["daemon", "stop", "--json", "--socket"])
        .arg(socket_path)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("failed to run `ee daemon stop`: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "daemon stop exited with {}; stdout={:?}; stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
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

fn daemon_pids_for_socket_path(socket_path: &Path) -> Result<Vec<u32>, String> {
    let executable = Path::new(env!("CARGO_BIN_EXE_ee"))
        .canonicalize()
        .map_err(|error| format!("resolve test CLI executable: {error}"))?;
    let output = Command::new("ps")
        .args(["-ww", "-eo", "pid=,command="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("failed to invoke ps: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "ps exited with {}; stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let current_pid = std::process::id();
    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((pid_field, command)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        if !is_owned_detached_daemon_command(command.trim_start(), &executable, socket_path) {
            continue;
        }
        let pid = pid_field
            .parse::<u32>()
            .map_err(|error| format!("failed to parse ps pid {pid_field:?}: {error}"))?;
        if pid != current_pid {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

fn is_owned_detached_daemon_command(command: &str, executable: &Path, socket_path: &Path) -> bool {
    command
        .strip_prefix(executable.to_string_lossy().as_ref())
        .and_then(|arguments| arguments.strip_prefix(" --workspace "))
        .is_some_and(|arguments| {
            arguments.ends_with(&format!(
                " daemon start --foreground --detached-session-child --socket {}",
                socket_path.display()
            ))
        })
}

fn wait_for_detached_daemon_pid(socket_path: &Path, timeout: Duration) -> Result<u32, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let pids = daemon_pids_for_socket_path(socket_path)?;
        if !pids.is_empty() {
            ensure(
                pids.len() == 1,
                format!("expected one owned daemon, found {pids:?}"),
            )?;
            return Ok(pids[0]);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!(
        "no detached daemon process containing {} appeared within {}ms",
        socket_path.display(),
        timeout.as_millis()
    ))
}

fn wait_for_no_daemon_pids(socket_path: &Path, timeout: Duration) -> TestResult {
    let deadline = Instant::now() + timeout;
    let mut last_pids = Vec::new();
    while Instant::now() < deadline {
        last_pids = daemon_pids_for_socket_path(socket_path)?;
        if last_pids.is_empty() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!(
        "detached daemon processes {last_pids:?} still matched {} after {}ms",
        socket_path.display(),
        timeout.as_millis()
    ))
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => return Err(format!("wait for child exit: {error}")),
        }
    }
    Err(format!(
        "child process {} did not exit within {}ms",
        child.id(),
        timeout.as_millis()
    ))
}

fn terminate_process(pid: u32, signal: &str) -> Result<(), String> {
    let output = Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("failed to invoke kill {signal} {pid}: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "kill {signal} {pid} exited with {}; stdout={:?}; stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// Stop only the detached child belonging to this test's executable and
/// complete socket argument. Cleanup is part of the test result, not a hint.
fn teardown_daemon(socket_path: &Path) -> TestResult {
    let graceful = (|| {
        if !daemon_pids_for_socket_path(socket_path)?.is_empty() {
            let stopped = run_daemon_stop(socket_path)?;
            ensure(
                stopped.pointer("/success").and_then(Value::as_bool) == Some(true),
                format!("owned daemon cleanup must report success:true; got {stopped}"),
            )?;
        }
        wait_for_no_daemon_pids(socket_path, Duration::from_secs(5))?;
        ensure(
            !socket_path.exists(),
            "daemon cleanup must remove its own socket",
        )
    })();
    if graceful.is_err() {
        // Emergency cleanup cannot turn a failed graceful shutdown into a
        // passing test. Re-identify exact owned PIDs before signaling them.
        let emergency = (|| {
            for pid in daemon_pids_for_socket_path(socket_path)? {
                terminate_process(pid, "-KILL")?;
            }
            wait_for_no_daemon_pids(socket_path, Duration::from_secs(5))
        })();
        return combine_test_results(graceful, emergency);
    }
    graceful
}

fn combine_test_results(result: TestResult, cleanup: TestResult) -> TestResult {
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(format!("{error}; additionally cleanup failed: {cleanup}"))
        }
    }
}

#[test]
fn daemon_start_foreground_sigterm_shuts_down_and_unlinks_socket() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-foreground-sigterm.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["daemon", "start", "--foreground", "--socket"])
        .arg(&socket_path)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn foreground daemon: {error}"))?;

    let result: TestResult = (|| {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "foreground daemon stdout was not piped".to_owned())?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|error| format!("read startup envelope: {error}"))?;
        let envelope: Value = serde_json::from_str(line.trim()).map_err(|error| {
            format!("startup envelope is not valid JSON: {error}; line={line:?}")
        })?;
        ensure(
            envelope.pointer("/success").and_then(Value::as_bool) == Some(true),
            format!("foreground start must report success:true; got {envelope}"),
        )?;
        ensure(
            envelope
                .pointer("/data/foreground")
                .and_then(Value::as_bool)
                == Some(true),
            format!("foreground start data.foreground must be true; got {envelope}"),
        )?;
        ensure(
            UnixStream::connect(&socket_path).is_ok(),
            "foreground daemon socket must be connectable before SIGTERM",
        )?;

        terminate_process(child.id(), "-TERM")?;
        let status = wait_for_child_exit(&mut child, Duration::from_secs(5))?;
        ensure(
            status.success(),
            format!("foreground daemon should exit successfully after SIGTERM; got {status}"),
        )?;
        ensure(
            !socket_path.exists(),
            "foreground daemon must unlink its socket during SIGTERM shutdown",
        )
    })();

    let cleanup = (|| {
        if child
            .try_wait()
            .map_err(|error| format!("inspect foreground child exit: {error}"))?
            .is_none()
        {
            terminate_process(child.id(), "-KILL")?;
            wait_for_child_exit(&mut child, Duration::from_secs(5))?;
        }
        ensure(
            !socket_path.exists(),
            "foreground shutdown must remove its socket",
        )
    })();
    combine_test_results(result, cleanup)
}

#[test]
fn daemon_start_detached_socket_is_connectable_before_success() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-lifecycle.sock");

    let result: TestResult = (|| {
        let envelope = run_daemon_start(&socket_path)?;
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

    combine_test_results(result, teardown_daemon(&socket_path))
}

#[test]
fn daemon_stop_detached_child_exits_and_unlinks_socket() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-stop-detached.sock");

    let result: TestResult = (|| {
        let envelope = run_daemon_start(&socket_path)?;
        ensure(
            envelope.pointer("/success").and_then(Value::as_bool) == Some(true),
            format!("detached start must report success:true; got {envelope}"),
        )?;
        let daemon_pid = wait_for_detached_daemon_pid(&socket_path, Duration::from_secs(2))?;
        ensure(
            daemon_pid > 0,
            format!("detached daemon pid must be positive; got {daemon_pid}"),
        )?;

        let stopped = run_daemon_stop(&socket_path)?;
        ensure(
            stopped.pointer("/success").and_then(Value::as_bool) == Some(true),
            format!("daemon stop must report success:true; got {stopped}"),
        )?;
        ensure(
            stopped.pointer("/data/removed").and_then(Value::as_bool) == Some(true),
            format!("daemon stop must report data.removed:true; got {stopped}"),
        )?;
        wait_for_no_daemon_pids(&socket_path, Duration::from_secs(5))?;
        ensure(
            !socket_path.exists(),
            "daemon stop must let the daemon unlink its socket during shutdown",
        )
    })();

    combine_test_results(result, teardown_daemon(&socket_path))
}

#[test]
fn daemon_start_reports_daemon_already_running_when_socket_is_live() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-already-running.sock");

    let result: TestResult = (|| {
        let first = run_daemon_start(&socket_path)?;
        ensure(
            first.pointer("/success").and_then(Value::as_bool) == Some(true),
            format!("first detached start must report success:true; got {first}"),
        )?;
        let original_pid = wait_for_detached_daemon_pid(&socket_path, Duration::from_secs(2))?;

        let second = run_daemon_start(&socket_path)?;
        ensure(
            second.pointer("/schema").and_then(Value::as_str) == Some("ee.error.v2"),
            format!("second detached start must emit ee.error.v2; got {second}"),
        )?;
        ensure(
            second.pointer("/error/code").and_then(Value::as_str) == Some("daemon_already_running"),
            format!("second detached start must report daemon_already_running; got {second}"),
        )?;
        ensure(
            second
                .pointer("/error/repair")
                .and_then(Value::as_str)
                .is_some(),
            format!("daemon_already_running error must include repair guidance; got {second}"),
        )?;
        ensure(
            daemon_pids_for_socket_path(&socket_path)? == vec![original_pid],
            "rejected second start must preserve exactly the original child",
        )?;
        Ok(())
    })();

    combine_test_results(result, teardown_daemon(&socket_path))
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

    let result = (|| {
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
            format!(
                "unbindable start must carry daemon_start_failed; got degraded codes {codes:?}"
            ),
        )?;
        Ok(())
    })();
    combine_test_results(result, teardown_daemon(&socket_path))
}

#[test]
fn detached_daemon_process_identity_requires_exact_executable_and_socket() {
    let executable = Path::new("/owned build/ee");
    let socket = Path::new("/tmp/owned/daemon.sock");
    let prefix = "/owned build/ee --workspace /workspace daemon start --foreground --detached-session-child --socket ";
    assert!(is_owned_detached_daemon_command(
        &format!("{prefix}{}", socket.display()),
        executable,
        socket
    ));
    for command in [
        format!("{prefix}/tmp/owned/daemon.sock.other"),
        format!("{prefix}/tmp/owned/daemon.sock --other"),
        "/different/ee --workspace /workspace daemon start --foreground --detached-session-child --socket /tmp/owned/daemon.sock".to_owned(),
        "/owned build/ee --workspace /workspace daemon start --foreground --socket /tmp/owned/daemon.sock".to_owned(),
        "/owned build/ee --workspace /workspace search /tmp/owned/daemon.sock".to_owned(),
    ] {
        assert!(!is_owned_detached_daemon_command(&command, executable, socket), "unowned argv matched: {command}");
    }
}
