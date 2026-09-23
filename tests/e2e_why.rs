//! bd-159vs: real-binary pin test for `ee why` validation and basic
//! surface.
//!
//! `ee why` explains memory provenance via graph traversal and is the
//! user-facing analog of the graph-algorithm family — but had no
//! dedicated real-binary pin test. `handle_why` (src/cli/mod.rs:34562)
//! has four distinct error/success surfaces that downstream agents
//! rely on for recovery hints and stable consumption:
//!
//! * `--confidence-threshold 2.0` -> Usage repair
//!   `"ee why <memory-id> --confidence-threshold 0.5"`
//! * `--confidence-threshold -1.0` -> same Usage shape
//! * Missing database (no `ee init`) -> Storage repair
//!   `"ee init --workspace <workspace>"`
//! * Non-existent memory id -> NotFound `"memory"` with repair
//!   `"ee memory list"`
//! * Happy path on a real memory -> stable envelope shape
//!   (schema=ee.response.v2, data.command="why", data.found=true,
//!   data.memoryId echoes the requested id)
//! * `--format mermaid` on a real memory -> a Mermaid diagram on
//!   stdout containing the memory id (separate render path at
//!   src/cli/mod.rs:34626)

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// Assert a command succeeded, and on failure say why: the exit code and both
/// streams (under `--json` the error envelope is on stdout). bd-z8mst.
fn ensure_command_success(output: &Output, context: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{context}: got exit {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ),
    )
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-why-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn init_workspace(workspace_arg: &str) -> TestResult {
    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )
}

fn remember(workspace_arg: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["public_id"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .or_else(|| parsed["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("remember response missing memory id: {}", parsed))
}

fn run_why_json(
    workspace_arg: &str,
    memory_id: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "why", memory_id];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("why stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_usage_error(parsed: &Value, message_needles: &[&str], repair_needle: &str) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    for needle in message_needles {
        ensure(
            message.contains(needle),
            format!("usage message must contain {needle:?}; got {message}"),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains(repair_needle),
        format!("usage repair must contain {repair_needle:?}; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn why_rejects_confidence_threshold_above_one_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-threshold-high")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_why_json(
        &workspace_arg,
        "mem_any",
        &["--confidence-threshold", "2.0"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee why --confidence-threshold 2.0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["confidence threshold must be a finite number between 0.0 and 1.0"],
        "ee why <memory-id> --confidence-threshold 0.5",
    )
}

#[test]
fn why_rejects_negative_confidence_threshold_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-threshold-neg")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    // `=` form, deliberately: passed as two arguments, clap consumes `-1.0` as
    // an unknown FLAG and answers "unexpected argument '-1' found" before
    // `ee`'s own validator (src/cli/mod.rs:53814) is ever reached. The
    // assertion below then cannot fire, while the `!status.success()` check
    // above still passes on clap's exit code -- the right result for the wrong
    // reason. `--confidence-threshold=-1.0` is a single argument, so the value
    // reaches the handler and the validator that this test exists to cover
    // actually runs. The positive sibling at :154 is unaffected because `2.0`
    // has no leading dash for clap to intercept.
    let (output, parsed) =
        run_why_json(&workspace_arg, "mem_any", &["--confidence-threshold=-1.0"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee why --confidence-threshold -1.0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["confidence threshold must be a finite number between 0.0 and 1.0"],
        "ee why <memory-id> --confidence-threshold 0.5",
    )
}

#[test]
fn why_surfaces_storage_error_when_database_missing() -> TestResult {
    // Skip ee init so the database-existence guard fires before any
    // explain work runs.
    let workspace = unique_workspace("usage-no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_why_json(&workspace_arg, "mem_any", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee why without ee init must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("Database not found at"),
        format!("error message must explain the missing database; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    let expected_repair = format!("ee init --workspace {workspace_arg}");
    ensure(
        repair.contains(&expected_repair),
        format!("error repair must point at `{expected_repair}`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn why_returns_not_found_for_unknown_memory_id() -> TestResult {
    let workspace = unique_workspace("not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_why_json(&workspace_arg, "mem_does_not_exist_in_workspace", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee why on unknown memory must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("mem_does_not_exist_in_workspace"),
        format!("error message must name the missing memory id; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee memory list"),
        format!("error repair must point at `ee memory list`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn why_returns_stable_envelope_for_existing_memory() -> TestResult {
    let workspace = unique_workspace("happy-path")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test why happy-path target memory.")?;

    let (output, parsed) = run_why_json(&workspace_arg, &memory_id, &[])?;
    ensure(
        output.status.success(),
        format!(
            "ee why on existing memory must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["schema"].as_str() == Some("ee.response.v2"),
        format!("envelope schema must be ee.response.v2; got {parsed}"),
    )?;
    ensure(
        parsed["success"] == Value::Bool(true),
        format!("success must be true; got {parsed}"),
    )?;
    let data = &parsed["data"];
    ensure(
        data["command"].as_str() == Some("why"),
        format!("data.command must be `why`; got {data}"),
    )?;
    ensure(
        data["memoryId"].as_str() == Some(memory_id.as_str()),
        format!("data.memoryId must echo the requested id {memory_id}; got {data}"),
    )?;
    ensure(
        data["found"] == Value::Bool(true),
        format!("data.found must be true for an existing memory; got {data}"),
    )?;
    ensure(
        data["content"].is_string(),
        format!("data.content must be a string for an existing memory; got {data}"),
    )?;
    ensure(
        data.pointer("/attestationBundle/schema")
            .and_then(Value::as_str)
            == Some("ee.attestation.surface_manifest.v1"),
        format!("why data must include attestation bundle manifest; got {data}"),
    )?;
    ensure(
        data.pointer("/attestationBundle/subject/kind")
            .and_then(Value::as_str)
            == Some("memory"),
        format!("why attestation subject kind must be memory; got {data}"),
    )?;
    ensure(
        data.pointer("/attestationBundle/bundleHash")
            .and_then(Value::as_str)
            .is_some_and(|hash| hash.starts_with("blake3:")),
        format!("why attestation bundle hash must be blake3-prefixed; got {data}"),
    )?;

    // bd-1n0np.22.3: the same memory must yield the same bundle hash on the
    // direct `ee attest memory` surface and on `why`, and neither surface may
    // perturb the hash the other reports (why writes an inspection audit).
    let why_hash = data
        .pointer("/attestationBundle/bundleHash")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("why attestation bundleHash missing; got {data}"))?
        .to_owned();
    let why_subject_id = data
        .pointer("/attestationBundle/subject/id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("why attestation subject id missing; got {data}"))?
        .to_owned();
    let attest = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "attest",
        "memory",
        &memory_id,
    ])?;
    ensure_command_success(&attest, "ee attest memory")?;
    let attest_json: Value = serde_json::from_slice(&attest.stdout)
        .map_err(|error| format!("attest stdout must be JSON: {error}"))?;
    ensure(
        attest_json
            .pointer("/data/subjectKind")
            .and_then(Value::as_str)
            == Some("memory"),
        format!("attest subject kind must be memory; got {attest_json}"),
    )?;
    ensure(
        attest_json
            .pointer("/data/subjectId")
            .and_then(Value::as_str)
            == Some(why_subject_id.as_str()),
        format!("attest and why must name the same public subject id; got {attest_json}"),
    )?;
    ensure(
        attest_json
            .pointer("/data/bundleHash")
            .and_then(Value::as_str)
            == Some(why_hash.as_str()),
        format!("attest memory bundleHash must equal why's ({why_hash}); got {attest_json}"),
    )?;

    let (again, reparsed) = run_why_json(&workspace_arg, &memory_id, &[])?;
    ensure_command_success(&again, "second ee why")?;
    ensure(
        reparsed
            .pointer("/data/attestationBundle/bundleHash")
            .and_then(Value::as_str)
            == Some(why_hash.as_str()),
        format!(
            "why bundleHash must be stable across why and attest calls ({why_hash}); got {reparsed}"
        ),
    )?;
    Ok(())
}

#[test]
fn why_format_mermaid_renders_diagram_referencing_memory_id() -> TestResult {
    // handle_why intercepts cli.format == Mermaid (and only when
    // --json/--robot are NOT set) and calls render_why_mermaid. Pin
    // that this render path is reachable and includes the requested
    // memory id so downstream agents can rely on it.
    let workspace = unique_workspace("mermaid")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test why mermaid memory.")?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--format",
        "mermaid",
        "why",
        memory_id.as_str(),
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee why --format mermaid must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    ensure(
        stdout.contains(&memory_id),
        format!(
            "mermaid output must reference the requested memory id {memory_id}; got {stdout:.500?}"
        ),
    )?;
    Ok(())
}
