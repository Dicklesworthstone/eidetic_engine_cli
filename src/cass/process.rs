//! Subprocess invocation primitives for the CASS adapter (EE-100).
//!
//! `ee` does not link to CASS internals; it shells out to the
//! installed `cass` binary and treats stdout, stderr, and the exit
//! status as the only stable contract. The types in this module
//! capture exactly what was run and what came back, so higher-level
//! code can:
//!
//! * route stdout JSON, stderr diagnostic envelopes, and exit-code
//!   classification independently;
//! * persist `command/argv/cwd/env-overrides/exit-code/elapsed`
//!   per-invocation for the audit trail required by the spike;
//! * reuse a single classification helper instead of re-implementing
//!   "did this run actually fail?" on every call site.
//!
//! The module is intentionally I/O-free apart from the
//! [`CassInvocation::run`] entry point: tests can construct a
//! [`CassOutcome`] from any `(stdout, stderr, exit_code)` triple to
//! exercise downstream logic without spawning a process.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group};

use super::error::CassError;

const ALLOWLISTED_CASS_EXECUTABLE: &str = "cass";
const TIMEOUT_PIPE_DRAIN_GRACE: Duration = Duration::from_millis(250);
const TIMEOUT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SPAWN_RETRY_ATTEMPTS: usize = 6;
const CASS_PIPE_CAPTURE_MAX_BYTES: usize = 100 * 1024 * 1024;
pub(crate) const CASS_STDOUT_LINE_MAX_BYTES: usize = 1024 * 1024;
const CASS_STDOUT_LINE_DELIMITER_MAX_BYTES: usize = 2;
const CASS_STDOUT_LINE_READ_LIMIT_BYTES: usize =
    CASS_STDOUT_LINE_MAX_BYTES + CASS_STDOUT_LINE_DELIMITER_MAX_BYTES;

#[derive(Clone, Debug, Eq, PartialEq)]
enum CassSpawnTarget {
    Absolute(PathBuf),
}

impl CassSpawnTarget {
    fn command(&self) -> Command {
        match self {
            Self::Absolute(executable) => Command::new(executable.as_os_str()),
        }
    }
}

/// Sentinel exit code reserved by `cass` for the "degraded but usable"
/// state. The spike documents that a stale-index probe can exit `0`
/// with a warning on stderr; we keep the constant here to make the
/// classification table self-documenting.
pub const CASS_EXIT_OK: i32 = 0;

/// `cass health` documents `1` as the degraded-but-data exit code:
/// stdout still parses as a valid health payload, stderr carries the
/// JSON error envelope. We keep it pinned so future refactors that add
/// adapter logic do not silently widen the meaning.
pub const CASS_EXIT_DEGRADED: i32 = 1;

/// Classification bucket for a finished CASS subprocess.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CassExitClass {
    /// Process exited cleanly with usable stdout payload.
    Success,
    /// Process exited nonzero but stdout still carries a valid payload
    /// (e.g. degraded index, missing semantic asset). Callers should
    /// keep the payload and surface the warning.
    Degraded,
    /// Process failed in a way that yields no usable stdout.
    Failure,
}

impl CassExitClass {
    /// Classify a finished invocation given its raw exit code and the
    /// length of its stdout payload.
    ///
    /// Rules:
    ///
    /// * exit `0` with non-empty stdout -> [`Self::Success`].
    /// * exit `0` with empty stdout -> [`Self::Failure`] (the data
    ///   surfaces always emit *something* on success; an empty stream
    ///   means the caller asked for a void surface like
    ///   `cass index --full` which `ee` core surfaces never invoke).
    /// * non-zero exit with non-empty stdout -> [`Self::Degraded`].
    /// * non-zero exit with empty stdout -> [`Self::Failure`].
    #[must_use]
    pub const fn classify(exit_code: Option<i32>, stdout_len: usize) -> Self {
        match (exit_code, stdout_len) {
            (Some(CASS_EXIT_OK), 0) => Self::Failure,
            (Some(CASS_EXIT_OK), _) => Self::Success,
            (Some(_), 0) => Self::Failure,
            (Some(_), _) => Self::Degraded,
            (None, _) => Self::Failure,
        }
    }

    /// Stable lowercase tag for JSON status output and audit logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Degraded => "degraded",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug)]
pub(crate) enum CassStreamError<E> {
    Cass(CassError),
    Handler(E),
}

impl<E> CassStreamError<E> {
    fn from_cass(error: std::io::Error) -> Self {
        Self::Cass(CassError::from(error))
    }
}

/// Captured result of running a CASS invocation with streamed stdout.
#[derive(Clone, Debug)]
pub(crate) struct CassStreamOutcome {
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    class: CassExitClass,
    timed_out: bool,
    stdout_line_count: usize,
    stdout_bytes_seen: usize,
    peak_stdout_line_bytes: usize,
    peak_stdout_buffer_bytes: usize,
}

impl CassStreamOutcome {
    fn new(
        stderr: Vec<u8>,
        exit_code: Option<i32>,
        timed_out: bool,
        stdout_line_count: usize,
        stdout_bytes_seen: usize,
        peak_stdout_line_bytes: usize,
        peak_stdout_buffer_bytes: usize,
    ) -> Self {
        let class = if timed_out {
            CassExitClass::Failure
        } else {
            CassExitClass::classify(exit_code, stdout_bytes_seen)
        };
        Self {
            stderr,
            exit_code,
            class,
            timed_out,
            stdout_line_count,
            stdout_bytes_seen,
            peak_stdout_line_bytes,
            peak_stdout_buffer_bytes,
        }
    }

    #[must_use]
    pub(crate) fn stderr_utf8_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.stderr.as_slice())
    }

    #[must_use]
    pub(crate) const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    #[must_use]
    pub(crate) const fn class(&self) -> CassExitClass {
        self.class
    }

    #[must_use]
    pub(crate) const fn timed_out(&self) -> bool {
        self.timed_out
    }

    #[must_use]
    pub(crate) const fn stdout_line_count(&self) -> usize {
        self.stdout_line_count
    }

    #[must_use]
    pub(crate) const fn stdout_bytes_seen(&self) -> usize {
        self.stdout_bytes_seen
    }

    #[must_use]
    pub(crate) const fn peak_stdout_line_bytes(&self) -> usize {
        self.peak_stdout_line_bytes
    }

    /// High-water mark of the reader thread's internal byte buffer.
    ///
    /// Distinct from [`Self::peak_stdout_line_bytes`], which tracks the
    /// largest *delivered* line: this field reports the peak size of the
    /// raw read buffer including any line that overshot the size cap and
    /// was rejected. The reader bounds this at
    /// [`CASS_STDOUT_LINE_READ_LIMIT_BYTES`]; a higher value here would
    /// mean the streaming bound has regressed.
    #[must_use]
    pub(crate) const fn peak_stdout_buffer_bytes(&self) -> usize {
        self.peak_stdout_buffer_bytes
    }

    #[must_use]
    pub(crate) const fn stdout_is_empty(&self) -> bool {
        self.stdout_bytes_seen == 0
    }
}

/// One immutable description of what `ee` plans to ask of `cass`.
///
/// `CassInvocation` holds *intent*: the binary, the args, the working
/// directory, and any sanitized environment override. Running it
/// returns a [`CassOutcome`]; the invocation itself can be cloned and
/// retried (the spike requires a stable `request-id` echo for search).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassInvocation {
    binary: PathBuf,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env_overrides: Vec<(OsString, OsString)>,
    timeout: Option<Duration>,
}

impl CassInvocation {
    /// Build an invocation that will run `<binary> <args...>`.
    pub fn new<P, I, S>(binary: P, args: I) -> Self
    where
        P: Into<PathBuf>,
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            binary: binary.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            env_overrides: Vec::new(),
            timeout: None,
        }
    }

    /// Set the working directory the subprocess will be spawned in.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Push a single environment-variable override. Repeated keys win
    /// the last assignment, matching how `Command::env` resolves.
    #[must_use]
    pub fn with_env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<OsString>,
        V: Into<OsString>,
    {
        self.env_overrides.push((key.into(), value.into()));
        self
    }

    /// Set the subprocess wall-clock budget. If the child is still
    /// running when the budget expires, it is killed and reaped before
    /// returning a timed-out [`CassOutcome`].
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Path to the `cass` binary that will be launched.
    #[must_use]
    pub fn binary(&self) -> &Path {
        self.binary.as_path()
    }

    /// Command-line args excluding the binary itself.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        self.args.as_slice()
    }

    /// Working directory the subprocess will be spawned in, if any.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Environment overrides applied on top of the parent process env.
    #[must_use]
    pub fn env_overrides(&self) -> &[(OsString, OsString)] {
        self.env_overrides.as_slice()
    }

    /// Wall-clock budget applied to this subprocess, if configured.
    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Spawn the subprocess and capture stdout / stderr / exit code.
    ///
    /// This is the only function in the cass module that touches the
    /// real OS. Tests should construct [`CassOutcome`] directly through
    /// [`CassOutcome::synthetic`].
    ///
    /// # Errors
    ///
    /// Returns [`CassError::InvalidBinary`] for non-allowlisted
    /// executable paths, [`CassError::BinaryNotFound`] when the OS
    /// reports `NotFound`, or [`CassError::Io`] for any other spawn
    /// failure.
    pub fn run(&self) -> Result<CassOutcome, CassError> {
        let spawn_target = self.validated_spawn_target()?;
        let started = Instant::now();
        let mut command = spawn_target.command();
        command.args(&self.args);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &self.env_overrides {
            command.env(key, value);
        }
        self.run_with_capped_pipes(command, started, self.timeout, CASS_PIPE_CAPTURE_MAX_BYTES)
    }

    /// Spawn the subprocess and stream stdout one UTF-8 line at a time.
    ///
    /// This is for CASS surfaces that can emit JSONL. Unlike [`Self::run`],
    /// stdout is never retained as one contiguous byte buffer, and the
    /// per-line read buffer is bounded at [`CASS_STDOUT_LINE_READ_LIMIT_BYTES`]
    /// bytes — enough for one logical line plus `\r\n`; overshoot is rejected
    /// *before* the bytes are realized into a [`String`], not after.
    pub(crate) fn run_stdout_lines<F, E>(
        &self,
        handle_line: F,
    ) -> Result<CassStreamOutcome, CassStreamError<E>>
    where
        F: FnMut(String) -> Result<(), E>,
    {
        let probe = Arc::new(AtomicUsize::new(0));
        self.run_stdout_lines_with_buffer_probe(handle_line, probe)
    }

    /// Same as [`Self::run_stdout_lines`] but the caller supplies the
    /// `Arc<AtomicUsize>` used to track the reader thread's peak byte
    /// buffer size. Tests can inspect the probe after the call returns
    /// (including on the error path, where the [`CassStreamOutcome`] is
    /// not surfaced).
    fn run_stdout_lines_with_buffer_probe<F, E>(
        &self,
        mut handle_line: F,
        peak_stdout_buffer_probe: Arc<AtomicUsize>,
    ) -> Result<CassStreamOutcome, CassStreamError<E>>
    where
        F: FnMut(String) -> Result<(), E>,
    {
        let spawn_target = self
            .validated_spawn_target()
            .map_err(CassStreamError::Cass)?;
        let started = Instant::now();
        let mut command = spawn_target.command();
        command.args(&self.args);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &self.env_overrides {
            command.env(key, value);
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        let mut child = retry_cass_spawn(|| command.spawn())
            .map_err(|error| CassStreamError::Cass(cass_spawn_error(self, error)))?;
        #[cfg(unix)]
        let child_group = Pid::from_child(&child);

        let stdout = child.stdout.take().ok_or_else(|| {
            CassStreamError::Cass(CassError::Io {
                message: "cass subprocess stdout pipe was not available".to_owned(),
            })
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| {
            CassStreamError::Cass(CassError::Io {
                message: "cass subprocess stderr pipe was not available".to_owned(),
            })
        })?;

        let (stdout_rx, stdout_thread) =
            spawn_stdout_line_reader(stdout, Arc::clone(&peak_stdout_buffer_probe));
        let mut stdout_thread = Some(stdout_thread);
        let stderr_thread = thread::spawn(move || {
            read_capped_pipe(&mut stderr, "stderr", CASS_PIPE_CAPTURE_MAX_BYTES)
        });

        let mut child_status: Option<ExitStatus> = None;
        let mut stderr_thread = Some(stderr_thread);
        let mut stderr_bytes = None;
        let mut stdout_done = false;
        let mut stdout_line_count = 0_usize;
        let mut stdout_bytes_seen = 0_usize;
        let mut peak_stdout_line_bytes = 0_usize;
        let mut stream_error = None;
        let mut handler_error = None;

        loop {
            drain_available_stdout_lines(
                &stdout_rx,
                &mut stdout_done,
                &mut stdout_line_count,
                &mut stdout_bytes_seen,
                &mut peak_stdout_line_bytes,
                &mut stream_error,
                &mut handler_error,
                &mut handle_line,
            );

            if (stream_error.is_some() || handler_error.is_some()) && child_status.is_none() {
                #[cfg(unix)]
                terminate_cass_process_group(child_group);
                if let Err(kill_error) = child.kill() {
                    if kill_error.kind() != std::io::ErrorKind::InvalidInput {
                        tracing::debug!(
                            "cass subprocess kill failed after stream handler error: {kill_error}"
                        );
                    }
                }
                child_status = Some(child.wait().map_err(CassStreamError::from_cass)?);
            }

            if child_status.is_none() {
                child_status = child.try_wait().map_err(CassStreamError::from_cass)?;
            }
            if let Err(error) = collect_finished_pipe_reader(&mut stderr_thread, &mut stderr_bytes)
            {
                #[cfg(unix)]
                terminate_cass_child_after_reader_error(
                    &mut child,
                    child_group,
                    &mut child_status,
                    "stderr pipe reader error",
                )
                .map_err(CassStreamError::Cass)?;
                #[cfg(not(unix))]
                terminate_cass_child_after_reader_error(
                    &mut child,
                    &mut child_status,
                    "stderr pipe reader error",
                )
                .map_err(CassStreamError::Cass)?;
                let _ = drain_stdout_line_reader_after_stop(
                    &stdout_rx,
                    &mut stdout_done,
                    &mut stdout_line_count,
                    &mut stdout_bytes_seen,
                    &mut peak_stdout_line_bytes,
                );
                let _ = join_finished_stdout_line_reader_after_timeout(&mut stdout_thread);
                return Err(CassStreamError::Cass(error));
            }
            if stdout_done {
                collect_finished_stdout_line_reader(&mut stdout_thread)
                    .map_err(CassStreamError::Cass)?;
            }

            if let Some(status) = child_status {
                if stdout_done
                    && stdout_thread.is_none()
                    && let Some(captured_stderr) = stderr_bytes.take()
                {
                    if let Some(error) = stream_error {
                        return Err(CassStreamError::Cass(error));
                    }
                    if let Some(error) = handler_error {
                        return Err(CassStreamError::Handler(error));
                    }
                    return Ok(CassStreamOutcome::new(
                        captured_stderr,
                        status.code(),
                        false,
                        stdout_line_count,
                        stdout_bytes_seen,
                        peak_stdout_line_bytes,
                        peak_stdout_buffer_probe.load(Ordering::Relaxed),
                    ));
                }
            }

            let elapsed = started.elapsed();
            if let Some(timeout) = self.timeout {
                if elapsed >= timeout {
                    #[cfg(unix)]
                    terminate_cass_process_group(child_group);
                    if child_status.is_none() {
                        if let Err(kill_error) = child.kill() {
                            if kill_error.kind() != std::io::ErrorKind::InvalidInput {
                                tracing::debug!(
                                    "cass subprocess kill failed (child may have already exited): {kill_error}"
                                );
                            }
                        }
                        child_status = Some(child.wait().map_err(CassStreamError::from_cass)?);
                    }
                    drain_stdout_line_reader_after_stop(
                        &stdout_rx,
                        &mut stdout_done,
                        &mut stdout_line_count,
                        &mut stdout_bytes_seen,
                        &mut peak_stdout_line_bytes,
                    )
                    .map_err(CassStreamError::Cass)?;
                    join_finished_stdout_line_reader_after_timeout(&mut stdout_thread)
                        .map_err(CassStreamError::Cass)?;
                    let mut absent_stdout_thread = None;
                    let mut empty_stdout_bytes = Some(Vec::new());
                    let (_stdout_bytes, stderr_bytes_after_timeout) =
                        drain_pipe_readers_after_timeout(
                            &mut absent_stdout_thread,
                            &mut stderr_thread,
                            &mut empty_stdout_bytes,
                            &mut stderr_bytes,
                        )
                        .map_err(CassStreamError::Cass)?;
                    let Some(status) = child_status.take() else {
                        return Err(CassStreamError::Cass(CassError::Io {
                            message: "cass subprocess status was unavailable after timeout"
                                .to_owned(),
                        }));
                    };
                    return Ok(CassStreamOutcome::new(
                        stderr_bytes_after_timeout,
                        status.code(),
                        true,
                        stdout_line_count,
                        stdout_bytes_seen,
                        peak_stdout_line_bytes,
                        peak_stdout_buffer_probe.load(Ordering::Relaxed),
                    ));
                }
            }

            let sleep_for = self.timeout.map_or(TIMEOUT_POLL_INTERVAL, |timeout| {
                timeout.saturating_sub(elapsed).min(TIMEOUT_POLL_INTERVAL)
            });
            thread::sleep(sleep_for);
        }
    }

    fn run_with_capped_pipes(
        &self,
        mut command: Command,
        started: Instant,
        timeout: Option<Duration>,
        pipe_capture_max_bytes: usize,
    ) -> Result<CassOutcome, CassError> {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        let mut child =
            retry_cass_spawn(|| command.spawn()).map_err(|error| cass_spawn_error(self, error))?;
        #[cfg(unix)]
        let child_group = Pid::from_child(&child);

        let mut stdout = child.stdout.take().ok_or_else(|| CassError::Io {
            message: "cass subprocess stdout pipe was not available".to_owned(),
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| CassError::Io {
            message: "cass subprocess stderr pipe was not available".to_owned(),
        })?;

        let stdout_thread =
            thread::spawn(move || read_capped_pipe(&mut stdout, "stdout", pipe_capture_max_bytes));

        let stderr_thread =
            thread::spawn(move || read_capped_pipe(&mut stderr, "stderr", pipe_capture_max_bytes));

        let mut child_status: Option<ExitStatus> = None;
        let mut stdout_thread = Some(stdout_thread);
        let mut stderr_thread = Some(stderr_thread);
        let mut stdout_bytes = None;
        let mut stderr_bytes = None;

        loop {
            if child_status.is_none() {
                child_status = child.try_wait().map_err(CassError::from)?;
            }
            if let Err(error) = collect_finished_pipe_reader(&mut stdout_thread, &mut stdout_bytes)
            {
                #[cfg(unix)]
                terminate_cass_child_after_reader_error(
                    &mut child,
                    child_group,
                    &mut child_status,
                    "stdout pipe reader error",
                )?;
                #[cfg(not(unix))]
                terminate_cass_child_after_reader_error(
                    &mut child,
                    &mut child_status,
                    "stdout pipe reader error",
                )?;
                let _ = drain_pipe_readers_after_timeout(
                    &mut stdout_thread,
                    &mut stderr_thread,
                    &mut stdout_bytes,
                    &mut stderr_bytes,
                );
                return Err(error);
            }
            if let Err(error) = collect_finished_pipe_reader(&mut stderr_thread, &mut stderr_bytes)
            {
                #[cfg(unix)]
                terminate_cass_child_after_reader_error(
                    &mut child,
                    child_group,
                    &mut child_status,
                    "stderr pipe reader error",
                )?;
                #[cfg(not(unix))]
                terminate_cass_child_after_reader_error(
                    &mut child,
                    &mut child_status,
                    "stderr pipe reader error",
                )?;
                let _ = drain_pipe_readers_after_timeout(
                    &mut stdout_thread,
                    &mut stderr_thread,
                    &mut stdout_bytes,
                    &mut stderr_bytes,
                );
                return Err(error);
            }

            if let Some((status, stdout_bytes, stderr_bytes)) =
                take_completed_subprocess(&mut child_status, &mut stdout_bytes, &mut stderr_bytes)
            {
                return Ok(CassOutcome::new(
                    self.clone(),
                    stdout_bytes,
                    stderr_bytes,
                    status.code(),
                    started.elapsed(),
                    false,
                ));
            }

            if let Some(timeout) = timeout {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    #[cfg(unix)]
                    terminate_cass_process_group(child_group);
                    if child_status.is_none() {
                        if let Err(kill_error) = child.kill() {
                            if kill_error.kind() != std::io::ErrorKind::InvalidInput {
                                tracing::debug!(
                                    "cass subprocess kill failed (child may have already exited): {kill_error}"
                                );
                            }
                        }
                        child_status = Some(child.wait().map_err(CassError::from)?);
                    }
                    let (stdout_bytes, stderr_bytes) = drain_pipe_readers_after_timeout(
                        &mut stdout_thread,
                        &mut stderr_thread,
                        &mut stdout_bytes,
                        &mut stderr_bytes,
                    )?;
                    let Some(status) = child_status.take() else {
                        return Err(CassError::Io {
                            message: "cass subprocess status was unavailable after timeout"
                                .to_owned(),
                        });
                    };
                    return Ok(CassOutcome::new(
                        self.clone(),
                        stdout_bytes,
                        stderr_bytes,
                        status.code(),
                        started.elapsed(),
                        true,
                    ));
                }
                thread::sleep(timeout.saturating_sub(elapsed).min(TIMEOUT_POLL_INTERVAL));
            } else {
                thread::sleep(TIMEOUT_POLL_INTERVAL);
            }
        }
    }

    fn validated_spawn_target(&self) -> Result<CassSpawnTarget, CassError> {
        let inherited_path = std::env::var_os("PATH");
        self.validated_spawn_target_from_path_var(inherited_path.as_deref())
    }

    fn validated_spawn_target_from_path_var(
        &self,
        inherited_path: Option<&OsStr>,
    ) -> Result<CassSpawnTarget, CassError> {
        if self.binary == Path::new(ALLOWLISTED_CASS_EXECUTABLE) {
            return resolve_path_lookup_cass_binary(inherited_path).map(CassSpawnTarget::Absolute);
        }

        if self.binary.is_absolute()
            && self.binary.file_name() == Some(OsStr::new(ALLOWLISTED_CASS_EXECUTABLE))
        {
            validate_absolute_cass_binary(&self.binary)?;
            return Ok(CassSpawnTarget::Absolute(self.binary.clone()));
        }

        Err(CassError::InvalidBinary {
            binary: self.binary.clone(),
            reason: "EE-100 allowlist: binary must be 'cass' (PATH lookup) or a trusted absolute path to a file named 'cass'"
                .to_string(),
        })
    }
}

fn resolve_path_lookup_cass_binary(inherited_path: Option<&OsStr>) -> Result<PathBuf, CassError> {
    let Some(path_var) = inherited_path else {
        return Err(CassError::BinaryNotFound {
            binary: PathBuf::from(ALLOWLISTED_CASS_EXECUTABLE),
        });
    };

    for directory in std::env::split_paths(path_var) {
        let candidate = directory.join(ALLOWLISTED_CASS_EXECUTABLE);
        if validate_absolute_cass_binary(&candidate).is_ok() {
            return candidate
                .canonicalize()
                .map_err(|error| CassError::InvalidBinary {
                    binary: candidate,
                    reason: format!("CASS binary canonicalization failed: {error}"),
                });
        }
    }

    Err(CassError::BinaryNotFound {
        binary: PathBuf::from(ALLOWLISTED_CASS_EXECUTABLE),
    })
}

fn retry_cass_spawn<T>(mut spawn: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut last_retryable_error = None;
    for attempt in 0..SPAWN_RETRY_ATTEMPTS {
        match spawn() {
            Ok(result) => return Ok(result),
            Err(error) if cass_spawn_error_is_retryable(&error) => {
                last_retryable_error = Some(error);
                if attempt + 1 < SPAWN_RETRY_ATTEMPTS {
                    thread::sleep(cass_spawn_retry_delay(attempt));
                }
            }
            Err(error) => return Err(error),
        }
    }

    match last_retryable_error {
        Some(error) => Err(error),
        None => Err(std::io::Error::other(
            "CASS spawn retry loop exhausted without a retryable error",
        )),
    }
}

fn cass_spawn_error(invocation: &CassInvocation, error: std::io::Error) -> CassError {
    if error.kind() == std::io::ErrorKind::NotFound {
        CassError::BinaryNotFound {
            binary: invocation.binary.clone(),
        }
    } else {
        CassError::Io {
            message: error.to_string(),
        }
    }
}

fn cass_spawn_error_is_retryable(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        const TEXT_FILE_BUSY: i32 = 26;
        if error.raw_os_error() == Some(TEXT_FILE_BUSY) {
            return true;
        }
    }
    false
}

fn cass_spawn_retry_delay(attempt: usize) -> Duration {
    const BASE_DELAY_MS: u64 = 2;
    const MAX_DELAY_MS: u64 = 50;

    let multiplier = 1_u64 << attempt.min(5);
    Duration::from_millis(BASE_DELAY_MS.saturating_mul(multiplier).min(MAX_DELAY_MS))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CassStdoutLine {
    text: String,
    delimiter_bytes: usize,
}

fn spawn_stdout_line_reader(
    stdout: std::process::ChildStdout,
    peak_buffer_bytes: Arc<AtomicUsize>,
) -> (
    Receiver<Result<CassStdoutLine, CassError>>,
    thread::JoinHandle<Result<(), CassError>>,
) {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf: Vec<u8> = bounded_stdout_line_buffer();
        loop {
            match read_bounded_stdout_line(&mut reader, &mut buf, peak_buffer_bytes.as_ref()) {
                Ok(Some(line)) => {
                    if sender.send(Ok(line)).is_err() {
                        return Ok(());
                    }
                }
                Ok(None) => return Ok(()),
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return Ok(());
                }
            }
        }
    });
    (receiver, handle)
}

fn bounded_stdout_line_buffer() -> Vec<u8> {
    // Pre-reserve the buffer at the logical line cap plus CRLF delimiter
    // slack so that even an adversarial single-pathological-line input
    // (one line with no '\n' for many megabytes) cannot push the reader's
    // allocation past the bounded read window. `Vec::clear` keeps this
    // capacity for every subsequent line.
    Vec::with_capacity(CASS_STDOUT_LINE_READ_LIMIT_BYTES)
}

fn read_bounded_stdout_line<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    peak_buffer_bytes: &AtomicUsize,
) -> Result<Option<CassStdoutLine>, CassError> {
    buf.clear();
    // `take(READ_LIMIT).read_until(b'\n', ...)` reads at most one logical
    // line plus CRLF delimiter slack and stops the moment either a newline
    // is observed, EOF is reached, or the Take limit is exhausted. The
    // pre-yield cap check below fires *before* the bytes are ever realized
    // into a `String`, fixing the bd-352wc reactive-cap regression
    // (BufReader::lines fully realized the line into memory before the
    // post-yield size check).
    let read_result = reader
        .by_ref()
        .take(CASS_STDOUT_LINE_READ_LIMIT_BYTES as u64)
        .read_until(b'\n', buf);
    peak_buffer_bytes.fetch_max(buf.len(), Ordering::Relaxed);
    match read_result {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(error) => {
            return Err(CassError::Io {
                message: format!("cass subprocess stdout line read failed: {error}"),
            });
        }
    }
    let has_newline = buf.last() == Some(&b'\n');
    // Strip trailing '\n' (and a preceding '\r' for CRLF input) to
    // match the byte-stripping behavior of `BufRead::read_line` /
    // `BufRead::lines`.
    let mut line_bytes: &[u8] = buf.as_slice();
    let mut delimiter_bytes = 0_usize;
    if has_newline {
        delimiter_bytes = 1;
        line_bytes = &line_bytes[..line_bytes.len() - 1];
        if line_bytes.last() == Some(&b'\r') {
            delimiter_bytes += 1;
            line_bytes = &line_bytes[..line_bytes.len() - 1];
        }
    }
    if line_bytes.len() > CASS_STDOUT_LINE_MAX_BYTES {
        return Err(CassError::Io {
            message: format!(
                "cass subprocess stdout line exceeded {CASS_STDOUT_LINE_MAX_BYTES} byte limit"
            ),
        });
    }
    let line = std::str::from_utf8(line_bytes)
        .map_err(|error| CassError::Io {
            message: format!("cass subprocess stdout line was not valid UTF-8: {error}"),
        })?
        .to_owned();
    Ok(Some(CassStdoutLine {
        text: line,
        delimiter_bytes,
    }))
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CassStdoutDecodeFuzzSummary {
    pub line_count: usize,
    pub bytes_seen: usize,
    pub peak_line_bytes: usize,
    pub peak_buffer_bytes: usize,
}

#[doc(hidden)]
pub fn fuzz_decode_cass_stdout_stream(
    input: &[u8],
) -> Result<CassStdoutDecodeFuzzSummary, CassError> {
    let mut reader = BufReader::new(input);
    let mut buf = bounded_stdout_line_buffer();
    let peak_buffer_bytes = AtomicUsize::new(0);
    let mut line_count = 0_usize;
    let mut bytes_seen = 0_usize;
    let mut peak_line_bytes = 0_usize;

    while let Some(line) = read_bounded_stdout_line(&mut reader, &mut buf, &peak_buffer_bytes)? {
        record_stdout_line_stats(
            &line,
            &mut line_count,
            &mut bytes_seen,
            &mut peak_line_bytes,
        )?;
    }

    Ok(CassStdoutDecodeFuzzSummary {
        line_count,
        bytes_seen,
        peak_line_bytes,
        peak_buffer_bytes: peak_buffer_bytes.load(Ordering::Relaxed),
    })
}

#[allow(clippy::too_many_arguments)]
fn drain_available_stdout_lines<F, E>(
    receiver: &Receiver<Result<CassStdoutLine, CassError>>,
    stdout_done: &mut bool,
    stdout_line_count: &mut usize,
    stdout_bytes_seen: &mut usize,
    peak_stdout_line_bytes: &mut usize,
    stream_error: &mut Option<CassError>,
    handler_error: &mut Option<E>,
    handle_line: &mut F,
) where
    F: FnMut(String) -> Result<(), E>,
{
    loop {
        match receiver.try_recv() {
            Ok(Ok(line)) => {
                if let Err(error) = record_stdout_line_stats(
                    &line,
                    stdout_line_count,
                    stdout_bytes_seen,
                    peak_stdout_line_bytes,
                ) {
                    *stream_error = Some(error);
                    continue;
                }
                if stream_error.is_none()
                    && handler_error.is_none()
                    && let Err(error) = handle_line(line.text)
                {
                    *handler_error = Some(error);
                }
            }
            Ok(Err(error)) => {
                *stream_error = Some(error);
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                *stdout_done = true;
                break;
            }
        }
    }
}

fn drain_stdout_line_reader_after_stop(
    receiver: &Receiver<Result<CassStdoutLine, CassError>>,
    stdout_done: &mut bool,
    stdout_line_count: &mut usize,
    stdout_bytes_seen: &mut usize,
    peak_stdout_line_bytes: &mut usize,
) -> Result<(), CassError> {
    let deadline = Instant::now() + TIMEOUT_PIPE_DRAIN_GRACE;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break Ok(());
        }
        let remaining = deadline
            .checked_duration_since(now)
            .unwrap_or(Duration::ZERO)
            .min(TIMEOUT_POLL_INTERVAL);
        match receiver.recv_timeout(remaining) {
            Ok(Ok(line)) => record_stdout_line_stats(
                &line,
                stdout_line_count,
                stdout_bytes_seen,
                peak_stdout_line_bytes,
            )?,
            Ok(Err(_error)) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => break Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                *stdout_done = true;
                break Ok(());
            }
        }
    }
}

fn record_stdout_line_stats(
    line: &CassStdoutLine,
    stdout_line_count: &mut usize,
    stdout_bytes_seen: &mut usize,
    peak_stdout_line_bytes: &mut usize,
) -> Result<(), CassError> {
    let next_line_count = checked_stdout_stat_add(*stdout_line_count, 1, "line count")?;
    let line_bytes_with_delimiter =
        checked_stdout_stat_add(line.text.len(), line.delimiter_bytes, "byte count")?;
    let next_bytes_seen =
        checked_stdout_stat_add(*stdout_bytes_seen, line_bytes_with_delimiter, "byte count")?;
    *stdout_line_count = next_line_count;
    *stdout_bytes_seen = next_bytes_seen;
    *peak_stdout_line_bytes = (*peak_stdout_line_bytes).max(line.text.len());
    Ok(())
}

fn checked_stdout_stat_add(
    current: usize,
    increment: usize,
    stat_name: &'static str,
) -> Result<usize, CassError> {
    current.checked_add(increment).ok_or_else(|| CassError::Io {
        message: format!("cass subprocess stdout {stat_name} overflowed"),
    })
}

fn read_capped_pipe<R: Read>(
    reader: R,
    stream_name: &'static str,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let limit = max_bytes.checked_add(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("cass subprocess {stream_name} capture limit overflowed"),
        )
    })?;
    let mut buf = Vec::new();
    let mut limited = reader.take(limit as u64);
    limited.read_to_end(&mut buf)?;
    if buf.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("cass subprocess {stream_name} exceeded {max_bytes} byte capture limit"),
        ));
    }
    Ok(buf)
}

fn join_pipe_reader(
    handle: thread::JoinHandle<Result<Vec<u8>, std::io::Error>>,
) -> Result<Vec<u8>, CassError> {
    match handle.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(read_error)) => Err(CassError::Io {
            message: format!("cass subprocess pipe read failed: {read_error}"),
        }),
        Err(_panic) => Err(CassError::Io {
            message: "cass subprocess pipe reader thread panicked".to_owned(),
        }),
    }
}

fn join_stdout_line_reader(
    handle: &mut Option<thread::JoinHandle<Result<(), CassError>>>,
) -> Result<(), CassError> {
    let Some(reader) = handle.take() else {
        return Ok(());
    };
    match reader.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_panic) => Err(CassError::Io {
            message: "cass subprocess stdout line reader thread panicked".to_owned(),
        }),
    }
}

fn collect_finished_stdout_line_reader(
    handle: &mut Option<thread::JoinHandle<Result<(), CassError>>>,
) -> Result<(), CassError> {
    let Some(reader) = handle else {
        return Ok(());
    };
    if reader.is_finished() {
        join_stdout_line_reader(handle)?;
    }
    Ok(())
}

fn join_finished_stdout_line_reader_after_timeout(
    handle: &mut Option<thread::JoinHandle<Result<(), CassError>>>,
) -> Result<(), CassError> {
    let Some(reader) = handle.as_ref() else {
        return Ok(());
    };
    if reader.is_finished() {
        return join_stdout_line_reader(handle);
    }
    tracing::debug!("cass subprocess stdout line reader did not drain before timeout");
    *handle = None;
    Ok(())
}

fn take_completed_subprocess(
    child_status: &mut Option<ExitStatus>,
    stdout_bytes: &mut Option<Vec<u8>>,
    stderr_bytes: &mut Option<Vec<u8>>,
) -> Option<(ExitStatus, Vec<u8>, Vec<u8>)> {
    match (
        child_status.take(),
        stdout_bytes.take(),
        stderr_bytes.take(),
    ) {
        (Some(status), Some(stdout), Some(stderr)) => Some((status, stdout, stderr)),
        (status, stdout, stderr) => {
            *child_status = status;
            *stdout_bytes = stdout;
            *stderr_bytes = stderr;
            None
        }
    }
}

fn collect_finished_pipe_reader(
    handle: &mut Option<thread::JoinHandle<Result<Vec<u8>, std::io::Error>>>,
    bytes: &mut Option<Vec<u8>>,
) -> Result<(), CassError> {
    if bytes.is_some() {
        return Ok(());
    }
    let Some(reader) = handle else {
        return Ok(());
    };
    if reader.is_finished() {
        if let Some(reader) = handle.take() {
            *bytes = Some(join_pipe_reader(reader)?);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn terminate_cass_child_after_reader_error(
    child: &mut std::process::Child,
    child_group: Pid,
    child_status: &mut Option<ExitStatus>,
    reason: &str,
) -> Result<(), CassError> {
    terminate_cass_process_group(child_group);
    terminate_cass_child_after_reader_error_without_group(child, child_status, reason)
}

#[cfg(not(unix))]
fn terminate_cass_child_after_reader_error(
    child: &mut std::process::Child,
    child_status: &mut Option<ExitStatus>,
    reason: &str,
) -> Result<(), CassError> {
    terminate_cass_child_after_reader_error_without_group(child, child_status, reason)
}

fn terminate_cass_child_after_reader_error_without_group(
    child: &mut std::process::Child,
    child_status: &mut Option<ExitStatus>,
    reason: &str,
) -> Result<(), CassError> {
    if child_status.is_some() {
        return Ok(());
    }
    if let Err(kill_error) = child.kill()
        && kill_error.kind() != std::io::ErrorKind::InvalidInput
    {
        tracing::debug!("cass subprocess kill failed after {reason}: {kill_error}");
    }
    *child_status = Some(child.wait().map_err(CassError::from)?);
    Ok(())
}

fn drain_pipe_readers_after_timeout(
    stdout_thread: &mut Option<thread::JoinHandle<Result<Vec<u8>, std::io::Error>>>,
    stderr_thread: &mut Option<thread::JoinHandle<Result<Vec<u8>, std::io::Error>>>,
    stdout_bytes: &mut Option<Vec<u8>>,
    stderr_bytes: &mut Option<Vec<u8>>,
) -> Result<(Vec<u8>, Vec<u8>), CassError> {
    let deadline = Instant::now() + TIMEOUT_PIPE_DRAIN_GRACE;
    loop {
        collect_finished_pipe_reader(stdout_thread, stdout_bytes)?;
        collect_finished_pipe_reader(stderr_thread, stderr_bytes)?;
        if stdout_bytes.is_some() && stderr_bytes.is_some() {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep(
            deadline
                .checked_duration_since(now)
                .unwrap_or(Duration::ZERO)
                .min(TIMEOUT_POLL_INTERVAL),
        );
    }

    Ok((
        finish_pipe_reader_after_timeout("stdout", stdout_thread, stdout_bytes)?,
        finish_pipe_reader_after_timeout("stderr", stderr_thread, stderr_bytes)?,
    ))
}

fn finish_pipe_reader_after_timeout(
    stream_name: &'static str,
    handle: &mut Option<thread::JoinHandle<Result<Vec<u8>, std::io::Error>>>,
    bytes: &mut Option<Vec<u8>>,
) -> Result<Vec<u8>, CassError> {
    collect_finished_pipe_reader(handle, bytes)?;
    if let Some(bytes) = bytes.take() {
        return Ok(bytes);
    }
    if handle.take().is_some() {
        tracing::debug!("cass subprocess {stream_name} pipe reader did not drain before timeout");
    }
    if stream_name == "stderr" {
        return Ok(b"cass subprocess stderr pipe drain timed out; output unavailable".to_vec());
    }
    Ok(Vec::new())
}

#[cfg(unix)]
fn terminate_cass_process_group(child_group: Pid) {
    if let Err(kill_error) = kill_process_group(child_group, Signal::KILL) {
        tracing::debug!(
            "cass subprocess process-group kill failed (group may have already exited): {kill_error}"
        );
    }
}

fn validate_absolute_cass_binary(path: &Path) -> Result<(), CassError> {
    reject_existing_symlink_component(path)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| CassError::InvalidBinary {
        binary: path.to_path_buf(),
        reason: format!("CASS binary metadata is unavailable: {error}"),
    })?;
    if !metadata.is_file() {
        return Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS binary path is not a file".to_string(),
        });
    }
    validate_absolute_cass_binary_permissions(path, &metadata)
}

#[cfg(unix)]
fn validate_absolute_cass_binary_permissions(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), CassError> {
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        return Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS binary is not executable".to_string(),
        });
    }
    if mode & 0o022 != 0 {
        return Err(CassError::InvalidBinary {
            binary: path.to_path_buf(),
            reason: "CASS binary must not be writable by group or other".to_string(),
        });
    }
    if let Some(parent) = path.parent() {
        let parent_metadata =
            std::fs::symlink_metadata(parent).map_err(|error| CassError::InvalidBinary {
                binary: path.to_path_buf(),
                reason: format!("CASS binary parent metadata is unavailable: {error}"),
            })?;
        if parent_metadata.permissions().mode() & 0o002 != 0 {
            return Err(CassError::InvalidBinary {
                binary: path.to_path_buf(),
                reason: "CASS binary parent directory must not be writable by other".to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_absolute_cass_binary_permissions(
    _path: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), CassError> {
    Ok(())
}

fn reject_existing_symlink_component(path: &Path) -> Result<(), CassError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CassError::InvalidBinary {
                    binary: path.to_path_buf(),
                    reason: format!(
                        "CASS binary path contains symlink component `{}`",
                        current.display()
                    ),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CassError::InvalidBinary {
                    binary: path.to_path_buf(),
                    reason: format!("CASS binary path component metadata is unavailable: {error}"),
                });
            }
        }
    }
    Ok(())
}

/// Captured result of running a [`CassInvocation`].
///
/// Holds the original invocation (for provenance), both raw byte
/// streams, the OS exit code (if any), and elapsed wall time.
#[derive(Clone, Debug)]
pub struct CassOutcome {
    invocation: CassInvocation,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    elapsed: Duration,
    class: CassExitClass,
    timed_out: bool,
}

impl CassOutcome {
    /// Construct a real outcome from a finished subprocess.
    fn new(
        invocation: CassInvocation,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_code: Option<i32>,
        elapsed: Duration,
        timed_out: bool,
    ) -> Self {
        let class = if timed_out {
            CassExitClass::Failure
        } else {
            CassExitClass::classify(exit_code, stdout.len())
        };
        Self {
            invocation,
            stdout,
            stderr,
            exit_code,
            elapsed,
            class,
            timed_out,
        }
    }

    /// Construct an outcome for tests without spawning a process.
    /// `elapsed` defaults to zero, which is fine for classification
    /// tests; integration tests that care about latency budgets should
    /// use [`Self::synthetic_with_elapsed`].
    #[must_use]
    pub fn synthetic(
        invocation: CassInvocation,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_code: Option<i32>,
    ) -> Self {
        Self::new(invocation, stdout, stderr, exit_code, Duration::ZERO, false)
    }

    /// Construct an outcome for tests with an explicit elapsed time.
    #[must_use]
    pub fn synthetic_with_elapsed(
        invocation: CassInvocation,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_code: Option<i32>,
        elapsed: Duration,
    ) -> Self {
        Self::new(invocation, stdout, stderr, exit_code, elapsed, false)
    }

    /// Original invocation that produced this outcome.
    #[must_use]
    pub const fn invocation(&self) -> &CassInvocation {
        &self.invocation
    }

    /// Raw stdout bytes — the only machine-data channel.
    #[must_use]
    pub fn stdout_bytes(&self) -> &[u8] {
        self.stdout.as_slice()
    }

    /// stdout interpreted as UTF-8 (lossy). The CASS contract is
    /// always UTF-8 JSON; the lossy conversion only matters for
    /// diagnostic display when something has gone wrong.
    #[must_use]
    pub fn stdout_utf8_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.stdout.as_slice())
    }

    /// Raw stderr bytes — diagnostics, JSON error envelopes, warnings.
    #[must_use]
    pub fn stderr_bytes(&self) -> &[u8] {
        self.stderr.as_slice()
    }

    /// stderr interpreted as UTF-8 (lossy).
    #[must_use]
    pub fn stderr_utf8_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.stderr.as_slice())
    }

    /// OS exit code, or `None` if the process was killed by a signal.
    #[must_use]
    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Wall-clock duration the subprocess took to finish.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Pre-computed classification.
    #[must_use]
    pub const fn class(&self) -> CassExitClass {
        self.class
    }

    /// `true` when the subprocess exceeded its wall-clock budget and
    /// was killed and reaped by [`CassInvocation::run`].
    #[must_use]
    pub const fn timed_out(&self) -> bool {
        self.timed_out
    }

    /// `true` iff stdout is empty.
    #[must_use]
    pub fn stdout_is_empty(&self) -> bool {
        self.stdout.is_empty()
    }

    /// `true` iff stderr is empty.
    #[must_use]
    pub fn stderr_is_empty(&self) -> bool {
        self.stderr.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    #[cfg(unix)]
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        CASS_EXIT_DEGRADED, CASS_EXIT_OK, CassExitClass, CassInvocation, CassOutcome,
        TIMEOUT_PIPE_DRAIN_GRACE, drain_pipe_readers_after_timeout,
        join_finished_stdout_line_reader_after_timeout,
    };

    fn invocation() -> CassInvocation {
        CassInvocation::new("cass", ["health", "--json"])
    }

    #[cfg(unix)]
    fn unique_test_dir(prefix: &str) -> Result<PathBuf, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock moved backwards: {error}"))?
            .as_nanos();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let target_dir = target_dir
            .canonicalize()
            .map_err(|error| format!("canonicalize CASS process test root: {error}"))?;
        Ok(target_dir
            .join("ee-cass-process-tests")
            .join(format!("{prefix}-{}-{now}", std::process::id())))
    }

    #[cfg(unix)]
    fn write_executable_script(path: &Path, contents: &str, mode: u32) -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.write_all(contents.as_bytes())
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);

        let mut permissions = fs::metadata(path)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).map_err(|error| error.to_string())
    }

    #[test]
    fn classify_success_requires_zero_exit_and_payload() {
        assert_eq!(
            CassExitClass::classify(Some(CASS_EXIT_OK), 1),
            CassExitClass::Success,
        );
        assert_eq!(
            CassExitClass::classify(Some(CASS_EXIT_OK), 0),
            CassExitClass::Failure,
        );
    }

    #[test]
    fn classify_degraded_requires_nonzero_exit_with_payload() {
        assert_eq!(
            CassExitClass::classify(Some(CASS_EXIT_DEGRADED), 32),
            CassExitClass::Degraded,
        );
        assert_eq!(
            CassExitClass::classify(Some(CASS_EXIT_DEGRADED), 0),
            CassExitClass::Failure,
        );
    }

    #[test]
    fn classify_signal_kill_is_failure() {
        assert_eq!(CassExitClass::classify(None, 0), CassExitClass::Failure);
        assert_eq!(CassExitClass::classify(None, 99), CassExitClass::Failure);
    }

    #[test]
    fn class_strings_are_stable() {
        assert_eq!(CassExitClass::Success.as_str(), "success");
        assert_eq!(CassExitClass::Degraded.as_str(), "degraded");
        assert_eq!(CassExitClass::Failure.as_str(), "failure");
    }

    #[test]
    fn invocation_preserves_intent_for_provenance() {
        let inv = CassInvocation::new("cass", ["search", "rust"])
            .with_cwd("/tmp")
            .with_env("CASS_IGNORE_SOURCES_CONFIG", "1");

        assert_eq!(inv.binary(), Path::new("cass"));
        assert_eq!(inv.args(), ["search", "rust"]);
        assert_eq!(inv.cwd(), Some(Path::new("/tmp")));
        assert_eq!(inv.env_overrides().len(), 1);
        assert_eq!(inv.env_overrides()[0].0, "CASS_IGNORE_SOURCES_CONFIG");
        assert_eq!(inv.env_overrides()[0].1, "1");
    }

    #[test]
    fn synthetic_outcome_preserves_streams_and_classification() {
        let outcome = CassOutcome::synthetic(
            invocation(),
            br#"{"ok":true}"#.to_vec(),
            b"index stale\n".to_vec(),
            Some(CASS_EXIT_DEGRADED),
        );

        assert_eq!(outcome.exit_code(), Some(CASS_EXIT_DEGRADED));
        assert_eq!(outcome.class(), CassExitClass::Degraded);
        assert!(!outcome.stdout_is_empty());
        assert!(!outcome.stderr_is_empty());
        assert_eq!(outcome.stdout_utf8_lossy(), r#"{"ok":true}"#);
        assert_eq!(outcome.stderr_utf8_lossy(), "index stale\n");
        assert_eq!(outcome.elapsed(), Duration::ZERO);
        assert_eq!(outcome.invocation().binary(), Path::new("cass"));
    }

    #[test]
    fn synthetic_outcome_preserves_explicit_elapsed() {
        let outcome = CassOutcome::synthetic_with_elapsed(
            invocation(),
            b"x".to_vec(),
            Vec::new(),
            Some(CASS_EXIT_OK),
            Duration::from_millis(42),
        );

        assert_eq!(outcome.elapsed(), Duration::from_millis(42));
        assert_eq!(outcome.class(), CassExitClass::Success);
        assert!(!outcome.timed_out());
    }

    #[test]
    fn timeout_pipe_drain_does_not_join_unfinished_reader_threads() -> Result<(), String> {
        let started = Instant::now();
        let mut stdout_thread = Some(thread::spawn(|| {
            thread::sleep(TIMEOUT_PIPE_DRAIN_GRACE * 4);
            Ok(b"late stdout".to_vec())
        }));
        let mut stderr_thread = Some(thread::spawn(|| Ok(b"fast stderr".to_vec())));
        let mut stdout_bytes = None;
        let mut stderr_bytes = None;

        let (stdout, stderr) = drain_pipe_readers_after_timeout(
            &mut stdout_thread,
            &mut stderr_thread,
            &mut stdout_bytes,
            &mut stderr_bytes,
        )
        .map_err(|error| error.to_string())?;

        assert!(
            started.elapsed() < TIMEOUT_PIPE_DRAIN_GRACE * 3,
            "timeout drain must not block on unfinished pipe readers"
        );
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"fast stderr");
        Ok(())
    }

    #[test]
    fn timeout_stdout_line_reader_join_is_bounded() -> Result<(), String> {
        let started = Instant::now();
        let mut stdout_thread = Some(thread::spawn(|| {
            thread::sleep(TIMEOUT_PIPE_DRAIN_GRACE * 4);
            Ok(())
        }));

        join_finished_stdout_line_reader_after_timeout(&mut stdout_thread)
            .map_err(|error| error.to_string())?;

        assert!(
            started.elapsed() < TIMEOUT_PIPE_DRAIN_GRACE * 3,
            "timeout path must not block on unfinished stdout line readers"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_kills_and_reaps_child_when_timeout_expires() -> Result<(), String> {
        let dir = unique_test_dir("timeout-binary")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(&binary, "#!/bin/sh\nexec sleep 5\n", 0o755)?;

        let inv = CassInvocation::new(binary, ["health", "--json"])
            .with_timeout(Duration::from_millis(20));
        let outcome = inv.run().map_err(|error| error.to_string())?;

        assert!(outcome.timed_out());
        assert_eq!(outcome.class(), CassExitClass::Failure);
        assert!(outcome.elapsed() >= Duration::from_millis(20));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_times_out_when_descendant_keeps_pipe_open_after_parent_exit() -> Result<(), String> {
        if std::env::var("TMPDIR")
            .unwrap_or_default()
            .contains("USBNVME")
        {
            return Ok(());
        }
        let dir = unique_test_dir("inherited-pipe-timeout")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(
            &binary,
            "#!/bin/sh\n(sleep 5) &\nprintf '{\"sessions\":[]}\\n'\nexit 0\n",
            0o755,
        )?;

        let inv = CassInvocation::new(binary, ["sessions", "--json"])
            .with_timeout(Duration::from_millis(250));
        let started = Instant::now();
        let outcome = inv.run().map_err(|error| error.to_string())?;

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timed-out CASS subprocess must not block on inherited pipe handles",
        );
        assert!(outcome.timed_out());
        assert_eq!(outcome.class(), CassExitClass::Failure);
        assert_eq!(outcome.exit_code(), Some(CASS_EXIT_OK));
        assert_eq!(outcome.stdout_utf8_lossy(), "{\"sessions\":[]}\n");
        Ok(())
    }

    #[test]
    fn run_rejects_non_allowlisted_binary_before_spawn() -> Result<(), String> {
        let inv = CassInvocation::new("/no/such/cass-binary-eeplaceholder", ["--help"]);
        let error = match inv.run() {
            Ok(_) => return Err("custom binary should fail before spawn".to_string()),
            Err(error) => error,
        };
        assert_eq!(error.kind_str(), "invalid_binary");
        assert!(error.to_string().contains("EE-100"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_rejects_world_writable_absolute_binary_before_spawn() -> Result<(), String> {
        if std::env::var("TMPDIR")
            .unwrap_or_default()
            .contains("USBNVME")
        {
            return Ok(());
        }
        let dir = unique_test_dir("writable-absolute-binary")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(&binary, "#!/bin/sh\nprintf '{\"ok\":true}\\n'\n", 0o777)?;

        let inv = CassInvocation::new(binary, ["health", "--json"]);
        let error = match inv.run() {
            Ok(_) => return Err("world-writable cass binary should fail before spawn".to_string()),
            Err(error) => error,
        };

        assert_eq!(error.kind_str(), "invalid_binary");
        assert!(error.to_string().contains("writable"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_rejects_symlinked_absolute_binary_before_spawn() -> Result<(), String> {
        let dir = unique_test_dir("symlinked-absolute-binary")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let real_binary = dir.join("real-cass");
        let binary_link = dir.join("cass");
        let marker = dir.join("symlink-ran");
        write_executable_script(
            &real_binary,
            &format!(
                "#!/bin/sh\nprintf ran > '{}'\nprintf '{{\"ok\":true}}\\n'\n",
                marker.display()
            ),
            0o755,
        )?;
        std::os::unix::fs::symlink(&real_binary, &binary_link)
            .map_err(|error| error.to_string())?;

        let inv = CassInvocation::new(binary_link, ["health", "--json"]);
        let error = match inv.run() {
            Ok(_) => return Err("symlinked cass binary should fail before spawn".to_string()),
            Err(error) => error,
        };

        assert_eq!(error.kind_str(), "invalid_binary");
        assert!(
            error.to_string().contains("symlink"),
            "unexpected error: {error}",
        );
        assert!(!marker.exists(), "symlinked cass binary was executed");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_uses_absolute_discovered_binary_path() -> Result<(), String> {
        let dir = unique_test_dir("absolute-binary")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(&binary, "#!/bin/sh\nprintf '{\"ok\":true}\\n'\n", 0o755)?;

        let inv = CassInvocation::new(binary.clone(), ["health", "--json"]);
        let outcome = inv.run().map_err(|error| error.to_string())?;

        assert_eq!(outcome.invocation().binary(), binary.as_path());
        assert_eq!(outcome.exit_code(), Some(CASS_EXIT_OK));
        assert_eq!(outcome.stdout_utf8_lossy(), "{\"ok\":true}\n");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_classifies_nonzero_exit_with_stdout_as_degraded() -> Result<(), String> {
        let dir = unique_test_dir("nonzero-stdout-degraded")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(
            &binary,
            "#!/bin/sh\nprintf '{\"ok\":false}\\n'\nprintf 'index stale\\n' >&2\nexit 7\n",
            0o755,
        )?;

        let outcome = CassInvocation::new(binary, ["health", "--json"])
            .run()
            .map_err(|error| error.to_string())?;

        assert_eq!(outcome.exit_code(), Some(7));
        assert_eq!(outcome.class(), CassExitClass::Degraded);
        assert!(!outcome.timed_out());
        assert_eq!(outcome.stdout_utf8_lossy(), "{\"ok\":false}\n");
        assert_eq!(outcome.stderr_utf8_lossy(), "index stale\n");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_classifies_nonzero_exit_without_stdout_as_failure() -> Result<(), String> {
        let dir = unique_test_dir("nonzero-empty-stdout-failure")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(
            &binary,
            "#!/bin/sh\nprintf 'fatal cass failure\\n' >&2\nexit 42\n",
            0o755,
        )?;

        let outcome = CassInvocation::new(binary, ["health", "--json"])
            .run()
            .map_err(|error| error.to_string())?;

        assert_eq!(outcome.exit_code(), Some(42));
        assert_eq!(outcome.class(), CassExitClass::Failure);
        assert!(!outcome.timed_out());
        assert!(outcome.stdout_is_empty());
        assert_eq!(outcome.stderr_utf8_lossy(), "fatal cass failure\n");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_stdout_lines_classifies_nonzero_exit_with_stdout_as_degraded() -> Result<(), String> {
        let dir = unique_test_dir("stream-nonzero-stdout-degraded")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(
            &binary,
            "#!/bin/sh\nprintf 'line-one\\nline-two\\n'\nprintf 'stream degraded\\n' >&2\nexit 9\n",
            0o755,
        )?;

        let inv = CassInvocation::new(binary, ["view", "--json"]);
        let mut lines = Vec::new();
        let outcome = inv
            .run_stdout_lines::<_, std::convert::Infallible>(|line| {
                lines.push(line);
                Ok(())
            })
            .map_err(|error| match error {
                super::CassStreamError::Cass(error) => error.to_string(),
                super::CassStreamError::Handler(infallible) => match infallible {},
            })?;

        assert_eq!(lines, vec!["line-one".to_string(), "line-two".to_string()]);
        assert_eq!(outcome.exit_code(), Some(9));
        assert_eq!(outcome.class(), CassExitClass::Degraded);
        assert_eq!(outcome.stdout_line_count(), 2);
        assert_eq!(
            outcome.stdout_bytes_seen(),
            "line-one\n".len() + "line-two\n".len()
        );
        assert_eq!(outcome.peak_stdout_line_bytes(), "line-two".len());
        assert_eq!(outcome.stderr_utf8_lossy(), "stream degraded\n");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_stdout_lines_rejects_invalid_utf8_before_handler() -> Result<(), String> {
        let dir = unique_test_dir("stream-invalid-utf8")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(&binary, "#!/bin/sh\nprintf '\\377\\n'\n", 0o755)?;

        let inv = CassInvocation::new(binary, ["view", "--json"]);
        let mut handler_invoked = false;
        let result = inv.run_stdout_lines::<_, std::convert::Infallible>(|_line| {
            handler_invoked = true;
            Ok(())
        });

        let error = match result {
            Ok(_) => return Err("invalid UTF-8 stdout should fail the stream".to_owned()),
            Err(super::CassStreamError::Cass(error)) => error,
            Err(super::CassStreamError::Handler(infallible)) => match infallible {},
        };

        assert!(
            error.to_string().contains("was not valid UTF-8"),
            "unexpected error: {error}",
        );
        assert!(!handler_invoked, "handler ran for invalid UTF-8 stdout");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_without_timeout_uses_capped_pipe_capture() -> Result<(), String> {
        let dir = unique_test_dir("plain-run-pipe-cap")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        write_executable_script(&binary, "#!/bin/sh\nprintf 123456789\n", 0o755)?;

        let inv = CassInvocation::new(binary.clone(), ["health", "--json"]);
        let mut command = std::process::Command::new(binary);
        command.args(["health", "--json"]);
        let error = inv
            .run_with_capped_pipes(command, Instant::now(), None, 8)
            .expect_err("plain run path must reject stdout over the capture cap");

        assert_eq!(error.kind_str(), "io");
        assert!(
            error.to_string().contains("stdout exceeded 8 byte"),
            "unexpected error: {error}",
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_absolute_binary_ignores_path_env_override() -> Result<(), String> {
        let dir = unique_test_dir("absolute-ignores-path")?;
        let trusted_dir = dir.join("trusted");
        let malicious_dir = dir.join("malicious");
        fs::create_dir_all(&trusted_dir).map_err(|error| error.to_string())?;
        fs::create_dir_all(&malicious_dir).map_err(|error| error.to_string())?;
        let trusted_binary = trusted_dir.join("cass");
        let marker = dir.join("malicious-ran");
        write_executable_script(
            &trusted_binary,
            "#!/bin/sh\nprintf '{\"trusted\":true}\\n'\n",
            0o755,
        )?;
        write_executable_script(
            &malicious_dir.join("cass"),
            &format!(
                "#!/bin/sh\nprintf malicious > '{}'\nprintf '{{\"trusted\":false}}\\n'\n",
                marker.display()
            ),
            0o755,
        )?;

        let inv = CassInvocation::new(trusted_binary.clone(), ["health", "--json"])
            .with_env("PATH", malicious_dir.as_os_str());
        let outcome = inv.run().map_err(|error| error.to_string())?;

        assert_eq!(outcome.invocation().binary(), trusted_binary.as_path());
        assert_eq!(outcome.exit_code(), Some(CASS_EXIT_OK));
        assert_eq!(outcome.stdout_utf8_lossy(), "{\"trusted\":true}\n");
        assert!(
            !marker.exists(),
            "absolute cass invocation must not execute PATH override binary",
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_resolves_before_invocation_path_env_override() -> Result<(), String> {
        let dir = unique_test_dir("path-lookup-ignores-env")?;
        let trusted_dir = dir.join("trusted");
        let malicious_dir = dir.join("malicious");
        fs::create_dir_all(&trusted_dir).map_err(|error| error.to_string())?;
        fs::create_dir_all(&malicious_dir).map_err(|error| error.to_string())?;
        let trusted_binary = trusted_dir.join("cass");
        let malicious_binary = malicious_dir.join("cass");
        let marker = dir.join("malicious-ran");
        write_executable_script(
            &trusted_binary,
            "#!/bin/sh\nprintf '{\"trusted\":true}\\n'\n",
            0o755,
        )?;
        write_executable_script(
            &malicious_binary,
            &format!(
                "#!/bin/sh\nprintf malicious > '{}'\nprintf '{{\"trusted\":false}}\\n'\n",
                marker.display()
            ),
            0o755,
        )?;

        let inv = CassInvocation::new("cass", ["health", "--json"])
            .with_env("PATH", malicious_dir.as_os_str());
        let spawn_target = inv
            .validated_spawn_target_from_path_var(Some(trusted_dir.as_os_str()))
            .map_err(|error| error.to_string())?;
        assert_eq!(
            spawn_target,
            super::CassSpawnTarget::Absolute(
                trusted_binary
                    .canonicalize()
                    .map_err(|error| error.to_string())?
            )
        );

        let mut command = spawn_target.command();
        command.args(inv.args());
        for (key, value) in inv.env_overrides() {
            command.env(key, value);
        }
        let outcome = inv
            .run_with_capped_pipes(command, Instant::now(), Some(Duration::from_secs(5)), 1024)
            .map_err(|error| error.to_string())?;

        assert_eq!(outcome.exit_code(), Some(CASS_EXIT_OK));
        assert_eq!(outcome.stdout_utf8_lossy(), "{\"trusted\":true}\n");
        assert!(
            !marker.exists(),
            "PATH lookup must resolve before invocation PATH overrides are applied",
        );
        Ok(())
    }

    #[test]
    fn join_pipe_reader_returns_bytes_on_success() -> Result<(), String> {
        use super::join_pipe_reader;
        use std::thread;

        let handle = thread::spawn(|| Ok(b"hello".to_vec()));
        let bytes = join_pipe_reader(handle)
            .map_err(|error| format!("expected success but got: {error}"))?;
        assert_eq!(bytes, b"hello");
        Ok(())
    }

    #[test]
    fn join_pipe_reader_surfaces_read_error() -> Result<(), String> {
        use super::join_pipe_reader;
        use std::io;
        use std::thread;

        let handle = thread::spawn(|| Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe broke")));
        let result = join_pipe_reader(handle);
        let err = result.err().ok_or_else(|| {
            "expected join_pipe_reader to return Err for read failure".to_string()
        })?;
        assert_eq!(err.kind_str(), "io");
        assert!(err.to_string().contains("pipe read failed"));
        assert!(err.to_string().contains("pipe broke"));
        Ok(())
    }

    #[test]
    fn join_pipe_reader_surfaces_thread_panic() -> Result<(), String> {
        use super::join_pipe_reader;
        use std::thread;

        let handle: thread::JoinHandle<Result<Vec<u8>, std::io::Error>> =
            thread::spawn(|| panic!("intentional test panic")); // ubs:ignore

        std::thread::sleep(Duration::from_millis(10));

        let result = join_pipe_reader(handle);
        let err = result.err().ok_or_else(|| {
            "expected join_pipe_reader to return Err for thread panic".to_string()
        })?;
        assert_eq!(err.kind_str(), "io");
        assert!(err.to_string().contains("panicked"));
        Ok(())
    }

    #[test]
    fn read_capped_pipe_accepts_exact_limit_payload() -> Result<(), String> {
        use super::read_capped_pipe;
        use std::io::Cursor;

        let bytes = read_capped_pipe(Cursor::new(vec![b'x'; 8]), "stdout", 8)
            .map_err(|error| format!("cap-sized pipe payload should decode: {error}"))?;

        assert_eq!(bytes.len(), 8);
        Ok(())
    }

    #[test]
    fn read_capped_pipe_rejects_payload_over_limit() {
        use super::read_capped_pipe;
        use std::io::{Cursor, ErrorKind};

        let error = read_capped_pipe(Cursor::new(vec![b'x'; 9]), "stderr", 8)
            .expect_err("oversized pipe payload should fail before silent truncation");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("stderr exceeded 8 byte"),
            "unexpected error: {error}",
        );
    }

    #[test]
    fn stdout_line_stats_reject_line_count_overflow() {
        use super::{CassStdoutLine, record_stdout_line_stats};

        let mut line_count = usize::MAX;
        let mut bytes_seen = 0_usize;
        let mut peak_line_bytes = 0_usize;
        let line = CassStdoutLine {
            text: String::new(),
            delimiter_bytes: 1,
        };

        let error = record_stdout_line_stats(
            &line,
            &mut line_count,
            &mut bytes_seen,
            &mut peak_line_bytes,
        )
        .expect_err("line count overflow should fail explicitly");

        assert!(
            error.to_string().contains("line count overflowed"),
            "unexpected error: {error}",
        );
        assert_eq!(line_count, usize::MAX);
        assert_eq!(bytes_seen, 0);
        assert_eq!(peak_line_bytes, 0);
    }

    #[test]
    fn stdout_line_stats_reject_byte_count_overflow_without_partial_update() {
        use super::{CassStdoutLine, record_stdout_line_stats};

        let mut line_count = 7_usize;
        let mut bytes_seen = usize::MAX;
        let mut peak_line_bytes = 3_usize;
        let line = CassStdoutLine {
            text: String::new(),
            delimiter_bytes: 1,
        };

        let error = record_stdout_line_stats(
            &line,
            &mut line_count,
            &mut bytes_seen,
            &mut peak_line_bytes,
        )
        .expect_err("byte count overflow should fail explicitly");

        assert!(
            error.to_string().contains("byte count overflowed"),
            "unexpected error: {error}",
        );
        assert_eq!(line_count, 7);
        assert_eq!(bytes_seen, usize::MAX);
        assert_eq!(peak_line_bytes, 3);
    }

    #[test]
    fn read_bounded_stdout_line_accepts_exact_cap_crlf_line() -> Result<(), String> {
        use std::io::BufReader;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::{
            CASS_STDOUT_LINE_MAX_BYTES, CASS_STDOUT_LINE_READ_LIMIT_BYTES,
            bounded_stdout_line_buffer, read_bounded_stdout_line,
        };

        let mut input = vec![b'x'; CASS_STDOUT_LINE_MAX_BYTES];
        input.extend_from_slice(b"\r\n");
        let mut reader = BufReader::new(input.as_slice());
        let mut buf = bounded_stdout_line_buffer();
        let peak_buffer_bytes = AtomicUsize::new(0);

        let line = read_bounded_stdout_line(&mut reader, &mut buf, &peak_buffer_bytes)
            .map_err(|error| error.to_string())?
            .ok_or("expected one CRLF-terminated line at the cap")?;

        assert_eq!(line.text.len(), CASS_STDOUT_LINE_MAX_BYTES);
        assert_eq!(line.delimiter_bytes, 2);
        assert!(line.text.bytes().all(|byte| byte == b'x'));
        assert!(peak_buffer_bytes.load(Ordering::Relaxed) <= CASS_STDOUT_LINE_READ_LIMIT_BYTES);
        assert!(
            read_bounded_stdout_line(&mut reader, &mut buf, &peak_buffer_bytes)
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn read_bounded_stdout_line_accepts_exact_cap_lf_line() -> Result<(), String> {
        use std::io::BufReader;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::{
            CASS_STDOUT_LINE_MAX_BYTES, CASS_STDOUT_LINE_READ_LIMIT_BYTES,
            bounded_stdout_line_buffer, read_bounded_stdout_line,
        };

        let mut input = vec![b'x'; CASS_STDOUT_LINE_MAX_BYTES];
        input.push(b'\n');
        let mut reader = BufReader::new(input.as_slice());
        let mut buf = bounded_stdout_line_buffer();
        let peak_buffer_bytes = AtomicUsize::new(0);

        let line = read_bounded_stdout_line(&mut reader, &mut buf, &peak_buffer_bytes)
            .map_err(|error| error.to_string())?
            .ok_or("expected one LF-terminated line at the cap")?;

        assert_eq!(line.text.len(), CASS_STDOUT_LINE_MAX_BYTES);
        assert_eq!(line.delimiter_bytes, 1);
        assert!(line.text.bytes().all(|byte| byte == b'x'));
        assert!(peak_buffer_bytes.load(Ordering::Relaxed) <= CASS_STDOUT_LINE_READ_LIMIT_BYTES);
        Ok(())
    }

    #[test]
    fn read_bounded_stdout_line_tracks_eof_without_delimiter() -> Result<(), String> {
        use std::io::BufReader;
        use std::sync::atomic::AtomicUsize;

        use super::{
            bounded_stdout_line_buffer, read_bounded_stdout_line, record_stdout_line_stats,
        };

        let input = b"unterminated";
        let mut reader = BufReader::new(input.as_slice());
        let mut buf = bounded_stdout_line_buffer();
        let peak_buffer_bytes = AtomicUsize::new(0);
        let mut line_count = 0_usize;
        let mut bytes_seen = 0_usize;
        let mut peak_line_bytes = 0_usize;

        let line = read_bounded_stdout_line(&mut reader, &mut buf, &peak_buffer_bytes)
            .map_err(|error| error.to_string())?
            .ok_or("expected one EOF-terminated line")?;
        record_stdout_line_stats(
            &line,
            &mut line_count,
            &mut bytes_seen,
            &mut peak_line_bytes,
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(line.text, "unterminated");
        assert_eq!(line.delimiter_bytes, 0);
        assert_eq!(line_count, 1);
        assert_eq!(bytes_seen, input.len());
        assert_eq!(peak_line_bytes, input.len());
        Ok(())
    }

    #[test]
    fn read_bounded_stdout_line_rejects_cap_plus_one_lf_line() {
        use std::io::BufReader;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::{
            CASS_STDOUT_LINE_MAX_BYTES, CASS_STDOUT_LINE_READ_LIMIT_BYTES,
            bounded_stdout_line_buffer, read_bounded_stdout_line,
        };

        let mut input = vec![b'x'; CASS_STDOUT_LINE_MAX_BYTES + 1];
        input.push(b'\n');
        let mut reader = BufReader::new(input.as_slice());
        let mut buf = bounded_stdout_line_buffer();
        let peak_buffer_bytes = AtomicUsize::new(0);

        let error = read_bounded_stdout_line(&mut reader, &mut buf, &peak_buffer_bytes)
            .expect_err("cap+1 logical line must reject even when LF-terminated");

        assert!(
            error.to_string().contains("stdout line exceeded"),
            "unexpected error: {error}",
        );
        assert!(peak_buffer_bytes.load(Ordering::Relaxed) <= CASS_STDOUT_LINE_READ_LIMIT_BYTES);
    }

    #[test]
    fn read_bounded_stdout_line_rejects_invalid_utf8() {
        use std::io::BufReader;
        use std::sync::atomic::AtomicUsize;

        use super::{bounded_stdout_line_buffer, read_bounded_stdout_line};

        let input = [0xff, b'\n'];
        let mut reader = BufReader::new(input.as_slice());
        let mut buf = bounded_stdout_line_buffer();
        let peak_buffer_bytes = AtomicUsize::new(0);

        let error = read_bounded_stdout_line(&mut reader, &mut buf, &peak_buffer_bytes)
            .expect_err("invalid UTF-8 line must reject before String allocation succeeds");

        assert!(
            error.to_string().contains("was not valid UTF-8"),
            "unexpected error: {error}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_with_capped_pipes_terminates_child_after_stdout_cap_error() -> Result<(), String> {
        let dir = unique_test_dir("pipe-cap-kills-child")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        let marker = dir.join("child-survived");
        write_executable_script(
            &binary,
            &format!(
                "#!/bin/sh\nprintf 123456789\nsleep 1\nprintf survived > '{}'\n",
                marker.display()
            ),
            0o755,
        )?;

        let inv = CassInvocation::new(binary.clone(), ["view"]);
        let error = inv
            .run_with_capped_pipes(
                std::process::Command::new(&binary),
                Instant::now(),
                Some(Duration::from_secs(5)),
                8,
            )
            .expect_err("stdout cap error should fail the invocation");

        assert!(
            error.to_string().contains("stdout exceeded 8 byte"),
            "unexpected error: {error}",
        );
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            !marker.exists(),
            "cass child survived after a stdout pipe reader cap error",
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_with_capped_pipes_terminates_child_after_stderr_cap_error() -> Result<(), String> {
        let dir = unique_test_dir("pipe-cap-kills-child-stderr")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        let marker = dir.join("child-survived");
        write_executable_script(
            &binary,
            &format!(
                "#!/bin/sh\nprintf 123456789 >&2\nsleep 1\nprintf survived > '{}'\n",
                marker.display()
            ),
            0o755,
        )?;

        let inv = CassInvocation::new(binary.clone(), ["view"]);
        let error = inv
            .run_with_capped_pipes(
                std::process::Command::new(&binary),
                Instant::now(),
                Some(Duration::from_secs(5)),
                8,
            )
            .expect_err("stderr cap error should fail the invocation");

        assert!(
            error.to_string().contains("stderr exceeded 8 byte"),
            "unexpected error: {error}",
        );
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            !marker.exists(),
            "cass child survived after a stderr pipe reader cap error",
        );
        Ok(())
    }

    /// bd-352wc regression: a single line larger than the 1 MiB cap
    /// must be rejected *before* its bytes are fully realized into the
    /// reader's buffer. Pre-fix, `BufReader::lines()` allocated the
    /// entire line into a `String` (e.g. 2 MiB for the input below)
    /// before the post-yield length check fired — so the cap was
    /// reactive, not preventive. This test feeds a 2 MiB single-line
    /// blob with no newline and asserts both that the line-cap error
    /// fires *and* that the reader's peak byte buffer stayed well
    /// under 2 MiB (i.e. ≤ CASS_STDOUT_LINE_READ_LIMIT_BYTES).
    #[cfg(unix)]
    #[test]
    fn run_stdout_lines_bounds_buffer_below_oversize_single_line() -> Result<(), String> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::{
            CASS_STDOUT_LINE_MAX_BYTES, CASS_STDOUT_LINE_READ_LIMIT_BYTES, CassStreamError,
            spawn_stdout_line_reader,
        };

        let dir = unique_test_dir("stdout-line-cap-bounded")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        // Emit exactly 2 MiB of 'x' with no newline, then exit.
        // /dev/zero + tr is portable across macOS and Linux and avoids
        // arithmetic on `seq` (which is slow for millions of items).
        let script = "#!/bin/sh\nhead -c 2097152 /dev/zero | tr '\\0' x\n";
        write_executable_script(&binary, script, 0o755)?;

        // Subprocess timeout is a backstop — the bounded reader must
        // fire the cap error long before this fires.
        let inv =
            CassInvocation::new(binary, ["view", "--json"]).with_timeout(Duration::from_secs(5));

        let probe = Arc::new(AtomicUsize::new(0));
        let started = Instant::now();
        let stream_result = inv.run_stdout_lines_with_buffer_probe::<_, std::convert::Infallible>(
            |_line| Ok(()),
            Arc::clone(&probe),
        );
        let elapsed = started.elapsed();

        let stream_err = match stream_result {
            Ok(outcome) => {
                return Err(format!(
                    "oversize single-line input must reject with a line-cap error, got \
                     outcome with peak_buffer={peak} class={class:?}",
                    peak = outcome.peak_stdout_buffer_bytes(),
                    class = outcome.class(),
                ));
            }
            Err(error) => error,
        };
        let cass_err = match stream_err {
            CassStreamError::Cass(err) => err,
            CassStreamError::Handler(_) => {
                return Err("handler error variant should be impossible for this test".to_string());
            }
        };
        let cass_err_message = cass_err.to_string();
        if !cass_err_message.contains("stdout line exceeded") {
            return Err(format!("expected line-cap error, got: {cass_err_message}"));
        }

        let peak_buffer_bytes = probe.load(Ordering::Relaxed);
        if peak_buffer_bytes > CASS_STDOUT_LINE_READ_LIMIT_BYTES {
            return Err(format!(
                "reader buffer overshot the cap: peak={peak_buffer_bytes} bytes \
                 (read limit = {CASS_STDOUT_LINE_READ_LIMIT_BYTES}); the cap is reactive, not preventive — \
                 see bd-352wc"
            ));
        }

        // Sanity: the reader must have actually read at least the cap
        // before deciding it had overshot. A peak < CAP would mean the
        // test stub never produced enough bytes, which would be a
        // setup bug rather than a regression signal.
        if peak_buffer_bytes < CASS_STDOUT_LINE_MAX_BYTES {
            return Err(format!(
                "reader peak buffer ({peak_buffer_bytes}) is below the cap \
                 ({CASS_STDOUT_LINE_MAX_BYTES}); the stub did not deliver \
                 enough bytes for this test to be meaningful"
            ));
        }

        // And: the function returned long before the subprocess timeout
        // backstop. A subprocess-timeout return would still surface the
        // line-cap error after the cap-reactive code path ran, but it
        // would do so only after blocking on `read_line` for the full
        // subprocess budget. The fix-only path returns within
        // milliseconds; we leave generous slack for slow CI hosts.
        if elapsed >= Duration::from_secs(3) {
            return Err(format!(
                "run_stdout_lines should return promptly once the cap is \
                 tripped; elapsed was {elapsed:?}"
            ));
        }

        // Also drive the public spawn helper directly to confirm the
        // reader can be constructed and rejects oversize input in
        // isolation. This guards against the function-level wrapper
        // accidentally being the only thing enforcing the cap.
        let (probe_direct, dir_direct) = (
            Arc::new(AtomicUsize::new(0)),
            unique_test_dir("stdout-line-cap-direct")?,
        );
        fs::create_dir_all(&dir_direct).map_err(|error| error.to_string())?;
        let binary_direct = dir_direct.join("cass");
        write_executable_script(&binary_direct, script, 0o755)?;
        let mut child = std::process::Command::new(&binary_direct)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let child_stdout = child.stdout.take().ok_or("missing stdout pipe")?;
        let (rx, handle) = spawn_stdout_line_reader(child_stdout, Arc::clone(&probe_direct));
        // Drain until disconnect or first error.
        let mut saw_cap_error = false;
        while let Ok(item) = rx.recv() {
            match item {
                Ok(_line) => {}
                Err(err) => {
                    let msg = err.to_string();
                    if msg.contains("stdout line exceeded") {
                        saw_cap_error = true;
                    }
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = handle.join();
        if !saw_cap_error {
            return Err(
                "direct spawn_stdout_line_reader did not surface the line-cap error".to_string(),
            );
        }
        let peak_direct = probe_direct.load(Ordering::Relaxed);
        if peak_direct > CASS_STDOUT_LINE_READ_LIMIT_BYTES {
            return Err(format!(
                "direct reader buffer overshot the cap: peak={peak_direct} bytes \
                 (read limit = {CASS_STDOUT_LINE_READ_LIMIT_BYTES})"
            ));
        }
        Ok(())
    }
}
