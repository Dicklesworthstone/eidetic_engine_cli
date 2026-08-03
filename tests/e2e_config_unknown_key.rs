//! bd-339a0: real-binary pin test for the UnknownKey branch of
//! `ee config get` and `ee config set`.
//!
//! `config_surface_error_to_domain` (src/cli/mod.rs:19006) maps
//! `ConfigSurfaceError::UnknownKey` to
//! `DomainError::Configuration { message: "Unknown config key
//! \`{key}\`.", repair: Some("Use \`ee config show graph.* --json\` to
//! list supported graph keys.") }`. This branch fires for both
//! `ee config get <unknown>` and `ee config set <unknown> <value>` (and
//! a similar Configuration error for InvalidValue on `ee config set`).
//! tests/property_pack_metamorphic.rs:542 covers only the happy path for
//! `config set search.graph_weight 0.0` and `config get
//! search.graph_weight`; the UnknownKey + repair text are unpinned for
//! both subcommands against the real binary.
//!
//! This pin-test mirrors the
//! `tests/e2e_schema_export_unknown.rs` harness shape.
//!
//! bd-config-unknown-keys-silent-mio6h extends the same real-binary
//! coverage to unknown keys read from `.ee/config.toml`. A typo inside a
//! task-lens override must fail both configuration inspection and lens
//! loading with the indexed key path and a sibling-key suggestion.

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
        .join("ee-config-unknown-key-pin")
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

fn workspace_with_task_lens_key_typo(prefix: &str) -> Result<String, String> {
    let workspace = unique_workspace(prefix)?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let config_path = workspace.join(".ee").join("config.toml");
    fs::write(
        &config_path,
        r#"[[task_lens.overrides]]
id = "local-bugfix-override"
version = 1
description = "Local bugfix lens used to exercise config validation."
allowed_kind = ["failure", "risk"]
"#,
    )
    .map_err(|error| format!("failed to write {}: {error}", config_path.display()))?;

    Ok(workspace_arg)
}

fn assert_unknown_key_error(output: &Output, label: &str, expected_key: &str) -> TestResult {
    ensure(
        !output.status.success(),
        format!(
            "ee config {label} bogus key must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout must be JSON: {error}"))?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains(&format!("Unknown config key `{expected_key}`.")),
        format!(
            "Configuration error message must pin the UnknownKey text for `{expected_key}`; got {message}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("Use `ee config show graph.* --json` to list supported graph keys."),
        format!("Configuration error repair must pin the documented suggestion; got {repair}"),
    )?;
    Ok(())
}

fn assert_config_file_unknown_key_error(output: &Output, command: &str) -> TestResult {
    ensure(
        output.status.code() == Some(2),
        format!(
            "ee {command} must exit 2 for an unknown config-file key; status: {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ee {command} stdout must be JSON: {error}"))?;
    ensure(
        parsed["schema"].as_str() == Some("ee.error.v2"),
        format!("ee {command} must emit ee.error.v2; got {parsed}"),
    )?;
    ensure(
        parsed["error"]["code"].as_str() == Some("configuration"),
        format!("ee {command} must emit error code configuration; got {parsed}"),
    )?;

    let message = parsed["error"]["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("task_lens.overrides[0].allowed_kind"),
        format!("ee {command} error must identify the indexed offending path; got `{message}`"),
    )?;
    ensure(
        message.contains("allowed_kinds"),
        format!("ee {command} error must suggest `allowed_kinds`; got `{message}`"),
    )
}

#[test]
fn config_get_unknown_key_returns_configuration_error() -> TestResult {
    let workspace = unique_workspace("get-unknown")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let phantom = "bogus.unknown.config.key";
    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "config",
        "get",
        phantom,
    ])?;
    assert_unknown_key_error(&output, "get", phantom)
}

#[test]
fn config_set_unknown_key_returns_configuration_error_before_write() -> TestResult {
    let workspace = unique_workspace("set-unknown")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let phantom = "bogus.unknown.config.key";
    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "config",
        "set",
        phantom,
        "0.5",
    ])?;
    assert_unknown_key_error(&output, "set", phantom)
}

#[test]
fn config_show_rejects_unknown_task_lens_override_key() -> TestResult {
    let workspace_arg = workspace_with_task_lens_key_typo("show-file-typo")?;
    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "config",
        "show",
    ])?;

    assert_config_file_unknown_key_error(&output, "config show")
}

#[test]
fn lens_list_rejects_unknown_task_lens_override_key() -> TestResult {
    let workspace_arg = workspace_with_task_lens_key_typo("lens-file-typo")?;
    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "lens",
        "list",
    ])?;

    assert_config_file_unknown_key_error(&output, "lens list")
}
