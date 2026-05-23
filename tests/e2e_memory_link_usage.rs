//! bd-1p09v: real-binary pin test for `ee memory link` usage
//! validation.
//!
//! The `ee memory link` command is the foundation of the entire
//! memory-link graph — every graph algorithm (pagerank, betweenness,
//! hits, etc.) operates on rows it creates. `memory_link_e2e.rs`
//! covers the create / list / duplicate happy path but does not pin
//! any of the five distinct `DomainError::Usage` paths in
//! `handle_memory_link` / `memory_link_mode_from_args` /
//! `parse_memory_link_relation` / `parse_memory_link_source` /
//! `parse_memory_link_score`. A reword of any of these would silently
//! break the contract that downstream agents rely on for recovery
//! hints.
//!
//! This pin test locks all five paths:
//!
//! * Create without `--relation` -> `"Creating a memory link requires
//!   --relation."` + repair `"Use ee memory link <source> <target>
//!   --relation supports."`
//! * `--relation garbage_relation` -> `"Unknown memory link relation"`
//!   + repair listing every valid relation
//! * `--weight 2.0` -> `"Invalid memory link weight: expected a
//!   finite number from 0.0 to 1.0"` + `"Use --weight 0.8."`
//! * `--confidence -1.0` -> same shape for confidence
//! * `--source garbage_source` -> `"Unknown memory link source"` +
//!   repair listing every valid source

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
        .join("ee-memory-link-usage-pin")
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

fn run_memory_link(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "memory", "link"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("memory link stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_usage_error(
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
            format!("usage message must contain {needle:?}; got {message}"),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    for needle in repair_needles {
        ensure(
            repair.contains(needle),
            format!("usage repair must contain {needle:?}; got {repair}"),
        )?;
    }
    Ok(())
}

#[test]
fn memory_link_create_without_relation_surfaces_usage_repair() -> TestResult {
    let workspace = unique_workspace("usage-no-relation")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    // Both source and target are present (triggering Create mode) but
    // --relation is omitted: memory_link_mode_from_args must surface
    // the documented repair pointing at the canonical example.
    let (output, parsed) = run_memory_link(&workspace_arg, &["mem_src", "mem_dst"])?;
    ensure(
        !output.status.success(),
        format!(
            "memory link without --relation must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["Creating a memory link requires --relation."],
        &["ee memory link", "--relation supports"],
    )
}

#[test]
fn memory_link_rejects_unknown_relation_with_vocabulary_repair() -> TestResult {
    let workspace = unique_workspace("usage-bad-relation")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_memory_link(
        &workspace_arg,
        &["mem_src", "mem_dst", "--relation", "garbage_relation"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "memory link --relation garbage_relation must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    // parse_memory_link_relation repair must list every documented
    // variant so an agent discovers the full vocabulary from a
    // single error response.
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("Unknown memory link relation") && message.contains("garbage_relation"),
        format!("usage message must name the rejected relation; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    for valid in [
        "supports",
        "contradicts",
        "derived_from",
        "supersedes",
        "related",
        "co_tag",
        "co_mention",
    ] {
        ensure(
            repair.contains(valid),
            format!("usage repair must list every valid relation ({valid} missing); got {repair}"),
        )?;
    }
    Ok(())
}

#[test]
fn memory_link_rejects_weight_out_of_range_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("usage-bad-weight")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_memory_link(
        &workspace_arg,
        &[
            "mem_src",
            "mem_dst",
            "--relation",
            "supports",
            "--weight",
            "2.0",
        ],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "memory link --weight 2.0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &[
            "Invalid memory link weight",
            "expected a finite number from 0.0 to 1.0",
        ],
        &["Use --weight 0.8."],
    )
}

#[test]
fn memory_link_rejects_confidence_out_of_range_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("usage-bad-confidence")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_memory_link(
        &workspace_arg,
        &[
            "mem_src",
            "mem_dst",
            "--relation",
            "supports",
            "--confidence",
            "-1.0",
        ],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "memory link --confidence -1.0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &[
            "Invalid memory link confidence",
            "expected a finite number from 0.0 to 1.0",
        ],
        &["Use --confidence 0.8."],
    )
}

#[test]
fn memory_link_rejects_unknown_source_with_vocabulary_repair() -> TestResult {
    let workspace = unique_workspace("usage-bad-source")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_memory_link(
        &workspace_arg,
        &[
            "mem_src",
            "mem_dst",
            "--relation",
            "supports",
            "--source",
            "garbage_source",
        ],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "memory link --source garbage_source must fail; stdout: {}",
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
        message.contains("Unknown memory link source") && message.contains("garbage_source"),
        format!("usage message must name the rejected source; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    for valid in ["agent", "auto", "import", "maintenance", "human"] {
        ensure(
            repair.contains(valid),
            format!("usage repair must list every valid source ({valid} missing); got {repair}"),
        )?;
    }
    Ok(())
}
