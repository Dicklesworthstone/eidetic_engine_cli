//! Regression harness for scripts/closure-lint.sh.
//!
//! The closure linter is a CI gate, so these tests execute the shell script
//! against synthetic workspaces instead of duplicating its logic in Rust.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

type TestResult = Result<(), String>;

const GRAPH_SCHEMA_DOCS: &[&str] = &[
    "ee.insights.v1",
    "ee.context.pack_dna.v1",
    "ee.why.causal.v1",
    "ee.health.structural.v1",
    "ee.status.skyline.v1",
    "ee.memory.impact_analysis.v1",
    "ee.proximity.v1",
    "ee.why.v1",
    "ee.context.v1",
];

fn ensure(actual: bool, context: impl AsRef<str>) -> TestResult {
    if actual {
        Ok(())
    } else {
        Err(context.as_ref().to_owned())
    }
}

fn ensure_eq<T>(actual: T, expected: T, context: impl AsRef<str>) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{}: expected {expected:?}, got {actual:?}",
            context.as_ref()
        ))
    }
}

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("closure-lint.sh")
}

fn write_workspace(
    root: &Path,
    issues: &[&str],
    cli_mod: &str,
    golden_surfaces: &[&str],
) -> TestResult {
    fs::create_dir_all(root.join(".beads")).map_err(|error| format!("create .beads: {error}"))?;
    fs::create_dir_all(root.join("src").join("cli"))
        .map_err(|error| format!("create src/cli: {error}"))?;
    fs::create_dir_all(root.join("tests").join("golden"))
        .map_err(|error| format!("create tests/golden: {error}"))?;
    fs::create_dir_all(root.join("docs").join("schemas"))
        .map_err(|error| format!("create docs/schemas: {error}"))?;
    fs::create_dir_all(root.join("tests").join("snapshots"))
        .map_err(|error| format!("create tests/snapshots: {error}"))?;

    fs::write(
        root.join(".beads").join("issues.jsonl"),
        issues.join("\n") + "\n",
    )
    .map_err(|error| format!("write issues.jsonl: {error}"))?;
    fs::write(root.join("src").join("cli").join("mod.rs"), cli_mod)
        .map_err(|error| format!("write src/cli/mod.rs fixture: {error}"))?;

    for surface in golden_surfaces {
        fs::write(
            root.join("tests")
                .join("golden")
                .join(format!("{surface}.snap")),
            format!("snapshot for {surface}\n"),
        )
        .map_err(|error| format!("write golden for {surface}: {error}"))?;
    }
    for schema in GRAPH_SCHEMA_DOCS {
        fs::write(
            root.join("docs")
                .join("schemas")
                .join(format!("{schema}.json")),
            "{}\n",
        )
        .map_err(|error| format!("write schema doc for {schema}: {error}"))?;
        let snapshot_name = schema.replace('.', "_");
        fs::write(
            root.join("tests")
                .join("snapshots")
                .join(format!("graph_schemas_v1__{snapshot_name}.snap")),
            format!("snapshot for {schema}\n"),
        )
        .map_err(|error| format!("write schema snapshot for {schema}: {error}"))?;
    }
    Ok(())
}

fn write_text_file(root: &Path, relative: &str, contents: &str) -> TestResult {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create parent directory for {}: {error}", path.display()))?;
    }
    fs::write(&path, contents).map_err(|error| format!("write {}: {error}", path.display()))
}

fn run_linter(root: &Path) -> Result<(Output, Value), String> {
    run_linter_with_env(root, &[])
}

fn run_linter_with_env(root: &Path, envs: &[(&str, &str)]) -> Result<(Output, Value), String> {
    let mut command = Command::new("sh");
    command
        .arg(script_path())
        .args(["--audit", "--json"])
        .current_dir(root);

    for (key, value) in envs {
        command.env(key, value);
    }

    let output = command
        .output()
        .map_err(|error| format!("run closure-lint.sh: {error}"))?;

    let report_path = root.join(".closure-lint-report.json");
    let report = fs::read(&report_path)
        .map_err(|error| format!("read {}: {error}", report_path.display()))?;
    let report_json: Value = serde_json::from_slice(&report)
        .map_err(|error| format!("parse {}: {error}", report_path.display()))?;
    Ok((output, report_json))
}

fn flock_available() -> bool {
    Command::new("flock")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn report_status(report: &Value) -> Result<&str, String> {
    report
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| "report.status must be a string".to_owned())
}

fn report_count(report: &Value) -> Result<u64, String> {
    report
        .get("count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "report.count must be an unsigned integer".to_owned())
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("violation.{field} must be a string"))
}

fn violation_keys(report: &Value) -> Result<Vec<(String, String, String)>, String> {
    let violations = report
        .get("violations")
        .and_then(Value::as_array)
        .ok_or_else(|| "report.violations must be an array".to_owned())?;
    let mut keys = Vec::new();
    for violation in violations {
        keys.push((
            string_field(violation, "bead")?.to_owned(),
            string_field(violation, "surface")?.to_owned(),
            string_field(violation, "reason")?.to_owned(),
        ));
    }
    keys.sort();
    Ok(keys)
}

fn output_excerpt(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!(
        "status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    )
}

#[test]
fn closure_lint_reports_each_taxonomy_violation() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    write_workspace(
        temp.path(),
        &[
            r#"{"id":"closed-abstention","title":"[implements-surface:abstention-surface] closed with stub language","status":"closed","close_reason":"closed with a stub placeholder","labels":["implements-surface:abstention-surface"]}"#,
            r#"{"id":"closed-unavailable","title":"[implements-surface:demo-execution] closed with sentinel","status":"closed","close_reason":"implemented real demo execution","labels":["implements-surface:demo-execution"]}"#,
            r#"{"id":"closed-missing-golden","title":"[implements-surface:missing-golden] closed without public snapshot","status":"closed","close_reason":"implemented real surface","labels":["implements-surface:missing-golden"]}"#,
            r#"{"id":"closed-honesty-orphan","title":"honesty-only closure without implementation sibling","status":"closed","close_reason":"honest degraded surface","labels":["honesty-only","orphan"]}"#,
            r#"{"id":"closed-clean","title":"[implements-surface:clean-surface] real implementation","status":"closed","close_reason":"implemented with durable evidence","labels":["implements-surface:clean-surface"]}"#,
        ],
        "const DEMO_EXECUTION_UNAVAILABLE_CODE: &str = \"demo_execution_unavailable\";\n",
        &["abstention-surface", "demo-execution", "clean-surface"],
    )?;

    let (output, report) = run_linter(temp.path())?;
    ensure(
        !output.status.success(),
        format!(
            "linter should fail for synthetic violations\n{}",
            output_excerpt(&output)
        ),
    )?;
    ensure_eq(report_status(&report)?, "fail", "report status")?;
    ensure_eq(report_count(&report)?, 4, "report count")?;
    ensure_eq(
        violation_keys(&report)?,
        vec![
            (
                "closed-abstention".to_owned(),
                "abstention-surface".to_owned(),
                "close_reason contains abstention language".to_owned(),
            ),
            (
                "closed-honesty-orphan".to_owned(),
                "unknown".to_owned(),
                "no implements-surface sibling matches this bead's surface labels".to_owned(),
            ),
            (
                "closed-missing-golden".to_owned(),
                "missing-golden".to_owned(),
                "missing tests/golden/missing-golden.snap".to_owned(),
            ),
            (
                "closed-unavailable".to_owned(),
                "demo-execution".to_owned(),
                "DEMO_EXECUTION_UNAVAILABLE_CODE still exists in src/cli/mod.rs".to_owned(),
            ),
        ],
        "violations should match one fixture per closure-lint rule",
    )
}

#[test]
fn closure_lint_accepts_clean_implementation_and_honesty_sibling() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    write_workspace(
        temp.path(),
        &[
            r#"{"id":"closed-clean","title":"[implements-surface:clean-surface] real implementation","status":"closed","close_reason":"implemented with durable evidence","labels":["implements-surface:clean-surface"]}"#,
            r#"{"id":"closed-sentinel-removed","title":"[implements-surface:sentinel-clean] real implementation with removed sentinel","status":"closed","close_reason":"SENTINEL_CLEAN_UNAVAILABLE_CODE deleted; real implementation shipped","labels":["implements-surface:sentinel-clean"]}"#,
            r#"{"id":"closed-honesty-implemented","title":"honesty-only procedure closure followed by implementation","status":"closed","close_reason":"honest degraded closure","labels":["honesty-only","procedure"]}"#,
            r#"{"id":"closed-procedure","title":"[implements-surface:procedure] real implementation sibling","status":"closed","close_reason":"implemented procedure store","labels":["implements-surface:procedure"]}"#,
            r#"{"id":"closed-honesty","title":"honesty-only tripwire closure","status":"closed","close_reason":"honest degraded closure","labels":["honesty-only","tripwire"]}"#,
            r#"{"id":"open-tripwire","title":"[implements-surface:tripwire-deeper] open implementation sibling","status":"open","labels":["implements-surface:tripwire-deeper"]}"#,
        ],
        "",
        &["clean-surface", "sentinel-clean", "procedure"],
    )?;

    let (output, report) = run_linter(temp.path())?;
    ensure(
        output.status.success(),
        format!(
            "linter should pass clean fixtures\n{}",
            output_excerpt(&output)
        ),
    )?;
    ensure_eq(report_status(&report)?, "pass", "report status")?;
    ensure_eq(report_count(&report)?, 0, "report count")?;
    ensure_eq(
        violation_keys(&report)?,
        Vec::<(String, String, String)>::new(),
        "no violations",
    )
}

#[test]
fn closure_lint_validates_referenced_test_paths_and_assertions() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    write_workspace(
        temp.path(),
        &[
            r#"{"id":"closed-real-test","title":"[implements-surface:real-test] real implementation","status":"closed","description":"Tests: tests/closure_lint_test_files_fixtures/real_test_exists.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:real-test"]}"#,
            r#"{"id":"closed-missing-test","title":"[implements-surface:missing-test] real implementation","status":"closed","description":"Tests: tests/closure_lint_test_files_fixtures/missing_test.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:missing-test"]}"#,
            r#"{"id":"closed-glob-test","title":"[implements-surface:glob-test] real implementation","status":"closed","description":"Tests: tests/closure_lint_test_files_fixtures/glob_*.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:glob-test"]}"#,
            r#"{"id":"closed-empty-test","title":"[implements-surface:empty-test] real implementation","status":"closed","description":"Tests: tests/closure_lint_test_files_fixtures/empty_test.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:empty-test"]}"#,
            r#"{"id":"closed-ignored-test","title":"[implements-surface:ignored-test] real implementation","status":"closed","description":"Tests: tests/closure_lint_test_files_fixtures/ignored_test.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:ignored-test"]}"#,
        ],
        "",
        &[
            "real-test",
            "missing-test",
            "glob-test",
            "empty-test",
            "ignored-test",
        ],
    )?;
    write_text_file(
        temp.path(),
        "tests/closure_lint_test_files_fixtures/real_test_exists.rs",
        "#[test]\nfn real_test_exists() {\n    assert!(true);\n}\n",
    )?;
    write_text_file(
        temp.path(),
        "tests/closure_lint_test_files_fixtures/glob_match.rs",
        "#[test]\nfn glob_match() {\n    assert_eq!(1 + 1, 2);\n}\n",
    )?;
    write_text_file(
        temp.path(),
        "tests/closure_lint_test_files_fixtures/empty_test.rs",
        "#[test]\nfn empty_test() {}\n",
    )?;
    write_text_file(
        temp.path(),
        "tests/closure_lint_test_files_fixtures/ignored_test.rs",
        "#[ignore]\n#[test]\nfn ignored_test() {\n    assert!(true);\n}\n",
    )?;

    let (output, report) = run_linter(temp.path())?;
    ensure(
        !output.status.success(),
        format!(
            "linter should fail missing and assertion-free test references\n{}",
            output_excerpt(&output)
        ),
    )?;
    ensure_eq(report_status(&report)?, "fail", "report status")?;
    ensure_eq(report_count(&report)?, 3, "report count")?;
    ensure_eq(
        violation_keys(&report)?,
        vec![
            (
                "closed-empty-test".to_owned(),
                "empty-test".to_owned(),
                "tests/closure_lint_test_files_fixtures/empty_test.rs lacks assertion-style coverage"
                    .to_owned(),
            ),
            (
                "closed-ignored-test".to_owned(),
                "ignored-test".to_owned(),
                "tests/closure_lint_test_files_fixtures/ignored_test.rs has no non-ignored test"
                    .to_owned(),
            ),
            (
                "closed-missing-test".to_owned(),
                "missing-test".to_owned(),
                "referenced test path missing: tests/closure_lint_test_files_fixtures/missing_test.rs"
                    .to_owned(),
            ),
        ],
        "only missing and assertion-free test references should fail",
    )
}

#[test]
fn closure_lint_requires_failure_mode_fixtures_for_part_ii_codes() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    write_workspace(
        temp.path(),
        &[
            r#"{"id":"bd-3usjw.valid","title":"[implements-surface:valid-failure-mode] real implementation","status":"closed","description":"DEGRADATION REQUIREMENT\n- code=valid_fixture_code when the substrate is stale.\n\nFILE SURFACE: tests/fixtures/failure_modes/valid_fixture_code.json","close_reason":"implemented with durable evidence","labels":["implements-surface:valid-failure-mode"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.missing","title":"[implements-surface:missing-failure-mode] real implementation","status":"closed","description":"DEGRADATION REQUIREMENT\n- code=missing_fixture_code when the substrate is missing.\n\nFILE SURFACE: src/core/example.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:missing-failure-mode"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.no-emit","title":"[implements-surface:fixture-no-emit] real implementation","status":"closed","description":"No degraded code is emitted by this read-only fixture-only surface.\n\nFILE SURFACE: tests/fixtures/failure_modes/fixture_no_emit_code.json","close_reason":"implemented with durable evidence","labels":["implements-surface:fixture-no-emit"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.stale-surface","title":"[implements-surface:stale-failure-mode-surface] real implementation","status":"closed","description":"DEGRADATION REQUIREMENT\n- code=stale_surface_code when the fixture path is omitted from the file surface.\n\nFILE SURFACE: src/core/example.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:stale-failure-mode-surface"],"parent":"bd-3usjw"}"#,
            r#"{"id":"older-clean","title":"[implements-surface:older-clean] real implementation","status":"closed","description":"DEGRADATION REQUIREMENT\n- code=older_missing_code should not be checked outside Part II.\n\nFILE SURFACE: src/core/example.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:older-clean"]}"#,
        ],
        "",
        &[
            "valid-failure-mode",
            "missing-failure-mode",
            "fixture-no-emit",
            "stale-failure-mode-surface",
            "older-clean",
        ],
    )?;
    write_text_file(
        temp.path(),
        "tests/fixtures/failure_modes/valid_fixture_code.json",
        r#"{"schema":"ee.failure_mode_fixture.v1","code":"valid_fixture_code"}"#,
    )?;
    write_text_file(
        temp.path(),
        "tests/fixtures/failure_modes/fixture_no_emit_code.json",
        r#"{"schema":"ee.failure_mode_fixture.v1","code":"fixture_no_emit_code"}"#,
    )?;
    write_text_file(
        temp.path(),
        "tests/fixtures/failure_modes/stale_surface_code.json",
        r#"{"schema":"ee.failure_mode_fixture.v1","code":"stale_surface_code"}"#,
    )?;

    let (output, report) = run_linter(temp.path())?;
    ensure(
        !output.status.success(),
        format!(
            "linter should fail missing failure-mode fixture obligations\n{}",
            output_excerpt(&output)
        ),
    )?;
    ensure_eq(report_status(&report)?, "fail", "report status")?;
    ensure_eq(report_count(&report)?, 2, "report count")?;
    ensure_eq(
        violation_keys(&report)?,
        vec![
            (
                "bd-3usjw.missing".to_owned(),
                "missing-failure-mode".to_owned(),
                "emitted degraded code missing fixture: tests/fixtures/failure_modes/missing_fixture_code.json".to_owned(),
            ),
            (
                "bd-3usjw.stale-surface".to_owned(),
                "stale-failure-mode-surface".to_owned(),
                "emitted degraded code fixture missing from FILE SURFACE: tests/fixtures/failure_modes/stale_surface_code.json".to_owned(),
            ),
        ],
        "only missing fixture and missing FILE SURFACE fixture reference should fail",
    )?;

    let violations = report
        .get("violations")
        .and_then(Value::as_array)
        .ok_or_else(|| "report.violations must be an array".to_owned())?;
    for violation in violations {
        string_field(violation, "bead_id")?;
        string_field(violation, "missing_fixture_path")?;
        string_field(violation, "emitted_code")?;
        string_field(violation, "severity")?;
    }
    Ok(())
}

#[test]
fn closure_lint_skips_when_beads_write_lock_is_held() -> TestResult {
    if !flock_available() {
        return Ok(());
    }

    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    write_workspace(
        temp.path(),
        &[
            r#"{"id":"closed-clean","title":"[implements-surface:clean-surface] real implementation","status":"closed","close_reason":"implemented with durable evidence","labels":["implements-surface:clean-surface"]}"#,
        ],
        "",
        &["clean-surface"],
    )?;

    let lock_path = temp.path().join(".beads").join(".write.lock");
    let ready_path = temp.path().join("lock-ready");
    let mut holder = Command::new("flock")
        .arg("-x")
        .arg(&lock_path)
        .arg("sh")
        .arg("-c")
        .arg("printf ready > \"$1\"; sleep 2")
        .arg("sh")
        .arg(&ready_path)
        .spawn()
        .map_err(|error| format!("start lock holder: {error}"))?;

    let deadline = Instant::now() + Duration::from_secs(2);
    while !ready_path.exists() && Instant::now() < deadline {
        if let Some(status) = holder
            .try_wait()
            .map_err(|error| format!("poll lock holder: {error}"))?
        {
            return Err(format!("lock holder exited before ready: {status}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    ensure(ready_path.exists(), "lock holder did not report ready")?;

    let linter_result = run_linter_with_env(temp.path(), &[("EE_BEADS_LOCK_WAIT_SECONDS", "0")]);
    let holder_status = holder
        .wait()
        .map_err(|error| format!("wait for lock holder: {error}"))?;
    ensure(
        holder_status.success(),
        format!("lock holder failed: {holder_status}"),
    )?;

    let (output, report) = linter_result?;
    ensure(
        output.status.success(),
        format!(
            "linter should skip successfully while write lock is held\n{}",
            output_excerpt(&output)
        ),
    )?;
    ensure_eq(report_status(&report)?, "skipped", "report status")?;
    ensure_eq(report_count(&report)?, 0, "report count")?;
    let reason = report
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(|| "report.reason must be a string".to_owned())?;
    ensure(
        reason.contains(".beads/.write.lock"),
        format!("skip reason should name the held write lock, got {reason:?}"),
    )
}
