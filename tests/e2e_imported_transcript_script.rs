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
//! including cargo's own test-binary path variable, so the binary under test is
//! the one cargo built from this tree rather than whatever is on PATH. (That
//! variable is not spelled out here: `scripts/check-ee-spawn-isolation.py`
//! counts every occurrence of it and attributes each to an enclosing function,
//! and a mention in module prose has no function to isolate. The gate's own
//! source dodges the same self-match by splitting the literal.)
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

    // ISOLATE THE USER-GLOBAL STORE. `ee` resolves its data dir from
    // `XDG_DATA_HOME`, else `HOME/.local/share`, so a spawn that inherits the
    // developer's real HOME can read and write the user-global memory store of
    // whoever runs the suite. `tests/support/isolated_ee.rs` exists for this,
    // but `isolated_ee_command` builds a `Command` for the ee BINARY and this
    // harness spawns `bash`; the env below is that helper's isolation surface
    // applied to the shell instead, which the script and every `ee` it invokes
    // then inherit. Caught by scripts/check-ee-spawn-isolation.py, which counts
    // a spawning fn as un-isolated unless it sets HOME or XDG_DATA_HOME.
    let home = scratch.join("home");
    let xdg_data = scratch.join("xdg-data");
    let xdg_config = scratch.join("xdg-config");
    let xdg_cache = scratch.join("xdg-cache");
    let xdg_state = scratch.join("xdg-state");
    for dir in [&home, &xdg_data, &xdg_config, &xdg_cache, &xdg_state] {
        fs::create_dir_all(dir).unwrap_or_else(|error| {
            panic!("create isolated dir {}: {error}", dir.display());
        });
    }

    let script = repo.join("scripts/e2e_imported_transcript_retrieval.sh");
    let started = Instant::now();
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(&repo)
        // The script accepts either spelling and resolves one to the other.
        .env("EE_BIN", &ee_bin)
        .env("EE_BINARY", &ee_bin)
        .env("EE_E2E_TMPDIR", &scratch)
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("XDG_STATE_HOME", &xdg_state)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("NO_COLOR", "1")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
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
