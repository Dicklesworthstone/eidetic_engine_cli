//! bd-6trv0: real-binary pin test for `ee memory link` SEMANTIC
//! validation.
//!
//! Companion to `bd-1p09v` (e2e_memory_link_usage.rs) which pinned
//! the five SYNTACTIC `DomainError::Usage` paths
//! (relation/source/weight/confidence vocabulary, missing
//! `--relation`). This pin test covers SEMANTIC validators that fire
//! after syntax parses but before durable insertion — three more
//! contracts that `handle_memory_link` -> `update_memory_link` must
//! preserve verbatim so downstream agents keep getting actionable
//! recovery hints:
//!
//! * Self-link (source memory id equals target memory id) -> Usage
//!   repair `"Use two different memory IDs."` +
//!   `"Memory links cannot target the same memory as their source."`
//! * `--metadata not-json` -> Usage repair from
//!   `validate_memory_link_metadata` naming `"Invalid memory link
//!   metadata JSON"` and showing the canonical example.
//! * Link involving a tombstoned target memory ->
//!   `DomainError::PolicyDenied` `"Cannot create memory links
//!   involving expired memories."` +
//!   `"Use ee memory show --include-tombstoned to inspect them."`

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
        .join("ee-memory-link-semantic-pin")
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

fn expire_memory(workspace_arg: &str, memory_id: &str) -> TestResult {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "memory",
        "expire",
        memory_id,
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee memory expire {memory_id} must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn run_memory_link(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "memory", "link"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("memory link stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

#[test]
fn memory_link_self_link_is_rejected_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("self-link")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let mem = remember(&workspace_arg, "Pin-test self-link source/target.")?;

    let (output, parsed) =
        run_memory_link(&workspace_arg, &[&mem, &mem, "--relation", "supports"])?;
    ensure(
        !output.status.success(),
        format!(
            "memory link with source == target must fail; stdout: {}",
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
        message.contains("Memory links cannot target the same memory as their source."),
        format!("self-link must surface the documented Usage message; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("Use two different memory IDs."),
        format!("self-link repair must point at distinct ids; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn memory_link_rejects_malformed_metadata_json_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("bad-metadata")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let src = remember(&workspace_arg, "Pin-test malformed-metadata source.")?;
    let dst = remember(&workspace_arg, "Pin-test malformed-metadata target.")?;

    let (output, parsed) = run_memory_link(
        &workspace_arg,
        &[
            &src,
            &dst,
            "--relation",
            "supports",
            "--metadata",
            "not-json",
        ],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "memory link --metadata not-json must fail; stdout: {}",
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
        message.contains("Invalid memory link metadata JSON"),
        format!(
            "malformed metadata must surface validate_memory_link_metadata text; got {message}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    // Repair shows the canonical example so an agent can copy/paste
    // it directly into the next invocation.
    ensure(
        repair.contains("--metadata") && repair.contains("reason"),
        format!("metadata repair must show the documented canonical example; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn memory_link_rejects_links_involving_tombstoned_memory_with_policy_repair() -> TestResult {
    let workspace = unique_workspace("tombstoned")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let src = remember(&workspace_arg, "Pin-test tombstone-link source.")?;
    let dst = remember(&workspace_arg, "Pin-test tombstone-link target.")?;
    // Tombstone the target so the link attempt hits the PolicyDenied
    // branch in update_memory_link.
    expire_memory(&workspace_arg, &dst)?;

    let (output, parsed) =
        run_memory_link(&workspace_arg, &[&src, &dst, "--relation", "supports"])?;
    ensure(
        !output.status.success(),
        format!(
            "memory link to a tombstoned target must fail; stdout: {}",
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
        message.contains("Cannot create memory links involving expired memories."),
        format!(
            "tombstoned-target link must surface the documented PolicyDenied text; got {message}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee memory show --include-tombstoned"),
        format!(
            "tombstoned-link repair must point at `ee memory show --include-tombstoned`; got {repair}"
        ),
    )?;
    Ok(())
}
