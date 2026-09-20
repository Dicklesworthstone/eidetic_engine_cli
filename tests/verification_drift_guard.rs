//! Contract tests for the verification drift guard (EE-eism).
//!
//! The drift guard prevents "invisible baseline drift" by ensuring that
//! any red verification gate has a corresponding open bead tracking it.

#![allow(clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NORMAL_CARGO_TEST_GATE: &str = "cargo test --workspace --lib --bins --tests --examples";
const BENCH_INCLUDED_TEST_GATE: &str = "cargo test --workspace --all-targets";

/// Marks a `[[stage]]` that has no measured p50 yet.
///
/// A stage carrying this must NOT also declare `expected_seconds_p50`: an
/// honest gap beats a plausible-looking number, and verify.sh already skips
/// budget enforcement for a stage whose p50 is absent.
const UNMEASURED_MARKER: &str = "expected_seconds_p50_unmeasured = true";

/// How many stages may sit unmeasured at once.
///
/// This is a RATCHET and it only moves down. It was set to 27 on 2026-09-16,
/// the exact number of verify.sh stages that had never been budgeted; measuring
/// one means lowering this by one in the same commit. It is deliberately not
/// slack for new stages -- a 28th unmeasured stage should fail here and be
/// measured instead.
const UNMEASURED_STAGE_ALLOWANCE: usize = 27;

fn project_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn verify_script_path() -> PathBuf {
    project_root().join("scripts/verify.sh")
}

fn verify_budget_path() -> PathBuf {
    project_root().join("scripts/verify-budget.toml")
}

fn overhaul_script_path() -> PathBuf {
    project_root().join("scripts/e2e_overhaul.sh")
}

/// Modules allowed to have no `--lib <module>::` shard in CI, each paired with
/// the step that covers them instead.
///
/// An entry is not an excuse: `ci_workflow_uses_normal_non_benchmark_test_gate`
/// re-derives the justification every run, asserting the module is still
/// feature-gated (so a default `--lib` run could not reach it anyway) AND that
/// the named covering step is still present in ci.yml.
const LIB_SHARD_EXEMPT: &[(&str, &str)] = &[("mcp", "scripts/mcp_lib_tests.sh")];

/// Top-level modules declared in `src/lib.rs`, each paired with whether its
/// declaration carries a `#[cfg(...)]` gate.
fn lib_modules() -> Vec<(String, bool)> {
    let source = fs::read_to_string(project_root().join("src/lib.rs")).expect("read src/lib.rs");
    let mut modules = Vec::new();
    let mut cfg_gated = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with("#[cfg(") {
            cfg_gated = true;
            continue;
        }
        let declaration = trimmed
            .strip_prefix("pub mod ")
            .or_else(|| trimmed.strip_prefix("mod "))
            .and_then(|rest| rest.strip_suffix(';'));
        if let Some(name) = declaration {
            if !name.contains(' ') && !name.contains('{') {
                modules.push((name.to_string(), cfg_gated));
            }
        }
        // Any line that is not a `#[cfg(...)]` attribute ends the attribute run.
        cfg_gated = false;
    }

    modules
}

fn output_excerpt(output: &Output) -> String {
    format!(
        "status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn init_git_fixture(root: &Path) {
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .output()
        .expect("git init fixture");
    assert!(
        output.status.success(),
        "git init fixture failed\n{}",
        output_excerpt(&output)
    );
}

fn git_add_fixture(root: &Path, paths: &[&str]) {
    let output = Command::new("git")
        .arg("add")
        .args(paths)
        .current_dir(root)
        .output()
        .expect("git add fixture");
    assert!(
        output.status.success(),
        "git add fixture failed\n{}",
        output_excerpt(&output)
    );
}

fn write_snapshot_fixture(root: &Path, name: &str, snap: Option<&str>, proposal: &str) {
    let snapshot_dir = root.join("tests/snapshots");
    fs::create_dir_all(&snapshot_dir).expect("create fixture snapshots dir");
    if let Some(contents) = snap {
        fs::write(snapshot_dir.join(format!("{name}.snap")), contents).expect("write .snap");
    }
    fs::write(snapshot_dir.join(format!("{name}.snap.new")), proposal).expect("write .snap.new");
}

fn run_snapshot_proposal_guard(root: &Path) -> Output {
    Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -euo pipefail
REPO_ROOT="$FIXTURE_ROOT"
eval "$(awk '/^snapshot_proposal_guard\(\) /,/^}/' "$VERIFY_SCRIPT")"
snapshot_proposal_guard
"#,
        )
        .env("FIXTURE_ROOT", root)
        .env("VERIFY_SCRIPT", verify_script_path())
        .current_dir(project_root())
        .output()
        .expect("run snapshot proposal guard")
}

fn run_drift_guard_at(root: &Path, args: &[&str]) -> Output {
    Command::new("sh")
        .arg(project_root().join("scripts/verification-drift-guard.sh"))
        .args(args)
        .current_dir(root)
        .output()
        .expect("run verification drift guard")
}

fn verify_stage_names(script: &str) -> BTreeSet<String> {
    script
        .lines()
        .filter_map(|line| {
            let marker = "run_stage \"";
            let start = line.find(marker)? + marker.len();
            let rest = &line[start..];
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        })
        .collect()
}

/// Render one `record_gated_off` call by executing verify.sh's own helper.
///
/// Extracts the function from the script and runs it, so the test observes
/// what a ci-smoke run WOULD emit rather than what the source happens to
/// spell. Same technique `run_snapshot_proposal_guard` already uses in this
/// file.
fn render_gated_off(label: &str, reason: &str) -> Output {
    Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -euo pipefail
eval "$(awk '/^record_gated_off\(\) /,/^}/' "$VERIFY_SCRIPT")"
STAGE_RESULTS=""
STAGE_GATED_OFF=0
STAGE_GATED_OFF_NAMES=""
record_gated_off "$LABEL" "$REASON"
printf '%b' "$STAGE_RESULTS"
"#,
        )
        .env("VERIFY_SCRIPT", verify_script_path())
        .env("LABEL", label)
        .env("REASON", reason)
        .current_dir(project_root())
        .output()
        .expect("render record_gated_off")
}

/// Runs the real `closure_lint_or_tracked_drift` with both of its subprocesses
/// stubbed, and returns the status it hands back to `run_stage`.
///
/// Extracts the function from verify.sh rather than restating its logic, for
/// the same reason `render_gated_off` does: the thing under test is what the
/// script WOULD do, not what this test remembers it spelling.
fn closure_gate_status(lint_code: i32, guard_code: i32) -> Output {
    Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -uo pipefail
# THIS PREAMBLE MIRRORS verify.sh'S CONSTANTS AND IS A DRIFT SURFACE.
#
# The harness extracts only the FUNCTION from verify.sh, so every constant the
# function reads must be redeclared here. Under `set -u` a missing one kills
# the function before it prints, and `closure_gate_code` then returns "" --
# which is what a stale preamble looks like: not one test failing with a wrong
# code, but EVERY test in this family failing with an empty one.
#
# That happened when CLOSURE_LINT_EMPTY_POPULATION_CODE was added to verify.sh
# and not to this list: four passing tests went red at once. Loud, and easy to
# misread as the change under test being wrong rather than the stub being
# incomplete. If a whole family here returns "", check this list first.
BEADS_LOCK_SKIP_CODE=75
CLOSURE_LINT_STALE_BASELINE_CODE=3
CLOSURE_LINT_EMPTY_POPULATION_CODE=4
with_beads_read_locks() {
    case "$1" in
        *closure-lint.sh)             return "$LINT_CODE" ;;
        *verification-drift-guard.sh) return "$GUARD_CODE" ;;
    esac
}
eval "$(awk '/^closure_lint_or_tracked_drift\(\) /,/^}/' "$VERIFY_SCRIPT")"
closure_lint_or_tracked_drift >/dev/null 2>&1
printf '%s' "$?"
"#,
        )
        .env("VERIFY_SCRIPT", verify_script_path())
        .env("LINT_CODE", lint_code.to_string())
        .env("GUARD_CODE", guard_code.to_string())
        .current_dir(project_root())
        .output()
        .expect("run closure_lint_or_tracked_drift")
}

fn closure_gate_code(lint_code: i32, guard_code: i32) -> String {
    let output = closure_gate_status(lint_code, guard_code);
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The closure-lint gate must be ABLE to fail.
///
/// Regression test for bd-closure-lint-gate-cannot-fail-6hb5b. The function
/// used to read `$?` after an `if`, and a false `if` with no `else` exits 0,
/// so the captured status was always 0 and `return "$closure_exit"` could
/// never be non-zero. Executed against the pre-fix function, ALL FIVE cases
/// below returned 0 -- the gate could not fail under any condition, while
/// run_stage recorded every one of them as PASS.
///
/// These assert the routing decision, not the linter's own logic.
#[test]
fn closure_lint_gate_can_actually_fail() {
    // A violation the drift guard does not excuse must fail the gate. This is
    // the case the branch exists for, and the one that was dead.
    assert_eq!(
        closure_gate_code(1, 1),
        "1",
        "unexcused closure-lint violation must fail the stage"
    );
}

#[test]
fn closure_lint_gate_still_passes_and_still_excuses() {
    // Paired positives: the fix must not turn the gate into one that always
    // fails, which would be the same defect pointing the other way.
    assert_eq!(closure_gate_code(0, 0), "0", "a clean lint must pass");
    assert_eq!(
        closure_gate_code(1, 0),
        "0",
        "tracked drift must still excuse a violation"
    );
}

/// A contended closure-lint must reach run_stage's contention counter.
///
/// Before the fix a held beads lock fell through to the drift guard, and a
/// passing guard converted a gate that NEVER EXECUTED into a PASS that no
/// counter saw -- invisible to the INCOMPLETE banner and to the exit status.
/// A STALE audit baseline must not be excusable by the drift guard.
///
/// `scripts/closure-lint.sh` exits 3 when its baseline lists debt that no longer
/// exists. That is bookkeeping to delete, not a violation for a bead to track,
/// and the guard decides what to excuse by reading `.count` from the report --
/// which is ZERO in this case, because every live violation IS baselined. So
/// routing exit 3 through the guard would excuse it on every run and make the
/// linter's stale arm inert inside the only gate this project has.
///
/// Measured before the code was chosen: with the linter at 1 and the guard at 0,
/// this function returns 0. That is the correct behaviour for a TRACKED
/// violation and the wrong one for a stale baseline, which is why the two need
/// different exit codes rather than different messages.
#[test]
fn a_stale_closure_lint_baseline_is_not_excusable_by_the_drift_guard() {
    assert_eq!(
        closure_gate_code(3, 0),
        "3",
        "a stale baseline must fail even when the drift guard passes"
    );
    assert_eq!(
        closure_gate_code(3, 1),
        "3",
        "a stale baseline must fail when the drift guard fails too"
    );
    // The paired contrast: a plain violation IS still excusable, so this is a
    // new un-excusable class rather than the end of the excuse path.
    assert_eq!(
        closure_gate_code(1, 0),
        "0",
        "a tracked violation must still be excused by a passing drift guard"
    );
}

/// An audit that read NOTHING must not be excusable either.
///
/// THE HOLE THIS CLOSES. closure-lint.sh's audit branch ended
/// `[ -z "$BEAD_ROWS" ] && [ "$VIOLATION_COUNT" -eq 0 ]` -> write a "pass"
/// report and exit 0. Audit mode reads the WHOLE ledger, which holds 233
/// matching rows today, so zero means the read failed -- a missing
/// .beads/issues.jsonl, a malformed line, or a filter that stopped matching.
/// `relevant_closed_bead_rows` ends `2>/dev/null || true`, so all three of
/// those collapse into the same empty result as a genuinely clean tree.
///
/// WHY A DISTINCT EXIT CODE AND NOT A MESSAGE. The drift guard decides by
/// reading `.count` from the report, which is ZERO for an abstention. Routing
/// it through the guard would excuse it every time and leave the arm inert --
/// the linter would report "I read nothing" and the gate would answer
/// "excused". That is the same reasoning the stale-baseline arm above records,
/// and the reason 4 is handled beside 3 in verify.sh rather than falling
/// through to the excuse path.
///
/// The contention case (75) was already covered when this was written; this
/// one was not, which is how an empty-world hole survived in a gate while a
/// newer gate was being built specifically to avoid one.
#[test]
fn an_empty_closure_lint_population_is_not_excusable_by_the_drift_guard() {
    assert_eq!(
        closure_gate_code(4, 0),
        "4",
        "an empty audit population must fail even when the drift guard passes"
    );
    assert_eq!(
        closure_gate_code(4, 1),
        "4",
        "an empty audit population must fail when the drift guard fails too"
    );
    // The paired contrast, as on the stale-baseline arm: a plain violation is
    // STILL excusable, so this is a new un-excusable class rather than the end
    // of the excuse path.
    assert_eq!(
        closure_gate_code(1, 0),
        "0",
        "a tracked violation must still be excused by a passing drift guard"
    );
    // And an abstention must not be confused with contention: both are
    // non-verdicts, but one is "blocked" and the other is "read nothing", and
    // collapsing them is what made this invisible.
    assert_ne!(
        closure_gate_code(4, 0),
        closure_gate_code(75, 0),
        "an abstention and a contention must not share an exit code"
    );
}

#[test]
fn contended_closure_lint_is_reported_as_contention_not_as_a_pass() {
    let skip_code = "75";
    assert_eq!(
        closure_gate_code(75, 0),
        skip_code,
        "a contended lint must not be excused by a guard that did run"
    );
    assert_eq!(
        closure_gate_code(75, 75),
        skip_code,
        "a contended lint must report contention when the guard is contended too"
    );
}

/// Runs the REAL `run_stage` over the REAL `closure_lint_or_tracked_drift`.
///
/// `closure_gate_status` above stops at the function boundary: it proves what
/// the gate hands back, not what `verify.sh` then does with it. The bead's
/// deletion condition is stricter than that -- "retire when an injected
/// closure-lint failure makes verify.sh exit NON-ZERO" -- and the two are
/// joined by `run_stage`, which nothing in this repo executed. A gate that
/// returns 1 into a wrapper that swallows it is still an ungated gate.
///
/// Only the two subprocesses are stubbed. `run_stage` and the gate are both
/// extracted verbatim, so this exercises the same composition as verify.sh:1062,
/// `run_stage "Closure Linter" "closure_lint_or_tracked_drift"`.
///
/// Returns (stdout, process exit status). On the failing path `run_stage` calls
/// `exit $exit_code`, so the trailing `printf` never runs and stdout is empty --
/// that absence IS the propagation, and is asserted as such.
fn closure_stage_through_run_stage(lint_code: i32, guard_code: i32) -> (String, i32) {
    let output = Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -uo pipefail
BEADS_LOCK_SKIP_CODE=75
CLOSURE_LINT_STALE_BASELINE_CODE=3
# Mirrors verify.sh, same drift surface as the preamble in
# closure_gate_status: a constant missing here kills the extracted function
# under `set -u` before it prints anything.
CLOSURE_LINT_EMPTY_POPULATION_CODE=4
STAGE_RESULTS=""
STAGE_PASSED=0
STAGE_SKIPPED_CONTENTION=0
STAGE_SKIPPED_CONTENTION_NAMES=""
ARTIFACT_DIRS=""
stage_budget_summary() { printf 'budget-stub'; }
capture_test_trace_artifacts() { :; }
enforce_stage_budget() { :; }
with_beads_read_locks() {
    case "$1" in
        *closure-lint.sh)             return "$LINT_CODE" ;;
        *verification-drift-guard.sh) return "$GUARD_CODE" ;;
    esac
}
eval "$(awk '/^run_stage\(\) /,/^}/' "$VERIFY_SCRIPT")"
eval "$(awk '/^closure_lint_or_tracked_drift\(\) /,/^}/' "$VERIFY_SCRIPT")"
run_stage "Closure Linter" "closure_lint_or_tracked_drift" >/dev/null 2>&1
printf 'survived=1 passed=%s contended=%s' "$STAGE_PASSED" "$STAGE_SKIPPED_CONTENTION"
"#,
        )
        .env("VERIFY_SCRIPT", verify_script_path())
        .env("LINT_CODE", lint_code.to_string())
        .env("GUARD_CODE", guard_code.to_string())
        .current_dir(project_root())
        .output()
        .expect("run run_stage over closure_lint_or_tracked_drift");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (stdout, output.status.code().unwrap_or(-1))
}

/// Run verify.sh's REAL `stage_status_for_exit_code` over one exit code.
///
/// Extracted from the script rather than mirrored: a copy of a `case` statement
/// proves only that the copy agrees with itself.
fn stage_status_for(exit_code: i32) -> String {
    let output = Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -uo pipefail
BEADS_LOCK_SKIP_CODE=75
eval "$(awk '/^stage_status_for_exit_code\(\) /,/^}/' "$VERIFY_SCRIPT")"
stage_status_for_exit_code "$CODE"
"#,
        )
        .env("VERIFY_SCRIPT", verify_script_path())
        .env("CODE", exit_code.to_string())
        .current_dir(project_root())
        .output()
        .expect("run stage_status_for_exit_code");
    assert_ne!(
        output.status.code(),
        Some(127),
        "exit 127 means stage_status_for_exit_code was never defined; the awk \
         extraction found nothing and this test would be vacuous"
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// bd-reality-core-convergence-1azkt.5: a stage outcome must name WHICH kind it
/// is. Before this, five distinguishable outcomes shared two tokens, and every
/// failure class printed "FAIL" -- so a timeout, which establishes nothing, was
/// spelled exactly like a real assertion failure, which establishes that the
/// checked thing is broken.
///
/// The default arm is asserted too. A classifier that returned FAIL for
/// everything would satisfy the FAIL cases alone.
#[test]
fn stage_status_names_the_kind_of_outcome_not_just_pass_or_fail() {
    for (code, expected) in [
        (0, "PASS"),
        (75, "SKIP"),
        (124, "TIMEOUT"),
        (137, "INFRA_ERROR"),
        (130, "CANCELLED"),
        (143, "CANCELLED"),
    ] {
        assert_eq!(
            stage_status_for(code),
            expected,
            "exit {code} must classify as {expected}"
        );
    }

    // Unrecognised codes stay FAIL. This is the arm that keeps the classifier
    // honest: it must not invent a gentler status for a code it does not know.
    for code in [1, 2, 101, 255] {
        assert_eq!(
            stage_status_for(code),
            "FAIL",
            "exit {code} is not a recognised infrastructure signal and must stay FAIL"
        );
    }
}

/// The vocabulary must be declared in full, so a status nothing emits is
/// visible as an undelivered promise rather than silently absent.
#[test]
fn verify_declares_the_whole_stage_status_vocabulary() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let declared = script
        .lines()
        .find(|line| line.starts_with("STAGE_STATUS_VOCABULARY="))
        .expect("verify.sh must declare STAGE_STATUS_VOCABULARY");
    for status in [
        "PASS",
        "FAIL",
        "NOT_APPLICABLE",
        "SKIP",
        "ADVISORY",
        "TRACKED_RED",
        "INFRA_ERROR",
        "TIMEOUT",
        "CANCELLED",
    ] {
        assert!(
            declared.contains(status),
            "the stage status vocabulary must declare {status}"
        );
    }
}

/// A declared-non-required stage may NEVER report green, and the excused
/// population must be countable (bd-reality-core-convergence-1azkt.5, ruled
/// 2026-09-17).
///
/// Green must mean one thing: this ran and passed. Once green can also mean
/// "this was excused", no consumer can tell a stage that worked from a stage
/// that was allowed not to. This pins the three properties that keep ADVISORY
/// and TRACKED_RED a classification rather than an escape hatch:
///
///   1. they are counted SEPARATELY from passed;
///   2. the summary prints a census, so an excused stage appears in the same
///      line that claims success;
///   3. nothing collapses them into PASS.
#[test]
fn excused_stages_are_counted_and_never_reported_as_passed() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");

    for counter in ["STAGE_ADVISORY=0", "STAGE_TRACKED_RED=0"] {
        assert!(
            script.contains(counter),
            "verify.sh must track {counter} separately from STAGE_PASSED"
        );
    }

    // The advisory and tracked-red branches must never touch STAGE_PASSED.
    for token in ["ADVISORY ${name}", "TRACKED_RED ${name}"] {
        assert!(
            script.contains(token),
            "the results ledger must record {token} verbatim, not a PASS alias"
        );
    }

    // The census must name every status, including the excused ones, in the
    // headline. A banner that prints only `passed` lets an excused stage hide
    // behind a number that looks like a total.
    let census_line = script
        .lines()
        .find(|line| line.contains("local census="))
        .expect("verification_summary_banner must build a per-status census");
    for label in ["advisory", "tracked-red", "did-not-run", "not-applicable"] {
        assert!(
            census_line.contains(label),
            "the summary census must report `{label}`; an excuse you cannot count \
             is an excuse nobody audits"
        );
    }
}

/// Absence of a `requirement` declaration means REQUIRED, and no stage may be
/// non-required today without a bead owning it.
///
/// One stage declares one as of 2026-09-19: "Lexical Relevance Contract
/// (tracked red)", owned by bd-reality-core-convergence-1azkt.11. Asserting the
/// exact count makes adding or removing a declaration a deliberate, reviewable
/// act instead of a default somebody drifts into — and the tracked_red arm
/// refuses a stage that claims known-red status without naming who owns it.
///
/// Known gap, NOT closed here: this guard and
/// `excused_stages_are_counted_and_never_reported_as_passed` both only inspect
/// verify.sh's FAILURE branch. `run_stage`'s success path never consults
/// `stage_requirement`, so a tracked_red stage whose pins start passing prints
/// PASS — the exact collapse between "this ran and passed" and "this was
/// excused" that the manifest's requirement policy forbids. Until that path
/// checks the declaration, the discipline is the manifest comment: remove the
/// declaration in the same commit that turns the stage green.
#[test]
fn no_stage_is_declared_non_required_without_a_bead() {
    let manifest = fs::read_to_string(verify_budget_path()).expect("read verify-budget.toml");
    let blocks = budget_stage_blocks(&manifest);

    let mut problems = Vec::new();
    let mut non_required = 0_usize;
    for block in &blocks {
        let name = block
            .iter()
            .find_map(|line| line.trim().strip_prefix("name = "))
            .map(|value| value.trim_matches('"'))
            .expect("stage should have a name");
        let Some(requirement) = block
            .iter()
            .find_map(|line| line.trim().strip_prefix("requirement = "))
            .map(|value| value.trim_matches('"'))
        else {
            continue;
        };

        non_required += 1;
        match requirement {
            "advisory" => {}
            "tracked_red" => {
                if !block
                    .iter()
                    .any(|line| line.trim().starts_with("tracked_red_bead = "))
                {
                    problems.push(format!(
                        "{name} declares requirement = \"tracked_red\" with no \
                         tracked_red_bead; a known-red stage must name the bead that owns it"
                    ));
                }
            }
            other => problems.push(format!(
                "{name} declares requirement = \"{other}\", which verify.sh does not \
                 recognise and will treat as required. Use \"advisory\" or \
                 \"tracked_red\", or drop the line."
            )),
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // 2 since 2026-09-19, and the two are red for DIFFERENT reasons:
    //   "Lexical Relevance Contract (tracked red)" -> bd-reality-core-convergence-1azkt.11
    //       the pins, red on purpose, documenting the lexical relevance defect
    //   "Lexical Relevance Contract Harness Guard" -> bd-iqg34
    //       red because the RCH target dir writes the test binary outside
    //       <target>/debug/deps, so the harness cannot locate it. Measured on
    //       hz4, job 30025237340881842: GUARD_EXIT=1 in 0s.
    // The guard was REQUIRED when introduced at e99eb0bbb, which fail-fasted
    // verify.sh on this fleet. Returns to 1 when bd-iqg34 is fixed and the
    // guard goes back to required, and to 0 when the pins themselves go green.
    assert_eq!(
        non_required, 2,
        "stages are declared non-required. That may be correct, but it is a \
         deliberate act: update this count in the same commit so the excused \
         population stays visible in review."
    );
}

/// The release-candidate capsule and the runner must share ONE status
/// vocabulary (bd-reality-core-convergence-1azkt.5).
///
/// This is the lockstep that makes "one executable truth" mean something at the
/// release boundary. If the capsule's enum could drift from what verify.sh
/// emits, then `SKIP` and `NOT_APPLICABLE` — "was supposed to run and did not"
/// versus "declared inapplicable" — could converge again in the record an
/// admission decision reads, which is exactly the collapse this bead exists to
/// undo one layer down.
///
/// Asserted as set equality in BOTH directions: a status in the script but not
/// the schema is unrepresentable in a capsule, and a status in the schema but
/// not the script is a promise nothing can emit.
#[test]
fn the_proof_capsule_and_verify_share_one_status_vocabulary() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let declared = script
        .lines()
        .find(|line| line.starts_with("STAGE_STATUS_VOCABULARY="))
        .expect("verify.sh must declare STAGE_STATUS_VOCABULARY");
    let script_statuses: BTreeSet<String> = declared
        .split_once('=')
        .expect("vocabulary line must be an assignment")
        .1
        .trim_matches('"')
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    let capsule_path = project_root().join("docs/schemas/ee.release_candidate_proof.v1.json");
    let capsule: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&capsule_path).expect("read the release-candidate proof schema"),
    )
    .expect("the proof capsule schema must be valid JSON");
    let schema_statuses: BTreeSet<String> = capsule
        .pointer("/properties/results/items/properties/status/enum")
        .and_then(serde_json::Value::as_array)
        .expect("capsule must enumerate per-stage status values")
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();

    assert!(
        script_statuses.len() >= 9,
        "the extraction found only {} statuses; a truncated parse would make this \
         comparison vacuous",
        script_statuses.len()
    );
    assert_eq!(
        script_statuses, schema_statuses,
        "verify.sh's STAGE_STATUS_VOCABULARY and the capsule's results[].status \
         enum must be the same set"
    );
}

/// A skeleton capsule must be unable to pose as a verified one.
///
/// `.5` emits the skeleton and `.19` populates a green capsule, so the shape
/// itself has to carry the difference — otherwise an empty capsule and a
/// verified candidate are the same document. That is the "attestation for a
/// binary that never ran" failure, at the release boundary.
#[test]
fn the_proof_capsule_cannot_omit_its_own_incompleteness() {
    let capsule_path = project_root().join("docs/schemas/ee.release_candidate_proof.v1.json");
    let capsule: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&capsule_path).expect("read the release-candidate proof schema"),
    )
    .expect("the proof capsule schema must be valid JSON");

    let required: BTreeSet<String> = capsule
        .pointer("/required")
        .and_then(serde_json::Value::as_array)
        .expect("capsule must declare required fields")
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();
    for field in ["completeness", "capsuleHash", "binaries", "results"] {
        assert!(
            required.contains(field),
            "`{field}` must be REQUIRED; an optional one can be omitted by the \
             emitter that most needs to declare it"
        );
    }

    let completeness_required: BTreeSet<String> = capsule
        .pointer("/properties/completeness/required")
        .and_then(serde_json::Value::as_array)
        .expect("completeness must declare required fields")
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();
    assert!(
        completeness_required.contains("populated")
            && completeness_required.contains("unestablished"),
        "completeness must require BOTH the populated flag and the list of what \
         was not established; a flag alone can say `false` without saying why"
    );

    // hostedCi must be nullable rather than mandatory-string: while the hosted
    // workflows are disabled_manually a capsule MUST be able to say "no hosted
    // run" instead of being forced to invent an id.
    let hosted = capsule
        .pointer("/properties/runIdentifiers/properties/hostedCi/type")
        .and_then(serde_json::Value::as_array)
        .expect("runIdentifiers.hostedCi must declare its type");
    assert!(
        hosted.iter().any(|value| value.as_str() == Some("null")),
        "hostedCi must accept null, or a capsule emitted while hosted CI is \
         disabled would have to fabricate a run identifier"
    );
}

/// A deliberately gated-off stage and a contention skip are different facts and
/// must not share a token in the results ledger. They both read "SKIP" before
/// bd-...-1azkt.5, which made a declared not-applicable indistinguishable from
/// a stage that was supposed to run and did not.
#[test]
fn a_gated_off_stage_is_not_applicable_not_a_skip() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let body: String = script
        .lines()
        .skip_while(|line| !line.starts_with("record_gated_off() "))
        .take_while(|line| *line != "}")
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("STAGE_RESULTS") && body.contains("NOT_APPLICABLE ${name}"),
        "record_gated_off must record NOT_APPLICABLE, not SKIP; body was:\n{body}"
    );
    assert!(
        !body.contains("}SKIP ${name}"),
        "record_gated_off must no longer emit the SKIP token; body was:\n{body}"
    );
}

/// THE DELETION CONDITION for bd-closure-lint-gate-cannot-fail-6hb5b: an
/// injected closure-lint failure must make verify.sh exit non-zero.
///
/// Measured against the PRE-FIX function (`git show 40a562a21^:scripts/verify.sh`)
/// composed with this same real `run_stage`, all five lint/guard pairs gave
/// `process exit 0, STAGE_PASSED=1` -- every one recorded as a passing stage.
/// That control is the finding; this test is the fix holding.
#[test]
fn an_unexcused_closure_lint_failure_exits_verify_non_zero() {
    let (stdout, code) = closure_stage_through_run_stage(1, 1);
    assert_eq!(
        code, 1,
        "an unexcused closure-lint violation must terminate verify.sh with the \
         stage's own code; stdout was:\n{stdout}"
    );
    assert!(
        stdout.is_empty(),
        "run_stage must `exit` rather than return on a failing stage, so nothing \
         after it runs; stdout was:\n{stdout}"
    );
}

/// The paired positives. Without these, a `run_stage` that exited on EVERY
/// stage would satisfy the test above and be the same defect inverted.
#[test]
fn a_clean_or_excused_closure_lint_stage_is_counted_as_passed() {
    let (stdout, code) = closure_stage_through_run_stage(0, 0);
    assert_eq!(
        code, 0,
        "a clean lint must not terminate verify.sh:\n{stdout}"
    );
    assert!(
        stdout.contains("passed=1") && stdout.contains("contended=0"),
        "a clean lint must increment STAGE_PASSED only:\n{stdout}"
    );

    let (stdout, code) = closure_stage_through_run_stage(1, 0);
    assert_eq!(
        code, 0,
        "a violation the drift guard excuses must not terminate verify.sh:\n{stdout}"
    );
    assert!(
        stdout.contains("passed=1") && stdout.contains("contended=0"),
        "an excused violation must still count as a passed stage:\n{stdout}"
    );
}

/// A contended closure-lint must land in run_stage's CONTENTION counter, not in
/// STAGE_PASSED -- that counter is what drives the INCOMPLETE banner and exit 75.
///
/// Before the fix the contended lint never reached this counter at all: the gate
/// fell through to the drift guard and a passing guard converted a stage that
/// NEVER EXECUTED into `passed=1`.
#[test]
fn a_contended_closure_lint_stage_is_counted_as_contention_not_as_passed() {
    for guard_code in [0, 75] {
        let (stdout, code) = closure_stage_through_run_stage(75, guard_code);
        assert_eq!(
            code, 0,
            "contention must not terminate the run (guard={guard_code}):\n{stdout}"
        );
        assert!(
            stdout.contains("contended=1"),
            "a contended lint must increment STAGE_SKIPPED_CONTENTION \
             (guard={guard_code}):\n{stdout}"
        );
        assert!(
            stdout.contains("passed=0"),
            "a stage that never executed must not count as passed \
             (guard={guard_code}):\n{stdout}"
        );
    }
}

/// Vacuity guard for the harness above.
///
/// `eval "$(awk ...)"` that matched nothing would define neither function, the
/// `run_stage` call would be a command-not-found returning 127, and the
/// non-zero assertion would pass for entirely the wrong reason. Pin both
/// extractions to a real, non-trivial body.
#[test]
fn the_run_stage_harness_extracts_real_functions() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    for name in ["run_stage", "closure_lint_or_tracked_drift"] {
        assert!(
            script.contains(&format!("{name}() {{")),
            "verify.sh must define {name} at column 0 for the awk extraction to find it"
        );
        let body: Vec<&str> = script
            .lines()
            .skip_while(|line| !line.starts_with(&format!("{name}() ")))
            .take_while(|line| *line != "}")
            .collect();
        assert!(
            body.len() > 10,
            "the awk range for {name} captured {} lines; an empty or truncated \
             extraction would make these tests vacuous",
            body.len()
        );
    }

    // And the extraction must actually run: a harness that defined nothing
    // would give 127, not 1.
    let (_, code) = closure_stage_through_run_stage(1, 1);
    assert_ne!(
        code, 127,
        "exit 127 means the extracted functions were never defined"
    );
}

/// Runs verify.sh's own closing verdict against an injected stage tally.
///
/// Extracts `verification_exit_status` and `verification_summary_banner` from
/// the script rather than restating them, for the same reason
/// `render_gated_off` does: the thing under test is what a real run WOULD
/// report, not what this test remembers the source spelling.
///
/// Returns (stdout, exit status) so one call can assert the banner text and the
/// exit code together -- they are two halves of one contract and asserting only
/// the banner is how the exit half went ungraded in the first place.
fn verification_verdict(passed: u32, contended: u32, gated_off: u32) -> (String, i32) {
    let output = Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -uo pipefail
BEADS_LOCK_SKIP_CODE=75
VERIFY_EXIT_INCOMPLETE="$BEADS_LOCK_SKIP_CODE"
STAGE_PASSED="$PASSED"
STAGE_SKIPPED_CONTENTION="$CONTENDED"
STAGE_GATED_OFF="$GATED_OFF"
STAGE_RESULTS=""
STAGE_SKIPPED_CONTENTION_NAMES="    - Verification Drift Guard (beads lock held)\n"
STAGE_GATED_OFF_NAMES="    - Performance Benchmarks (--include-bench not set)\n"
# The excused counters the census reads. This harness runs under `set -u`, so
# omitting them is not a silent default -- it kills the extracted banner, which
# is how the bd-...-1azkt.5 census change surfaced here as exit 127 rather than
# as a wrong number.
STAGE_ADVISORY=0
STAGE_ADVISORY_NAMES=""
STAGE_TRACKED_RED=0
STAGE_TRACKED_RED_NAMES=""
# Same rule as the counters above: verification_exit_status reads this, so
# omitting it kills the extracted function under `set -u` instead of returning a
# wrong number. 70 = EX_SOFTWARE, the code a run that attempted NOTHING returns.
VERIFY_EXIT_NOTHING_ATTEMPTED=70
eval "$(awk '/^verification_exit_status\(\) /,/^}/' "$VERIFY_SCRIPT")"
eval "$(awk '/^verification_summary_banner\(\) /,/^}/' "$VERIFY_SCRIPT")"
verification_summary_banner
exit "$(verification_exit_status)"
"#,
        )
        .env("VERIFY_SCRIPT", verify_script_path())
        .env("PASSED", passed.to_string())
        .env("CONTENDED", contended.to_string())
        .env("GATED_OFF", gated_off.to_string())
        .current_dir(project_root())
        .output()
        .expect("run verification verdict");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (stdout, output.status.code().unwrap_or(-1))
}

/// A run that attempted NOTHING is not a pass (1azkt.5 acceptance bullet 4).
///
/// `verification_exit_status` used to ask one question -- were stages skipped
/// for contention -- and answer 0 otherwise. Measured against the real function
/// before the fix:
///
/// ```text
/// passed=0 contended=0 gated_off=0 -> exit=0
/// passed=0 contended=0 gated_off=9 -> exit=0
/// ```
///
/// A green there was the absence of a FAIL, not the presence of a completed
/// check: the same vacuous-pass shape this file exists to catch, one layer up in
/// the runner that reports on everything else.
///
/// 70 rather than 75 is deliberate and is the half worth pinning. 75 is
/// EX_TEMPFAIL and wrappers RETRY it, so a zero-stage run sharing that code
/// would be retried forever, attempting nothing each time. Asserting the exact
/// code, not merely "non-zero", is what keeps the two states distinguishable.
#[test]
fn a_run_that_attempted_nothing_is_not_a_pass() {
    let (stdout, code) = verification_verdict(0, 0, 0);
    assert_eq!(
        code, 70,
        "a run with zero attempted stages must exit 70 (EX_SOFTWARE), not 0 and \
         not 75; banner was:\n{stdout}"
    );

    // Every stage gated off is the same absence wearing a different label: the
    // run declared work and performed none of it.
    let (stdout, code) = verification_verdict(0, 0, 9);
    assert_eq!(
        code, 70,
        "a run whose every declared stage was gated off must exit 70; banner \
         was:\n{stdout}"
    );
}

/// A run where every attempted stage passed exits 0.
#[test]
fn a_complete_run_exits_zero() {
    let (stdout, code) = verification_verdict(112, 0, 0);
    assert_eq!(code, 0, "clean run must exit 0; banner was:\n{stdout}");
    assert!(
        stdout.contains("112/112 attempted verification stages passed"),
        "banner must state the attempted denominator:\n{stdout}"
    );
}

/// A contended run exits 75 and NAMES what did not run.
///
/// This is the half that had no assertion anywhere. `cc9edd17b` changed the
/// exit contract of the only gate every agent is told to run before pushing,
/// and until this test existed nothing graded it: `verify.sh` has no lib tests,
/// and this file asserted the banner but never the status.
///
/// 75 is EX_TEMPFAIL and is deliberately the same value as
/// `BEADS_LOCK_SKIP_CODE` -- the code a contended stage already returns is the
/// code the whole run returns. A wrapper retries on 75 and escalates on 1, so
/// contention can never be mistaken for a stage that ran and failed.
#[test]
fn a_contended_run_exits_seventy_five_and_names_the_skipped_stages() {
    let (stdout, code) = verification_verdict(108, 4, 0);
    assert_eq!(
        code, 75,
        "contended run must exit 75; banner was:\n{stdout}"
    );
    assert!(
        stdout.contains("INCOMPLETE"),
        "a contended run must lead with INCOMPLETE:\n{stdout}"
    );
    assert!(
        stdout.contains("did NOT run (lock contention)"),
        "the banner must say the stages did not run:\n{stdout}"
    );
    assert!(
        stdout.contains("Verification Drift Guard"),
        "skipped stages must be named, not just counted:\n{stdout}"
    );
}

/// Deliberately gating a stage off is NOT incompleteness: it still exits 0.
///
/// The paired negative. Without it, an implementation that returned 75 whenever
/// any stage was absent would satisfy the contention test above and still be
/// wrong -- choosing not to run benches is not the same as being unable to run
/// the drift guard.
#[test]
fn a_gated_off_run_still_exits_zero() {
    let (stdout, code) = verification_verdict(108, 0, 4);
    assert_eq!(
        code, 0,
        "gated-off is not incomplete; banner was:\n{stdout}"
    );
    assert!(
        stdout.contains("108/108 attempted verification stages passed"),
        "gated-off stages must be outside the attempted denominator:\n{stdout}"
    );
    assert!(
        stdout.contains("gated off (not attempted) : 4"),
        "gated-off stages must be counted separately:\n{stdout}"
    );
}

/// The verdict harness must be executing verify.sh's real functions.
///
/// `eval "$(awk ...)"` that matched nothing would define no functions, and the
/// three tests above would then be asserting against an empty banner and a
/// default exit code. That is the same vacuity this file already guards against
/// for `record_gated_off`, and it is the shape that let the exit contract go
/// ungraded while looking covered.
#[test]
fn the_verdict_harness_extracts_real_functions() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    for name in ["verification_exit_status", "verification_summary_banner"] {
        assert!(
            script.contains(&format!("{name}() {{")),
            "verify.sh must define {name} at column 0 for the awk extraction to find it"
        );
    }
    let (stdout, _) = verification_verdict(1, 0, 0);
    assert!(
        stdout.contains("Stage accounting:"),
        "the extracted banner produced no accounting block, so the extraction \
         found nothing:\n{stdout}"
    );
}

#[test]
fn fake_oidc_idp_selfcheck_wiring() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let ordered_calls = [
        "run_stage \"Fake Tailscale Harness E2E (SRR6.46.10)\" \"./scripts/e2e_overhaul/lib/test_fake_tailscale.sh\"",
        "run_stage \"Fake OIDC IdP Harness E2E (T7.7)\" \"./scripts/e2e_overhaul/fake_idp_harness_smoke.sh\"",
        "run_stage \"Fake OIDC IdP Defects E2E (T7.7)\" \"./scripts/e2e_overhaul/fake_idp_defects_smoke.sh\"",
        "run_stage \"Fake OIDC IdP Matrix Self-Check E2E (T7.7)\" \"./scripts/e2e_overhaul/fake_idp_selfcheck.sh\"",
        "run_stage \"Tailscale Local Probe E2E (SRR6.46.1)\" \"./scripts/e2e_overhaul/tailscale_local_probe.sh\"",
    ];
    let run_stage_lines: Vec<_> = script
        .lines()
        .enumerate()
        .filter_map(|(line_number, line)| {
            let trimmed = line.trim();
            trimmed
                .starts_with("run_stage ")
                .then_some((line_number, trimmed))
        })
        .collect();
    let positions: Vec<_> = ordered_calls
        .iter()
        .map(|call| {
            let matches: Vec<_> = run_stage_lines
                .iter()
                .filter(|(_, line)| line == call)
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "{call} should be an exact executable stage exactly once"
            );
            matches[0].0
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "fake IdP stages must remain after fake Tailscale and before local probe"
    );

    let selfcheck = project_root().join("scripts/e2e_overhaul/fake_idp_selfcheck.sh");
    assert!(selfcheck.is_file(), "matrix self-check should exist");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&selfcheck)
            .expect("read matrix self-check metadata")
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "matrix self-check should be executable");
    }

    // These previously grepped verify.sh's SOURCE for the literal
    // `SKIP {label} (ci-smoke)`. 0f2256be4 replaced the hand-written lines
    // with `record_gated_off "<label>" "ci-smoke"`, which emits byte-identical
    // text AT RUNTIME -- so behaviour was unchanged and the source literal was
    // gone, and this assertion went red for a refactor that improved the
    // script. That is the same defect 6a6d30978 fixed one commit earlier in
    // this file: a guard pinned to a spelling rather than a behaviour.
    //
    // Now asserts BOTH halves, which is strictly more than the literal did:
    //   1. the label is registered exactly once through the sanctioned helper
    //      -- a label silently dropped from verify.sh still fails here;
    //   2. that helper actually renders `NOT_APPLICABLE {label} (ci-smoke)` --
    //      checked by executing it, so a change to the helper's output format
    //      fails too, which the old source grep could not see. That is how this
    //      test caught the bd-...-1azkt.5 token change instead of passing
    //      through it.
    for label in [
        "Fake OIDC IdP Harness E2E (T7.7)",
        "Fake OIDC IdP Defects E2E (T7.7)",
        "Fake OIDC IdP Matrix Self-Check E2E (T7.7)",
    ] {
        let registration = format!("record_gated_off \"{label}\" \"ci-smoke\"");
        assert_eq!(
            script.matches(&registration).count(),
            1,
            "{label} should be registered as gated-off exactly once under ci-smoke"
        );

        let rendered = render_gated_off(label, "ci-smoke");
        assert!(
            rendered.status.success(),
            "rendering record_gated_off for {label} failed\n{}",
            output_excerpt(&rendered)
        );
        let stdout = String::from_utf8_lossy(&rendered.stdout);
        // NOT_APPLICABLE since bd-...-1azkt.5. A ci-smoke exclusion is a
        // DECLARED not-applicable, and it used to share the SKIP token with a
        // contention skip -- a stage that was supposed to run and did not.
        let expected = format!("NOT_APPLICABLE {label} (ci-smoke)");
        assert_eq!(
            stdout.matches(&expected).count(),
            1,
            "record_gated_off must emit `{expected}`; got {stdout:?}"
        );
    }
}

fn budget_stage_blocks(manifest: &str) -> Vec<Vec<&str>> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();

    for line in manifest.lines() {
        if line.trim() == "[[stage]]" {
            if !current.is_empty() {
                blocks.push(current);
                current = Vec::new();
            }
            continue;
        }
        if !current.is_empty() || line.trim_start().starts_with("name = ") {
            current.push(line);
        }
    }

    if !current.is_empty() {
        blocks.push(current);
    }

    blocks
}

fn budget_stage_names(manifest: &str) -> BTreeSet<String> {
    budget_stage_blocks(manifest)
        .into_iter()
        .filter_map(|block| {
            block.iter().find_map(|line| {
                let trimmed = line.trim();
                let value = trimmed.strip_prefix("name = ")?;
                Some(value.trim_matches('"').to_string())
            })
        })
        .collect()
}

fn budget_stage_p50(block: &[&str]) -> Option<u64> {
    block.iter().find_map(|line| {
        let trimmed = line.trim();
        let value = trimmed.strip_prefix("expected_seconds_p50 = ")?;
        value.parse().ok()
    })
}

#[test]
fn drift_guard_script_exists_and_is_executable() {
    let script_path = project_root().join("scripts/verification-drift-guard.sh");
    assert!(
        script_path.exists(),
        "scripts/verification-drift-guard.sh should exist"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&script_path).expect("read metadata");
        let mode = metadata.permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "verification-drift-guard.sh should be executable"
        );
    }
}

#[test]
fn verify_budget_manifest_declares_every_verify_stage() {
    let verify_script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let budget_manifest =
        fs::read_to_string(verify_budget_path()).expect("read verify-budget.toml");

    let script_stages = verify_stage_names(&verify_script);
    let budget_stages = budget_stage_names(&budget_manifest);

    assert_eq!(
        script_stages, budget_stages,
        "verify-budget.toml must declare exactly the run_stage names from verify.sh"
    );
}

#[test]
fn verify_budget_manifest_has_p50_and_regression_factor_for_every_stage() {
    let budget_manifest =
        fs::read_to_string(verify_budget_path()).expect("read verify-budget.toml");
    let blocks = budget_stage_blocks(&budget_manifest);

    assert!(
        !blocks.is_empty(),
        "verify-budget.toml should declare at least one [[stage]]"
    );

    let mut non_benchmark_p50_total = 0;
    let mut unmeasured = Vec::new();
    for block in &blocks {
        let name = block
            .iter()
            .find_map(|line| line.trim().strip_prefix("name = "))
            .map(|value| value.trim_matches('"'))
            .expect("stage should have a name");

        if block.iter().any(|line| line.trim() == UNMEASURED_MARKER) {
            // An unmeasured stage must not also carry a p50. verify.sh's
            // `stage_budget_thresholds` returns non-zero when the field is
            // absent and `enforce_stage_budget` then skips the stage, so
            // "unmeasured" is structurally unable to produce a threshold
            // rather than merely documented as not having one.
            assert!(
                budget_stage_p50(block).is_none(),
                "stage {name} is marked `{UNMEASURED_MARKER}` but also declares \
                 expected_seconds_p50; drop one. A stage cannot be both measured \
                 and unmeasured, and keeping the number is how a guess becomes a budget."
            );
            unmeasured.push(name.to_string());
            continue;
        }

        let p50 = budget_stage_p50(block).unwrap_or_else(|| {
            panic!(
                "stage {name} must declare expected_seconds_p50, or `{UNMEASURED_MARKER}` \
                 if no measurement exists yet"
            )
        });
        let factor = block
            .iter()
            .find(|line| line.trim() == "regression_factor = 1.5")
            .unwrap_or_else(|| panic!("stage {name} should use regression_factor = 1.5"));

        assert!(
            !factor.trim().is_empty(),
            "stage {name} should keep an explicit regression factor"
        );

        if name != "Performance Benchmarks" {
            non_benchmark_p50_total += p50;
        }
    }

    // A CEILING, not an equality.
    //
    // This was `assert_eq!(non_benchmark_p50_total, 600)`. Combined with
    // `verify_budget_manifest_declares_every_verify_stage` -- which requires
    // every verify.sh stage to appear here -- an exact sum made the two guards
    // contradict each other: adding any stage to verify.sh broke one of them
    // unless someone reduced another stage's p50, i.e. wrote down a number that
    // was not the measurement. A gate that can only be satisfied by falsifying
    // a latency is worse than a loose gate, so the equality is the defect.
    //
    // Measured 2026-09-16: verify.sh had 112 stages, this manifest 85, and the
    // 85 summed to exactly 600. The guard had been red since 2026-08-10.
    assert!(
        non_benchmark_p50_total <= 600,
        "non-benchmark p50 budgets total {non_benchmark_p50_total}s, over the 600s \
         readiness ceiling. Re-measure and reduce a real stage cost -- do not \
         retune a p50 to fit, because that turns this file into fiction."
    );
    assert!(
        budget_manifest.contains("total_expected_seconds = 600"),
        "manifest should document the total 10-minute verification budget"
    );

    // Unmeasured entries are an honest gap, but they are not free: each one is
    // a stage running with no budget enforcement at all. Surfacing the count
    // keeps that visible instead of letting the list grow quietly.
    assert!(
        unmeasured.len() <= UNMEASURED_STAGE_ALLOWANCE,
        "{} stages carry no measured p50, over the allowance of {UNMEASURED_STAGE_ALLOWANCE}. \
         Measure some before adding more: {unmeasured:?}",
        unmeasured.len()
    );
}

#[test]
fn overhaul_registry_wires_tiered_recall_e2e_driver() {
    let overhaul_script = fs::read_to_string(overhaul_script_path()).expect("read e2e_overhaul.sh");

    assert!(
        overhaul_script.contains("EPIC_LETTERS=(A B C D Q E F G H I J K L M N O P R S T U V W)"),
        "scripts/e2e_overhaul.sh must include tiered recall's W epic in EPIC_LETTERS"
    );
    assert_eq!(
        overhaul_script
            .matches("[W]=\"tiered_recall_e2e.sh\"")
            .count(),
        1,
        "tiered_recall_e2e.sh must be registered exactly once in EPIC_SCRIPTS"
    );
    assert_eq!(
        overhaul_script.matches("[W]=\"tiered_recall_e2e\"").count(),
        1,
        "tiered_recall_e2e must be registered exactly once in EPIC_NAMES"
    );
    assert!(
        project_root()
            .join("scripts/e2e_overhaul/tiered_recall_e2e.sh")
            .is_file(),
        "registered tiered recall E2E script must exist on disk"
    );
}

#[test]
fn verify_sh_reports_and_enforces_stage_budgets() {
    let verify_script = fs::read_to_string(verify_script_path()).expect("read verify.sh");

    assert!(
        verify_script.contains("VERIFY_BUDGET_FILE"),
        "verify.sh should load scripts/verify-budget.toml"
    );
    assert!(
        verify_script.contains("stage_budget_summary"),
        "verify.sh should report elapsed time against each stage budget"
    );
    assert!(
        verify_script.contains("budget=advisory"),
        "verify.sh should classify advisory budget regressions"
    );
    assert!(
        verify_script.contains("exceeded hard budget"),
        "verify.sh should fail when a stage exceeds the hard budget"
    );
}

#[test]
fn drift_guard_produces_json_report() {
    let output = Command::new("sh")
        .args(["-c", "./scripts/verification-drift-guard.sh --json || true"])
        .current_dir(project_root())
        .output()
        .expect("run drift guard");

    let report_path = project_root().join(".verification-drift-report.json");

    // The script should always produce a report file
    if report_path.exists() {
        let contents = fs::read_to_string(&report_path).expect("read report");
        let parsed: serde_json::Value = serde_json::from_str(&contents).expect("parse as JSON");

        assert!(
            parsed.get("status").is_some(),
            "report should have status field: {contents}"
        );
        assert!(
            parsed.get("driftViolations").is_some(),
            "report should have driftViolations field: {contents}"
        );
        assert!(
            parsed.get("count").is_some(),
            "report should have count field: {contents}"
        );
    } else {
        // If no report, the script ran but may have exited early (e.g., no closure report)
        // This is acceptable for the contract test
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("error:") && !stderr.contains("syntax error"),
            "script should not have syntax errors: {stderr}"
        );
    }
}

#[test]
fn drift_guard_help_flag_works() {
    let output = Command::new("sh")
        .args(["-c", "./scripts/verification-drift-guard.sh --help"])
        .current_dir(project_root())
        .output()
        .expect("run drift guard --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Verification Drift Guard") || stdout.contains("drift"),
        "help should describe the drift guard: {stdout}"
    );
    assert!(output.status.success(), "help should exit 0");
}

#[test]
fn drift_guard_detects_closure_violations_without_bead() {
    // This test verifies the guard's logic:
    // If closure-lint reports violations AND no bead tracks them, drift is detected.
    //
    // We can't easily mock the beads file in an integration test, but we can verify
    // the script's JSON output structure is correct when run.

    let output = Command::new("sh")
        .args([
            "-c",
            "./scripts/verification-drift-guard.sh --json 2>&1 || true",
        ])
        .current_dir(project_root())
        .output()
        .expect("run drift guard");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The script should either:
    // 1. Report "pass" if all gates have tracking beads
    // 2. Report "fail" with drift violations if not
    // 3. Or produce a JSON report file
    // Either way, it should not have syntax errors
    assert!(
        combined.contains("Report written")
            || combined.contains("No drift detected")
            || combined.contains("drift")
            || project_root()
                .join(".verification-drift-report.json")
                .exists(),
        "script should produce meaningful output or report file: {combined}"
    );
}

#[test]
fn drift_guard_recognizes_open_closure_lint_tracking_bead() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".beads")).expect("create .beads");
    fs::write(
        temp.path().join(".closure-lint-report.json"),
        r#"{"violations":[{"bead":"closed-demo","label":"implements-surface","surface":"demo","reason":"missing tests/golden/demo.snap"}],"count":1,"status":"fail"}"#,
    )
    .expect("write closure report");
    fs::write(
        temp.path().join(".beads/issues.jsonl"),
        r#"{"id":"bd-track","title":"closure-lint verifier blocker","status":"open","labels":["verify"],"description":"Tracks closure linter violations until remediation lands."}"#,
    )
    .expect("write beads fixture");

    let output = run_drift_guard_at(temp.path(), &["--gate=closure-lint", "--json"]);
    assert!(
        output.status.success(),
        "open closure-lint bead should satisfy drift guard\n{}",
        output_excerpt(&output)
    );

    let report_path = temp.path().join(".verification-drift-report.json");
    let report = fs::read_to_string(&report_path).expect("read drift report");
    let parsed: serde_json::Value = serde_json::from_str(&report).expect("parse drift report");
    assert_eq!(parsed["status"], "pass", "tracked closure-lint report");
    assert_eq!(parsed["count"], 0, "tracked closure-lint drift count");
}

#[test]
fn verify_sh_routes_tracked_closure_lint_failures_through_drift_guard() {
    let verify_script = fs::read_to_string(verify_script_path()).expect("read verify.sh");

    assert!(
        verify_script.contains("closure_lint_or_tracked_drift()"),
        "verify.sh should define a closure-lint wrapper"
    );
    assert!(
        verify_script.contains("verification-drift-guard.sh --gate=closure-lint --json"),
        "closure-lint wrapper should delegate tracked red reports to the drift guard"
    );
    assert!(
        verify_script.contains(r#"run_stage "Closure Linter" "closure_lint_or_tracked_drift""#),
        "Closure Linter stage should run through the tracked-drift wrapper"
    );
}

#[test]
fn verify_sh_includes_drift_guard_gate() {
    let verify_script = fs::read_to_string(verify_script_path()).expect("read verify.sh");

    assert!(
        verify_script.contains("verification-drift-guard.sh"),
        "verify.sh should include the drift guard gate"
    );
    assert!(
        verify_script.contains("Verification Drift Guard"),
        "verify.sh should name the drift guard stage"
    );
}

#[test]
fn verify_sh_includes_snapshot_proposal_guard_gate() {
    let verify_script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let snapshot_guard_pos = verify_script
        .find("Snapshot Proposal Guard")
        .expect("verify.sh should name the snapshot proposal guard stage");
    let cargo_test_pos = verify_script
        .find(NORMAL_CARGO_TEST_GATE)
        .expect("verify.sh should contain the normal cargo test gate");

    assert!(
        verify_script.contains("snapshot_proposal_guard"),
        "verify.sh should define and run the snapshot proposal guard"
    );
    assert!(
        snapshot_guard_pos < cargo_test_pos,
        "snapshot proposal guard should run before the broad cargo test gate"
    );
}

#[test]
fn snapshot_proposal_guard_accepts_matching_tracked_proposals() {
    let temp = tempfile::tempdir().expect("tempdir");
    init_git_fixture(temp.path());
    write_snapshot_fixture(temp.path(), "accepted", Some("same\n"), "same\n");
    git_add_fixture(
        temp.path(),
        &[
            "tests/snapshots/accepted.snap",
            "tests/snapshots/accepted.snap.new",
        ],
    );

    let output = run_snapshot_proposal_guard(temp.path());
    assert!(
        output.status.success(),
        "matching proposal should pass\n{}",
        output_excerpt(&output)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("1 tracked insta proposal snapshot(s) match accepted snapshots"),
        "guard should report matching tracked proposal count: {stdout}"
    );
}

#[test]
fn snapshot_proposal_guard_rejects_orphaned_tracked_proposals() {
    let temp = tempfile::tempdir().expect("tempdir");
    init_git_fixture(temp.path());
    write_snapshot_fixture(temp.path(), "orphaned", None, "proposal\n");
    git_add_fixture(temp.path(), &["tests/snapshots/orphaned.snap.new"]);

    let output = run_snapshot_proposal_guard(temp.path());
    assert!(
        !output.status.success(),
        "orphaned proposal should fail\n{}",
        output_excerpt(&output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("tracked insta proposal has no accepted snapshot"),
        "guard should explain missing accepted snapshot: {stderr}"
    );
}

#[test]
fn snapshot_proposal_guard_rejects_divergent_tracked_proposals() {
    let temp = tempfile::tempdir().expect("tempdir");
    init_git_fixture(temp.path());
    write_snapshot_fixture(temp.path(), "changed", Some("accepted\n"), "proposal\n");
    git_add_fixture(
        temp.path(),
        &[
            "tests/snapshots/changed.snap",
            "tests/snapshots/changed.snap.new",
        ],
    );

    let output = run_snapshot_proposal_guard(temp.path());
    assert!(
        !output.status.success(),
        "divergent proposal should fail\n{}",
        output_excerpt(&output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("tracked insta proposal differs from accepted snapshot"),
        "guard should explain divergent proposal: {stderr}"
    );
}

#[test]
fn normal_verify_test_gate_excludes_criterion_benches() {
    let verify_script = fs::read_to_string(verify_script_path()).expect("read verify.sh");

    assert!(
        verify_script.contains(NORMAL_CARGO_TEST_GATE),
        "verify.sh should use the non-benchmark cargo test gate: {NORMAL_CARGO_TEST_GATE}"
    );
    assert!(
        !verify_script.contains(BENCH_INCLUDED_TEST_GATE),
        "verify.sh normal test gate must not use `{BENCH_INCLUDED_TEST_GATE}`; benches belong behind --include-bench"
    );
    assert!(
        verify_script.contains("--include-bench")
            && verify_script.contains("./scripts/bench_perf_regression.sh"),
        "verify.sh should preserve an explicit benchmark gate"
    );
}

/// CI must give the same coverage as verify.sh's normal gate, without benches.
///
/// This asserts the PROPERTY, not the command spelling. The earlier version
/// substring-matched `NORMAL_CARGO_TEST_GATE` against ci.yml, so when
/// `8cec224e6` ("fix(ci): shard tests and repair exposed blockers") split the
/// single combined invocation into 26 per-module `--lib` shards plus one
/// `--bins --tests --examples` run, this guard went red for three months while
/// CI was correct the entire time. A guard that fails when the thing it guards
/// improves teaches people to edit the guard, which is worse than no guard.
///
/// verify.sh is still checked against the literal spelling by
/// `normal_verify_test_gate_excludes_criterion_benches`: verify.sh is where
/// that spelling is defined, so pinning it there is a definition, not a shape.
/// bd-p54ks: pin WHICH workflows can run on an arbitrary push to main.
///
/// Measured 2026-09-20: of 48 workflow files, 36 invoke a compiling cargo
/// command, but 35 of those are PATH-FILTERED to their own delivery payload
/// (`paths: ['scripts/delivery/<name>.patch', '.github/workflows/<name>.yml']`),
/// so they never observe an ordinary source commit. Exactly one compiling
/// workflow is unfiltered -- ci.yml -- and `gh workflow list --all` reports it
/// `disabled_manually`. The one active unfiltered workflow, ci-static.yml,
/// runs `cargo fmt --check` and nothing that typechecks; its own header says
/// "Do not read a green CI Static run as 'CI is restored.'"
///
/// This guard pins the UNFILTERED set only, deliberately. Pinning totals would
/// red on every new delivery workflow -- 29 arrived in 24h -- and a guard that
/// reds on ordinary activity gets edited away. The unfiltered set is stable
/// precisely because delivery workflows are payload-scoped, so a change to it
/// means someone altered what gates main, which is the thing worth noticing.
///
/// It is a SNAPSHOT, not an assertion that main is ungated. Phrasing it as
/// "nothing compiles main" would go red the day somebody fixes that, and a
/// guard that fails when its subject improves teaches people to delete it --
/// the same trap the comment above this test already warns about.
///
/// Enabling ci.yml is not this guard's business: ci-static.yml records why it
/// is off (90-minute cargo shards queued against a predicted-red tree during a
/// 6-agent swarm). That is a capacity decision, and this test only makes the
/// current shape visible.
#[test]
fn unfiltered_push_to_main_workflows_are_pinned() {
    // (file name, runs a compiling cargo command)
    //
    // TWO workflows can run on an arbitrary push to main. ci.yml compiles and
    // is disabled_manually; ci-static.yml is active and compiles nothing. So
    // exactly one workflow observes an ordinary main commit, and it does not
    // typecheck.
    //
    // My first draft of this list had three entries. It wrongly included
    // recovery-source-snapshot.yml, because the scan I built it from applied
    // the `paths:` check only to workflows that compile, and that one does
    // not -- so a path-filtered workflow arrived here looking unfiltered.
    // This test failed on it, which is the negative arm doing its job against
    // a real mistake rather than a planted one.
    const EXPECTED_UNFILTERED: [(&str, bool); 2] = [("ci-static.yml", false), ("ci.yml", true)];

    let actual = classify_unfiltered_push_to_main(&project_root().join(".github/workflows"));
    let expected: Vec<(String, bool)> = EXPECTED_UNFILTERED
        .iter()
        .map(|(name, compiles)| ((*name).to_owned(), *compiles))
        .collect();
    assert_eq!(
        actual, expected,
        "the set of workflows that run on an ARBITRARY push to main changed. \
         Update this snapshot in the same commit, and say in the message whether \
         main's compiling coverage went up or down."
    );

    // NEGATIVE ARM, in-test: prove the classifier still detects an unfiltered
    // compiling workflow. Without this, the assertion above passes equally well
    // when the classifier is broken and when nothing changed, and those are
    // different facts.
    let probe_dir = std::env::temp_dir().join(format!(
        "ee-p54ks-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    fs::create_dir_all(&probe_dir).expect("create probe dir");
    fs::write(
        probe_dir.join("synthetic-unfiltered.yml"),
        "name: Synthetic\non:\n  push:\n    branches: [main]\njobs:\n  a:\n    steps:\n      - run: cargo test --lib\n",
    )
    .expect("write probe workflow");
    fs::write(
        probe_dir.join("synthetic-filtered.yml"),
        "name: Filtered\non:\n  push:\n    branches: [main]\n    paths:\n      - 'x.patch'\njobs:\n  a:\n    steps:\n      - run: cargo test --lib\n",
    )
    .expect("write probe workflow");
    fs::write(
        probe_dir.join("synthetic-commented.yml"),
        "# this one only MENTIONS cargo test in prose\nname: Commented\non:\n  push:\n    branches: [main]\njobs:\n  a:\n    steps:\n      - run: echo hi\n",
    )
    .expect("write probe workflow");

    let probe = classify_unfiltered_push_to_main(&probe_dir);
    let _ = fs::remove_dir_all(&probe_dir);
    assert_eq!(
        probe,
        vec![
            ("synthetic-commented.yml".to_owned(), false),
            ("synthetic-unfiltered.yml".to_owned(), true),
        ],
        "classifier probe failed: it must see the unfiltered one, skip the \
         path-filtered one, and NOT count a cargo mention that lives in a comment"
    );
}

/// Workflows whose `push:` trigger includes main with no `paths:` filter,
/// paired with whether they invoke a compiling cargo command. bd-p54ks.
///
/// Comments are stripped first. That is not incidental: ci-static.yml's header
/// contains the sentence "Clippy and cargo test stay on RCH", so a scan that
/// keeps comments reads a workflow's prose about what it does NOT do as proof
/// that it does. My first pass made exactly that mistake.
fn classify_unfiltered_push_to_main(dir: &Path) -> Vec<(String, bool)> {
    const COMPILING: [&str; 6] = [
        "cargo build",
        "cargo check",
        "cargo test",
        "cargo clippy",
        "cargo nextest",
        "cargo bench",
    ];
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yml"))
        .collect();
    paths.sort();
    for path in paths {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let body: String = raw
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        // The `on:` block runs to the next top-level key.
        let mut on_block = String::new();
        let mut in_on = false;
        for line in body.lines() {
            if in_on {
                if !line.trim().is_empty() && !line.starts_with(char::is_whitespace) {
                    break;
                }
                on_block.push_str(line);
                on_block.push('\n');
            } else if line.trim_end() == "on:" {
                in_on = true;
            }
        }
        // The `push:` sub-block runs to the next key at its own indent.
        let mut push_block = String::new();
        let mut in_push = false;
        let mut push_indent = 0usize;
        for line in on_block.lines() {
            let indent = line.len() - line.trim_start().len();
            if in_push {
                if !line.trim().is_empty() && indent <= push_indent {
                    break;
                }
                push_block.push_str(line);
                push_block.push('\n');
            } else if line.trim_start().starts_with("push:") {
                in_push = true;
                push_indent = indent;
            }
        }
        if !push_block.contains("main") || push_block.contains("paths:") {
            continue;
        }
        let compiles = COMPILING.iter().any(|needle| body.contains(needle));
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push((name, compiles));
    }
    out
}

#[test]
fn ci_workflow_uses_normal_non_benchmark_test_gate() {
    let ci_workflow =
        fs::read_to_string(project_root().join(".github/workflows/ci.yml")).expect("read ci.yml");

    // 1. Benches stay out of the normal gate. This is the original purpose of
    //    the guard (598cd6401, "fix(ci): keep benches out of normal test gate").
    assert!(
        !ci_workflow.contains(BENCH_INCLUDED_TEST_GATE),
        "CI's normal Tests step must not run `{BENCH_INCLUDED_TEST_GATE}`; \
         benches belong behind an explicit benchmark job"
    );

    // 2. The non-lib targets run together, as verify.sh runs them.
    assert!(
        ci_workflow.contains("--workspace --bins --tests --examples"),
        "CI should run the non-lib targets as one gate (--bins --tests --examples), \
         matching the tail of verify.sh's `{NORMAL_CARGO_TEST_GATE}`"
    );

    // 3. Every module in src/lib.rs is reachable by CI: either it has its own
    //    `--lib <module>::` shard, or it is a named exemption that still earns
    //    the exemption. This is the coverage `--workspace --lib` gave for free
    //    before the split, re-derived per module so a new module cannot be
    //    added to lib.rs and silently miss CI.
    let mut unsharded = Vec::new();
    for (module, cfg_gated) in lib_modules() {
        if ci_workflow.contains(&format!("--lib --jobs 1 {module}::")) {
            continue;
        }
        match LIB_SHARD_EXEMPT
            .iter()
            .find(|(exempt, _)| *exempt == module.as_str())
        {
            Some((_, covering_step)) => {
                assert!(
                    cfg_gated,
                    "module `{module}` is exempt from `--lib` sharding only because it is \
                     feature-gated and a default `--lib` run cannot reach it. It is no longer \
                     gated, so it needs a real shard in ci.yml."
                );
                assert!(
                    ci_workflow.contains(covering_step),
                    "exempt module `{module}` is only exempt while ci.yml still runs \
                     `{covering_step}`; that step is gone, so `{module}` now runs in no job"
                );
            }
            None => unsharded.push(module),
        }
    }

    assert!(
        unsharded.is_empty(),
        "these src/lib.rs modules have no CI `--lib` shard and no named exemption, \
         so their unit tests run in no job: {unsharded:?}"
    );
}

#[test]
fn agent_docs_match_normal_non_benchmark_test_gate() {
    let agent_docs = fs::read_to_string(project_root().join("AGENTS.md")).expect("read AGENTS.md");

    assert!(
        agent_docs.contains(NORMAL_CARGO_TEST_GATE),
        "AGENTS.md should document the central verifier's non-benchmark test gate"
    );
    assert!(
        !agent_docs.contains(BENCH_INCLUDED_TEST_GATE),
        "AGENTS.md should not document `{BENCH_INCLUDED_TEST_GATE}` as the normal verify test gate"
    );
    assert!(
        agent_docs.contains("--include-bench")
            && agent_docs.contains("./scripts/bench_perf_regression.sh"),
        "AGENTS.md should point benchmark verification at the explicit benchmark gate"
    );
}

/// Runs the REAL ruby-availability call sites with the REAL `record_gated_off`.
///
/// `RUBY_MISSING` is the status the stubbed `command -v ruby` returns, so both
/// hosts can be exercised from either host. `run_stage` is stubbed to record an
/// ATTEMPT rather than to run anything: what is under test is which of the three
/// buckets a ruby-less host lands in, not what the ruby scripts do.
///
/// Extracts the `if command -v ruby ...; then ... fi` blocks verbatim rather
/// than restating them, so a regression that reverted the call sites to the old
/// `run_stage "..." "ruby_gate_or_skip ..."` wrapper shape is caught here.
fn ruby_gate_accounting(ruby_missing: bool) -> (String, i32) {
    let output = Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -uo pipefail
STAGE_RESULTS=""
STAGE_PASSED=0
STAGE_GATED_OFF=0
STAGE_GATED_OFF_NAMES=""
command() {
    if [ "$1" = "-v" ] && [ "$2" = "ruby" ]; then
        return "$RUBY_MISSING"
    fi
    builtin command "$@"
}
run_stage() {
    STAGE_PASSED=$((STAGE_PASSED + 1))
    STAGE_RESULTS="${STAGE_RESULTS}ATTEMPTED ${1}\n"
}
eval "$(awk '/^record_gated_off\(\) /,/^}/' "$VERIFY_SCRIPT")"
eval "$(awk '/^if command -v ruby >\/dev\/null 2>&1; then$/,/^fi$/' "$VERIFY_SCRIPT")"
printf 'passed=%s gated_off=%s\n' "$STAGE_PASSED" "$STAGE_GATED_OFF"
printf '%b' "$STAGE_GATED_OFF_NAMES"
printf '%b' "$STAGE_RESULTS"
"#,
        )
        .env("VERIFY_SCRIPT", verify_script_path())
        .env("RUBY_MISSING", if ruby_missing { "1" } else { "0" })
        .current_dir(project_root())
        .output()
        .expect("run the ruby-availability call sites");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (stdout, output.status.code().unwrap_or(-1))
}

const RUBY_PROOF_LANE_STAGES: [&str; 3] = [
    "CI Proof-Lane Snapshot Contract",
    "CI Proof-Lane Hygiene Contract",
    "CI Proof-Lane Hygiene Advisory",
];

/// A ruby-less host must report the three CI Proof-Lane stages as NOT ATTEMPTED.
///
/// Regression test for bd-ruby-gate-skip-counts-as-passed-jb56c. The wrapper it
/// replaced returned 0 from inside `run_stage` when ruby was absent, so a stage
/// that never executed was counted in STAGE_PASSED and appeared in the banner as
/// one of the "attempted stages passed".
///
/// Control, measured against the pre-fix call sites
/// (`git show fe287b0a6^:scripts/verify.sh`) with the same ruby-less stub:
/// `passed=3 gated_off=0`, all three reported PASS. After the fix:
/// `passed=0 gated_off=3`, each named with its reason.
#[test]
fn a_ruby_less_host_gates_the_proof_lane_stages_off_instead_of_passing_them() {
    let (stdout, code) = ruby_gate_accounting(true);
    assert_eq!(code, 0, "the call sites must not error:\n{stdout}");
    assert!(
        stdout.contains("passed=0"),
        "a stage that never ran must not be counted in STAGE_PASSED:\n{stdout}"
    );
    assert!(
        stdout.contains("gated_off=3"),
        "all three CI Proof-Lane stages must land in the gated-off bucket:\n{stdout}"
    );
    for stage in RUBY_PROOF_LANE_STAGES {
        assert!(
            stdout.contains(&format!("- {stage} (ruby unavailable on this host)")),
            "the banner must NAME {stage} and say why it did not run:\n{stdout}"
        );
    }
}

/// The paired positive: a ruby-capable host must still attempt all three.
///
/// Without it, deleting the call sites outright would satisfy the test above.
#[test]
fn a_ruby_capable_host_still_attempts_the_proof_lane_stages() {
    let (stdout, code) = ruby_gate_accounting(false);
    assert_eq!(code, 0, "the call sites must not error:\n{stdout}");
    assert!(
        stdout.contains("passed=3") && stdout.contains("gated_off=0"),
        "ruby being present must attempt every proof-lane stage:\n{stdout}"
    );
    for stage in RUBY_PROOF_LANE_STAGES {
        assert!(
            stdout.contains(&format!("ATTEMPTED {stage}")),
            "{stage} must be attempted when ruby is available:\n{stdout}"
        );
    }
}

/// The repaired shape must stay repaired.
///
/// `ruby_gate_or_skip` could not be fixed in place: `run_stage` runs its command
/// in a pipeline, therefore a subshell, so a wrapper calling `record_gated_off`
/// from inside the command would have its counter increments discarded. The
/// decision has to be made BEFORE `run_stage` is entered. Reintroducing any
/// wrapper that decides availability inside the staged command would silently
/// restore the defect, so pin the absence by name and pin the three call sites
/// to the guarded form.
#[test]
fn no_stage_wrapper_decides_availability_inside_run_stage() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let offenders: Vec<&str> = script
        .lines()
        .filter(|line| line.trim_start().starts_with("run_stage "))
        .filter(|line| line.contains("_or_skip"))
        .collect();
    assert!(
        offenders.is_empty(),
        "a stage command that decides its own availability is counted as PASSED \
         when it declines to run; decide before run_stage instead. Offending \
         call sites:\n{}",
        offenders.join("\n")
    );

    for stage in RUBY_PROOF_LANE_STAGES {
        assert!(
            script.contains(&format!(
                "record_gated_off \"{stage}\" \"ruby unavailable on this host\""
            )),
            "{stage} must route a ruby-less host through record_gated_off"
        );
    }
}

/// Every name `record_gated_off` can emit, paired with the line it came from.
fn gated_off_registrations(script: &str) -> Vec<(usize, String)> {
    script
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let marker = "record_gated_off \"";
            let start = line.find(marker)? + marker.len();
            let rest = &line[start..];
            let end = rest.find('"')?;
            Some((index + 1, rest[..end].to_string()))
        })
        .collect()
}

/// `run_stage` call sites that are indented, i.e. reached only on one branch of
/// a conditional, paired with the line they came from.
fn conditional_stage_sites(script: &str) -> Vec<(usize, String)> {
    script
        .lines()
        .enumerate()
        .filter(|(_, line)| line.starts_with(char::is_whitespace))
        .filter_map(|(index, line)| {
            let marker = "run_stage \"";
            let start = line.find(marker)? + marker.len();
            let rest = &line[start..];
            let end = rest.find('"')?;
            Some((index + 1, rest[..end].to_string()))
        })
        .collect()
}

/// The banner's denominator must cover the whole stage population.
///
/// `verification_summary_banner` computes `declared` as
/// `STAGE_PASSED + STAGE_SKIPPED_CONTENTION + STAGE_GATED_OFF` -- a TALLY of
/// what the run accounted for, not a count of what exists. That is honest only
/// while every stage lands in exactly one of those three buckets on every path.
/// Two edits would quietly break it and leave the banner still reading as a
/// full sweep:
///
///   1. a `record_gated_off` for a name that is not a stage, inflating `declared`
///      past the real population;
///   2. a `run_stage` on one branch of a conditional whose other branch neither
///      runs a stage of that name nor records it as gated off, so the stage
///      vanishes from all three counters.
///
/// Measured at the time this test was written: 112 distinct stage names, 23
/// distinct gated-off names, ALL 23 of which are stage names; 24 distinct
/// conditionally-reached stage names, of which exactly one -- "Native Reranker
/// E2E (bd-1nl13.14)" -- has no gated-off record, and that one is covered
/// because BOTH branches run a stage of that name. So the invariant holds today
/// and nothing was asserting it.
///
/// Conditional reachability is detected by indentation, which is how the
/// population above was measured; a `run_stage` at column 0 runs on every path
/// that reaches the banner.
#[test]
fn the_banner_denominator_covers_every_declared_stage() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let stage_names = verify_stage_names(&script);
    assert!(
        stage_names.len() > 100,
        "expected the full stage population; found {} -- the parser, not the \
         script, is the likely fault",
        stage_names.len()
    );

    let phantoms: Vec<String> = gated_off_registrations(&script)
        .into_iter()
        .filter(|(_, name)| !stage_names.contains(name))
        .map(|(line, name)| format!("  verify.sh:{line}  {name}"))
        .collect();
    assert!(
        phantoms.is_empty(),
        "record_gated_off names a stage that does not exist, so `declared` counts \
         something no run_stage could ever have attempted:\n{}",
        phantoms.join("\n")
    );

    let gated: BTreeSet<String> = gated_off_registrations(&script)
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let sites = conditional_stage_sites(&script);
    let mut uncounted: Vec<String> = Vec::new();
    for (line, name) in &sites {
        if gated.contains(name) {
            continue;
        }
        // A sibling branch running a stage of the same name also keeps it in
        // the denominator on every path.
        let occurrences = sites.iter().filter(|(_, other)| other == name).count();
        if occurrences > 1 {
            continue;
        }
        uncounted.push(format!("  verify.sh:{line}  {name}"));
    }
    assert!(
        uncounted.is_empty(),
        "these stages are reached on only one branch and are neither recorded as \
         gated off nor run on the other, so a run that takes that branch drops \
         them out of `declared` entirely and the banner still reads as a full \
         sweep:\n{}",
        uncounted.join("\n")
    );
}

/// The double-quoted runs of a shell line, with `\"` kept as literal text.
///
/// `run_stage "<name>" "<command>"` puts the command in the LAST run. Splitting
/// naively on `"` would cut a command like `"\"${VAR}\" eval run ..."` in half
/// and hide exactly the pinned form this check must recognise.
fn shell_quoted_runs(line: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => {
                escaped = true;
                if in_quote {
                    current.push(ch);
                }
            }
            '"' if in_quote => {
                runs.push(std::mem::take(&mut current));
                in_quote = false;
            }
            '"' => in_quote = true,
            _ => {
                if in_quote {
                    current.push(ch);
                }
            }
        }
    }
    runs
}

/// The executable a staged command would run, with leading `NAME=value`
/// environment assignments stripped the way the shell strips them.
fn staged_command_head(command: &str) -> Option<String> {
    let mut tokens = command.split_whitespace();
    for token in tokens.by_ref() {
        let is_env_assignment = token.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if !is_env_assignment {
            return Some(token.to_owned());
        }
    }
    None
}

/// No verify.sh stage may resolve the BINARY UNDER TEST through PATH.
///
/// Regression test for bd-smxdr. Gate 8.76 used to read
///
///     run_stage "Ask Eval Quality Gate (bd-169v0.4)" "ee eval run ask_v1 --json"
///
/// and `run_stage` executes its command with `eval`, so a bare `ee` resolved
/// through PATH. On the machine where this was found, `command -v ee` was a
/// 25-day-old installed build with no relationship to the tree being verified:
/// the gate graded some other `ee` and reported PASS for this one.
///
/// The distinction this asserts is narrow on purpose. `cargo` and `python3` are
/// also PATH-resolved by other stages and that is CORRECT -- they are tools.
/// `ee` is the artifact under test, and grading an artifact with a different
/// copy of itself is the defect. So only `ee` is named here.
///
/// Detector validated against the pre-fix script rather than trusted because it
/// came back clean: run over `git show 40a562a21^:scripts/verify.sh` it reports
/// exactly one offender, verify.sh:1613, the line above. Over HEAD it reports
/// none.
#[test]
fn no_verify_stage_resolves_the_ee_binary_through_path() {
    let script = fs::read_to_string(verify_script_path()).expect("read verify.sh");
    let mut offenders: Vec<String> = Vec::new();
    let mut staged = 0usize;

    for (index, line) in script.lines().enumerate() {
        if !line.trim_start().starts_with("run_stage ") {
            continue;
        }
        let runs = shell_quoted_runs(line);
        if runs.len() < 2 {
            continue;
        }
        staged += 1;
        let command = runs.last().expect("command run");
        if staged_command_head(command).as_deref() == Some("ee") {
            offenders.push(format!("  verify.sh:{}  {}", index + 1, line.trim()));
        }
    }

    assert!(
        staged > 100,
        "expected to have parsed the full stage population; parsed {staged} -- the \
         parser, not the script, is the likely fault"
    );
    assert!(
        offenders.is_empty(),
        "these stages invoke a PATH-resolved `ee`, so they grade whatever build is \
         installed rather than the tree under verification; pin them to \
         CURRENT_SOURCE_EE_BINARY:\n{}",
        offenders.join("\n")
    );
}

/// Drive the vision-coverage gate at a scratch root with a chosen report body.
///
/// `--gate=cargo-test` is deliberate: it makes the other two checks return
/// early, so the exit status is attributable to the vision check alone rather
/// than to a `cargo tree` that has no manifest in a temp dir.
fn run_vision_gate_with_report(report: &str, beads: Option<&str>) -> Output {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join(".vision-coverage-report.json"), report)
        .expect("write vision report");
    if let Some(beads) = beads {
        fs::create_dir_all(temp.path().join(".beads")).expect("create .beads");
        fs::write(temp.path().join(".beads/issues.jsonl"), beads).expect("write beads fixture");
    }
    run_drift_guard_at(temp.path(), &["--gate=cargo-test"])
}

const HEALTHY_VISION_REPORT: &str = r#"{"surfaces":{"total_documented":141,"implemented":141,"stubbed":0,"missing":0,"with_open_implements_bead":0}}"#;

/// bd-ry56h. The vision check must be able to reach all three verdicts.
///
/// It previously reached none of them. The query was
/// `.surfaces | to_entries | map(select(.value.status == "missing")) | length`,
/// but `.surfaces` is a COUNTS object, not a map of per-surface records, so
/// jq exited 5 with `Cannot index number with string "status"`. The call site
/// ended in `2>/dev/null || echo "0"`, the error was swallowed, `0` never
/// exceeded the threshold, and the gate reported clean by failing — for every
/// run, on every input.
///
/// A one-line path fix would have restored the happy path and left the
/// swallow in place, so the next time the query broke the gate would die
/// silently again. The fix is therefore fail-closed reading, and this test
/// exists to hold that: it pins the FATAL arm alongside the fire and clean
/// arms, because a gate that cannot distinguish "nothing wrong" from "could
/// not look" is not a gate.
#[test]
fn vision_coverage_gate_can_fire_can_pass_and_cannot_be_silenced() {
    // FIRE: over the threshold with nothing tracking it.
    let fires = run_vision_gate_with_report(
        r#"{"surfaces":{"total_documented":141,"implemented":132,"stubbed":0,"missing":9,"with_open_implements_bead":0}}"#,
        Some(
            r#"{"id":"bd-unrelated","title":"placeholder","status":"open","labels":[],"description":"nothing relevant"}"#,
        ),
    );
    assert_eq!(
        fires.status.code(),
        Some(1),
        "9 missing surfaces with no tracking bead must be reported as drift\n{}",
        output_excerpt(&fires)
    );
    assert!(
        String::from_utf8_lossy(&fires.stdout).contains("9 missing surfaces"),
        "the drift message must name the count it read\n{}",
        output_excerpt(&fires)
    );

    // PASS: under the threshold. Distinguishes a real clean from the old
    // accidental one, since this run actually parsed a number.
    let clean = run_vision_gate_with_report(HEALTHY_VISION_REPORT, None);
    assert_eq!(
        clean.status.code(),
        Some(0),
        "0 missing surfaces is a genuine pass\n{}",
        output_excerpt(&clean)
    );

    // FATAL: the report exists but cannot be read. This is the arm the old
    // code failed — it is exactly the state that used to yield "clean".
    for (label, body) in [
        ("truncated", r#"{"surfaces": {"total_documented": 141, "#),
        ("not json", "this is not json at all\n"),
        (
            "key absent",
            r#"{"surfaces":{"total_documented":141,"implemented":141}}"#,
        ),
        (
            "wrong shape",
            r#"{"surfaces":{"missing":{"nested":"object"}}}"#,
        ),
    ] {
        let broken = run_vision_gate_with_report(body, None);
        assert_eq!(
            broken.status.code(),
            Some(2),
            "an unreadable vision report ({label}) must fail the guard, never report clean\n{}",
            output_excerpt(&broken)
        );
        assert!(
            String::from_utf8_lossy(&broken.stderr).contains("Refusing to report a verdict"),
            "the guard must say why it refused ({label})\n{}",
            output_excerpt(&broken)
        );
    }
}

/// The swallow itself must not come back, in any of the three checks.
///
/// bd-ry56h was one instance of a shape this repo keeps rediscovering:
/// `$(... 2>/dev/null || echo "0")` turns "I could not evaluate this" into
/// "I evaluated this and it was fine". The vision check is fixed above; this
/// pins the whole script so the pattern cannot be reintroduced in a sibling
/// check and quietly kill a different gate.
#[test]
fn the_drift_guard_never_defaults_a_failed_probe_to_zero() {
    let script = fs::read_to_string(project_root().join("scripts/verification-drift-guard.sh"))
        .expect("read verification-drift-guard.sh");
    let offenders: Vec<String> = script
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let line = line.trim_start();
            !line.starts_with('#')
                && line.contains("|| echo \"0\"")
                && !line.contains("guard_input_error")
        })
        .map(|(index, line)| format!("{}: {}", index + 1, line.trim()))
        .collect();
    assert!(
        offenders.is_empty(),
        "a probe that defaults to 0 on failure reports clean without checking; \
         read it through read_json_metric (or check the producer's exit status) \
         so the guard fails instead:\n{}",
        offenders.join("\n")
    );
}

/// Where the grandfathered inventory of open-coded success assertions lives.
const OPEN_CODED_BASELINE: &str = "tests/fixtures/golden/open_coded_success_baseline.tsv";

// THERE IS NO HELPER ALLOWLIST, AND THAT IS THE DESIGN, NOT AN OMISSION.
//
// The spelling-keyed scanner needed one: `ensure_command_success` matched the
// forbidden shape, so its NAME had to be exempted or the gate would fire on
// the exact pattern it promotes -- and each test binary needs its own copy,
// being separate compilation units (usr002 and usr003 each got one in
// 43a96043c), so the list could never be closed.
//
// Keying on WHAT A SITE PRINTS removes the need entirely. A helper that prints
// all three facts classifies as complete and is simply not recorded, wherever
// it lives and however many copies exist. A helper that does NOT is recorded,
// which is correct: `parse_logged_response` and `parse_logged_external_json`
// surfaced both streams and dropped the exit code, and a name-based allowlist
// would have exempted them for looking like chokepoints while they centralized
// the defect rather than fixing it. Both were repaired in d6abbb3aa.
//
// The old `SUCCESS_ASSERTION_HELPERS` constant survived the rewrite unused and
// made 3258fd6ce born-red under `cargo clippy --all-targets -- -D warnings`
// (dead_code). Kept as a comment because the reasoning is why the allowlist is
// absent; a reader who does not find one should not conclude it was forgotten.
//
// THE NAME ABOVE IS PROSE, NOT CODE. This sentence is the only place it still
// appears, so `grep SUCCESS_ASSERTION_HELPERS` returns a hit forever and
// CANNOT tell you whether the constant is declared. Only the compiler can:
// `cargo clippy --all-targets -- -D warnings` reported it and now does not.
// A name-grep standing in for a declaration check is the same substitution
// this gate exists to catch, so it should not be how its own removal is
// verified.

/// The one file excluded from the inventory: this one.
///
/// It necessarily contains specimens of the pattern it hunts -- in the failure
/// message that teaches the right shape, and in the fixture that proves the
/// scanner fires. Counting those would make the gate report on its own test
/// data and red whenever someone improved its wording.
///
/// MEASURED before excluding it, because "it only contains specimens" is an
/// assumption that decays: all six matches here are specimens (two doc
/// comments, one assertion message, three fixture lines) and none is a real
/// assertion. This file guards with `assert!`, not `ensure()`. If that ever
/// changes, this exclusion starts hiding real sites.
const OPEN_CODED_SELF_EXCLUSION: &str = "verification_drift_guard";

/// Inventory the open-coded command-success assertions in one directory.
///
/// WHAT COUNTS. `ensure(<expr>.status.success(), <label>)`, in both the
/// multi-line and single-line spellings. Such an assertion reports only THAT a
/// command failed and discards what would say why. The five rows repaired in
/// c91baece8 and 43a96043c each lost a different half of the evidence -- one
/// surface kept the exit code and dropped stderr, another kept stderr and
/// dropped the exit code, a third kept neither -- so no cross-surface
/// hypothesis about their shared cause could even be tested.
///
/// WHAT DOES NOT COUNT, and why each exclusion is principled rather than
/// convenient:
///   - `ensure(!x.status.success(), ..)` asserts FAILURE. That is a different
///     and legitimate shape; a label there is describing an expected failure,
///     not discarding a diagnostic.
///   - `if`/`let`/`while`/`match` on `.status.success()` is control flow, not
///     an assertion, and has no failure message to carry anything.
///   - A COMPLETE assertion, wherever it lives. A helper that prints all three
///     facts is not recorded, which is why no name allowlist exists.
///
/// KEY SHAPE: `<file stem>::<enclosing fn>`, counted. Per enclosing function
/// rather than per file is deliberate -- a per-file count cannot show a
/// compensating change, and this repo has already read "37 -> 36, within
/// spread" as noise when it hid two failures out and one in.
///
/// WHAT THIS CANNOT SEE, written down because a gate whose limits are unstated
/// gets trusted past them: it cannot detect one open-coded assertion REPLACING
/// another inside the SAME function. The count is unchanged and the swap is
/// invisible. This stops the population from GROWING. That is the entire claim.
/// How many lines after an assertion's first line may belong to it.
///
/// Every spelling in this tree closes within a few lines. A window rather than
/// a paren-matcher because the classifier only needs to know WHICH TOKENS the
/// failure text mentions, not to parse Rust -- and a wrong window makes a site
/// look less complete than it is, which fails toward recording debt rather
/// than toward hiding it.
const ASSERTION_WINDOW: usize = 14;

/// The exact source text of ONE assertion, bounded by bracket depth.
///
/// A FIXED LINE WINDOW CANNOT BOUND AN ASSERTION, and trying cost two wrong
/// classifications before this existed. It bled past the function boundary and
/// borrowed the next function's tokens; bounding it at the boundary then still
/// bled across ADJACENT STATEMENTS, so an assertion printing nothing was read
/// as printing stderr because the next statement did. Both times the counts
/// looked plausible and every affected row recorded the wrong deficiency.
///
/// Depth matching is exact instead: walk from the construct's opening bracket
/// until it closes. `if cond { .. }` ends at its matching brace; `ensure( .. )`
/// ends when the parens balance, on one line or five.
///
/// The start backs up one line when the previous line opened an `ensure(`,
/// because for a multi-line ensure the CANDIDATE line is the condition, which
/// sits inside the construct rather than at its head.
fn assertion_window(lines: &[&str], index: usize) -> String {
    let start = if index > 0 && lines[index - 1].trim().ends_with("ensure(") {
        index - 1
    } else {
        index
    };
    let hard_end = (start + ASSERTION_WINDOW).min(lines.len());
    let mut depth: i32 = 0;
    let mut opened = false;
    let mut end = start;
    while end < hard_end {
        for ch in lines[end].chars() {
            match ch {
                '(' | '{' | '[' => {
                    depth += 1;
                    opened = true;
                }
                ')' | '}' | ']' => depth -= 1,
                _ => {}
            }
        }
        end += 1;
        if opened && depth <= 0 {
            break;
        }
    }
    lines[start..end].join("\n")
}

/// Does `window` mention `token` as a WHOLE WORD?
///
/// `stdout_json(&init, "init")` contains the substring `stdout` and prints
/// nothing of the sort. A naive `contains` read that helper's NAME as evidence
/// the assertion surfaces stdout, marking incomplete sites complete -- the
/// failure direction that hides debt rather than inventing it.
fn mentions_token(window: &str, token: &str) -> bool {
    window.match_indices(token).any(|(at, _)| {
        let before_ok = at == 0
            || !window[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = &window[at + token.len()..];
        let after_ok = !after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        before_ok && after_ok
    })
}

/// Which of the three diagnostic facts a failing assertion will carry.
///
/// THIS IS THE KEY THE GATE IS BUILT ON, and it is deliberately NOT the
/// spelling. Spelling is orthogonal to quality: `require_ok` compared
/// `status.code()` and printed the exit code and stderr, while
/// `ensure_equal(&x.status.code(), &Some(0), "init exit")` uses the same
/// comparison and prints neither stream. Keying on syntax would grandfather
/// 281 sites of unknown quality, condemning some already adequate and blessing
/// some that are not. Keying on the PROPERTY -- does a failing assertion say
/// why -- is what makes bd-wq41r's deletion condition true rather than
/// aspirational, because it cannot be evaded by changing spelling.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
struct DiagnosticFacts {
    code: bool,
    stdout: bool,
    stderr: bool,
}

impl DiagnosticFacts {
    fn complete(self) -> bool {
        self.code && self.stdout && self.stderr
    }

    /// The missing set, rendered stably for the baseline and for failure text.
    fn missing(self) -> String {
        let mut out = Vec::new();
        if !self.code {
            out.push("code");
        }
        if !self.stdout {
            out.push("stdout");
        }
        if !self.stderr {
            out.push("stderr");
        }
        out.join("+")
    }
}

/// Inventory the success assertions under `tests/` that cannot explain a failure.
///
/// WHAT COUNTS AS A SUCCESS ASSERTION, in either spelling:
///   `ensure(<expr>.status.success(), <msg>)`
///   `ensure_equal(&<expr>.status.code(), &Some(0|EXIT_SUCCESS), <label>)`
///   `if <expr>.status.code() == Some(0|EXIT_SUCCESS) { .. } else { .. Err(..) }`
///
/// WHAT DOES NOT, and why each exclusion is principled:
///   - `!<expr>.status.success()` and comparisons against a NON-success code
///     (`Some(10)`) assert FAILURE. That is a legitimate and different shape.
///   - Control flow with no `Err(` in its window is not an assertion at all and
///     has no failure message to carry anything.
///   - Comments. A specimen quoted in prose is not a site, and this file quotes
///     the pattern constantly.
///
/// WHAT IS RECORDED: only INCOMPLETE sites, with WHICH facts are missing. A
/// complete site is not debt and vanishes from the baseline when repaired --
/// that disappearance is the decay signal the gate exists to make visible.
///
/// An `ensure_equal` on `status.code()` counts as carrying the code because
/// ensure_equal renders `expected Some(0), got Some(130)` itself. That is how
/// the why_conformance rows showed an exit code and no streams.
///
/// WHAT THIS CANNOT SEE: one incomplete assertion REPLACING another inside the
/// same function with the same missing set. The counts are unchanged and the
/// swap is invisible. It stops the population GROWING, which is the claim.
fn scan_open_coded_success_sites(dir: &Path) -> BTreeMap<String, usize> {
    let mut sites: BTreeMap<String, usize> = BTreeMap::new();
    for (key, missing) in scan_incomplete_success_assertions(dir) {
        // The missing set is part of the KEY, not a note beside it. Repairing a
        // site from `code+stdout+stderr` to `stdout` must show up as a change,
        // and a key of the function alone would hide a partial repair behind an
        // unchanged count.
        *sites.entry(format!("{key} [{missing}]")).or_insert(0) += 1;
    }
    sites
}

/// The classifier proper: every incomplete site as `(file::fn, missing-set)`.
fn scan_incomplete_success_assertions(dir: &Path) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut files: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .collect(),
        Err(_) => return found,
    };
    files.sort();

    for path in files {
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        if stem == OPEN_CODED_SELF_EXCLUSION {
            continue;
        }
        let lines: Vec<&str> = body.split('\n').collect();
        let mut enclosing = "<file scope>".to_string();

        for (index, raw) in lines.iter().enumerate() {
            let trimmed_start = raw.trim_start();
            if raw.starts_with("fn ") || raw.starts_with("pub fn ") {
                if let Some(rest) = trimmed_start
                    .strip_prefix("pub fn ")
                    .or_else(|| trimmed_start.strip_prefix("fn "))
                {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        enclosing = name;
                    }
                }
            }

            let line = raw.trim();
            if line.starts_with("//") {
                continue;
            }
            let success_spelling = line.contains(".status.success()");
            let code_spelling = line.contains(".status.code()");
            if !success_spelling && !code_spelling {
                continue;
            }
            // ASSERTING FAILURE IS A DIFFERENT SHAPE -- BUT `!` ALONE DOES NOT
            // MEAN THAT, AND TREATING IT SO HID 153 SITES.
            //
            //   ensure(!out.status.success(), "should fail")   asserts FAILURE
            //   if !out.status.success() { return Err(..) }    asserts SUCCESS
            //
            // Both contain `!` and `.status.success()`; they mean opposite
            // things. The earlier rule skipped any line with `!`, which is the
            // negation the `if`-not idiom is BUILT from -- so every
            // success assertion written that way was silently exempt.
            //
            // Third time this session that a rule derived from the examples in
            // front of me failed to survive the population. The construct is
            // what disambiguates, not the operator.
            let negated_ensure = line.contains("ensure(!")
                || (line.starts_with('!')
                    && index > 0
                    && lines[index - 1].trim().ends_with("ensure("));
            if negated_ensure {
                continue;
            }
            // For the code spelling, a line is a SUCCESS assertion only when it
            // actually compares against success. Anything else is either a
            // comparison against a specific failure code -- a different and
            // legitimate shape -- or a `.status.code()` interpolated into a
            // MESSAGE, which is evidence, not an assertion. Without this, the
            // repaired `require_ok` shape counts twice: once for its comparison
            // and once for the exit code it prints. Its own known-positive
            // caught that.
            if !success_spelling
                && !(line.contains("Some(0)") || line.contains("Some(EXIT_SUCCESS)"))
            {
                continue;
            }

            let window = assertion_window(&lines, index);

            // An assertion has somewhere to put a message. Control flow does not.
            let asserts = window.contains("Err(")
                || line.contains("ensure(")
                || line.contains("ensure_equal(")
                || (index > 0 && lines[index - 1].trim().ends_with("ensure("));
            if !asserts {
                continue;
            }

            let facts = DiagnosticFacts {
                // ensure_equal on status.code() renders expected/got itself.
                code: window.contains(".code()"),
                stdout: mentions_token(&window, "stdout"),
                stderr: mentions_token(&window, "stderr"),
            };
            if facts.complete() {
                continue;
            }
            found.push((format!("{stem}::{enclosing}"), facts.missing()));
        }
    }
    found
}

/// The baseline's explanatory header.
///
/// Emitted by [`render_open_coded_baseline`] rather than hand-written into the
/// file, because a header that regeneration deletes is a header that survives
/// exactly until the first person uses the documented regeneration command.
const OPEN_CODED_BASELINE_HEADER: &str = "\
# INCOMPLETE SUCCESS-ASSERTION BASELINE (bd-w5bza) -- generated, do not hand-edit.
#
# WHAT A ROW IS. One row per (TEST FUNCTION, MISSING-FACT-SET), with how many
# such assertions it holds. The bracket is part of the key:
#
#     smoke::some_test [code+stdout]    3
#
# means three assertions there whose failure text carries stderr and NEITHER
# the exit code NOR stdout.
#
# KEYED ON WHAT A SITE PRINTS, NOT ON ITS SPELLING -- this is the whole design.
# Spelling is orthogonal to quality: `require_ok` compared status.code() and
# printed the exit code and stderr, while an ensure_equal on status.code() uses
# the same comparison and prints neither stream. A spelling-keyed gate
# grandfathers sites of unknown quality, condemning some already adequate and
# blessing some that are not, and is evaded by changing spelling. This one
# polices the property cared about: DOES A FAILING ASSERTION SAY WHY.
#
# UNIT CHANGE -- DO NOT COMPARE THE TOTALS.
#   bd-wq41r  367 rows / 569 sites   ONE spelling, EVERY site recorded
#   bd-w5bza  see CURRENT below      BOTH spellings, only INCOMPLETE recorded
# The number rose because coverage widened, not because the tree got worse.
# Complete sites are not debt and are not listed; a row DISAPPEARING is the
# decay signal this gate exists to make visible.
#
# RELATIONSHIP TO EARLIER FIGURES, recorded so nobody derives a fourth:
#   281  RETIRED as a target. The sites the previous gate could not SEE, which
#        motivated bd-w5bza. Superseded by the quality-keyed census.
#   174  RETIRED. Published 2026-09-19T12:50Z over THREE files only and built
#        on three miscounts -- a `grep -c` that counted a definition line, a
#        `grep -v` that matched nothing, and a substring anchored to labels
#        ending in `should succeed\"`.
#   573  an intermediate census over all of tests/*.rs, taken before comment
#        lines and this gate's own specimens were excluded.
#   156  the three-file subset of the current total (smoke 136, usr002 10,
#        usr003 10) -- what 174 was trying and failing to measure.
#
# GRANDFATHERED, NOT APPROVED. Every row is a site that reports THAT a command
# failed and discards the exit code, stdout and stderr that would say why. This
# file forbids the NEXT one; it does not bless these.
#
# BOTH SPELLINGS ARE NOW COVERED:
#     ensure(<expr>.status.success(), <msg>)
#     ensure_equal(&<expr>.status.code(), &Some(0|EXIT_SUCCESS), <label>)
#     if <expr>.status.code() == Some(0|EXIT_SUCCESS) { .. } else { .. Err(..) }
#
# HOW THE PREVIOUS GATE MISSED HALF THE TREE, kept because the mistake is
# reusable: its pattern was derived from the five rows already repaired, all of
# which happened to use `.status.success()`. THE PATTERN CAME FROM THE SAMPLE,
# NOT THE POPULATION -- and why_conformance's 50 sites, the very rows bd-2bdos
# was filed for, sat outside it.
#
# WHAT THIS GATE STILL CANNOT SEE, so nobody infers a guarantee it lacks:
#   - One incomplete assertion REPLACING another in the same function with the
#     same missing set. Counts are unchanged and the swap is invisible.
#   - Whether an interpolated value is USEFUL. It matches the tokens `code()`,
#     `stdout` and `stderr` in the assertion's window; a message that mentions
#     stderr and prints an empty one still counts as carrying it.
#   - Assertions that reach a status through a helper this classifier does not
#     read. A helper whose own body is complete is correctly not recorded, but
#     its callers are judged by the helper, not by themselves.
#
# Regenerate: UPDATE_GOLDEN=1 cargo test --test verification_drift_guard \\
#   open_coded_success_assertions_do_not_grow
";

/// Render an inventory as the baseline file's exact on-disk form.
fn render_open_coded_baseline(sites: &BTreeMap<String, usize>) -> String {
    let mut out = String::from(OPEN_CODED_BASELINE_HEADER);
    out.push_str(&format!(
        "#\n# CURRENT: {} rows (test functions), {} sites (assertions).\n\n",
        sites.len(),
        sites.values().sum::<usize>()
    ));
    for (key, count) in sites {
        out.push_str(&format!("{key}\t{count}\n"));
    }
    out
}

fn parse_open_coded_baseline(text: &str) -> BTreeMap<String, usize> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let (key, count) = line.split_once('\t')?;
            Some((key.to_string(), count.trim().parse::<usize>().ok()?))
        })
        .collect()
}

/// The gate: the open-coded population may shrink, never grow.
///
/// This is bd-wq41r's deletion condition made executable. The 571 existing
/// sites are grandfathered; what this forbids is site 572. That converts an
/// unbounded manual sweep into a bounded frontier -- the backlog decays as
/// ordinary work touches it, and nobody has to schedule 571 edits.
///
/// Removals are reported too, not just additions. A baseline that silently
/// keeps stale entries stops describing the tree, and the decay this gate is
/// supposed to make visible would become invisible instead.
#[test]
fn open_coded_success_assertions_do_not_grow() {
    let tests_dir = project_root().join("tests");
    let found = scan_open_coded_success_sites(&tests_dir);
    let baseline_path = project_root().join(OPEN_CODED_BASELINE);

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        fs::write(&baseline_path, render_open_coded_baseline(&found))
            .expect("write open-coded baseline");
        return;
    }

    let recorded =
        parse_open_coded_baseline(&fs::read_to_string(&baseline_path).unwrap_or_else(|error| {
            panic!("missing {OPEN_CODED_BASELINE}: {error}; regenerate with UPDATE_GOLDEN=1")
        }));

    // A RATCHET READS A BROKEN SCANNER AS SUCCESS. This is the empty-world
    // trap inverted, and inverted is the dangerous direction: `all()` over an
    // empty set is merely vacuously true, but here an enumeration that
    // collapses to nothing renders as several hundred REMOVALS -- and removals
    // are improvement. A scanner broken by a refactor would not look like a
    // broken gate; it would look like someone repaired the whole tree
    // overnight, and the baseline would then be regenerated to match it.
    //
    // So refuse to grade a collapse, as its own failure rather than a pass.
    let found_total: usize = found.values().sum();
    let recorded_total: usize = recorded.values().sum();
    assert!(
        !(found_total == 0 && recorded_total > 0),
        "the scanner found ZERO open-coded sites while the baseline records \
         {recorded_total} across {} keys. That is not a repaired tree, it is a \
         broken enumeration -- the likeliest cause is a change to the detected \
         spelling or to the tests/ layout. REFUSING TO GRADE. Fix the scanner; \
         do not regenerate the baseline.",
        recorded.len()
    );
    assert!(
        found_total * 2 >= recorded_total,
        "the scanner found {found_total} open-coded sites against a baseline of \
         {recorded_total} -- a collapse of more than half in one step. A ratchet \
         reads that as improvement, which is exactly how a broken scanner gets \
         ratified into the baseline. REFUSING TO GRADE.\n\
         If the tree really was repaired this much, regenerate deliberately with \
         UPDATE_GOLDEN=1 and say so in the commit message; this gate will not \
         infer it for you."
    );

    // PRINT THE HITS, never a count. A bare "572 > 571" tells the next reader
    // that something grew and nothing about where to look.
    let mut grew: Vec<String> = Vec::new();
    let mut shrank: Vec<String> = Vec::new();
    for (key, count) in &found {
        let was = recorded.get(key).copied().unwrap_or(0);
        if *count > was {
            grew.push(format!("  {key}: {was} -> {count}"));
        } else if *count < was {
            shrank.push(format!("  {key}: {was} -> {count}"));
        }
    }
    for (key, was) in &recorded {
        if !found.contains_key(key) {
            shrank.push(format!("  {key}: {was} -> 0 (gone)"));
        }
    }

    assert!(
        grew.is_empty(),
        "new success assertion(s) under tests/ that cannot explain a failure.\n\
         The bracket names WHAT IS MISSING from the failure text, so a site \
         marked [code+stdout] will print stderr and neither the exit code nor \
         stdout.\n\
         Why it matters: such an assertion reports THAT a command failed and \
         discards what says why. Under `--json` the ee.error.v2 envelope goes \
         to STDOUT while stderr stays empty, and exit 130 is \
         Outcome::Cancelled, so omitting any one of the three can make a \
         cancellation indistinguishable from a rejection. Five rows in \
         bd-hwye2 cost a fleet dispatch each for exactly that reason.\n\
         FIX: print all three. `ensure_command_success(&output, \"context\")` \
         does it for the ensure spelling; for the `status.code()` spelling add \
         the two streams to the message you already build.\n\
         NOT A DEFECT, and spell it this way so the gate agrees: asserting a \
         command FAILED is `ensure(!x.status.success(), ..)`, and asserting a \
         SPECIFIC failure code is a comparison against that code. Neither is \
         reported here. Note `if !x.status.success() {{ Err(..) }}` asserts \
         SUCCESS and IS reported.\n\
         Grew:\n{}\n\
         (bd-w5bza. Existing sites are grandfathered in {OPEN_CODED_BASELINE}.)",
        grew.join("\n")
    );

    assert!(
        shrank.is_empty(),
        "open-coded assertion sites were REPAIRED but the baseline still lists \
         them. That is good news the gate cannot accept silently: a stale \
         baseline stops describing the tree and hides the decay this gate \
         exists to make visible.\n\
         Regenerate with `UPDATE_GOLDEN=1 cargo test --test \
         verification_drift_guard open_coded_success_assertions_do_not_grow`.\n\
         Shrank:\n{}",
        shrank.join("\n")
    );
}

/// Prove the gate can fail before trusting it to pass.
///
/// A gate whose failing arm has never fired is an unvalidated instrument, and
/// a clean reading from one of those is the least trustworthy reading there
/// is. This fires the scanner at a known positive and a known negative in a
/// fixture directory, so a refactor that quietly stops matching is caught by
/// the ADDITION arm going silent rather than by someone noticing years later.
#[test]
fn the_open_coded_success_gate_can_actually_fail() {
    let dir = std::env::temp_dir().join(format!(
        "ee_open_coded_gate_{}_{}",
        std::process::id(),
        line!()
    ));
    fs::create_dir_all(&dir).expect("create fixture dir");
    let fixture = dir.join("subject.rs");
    fs::write(
        &fixture,
        r#"
fn offending_multiline() -> TestResult {
    ensure(
        output.status.success(),
        format!("context should succeed; stderr: {stderr}"),
    )?;
}

fn offending_singleline() -> TestResult {
    ensure(context.output.status.success(), "context should succeed")?;
}

fn legitimate_failure_assertion() -> TestResult {
    ensure(!output.status.success(), "perf compare should fail")?;
}

fn control_flow_is_not_an_assertion() -> TestResult {
    let first = if output.status.success() { "a" } else { "b" };
}

fn already_repaired() -> TestResult {
    ensure_command_success(&output, "context")?;
}

fn code_spelling_printing_nothing() -> TestResult {
    ensure_equal(&init.status.code(), &Some(0), "init exit")?;
}

fn code_spelling_printing_two_of_three() -> TestResult {
    if output.status.code() == Some(EXIT_SUCCESS) {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{label} got {:?}; stderr: {stderr}", output.status.code()))
    }
}

fn asserting_a_specific_failure_code() -> TestResult {
    ensure(output.status.code() == Some(10), "must exit 10")?;
}

fn if_not_idiom_asserts_success() -> TestResult {
    if !output.status.success() {
        return Err(format!("{label} failed; stderr: {stderr}"));
    }
}

fn ensure_command_success(output: &Output, context: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{context}: got exit {:?}; stdout: {stdout}; stderr: {stderr}",
            output.status.code()
        ),
    )
}
"#,
    )
    .expect("write fixture");

    let found = scan_open_coded_success_sites(&dir);
    let _ = fs::remove_file(&fixture);
    let _ = fs::remove_dir(&dir);

    // KNOWN POSITIVES. Each asserts the MISSING SET, not merely that something
    // was found -- a classifier that detects every site but mislabels what it
    // lacks would pass a presence-only check while making the baseline lie.
    for (key, why) in [
        (
            "subject::offending_multiline [code+stdout]",
            "prints stderr only",
        ),
        (
            "subject::offending_singleline [code+stdout+stderr]",
            "a bare literal prints nothing",
        ),
        (
            "subject::code_spelling_printing_nothing [stdout+stderr]",
            "ensure_equal renders the code, and no stream",
        ),
        (
            "subject::code_spelling_printing_two_of_three [stdout]",
            "the require_ok shape: code and stderr, no stdout",
        ),
        (
            "subject::if_not_idiom_asserts_success [code+stdout]",
            "`if !success() { Err }` asserts SUCCESS and must NOT be exempt",
        ),
    ] {
        assert_eq!(
            found.get(key),
            Some(&1),
            "{key} must be reported ({why}); found: {found:?}"
        );
    }

    // KNOWN NEGATIVES: each exclusion must hold, or the gate reports offenders
    // it has no business reporting and gets switched off. Matched by PREFIX
    // because the missing-set suffix must not let a mislabelled row slip past.
    for exempt in [
        "subject::legitimate_failure_assertion",
        "subject::control_flow_is_not_an_assertion",
        "subject::already_repaired",
        "subject::asserting_a_specific_failure_code",
        "subject::ensure_command_success",
    ] {
        assert!(
            !found.keys().any(|key| key.starts_with(exempt)),
            "{exempt} must not be reported; found: {found:?}"
        );
    }
}
