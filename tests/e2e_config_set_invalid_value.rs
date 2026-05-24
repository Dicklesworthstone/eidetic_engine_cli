//! bd-rs15z: real-binary pin test for the InvalidValue branch of
//! `ee config set`.
//!
//! `config_surface_error_to_domain` (src/cli/mod.rs:19018) maps
//! `ConfigSurfaceError::InvalidValue { key, value, expected }` to
//! `DomainError::Configuration { message: "Invalid value \`{value}\`
//! for \`{key}\`; expected {expected}.", repair: Some("Choose a value
//! inside the documented range and retry \`ee config set\`.") }`.
//!
//! `parse_graph_value` (src/core/config_surface.rs:743) returns
//! `InvalidValue` across four `GraphValueKind` variants:
//! `Bool`, `UnitFloat`, `PositiveFloat` (and `NonNegativeFloat`), and
//! `UnsignedInteger`. None of those mappings is currently pinned through
//! the full clap -> CLI -> core -> output -> JSON-envelope path against
//! the real binary. The sibling
//! `tests/e2e_config_unknown_key.rs` (bd-339a0) pins only the
//! `UnknownKey` branch and its docstring explicitly flags `InvalidValue`
//! as still unpinned. The inline unit tests at
//! `src/core/config_surface.rs:958` and `:972` cover the `set_config()`
//! Rust API for two of the four variants but never exercise the real
//! `ee` binary.
//!
//! This pin-test mirrors the `tests/e2e_config_unknown_key.rs` harness
//! shape and additionally asserts that the project's
//! `<workspace>/.ee/config.toml` is byte-identical before and after the
//! rejected `ee config set` call, proving the validator fires before any
//! write.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
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
        .join("ee-config-set-invalid-value-pin")
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

fn snapshot_config_toml(workspace: &Path) -> Option<Vec<u8>> {
    let path = workspace.join(".ee").join("config.toml");
    fs::read(path).ok()
}

fn assert_invalid_value_error(
    output: &Output,
    key: &str,
    value: &str,
    expected_fragment: &str,
) -> TestResult {
    ensure(
        !output.status.success(),
        format!(
            "ee config set {key} {value} must fail; stdout: {}",
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
    let expected_message =
        format!("Invalid value `{value}` for `{key}`; expected {expected_fragment}.");
    ensure(
        message.contains(&expected_message),
        format!(
            "Configuration error message must pin the InvalidValue text; expected substring `{expected_message}`, got `{message}`"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("Choose a value inside the documented range and retry `ee config set`."),
        format!("Configuration error repair must pin the documented suggestion; got `{repair}`"),
    )?;
    Ok(())
}

fn run_invalid_value_case(
    prefix: &str,
    key: &str,
    value: &str,
    expected_fragment: &str,
) -> TestResult {
    let workspace = unique_workspace(prefix)?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let before = snapshot_config_toml(&workspace);

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "config",
        "set",
        key,
        value,
    ])?;
    assert_invalid_value_error(&output, key, value, expected_fragment)?;

    let after = snapshot_config_toml(&workspace);
    ensure(
        before == after,
        format!(
            "rejected `ee config set {key} {value}` must not mutate <workspace>/.ee/config.toml; before={before:?} after={after:?}"
        ),
    )
}

#[test]
fn config_set_unit_float_out_of_range_returns_configuration_error() -> TestResult {
    run_invalid_value_case(
        "unit-float",
        "graph.ppr.alpha",
        "1.5",
        "a finite number in the range 0.0..=1.0",
    )
}

#[test]
fn config_set_bool_invalid_returns_configuration_error() -> TestResult {
    run_invalid_value_case(
        "bool",
        "graph.feature.ppr.enabled",
        "maybe",
        "`true` or `false`",
    )
}

#[test]
fn config_set_positive_float_zero_returns_configuration_error() -> TestResult {
    run_invalid_value_case(
        "positive-float",
        "graph.curate.onion_decay_max",
        "0.0",
        "a finite number greater than 0.0",
    )
}

#[test]
fn config_set_unsigned_integer_negative_returns_configuration_error() -> TestResult {
    run_invalid_value_case(
        "unsigned-int",
        "graph.pack_dna.max_items",
        "-1",
        "a non-negative integer",
    )
}
