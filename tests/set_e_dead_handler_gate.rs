//! bd-w6849 — give `scripts/audit-set-e-dead-handlers.py` a root.
//!
//! WHAT THIS IS FOR. The census was invoked by nothing: zero references
//! anywhere in `scripts/`, `tests/`, `.github/`, `src/` or `docs/`. Run once
//! by hand on 2026-10-18 it found SIX real dead error-handlers, fixed in
//! 2552809ee and faee5fef8 — four in `handoff_hmac.sh` / `handoff_stale.sh`
//! where a negative-path assertion could not run because the EXPECTED exit
//! aborted the script before the rc was captured, and two in
//! `determinism.sh` where `run_3x_assert_identical()` could not report its own
//! failures. No gate would have caught any of them, because the tool that
//! finds them was wired to nothing.
//!
//! WHY THIS IS A TEST AND NOT A verify.sh STAGE. `scripts/verify-budget.toml`
//! declares a fixed `total_expected_seconds` and
//! `tests/verification_drift_guard.rs` asserts the stage set, so a new
//! `run_stage` needs a budget key and a total update. Riding an existing
//! `#[path]` include into a suite bundle adds no test target, no verify.sh
//! stage and no budget key — the route bd-16imy established. The census is a
//! static analyser and needs no `ee` binary, so it runs wherever the suite
//! runs.
//!
//! WHY `--gate` AND NOT THE BARE CENSUS. The census has no exit statement at
//! all: it falls off the end of `__main__` and ALWAYS exits 0. Asserting exit
//! 0 on it would have been unfalsifiable — a check that cannot red, which is
//! worse than an unwired tool because it reads as coverage. `--gate` is the
//! opt-in verdict mode, and its two polarities were demonstrated end-to-end
//! against one fixture differing only by the repair, before this file existed:
//!
//!     out="$(false)"       rc=$?    ->  exit 1, "DEAD: my_handler() only
//!                                       ever called bare"
//!     out="$(false)" || rc=$?       ->  exit 0, "GATE PASS"
//!
//! UNKNOWN verdicts do NOT block. The census has shipped four false-positive
//! classes already (b9727a932, 8c24ce49c) — at one point three quarters of its
//! headline DEAD rows were misattributed `$?` reads — so a gate that red on
//! "no call site found" would red on the instrument's own uncertainty.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn census_script() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("audit-set-e-dead-handlers.py")
}

fn run_census(args: &[&str]) -> Result<(bool, String), String> {
    let output = Command::new("python3")
        .arg(census_script())
        .args(args)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run set -e census with {args:?}: {error}"))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), combined))
}

/// The census's own arms, which include both polarities of the gate predicate.
///
/// This arm is what keeps the arm below honest. A gate is only as good as the
/// classifier under it, and this classifier's history is four false-positive
/// classes. If a future edit breaks its discrimination — so that a real dead
/// handler stops being reported, or a correct `|| rc=$?` starts being reported
/// — the self-test reds here rather than silently widening or emptying the
/// gate.
#[test]
fn set_e_census_self_test_passes() -> TestResult {
    let (ok, out) = run_census(&["--self-test"])?;
    if !ok {
        return Err(format!("set -e census self-test failed:\n{out}"));
    }
    // A self-test that runs zero arms also "passes". Read the denominator.
    if !out.contains("arms passed") {
        return Err(format!(
            "set -e census self-test printed no arm tally, so nothing proves \
             it ran any arms:\n{out}"
        ));
    }
    Ok(())
}

fn check_format_script() -> PathBuf {
    repo_root().join("scripts").join("check-format.sh")
}

fn run_check_format(args: &[&str]) -> Result<(bool, String), String> {
    let output = Command::new(check_format_script())
        .args(args)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run check-format.sh with {args:?}: {error}"))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), combined))
}

/// `scripts/check-format.sh`'s own arms. bd-w6849's second half.
///
/// WHY IT LIVES IN THIS FILE. This file already is "the gate scripts that were
/// invoked by nothing, given a root", and `autotests = false` means a new
/// `tests/*.rs` needs either a `[[test]]` entry or another `#[path]` include.
/// Riding this one costs no target, no verify.sh stage and no budget key.
///
/// WHY IT IS NOT REDUNDANT WITH THE RUSTFMT GATE. `cargo fmt` walks only the
/// module tree reachable under the DEFAULT cfg, so files pulled in by
/// `include!` and files behind `#[cfg(test)]`, `#[cfg(unix)]` or a feature gate
/// are never visited. Measured on this tree the script reports SIX
/// include!-only files plus eight cfg-gated ones that `cargo fmt --check` does
/// not see, which is exactly the population no other gate covers.
///
/// EXECUTED BEFORE BEING WIRED, as bd-w6849 requires: `--self-test` passed 7
/// of 7 arms and the live run exited 0 on this tree before this file named it.
#[test]
fn check_format_self_test_passes() -> TestResult {
    let (ok, out) = run_check_format(&["--self-test"])?;
    if !ok {
        return Err(format!("check-format.sh self-test failed:\n{out}"));
    }
    // A self-test that runs zero arms also "passes". Read the denominator.
    if !out.contains("arms passed") {
        return Err(format!(
            "check-format.sh self-test printed no arm tally, so nothing proves \
             it ran any arms:\n{out}"
        ));
    }
    Ok(())
}

/// Files that `cargo fmt` cannot reach must still be formatted.
///
/// WHY EXIT 0 IS NOT ENOUGH HERE. The script documents its own contract as
/// "0 clean, OR inconclusive (fail-open)": an unresolvable pinned toolchain, a
/// missing rustfmt, or a timeout each warn and return 0. Asserting only the
/// status would credit a check that never ran — the failure mode this file's
/// header objects to — so an inconclusive run is treated as a RED here, with
/// the warning quoted. In a test context the pinned toolchain is a declared
/// requirement of this repo, so "I could not check" is a finding, not noise.
#[test]
fn include_only_and_cfg_gated_files_stay_formatted() -> TestResult {
    let (ok, out) = run_check_format(&[])?;
    if !ok {
        return Err(format!(
            "check-format.sh reported formatting drift in files `cargo fmt` \
             does not reach (include!-only or cfg-gated). Run \
             `scripts/check-format.sh` and format the files it names.\n{out}"
        ));
    }
    if out.contains("inconclusive") {
        return Err(format!(
            "check-format.sh exited 0 INCONCLUSIVE, so nothing was actually \
             checked. Its fail-open paths are an unresolvable pinned \
             toolchain, a missing rustfmt under it, or a timeout.\n{out}"
        ));
    }
    if !out.contains("include!-only file(s)") {
        return Err(format!(
            "check-format.sh printed no include!-only tally, so nothing proves \
             it walked the population this arm exists for:\n{out}"
        ));
    }
    Ok(())
}

/// No shell script in the tree may carry a handler that `set -e` prevents from
/// running.
#[test]
fn no_shell_script_has_a_dead_error_handler() -> TestResult {
    let (ok, out) = run_census(&["--gate"])?;
    if ok {
        return Ok(());
    }
    Err(format!(
        "scripts/audit-set-e-dead-handlers.py --gate reported dead error \
         handlers. A handler below a bare `out=$(cmd)` under `set -e` never \
         runs: -e aborts the script at the failing assignment, so the rc is \
         never captured and the negative-path assertion that depends on it is \
         silently skipped. Put the assignment to the LEFT of `||` so -e is \
         suspended through the whole call, e.g. `out=$(cmd) || rc=$?`.\n{out}"
    ))
}
