//! Contract coverage for `ee insights --json-stream`.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_LOG_JSON")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("ee stdout should be UTF-8: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "ee {} should succeed; stdout: {stdout}; stderr: {stderr}",
            args.join(" ")
        ));
    }
    if !stderr.is_empty() {
        return Err(format!(
            "ee {} stderr should be empty: {stderr}",
            args.join(" ")
        ));
    }
    Ok(stdout)
}

fn jq_accepts_ndjson(stream: &str) -> TestResult {
    let mut child = Command::new("jq")
        .arg("-e")
        .arg(".")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("jq is required to validate insights NDJSON: {error}"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "jq stdin should be available".to_owned())?;
        stdin
            .write_all(stream.as_bytes())
            .map_err(|error| format!("write NDJSON to jq: {error}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for jq NDJSON parse: {error}"))?;
    if output.status.success() {
        return Ok(());
    }

    Err(format!(
        "jq rejected insights NDJSON: {}",
        String::from_utf8_lossy(&output.stderr)
    ))
}

#[test]
fn insights_json_stream_cli_emits_rust_and_jq_parseable_ndjson() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_path = workspace
        .path()
        .to_str()
        .ok_or_else(|| "temporary workspace path should be UTF-8".to_owned())?;
    let stdout = run_ee(&[
        "--workspace",
        workspace_path,
        "insights",
        "--json-stream",
        "--section",
        "topMemories",
    ])?;

    jq_accepts_ndjson(&stdout)?;

    let lines = stdout.lines().collect::<Vec<_>>();
    if lines.len() != 3 {
        return Err(format!(
            "topMemories stream should emit header, one section, and footer; got {} lines: {stdout}",
            lines.len()
        ));
    }

    let values = lines
        .iter()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("NDJSON line should parse as JSON: {error}: {line}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if values[0]["schema"] != "ee.insights.json_stream.header.v1" {
        return Err(format!("unexpected stream header: {}", values[0]));
    }
    if values[0]["reportSchema"] != "ee.insights.v1" {
        return Err(format!("unexpected report schema in header: {}", values[0]));
    }
    if values[0]["sectionCount"] != 1 {
        return Err(format!("header sectionCount drifted: {}", values[0]));
    }
    if values[1]["schema"] != "ee.insights.json_stream.section.v1" {
        return Err(format!("unexpected stream section line: {}", values[1]));
    }
    if values[1]["name"] != "topMemories" || values[1]["section"]["name"] != "topMemories" {
        return Err(format!("section name drifted: {}", values[1]));
    }
    if values[2]["schema"] != "ee.insights.json_stream.footer.v1" {
        return Err(format!("unexpected stream footer: {}", values[2]));
    }
    if !values[2]["degraded"].is_array() {
        return Err(format!(
            "footer degraded field should be an array: {}",
            values[2]
        ));
    }
    if values[2]["runDurationMs"] != 0 {
        return Err(format!("footer runDurationMs drifted: {}", values[2]));
    }

    Ok(())
}
