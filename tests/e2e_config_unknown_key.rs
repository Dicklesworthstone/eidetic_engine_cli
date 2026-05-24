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
