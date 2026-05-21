//! N7.1 / ADR 0032 — `ee why --json` includes the Bayes posterior.

use std::process::{Command, Output};

use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn assert_success_json(output: &Output, context: &str) -> Result<JsonValue, String> {
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|error| format!("{context} stdout must be UTF-8: {error}"))?;
    let stderr = std::str::from_utf8(&output.stderr)
        .map_err(|error| format!("{context} stderr must be UTF-8: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "{context} failed; stderr: {stderr}; stdout: {stdout}"
        ));
    }
    serde_json::from_str(stdout)
        .map_err(|error| format!("{context} stdout must parse as JSON: {error}; got {stdout:?}"))
}

fn assert_f64(value: Option<&JsonValue>, expected: f64, label: &str) -> TestResult {
    let actual = value
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| format!("{label} must be a JSON number"))?;
    if (actual - expected).abs() <= 0.000_001 {
        Ok(())
    } else {
        Err(format!("{label}: expected {expected}, got {actual}"))
    }
}

#[test]
fn why_json_renders_jeffreys_bayes_posterior_for_fresh_memory() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir
        .path()
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;

    assert_success_json(
        &run_ee(&["--workspace", workspace, "--json", "init"])?,
        "ee init",
    )?;
    let remember = assert_success_json(
        &run_ee(&[
            "--workspace",
            workspace,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "Run cargo fmt --check before release.",
        ])?,
        "ee remember",
    )?;
    let memory_id = remember
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "remember response must include data.memory_id".to_owned())?;

    let why = assert_success_json(
        &run_ee(&["--workspace", workspace, "--json", "why", memory_id])?,
        "ee why",
    )?;
    let posterior = why
        .pointer("/data/bayesPosterior")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| "why response must include data.bayesPosterior object".to_owned())?;

    if posterior.get("schema").and_then(JsonValue::as_str) != Some("ee.bayes.posterior.v1") {
        return Err("bayesPosterior schema mismatch".to_owned());
    }
    assert_f64(posterior.get("alpha"), 0.5, "bayes alpha")?;
    assert_f64(posterior.get("beta"), 0.5, "bayes beta")?;
    assert_f64(posterior.get("mean"), 0.5, "bayes mean")?;
    assert_f64(
        posterior.get("effectiveSampleSize"),
        1.0,
        "bayes effective sample size",
    )?;

    let ci90 = posterior
        .get("credibleInterval90")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| "bayesPosterior.credibleInterval90 must be an object".to_owned())?;
    assert_f64(ci90.get("level"), 0.9, "ci90 level")?;
    let ci90_lower = ci90
        .get("lower")
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| "ci90 lower must be numeric".to_owned())?;
    let ci90_upper = ci90
        .get("upper")
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| "ci90 upper must be numeric".to_owned())?;
    if !(ci90_lower < 0.02 && ci90_upper > 0.98 && ci90_lower < ci90_upper) {
        return Err(format!(
            "Jeffreys ci90 should be wide around 0.5, got [{ci90_lower}, {ci90_upper}]"
        ));
    }

    let ci50 = posterior
        .get("credibleInterval50")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| "bayesPosterior.credibleInterval50 must be an object".to_owned())?;
    assert_f64(ci50.get("level"), 0.5, "ci50 level")?;
    Ok(())
}
