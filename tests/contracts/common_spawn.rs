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

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// How many real-binary spawns may run at once.
///
/// bd-7vtqm made this gate a crate-wide mutex -- effectively a permit count of
/// one -- as CPU-contention hygiene. That is a throttle, not a correctness
/// device: no two serialized spawns share any state. Of the 25 contending
/// modules, 14 invoke `ee` with no workspace at all (`--version`, `--help`,
/// `capabilities`) and 11 build a fresh `tempfile::tempdir()` per test. None
/// uses a fixed shared path, so there is no shared database, no shared flock,
/// and nothing for two concurrent spawns to corrupt.
///
/// A permit count of one made the suite's wall clock the SUM of every spawn,
/// which is why contracts could not finish inside its cap even after the
/// deadlock was bounded. Four permits keep the thundering-herd protection
/// bd-7vtqm wanted while letting the OS scheduler do the job it exists for.
const REAL_EE_MAX_CONCURRENT_SPAWNS: usize = 4;

fn max_concurrent_spawns() -> usize {
    std::env::var("EE_CONTRACTS_MAX_CONCURRENT_SPAWNS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|permits| *permits > 0)
        .unwrap_or(REAL_EE_MAX_CONCURRENT_SPAWNS)
}

/// Permits currently in use, paired with the condvar waiters block on.
static REAL_EE_PERMITS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// Releases its permit on drop, including when the test panics, so a failing
/// peer cannot leak capacity the way a poisoned mutex once could.
struct SpawnPermit;

impl Drop for SpawnPermit {
    fn drop(&mut self) {
        let (lock, waiters) = &REAL_EE_PERMITS;
        let mut in_use = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *in_use = in_use.saturating_sub(1);
        waiters.notify_one();
    }
}

fn lock_real_ee_serial() -> SpawnPermit {
    let permits = max_concurrent_spawns();
    let (lock, waiters) = &REAL_EE_PERMITS;
    // A panicked peer poisons the gate, but it guards scheduling hygiene only;
    // poisoned or not, later spawns must proceed.
    let mut in_use = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    while *in_use >= permits {
        in_use = waiters
            .wait(in_use)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    *in_use += 1;
    SpawnPermit
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

    // Drain both pipes on their own threads, CONCURRENTLY with the wait.
    //
    // This is not optional and it is not an optimisation. `Command::output()`
    // drains while it waits (`wait_with_output` reads both pipes). A poll loop
    // that waits WITHOUT draining deadlocks any child that writes more than the
    // pipe buffer -- ~64 KiB -- because the child blocks on write, never exits,
    // and `try_wait` therefore never reports exit. The first version of this
    // function had exactly that bug: it polled `try_wait` and only drained
    // afterwards. It did not fire in practice because the largest contracts
    // spawn measures ~30 KB (`ee capabilities --json`), which is under the
    // buffer -- so the defect was real, latent, and masked by output size.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || drain(stdout_pipe));
    let stderr_reader = std::thread::spawn(move || drain(stderr_pipe));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            let _ = child.kill();
            let _ = child.wait();
            // Join before returning so the reader threads cannot outlive this
            // call; killing the child closes the pipes, so both return.
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
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
    };

    Ok(Output {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

/// Read a child pipe to end, returning what arrived.
///
/// A read error yields what was collected rather than failing the spawn: the
/// caller's contract is the child's exit status plus its output, and a partial
/// read is strictly more informative than an error that discards both.
fn drain(pipe: Option<impl Read>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut buffer);
    }
    buffer
}

/// A private workspace registry for one spawn.
///
/// `ee` resolves its workspace registry to EE_WORKSPACE_REGISTRY, else
/// XDG_DATA_HOME/ee/workspaces.db, else <home>/.local/share/ee/workspaces.db --
/// ONE SQLite file, opened for write, under the real home directory. Its
/// connection sets `PRAGMA busy_timeout = 0` (src/db/mod.rs:2209), so
/// concurrent writers do not queue, they FAIL.
///
/// Of the 24 modules that spawn here, exactly one sets EE_WORKSPACE_REGISTRY.
/// Eight explicitly `env_remove` it and fifteen never mention it -- so 23 of 24
/// resolve to that shared file, and none of them overrides HOME. The suite is
/// safe today only because no contracts test spawns a verb that writes the
/// registry: the sole production writer is `alias_workspace`
/// (src/core/workspace.rs:2121), which nothing here invokes. Verified by probe
/// as well as by reading -- `ee init`, `status`, `doctor` and `capabilities`
/// each leave the registry uncreated.
///
/// That is safety by accident. Giving each spawn its own path makes it safety
/// by construction, and it is a precondition for ever raising concurrency here
/// (bd-contracts-serialized-spawn-queue-loibi).
fn isolated_registry_path() -> PathBuf {
    static SPAWN_COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = SPAWN_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join("ee-contracts-registry")
        .join(format!("{}-{sequence}", std::process::id()))
        .join("workspaces.db")
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
    command
        .args(&arguments)
        .env("EE_WORKSPACE_REGISTRY", isolated_registry_path());
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
    // AFTER `configure` deliberately: eight modules `env_remove` this variable
    // to avoid inheriting an ambient registry, which silently drops them onto
    // the global one. Their intent is isolation; setting it here delivers that
    // intent rather than overriding it.
    command.env("EE_WORKSPACE_REGISTRY", isolated_registry_path());
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

/// A child that writes more than the pipe buffer must still complete.
///
/// This is the regression test for the bug the first version of
/// `output_with_deadline` had: polling `try_wait` without draining. A child
/// writing past ~64 KiB blocks on write, so it never exits, so `try_wait` never
/// reports exit, and the call burns the whole timeout before killing a process
/// that was only trying to talk. 256 KiB is comfortably past the buffer on
/// every platform we run on.
///
/// It did not fire in production only because the largest contracts spawn is
/// ~30 KB. A test that used a small payload would have passed against the
/// broken version, which is why the size is the point of this test.
#[cfg(unix)]
#[test]
fn a_child_that_outgrows_the_pipe_buffer_still_completes() {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("i=0; while [ $i -lt 4096 ]; do printf '%064d' $i; i=$((i+1)); done");
    let output = output_with_deadline(&mut command, Duration::from_secs(60))
        .expect("a large-output child must not be reported as a timeout");
    assert!(
        output.status.success(),
        "generator should exit 0: {output:?}"
    );
    assert_eq!(
        output.stdout.len(),
        4096 * 64,
        "stdout must be captured in full, not truncated at the pipe buffer"
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
