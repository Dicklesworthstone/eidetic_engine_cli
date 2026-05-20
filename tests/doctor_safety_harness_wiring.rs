//! bd-21joy: structural contract that wires scripts/run-safety-harness.sh
//! into scripts/verify.sh as Stage 8.5.
//!
//! Pins the bead's acceptance items without needing the bd-2oh15
//! per-FM fixture suite to exist yet: this contract proves the gate
//! and the wrapper are present and would invoke the documented sub-
//! harnesses (verify-undo, verify-idempotence, verify-crash-recovery,
//! verify-concurrency, verify-metamorphic) the moment they land under
//! scripts/. While bd-2oh15 fixtures are absent, the wrapper emits a
//! stable `safety_harness_fixtures_unavailable` degraded event and
//! exits 0 — that advisory posture is also covered here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

const HARNESS_PATH: &str = "scripts/run-safety-harness.sh";
const VERIFY_PATH: &str = "scripts/verify.sh";
const SUB_HARNESSES: &[&str] = &[
    "verify-undo.sh",
    "verify-idempotence.sh",
    "verify-crash-recovery.sh",
    "verify-concurrency.sh",
    "verify-metamorphic.sh",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read_repo_file(relative: &str) -> Result<String, String> {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

#[test]
fn run_safety_harness_script_exists_and_is_executable() -> TestResult {
    let path = repo_root().join(HARNESS_PATH);
    let metadata = fs::metadata(&path)
        .map_err(|e| format!("safety harness wrapper {} missing: {e}", path.display()))?;
    ensure(
        metadata.is_file(),
        "safety harness wrapper is not a regular file",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        ensure(
            mode & 0o111 != 0,
            format!("safety harness wrapper must be executable; mode={mode:o}"),
        )?;
    }
    Ok(())
}

#[test]
fn run_safety_harness_references_all_five_sub_harnesses() -> TestResult {
    let body = read_repo_file(HARNESS_PATH)?;
    for sub in SUB_HARNESSES {
        ensure(
            body.contains(sub),
            format!(
                "scripts/run-safety-harness.sh must reference sub-harness `{sub}` so the \
                 wrapper invokes it when bd-2oh15 fixtures land; current body does not."
            ),
        )?;
    }
    Ok(())
}

#[test]
fn run_safety_harness_emits_fixtures_unavailable_degraded_code() -> TestResult {
    let body = read_repo_file(HARNESS_PATH)?;
    ensure(
        body.contains("safety_harness_fixtures_unavailable"),
        "wrapper must emit `safety_harness_fixtures_unavailable` when \
         tests/doctor_fixtures/ is missing or empty (bd-2oh15 owns the suite).",
    )?;
    ensure(
        body.contains("safety_harness_sub_scripts_missing"),
        "wrapper must emit `safety_harness_sub_scripts_missing` when one or \
         more sub-harness scripts are absent (drift slices fill them in).",
    )?;
    ensure(
        body.contains("EE_SAFETY_HARNESS_STRICT"),
        "wrapper must honor EE_SAFETY_HARNESS_STRICT=1 to fail closed; \
         default posture is advisory while bd-2oh15 is in_progress.",
    )?;
    Ok(())
}

#[test]
fn verify_script_invokes_safety_harness_as_a_stage() -> TestResult {
    let body = read_repo_file(VERIFY_PATH)?;
    ensure(
        body.contains("./scripts/run-safety-harness.sh"),
        "scripts/verify.sh must invoke ./scripts/run-safety-harness.sh as a \
         run_stage call (bd-21joy acceptance item)",
    )?;
    ensure(
        body.contains("bd-21joy"),
        "scripts/verify.sh must reference bd-21joy near the safety harness \
         stage so reviewers can trace the wiring back to the bead",
    )?;
    // The stage call must appear in the gate sequence between Gate 8 and
    // the optional Gate 9 perf-bench block, matching the wrapper's
    // declared Stage 8.5 position.
    let stage_idx = body
        .find("./scripts/run-safety-harness.sh")
        .ok_or_else(|| "wrapper invocation missing".to_string())?;
    let perf_idx = body
        .find("Performance Benchmarks")
        .ok_or_else(|| "Gate 9 perf bench anchor missing".to_string())?;
    ensure(
        stage_idx < perf_idx,
        "safety harness stage must precede Gate 9 perf benchmark block",
    )?;
    Ok(())
}
