//! Shared real-binary (`CARGO_BIN_EXE_ee`) spawn hygiene for contracts tests.
//!
//! bd-7vtqm: under an unfiltered `cargo test --test contracts`, libtest runs
//! each module's tests on its own worker threads, and every real-binary
//! invocation competes for CPU with every other one. The resume bridges first
//! serialized their own spawns behind a file-local mutex
//! (bd-resume-verb-v0f57, commit 7f50e5c7); this module promotes that pattern
//! to one crate-wide lock so ALL direct binary spawns serialize as contention
//! hygiene. Per-module semantics are unchanged: callers keep their argument
//! shapes, environment tweaks, current_dir, and failure text; only the
//! serialization gate is shared.

use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Crate-wide serialization gate for every real-binary spawn below.
static REAL_EE_SERIAL: Mutex<()> = Mutex::new(());

fn lock_real_ee_serial() -> MutexGuard<'static, ()> {
    // A panicked peer poisons the gate, but the lock guards scheduling
    // hygiene only; poisoned or not, later spawns must proceed.
    REAL_EE_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Wall-clock cap for one serialized spawn.
///
/// This cap does NOT exist to police test speed. It exists because the
/// serialization guard above is held ACROSS the child's whole lifetime, so a
/// single `ee` invocation that never exits holds the crate-wide lock forever
/// and wedges every other contracts module behind it -- 154 invocation sites
/// across 24 modules at the time this was added
/// (bd-contracts-serialized-spawn-queue-loibi).
///
/// Poisoning was already handled: `lock_real_ee_serial` recovers from a
/// PANICKED peer. A HANGING child was the remaining hole, and it is the worse
/// one, because `ee`'s database write lock is an OS flock with `busy_timeout 0`
/// -- a spawn blocked there while holding this mutex is a cross-layer deadlock
/// with no timeout on either layer.
///
/// 120s is deliberately far above the observed per-spawn cost (~2s, with `init`
/// and migrate operations heavier). It is a deadlock bound, not a budget: a
/// value tight enough to catch slowness would make the suite flaky under swarm
/// load, which is the failure mode this whole module was written to avoid.
const REAL_EE_SPAWN_TIMEOUT_SECS: u64 = 120;

fn spawn_timeout() -> Duration {
    std::env::var("EE_CONTRACTS_SPAWN_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or_else(
            || Duration::from_secs(REAL_EE_SPAWN_TIMEOUT_SECS),
            Duration::from_secs,
        )
}

/// `Command::output()` with a deadline, so a stuck child cannot hold the
/// crate-wide lock forever.
///
/// Same shape as `run_capped` in `tests/cli_no_panic_smoke.rs`: spawn with piped
/// stdio, poll `try_wait`, and re-check once more before killing. That second
/// check is not redundant -- without it a child that exits between the deadline
/// test and the kill is reported as a timeout, turning a pass into a spurious
/// failure.
///
/// stdio is set AFTER the caller's `configure` runs, matching `output()`, which
/// also overrides those handles. stdin is nulled because a child waiting on
/// stdin is its own hang class.
fn output_with_timeout(command: &mut Command) -> std::io::Result<Output> {
    output_with_deadline(command, spawn_timeout())
}

/// The deadline loop, with the cap passed in so it can be tested without
/// mutating a process-global env var that every other spawn reads.
fn output_with_deadline(command: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            if child.try_wait()?.is_some() {
                return child.wait_with_output();
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "real ee spawn exceeded {timeout:?} and was killed while holding \
                     the crate-wide serialization lock; raise \
                     EE_CONTRACTS_SPAWN_TIMEOUT_SECS if this host is genuinely slower"
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Path of the real `ee` binary provided by the Cargo test harness. This is
/// the only `CARGO_BIN_EXE_ee` reference allowed in `tests/contracts`.
pub fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

/// Spawn the real binary with `args` while holding the crate-wide
/// serialization lock. Failure text matches the conventional per-module
/// helpers: `failed to run ee <args joined by spaces>: <io error>`.
pub fn serialized_real_ee<I, S>(args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let arguments: Vec<String> = args
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();
    let _serial_guard = lock_real_ee_serial();
    let mut command = Command::new(ee_binary());
    command.args(&arguments);
    output_with_timeout(&mut command)
        .map_err(|error| format!("failed to run ee {}: {error}", arguments.join(" ")))
}

/// Caller-configured variant: holds the crate-wide serialization lock while
/// `configure` mutates a fresh command pre-seeded with the real binary path.
/// Use this for workspace prefixes, env removals or overrides, `current_dir`,
/// or custom failure text; the io error is returned unmapped so each call
/// site keeps its original message.
pub fn serialized_real_ee_with<F>(configure: F) -> std::io::Result<Output>
where
    F: FnOnce(&mut Command),
{
    let _serial_guard = lock_real_ee_serial();
    let mut command = Command::new(ee_binary());
    configure(&mut command);
    output_with_timeout(&mut command)
}

/// The deadline must actually fire on a child that never exits.
///
/// Without this the timeout is an untested guard: the suite would pass whether
/// or not the deadline works, because no existing test spawns anything that
/// hangs. That is the shape this whole bead is about -- a mechanism that looks
/// present and establishes nothing.
#[cfg(unix)]
#[test]
fn the_deadline_kills_a_child_that_never_exits() {
    let started = Instant::now();
    let mut command = Command::new("sleep");
    command.arg("120");
    let result = output_with_deadline(&mut command, Duration::from_millis(400));
    let error = result.expect_err("a sleeping child must not return output");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::TimedOut,
        "a killed child must report TimedOut, got {error:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the deadline did not bound the wait; elapsed {:?}",
        started.elapsed()
    );
}

/// The paired positive: a child that exits normally is NOT reported as a
/// timeout, and its output still comes back.
///
/// A deadline implementation that always killed would satisfy the test above
/// and be useless. This is what distinguishes a bound from a break.
#[cfg(unix)]
#[test]
fn the_deadline_leaves_a_fast_child_alone() {
    let mut command = Command::new("echo");
    command.arg("contracts-spawn-probe");
    let output = output_with_deadline(&mut command, Duration::from_secs(30))
        .expect("a fast child must return output");
    assert!(output.status.success(), "echo should succeed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("contracts-spawn-probe"),
        "stdout must still be captured through the deadline path: {output:?}"
    );
}
