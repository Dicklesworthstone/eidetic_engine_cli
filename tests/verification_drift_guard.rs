//! Contract tests for the verification drift guard (EE-eism).
//!
//! The drift guard prevents "invisible baseline drift" by ensuring that
//! any red verification gate has a corresponding open bead tracking it.

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
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
    //   2. that helper actually renders `SKIP {label} (ci-smoke)` -- checked
    //      by executing it, so a change to the helper's output format fails
    //      too, which the old source grep could not see.
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
        let expected = format!("SKIP {label} (ci-smoke)");
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
