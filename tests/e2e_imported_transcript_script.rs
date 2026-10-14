#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code may unwrap/expect
//! Invoke `scripts/e2e_imported_transcript_retrieval.sh` from the test suite.
//!
//! WHY THIS FILE EXISTS. The script is bd-16imy's named acceptance artifact and
//! it passed its first real run (14 assertions, 0 failures, 2026-10-14). It was
//! also invoked by NOTHING, which `scripts/e2e_invocation_audit.sh` reported as
//! the single NEW orphan and which reddened the "E2E Invocation Audit" stage of
//! `scripts/verify.sh`. A script nothing runs makes a true claim about the past
//! and guarantees nothing about the present.
//!
//! WHY A RUST HARNESS RATHER THAN A verify.sh STAGE. Both honour the audit; only
//! one is cheap. A new `run_stage` needs a matching `[[stage]]` key in
//! `scripts/verify-budget.toml` plus an update to `total_expected_seconds`,
//! which `tests/verification_drift_guard.rs` asserts — three coordinated edits
//! to add one invocation. Riding the existing test target adds no stage name and
//! no budget key. `tests/e2e_ask_script.rs` is the precedent this file copies,
//! including `CARGO_BIN_EXE_ee` so cargo supplies a binary built from the tree
//! under test rather than whatever is on PATH.
//!
//! WHAT IT ASSERTS, AND WHY NOT JUST THE EXIT CODE. The script's own exit status
//! is the primary gate, but an exit code alone cannot distinguish "every
//! assertion passed" from "the harness exited early before asserting anything"
//! — the empty-run failure mode. So this also parses the `ee.test_event.v1`
//! assert_result events off stdout and requires a FLOOR on the pass count with
//! zero failures. The floor is deliberately below the 14 observed, so adding an
//! assertion to the script does not red this test, while deleting most of them
//! does.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// A scratch base the script can `mktemp -d` inside. Retained rather than
/// cleaned: on a failure the workspace it built is the only evidence of why.
fn retained_scratch_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "ee-imported-transcript-e2e-script-{}-{nanos}",
        std::process::id()
    ))
}

/// Count `assert_result` events by status off the script's stdout.
///
/// Substring matching on the emitted JSON rather than a parse: the script
/// prints one compact `ee.test_event.v1` object per line and these two keys are
/// spelled exactly this way in it, so a parse buys nothing and would fail the
/// test for a formatting change rather than a behavioural one.
fn assertion_tally(stdout: &str) -> (usize, usize) {
    let mut passed = 0usize;
    let mut failed = 0usize;
    for line in stdout.lines() {
        if !line.contains("\"kind\":\"assert_result\"") {
            continue;
        }
        if line.contains("\"status\":\"pass\"") {
            passed = passed.saturating_add(1);
        } else if line.contains("\"status\":\"fail\"") {
            failed = failed.saturating_add(1);
        }
    }
    (passed, failed)
}

/// The script asserted this many times on 2026-10-14. The floor below is lower
/// on purpose; see the module comment.
const OBSERVED_ASSERTION_COUNT: usize = 14;
const MINIMUM_ASSERTION_COUNT: usize = 10;

#[test]
fn imported_transcript_e2e_script_exercises_public_cli() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ee_bin = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
    let scratch = retained_scratch_dir();
    fs::create_dir_all(&scratch).expect("create imported-transcript e2e scratch base");

    let script = repo.join("scripts/e2e_imported_transcript_retrieval.sh");
    let started = Instant::now();
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(&repo)
        // The script accepts either spelling and resolves one to the other.
        .env("EE_BIN", &ee_bin)
        .env("EE_BINARY", &ee_bin)
        .env("EE_E2E_TMPDIR", &scratch)
        .output()
        .expect("run scripts/e2e_imported_transcript_retrieval.sh");
    let elapsed = started.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let (passed, failed) = assertion_tally(&stdout);

    assert!(
        output.status.success(),
        "scripts/e2e_imported_transcript_retrieval.sh failed with status {:?} \
         after {:?} ({passed} passed, {failed} failed)\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code(),
        elapsed
    );

    // A zero-assertion run exits 0 if the harness returns before asserting.
    // Without this floor, that silent no-op would read as a pass.
    assert!(
        passed >= MINIMUM_ASSERTION_COUNT,
        "expected at least {MINIMUM_ASSERTION_COUNT} assert_result passes \
         (observed {OBSERVED_ASSERTION_COUNT} on 2026-10-14), saw {passed} with \
         {failed} failures; a run that asserts nothing exits 0\nstdout:\n{stdout}"
    );
    assert_eq!(
        failed, 0,
        "script reported {failed} failing assertion(s)\nstdout:\n{stdout}"
    );
}
