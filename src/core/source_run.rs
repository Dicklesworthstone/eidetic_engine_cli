//! Bounded external source runner for swarm coordination evidence.
//!
//! This module is intentionally core-facing and CLI-free. It runs one external
//! source command under an explicit budget, captures bounded output tails, and
//! returns an `ee.source_run_evidence.v1` record that higher-level swarm
//! surfaces can persist or embed.

use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group};
use serde::{Deserialize, Serialize};

use crate::core::preflight_guard::classify_repair_action_for_preflight;
use crate::models::RecoveryKind;
use crate::models::producer::{ProducerMetadata, ProducerSourceSystem};
use crate::policy::redact_secret_like_content;

pub const SOURCE_RUN_EVIDENCE_SCHEMA_V1: &str = "ee.source_run_evidence.v1";

const DEFAULT_TAIL_BYTES_MAX: usize = 8192;
const TIMEOUT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TIMEOUT_PIPE_DRAIN_GRACE: Duration = Duration::from_millis(250);
/// Smallest source-run timeout. `ee.source_run_evidence.v1` requires
/// `timing.timeoutMs >= 1` (bd-29xk0), and a zero timeout would also fire
/// instantly in the poll loop, so requests are floored to 1ms.
const MIN_SOURCE_RUN_TIMEOUT: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunKind {
    AgentMail,
    Beads,
    Bv,
    Cass,
    Rch,
    Git,
    Ee,
    SwarmCollector,
    Shell,
    Other,
}

impl SourceRunKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentMail => "agent_mail",
            Self::Beads => "beads",
            Self::Bv => "bv",
            Self::Cass => "cass",
            Self::Rch => "rch",
            Self::Git => "git",
            Self::Ee => "ee",
            Self::SwarmCollector => "swarm_collector",
            Self::Shell => "shell",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunSource {
    pub kind: SourceRunKind,
    pub source_id: String,
    pub operation: String,
}

impl SourceRunSource {
    #[must_use]
    pub fn new(
        kind: SourceRunKind,
        source_id: impl Into<String>,
        operation: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            source_id: source_id.into(),
            operation: operation.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunArgvRedaction {
    LiteralSafe,
    Redacted,
    HashOnly,
}

impl SourceRunArgvRedaction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LiteralSafe => "literal_safe",
            Self::Redacted => "redacted",
            Self::HashOnly => "hash_only",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunRequiredMode {
    BestEffortCoordination,
    RequiredCoordination,
    RemoteVerificationRequired,
    MutationGuard,
    ReadOnlyEvidence,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunFailurePolicy {
    ContinueDegraded,
    FailClosed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunPolicy {
    pub required_mode: SourceRunRequiredMode,
    pub on_failure: SourceRunFailurePolicy,
    pub fail_closed_reason: Option<String>,
}

impl SourceRunPolicy {
    #[must_use]
    pub fn best_effort_coordination() -> Self {
        Self {
            required_mode: SourceRunRequiredMode::BestEffortCoordination,
            on_failure: SourceRunFailurePolicy::ContinueDegraded,
            fail_closed_reason: None,
        }
    }

    #[must_use]
    pub fn fail_closed(required_mode: SourceRunRequiredMode, reason: impl Into<String>) -> Self {
        Self {
            required_mode,
            on_failure: SourceRunFailurePolicy::FailClosed,
            fail_closed_reason: Some(reason.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRunCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env_overrides: Vec<(String, String)>,
    pub display: Option<String>,
    pub argv_redaction: SourceRunArgvRedaction,
}

impl SourceRunCommand {
    #[must_use]
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env_overrides: Vec::new(),
            display: None,
            argv_redaction: SourceRunArgvRedaction::LiteralSafe,
        }
    }

    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_overrides.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn with_display(mut self, display: impl Into<String>) -> Self {
        self.display = Some(display.into());
        self
    }

    #[must_use]
    pub const fn with_argv_redaction(mut self, redaction: SourceRunArgvRedaction) -> Self {
        self.argv_redaction = redaction;
        self
    }

    #[must_use]
    pub fn actual_argv(&self) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.args.len() + 1);
        argv.push(self.program.clone());
        argv.extend(self.args.iter().cloned());
        argv
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRunRequest {
    pub source: SourceRunSource,
    pub command: SourceRunCommand,
    pub policy: SourceRunPolicy,
    pub timeout: Duration,
    pub tail_bytes_max: usize,
    pub artifacts: Vec<SourceRunArtifact>,
    pub producer: ProducerMetadata,
}

impl SourceRunRequest {
    #[must_use]
    pub fn new(source: SourceRunSource, command: SourceRunCommand, timeout: Duration) -> Self {
        Self {
            source,
            command,
            policy: SourceRunPolicy::best_effort_coordination(),
            // Floor the timeout so the v1 evidence schema's `timeoutMs >= 1`
            // invariant holds and the poll loop never times out instantly.
            timeout: timeout.max(MIN_SOURCE_RUN_TIMEOUT),
            tail_bytes_max: DEFAULT_TAIL_BYTES_MAX,
            artifacts: Vec::new(),
            producer: ProducerMetadata::unknown_agent(
                ProducerSourceSystem::Verification,
                None,
                None,
                None,
                None,
            ),
        }
    }

    #[must_use]
    pub fn with_policy(mut self, policy: SourceRunPolicy) -> Self {
        self.policy = policy;
        self
    }

    #[must_use]
    pub const fn with_tail_bytes_max(mut self, tail_bytes_max: usize) -> Self {
        self.tail_bytes_max = tail_bytes_max;
        self
    }

    #[must_use]
    pub fn with_artifacts(mut self, artifacts: Vec<SourceRunArtifact>) -> Self {
        self.artifacts = artifacts;
        self
    }

    #[must_use]
    pub fn with_producer(mut self, producer: ProducerMetadata) -> Self {
        self.producer = producer;
        self
    }
}

pub trait SourceRunClock {
    fn now_rfc3339(&self) -> Option<String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSourceRunClock;

impl SourceRunClock for SystemSourceRunClock {
    fn now_rfc3339(&self) -> Option<String> {
        Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true))
    }
}

pub trait SourceRunExecutor {
    fn execute(&self, request: &SourceRunRequest) -> SourceRunExecution;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSourceRunExecutor;

impl SourceRunExecutor for SystemSourceRunExecutor {
    fn execute(&self, request: &SourceRunRequest) -> SourceRunExecution {
        run_system_source_command(request)
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "execution variants keep captures inline so injected test runners can return one self-contained outcome"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceRunExecution {
    Completed {
        exit_code: Option<i32>,
        signal: Option<String>,
        stdout: SourceRunPipeCapture,
        stderr: SourceRunPipeCapture,
        elapsed: Duration,
    },
    TimedOut {
        exit_code: Option<i32>,
        signal: Option<String>,
        stdout: SourceRunPipeCapture,
        stderr: SourceRunPipeCapture,
        elapsed: Duration,
        killed_own_child: bool,
    },
    SpawnFailed {
        error: String,
        elapsed: Duration,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRunPipeCapture {
    total_bytes: usize,
    tail: Vec<u8>,
    full_hash: Option<String>,
}

impl SourceRunPipeCapture {
    #[must_use]
    pub fn from_bytes(bytes: &[u8], tail_bytes_max: usize) -> Self {
        let mut capture = TailCapture::new(tail_bytes_max);
        capture.push(bytes);
        capture.finish()
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            total_bytes: 0,
            tail: Vec::new(),
            full_hash: None,
        }
    }
}

struct TailCapture {
    tail_bytes_max: usize,
    total_bytes: usize,
    tail: Vec<u8>,
    hasher: blake3::Hasher,
}

impl TailCapture {
    fn new(tail_bytes_max: usize) -> Self {
        Self {
            tail_bytes_max,
            total_bytes: 0,
            tail: Vec::new(),
            hasher: blake3::Hasher::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        self.hasher.update(bytes);
        if self.tail_bytes_max == 0 {
            self.tail.clear();
            return;
        }
        if bytes.len() >= self.tail_bytes_max {
            self.tail = bytes[bytes.len() - self.tail_bytes_max..].to_vec();
            return;
        }
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > self.tail_bytes_max {
            let excess = self.tail.len() - self.tail_bytes_max;
            self.tail.drain(..excess);
        }
    }

    fn finish(self) -> SourceRunPipeCapture {
        SourceRunPipeCapture {
            total_bytes: self.total_bytes,
            tail: self.tail,
            full_hash: if self.total_bytes == 0 {
                None
            } else {
                Some(format!("blake3:{}", self.hasher.finalize().to_hex()))
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunEvidence {
    pub schema: &'static str,
    pub run_id: String,
    pub captured_at: Option<String>,
    pub source: SourceRunSource,
    pub command: SourceRunCommandEvidence,
    pub policy: SourceRunPolicy,
    pub timing: SourceRunTiming,
    pub status: SourceRunStatus,
    pub exit: SourceRunExit,
    pub output: SourceRunOutput,
    pub degraded: Vec<SourceRunDegradation>,
    pub recovery: Vec<SourceRunRecoveryAction>,
    pub artifacts: Vec<SourceRunArtifact>,
    pub redaction: SourceRunRedaction,
    pub provenance_hash: String,
    pub producer: ProducerMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunCommandEvidence {
    pub display: Option<String>,
    pub argv: Vec<String>,
    pub argv_redaction: SourceRunArgvRedaction,
    pub command_hash: String,
    pub normalized_argv_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunTiming {
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub timeout_ms: u64,
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunStatus {
    Passed,
    Failed,
    TimedOut,
    SpawnFailed,
    ParseFailed,
    StaleSource,
    MalformedStore,
    Blocked,
}

impl SourceRunStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::SpawnFailed => "spawn_failed",
            Self::ParseFailed => "parse_failed",
            Self::StaleSource => "stale_source",
            Self::MalformedStore => "malformed_store",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunExit {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub killed_own_child: bool,
    pub killed_peer_processes: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunOutput {
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub tail_bytes_max: usize,
    pub stdout_hash: Option<String>,
    pub stderr_excerpt_hash: Option<String>,
    pub redacted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceRunDegradation {
    pub code: String,
    pub severity: SourceRunSeverity,
    pub message: String,
    pub repair: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunSeverity {
    Info,
    Low,
    Warning,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunRecoveryAction {
    pub priority: u32,
    pub kind: SourceRunRecoveryKind,
    pub command: Option<String>,
    pub message: String,
    pub repair_safety: SourceRunRepairSafety,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunRecoveryKind {
    Retry,
    RetryWithLongerTimeout,
    UseStaticFallback,
    RepairSubstrateAfterApproval,
    ManualCoordination,
    FailClosed,
    SkipSource,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunRepairSafety {
    pub risk_class: String,
    pub preflight_command: Option<String>,
    pub requires_human_approval: bool,
    pub mutates_external_state: bool,
    pub mutates_tracker_state: bool,
    pub privacy_class: String,
    pub next_action: String,
    pub rule_id: String,
    pub source: String,
    pub reason_code: String,
    pub evidence: Vec<String>,
    pub preconditions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunArtifact {
    pub kind: SourceRunArtifactKind,
    pub reference: String,
    pub content_hash: String,
    pub redaction_status: SourceRunArtifactRedactionStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunArtifactKind {
    Bead,
    Verification,
    SupportBundle,
    LogExcerpt,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunArtifactRedactionStatus {
    IdOnly,
    RedactedSummary,
    HashOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRunRedaction {
    pub raw_bodies_included: bool,
    pub raw_env_included: bool,
    pub secret_scan_applied: bool,
    pub path_policy: SourceRunPathPolicy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRunPathPolicy {
    None,
    RedactHome,
    HashPaths,
    LabelsOnly,
}

#[must_use]
pub fn run_source_command(request: &SourceRunRequest) -> SourceRunEvidence {
    run_source_command_with(request, &SystemSourceRunExecutor, &SystemSourceRunClock)
}

#[must_use]
pub fn run_source_command_with(
    request: &SourceRunRequest,
    executor: &dyn SourceRunExecutor,
    clock: &dyn SourceRunClock,
) -> SourceRunEvidence {
    let started_at = clock.now_rfc3339();
    let execution = executor.execute(request);
    let finished_at = clock.now_rfc3339();
    build_evidence(request, execution, started_at, finished_at)
}

fn build_evidence(
    request: &SourceRunRequest,
    execution: SourceRunExecution,
    started_at: Option<String>,
    finished_at: Option<String>,
) -> SourceRunEvidence {
    let command_hash = hash_actual_command(&request.command);
    let command = command_evidence(&request.command, &command_hash);
    let captured_at = finished_at.clone();
    let run_id = source_run_id(&request.source, captured_at.as_deref(), &command_hash);
    let (status, exit, output, elapsed_ms) = execution_evidence(&execution, request.tail_bytes_max);
    let mut degraded = degradation_for_status(status, &request.source, &request.policy);
    let mut recovery = recovery_for_status(status, &request.policy);
    if status == SourceRunStatus::Passed {
        degraded.clear();
        recovery.clear();
    }

    let mut producer = request.producer.clone();
    if producer.run.run_id.is_none() {
        producer.run.run_id = Some(run_id.clone());
    }
    if producer.observed_at.is_none() {
        producer.observed_at = captured_at.clone();
    }

    let redaction = SourceRunRedaction {
        raw_bodies_included: false,
        raw_env_included: false,
        secret_scan_applied: true,
        path_policy: SourceRunPathPolicy::LabelsOnly,
    };
    let timing = SourceRunTiming {
        started_at,
        finished_at,
        // Defense in depth: `timeout` is a pub field that a caller could mutate
        // back to zero after construction, so guarantee the v1 schema's
        // `timeoutMs >= 1` minimum at the serialization boundary too (bd-29xk0).
        timeout_ms: duration_millis(request.timeout).max(1),
        elapsed_ms,
    };
    let mut evidence = SourceRunEvidence {
        schema: SOURCE_RUN_EVIDENCE_SCHEMA_V1,
        run_id,
        captured_at,
        source: request.source.clone(),
        command,
        policy: request.policy.clone(),
        timing,
        status,
        exit,
        output,
        degraded,
        recovery,
        artifacts: request.artifacts.clone(),
        redaction,
        provenance_hash: String::new(),
        producer,
    };
    evidence.provenance_hash = provenance_hash(&evidence);
    evidence
}

fn execution_evidence(
    execution: &SourceRunExecution,
    tail_bytes_max: usize,
) -> (SourceRunStatus, SourceRunExit, SourceRunOutput, Option<u64>) {
    match execution {
        SourceRunExecution::Completed {
            exit_code,
            signal,
            stdout,
            stderr,
            elapsed,
        } => {
            let status = if *exit_code == Some(0) {
                SourceRunStatus::Passed
            } else {
                SourceRunStatus::Failed
            };
            (
                status,
                SourceRunExit {
                    exit_code: *exit_code,
                    signal: signal.clone(),
                    killed_own_child: false,
                    killed_peer_processes: false,
                },
                output_evidence(stdout, stderr, tail_bytes_max),
                Some(duration_millis(*elapsed)),
            )
        }
        SourceRunExecution::TimedOut {
            exit_code,
            signal,
            stdout,
            stderr,
            elapsed,
            killed_own_child,
        } => (
            SourceRunStatus::TimedOut,
            SourceRunExit {
                exit_code: *exit_code,
                signal: signal.clone(),
                killed_own_child: *killed_own_child,
                killed_peer_processes: false,
            },
            output_evidence(stdout, stderr, tail_bytes_max),
            Some(duration_millis(*elapsed)),
        ),
        SourceRunExecution::SpawnFailed { error, elapsed } => {
            let stderr = SourceRunPipeCapture::from_bytes(error.as_bytes(), tail_bytes_max);
            (
                SourceRunStatus::SpawnFailed,
                SourceRunExit {
                    exit_code: None,
                    signal: None,
                    killed_own_child: false,
                    killed_peer_processes: false,
                },
                output_evidence(&SourceRunPipeCapture::empty(), &stderr, tail_bytes_max),
                Some(duration_millis(*elapsed)),
            )
        }
    }
}

fn output_evidence(
    stdout: &SourceRunPipeCapture,
    stderr: &SourceRunPipeCapture,
    tail_bytes_max: usize,
) -> SourceRunOutput {
    let (stdout_tail, stdout_redacted) = redacted_tail(stdout, tail_bytes_max);
    let (stderr_tail, stderr_redacted) = redacted_tail(stderr, tail_bytes_max);
    SourceRunOutput {
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
        stdout_tail,
        stderr_excerpt_hash: stderr_tail
            .as_deref()
            .map(|tail| blake3_hash(tail.as_bytes())),
        stderr_tail,
        tail_bytes_max,
        stdout_hash: stdout.full_hash.clone(),
        redacted: stdout_redacted || stderr_redacted,
    }
}

fn redacted_tail(capture: &SourceRunPipeCapture, tail_bytes_max: usize) -> (Option<String>, bool) {
    if tail_bytes_max == 0 || capture.tail.is_empty() {
        return (None, false);
    }
    let tail = if capture.tail.len() > tail_bytes_max {
        &capture.tail[capture.tail.len() - tail_bytes_max..]
    } else {
        &capture.tail
    };
    let raw = String::from_utf8_lossy(tail).into_owned();
    let redaction = redact_secret_like_content(&raw);
    (Some(redaction.content), redaction.redacted)
}

fn command_evidence(command: &SourceRunCommand, command_hash: &str) -> SourceRunCommandEvidence {
    let argv = match command.argv_redaction {
        SourceRunArgvRedaction::LiteralSafe => command.actual_argv(),
        SourceRunArgvRedaction::Redacted => vec!["<redacted-argv>".to_owned()],
        SourceRunArgvRedaction::HashOnly => vec![format!("hash-only:{command_hash}")],
    };
    SourceRunCommandEvidence {
        display: if command.argv_redaction == SourceRunArgvRedaction::LiteralSafe {
            command.display.clone()
        } else {
            None
        },
        normalized_argv_hash: hash_argv(&argv),
        argv,
        argv_redaction: command.argv_redaction,
        command_hash: command_hash.to_owned(),
    }
}

fn hash_actual_command(command: &SourceRunCommand) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_field(&mut hasher, "program", &command.program);
    for (index, arg) in command.args.iter().enumerate() {
        hash_field(&mut hasher, &format!("arg:{index}"), arg);
    }
    if let Some(cwd) = &command.cwd {
        hash_field(&mut hasher, "cwd", &cwd.to_string_lossy());
    }
    for (key, value) in &command.env_overrides {
        hash_field(&mut hasher, &format!("env:{key}"), value);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_argv(argv: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    for (index, value) in argv.iter().enumerate() {
        hash_field(&mut hasher, &format!("argv:{index}"), value);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_field(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(label.as_bytes());
    hasher.update(&[0]);
    hasher.update(value.as_bytes());
    hasher.update(&[0]);
}

fn blake3_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn source_run_id(
    source: &SourceRunSource,
    captured_at: Option<&str>,
    command_hash: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_field(&mut hasher, "kind", source.kind.as_str());
    hash_field(&mut hasher, "source_id", &source.source_id);
    hash_field(&mut hasher, "operation", &source.operation);
    hash_field(&mut hasher, "captured_at", captured_at.unwrap_or("unknown"));
    hash_field(&mut hasher, "command_hash", command_hash);
    let digest = hasher.finalize().to_hex().to_string();
    format!("source_run_{}_{}", source.kind.as_str(), &digest[..24])
}

fn provenance_hash(evidence: &SourceRunEvidence) -> String {
    let mut clone = evidence.clone();
    clone.provenance_hash.clear();
    let canonical =
        serde_json::to_string(&clone).unwrap_or_else(|_| SOURCE_RUN_EVIDENCE_SCHEMA_V1.to_owned());
    blake3_hash(canonical.as_bytes())
}

fn degradation_for_status(
    status: SourceRunStatus,
    source: &SourceRunSource,
    policy: &SourceRunPolicy,
) -> Vec<SourceRunDegradation> {
    let severity = if policy.on_failure == SourceRunFailurePolicy::FailClosed {
        SourceRunSeverity::High
    } else {
        SourceRunSeverity::Warning
    };
    match status {
        SourceRunStatus::Passed => Vec::new(),
        SourceRunStatus::Failed => vec![SourceRunDegradation {
            code: "source_run_failed".to_owned(),
            severity,
            message: format!("{} source command exited unsuccessfully.", source.source_id),
            repair: Some(
                "Inspect the bounded stderr tail and retry after fixing the source command."
                    .to_owned(),
            ),
        }],
        SourceRunStatus::TimedOut => vec![SourceRunDegradation {
            code: "source_run_timeout".to_owned(),
            severity,
            message: format!(
                "{} source command exceeded its timeout budget.",
                source.source_id
            ),
            repair: Some(
                "Retry with a longer explicit timeout only if the source is required.".to_owned(),
            ),
        }],
        SourceRunStatus::SpawnFailed => vec![SourceRunDegradation {
            code: "source_run_spawn_failed".to_owned(),
            severity,
            message: format!("{} source command could not be spawned.", source.source_id),
            repair: Some(
                "Check that the source tool is installed and available on PATH.".to_owned(),
            ),
        }],
        SourceRunStatus::ParseFailed
        | SourceRunStatus::StaleSource
        | SourceRunStatus::MalformedStore
        | SourceRunStatus::Blocked => Vec::new(),
    }
}

fn recovery_for_status(
    status: SourceRunStatus,
    policy: &SourceRunPolicy,
) -> Vec<SourceRunRecoveryAction> {
    match status {
        SourceRunStatus::Passed => Vec::new(),
        SourceRunStatus::TimedOut => vec![SourceRunRecoveryAction {
            priority: 1,
            kind: SourceRunRecoveryKind::RetryWithLongerTimeout,
            command: None,
            message:
                "Retry the same source with a longer explicit timeout if the evidence is required."
                    .to_owned(),
            repair_safety: source_run_manual_repair_safety(
                "source_run_retry_timeout_manual_only",
                &["source_run_recovery_without_command", "source_run_timeout"],
                &["caller_must_set_explicit_timeout"],
            ),
        }],
        SourceRunStatus::Failed | SourceRunStatus::SpawnFailed => {
            let kind = if policy.on_failure == SourceRunFailurePolicy::FailClosed {
                SourceRunRecoveryKind::FailClosed
            } else {
                SourceRunRecoveryKind::UseStaticFallback
            };
            let (reason_code, evidence, preconditions) =
                if policy.on_failure == SourceRunFailurePolicy::FailClosed {
                    (
                        "source_run_fail_closed_manual_only",
                        [
                            "source_run_recovery_without_command",
                            "source_run_fail_closed_policy",
                        ],
                        ["operator_decision_required"],
                    )
                } else {
                    (
                        "source_run_static_fallback_manual_only",
                        [
                            "source_run_recovery_without_command",
                            "source_run_best_effort_policy",
                        ],
                        ["fallback_must_be_documented"],
                    )
                };
            vec![SourceRunRecoveryAction {
                priority: 1,
                kind,
                command: None,
                message: "Use a documented fallback only when the source is best-effort; otherwise fail closed.".to_owned(),
                repair_safety: source_run_manual_repair_safety(
                    reason_code,
                    &evidence,
                    &preconditions,
                ),
            }]
        }
        SourceRunStatus::ParseFailed
        | SourceRunStatus::StaleSource
        | SourceRunStatus::MalformedStore
        | SourceRunStatus::Blocked => Vec::new(),
    }
}

fn source_run_manual_repair_safety(
    reason_code: &str,
    evidence: &[&str],
    preconditions: &[&str],
) -> SourceRunRepairSafety {
    let assessment = classify_repair_action_for_preflight(RecoveryKind::None, None);
    SourceRunRepairSafety {
        risk_class: assessment.risk_class.to_owned(),
        preflight_command: assessment.preflight_command,
        requires_human_approval: assessment.requires_human_approval,
        mutates_external_state: assessment.mutates_external_state,
        mutates_tracker_state: assessment.mutates_tracker_state,
        privacy_class: assessment.privacy_class.to_owned(),
        next_action: assessment.next_action.as_str().to_owned(),
        rule_id: assessment.rule_id.to_owned(),
        source: assessment.source.to_owned(),
        reason_code: reason_code.to_owned(),
        evidence: evidence.iter().map(|item| (*item).to_owned()).collect(),
        preconditions: preconditions
            .iter()
            .map(|item| (*item).to_owned())
            .collect(),
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn run_system_source_command(request: &SourceRunRequest) -> SourceRunExecution {
    let started = Instant::now();
    let mut command = Command::new(&request.command.program);
    command.args(&request.command.args);
    if let Some(cwd) = &request.command.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &request.command.env_overrides {
        command.env(key, value);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return SourceRunExecution::SpawnFailed {
                error: error.to_string(),
                elapsed: started.elapsed(),
            };
        }
    };
    #[cfg(unix)]
    let child_group = Pid::from_child(&child);

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            return SourceRunExecution::SpawnFailed {
                error: "source command stdout pipe was not available".to_owned(),
                elapsed: started.elapsed(),
            };
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            return SourceRunExecution::SpawnFailed {
                error: "source command stderr pipe was not available".to_owned(),
                elapsed: started.elapsed(),
            };
        }
    };

    let tail_bytes_max = request.tail_bytes_max;
    let stdout_thread = thread::spawn(move || read_tail_pipe(stdout, tail_bytes_max));
    let stderr_thread = thread::spawn(move || read_tail_pipe(stderr, tail_bytes_max));
    let mut stdout_thread = Some(stdout_thread);
    let mut stderr_thread = Some(stderr_thread);
    let mut child_status = None;

    loop {
        if child_status.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    child_status = Some(status);
                }
                Ok(None) => {}
                Err(error) => {
                    #[cfg(unix)]
                    terminate_source_process_group(child_group);
                    terminate_child_after_error(&mut child);
                    let (stdout, mut stderr) = drain_capture_readers_after_timeout(
                        &mut stdout_thread,
                        &mut stderr_thread,
                        tail_bytes_max,
                    );
                    append_capture_error(
                        &mut stderr,
                        &format!("source command wait failed: {error}"),
                        tail_bytes_max,
                    );
                    return SourceRunExecution::Completed {
                        exit_code: None,
                        signal: None,
                        stdout,
                        stderr,
                        elapsed: started.elapsed(),
                    };
                }
            }
        }

        if child_status.is_some() && capture_readers_finished(&stdout_thread, &stderr_thread) {
            let Some(status) = child_status.take() else {
                continue;
            };
            let stdout = join_capture_reader(&mut stdout_thread, tail_bytes_max);
            let stderr = join_capture_reader(&mut stderr_thread, tail_bytes_max);
            return SourceRunExecution::Completed {
                exit_code: status.code(),
                signal: exit_signal(&status),
                stdout,
                stderr,
                elapsed: started.elapsed(),
            };
        }

        let elapsed = started.elapsed();
        if elapsed >= request.timeout {
            #[cfg(unix)]
            terminate_source_process_group(child_group);
            let killed_own_child = if child_status.is_none() {
                terminate_child_after_error(&mut child)
            } else {
                false
            };
            let status = child_status.take().or_else(|| child.wait().ok());
            let (stdout, stderr) = drain_capture_readers_after_timeout(
                &mut stdout_thread,
                &mut stderr_thread,
                tail_bytes_max,
            );
            return SourceRunExecution::TimedOut {
                exit_code: status.as_ref().and_then(std::process::ExitStatus::code),
                signal: status.as_ref().and_then(exit_signal),
                stdout,
                stderr,
                elapsed: started.elapsed(),
                killed_own_child,
            };
        }

        thread::sleep(
            request
                .timeout
                .saturating_sub(elapsed)
                .min(TIMEOUT_POLL_INTERVAL),
        );
    }
}

fn capture_readers_finished(
    stdout_thread: &Option<thread::JoinHandle<io::Result<SourceRunPipeCapture>>>,
    stderr_thread: &Option<thread::JoinHandle<io::Result<SourceRunPipeCapture>>>,
) -> bool {
    stdout_thread
        .as_ref()
        .is_none_or(thread::JoinHandle::is_finished)
        && stderr_thread
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
}

fn read_tail_pipe<R: Read>(
    mut reader: R,
    tail_bytes_max: usize,
) -> io::Result<SourceRunPipeCapture> {
    let mut capture = TailCapture::new(tail_bytes_max);
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => capture.push(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(capture.finish())
}

fn join_capture_reader(
    handle: &mut Option<thread::JoinHandle<io::Result<SourceRunPipeCapture>>>,
    tail_bytes_max: usize,
) -> SourceRunPipeCapture {
    let Some(handle) = handle.take() else {
        return SourceRunPipeCapture::empty();
    };
    match handle.join() {
        Ok(Ok(capture)) => capture,
        Ok(Err(error)) => SourceRunPipeCapture::from_bytes(
            format!("source command pipe read failed: {error}").as_bytes(),
            tail_bytes_max,
        ),
        Err(_panic) => SourceRunPipeCapture::from_bytes(
            b"source command pipe reader thread panicked",
            tail_bytes_max,
        ),
    }
}

fn join_finished_capture_reader(
    handle: &mut Option<thread::JoinHandle<io::Result<SourceRunPipeCapture>>>,
    tail_bytes_max: usize,
) -> SourceRunPipeCapture {
    let Some(reader) = handle.as_ref() else {
        return SourceRunPipeCapture::empty();
    };
    if reader.is_finished() {
        return join_capture_reader(handle, tail_bytes_max);
    }
    SourceRunPipeCapture::from_bytes(
        b"source command pipe drain timed out; output tail unavailable",
        tail_bytes_max,
    )
}

fn drain_capture_readers_after_timeout(
    stdout_thread: &mut Option<thread::JoinHandle<io::Result<SourceRunPipeCapture>>>,
    stderr_thread: &mut Option<thread::JoinHandle<io::Result<SourceRunPipeCapture>>>,
    tail_bytes_max: usize,
) -> (SourceRunPipeCapture, SourceRunPipeCapture) {
    let deadline = Instant::now() + TIMEOUT_PIPE_DRAIN_GRACE;
    loop {
        if capture_readers_finished(stdout_thread, stderr_thread) {
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
    (
        join_finished_capture_reader(stdout_thread, tail_bytes_max),
        join_finished_capture_reader(stderr_thread, tail_bytes_max),
    )
}

fn append_capture_error(capture: &mut SourceRunPipeCapture, message: &str, tail_bytes_max: usize) {
    let mut rebuilt = TailCapture::new(tail_bytes_max);
    rebuilt.push(&capture.tail);
    rebuilt.push(message.as_bytes());
    *capture = rebuilt.finish();
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<String> {
    status.signal().map(|signal| format!("signal:{signal}"))
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<String> {
    None
}

#[cfg(unix)]
fn terminate_source_process_group(child_group: Pid) {
    if let Err(error) = kill_process_group(child_group, Signal::KILL) {
        tracing::debug!("source command process-group kill failed: {error}");
    }
}

fn terminate_child_after_error(child: &mut std::process::Child) -> bool {
    match child.kill() {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => false,
        Err(error) => {
            tracing::debug!("source command child kill failed: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct FixedClock {
        value: Option<String>,
    }

    impl FixedClock {
        fn new(value: &str) -> Self {
            Self {
                value: Some(value.to_owned()),
            }
        }
    }

    impl SourceRunClock for FixedClock {
        fn now_rfc3339(&self) -> Option<String> {
            self.value.clone()
        }
    }

    #[derive(Clone, Debug)]
    struct FakeExecutor {
        execution: SourceRunExecution,
    }

    impl SourceRunExecutor for FakeExecutor {
        fn execute(&self, _request: &SourceRunRequest) -> SourceRunExecution {
            self.execution.clone()
        }
    }

    fn request() -> SourceRunRequest {
        SourceRunRequest::new(
            SourceRunSource::new(SourceRunKind::AgentMail, "agent-mail", "health_check"),
            SourceRunCommand::new("agent-mail")
                .with_args(["health_check"])
                .with_display("agent-mail health_check"),
            Duration::from_millis(5_000),
        )
        .with_tail_bytes_max(8)
    }

    fn evidence_for(execution: SourceRunExecution) -> SourceRunEvidence {
        run_source_command_with(
            &request(),
            &FakeExecutor { execution },
            &FixedClock::new("2026-05-24T05:04:00Z"),
        )
    }

    #[test]
    fn clean_exit_produces_passed_evidence() {
        let evidence = evidence_for(SourceRunExecution::Completed {
            exit_code: Some(0),
            signal: None,
            stdout: SourceRunPipeCapture::from_bytes(b"{\"ok\":true}", 8),
            stderr: SourceRunPipeCapture::empty(),
            elapsed: Duration::from_millis(42),
        });

        assert_eq!(evidence.status, SourceRunStatus::Passed);
        assert_eq!(evidence.exit.exit_code, Some(0));
        assert_eq!(evidence.output.stdout_tail.as_deref(), Some("k\":true}"));
        assert!(evidence.output.stdout_hash.is_some());
        assert!(evidence.degraded.is_empty());
        assert!(evidence.recovery.is_empty());
    }

    #[test]
    fn zero_timeout_request_floors_to_schema_valid_timeout_ms_bd_29xk0() {
        // bd-29xk0: a zero/sub-ms timeout must never serialize
        // `timing.timeoutMs == 0`, which violates ee.source_run_evidence.v1's
        // `minimum: 1`. Construction floors to 1ms; serialization clamps too.
        let zero_request = SourceRunRequest::new(
            SourceRunSource::new(SourceRunKind::Shell, "shell", "zero_timeout"),
            SourceRunCommand::new("true"),
            Duration::ZERO,
        );
        assert_eq!(
            zero_request.timeout,
            Duration::from_millis(1),
            "construction must floor a zero timeout to 1ms"
        );

        let evidence = run_source_command_with(
            &zero_request,
            &FakeExecutor {
                execution: SourceRunExecution::Completed {
                    exit_code: Some(0),
                    signal: None,
                    stdout: SourceRunPipeCapture::empty(),
                    stderr: SourceRunPipeCapture::empty(),
                    elapsed: Duration::from_millis(1),
                },
            },
            &FixedClock::new("2026-05-24T05:04:00Z"),
        );
        assert!(
            evidence.timing.timeout_ms >= 1,
            "evidence timeout_ms must satisfy the v1 schema minimum, got {}",
            evidence.timing.timeout_ms
        );
    }

    #[test]
    fn nonzero_exit_is_failed_but_keeps_stderr_diagnostics() {
        let evidence = evidence_for(SourceRunExecution::Completed {
            exit_code: Some(2),
            signal: None,
            stdout: SourceRunPipeCapture::empty(),
            stderr: SourceRunPipeCapture::from_bytes(b"permission denied", 8),
            elapsed: Duration::from_millis(7),
        });

        assert_eq!(evidence.status, SourceRunStatus::Failed);
        assert_eq!(evidence.exit.exit_code, Some(2));
        assert_eq!(evidence.output.stderr_tail.as_deref(), Some("n denied"));
        assert_eq!(evidence.degraded[0].code, "source_run_failed");
        assert_eq!(
            evidence.recovery[0].kind,
            SourceRunRecoveryKind::UseStaticFallback
        );
    }

    #[test]
    fn spawn_failure_is_recorded_as_stderr_tail() {
        let evidence = evidence_for(SourceRunExecution::SpawnFailed {
            error: "binary missing".to_owned(),
            elapsed: Duration::from_millis(1),
        });

        assert_eq!(evidence.status, SourceRunStatus::SpawnFailed);
        assert_eq!(evidence.exit.exit_code, None);
        assert_eq!(evidence.output.stderr_tail.as_deref(), Some(" missing"));
    }

    #[test]
    fn stdout_and_stderr_are_truncated_to_tail_limit() {
        let evidence = evidence_for(SourceRunExecution::Completed {
            exit_code: Some(1),
            signal: None,
            stdout: SourceRunPipeCapture::from_bytes(b"0123456789abcdef", 8),
            stderr: SourceRunPipeCapture::from_bytes(b"abcdefghijklmnop", 8),
            elapsed: Duration::from_millis(1),
        });

        assert_eq!(evidence.output.stdout_bytes, 16);
        assert_eq!(evidence.output.stderr_bytes, 16);
        assert_eq!(evidence.output.stdout_tail.as_deref(), Some("89abcdef"));
        assert_eq!(evidence.output.stderr_tail.as_deref(), Some("ijklmnop"));
        assert!(evidence.output.stderr_excerpt_hash.is_some());
    }

    #[test]
    fn timeout_marks_only_own_child_terminated() {
        let evidence = evidence_for(SourceRunExecution::TimedOut {
            exit_code: None,
            signal: None,
            stdout: SourceRunPipeCapture::empty(),
            stderr: SourceRunPipeCapture::from_bytes(b"still running", 8),
            elapsed: Duration::from_millis(5_001),
            killed_own_child: true,
        });

        assert_eq!(evidence.status, SourceRunStatus::TimedOut);
        assert!(evidence.exit.killed_own_child);
        assert!(!evidence.exit.killed_peer_processes);
        assert_eq!(evidence.degraded[0].code, "source_run_timeout");
        assert_eq!(
            evidence.recovery[0].repair_safety.risk_class,
            "unavailable_or_manual_only"
        );
        assert_eq!(
            evidence.recovery[0].repair_safety.next_action,
            "manual_only"
        );
        assert_eq!(
            evidence.recovery[0].repair_safety.reason_code,
            "source_run_retry_timeout_manual_only"
        );
    }

    #[test]
    fn fail_closed_timeout_uses_high_severity() {
        let mut request = request();
        request.policy = SourceRunPolicy::fail_closed(
            SourceRunRequiredMode::RemoteVerificationRequired,
            "remote verification evidence is mandatory",
        );
        let evidence = run_source_command_with(
            &request,
            &FakeExecutor {
                execution: SourceRunExecution::TimedOut {
                    exit_code: None,
                    signal: None,
                    stdout: SourceRunPipeCapture::empty(),
                    stderr: SourceRunPipeCapture::empty(),
                    elapsed: Duration::from_millis(5_001),
                    killed_own_child: true,
                },
            },
            &FixedClock::new("2026-05-24T05:04:00Z"),
        );

        assert_eq!(evidence.status, SourceRunStatus::TimedOut);
        assert_eq!(evidence.degraded[0].severity, SourceRunSeverity::High);
    }

    #[test]
    fn serialized_recovery_actions_include_repair_safety_contract() -> Result<(), String> {
        let evidence = evidence_for(SourceRunExecution::Completed {
            exit_code: Some(2),
            signal: None,
            stdout: SourceRunPipeCapture::empty(),
            stderr: SourceRunPipeCapture::from_bytes(b"coordination source failed", 128),
            elapsed: Duration::from_millis(7),
        });
        let serialized = serde_json::to_value(&evidence)
            .map_err(|error| format!("serialize evidence: {error}"))?;
        let repair_safety = serialized
            .pointer("/recovery/0/repairSafety")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "source-run recovery action must serialize repairSafety".to_owned())?;

        assert_eq!(
            repair_safety
                .get("riskClass")
                .and_then(serde_json::Value::as_str),
            Some("unavailable_or_manual_only")
        );
        assert_eq!(
            repair_safety
                .get("nextAction")
                .and_then(serde_json::Value::as_str),
            Some("manual_only")
        );
        assert_eq!(
            repair_safety
                .get("source")
                .and_then(serde_json::Value::as_str),
            Some("repair_action_safety")
        );
        assert_eq!(
            repair_safety
                .get("reasonCode")
                .and_then(serde_json::Value::as_str),
            Some("source_run_static_fallback_manual_only")
        );
        Ok(())
    }

    #[test]
    fn unsafe_argv_and_env_material_are_not_serialized() -> Result<(), String> {
        let mut request = request();
        request.command = SourceRunCommand::new("agent-mail")
            .with_args(["--token", "SECRET_TOKEN=super-secret-value"])
            .with_env("SECRET_TOKEN", "super-secret-value")
            .with_display("agent-mail --token SECRET_TOKEN=super-secret-value")
            .with_argv_redaction(SourceRunArgvRedaction::HashOnly);
        let evidence = run_source_command_with(
            &request,
            &FakeExecutor {
                execution: SourceRunExecution::Completed {
                    exit_code: Some(0),
                    signal: None,
                    stdout: SourceRunPipeCapture::empty(),
                    stderr: SourceRunPipeCapture::empty(),
                    elapsed: Duration::from_millis(1),
                },
            },
            &FixedClock::new("2026-05-24T05:04:00Z"),
        );
        let serialized = serde_json::to_string(&evidence)
            .map_err(|error| format!("serialize evidence: {error}"))?;

        assert_eq!(
            evidence.command.argv_redaction,
            SourceRunArgvRedaction::HashOnly
        );
        assert!(evidence.command.display.is_none());
        assert!(!serialized.contains("SECRET_TOKEN"));
        assert!(!serialized.contains("super-secret-value"));
        Ok(())
    }

    #[test]
    fn output_tails_are_secret_redacted() {
        let mut request = request();
        request.tail_bytes_max = 128;
        let evidence = run_source_command_with(
            &request,
            &FakeExecutor {
                execution: SourceRunExecution::Completed {
                    exit_code: Some(1),
                    signal: None,
                    stdout: SourceRunPipeCapture::empty(),
                    stderr: SourceRunPipeCapture::from_bytes(
                        b"api_key=abcdefghijklmnopqrstuvwxyz0123456789",
                        128,
                    ),
                    elapsed: Duration::from_millis(1),
                },
            },
            &FixedClock::new("2026-05-24T05:04:00Z"),
        );

        let stderr_tail = evidence.output.stderr_tail.as_deref().unwrap_or_default();
        assert!(evidence.output.redacted);
        assert!(!stderr_tail.contains("abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn fixed_clock_makes_identity_and_timestamps_deterministic() {
        let execution = SourceRunExecution::Completed {
            exit_code: Some(0),
            signal: None,
            stdout: SourceRunPipeCapture::from_bytes(b"ok", 8),
            stderr: SourceRunPipeCapture::empty(),
            elapsed: Duration::from_millis(1),
        };
        let left = evidence_for(execution.clone());
        let right = evidence_for(execution);

        assert_eq!(left.run_id, right.run_id);
        assert_eq!(left.captured_at, Some("2026-05-24T05:04:00Z".to_owned()));
        assert_eq!(left.timing.started_at, right.timing.started_at);
        assert_eq!(left.provenance_hash, right.provenance_hash);
    }

    #[test]
    fn timeout_pipe_drain_does_not_join_unfinished_reader_threads() {
        let started = Instant::now();
        let mut stdout_thread = Some(thread::spawn(|| {
            thread::sleep(TIMEOUT_PIPE_DRAIN_GRACE * 4);
            Ok(SourceRunPipeCapture::from_bytes(b"late stdout", 64))
        }));
        let mut stderr_thread = Some(thread::spawn(|| {
            Ok(SourceRunPipeCapture::from_bytes(b"fast stderr", 64))
        }));

        let (stdout, stderr) =
            drain_capture_readers_after_timeout(&mut stdout_thread, &mut stderr_thread, 64);

        assert!(
            started.elapsed() < TIMEOUT_PIPE_DRAIN_GRACE * 3,
            "timeout drain must not block on unfinished pipe readers"
        );
        let unavailable = b"source command pipe drain timed out; output tail unavailable";
        assert_eq!(stdout.total_bytes, unavailable.len());
        assert_eq!(stdout.tail.as_slice(), unavailable);
        assert_eq!(stderr.tail.as_slice(), b"fast stderr");
    }

    #[test]
    fn pipe_reader_error_fallback_honors_tail_limit() {
        let mut reader_thread = Some(thread::spawn(|| {
            Err(io::Error::other("very long pipe reader failure diagnostic"))
        }));

        let capture = join_capture_reader(&mut reader_thread, 8);

        assert_eq!(capture.tail.len(), 8);
        assert_eq!(capture.tail.as_slice(), b"agnostic");
    }

    #[test]
    fn appended_wait_error_honors_tail_limit() {
        let mut capture = SourceRunPipeCapture::from_bytes(b"prior stderr", 8);

        append_capture_error(
            &mut capture,
            "source command wait failed: very long wait diagnostic",
            10,
        );

        assert_eq!(capture.tail.len(), 10);
        assert_eq!(capture.tail.as_slice(), b"diagnostic");
    }

    #[test]
    fn system_runner_timeout_marks_child_termination() {
        let request = SourceRunRequest::new(
            SourceRunSource::new(SourceRunKind::Shell, "shell", "sleep"),
            SourceRunCommand::new("sh").with_args(["-c", "sleep 2"]),
            Duration::from_millis(20),
        )
        .with_tail_bytes_max(64);

        let evidence = run_source_command_with(
            &request,
            &SystemSourceRunExecutor,
            &FixedClock::new("2026-05-24T05:04:00Z"),
        );

        assert_eq!(evidence.status, SourceRunStatus::TimedOut);
        assert!(evidence.exit.killed_own_child);
        assert!(!evidence.exit.killed_peer_processes);
    }

    #[test]
    fn system_runner_times_out_when_descendant_keeps_pipe_open() {
        let request = SourceRunRequest::new(
            SourceRunSource::new(SourceRunKind::Shell, "shell", "inherited_pipe"),
            SourceRunCommand::new("sh").with_args(["-c", "(sleep 2) & printf 'ready\\n'; exit 0"]),
            Duration::from_millis(50),
        )
        .with_tail_bytes_max(64);
        let started = Instant::now();

        let evidence = run_source_command_with(
            &request,
            &SystemSourceRunExecutor,
            &FixedClock::new("2026-05-24T05:04:00Z"),
        );

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "source runner must not block on inherited pipe handles after parent exit"
        );
        assert_eq!(evidence.status, SourceRunStatus::TimedOut);
        assert_eq!(evidence.exit.exit_code, Some(0));
        assert!(!evidence.exit.killed_own_child);
        assert!(
            evidence
                .output
                .stdout_tail
                .as_deref()
                .is_some_and(|tail| tail.contains("ready"))
        );
    }
}
