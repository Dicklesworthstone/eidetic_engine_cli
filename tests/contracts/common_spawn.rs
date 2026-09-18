//! Shared real-binary (`CARGO_BIN_EXE_ee`) spawn hygiene for contracts tests.
//!
//! bd-7vtqm: under an unfiltered `cargo test --test contracts`, libtest runs
//! each module's tests on its own worker threads, and every real-binary
//! invocation competes for CPU with every other one. The resume bridges first
//! serialized their own spawns behind a file-local mutex
//! (bd-resume-verb-v0f57, commit 7f50e5c7); this module promotes that pattern
//! to one crate-wide gate. That gate began as a mutex -- a permit count of one,
//! so ALL spawns serialized -- and is now a bounded semaphore
//! (bd-contracts-serialized-spawn-queue-loibi), because serializing 154 spawn
//! sites made the suite's wall clock their SUM and it could not finish inside
//! its cap. Per-module semantics are unchanged: callers keep their argument
//! shapes, environment tweaks, current_dir, and failure text; only the
//! admission gate is shared.

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

fn acquire_spawn_permit() -> SpawnPermit {
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

/// Wall-clock cap for one spawn.
///
/// This cap does NOT exist to police test speed. A permit is held ACROSS the
/// child's whole lifetime, so an `ee` invocation that never exits never gives
/// its permit back.
///
/// At the original permit count of one, that wedged the entire suite: a single
/// stuck child blocked all 154 invocation sites across 24 modules, which is
/// exactly how bv15 died at 920 of 1454 rows. At a bounded permit count the
/// blast radius is smaller but the leak is permanent -- one stuck child
/// permanently costs a quarter of the capacity, and four cost all of it. The
/// cap is still required; only the size of the failure changed
/// (bd-contracts-serialized-spawn-queue-loibi).
///
/// Poisoning was already handled: the permit gate recovers from a PANICKED
/// peer, and `SpawnPermit`'s `Drop` returns capacity even on unwind. A HANGING
/// child was the remaining hole, and it is the worse one, because `ee`'s
/// database write lock is an OS flock with `busy_timeout 0` -- a spawn blocked
/// there while holding a permit is a cross-layer deadlock with no timeout on
/// either layer.
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

/// `Command::output()` with a deadline, so a stuck child cannot hold its spawn
/// permit forever.
///
/// Same shape as `run_capped` in `tests/cli_no_panic_smoke.rs`: spawn with piped
/// stdio, poll `try_wait`, and re-check once more before killing. That second
/// check is not redundant -- without it a child that exits between the deadline
/// test and the kill is reported as a timeout, turning a pass into a spurious
/// failure.
///
/// stdio is set AFTER the caller's `configure` runs, matching `output()`, which
/// also overrides those handles -- which is why a caller cannot simply set
/// stdin itself, and why supplying one has to be a parameter. stdin defaults to
/// null because a child waiting on stdin is its own hang class; `Some(handle)`
/// is for the surfaces that must read it, and the caller owns reaching EOF.
fn output_with_timeout(command: &mut Command, stdin: Option<Stdio>) -> std::io::Result<Output> {
    output_with_deadline(command, spawn_timeout(), stdin)
}

/// The deadline loop, with the cap passed in so it can be tested without
/// mutating a process-global env var that every other spawn reads.
fn output_with_deadline(
    command: &mut Command,
    timeout: Duration,
    stdin: Option<Stdio>,
) -> std::io::Result<Output> {
    // `None` keeps the original null. A caller that supplies a handle takes
    // responsibility for it reaching EOF: `ee remember --batch --stdin` reads
    // until the pipe closes, and a handle that never closes is the hang class
    // the null was here to prevent. Passing an opened File satisfies that by
    // construction -- it EOFs at the end of the file with no writer thread and
    // no close protocol to get wrong.
    command
        .stdin(stdin.unwrap_or_else(Stdio::null))
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
                     its spawn permit; raise \
                     EE_CONTRACTS_SPAWN_TIMEOUT_SECS if this host is genuinely slower"
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    // Both joins can now fail, and that is the point: a reader that died used to
    // be indistinguishable from a child that printed nothing.
    let stdout = join_reader(stdout_reader, "stdout")?;
    let stderr = join_reader(stderr_reader, "stderr")?;

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Read a child pipe to end, returning what arrived AND why reading stopped.
///
/// The original contract was right about the bytes and wrong about the error:
/// "a partial read is strictly more informative than an error that discards
/// both" argues for keeping what was collected, and says nothing in favour of
/// throwing the failure away. Those are separable, and this returns both.
///
/// Keeping the error is what makes a partial read DISTINGUISHABLE from a child
/// that simply wrote nothing. Without it the two are byte-identical at the call
/// site, which is the shape this repository's no-silent-fallback guard exists to
/// reject (bd-w12xz).
fn drain(pipe: Option<impl Read>) -> (Vec<u8>, Option<std::io::Error>) {
    let mut buffer = Vec::new();
    let mut failure = None;
    if let Some(mut pipe) = pipe {
        if let Err(error) = pipe.read_to_end(&mut buffer) {
            failure = Some(error);
        }
    }
    (buffer, failure)
}

/// Join one reader thread, naming the pipe in anything that went wrong.
///
/// A panicked reader used to arrive at the caller as EMPTY OUTPUT. Consumers
/// parse that output, so a dead reader thread was reported as a malformed JSON
/// document from the child: the harness blamed the program under test for its
/// own defect. A panic here describes this harness, never the child, so it
/// fails with the pipe named instead of impersonating a silent program.
///
/// A read error also fails, and carries the bytes recovered so far in its
/// message -- so the original rationale holds. Nothing is discarded; the
/// partial output travels WITH the reason rather than instead of it.
fn join_reader(
    reader: std::thread::JoinHandle<(Vec<u8>, Option<std::io::Error>)>,
    pipe: &str,
) -> std::io::Result<Vec<u8>> {
    let (bytes, failure) = reader.join().map_err(|_| {
        std::io::Error::other(format!(
            "{pipe} reader thread panicked while draining the child pipe; \
             this is a defect in the spawn harness, not output from the child"
        ))
    })?;
    match failure {
        None => Ok(bytes),
        Some(error) => {
            let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).into_owned();
            Err(std::io::Error::other(format!(
                "{pipe} read failed after {} byte(s): {error}; recovered prefix: {preview:?}",
                bytes.len()
            )))
        }
    }
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
    let _spawn_permit = acquire_spawn_permit();
    let mut command = Command::new(ee_binary());
    command
        .args(&arguments)
        .env("EE_WORKSPACE_REGISTRY", isolated_registry_path());
    output_with_timeout(&mut command, None)
        .map_err(|error| format!("failed to run ee {}: {error}", arguments.join(" ")))
}

/// Caller-configured variant: holds a spawn permit while
/// `configure` mutates a fresh command pre-seeded with the real binary path.
/// Use this for workspace prefixes, env removals or overrides, `current_dir`,
/// or custom failure text; the io error is returned unmapped so each call
/// site keeps its original message.
pub fn serialized_real_ee_with<F>(configure: F) -> std::io::Result<Output>
where
    F: FnOnce(&mut Command),
{
    serialized_real_ee_with_stdin(configure, None)
}

/// As [`serialized_real_ee_with`], but able to hand the child a stdin.
///
/// `ee remember --batch --stdin` is the only remember path that defers index
/// processing (`defer_index_processing: true`, bd-2efx1), so N memories cost
/// ONE embedding-model load instead of N. It was unreachable from this harness
/// because the stdio handles are set after `configure` runs, so a caller could
/// not supply a stdin even by trying.
///
/// The caller owns EOF. Pass an opened `File`: it EOFs by construction, with no
/// writer thread and no close protocol, which keeps the hang class the default
/// `Stdio::null()` exists to prevent.
pub fn serialized_real_ee_with_stdin<F>(
    configure: F,
    stdin: Option<Stdio>,
) -> std::io::Result<Output>
where
    F: FnOnce(&mut Command),
{
    let _spawn_permit = acquire_spawn_permit();
    let mut command = Command::new(ee_binary());
    configure(&mut command);
    // AFTER `configure`, but ONLY when the caller did not choose a registry.
    //
    // Two intents are indistinguishable at this point unless we look, and
    // conflating them broke a contract row:
    //
    //   `env_remove` (the common case) means "isolate me". `get_envs` reports
    //   the key with a None value, and injecting the per-spawn registry
    //   DELIVERS that intent rather than overriding it.
    //
    //   `.env(path)` means "use THIS registry". Overwriting it destroys the
    //   caller's setup. resume_schema's `run_real_ee_with_registry` exists
    //   precisely to hand `ee` a chosen registry, and its two
    //   registry-unavailable rows point it at one the test made unreadable.
    //   Replacing that with a fresh, valid, isolated registry made the
    //   precondition FALSE, so both rows asserted against a working registry
    //   and failed -- not because `ee` regressed, but because the harness
    //   removed the condition under test.
    let caller_chose_registry = command
        .get_envs()
        .any(|(key, value)| key.to_str() == Some("EE_WORKSPACE_REGISTRY") && value.is_some());
    if !caller_chose_registry {
        command.env("EE_WORKSPACE_REGISTRY", isolated_registry_path());
    }
    output_with_timeout(&mut command, stdin)
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
    let result = output_with_deadline(&mut command, Duration::from_millis(400), None);
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
    let output = output_with_deadline(&mut command, Duration::from_secs(60), None)
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
    let output = output_with_deadline(&mut command, Duration::from_secs(30), None)
        .expect("a fast child must return output");
    assert!(output.status.success(), "echo should succeed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("contracts-spawn-probe"),
        "stdout must still be captured through the deadline path: {output:?}"
    );
}
