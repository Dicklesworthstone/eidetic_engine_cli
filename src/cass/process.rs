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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::error::CassError;

const ALLOWLISTED_CASS_EXECUTABLE: &str = "cass";

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
        self.ensure_allowlisted_binary()?;
        let started = Instant::now();
        let mut command = self.command_for_spawn()?;
        command.args(&self.args);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &self.env_overrides {
            command.env(key, value);
        }
        let output = command.output().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CassError::BinaryNotFound {
                    binary: self.binary.clone(),
                }
            } else {
                CassError::Io {
                    message: error.to_string(),
                }
            }
        })?;
        let elapsed = started.elapsed();
        let exit_code = output.status.code();
        let stdout = output.stdout;
        let stderr = output.stderr;
        Ok(CassOutcome::new(
            self.clone(),
            stdout,
            stderr,
            exit_code,
            elapsed,
        ))
    }

    fn command_for_spawn(&self) -> Result<Command, CassError> {
        let mut command = Command::new(ALLOWLISTED_CASS_EXECUTABLE);
        if self.binary == Path::new(ALLOWLISTED_CASS_EXECUTABLE) {
            return Ok(command);
        }

        let parent = self
            .binary
            .parent()
            .ok_or_else(|| CassError::InvalidBinary {
                binary: self.binary.clone(),
                reason: "absolute CASS binary must have a parent directory".to_string(),
            })?;
        let mut path_entries = vec![parent.to_path_buf()];
        if let Some(existing_path) = std::env::var_os("PATH") {
            path_entries.extend(std::env::split_paths(&existing_path));
        }
        let path = std::env::join_paths(path_entries).map_err(|error| CassError::Io {
            message: format!("failed to construct PATH for CASS binary: {error}"),
        })?;
        command.env("PATH", path);
        Ok(command)
    }

    fn ensure_allowlisted_binary(&self) -> Result<(), CassError> {
        // Allow the default "cass" name (PATH lookup at spawn time)
        if self.binary == Path::new(ALLOWLISTED_CASS_EXECUTABLE) {
            return Ok(());
        }

        // Allow absolute paths from discovery (EE-101) - these are
        // pre-validated by discover() or discover_with_override()
        if self.binary.is_absolute()
            && self.binary.is_file()
            && self.binary.file_name() == Some(OsStr::new(ALLOWLISTED_CASS_EXECUTABLE))
        {
            return Ok(());
        }

        Err(CassError::InvalidBinary {
            binary: self.binary.clone(),
            reason: "EE-100 allowlist: binary must be 'cass' (PATH lookup) or an absolute path to a file named 'cass'"
                .to_string(),
        })
    }
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
}

impl CassOutcome {
    /// Construct a real outcome from a finished subprocess.
    fn new(
        invocation: CassInvocation,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_code: Option<i32>,
        elapsed: Duration,
    ) -> Self {
        let class = CassExitClass::classify(exit_code, stdout.len());
        Self {
            invocation,
            stdout,
            stderr,
            exit_code,
            elapsed,
            class,
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
        Self::new(invocation, stdout, stderr, exit_code, Duration::ZERO)
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
        Self::new(invocation, stdout, stderr, exit_code, elapsed)
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
    use super::{CASS_EXIT_DEGRADED, CASS_EXIT_OK, CassExitClass, CassInvocation, CassOutcome};

    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    #[cfg(unix)]
    use std::path::PathBuf;
    use std::time::Duration;
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    fn invocation() -> CassInvocation {
        CassInvocation::new("cass", ["health", "--json"])
    }

    #[cfg(unix)]
    fn unique_test_dir(prefix: &str) -> Result<PathBuf, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock moved backwards: {error}"))?
            .as_nanos();
        Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("ee-cass-process-tests")
            .join(format!("{prefix}-{}-{now}", std::process::id())))
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
    fn run_uses_absolute_discovered_binary_path() -> Result<(), String> {
        let dir = unique_test_dir("absolute-binary")?;
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let binary = dir.join("cass");
        fs::write(&binary, "#!/bin/sh\nprintf '{\"ok\":true}\\n'\n")
            .map_err(|error| error.to_string())?;
        let mut permissions = fs::metadata(&binary)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).map_err(|error| error.to_string())?;

        let inv = CassInvocation::new(binary.clone(), ["health", "--json"]);
        let outcome = inv.run().map_err(|error| error.to_string())?;

        assert_eq!(outcome.invocation().binary(), binary.as_path());
        assert_eq!(outcome.exit_code(), Some(CASS_EXIT_OK));
        assert_eq!(outcome.stdout_utf8_lossy(), "{\"ok\":true}\n");
        Ok(())
    }
}
