//! bd-2lnpo: real-binary pin test for `ee memory show` and
//! `ee memory history`.
//!
//! `ee memory history` surfaces the audit-trail spine of the
//! revision DAG that `graph_neighborhood_smoke.rs` touches via
//! `revisionLineage`. `ee memory show` is the canonical
//! single-memory view that downstream pack/why surfaces depend on.
//! Neither had dedicated real-binary pin coverage before this commit.
//!
//! Pins for `ee memory show`:
//! * Missing database -> Storage repair `"ee init --workspace ."`
//! * Non-existent memory -> NotFound `"memory"` + `"ee memory list"`
//! * Happy path -> envelope schema=`ee.response.v2`, data.command=
//!   `memory show`, data.found=true, data.memoryId echoed
//!
//! Pins for `ee memory history`:
//! * Missing database -> Storage repair
//! * Non-existent memory -> NotFound + `"ee memory list"`
//! * Happy path on a remembered memory -> data.command=`memory
//!   history`, data.memory_exists=true, data.entries contains at
//!   least one audit-trail entry with audit_id + timestamp + action
//! * `--format mermaid` -> deterministic flowchart with memory id,
//!   audit provenance comments, and no stderr
//! * `--json --format mermaid` and `--robot --format mermaid` ->
//!   canonical JSON envelope, not diagram or human fallback text

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
        .join("ee-memory-show-history-pin")
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
        .ok_or_else(|| {
            format!(
                "remember response missing memory id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn run_show(workspace_arg: &str, memory_id: &str) -> Result<(Output, Value), String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "memory",
        "show",
        memory_id,
    ])?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("memory show stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn run_history(workspace_arg: &str, memory_id: &str) -> Result<(Output, Value), String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "memory",
        "history",
        memory_id,
    ])?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("memory history stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn run_history_mermaid(workspace_arg: &str, memory_id: &str) -> Result<Output, String> {
    run_ee(&[
        "--workspace",
        workspace_arg,
        "--format",
        "mermaid",
        "memory",
        "history",
        memory_id,
    ])
}

fn run_history_mermaid_machine_mode(
    workspace_arg: &str,
    memory_id: &str,
    mode_flag: &str,
) -> Result<(Output, Value), String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        mode_flag,
        "--format",
        "mermaid",
        "memory",
        "history",
        memory_id,
    ])?;
    let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        let first_line = first_stdout_line(&output);
        format!(
            "ee memory history {mode_flag} --format mermaid expected canonical JSON override, \
             but stdout was not JSON: {error}; first output line: {first_line:?}"
        )
    })?;
    Ok((output, parsed))
}

fn first_stdout_line(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect()
}

fn assert_error_with_repair(
    parsed: &Value,
    message_needles: &[&str],
    repair_needles: &[&str],
) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    for needle in message_needles {
        ensure(
            message.contains(needle),
            format!("message must contain {needle:?}; got {message}"),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    for needle in repair_needles {
        ensure(
            repair.contains(needle),
            format!("repair must contain {needle:?}; got {repair}"),
        )?;
    }
    Ok(())
}

#[test]
fn memory_show_surfaces_storage_error_when_database_missing() -> TestResult {
    let workspace = unique_workspace("show-no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_show(&workspace_arg, "mem_any")?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory show without ee init must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["Database not found at"],
        &["ee init --workspace ."],
    )
}

#[test]
fn memory_show_returns_not_found_for_unknown_memory_id() -> TestResult {
    let workspace = unique_workspace("show-not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_show(&workspace_arg, "mem_does_not_exist_in_workspace")?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory show on unknown memory must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee memory list"),
        format!("not-found repair must point at `ee memory list`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn memory_show_happy_path_returns_stable_envelope() -> TestResult {
    let workspace = unique_workspace("show-happy")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test memory-show happy-path target.")?;

    let (output, parsed) = run_show(&workspace_arg, &memory_id)?;
    ensure(
        output.status.success(),
        format!(
            "ee memory show on existing memory must exit zero; stderr: {}",
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
        data["command"].as_str() == Some("memory show"),
        format!("data.command must be `memory show`; got {data}"),
    )?;
    ensure(
        data["found"] == Value::Bool(true),
        format!("data.found must be true; got {data}"),
    )?;
    ensure(
        data["memoryId"].as_str() == Some(memory_id.as_str()),
        format!("data.memoryId must echo the requested id {memory_id}; got {data}"),
    )?;
    ensure(
        data["memory"].is_object(),
        format!("data.memory must be an object; got {data}"),
    )?;
    Ok(())
}

#[test]
fn memory_history_surfaces_storage_error_when_database_missing() -> TestResult {
    let workspace = unique_workspace("history-no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_history(&workspace_arg, "mem_any")?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory history without ee init must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["Database not found at"],
        &["ee init --workspace ."],
    )
}

#[test]
fn memory_history_returns_not_found_for_unknown_memory_id() -> TestResult {
    let workspace = unique_workspace("history-not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_history(&workspace_arg, "mem_does_not_exist_in_workspace")?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory history on unknown memory must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee memory list"),
        format!("not-found repair must point at `ee memory list`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn memory_history_happy_path_returns_audit_entries() -> TestResult {
    let workspace = unique_workspace("history-happy")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test memory-history happy-path target.")?;

    let (output, parsed) = run_history(&workspace_arg, &memory_id)?;
    ensure(
        output.status.success(),
        format!(
            "ee memory history on existing memory must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["schema"].as_str() == Some("ee.response.v2"),
        format!("envelope schema must be ee.response.v2; got {parsed}"),
    )?;
    let data = &parsed["data"];
    ensure(
        data["command"].as_str() == Some("memory history"),
        format!("data.command must be `memory history`; got {data}"),
    )?;
    ensure(
        data["memory_exists"] == Value::Bool(true),
        format!("data.memory_exists must be true; got {data}"),
    )?;
    ensure(
        data["memory_id"].as_str() == Some(memory_id.as_str()),
        format!("data.memory_id must echo the requested id {memory_id}; got {data}"),
    )?;
    let entries = data["entries"]
        .as_array()
        .ok_or_else(|| format!("data.entries must be an array; got {data}"))?;
    ensure(
        !entries.is_empty(),
        format!(
            "data.entries must include at least one audit entry from ee remember; got {entries:?}"
        ),
    )?;
    // Verify the first entry carries the documented audit-row shape
    // (audit_id, timestamp, action). Other fields like actor/details
    // are optional.
    let first = &entries[0];
    ensure(
        first["audit_id"].is_string(),
        format!("entries[0].audit_id must be a string; got {first}"),
    )?;
    ensure(
        first["timestamp"].is_string(),
        format!("entries[0].timestamp must be a string; got {first}"),
    )?;
    ensure(
        first["action"].is_string(),
        format!("entries[0].action must be a string; got {first}"),
    )?;
    Ok(())
}

#[test]
fn memory_history_format_mermaid_renders_deterministic_audit_diagram() -> TestResult {
    let workspace = unique_workspace("history-mermaid")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(
        &workspace_arg,
        "Pin-test memory-history mermaid target with \"quotes\".",
    )?;

    let first = run_history_mermaid(&workspace_arg, &memory_id)?;
    let second = run_history_mermaid(&workspace_arg, &memory_id)?;
    ensure(
        first.status.success() && second.status.success(),
        format!(
            "ee memory history --format mermaid must exit zero; first stderr: {}; second stderr: {}",
            String::from_utf8_lossy(&first.stderr),
            String::from_utf8_lossy(&second.stderr)
        ),
    )?;
    ensure(
        first.stderr.is_empty() && second.stderr.is_empty(),
        format!(
            "memory history Mermaid must not write stderr; first={}, second={}",
            String::from_utf8_lossy(&first.stderr),
            String::from_utf8_lossy(&second.stderr)
        ),
    )?;
    let first_stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    let second_stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    ensure(
        first_stdout == second_stdout,
        format!(
            "memory history Mermaid output must be byte-deterministic; first={first_stdout:?} second={second_stdout:?}"
        ),
    )?;
    ensure(
        first_stdout.starts_with("flowchart TD\n"),
        format!("Mermaid history output must start with flowchart TD; got {first_stdout:.200?}"),
    )?;
    ensure(
        first_stdout.contains("command: memory history"),
        format!(
            "Mermaid history output must preserve command provenance; got {first_stdout:.500?}"
        ),
    )?;
    ensure(
        first_stdout.contains(&memory_id),
        format!(
            "Mermaid history output must reference memory id {memory_id}; got {first_stdout:.500?}"
        ),
    )?;
    ensure(
        first_stdout.contains("audit_id[1]"),
        format!("Mermaid history output must preserve audit id comments; got {first_stdout:.500?}"),
    )?;
    ensure(
        first_stdout.contains("-->|records| memory"),
        format!(
            "Mermaid history output must connect audit rows to memory; got {first_stdout:.500?}"
        ),
    )?;
    ensure(
        first_stdout.ends_with('\n'),
        format!("Mermaid history output must end with a newline; got {first_stdout:.50?}"),
    )?;
    Ok(())
}

#[test]
fn memory_history_machine_modes_override_format_mermaid() -> TestResult {
    let workspace = unique_workspace("history-mermaid-machine")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(
        &workspace_arg,
        "Pin-test memory-history JSON override for Mermaid requests.",
    )?;

    for mode_flag in ["--json", "--robot"] {
        let (output, parsed) =
            run_history_mermaid_machine_mode(&workspace_arg, &memory_id, mode_flag)?;
        let first_line = first_stdout_line(&output);
        ensure(
            output.status.success(),
            format!(
                "command=`ee memory history {mode_flag} --format mermaid`; \
                 expected capability=canonical JSON override; status failed; \
                 first output line={first_line:?}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        ensure(
            output.stderr.is_empty(),
            format!(
                "command=`ee memory history {mode_flag} --format mermaid`; \
                 expected capability=canonical JSON override; stderr must be empty; \
                 first output line={first_line:?}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        ensure(
            parsed["schema"].as_str() == Some("ee.response.v2"),
            format!(
                "command=`ee memory history {mode_flag} --format mermaid`; \
                 expected capability=canonical JSON override; schema drifted; \
                 first output line={first_line:?}; got {parsed}"
            ),
        )?;
        ensure(
            parsed["data"]["command"].as_str() == Some("memory history"),
            format!(
                "command=`ee memory history {mode_flag} --format mermaid`; \
                 expected capability=canonical JSON override; data.command drifted; \
                 first output line={first_line:?}; got {}",
                parsed["data"]
            ),
        )?;
        ensure(
            !first_line.starts_with("flowchart") && !first_line.starts_with("Memory history"),
            format!(
                "command=`ee memory history {mode_flag} --format mermaid`; \
                 expected JSON override, not Mermaid or human fallback; \
                 first output line={first_line:?}"
            ),
        )?;
    }

    Ok(())
}
