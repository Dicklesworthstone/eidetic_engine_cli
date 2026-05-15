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
