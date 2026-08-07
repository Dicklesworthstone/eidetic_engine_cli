//! E2E coverage for advisory preflight risk context.
//!
//! This exercises the public CLI rather than calling `core::preflight_guard`
//! directly: a risk memory with provenance is stored through `ee remember`,
//! then `ee preflight check` must surface it without denying the command.

use std::fmt::Debug;
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: T, expected: T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_exit(output: &Output, expected: i32, context: &str) -> TestResult {
    if output.status.code() == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected Some({expected}), got {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn stdout_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context} stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context} stdout was not JSON: {error}; stdout: {stdout}"))
}

fn assert_clean_stderr(output: &Output, context: &str) -> TestResult {
    ensure(
        output.stderr.is_empty(),
        format!(
            "{context} stderr should be empty; got {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn assert_no_execution_authority_fields(value: &Value, context: &str) -> TestResult {
    fn visit(value: &Value, path: &str, forbidden_paths: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, nested) in object {
                    let normalized = key
                        .chars()
                        .filter(|character| character.is_ascii_alphanumeric())
                        .map(|character| character.to_ascii_lowercase())
                        .collect::<String>();
                    let is_authority_field = matches!(
                        normalized.as_str(),
                        "permissiondecision"
                            | "requireshumanapproval"
                            | "nextaction"
                            | "preflightcommand"
                            | "shouldhalt"
                            | "cleared"
                    ) || normalized.starts_with("block")
                        || normalized.starts_with("allow");
                    let nested_path = format!("{path}/{key}");
                    if is_authority_field {
                        forbidden_paths.push(nested_path.clone());
                    }
                    visit(nested, &nested_path, forbidden_paths);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    visit(item, &format!("{path}/{index}"), forbidden_paths);
                }
            }
            _ => {}
        }
    }

    let mut forbidden_paths = Vec::new();
    visit(value, "$", &mut forbidden_paths);
    ensure(
        forbidden_paths.is_empty(),
        format!("{context} exposed execution-authority fields {forbidden_paths:?}: {value}"),
    )
}

#[test]
fn destructive_command_surfaces_matching_risk_memory_provenance() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();
    let provenance_input = "cass-session://incident-rm-rf#L1-L3";
    let provenance_canonical = "cass-session://incident-rm-rf#L1-3";
    let risk_content =
        "Prior incident: rm -rf /tmp/work recursively removed another agent workspace.";

    let init = run_ee(&["--workspace", &workspace, "--json", "init"])?;
    ensure_exit(&init, EXIT_SUCCESS, "ee init exit")?;
    assert_clean_stderr(&init, "ee init")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        risk_content,
        "--level",
        "procedural",
        "--kind",
        "risk",
        "--source",
        provenance_input,
        "--no-auto-link",
        "--no-propose-candidates",
        "--json",
    ])?;
    ensure_exit(&remember, EXIT_SUCCESS, "risk memory remember exit")?;
    assert_clean_stderr(&remember, "risk memory remember")?;
    let remembered = stdout_json(&remember, "risk memory remember")?;
    let memory_id = remembered
        .pointer("/data/memory_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("remember response missing memory_id: {remembered}"))?;

    let preflight = run_ee(&[
        "--workspace",
        &workspace,
        "--json",
        "preflight",
        "check",
        "--cmd",
        "rm -rf /tmp/work",
    ])?;
    ensure_exit(&preflight, EXIT_SUCCESS, "destructive preflight exit")?;
    assert_clean_stderr(&preflight, "destructive preflight")?;
    let report = stdout_json(&preflight, "destructive preflight")?;

    ensure_equal(
        report.get("schema").and_then(Value::as_str),
        Some("ee.preflight.guard.v1"),
        "preflight schema",
    )?;
    ensure_equal(
        report.get("exitCode").and_then(Value::as_i64),
        Some(i64::from(EXIT_SUCCESS)),
        "preflight exitCode",
    )?;
    ensure(
        report
            .get("matches")
            .and_then(Value::as_array)
            .is_some_and(|matches| !matches.is_empty()),
        format!("destructive preflight should include guard matches: {report}"),
    )?;

    let matched_memories = report
        .get("matchedMemories")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("preflight response missing matchedMemories: {report}"))?;
    ensure_equal(matched_memories.len(), 1, "matchedMemories length")?;
    let matched = &matched_memories[0];
    ensure_equal(
        matched.get("memoryId").and_then(Value::as_str),
        Some(memory_id),
        "matched memory id",
    )?;
    ensure_equal(
        matched.get("kind").and_then(Value::as_str),
        Some("risk"),
        "matched memory kind",
    )?;
    ensure_equal(
        matched.get("provenanceUri").and_then(Value::as_str),
        Some(provenance_canonical),
        "matched memory provenance",
    )?;
    ensure(
        matched
            .get("matchedTerms")
            .and_then(Value::as_array)
            .is_some_and(|terms| !terms.is_empty()),
        format!("matched memory should include matched_terms: {matched}"),
    )
}

#[test]
fn malformed_workspace_rules_still_surface_advisory_builtin_context() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();

    let init = run_ee(&["--workspace", &workspace, "--json", "init"])?;
    ensure_exit(&init, EXIT_SUCCESS, "ee init exit")?;
    assert_clean_stderr(&init, "ee init")?;

    let rules_path = tempdir.path().join(".ee").join("preflight_rules.toml");
    std::fs::write(
        &rules_path,
        r#"
[[rules]]
id = "bad_action"
pattern = "*rm -rf*"
action = "explode"
"#,
    )
    .map_err(|error| format!("write malformed preflight rules: {error}"))?;

    let preflight = run_ee(&[
        "--workspace",
        &workspace,
        "--json",
        "preflight",
        "check",
        "--cmd",
        "rm -rf /tmp/work",
    ])?;
    ensure_exit(&preflight, EXIT_SUCCESS, "malformed-rule preflight exit")?;
    assert_clean_stderr(&preflight, "malformed-rule destructive preflight")?;
    let report = stdout_json(&preflight, "malformed-rule destructive preflight")?;

    ensure_equal(
        report.get("schema").and_then(Value::as_str),
        Some("ee.preflight.guard.v1"),
        "preflight schema",
    )?;
    ensure(
        report
            .get("matches")
            .and_then(Value::as_array)
            .is_some_and(|matches| {
                matches.iter().any(|matched| {
                    matched.get("ruleId").and_then(Value::as_str) == Some("builtin:file_deletion")
                })
            }),
        format!("built-in deletion guard should still match: {report}"),
    )?;
    ensure(
        report
            .get("degraded")
            .and_then(Value::as_array)
            .is_some_and(|degraded| {
                degraded.iter().any(|entry| {
                    entry.get("code").and_then(Value::as_str)
                        == Some("preflight_patterns_unavailable")
                })
            }),
        format!("malformed workspace rules should surface degradation: {report}"),
    )
}

#[test]
fn cargo_rch_and_rustc_commands_are_never_denied_or_classified_as_destructive() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();

    let init = run_ee(&["--workspace", &workspace, "--json", "init"])?;
    ensure_exit(&init, EXIT_SUCCESS, "ee init exit")?;
    assert_clean_stderr(&init, "ee init")?;

    for command in [
        "cargo test --all-targets",
        "cargo check --all-targets",
        "cargo clippy --all-targets",
        "rch exec -- cargo check --all-targets",
        "rustc src/main.rs",
        "rustdoc --test src/lib.rs",
        "scripts/rch_verify.sh --bead-id bd-123 -- cargo test --lib foo",
    ] {
        let preflight = run_ee(&[
            "--workspace",
            &workspace,
            "--json",
            "preflight",
            "check",
            "--cmd",
            command,
        ])?;
        ensure_exit(&preflight, EXIT_SUCCESS, "Rust command preflight exit")?;
        assert_clean_stderr(&preflight, "Rust command preflight")?;
        let report = stdout_json(&preflight, "Rust command preflight")?;

        ensure_equal(
            report.get("schema").and_then(Value::as_str),
            Some("ee.preflight.guard.v1"),
            "preflight schema",
        )?;
        ensure_equal(
            report.get("exitCode").and_then(Value::as_i64),
            Some(i64::from(EXIT_SUCCESS)),
            "preflight exitCode",
        )?;
        assert_no_execution_authority_fields(&report, "Rust command preflight")?;
        ensure(
            report
                .get("matches")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            format!("Rust command must not match destructive rules: {report}"),
        )?;
        ensure(
            report
                .get("matchedMemories")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            format!("Rust command should not query destructive risk memories: {report}"),
        )?;
    }
    Ok(())
}
