//! Regression harness for scripts/closure-lint.sh.
//!
//! The closure linter is a CI gate, so these tests execute the shell script
//! against synthetic workspaces instead of duplicating its logic in Rust.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

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
    Ok(())
}

fn run_linter(root: &Path) -> Result<(Output, Value), String> {
    let output = Command::new("sh")
        .arg(script_path())
        .args(["--audit", "--json"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("run closure-lint.sh: {error}"))?;

    let report_path = root.join(".closure-lint-report.json");
    let report = fs::read(&report_path)
        .map_err(|error| format!("read {}: {error}", report_path.display()))?;
    let report_json: Value = serde_json::from_slice(&report)
        .map_err(|error| format!("parse {}: {error}", report_path.display()))?;
    Ok((output, report_json))
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
