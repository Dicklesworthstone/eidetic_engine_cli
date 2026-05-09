use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn unique_report_path(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("vision-coverage-tests");
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir.join(format!("{prefix}-{}-{now}.json", std::process::id())))
}

fn ensure_beads_snapshot() -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".beads")
        .join("issues.jsonl");
    if path.exists() {
        return Ok(());
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let bead = serde_json::json!({
        "id": "eidetic_engine_cli-test-audit",
        "title": "[implements-surface:audit] fixture bead",
        "status": "open",
        "labels": ["implements-surface:audit"]
    });
    fs::write(&path, format!("{bead}\n"))
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn run_gate(
    report_path: &PathBuf,
    release_tag: bool,
    compare_ref: Option<&str>,
) -> Result<std::process::Output, String> {
    ensure_beads_snapshot()?;
    let mut command = Command::new("sh");
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("./scripts/vision-coverage.sh")
        .arg("--json")
        .arg("--report")
        .arg(report_path);
    if release_tag {
        command.arg("--release-tag");
    }
    if let Some(ref_name) = compare_ref {
        command.arg("--compare-ref").arg(ref_name);
    }
    command
        .output()
        .map_err(|error| format!("failed to run vision coverage gate: {error}"))
}

fn read_report(report_path: &PathBuf) -> Result<serde_json::Value, String> {
    let text = fs::read_to_string(report_path)
        .map_err(|error| format!("failed to read {}: {error}", report_path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("invalid report JSON in {}: {error}", report_path.display()))
}

fn pointer_u64(value: &serde_json::Value, pointer: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("{pointer} is not an unsigned integer"))
}

fn pointer_str<'a>(value: &'a serde_json::Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{pointer} is not a string"))
}

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn string_array_contains(value: &serde_json::Value, pointer: &str, needle: &str) -> TestResult {
    let array = value
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{pointer} is not an array"))?;
    ensure(
        array.iter().any(|item| item.as_str() == Some(needle)),
        &format!("{pointer} does not contain {needle}"),
    )
}

fn string_array_omits(value: &serde_json::Value, pointer: &str, needle: &str) -> TestResult {
    let array = value
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{pointer} is not an array"))?;
    ensure(
        array.iter().all(|item| item.as_str() != Some(needle)),
        &format!("{pointer} unexpectedly contains {needle}"),
    )
}

#[test]
fn vision_coverage_report_has_required_shape() -> TestResult {
    let report_path = unique_report_path("shape")?;
    let output = run_gate(&report_path, false, None)?;
    ensure(
        output.status.success(),
        &format!(
            "non-release gate should warn without failing\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let report = read_report(&report_path)?;
    ensure(
        pointer_str(&report, "/schema")? == "ee.vision_coverage.v1",
        "schema is pinned",
    )?;
    ensure(
        pointer_str(&report, "/status")? == "warn" || pointer_str(&report, "/status")? == "pass",
        "ordinary commits should report pass or warn",
    )?;
    ensure(
        pointer_u64(&report, "/surfaces/total_documented")? > 0,
        "report should discover documented command surfaces",
    )?;
    let stubbed = report
        .pointer("/stubbed_surfaces")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "stubbed_surfaces is not an array".to_owned())?;
    if !stubbed.is_empty() {
        ensure(
            stubbed.iter().all(|surface| {
                surface
                    .get("stub_constant")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|constant| constant.ends_with("_UNAVAILABLE_CODE"))
            }),
            "stubbed surfaces should record unavailable-code constants",
        )?;
        ensure(
            pointer_u64(&report, "/surfaces/with_open_implements_bead")? > 0,
            "stubbed surfaces should be linked to open implements-surface beads",
        )?;
    }
    Ok(())
}

#[test]
fn vision_coverage_canonicalizes_known_command_aliases() -> TestResult {
    let report_path = unique_report_path("aliases")?;
    let output = run_gate(&report_path, false, None)?;
    ensure(
        output.status.success(),
        &format!(
            "alias coverage should warn without failing\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let report = read_report(&report_path)?;
    string_array_contains(&report, "/implemented_surfaces", "pack build")?;
    string_array_omits(&report, "/missing_surfaces", "pack")?;
    string_array_contains(&report, "/implemented_surfaces", "graph centrality-refresh")?;
    string_array_omits(&report, "/missing_surfaces", "graph refresh")?;

    string_array_contains(&report, "/missing_surfaces", "index vacuum")?;
    string_array_contains(&report, "/missing_surfaces", "eval report")?;
    string_array_contains(&report, "/missing_surfaces", "completion")
}

#[test]
fn vision_coverage_blocks_release_tag_when_gap_remains() -> TestResult {
    let report_path = unique_report_path("release-tag")?;
    let output = run_gate(&report_path, true, None)?;
    let report = read_report(&report_path)?;
    ensure(
        report
            .pointer("/release_tag_commit")
            .and_then(serde_json::Value::as_bool)
            == Some(true),
        "release_tag_commit is true in forced release-tag mode",
    )?;

    let gap = report
        .pointer("/gap_percentage")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "gap_percentage is not a number".to_owned())?;
    if gap > 0.0 {
        ensure(
            !output.status.success(),
            &format!(
                "release-tag mode should fail while gaps remain\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        ensure(
            pointer_str(&report, "/status")? == "fail",
            "release-tag report status is fail",
        )?;
    } else {
        ensure(
            output.status.success(),
            "release-tag mode should pass once the documented gap reaches zero",
        )?;
        ensure(
            pointer_str(&report, "/status")? == "pass",
            "zero-gap release-tag report status is pass",
        )?;
    }
    Ok(())
}

#[test]
fn vision_coverage_compare_ref_includes_delta_report() -> TestResult {
    let report_path = unique_report_path("compare-ref")?;
    let output = run_gate(&report_path, false, Some("HEAD"))?;
    ensure(
        output.status.success(),
        &format!(
            "compare-ref mode should not fail on ordinary commits\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let report = read_report(&report_path)?;
    ensure(
        pointer_str(&report, "/delta_vs_main/ref")? == "HEAD",
        "delta report records comparison ref",
    )?;
    let delta_available = report
        .pointer("/delta_vs_main/available")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "/delta_vs_main/available is not a boolean".to_owned())?;
    ensure(
        report
            .pointer("/delta_vs_main/current_gap_percentage")
            .and_then(serde_json::Value::as_f64)
            .is_some(),
        "delta report includes current gap",
    )?;
    if delta_available {
        ensure(
            report
                .pointer("/delta_vs_main/baseline_gap_percentage")
                .and_then(serde_json::Value::as_f64)
                .is_some(),
            "available delta report includes baseline gap",
        )?;
    } else {
        ensure(
            pointer_str(&report, "/delta_vs_main/reason")? == "compare_ref_unavailable",
            "unavailable delta report records repairable reason",
        )?;
        ensure(
            report.pointer("/delta_vs_main/baseline_gap_percentage")
                == Some(&serde_json::Value::Null),
            "unavailable delta report has no synthetic baseline gap",
        )?;
        ensure(
            report.pointer("/delta_vs_main/gap_delta_percentage") == Some(&serde_json::Value::Null),
            "unavailable delta report has no synthetic gap delta",
        )?;
    }
    Ok(())
}
