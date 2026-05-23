//! bd-1vkhl: real-binary pin test for `ee memory tags` validators.
//!
//! Tags are graph-adjacent — the `co_tag` relation is one of the
//! memory-link relations pinned by `bd-1p09v`. Two distinct
//! validators in `handle_memory_tags` had no end-to-end coverage
//! before this commit:
//!
//! * `memory_tags_mode_from_args` exclusive-mode guard:
//!   `"Choose only one memory tag mutation mode: --add/--remove,
//!   --set, or --clear."` + `"Use ee memory tags <id> --add
//!   release,testing."`
//! * `parse_memory_tags_values` -> `Tag::parse` character validator:
//!   `"tag `<input>` contains characters outside the accepted set..."`
//!   + `"Use lowercase tag names such as --add release,testing."`
//!
//! Plus the standard missing-database guard and a happy-path list
//! mode (no mutation flags) on a freshly-remembered memory.

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
        .join("ee-memory-tags-validation-pin")
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

fn run_tags(
    workspace_arg: &str,
    memory_id: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "memory", "tags"];
    args.push(memory_id);
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("memory tags stdout must be JSON: {error}"))?;
    Ok((output, parsed))
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
fn memory_tags_rejects_add_and_set_together_with_exclusive_mode_repair() -> TestResult {
    let workspace = unique_workspace("exclusive-add-set")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test tags exclusive add/set target.")?;

    let (output, parsed) = run_tags(
        &workspace_arg,
        &memory_id,
        &["--add", "foo", "--set", "bar"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory tags --add + --set must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["Choose only one memory tag mutation mode"],
        &["ee memory tags", "--add release,testing"],
    )
}

#[test]
fn memory_tags_rejects_add_and_clear_together_with_exclusive_mode_repair() -> TestResult {
    let workspace = unique_workspace("exclusive-add-clear")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test tags exclusive add/clear target.")?;

    let (output, parsed) = run_tags(&workspace_arg, &memory_id, &["--add", "foo", "--clear"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory tags --add + --clear must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["Choose only one memory tag mutation mode"],
        &["ee memory tags", "--add release,testing"],
    )
}

#[test]
fn memory_tags_rejects_invalid_tag_character_with_lowercase_repair() -> TestResult {
    let workspace = unique_workspace("invalid-tag-char")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test tags invalid-char target.")?;

    // A tag containing a space is rejected by is_valid_tag_str
    // (whitespace is explicitly excluded). The error wraps with the
    // documented lowercase-tag hint from parse_memory_tags_values.
    let (output, parsed) = run_tags(&workspace_arg, &memory_id, &["--add", "bad tag"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory tags --add `bad tag` must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["contains characters outside the accepted set"],
        &["Use lowercase tag names", "--add release,testing"],
    )
}

#[test]
fn memory_tags_surfaces_storage_error_when_database_missing() -> TestResult {
    // Skip ee init so the database-existence guard fires before any
    // tag-mutation work runs.
    let workspace = unique_workspace("usage-no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_tags(&workspace_arg, "mem_any", &["--add", "foo"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory tags without ee init must fail; stdout: {}",
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
fn memory_tags_list_mode_on_untagged_memory_succeeds() -> TestResult {
    // No mutation flags -> List mode. A freshly-remembered memory
    // has no tags; the list should succeed (exit 0) under a stable
    // envelope, without surfacing a Usage or Storage error.
    let workspace = unique_workspace("list-empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test tags list-mode target.")?;

    let (output, parsed) = run_tags(&workspace_arg, &memory_id, &[])?;
    ensure(
        output.status.success(),
        format!(
            "ee memory tags list mode on untagged memory must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["error"].is_null() || parsed.get("error").is_none(),
        format!("list mode must not surface an error; got {parsed}"),
    )?;
    Ok(())
}
