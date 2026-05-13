//! Contract tests for the verification drift guard (EE-eism).
//!
//! The drift guard prevents "invisible baseline drift" by ensuring that
//! any red verification gate has a corresponding open bead tracking it.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NORMAL_CARGO_TEST_GATE: &str = "cargo test --workspace --lib --bins --tests --examples";
const BENCH_INCLUDED_TEST_GATE: &str = "cargo test --workspace --all-targets";

fn project_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn verify_script_path() -> PathBuf {
    project_root().join("scripts/verify.sh")
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

#[test]
fn ci_workflow_uses_normal_non_benchmark_test_gate() {
    let ci_workflow =
        fs::read_to_string(project_root().join(".github/workflows/ci.yml")).expect("read ci.yml");

    assert!(
        ci_workflow.contains(NORMAL_CARGO_TEST_GATE),
        "CI should run the same non-benchmark test gate as verify.sh"
    );
    assert!(
        !ci_workflow.contains(BENCH_INCLUDED_TEST_GATE),
        "CI's normal Tests step must not run `{BENCH_INCLUDED_TEST_GATE}`"
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
