//! Coord-watchdog hung-source integration coverage (bd-12v87.5).
//!
//! Exercises the real `SystemSourceRunExecutor` against subprocess scenarios
//! that mirror the bd-12v87.5 acceptance fixtures: a command that emits
//! partial stdout and then hangs past its budget, and a command that fails
//! cleanly with diagnostic stderr. The tests assert the watchdog kills its
//! own child, that elapsed time stays bounded by the configured timeout,
//! that the partial output tail survives the kill, and that stable degraded
//! codes plus recovery actions reach evidence.
//!
//! The test logs one structured `ee.test_event.v1` line per scenario with
//! command hash, source kind, timeout budget, elapsed ms, evidence hash,
//! and artifact paths. Raw stdout/stderr bodies are deliberately omitted;
//! only bounded tails and hashes leave the test surface.

use std::path::PathBuf;
use std::time::Duration;

use ee::core::source_run::{
    SOURCE_RUN_EVIDENCE_SCHEMA_V1, SourceRunCommand, SourceRunEvidence, SourceRunKind,
    SourceRunRecoveryKind, SourceRunRequest, SourceRunSource, SourceRunStatus, run_source_command,
};
use serde::Serialize;

const HUNG_SOURCE_TIMEOUT: Duration = Duration::from_millis(150);
const HUNG_SOURCE_TIMEOUT_TOLERANCE: Duration = Duration::from_millis(750);
const HUNG_SOURCE_TAIL_BYTES_MAX: usize = 64;
const PARTIAL_STDOUT_MARKER: &str = "PARTIAL_STDOUT_MARKER";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestEventV1<'a> {
    schema: &'static str,
    bead: &'static str,
    scenario: &'a str,
    source_kind: &'a str,
    timeout_ms: u64,
    elapsed_ms: u64,
    elapsed_within_tolerance: bool,
    status: &'a str,
    command_hash: &'a str,
    evidence_hash: &'a str,
    degraded_codes: Vec<&'a str>,
    recovery_kinds: Vec<&'a str>,
    artifact_references: Vec<String>,
}

fn emit_test_event(scenario: &str, evidence: &SourceRunEvidence, within_tolerance: bool) {
    let event = TestEventV1 {
        schema: "ee.test_event.v1",
        bead: "bd-12v87.5",
        scenario,
        source_kind: evidence.source.kind.as_str(),
        timeout_ms: evidence.timing.timeout_ms,
        elapsed_ms: evidence.timing.elapsed_ms.unwrap_or(0),
        elapsed_within_tolerance: within_tolerance,
        status: evidence.status.as_str(),
        command_hash: &evidence.command.command_hash,
        evidence_hash: &evidence.provenance_hash,
        degraded_codes: evidence.degraded.iter().map(|d| d.code.as_str()).collect(),
        recovery_kinds: evidence
            .recovery
            .iter()
            .map(|r| source_recovery_label(r.kind))
            .collect(),
        artifact_references: evidence
            .artifacts
            .iter()
            .map(|a| a.reference.clone())
            .collect(),
    };
    eprintln!(
        "{}",
        serde_json::to_string(&event).expect("test event must serialize")
    );
}

fn source_recovery_label(kind: SourceRunRecoveryKind) -> &'static str {
    match kind {
        SourceRunRecoveryKind::Retry => "retry",
        SourceRunRecoveryKind::RetryWithLongerTimeout => "retry_with_longer_timeout",
        SourceRunRecoveryKind::UseStaticFallback => "use_static_fallback",
        SourceRunRecoveryKind::RepairSubstrateAfterApproval => "repair_substrate_after_approval",
        SourceRunRecoveryKind::ManualCoordination => "manual_coordination",
        SourceRunRecoveryKind::FailClosed => "fail_closed",
        SourceRunRecoveryKind::SkipSource => "skip_source",
    }
}

fn hung_after_partial_stdout_request() -> SourceRunRequest {
    SourceRunRequest::new(
        SourceRunSource::new(SourceRunKind::Shell, "shell", "hung_after_partial_stdout"),
        SourceRunCommand::new("sh")
            .with_args(["-c", &format!("printf '{PARTIAL_STDOUT_MARKER}'; sleep 30")]),
        HUNG_SOURCE_TIMEOUT,
    )
    .with_tail_bytes_max(HUNG_SOURCE_TAIL_BYTES_MAX)
}

#[test]
fn hung_source_after_partial_stdout_kills_child_within_timeout_budget() {
    let request = hung_after_partial_stdout_request();
    let started = std::time::Instant::now();
    let evidence = run_source_command(&request);
    let wall = started.elapsed();

    let within = wall <= HUNG_SOURCE_TIMEOUT + HUNG_SOURCE_TIMEOUT_TOLERANCE;
    emit_test_event("hung_after_partial_stdout", &evidence, within);

    assert_eq!(evidence.schema, SOURCE_RUN_EVIDENCE_SCHEMA_V1);
    assert_eq!(
        evidence.status,
        SourceRunStatus::TimedOut,
        "wedged source must mark TimedOut so swarm brief/doctor stays bounded"
    );
    assert!(
        evidence.exit.killed_own_child,
        "watchdog must kill its own child instead of leaving a zombie"
    );
    assert!(
        !evidence.exit.killed_peer_processes,
        "child kill must stay scoped to the source's own process group"
    );
    assert!(
        within,
        "wall-clock elapsed {wall:?} exceeded timeout {HUNG_SOURCE_TIMEOUT:?} + tolerance {HUNG_SOURCE_TIMEOUT_TOLERANCE:?}; watchdog did not bound the source"
    );
    assert_eq!(
        evidence.timing.timeout_ms,
        Some(HUNG_SOURCE_TIMEOUT.as_millis() as u64),
        "evidence timing must echo the configured timeout budget"
    );
    let elapsed_ms = evidence
        .timing
        .elapsed_ms
        .expect("elapsed_ms must be populated");
    assert!(
        elapsed_ms >= HUNG_SOURCE_TIMEOUT.as_millis() as u64,
        "elapsed_ms ({elapsed_ms}) should be at least the timeout budget for a wedged source"
    );

    let stdout_tail = evidence.output.stdout_tail.as_deref().unwrap_or_default();
    let drain_marker = "source command pipe drain timed out";
    assert!(
        stdout_tail.contains(PARTIAL_STDOUT_MARKER) || stdout_tail.contains(drain_marker),
        "stdout tail must preserve partial output or the drain-timeout sentinel; got {stdout_tail:?}"
    );
    assert!(
        evidence.output.stdout_hash.is_some(),
        "stdout_hash must be populated even for partial output"
    );

    assert!(
        !evidence.degraded.is_empty(),
        "TimedOut must surface at least one degraded entry"
    );
    assert_eq!(evidence.degraded[0].code, "source_run_timeout");

    assert!(
        !evidence.recovery.is_empty(),
        "TimedOut must surface at least one recovery action"
    );
    assert!(matches!(
        evidence.recovery[0].kind,
        SourceRunRecoveryKind::Retry | SourceRunRecoveryKind::UseStaticFallback
    ));

    let serialized = serde_json::to_string(&evidence).expect("evidence must serialize");
    assert!(
        !serialized.contains("PARTIAL_STDOUT_MARKER\":"),
        "raw structured stdout body must not appear as a serialized field name"
    );
}

#[test]
fn clean_failure_emits_stable_failed_evidence() {
    let request = SourceRunRequest::new(
        SourceRunSource::new(SourceRunKind::Shell, "shell", "permission_denied"),
        SourceRunCommand::new("sh").with_args(["-c", "echo permission denied >&2; exit 2"]),
        Duration::from_secs(5),
    )
    .with_tail_bytes_max(HUNG_SOURCE_TAIL_BYTES_MAX);
    let started = std::time::Instant::now();
    let evidence = run_source_command(&request);
    let wall = started.elapsed();
    let within = wall <= Duration::from_secs(5);
    emit_test_event("clean_failure_nonzero_exit", &evidence, within);

    assert_eq!(evidence.schema, SOURCE_RUN_EVIDENCE_SCHEMA_V1);
    assert_eq!(evidence.status, SourceRunStatus::Failed);
    assert_eq!(evidence.exit.exit_code, Some(2));
    assert!(!evidence.exit.killed_own_child);
    let stderr_tail = evidence.output.stderr_tail.as_deref().unwrap_or_default();
    assert!(
        stderr_tail.contains("permission denied"),
        "stderr tail must preserve diagnostic body; got {stderr_tail:?}"
    );
    assert!(!evidence.degraded.is_empty());
    assert_eq!(evidence.degraded[0].code, "source_run_failed");
    assert!(!evidence.recovery.is_empty());
}

#[test]
fn missing_binary_spawn_failure_does_not_panic_or_unblock_other_sources() {
    // Use an absolute path that is guaranteed not to resolve to a real binary
    // on developer or CI hosts so the test exercises the SpawnFailed path
    // rather than depending on PATH semantics.
    let missing = PathBuf::from("/nonexistent/ee-watchdog-missing-binary-bd-12v87-5");
    let request = SourceRunRequest::new(
        SourceRunSource::new(SourceRunKind::Shell, "shell", "missing_binary"),
        SourceRunCommand::new(missing.to_string_lossy().into_owned()),
        Duration::from_secs(2),
    )
    .with_tail_bytes_max(HUNG_SOURCE_TAIL_BYTES_MAX);
    let started = std::time::Instant::now();
    let evidence = run_source_command(&request);
    let within = started.elapsed() <= Duration::from_secs(2);
    emit_test_event("missing_binary_spawn_failure", &evidence, within);

    assert_eq!(evidence.status, SourceRunStatus::SpawnFailed);
    assert_eq!(evidence.exit.exit_code, None);
    assert!(!evidence.exit.killed_own_child);
    assert!(!evidence.degraded.is_empty());
    // The runner must still produce a valid evidence record; downstream
    // collectors can keep collecting sibling sources rather than stalling.
    assert_eq!(evidence.schema, SOURCE_RUN_EVIDENCE_SCHEMA_V1);
    assert!(!evidence.provenance_hash.is_empty());
}
