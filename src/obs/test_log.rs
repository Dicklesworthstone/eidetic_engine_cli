//! Structured test logging harness (bead bd-17c65.10.1 / J1).
//!
//! Emits JSON-line events to the path in `EE_TEST_LOG_PATH` so every test —
//! Rust unit, Rust integration, and bash e2e — produces a uniform, agent-
//! parseable record of what happened. Companion bash harness lives at
//! `scripts/lib/e2e_logger.sh`; both follow `docs/schemas/test_event_v1.json`.
//!
//! Design notes (see bead description for full context):
//!
//! - JSON-line so consumers can `jq` over the stream.
//! - BLAKE3 hashes of stdin/stdout so we never log secret bytes.
//! - `EE_TEST_LOG_PATH` unset → all calls no-op (opt-in by design).
//! - `EE_TEST_LOG_LEVEL` (`quiet|normal|verbose`) controls filtering.
//! - Events are appended atomically (one full line per `write` call) so
//!   multiple processes can share a log file without partial-line interleave.
//! - The crate forbids `unsafe`, so unit tests use `log_event_to`
//!   (explicit-path entry point) instead of mutating `EE_TEST_LOG_PATH`. The
//!   integration smoke test (`tests/support_logging_smoke.rs`) spawns
//!   subprocesses for env-based validation.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use crate::config::{EnvVar, read_env_var, read_env_var_os};

/// Schema version. Bump on shape changes; `docs/schemas/test_event_v1.json`
/// must move in lock-step.
pub const TEST_EVENT_SCHEMA_V1: &str = "ee.test_event.v1";

/// Env var naming the JSON-line log file. Unset → harness no-ops.
pub const ENV_TEST_LOG_PATH: &str = EnvVar::TestLogPath.name();

/// Env var controlling verbosity (`quiet|normal|verbose`).
pub const ENV_TEST_LOG_LEVEL: &str = EnvVar::TestLogLevel.name();
/// Env var naming the current test scenario.
pub const ENV_TEST_LOG_TEST_ID: &str = EnvVar::TestLogTestId.name();
/// S7 benchmark gate degraded code for measurements beyond tolerance.
pub const SWARM_SCALE_BUDGET_EXCEEDED_CODE: &str = "swarm_scale_budget_exceeded";
/// S7 benchmark gate degraded code for non-identical normalized outputs.
pub const SWARM_SCALE_NONDETERMINISM_CODE: &str = "swarm_scale_nondeterminism";

/// Event kinds emitted by the harness. `timer_lap` is suppressed at the
/// `normal` level; everything else is always emitted when a log path is
/// configured (modulo level-specific drops in `quiet` mode).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A command is about to run.
    CommandStart,
    /// A command finished, with exit code + duration + I/O hashes.
    CommandEnd,
    /// An assertion succeeded.
    AssertOk,
    /// An assertion failed (with a free-form `expected`/`actual` payload).
    AssertFail,
    /// A golden comparison was performed (`matched` lives in `fields`).
    GoldenCompare,
    /// An intermediate timer reading (suppressed at level=normal).
    TimerLap,
    /// A free-form note (e.g. test setup phase markers).
    Note,
    /// Pack-hash subcomponent hashes for determinism debugging.
    PackHashComponents,
    /// Observed DB/index generations for staleness debugging.
    DbGenerationObserved,
    /// Volatile fields stripped before determinism hashing.
    VolatileStrip,
    /// Test-event schema gate summary.
    SchemaGate,
    /// Output field selector application summary.
    FieldSelector,
    /// Performance benchmark iteration summary emitted by scripts/bench.sh.
    BenchIteration,
    /// Verification artifact manifest emitted by the J1 bash harness.
    ArtifactManifest,
    /// Determinism-lint gate summary emitted by the N4.4 e2e driver.
    LintDeterminism,
    /// Determinism proptest summary emitted by the N4.5 e2e driver.
    ProptestRun,
    /// Redaction application summary emitted by redaction matrix drivers.
    RedactionApply,
}

impl EventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommandStart => "command_start",
            Self::CommandEnd => "command_end",
            Self::AssertOk => "assert_ok",
            Self::AssertFail => "assert_fail",
            Self::GoldenCompare => "golden_compare",
            Self::TimerLap => "timer_lap",
            Self::Note => "note",
            Self::PackHashComponents => "pack_hash_components",
            Self::DbGenerationObserved => "db_generation_observed",
            Self::VolatileStrip => "volatile_strip",
            Self::SchemaGate => "schema_gate",
            Self::FieldSelector => "field_selector",
            Self::BenchIteration => "bench_iteration",
            Self::ArtifactManifest => "artifact_manifest",
            Self::LintDeterminism => "lint_determinism",
            Self::ProptestRun => "proptest_run",
            Self::RedactionApply => "redaction_apply",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 17] {
        [
            Self::CommandStart,
            Self::CommandEnd,
            Self::AssertOk,
            Self::AssertFail,
            Self::GoldenCompare,
            Self::TimerLap,
            Self::Note,
            Self::PackHashComponents,
            Self::DbGenerationObserved,
            Self::VolatileStrip,
            Self::SchemaGate,
            Self::FieldSelector,
            Self::BenchIteration,
            Self::ArtifactManifest,
            Self::LintDeterminism,
            Self::ProptestRun,
            Self::RedactionApply,
        ]
    }
}

/// Verbosity filter parsed from `EE_TEST_LOG_LEVEL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Quiet,
    Normal,
    Verbose,
}

impl LogLevel {
    fn parse(raw: Option<OsString>) -> Self {
        match raw.as_deref().and_then(|s| s.to_str()) {
            Some("quiet") => Self::Quiet,
            Some("verbose") => Self::Verbose,
            _ => Self::Normal,
        }
    }

    /// Predicate: does this level emit `kind`?
    #[must_use]
    pub const fn allows(self, kind: EventKind) -> bool {
        match self {
            Self::Quiet => matches!(
                kind,
                EventKind::CommandEnd
                    | EventKind::AssertFail
                    | EventKind::GoldenCompare
                    | EventKind::LintDeterminism
                    | EventKind::ProptestRun
                    | EventKind::RedactionApply
            ),
            Self::Normal => !matches!(kind, EventKind::TimerLap),
            Self::Verbose => true,
        }
    }
}

/// A single test event. Matches `docs/schemas/test_event_v1.json` exactly.
///
/// `fields` is a free-form map for per-test metadata that doesn't fit the
/// fixed columns (e.g. `bytes_per_token`, `arm_lexical_hits`).
#[derive(Debug, Serialize)]
pub struct TestEvent {
    pub schema: &'static str,
    pub ts: DateTime<Utc>,
    pub test_id: String,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
}

impl TestEvent {
    /// Construct an event with the schema + timestamp filled in.
    #[must_use]
    pub fn new(test_id: impl Into<String>, kind: EventKind) -> Self {
        Self {
            schema: TEST_EVENT_SCHEMA_V1,
            ts: Utc::now(),
            test_id: test_id.into(),
            kind,
            command: None,
            args: Vec::new(),
            stdin_hash: None,
            stdout_hash: None,
            stderr_excerpt: None,
            exit_code: None,
            elapsed_ms: None,
            fields: Map::new(),
        }
    }

    /// Attach a free-form field.
    #[must_use]
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }
}

/// Resolve the configured log path (env-based). `None` when unset.
#[must_use]
pub fn log_path() -> Option<PathBuf> {
    read_env_var_os(EnvVar::TestLogPath).map(PathBuf::from)
}

/// Current log level (env-based). Cheap; called per-emit so tests can flip
/// the env mid-run via subprocess `Command::env` (NOT in-process mutation
/// because this crate forbids `unsafe`).
#[must_use]
pub fn log_level() -> LogLevel {
    LogLevel::parse(read_env_var_os(EnvVar::TestLogLevel))
}

/// Resolve the current test id, falling back to a stable caller-supplied id.
#[must_use]
pub fn test_id_or(default: impl Into<String>) -> String {
    read_env_var(EnvVar::TestLogTestId).unwrap_or_else(|| default.into())
}

// Lock keeps appends atomic in-process; cross-process appends rely on the
// OS-level file append semantics (single `write` syscall per line on POSIX).
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Append a single event to an explicit path with an explicit level. The
/// testable core — wrappers below add env-based discovery.
///
/// Returns `true` when the event was written, `false` when filtered or
/// suppressed by an I/O error. Best-effort — never panics in tests.
pub fn log_event_to(path: &Path, level: LogLevel, event: &TestEvent) -> bool {
    if !level.allows(event.kind) {
        return false;
    }
    let Ok(serialized) = serde_json::to_string(event) else {
        return false;
    };
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if test_log_path_has_symlink_component(path).unwrap_or(true) {
        return false;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if test_log_path_has_symlink_component(path).unwrap_or(true) {
        return false;
    }
    if !test_log_final_path_is_regular_or_missing(path) {
        return false;
    }
    let Ok(mut file) = open_test_log_for_append(path) else {
        return false;
    };
    writeln!(file, "{serialized}").is_ok()
}

fn open_test_log_for_append(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    configure_test_log_append_options(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_test_log_append_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_test_log_append_options(_options: &mut OpenOptions) {}

fn test_log_final_path_is_regular_or_missing(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_file(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

fn test_log_path_has_symlink_component(path: &Path) -> io::Result<bool> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// Append a single event to the configured log file. No-op when unconfigured.
pub fn log_event(event: TestEvent) {
    let Some(path) = log_path() else { return };
    let _ = log_event_to(&path, log_level(), &event);
}

/// Compute BLAKE3 of a byte slice as `blake3:<64-hex>`. Used for stdin/stdout
/// so logs stay small and never carry content bytes.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

/// Truncate stderr to at most `cap` bytes (default cap is the spec's 4096).
#[must_use]
pub fn excerpt_stderr(stderr: &[u8], cap: usize) -> String {
    let stop = stderr.len().min(cap);
    String::from_utf8_lossy(&stderr[..stop]).into_owned()
}

/// Wraps `std::process::Command` so command-start/end events are emitted
/// automatically. Use this in tests instead of raw `Command::new(...)`.
pub struct CommandRecorder<'a> {
    inner: &'a mut Command,
    test_id: String,
    command_label: String,
    args: Vec<String>,
    stdin_bytes: Option<Vec<u8>>,
    stderr_cap: usize,
}

impl<'a> CommandRecorder<'a> {
    /// Wrap a command for recording. The label is the human/agent name (e.g.
    /// `"ee remember"`), distinct from the command's argv\[0\].
    pub fn new(
        test_id: impl Into<String>,
        command_label: impl Into<String>,
        command: &'a mut Command,
    ) -> Self {
        Self {
            inner: command,
            test_id: test_id.into(),
            command_label: command_label.into(),
            args: Vec::new(),
            stdin_bytes: None,
            stderr_cap: 4096,
        }
    }

    /// Record the arg list for the event; mirrors what was passed to the
    /// underlying `Command`.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for arg in args {
            self.args.push(arg.as_ref().to_string());
        }
        self
    }

    /// Attach stdin bytes; the recorder hashes them for the event.
    #[must_use]
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin_bytes = Some(bytes.into());
        self
    }

    /// Override the stderr excerpt cap.
    #[must_use]
    pub const fn stderr_cap(mut self, cap: usize) -> Self {
        self.stderr_cap = cap;
        self
    }

    /// Run the command, emit start/end events, return the `Output`.
    ///
    /// # Errors
    ///
    /// Returns the error from `Command::output()` (or the spawn/IO equivalent
    /// when stdin is piped) when the process cannot be spawned.
    pub fn run(self) -> std::io::Result<std::process::Output> {
        let mut start_event = TestEvent::new(self.test_id.clone(), EventKind::CommandStart)
            .with_field("command_label", Value::String(self.command_label.clone()))
            .with_field(
                "args",
                Value::Array(self.args.iter().cloned().map(Value::String).collect()),
            );
        if let Some(ref stdin) = self.stdin_bytes {
            start_event = start_event.with_field("stdin_hash", Value::String(hash_bytes(stdin)));
        }
        log_event(start_event);

        let started = Instant::now();
        let output = if let Some(stdin) = self.stdin_bytes {
            use std::process::Stdio;
            self.inner
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = self.inner.spawn()?;
            if let Some(mut writer) = child.stdin.take() {
                writer.write_all(&stdin)?;
            }
            child.wait_with_output()?
        } else {
            self.inner.output()?
        };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        let mut end_event = TestEvent::new(self.test_id, EventKind::CommandEnd)
            .with_field("command_label", Value::String(self.command_label))
            .with_field(
                "args",
                Value::Array(self.args.into_iter().map(Value::String).collect()),
            )
            .with_field("stdout_hash", Value::String(hash_bytes(&output.stdout)))
            .with_field(
                "stderr_excerpt",
                Value::String(excerpt_stderr(&output.stderr, self.stderr_cap)),
            );
        if let Some(code) = output.status.code() {
            end_event.exit_code = Some(code);
        }
        end_event.elapsed_ms = Some(elapsed_ms);
        log_event(end_event);

        Ok(output)
    }
}

/// Emit an `AssertOk` event with a label.
pub fn assert_ok(test_id: impl Into<String>, label: impl Into<String>) {
    log_event(
        TestEvent::new(test_id, EventKind::AssertOk)
            .with_field("label", Value::String(label.into())),
    );
}

/// Emit an `AssertFail` event with label + expected + actual.
pub fn assert_fail(
    test_id: impl Into<String>,
    label: impl Into<String>,
    expected: impl Into<Value>,
    actual: impl Into<Value>,
) {
    log_event(
        TestEvent::new(test_id, EventKind::AssertFail)
            .with_field("label", Value::String(label.into()))
            .with_field("expected", expected)
            .with_field("actual", actual),
    );
}

/// Emit a golden-comparison event with `matched` in `fields`.
pub fn golden_compare(
    test_id: impl Into<String>,
    name: impl Into<String>,
    generated_path: &Path,
    expected_path: &Path,
    matched: bool,
) {
    log_event(
        TestEvent::new(test_id, EventKind::GoldenCompare)
            .with_field("name", Value::String(name.into()))
            .with_field(
                "generated_path",
                Value::String(generated_path.display().to_string()),
            )
            .with_field(
                "expected_path",
                Value::String(expected_path.display().to_string()),
            )
            .with_field("matched", Value::Bool(matched)),
    );
}

/// Emit a free-form note.
pub fn note(test_id: impl Into<String>, message: impl Into<String>) {
    log_event(
        TestEvent::new(test_id, EventKind::Note)
            .with_field("message", Value::String(message.into())),
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn read_lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .expect("read log")
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<Value>(l).expect("valid json"))
            .collect()
    }

    #[test]
    fn log_event_to_writes_jsonl() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        log_event_to(
            tmp.path(),
            LogLevel::Normal,
            &TestEvent::new("smoke", EventKind::Note)
                .with_field("message", Value::String("hello".into())),
        );
        log_event_to(
            tmp.path(),
            LogLevel::Normal,
            &TestEvent::new("smoke", EventKind::AssertOk)
                .with_field("label", Value::String("pass".into())),
        );
        let lines = read_lines(tmp.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["schema"], TEST_EVENT_SCHEMA_V1);
        assert_eq!(lines[0]["kind"], "note");
        assert_eq!(lines[0]["fields"]["message"], "hello");
        assert_eq!(lines[1]["kind"], "assert_ok");
    }

    #[cfg(unix)]
    #[test]
    fn log_event_to_rejects_symlinked_parent_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let real_logs = tmp.path().join("real-logs");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&real_logs).expect("real logs");
        std::os::unix::fs::symlink(&real_logs, workspace.join("logs")).expect("symlink logs");
        let log_path = workspace.join("logs").join("events.jsonl");

        assert!(
            !log_event_to(
                &log_path,
                LogLevel::Normal,
                &TestEvent::new("smoke", EventKind::Note)
            ),
            "test-event logging should reject symlinked parents"
        );
        assert!(
            !real_logs.join("events.jsonl").exists(),
            "test-event logging must not write through symlinked parent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn log_event_to_rejects_symlinked_log_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).expect("log dir");
        let outside_log = tmp.path().join("outside.jsonl");
        std::fs::write(&outside_log, "outside\n").expect("outside log");
        let linked_log = log_dir.join("events.jsonl");
        std::os::unix::fs::symlink(&outside_log, &linked_log).expect("symlink log");

        assert!(
            !log_event_to(
                &linked_log,
                LogLevel::Normal,
                &TestEvent::new("smoke", EventKind::Note)
            ),
            "test-event logging should reject symlinked log files"
        );
        assert_eq!(
            std::fs::read_to_string(&outside_log).expect("outside content"),
            "outside\n",
            "test-event logging must not append through symlinked file"
        );
    }

    #[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
    #[test]
    fn open_test_log_for_append_rejects_symlinked_final_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside_log = tmp.path().join("outside.jsonl");
        std::fs::write(&outside_log, "outside\n").expect("outside log");
        let linked_log = tmp.path().join("events.jsonl");
        std::os::unix::fs::symlink(&outside_log, &linked_log).expect("symlink log");

        assert!(
            open_test_log_for_append(&linked_log).is_err(),
            "test-event log append open should not follow final symlinks"
        );
        assert_eq!(
            std::fs::read_to_string(&outside_log).expect("outside content"),
            "outside\n",
            "test-event log append open must not mutate the linked target"
        );
    }

    #[cfg(unix)]
    #[test]
    fn log_event_to_rejects_non_regular_log_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("events.jsonl");
        std::fs::create_dir(&log_path).expect("directory at log path");

        assert!(
            !log_event_to(
                &log_path,
                LogLevel::Normal,
                &TestEvent::new("smoke", EventKind::Note)
            ),
            "test-event logging should reject non-regular final log paths"
        );
        assert!(
            log_path.is_dir(),
            "test-event logging must leave a non-regular final path untouched"
        );
    }

    #[test]
    fn timer_lap_suppressed_at_normal_level() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        log_event_to(
            tmp.path(),
            LogLevel::Normal,
            &TestEvent::new("smoke", EventKind::TimerLap),
        );
        log_event_to(
            tmp.path(),
            LogLevel::Normal,
            &TestEvent::new("smoke", EventKind::Note),
        );
        let lines = read_lines(tmp.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["kind"], "note");
    }

    #[test]
    fn quiet_level_keeps_only_failure_events() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        for kind in [
            EventKind::Note,
            EventKind::AssertOk,
            EventKind::AssertFail,
            EventKind::CommandEnd,
            EventKind::ProptestRun,
            EventKind::RedactionApply,
        ] {
            log_event_to(tmp.path(), LogLevel::Quiet, &TestEvent::new("smoke", kind));
        }
        let kinds: Vec<String> = read_lines(tmp.path())
            .iter()
            .map(|l| l["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "assert_fail",
                "command_end",
                "proptest_run",
                "redaction_apply"
            ]
        );
    }

    #[test]
    fn verbose_level_keeps_everything() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        for kind in [EventKind::Note, EventKind::TimerLap, EventKind::AssertOk] {
            log_event_to(
                tmp.path(),
                LogLevel::Verbose,
                &TestEvent::new("smoke", kind),
            );
        }
        assert_eq!(read_lines(tmp.path()).len(), 3);
    }

    #[test]
    fn level_parse_recognizes_variants() {
        assert_eq!(LogLevel::parse(Some("quiet".into())), LogLevel::Quiet);
        assert_eq!(LogLevel::parse(Some("verbose".into())), LogLevel::Verbose);
        assert_eq!(LogLevel::parse(Some("normal".into())), LogLevel::Normal);
        assert_eq!(LogLevel::parse(Some("garbage".into())), LogLevel::Normal);
        assert_eq!(LogLevel::parse(None), LogLevel::Normal);
    }

    #[test]
    fn level_allows_table_is_correct() {
        // Quiet
        assert!(!LogLevel::Quiet.allows(EventKind::Note));
        assert!(!LogLevel::Quiet.allows(EventKind::AssertOk));
        assert!(LogLevel::Quiet.allows(EventKind::AssertFail));
        assert!(LogLevel::Quiet.allows(EventKind::CommandEnd));
        assert!(LogLevel::Quiet.allows(EventKind::GoldenCompare));
        // Normal
        assert!(LogLevel::Normal.allows(EventKind::Note));
        assert!(!LogLevel::Normal.allows(EventKind::TimerLap));
        // Verbose
        assert!(LogLevel::Verbose.allows(EventKind::TimerLap));
        assert!(LogLevel::Verbose.allows(EventKind::Note));
    }

    #[test]
    fn hash_bytes_is_blake3_prefixed_and_deterministic() {
        let h1 = hash_bytes(b"abc");
        let h2 = hash_bytes(b"abc");
        assert_eq!(h1, h2);
        assert!(h1.starts_with("blake3:"));
        assert_eq!(h1.len(), "blake3:".len() + 64);
        assert_ne!(hash_bytes(b"abc"), hash_bytes(b"def"));
    }

    #[test]
    fn excerpt_stderr_caps_to_size() {
        let huge = vec![b'x'; 10_000];
        assert_eq!(excerpt_stderr(&huge, 4096).len(), 4096);
        let small = b"short stderr";
        assert_eq!(excerpt_stderr(small, 4096), "short stderr");
    }

    #[test]
    fn empty_log_path_function_returns_none_when_unset() {
        // Read-only env access is safe even without unsafe.
        if read_env_var_os(EnvVar::TestLogPath).is_none() {
            assert!(log_path().is_none());
        } else {
            // Test runner has the var set externally — skip rather than fail.
        }
    }

    #[test]
    fn event_omits_empty_optional_fields_in_json() {
        let event = TestEvent::new("smoke", EventKind::Note);
        let json = serde_json::to_value(&event).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("stdin_hash"));
        assert!(!obj.contains_key("stdout_hash"));
        assert!(!obj.contains_key("exit_code"));
        assert!(!obj.contains_key("elapsed_ms"));
        assert!(!obj.contains_key("fields"));
        assert!(obj.contains_key("schema"));
        assert!(obj.contains_key("ts"));
        assert!(obj.contains_key("test_id"));
        assert!(obj.contains_key("kind"));
    }
}
