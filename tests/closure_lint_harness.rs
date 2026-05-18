//! Regression harness for scripts/closure-lint.sh.
//!
//! The closure linter is a CI gate, so these tests execute the shell script
//! against synthetic workspaces instead of duplicating its logic in Rust.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::{Builder as TempDirBuilder, TempDir};

type TestResult = Result<(), String>;

/// Create a temp directory rooted under `/tmp` when running on a host that
/// has it (every RCH Linux worker), falling back to the platform default
/// otherwise. The bd-3usjw.7 hook tests added this because Mac-side
/// `tempfile::tempdir()` inherits a `TMPDIR=/Volumes/USBNVME16TB/...`
/// pointer from `~/.zshenv`; that path does not exist on the worker after
/// RCH sync and the test panics with `os error 2`. New tests in this file
/// (bd-3usjw.62, bd-3usjw.61) use the same helper so they survive the
/// Mac→Linux RCH round-trip.
fn closure_lint_worker_local_tempdir(prefix: &str) -> Result<TempDir, String> {
    let tmp_root = Path::new("/tmp");
    if tmp_root.is_dir() {
        TempDirBuilder::new()
            .prefix(prefix)
            .tempdir_in(tmp_root)
            .map_err(|error| format!("tempdir: {error}"))
    } else {
        TempDirBuilder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| format!("tempdir: {error}"))
    }
}

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

// Regression for bd-37u08: close_reason_contains_abstention must scrub
// false-positive abstention triggers before applying the regex. The five
// patterns covered: 'non-abstention' (negation), '*_UNAVAILABLE_CODE' (literal
// constant-pattern meta-reference), 'docs/degraded_code{s,_taxonomy}.md'
// (path reference to the failure-mode docs), 'degraded {case,code,mode,entry}s'
// (concept references), 'degraded_codes none' (RCH verifier status field),
// and the lowercase '[a-z_]+_unavailable' fixture/code-name convention.
//
// All five forms appear together in a synthetic close_reason; the linter
// must report 0 violations for that bead. A sixth fixture uses an actual
// abstention phrase ("closed with a stub placeholder") to prove the
// scrub doesn't disable the genuine abstention rule.
#[test]
fn closure_lint_scrubs_abstention_false_positives() -> TestResult {
    let temp = closure_lint_worker_local_tempdir("closure-lint-abstention-scrub")?;
    write_workspace(
        temp.path(),
        &[
            // Synthetic bead with close_reason carrying every documented
            // false-positive trigger. The linter must NOT flag it.
            r#"{"id":"closed-false-positive","title":"[implements-surface:false-positive-surface] real implementation with hygiene-noisy close_reason","status":"closed","close_reason":"Real implementation: returns non-abstention payloads. Documented degraded codes in docs/degraded_code_taxonomy.md and docs/degraded_codes.md, with named entries cass_unavailable and preflight_patterns_unavailable carrying full fixtures. Covered degraded cases and degraded modes in the integration suite. RCH remote proof: 9 tests passed, degraded_codes none. No *_UNAVAILABLE_CODE constant precedent existed for this surface.","labels":["implements-surface:false-positive-surface"]}"#,
            // Sibling that legitimately abstains; proves the scrub did not
            // disable the rule globally.
            r#"{"id":"closed-real-abstention","title":"[implements-surface:real-abstention-surface] closed with stub language","status":"closed","close_reason":"closed with a stub placeholder","labels":["implements-surface:real-abstention-surface"]}"#,
        ],
        "",
        &["false-positive-surface", "real-abstention-surface"],
    )?;

    let (output, report) = run_linter(temp.path())?;
    ensure(
        !output.status.success(),
        format!(
            "linter must still flag the genuine abstention sibling\n{}",
            output_excerpt(&output)
        ),
    )?;
    let keys = violation_keys(&report)?;
    let abstention_violations: Vec<_> = keys
        .iter()
        .filter(|(_, _, reason)| reason == "close_reason contains abstention language")
        .collect();
    ensure_eq(
        abstention_violations.len(),
        1,
        "exactly one bead should fail abstention check (the genuine one)",
    )?;
    ensure_eq(
        abstention_violations[0].0.as_str(),
        "closed-real-abstention",
        "the genuine abstention fixture must be the only one flagged",
    )?;
    for (bead, _, reason) in &keys {
        if bead == "closed-false-positive" && reason == "close_reason contains abstention language"
        {
            return Err(
                "bd-37u08 regression: false-positive fixture flagged as abstention".to_owned(),
            );
        }
    }
    Ok(())
}

// CLAUDE.md lists three canonical golden artifact locations: tests/golden/*.snap,
// tests/snapshots/*.snap (insta), and tests/fixtures/golden/**. The closure
// linter must accept a real insta snapshot under tests/snapshots/ as evidence
// for a closed implements-surface bead. Regression for bd-1k5cu.
#[test]
fn closure_lint_accepts_tests_snapshots_as_golden_evidence() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    write_workspace(
        temp.path(),
        &[
            r#"{"id":"closed-insta","title":"[implements-surface:insta-surface] real implementation with insta snapshot","status":"closed","close_reason":"implemented with durable evidence","labels":["implements-surface:insta-surface"]}"#,
        ],
        "",
        // Intentionally no tests/golden/insta-surface.snap; evidence lives
        // under tests/snapshots/ instead, like tests/snapshots/health_structural.snap.
        &[],
    )?;
    write_text_file(
        temp.path(),
        "tests/snapshots/insta-surface.snap",
        "---\nsource: tests/insta_surface.rs\nexpression: snapshot\n---\n\"evidence\"\n",
    )?;

    let (output, report) = run_linter(temp.path())?;
    ensure(
        output.status.success(),
        format!(
            "linter should pass when evidence lives under tests/snapshots/\n{}",
            output_excerpt(&output)
        ),
    )?;
    ensure_eq(report_status(&report)?, "pass", "report status")?;
    ensure_eq(report_count(&report)?, 0, "report count")?;
    ensure_eq(
        violation_keys(&report)?,
        Vec::<(String, String, String)>::new(),
        "tests/snapshots/<surface>.snap must satisfy surface_has_golden_snapshot",
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

// bd-3usjw.62 — unit_test_obligation_part_ii.
//
// The closure linter must reject closing an implements-surface:* bead
// whose FILE SURFACE lists src/ implementation files when none of those
// files ships inline #[cfg(test)] coverage that meets AGENTS.md L300-302
// (>=3 non-ignored #[test] fns plus assertion-style coverage). The check
// is exempt when FILE SURFACE has no src/ entries at all.

const UNIT_TESTS_HAPPY_EDGE_ERROR: &str = r#"
pub fn echo(value: u32) -> u32 { value }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        assert_eq!(echo(1), 1);
    }

    #[test]
    fn empty_or_boundary() {
        assert_eq!(echo(0), 0);
        assert_eq!(echo(u32::MAX), u32::MAX);
    }

    #[test]
    fn error_or_invalid() {
        assert_ne!(echo(2), 3);
    }
}
"#;

const UNIT_TESTS_MISSING_MOD: &str = r#"
pub fn echo(value: u32) -> u32 { value }
"#;

const UNIT_TESTS_ONLY_TWO: &str = r#"
pub fn echo(value: u32) -> u32 { value }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        assert_eq!(echo(1), 1);
    }

    #[test]
    fn boundary() {
        assert_eq!(echo(0), 0);
    }
}
"#;

const UNIT_TESTS_ALL_IGNORED: &str = r#"
pub fn echo(value: u32) -> u32 { value }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn happy_path() {
        assert_eq!(echo(1), 1);
    }

    #[test]
    #[ignore]
    fn boundary() {
        assert_eq!(echo(0), 0);
    }

    #[test]
    #[ignore]
    fn error_path() {
        assert_ne!(echo(2), 3);
    }
}
"#;

const UNIT_TESTS_THIN_REEXPORT: &str = r#"
pub use crate::other::*;
"#;

#[test]
fn closure_lint_requires_inline_unit_tests_for_part_ii_implementations() -> TestResult {
    let temp = closure_lint_worker_local_tempdir("closure-lint-unit-test-obligation-")?;
    write_workspace(
        temp.path(),
        &[
            r#"{"id":"bd-3usjw.valid-inline","title":"[implements-surface:valid-inline-surface] real implementation","status":"closed","description":"Happy/edge/error inline tests live next to the implementation.\n\nFILE SURFACE: src/core/example.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:valid-inline-surface"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.missing-mod-tests","title":"[implements-surface:missing-mod-tests-surface] real implementation","status":"closed","description":"Implementation ships without any #[cfg(test)] mod tests block.\n\nFILE SURFACE: src/core/example.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:missing-mod-tests-surface"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.fewer-than-three","title":"[implements-surface:fewer-than-three-surface] real implementation","status":"closed","description":"Inline tests module exists but only ships 2 #[test] fns, missing the happy/edge/error triad.\n\nFILE SURFACE: src/core/example.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:fewer-than-three-surface"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.all-ignored","title":"[implements-surface:all-ignored-surface] real implementation","status":"closed","description":"Inline tests module has 3 #[test] fns but every one is gated with #[ignore].\n\nFILE SURFACE: src/core/example.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:all-ignored-surface"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.multi-src","title":"[implements-surface:multi-src-surface] real implementation","status":"closed","description":"Two src files; one ships inline coverage, the other is a thin re-export module without tests. At least one satisfying file should satisfy the rule.\n\nFILE SURFACE: src/core/example.rs, src/core/reexport.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:multi-src-surface"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.no-src-surface","title":"[implements-surface:no-src-surface] real implementation","status":"closed","description":"Script-only surface; FILE SURFACE points at scripts and fixtures, not src/.\n\nFILE SURFACE: scripts/closure-lint.sh, tests/fixtures/closure_lint/unit_test_obligation_cases","close_reason":"implemented with durable evidence","labels":["implements-surface:no-src-surface"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.older-non-part-ii","title":"[implements-surface:older-non-part-ii-surface] real implementation","status":"closed","description":"Closed implementation without the bd-3usjw parent. The rule fires for every implements-surface closure regardless of Part II membership.\n\nFILE SURFACE: src/core/example.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:older-non-part-ii-surface"]}"#,
        ],
        "",
        &[
            "valid-inline-surface",
            "missing-mod-tests-surface",
            "fewer-than-three-surface",
            "all-ignored-surface",
            "multi-src-surface",
            "no-src-surface",
            "older-non-part-ii-surface",
        ],
    )?;

    // Synthetic implementation files for each declared FILE SURFACE.
    // The shared `src/core/example.rs` path is rewritten per-bead via the
    // distinct workspace temp dirs would normally — but because all beads
    // here point at the same relative path inside one workspace, we
    // collapse the cases by writing the WORST shape that still produces
    // the per-bead expected outcome below. Instead, we use distinct paths
    // per bead by overriding FILE SURFACE — but reusing the harness's
    // single-tempdir setup is simpler. So each fixture file written here
    // is the file the bead declared, named uniquely per bead.
    //
    // To avoid collision, we override the FILE SURFACE per bead with a
    // per-bead path here: re-write the issues with distinct src paths.
    // (The harness happens to accept arbitrary file existence; we just
    // need one path per case.)
    write_text_file(
        temp.path(),
        "src/core/example_valid.rs",
        UNIT_TESTS_HAPPY_EDGE_ERROR,
    )?;
    write_text_file(
        temp.path(),
        "src/core/example_missing_mod.rs",
        UNIT_TESTS_MISSING_MOD,
    )?;
    write_text_file(
        temp.path(),
        "src/core/example_only_two.rs",
        UNIT_TESTS_ONLY_TWO,
    )?;
    write_text_file(
        temp.path(),
        "src/core/example_all_ignored.rs",
        UNIT_TESTS_ALL_IGNORED,
    )?;
    write_text_file(
        temp.path(),
        "src/core/example_multi_primary.rs",
        UNIT_TESTS_HAPPY_EDGE_ERROR,
    )?;
    write_text_file(
        temp.path(),
        "src/core/example_multi_thin.rs",
        UNIT_TESTS_THIN_REEXPORT,
    )?;
    write_text_file(
        temp.path(),
        "src/core/example_older.rs",
        UNIT_TESTS_MISSING_MOD,
    )?;

    // Re-emit the issues file with per-bead distinct FILE SURFACE paths
    // so the linter checks the right file per case.
    let issues = [
        r#"{"id":"bd-3usjw.valid-inline","title":"[implements-surface:valid-inline-surface] real implementation","status":"closed","description":"Happy/edge/error inline tests live next to the implementation.\n\nFILE SURFACE: src/core/example_valid.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:valid-inline-surface"],"parent":"bd-3usjw"}"#,
        r#"{"id":"bd-3usjw.missing-mod-tests","title":"[implements-surface:missing-mod-tests-surface] real implementation","status":"closed","description":"Implementation ships without any #[cfg(test)] mod tests block.\n\nFILE SURFACE: src/core/example_missing_mod.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:missing-mod-tests-surface"],"parent":"bd-3usjw"}"#,
        r#"{"id":"bd-3usjw.fewer-than-three","title":"[implements-surface:fewer-than-three-surface] real implementation","status":"closed","description":"Inline tests module exists but only ships 2 #[test] fns, missing the happy/edge/error triad.\n\nFILE SURFACE: src/core/example_only_two.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:fewer-than-three-surface"],"parent":"bd-3usjw"}"#,
        r#"{"id":"bd-3usjw.all-ignored","title":"[implements-surface:all-ignored-surface] real implementation","status":"closed","description":"Inline tests module has 3 #[test] fns but every one is gated with #[ignore].\n\nFILE SURFACE: src/core/example_all_ignored.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:all-ignored-surface"],"parent":"bd-3usjw"}"#,
        r#"{"id":"bd-3usjw.multi-src","title":"[implements-surface:multi-src-surface] real implementation","status":"closed","description":"Two src files; one ships inline coverage, the other is a thin re-export module without tests. At least one satisfying file should satisfy the rule.\n\nFILE SURFACE: src/core/example_multi_primary.rs, src/core/example_multi_thin.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:multi-src-surface"],"parent":"bd-3usjw"}"#,
        r#"{"id":"bd-3usjw.no-src-surface","title":"[implements-surface:no-src-surface] real implementation","status":"closed","description":"Script-only surface; FILE SURFACE points at scripts and fixtures, not src/.\n\nFILE SURFACE: scripts/closure-lint.sh, tests/fixtures/closure_lint/unit_test_obligation_cases","close_reason":"implemented with durable evidence","labels":["implements-surface:no-src-surface"],"parent":"bd-3usjw"}"#,
        r#"{"id":"bd-3usjw.older-non-part-ii","title":"[implements-surface:older-non-part-ii-surface] real implementation","status":"closed","description":"Closed implementation without the bd-3usjw parent. The rule fires for every implements-surface closure regardless of Part II membership.\n\nFILE SURFACE: src/core/example_older.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:older-non-part-ii-surface"]}"#,
    ];
    fs::write(
        temp.path().join(".beads").join("issues.jsonl"),
        issues.join("\n") + "\n",
    )
    .map_err(|error| format!("rewrite issues.jsonl: {error}"))?;

    let (output, report) = run_linter(temp.path())?;
    ensure(
        !output.status.success(),
        format!(
            "linter should fail missing inline-unit-test obligations\n{}",
            output_excerpt(&output)
        ),
    )?;
    ensure_eq(report_status(&report)?, "fail", "report status")?;
    let observed = violation_keys(&report)?;
    let observed_keys: Vec<(String, String)> = observed
        .iter()
        .filter(|(_, _, reason)| {
            reason.contains("no src/ file in FILE SURFACE has #[cfg(test)] mod tests")
        })
        .map(|(bead, surface, _)| (bead.clone(), surface.clone()))
        .collect();
    let mut expected_keys: Vec<(String, String)> = vec![
        (
            "bd-3usjw.missing-mod-tests".to_owned(),
            "missing-mod-tests-surface".to_owned(),
        ),
        (
            "bd-3usjw.fewer-than-three".to_owned(),
            "fewer-than-three-surface".to_owned(),
        ),
        (
            "bd-3usjw.all-ignored".to_owned(),
            "all-ignored-surface".to_owned(),
        ),
        (
            "bd-3usjw.older-non-part-ii".to_owned(),
            "older-non-part-ii-surface".to_owned(),
        ),
    ];
    expected_keys.sort();
    let mut observed_sorted = observed_keys.clone();
    observed_sorted.sort();
    ensure_eq(
        observed_sorted,
        expected_keys,
        "unit-test-obligation violations",
    )?;

    // The valid, multi-src-with-satisfying-file, and no-src-surface beads
    // must NOT show up under this rule.
    for clean_bead in [
        "bd-3usjw.valid-inline",
        "bd-3usjw.multi-src",
        "bd-3usjw.no-src-surface",
    ] {
        ensure(
            !observed.iter().any(|(bead, _, reason)| {
                bead == clean_bead
                    && reason.contains("no src/ file in FILE SURFACE has #[cfg(test)] mod tests")
            }),
            format!("{clean_bead} should not violate unit-test obligation"),
        )?;
    }
    Ok(())
}

// bd-3usjw.61 — audit_row_obligation_part_ii.
//
// Rule 8 validates the shape of an `AUDIT EMISSION:` block WHEN
// declared: a concrete `event_type=` literal, a `chain_continuity` or
// equivalent acceptance phrase, and an existing `*_audit.rs` test file.
// Beads that declare no block are exempt for this slice; the bulk
// retrofit + the durable_write enforcement gate are separate follow-up
// beads.

#[test]
fn closure_lint_validates_audit_emission_block_shape_when_declared() -> TestResult {
    let temp = closure_lint_worker_local_tempdir("closure-lint-audit-emission-")?;
    write_workspace(
        temp.path(),
        &[
            r#"{"id":"bd-3usjw.no-audit-block","title":"[implements-surface:no-audit-block] real implementation","status":"closed","description":"No AUDIT EMISSION marker is declared on this bead.\n\nFILE SURFACE: scripts/some.sh","close_reason":"implemented with durable evidence","labels":["implements-surface:no-audit-block"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.valid-audit","title":"[implements-surface:valid-audit] real implementation","status":"closed","description":"AUDIT EMISSION:\n- event_type=valid.surface.committed\n- chain_continuity: audit row joins ee_audit chain via prev_chain_hash and ee audit verify --json integrity_ok=true.\nTests: tests/valid_surface_audit.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:valid-audit"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.no-event-type","title":"[implements-surface:no-event-type] real implementation","status":"closed","description":"AUDIT EMISSION:\n- TODO: name the event_type literal once we agree.\n- chain_continuity: audit row joins ee_audit chain.\nTests: tests/no_event_type_surface_audit.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:no-event-type"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.no-chain","title":"[implements-surface:no-chain] real implementation","status":"closed","description":"AUDIT EMISSION:\n- event_type=missing.chain.surface\n- Will think about continuity later.\nTests: tests/no_chain_surface_audit.rs","close_reason":"implemented with durable evidence","labels":["implements-surface:no-chain"],"parent":"bd-3usjw"}"#,
            r#"{"id":"bd-3usjw.no-test-file","title":"[implements-surface:no-test-file] real implementation","status":"closed","description":"AUDIT EMISSION:\n- event_type=missing.test.surface\n- chain_continuity: chain_hash verified.\nTests: see ee audit verify integration coverage.","close_reason":"implemented with durable evidence","labels":["implements-surface:no-test-file"],"parent":"bd-3usjw"}"#,
        ],
        "",
        &[
            "no-audit-block",
            "valid-audit",
            "no-event-type",
            "no-chain",
            "no-test-file",
        ],
    )?;
    write_text_file(
        temp.path(),
        "tests/valid_surface_audit.rs",
        "#[test]\nfn covers_valid_surface_audit() {\n    assert!(true);\n}\n",
    )?;
    write_text_file(
        temp.path(),
        "tests/no_event_type_surface_audit.rs",
        "#[test]\nfn covers_no_event_type_audit() {\n    assert!(true);\n}\n",
    )?;
    write_text_file(
        temp.path(),
        "tests/no_chain_surface_audit.rs",
        "#[test]\nfn covers_no_chain_audit() {\n    assert!(true);\n}\n",
    )?;

    let (output, report) = run_linter(temp.path())?;
    ensure(
        !output.status.success(),
        format!(
            "linter should fail malformed AUDIT EMISSION blocks\n{}",
            output_excerpt(&output)
        ),
    )?;
    ensure_eq(report_status(&report)?, "fail", "report status")?;

    let observed = violation_keys(&report)?;
    let audit_observed: Vec<(String, String, String)> = observed
        .iter()
        .filter(|(_, _, reason)| reason.contains("AUDIT EMISSION block declared"))
        .cloned()
        .collect();

    // Three malformed beads each fire exactly one Rule 8 violation
    // naming the missing component.
    let mut expected = vec![
        (
            "bd-3usjw.no-event-type".to_owned(),
            "no-event-type".to_owned(),
            "AUDIT EMISSION block declared but missing event_type= literal (missing: event_type)"
                .to_owned(),
        ),
        (
            "bd-3usjw.no-chain".to_owned(),
            "no-chain".to_owned(),
            "AUDIT EMISSION block declared but missing chain_continuity acceptance criterion (missing: chain_continuity)"
                .to_owned(),
        ),
        (
            "bd-3usjw.no-test-file".to_owned(),
            "no-test-file".to_owned(),
            "AUDIT EMISSION block declared but no *_audit.rs test file is referenced and on disk (missing: audit_test_file)"
                .to_owned(),
        ),
    ];
    expected.sort();
    let mut sorted = audit_observed.clone();
    sorted.sort();
    ensure_eq(sorted, expected, "Rule 8 violation set")?;

    // The exempt and valid beads must not appear under Rule 8.
    for clean_bead in ["bd-3usjw.no-audit-block", "bd-3usjw.valid-audit"] {
        ensure(
            !audit_observed.iter().any(|(bead, _, _)| bead == clean_bead),
            format!("{clean_bead} should not violate audit-emission obligation"),
        )?;
    }
    Ok(())
}
