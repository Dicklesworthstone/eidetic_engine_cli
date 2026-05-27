//! bd-12v87.5 — coord-watchdog fixture harness.
//!
//! Exercises `core::source_run::run_source_command` against the
//! deterministic fixture scripts under `tests/fixtures/coord_watchdog/`
//! that simulate the five classes of external-tool behavior the
//! source-run runner must contain without stalling the agent workflow:
//!
//!   1. CLEAN — valid JSONL on stdout, exit 0 → status=Passed, no
//!      degraded entries.
//!   2. MALFORMED_JSONL — broken JSONL on stdout, exit 0 → runner
//!      itself reports Passed (it does not parse JSON); the malformed
//!      tail is observable through `evidence.output.stdout_tail` so a
//!      downstream consumer can detect the parse-fail shape.
//!   3. HANGS_AFTER_PARTIAL — emits one record then sleeps. Runner
//!      must terminate the child within the configured timeout and
//!      report status=TimedOut with `killed_own_child=true` and
//!      `killed_peer_processes=false`, retaining the partial stdout
//!      tail.
//!   4. AGENT_MAIL_CORRUPT — emits the canonical "malformed disk
//!      image" SQLite error to stderr and exits non-zero. Runner must
//!      preserve the exit code, surface the stderr tail, and report
//!      status=Failed.
//!   5. RCH_TOPOLOGY_REFUSED — emits the canonical RCH-E327
//!      path-dependency refusal to stderr and exits 1. Runner must
//!      preserve the exit code + stderr tail so callers can detect the
//!      topology blocker without retrying.
//!
//! The fixtures + this harness are the deterministic test backbone for
//! the broader bd-12v87 epic. They do NOT depend on bd-12v87.3's
//! integration into swarm-brief / doctor / work-packet; they drive the
//! runner directly so a regression in the runner itself fails
//! immediately, independent of integration progress.
//!
//! All cases assert two safety invariants the bead's scope highlights:
//!   - `evidence.exit.killed_peer_processes == false` (the runner must
//!     never kill peer processes discovered by pgrep/ps);
//!   - `evidence.timing.elapsed_ms` bounded by `timeout_ms` (the
//!     TimedOut case may exceed timeout by the pipe-drain grace, but
//!     never by more than a reasonable margin — pinned via a generous
//!     upper bound rather than a tight equality so the test is not
//!     flaky on slow CI).
//!
//! All cases run under `#[cfg(unix)]` because the fixtures use POSIX
//! shell scripts; on Windows the tests are no-ops at compile time.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(unix)]
mod unix_fixture_harness {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use ee::core::source_run::{
        SOURCE_RUN_EVIDENCE_SCHEMA_V1, SourceRunCommand, SourceRunEvidence, SourceRunKind,
        SourceRunRequest, SourceRunSource, SourceRunStatus, run_source_command,
    };

    type TestResult = Result<(), String>;

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("coord_watchdog")
            .join(name)
    }

    /// Build a SourceRunRequest pointing at the given fixture script.
    /// `timeout` lets the hangs-case use a tight 1s budget while the
    /// clean/malformed/error cases use a generous budget that should
    /// never trip — separating "I timed out because the fixture asked
    /// me to" from "I timed out because the harness is slow."
    fn fixture_request(
        kind: SourceRunKind,
        operation: &str,
        fixture_name: &str,
        timeout: Duration,
    ) -> SourceRunRequest {
        let path = fixture_path(fixture_name);
        let command = SourceRunCommand::new(path.to_string_lossy().into_owned())
            .with_display(format!("coord_watchdog_fixture:{fixture_name}"));
        SourceRunRequest::new(
            SourceRunSource::new(kind, fixture_name, operation),
            command,
            timeout,
        )
    }

    /// Invariants every fixture run must satisfy regardless of the
    /// scenario: the evidence must serialize back to JSON, the schema
    /// id must be the canonical v1 string, the runner must not have
    /// killed peer processes, and the recorded elapsed time must not
    /// exceed `2 * timeout` (the timeout itself + pipe-drain grace +
    /// scheduler jitter; tighter bounds are flaky on shared CI).
    fn assert_universal_safety(
        evidence: &SourceRunEvidence,
        scenario: &str,
        timeout: Duration,
    ) -> TestResult {
        if evidence.schema != SOURCE_RUN_EVIDENCE_SCHEMA_V1 {
            return Err(format!(
                "{scenario}: schema must be {SOURCE_RUN_EVIDENCE_SCHEMA_V1}; got {}",
                evidence.schema
            ));
        }
        if evidence.exit.killed_peer_processes {
            return Err(format!(
                "{scenario}: runner reported killed_peer_processes=true; the runner must \
                 never reach beyond its own child process tree"
            ));
        }
        if let Some(elapsed_ms) = evidence.timing.elapsed_ms {
            let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
            // 2 * timeout + 5s absolute floor; the absolute floor
            // bounds very-short timeouts (e.g. 100ms) where scheduler
            // jitter can dominate the 2x multiplier.
            let upper_bound = timeout_ms.saturating_mul(2).max(5_000);
            if elapsed_ms > upper_bound {
                return Err(format!(
                    "{scenario}: elapsed_ms {elapsed_ms} exceeds upper bound {upper_bound} \
                     (timeout was {timeout_ms} ms)"
                ));
            }
        }
        // Serialization must round-trip cleanly. If the evidence
        // contains a value that serde refuses (e.g. a NaN slipping
        // into a Serialize float), this catches it inside the test
        // surface rather than in production logs.
        serde_json::to_string(evidence)
            .map_err(|error| format!("{scenario}: evidence must serialize: {error}"))?;
        Ok(())
    }

    #[test]
    fn clean_fixture_is_passed_with_no_degradation() -> TestResult {
        let timeout = Duration::from_secs(10);
        let request = fixture_request(SourceRunKind::Beads, "list", "clean.sh", timeout);
        let evidence = run_source_command(&request);

        assert_universal_safety(&evidence, "clean", timeout)?;

        if evidence.status != SourceRunStatus::Passed {
            return Err(format!(
                "clean fixture must be Passed; got {:?}; stderr_tail={:?}",
                evidence.status, evidence.output.stderr_tail
            ));
        }
        if evidence.exit.exit_code != Some(0) {
            return Err(format!(
                "clean fixture must exit 0; got {:?}",
                evidence.exit.exit_code
            ));
        }
        if !evidence.degraded.is_empty() {
            return Err(format!(
                "clean fixture must have no degraded entries; got {:?}",
                evidence.degraded
            ));
        }
        let stdout_tail = evidence.output.stdout_tail.as_deref().unwrap_or_default();
        if !stdout_tail.contains("\"scenario\":\"clean\"") {
            return Err(format!(
                "clean fixture stdout_tail must include the canonical scenario marker; got {stdout_tail:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn malformed_jsonl_fixture_preserves_partial_tail_for_downstream_classification() -> TestResult
    {
        let timeout = Duration::from_secs(10);
        let request = fixture_request(SourceRunKind::Beads, "list", "malformed_jsonl.sh", timeout);
        let evidence = run_source_command(&request);

        assert_universal_safety(&evidence, "malformed_jsonl", timeout)?;

        // The runner is JSON-agnostic: malformed stdout that the
        // child still exits 0 on is reported as Passed. The contract
        // here is that the malformed body is OBSERVABLE so a
        // downstream consumer (work-packet, swarm-brief) can detect
        // the parse-fail and emit its own degraded code.
        if evidence.status != SourceRunStatus::Passed {
            return Err(format!(
                "malformed_jsonl runner status must be Passed (the child exited 0); got {:?}",
                evidence.status
            ));
        }
        let stdout_tail = evidence.output.stdout_tail.as_deref().unwrap_or_default();
        // The fixture omits the closing brace deliberately. If a future
        // refactor of the fixture or the runner introduces a brace
        // somewhere, this assertion would fail and force the test
        // owner to update either the fixture or the contract.
        if !stdout_tail.contains("\"scenario\":\"malformed_jsonl\"") {
            return Err(format!(
                "malformed_jsonl stdout_tail must include the scenario marker; got {stdout_tail:?}"
            ));
        }
        if stdout_tail.contains("}\n") {
            return Err(format!(
                "malformed_jsonl stdout_tail must NOT contain a complete JSON object \
                 (the fixture omits the closing brace); got {stdout_tail:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn hangs_after_partial_fixture_is_timed_out_with_partial_tail() -> TestResult {
        // Tight timeout so the test is fast. 750ms is well above
        // process-start cost on Mac and Linux yet well below the
        // fixture's 3600s sleep, so the runner has to enforce the
        // timeout for this test to pass at all.
        let timeout = Duration::from_millis(750);
        let request = fixture_request(
            SourceRunKind::SwarmCollector,
            "list",
            "hangs_after_partial.sh",
            timeout,
        );
        let evidence = run_source_command(&request);

        assert_universal_safety(&evidence, "hangs_after_partial", timeout)?;

        if evidence.status != SourceRunStatus::TimedOut {
            return Err(format!(
                "hangs_after_partial must be TimedOut; got {:?}; stderr_tail={:?}; \
                 stdout_tail={:?}",
                evidence.status, evidence.output.stderr_tail, evidence.output.stdout_tail
            ));
        }
        if !evidence.exit.killed_own_child {
            return Err(format!(
                "hangs_after_partial must report killed_own_child=true on timeout; \
                 got exit={:?}",
                evidence.exit
            ));
        }
        // Partial-tail visibility: the fixture wrote one JSONL line
        // BEFORE sleeping. The runner must retain that line in
        // stdout_tail so the operator can see the partial evidence
        // even though the child never finished.
        let stdout_tail = evidence.output.stdout_tail.as_deref().unwrap_or_default();
        if !stdout_tail.contains("\"scenario\":\"hangs_after_partial\"") {
            return Err(format!(
                "hangs_after_partial must retain the pre-sleep partial stdout; got {stdout_tail:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn agent_mail_corrupt_fixture_is_failed_with_stderr_tail() -> TestResult {
        let timeout = Duration::from_secs(10);
        let request = fixture_request(
            SourceRunKind::AgentMail,
            "health_check",
            "agent_mail_corrupt.sh",
            timeout,
        );
        let evidence = run_source_command(&request);

        assert_universal_safety(&evidence, "agent_mail_corrupt", timeout)?;

        if evidence.status != SourceRunStatus::Failed {
            return Err(format!(
                "agent_mail_corrupt must be Failed; got {:?}",
                evidence.status
            ));
        }
        if evidence.exit.exit_code != Some(21) {
            return Err(format!(
                "agent_mail_corrupt must preserve fixture exit code 21; got {:?}",
                evidence.exit.exit_code
            ));
        }
        let stderr_tail = evidence.output.stderr_tail.as_deref().unwrap_or_default();
        if !stderr_tail.contains("malformed disk image") {
            return Err(format!(
                "agent_mail_corrupt stderr_tail must include the SQLite corruption marker; \
                 got {stderr_tail:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn rch_topology_refused_fixture_is_failed_with_e327_marker() -> TestResult {
        let timeout = Duration::from_secs(10);
        let request = fixture_request(
            SourceRunKind::Rch,
            "verify",
            "rch_topology_refused.sh",
            timeout,
        );
        let evidence = run_source_command(&request);

        assert_universal_safety(&evidence, "rch_topology_refused", timeout)?;

        if evidence.status != SourceRunStatus::Failed {
            return Err(format!(
                "rch_topology_refused must be Failed; got {:?}",
                evidence.status
            ));
        }
        if evidence.exit.exit_code != Some(1) {
            return Err(format!(
                "rch_topology_refused must preserve fixture exit code 1; got {:?}",
                evidence.exit.exit_code
            ));
        }
        let stderr_tail = evidence.output.stderr_tail.as_deref().unwrap_or_default();
        if !stderr_tail.contains("RCH-E327") {
            return Err(format!(
                "rch_topology_refused stderr_tail must include the RCH-E327 code; \
                 got {stderr_tail:?}"
            ));
        }
        Ok(())
    }
}
