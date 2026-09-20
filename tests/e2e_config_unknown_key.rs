//! bd-339a0: real-binary pin test for the UnknownKey branch of
//! `ee config get` and `ee config set`.
//!
//! `config_surface_error_to_domain` (src/cli/mod.rs:28226) maps
//! `ConfigSurfaceError` onto `DomainError::Configuration`. Under bd-p7wjm
//! that mapping is no longer shared between the two subcommands: `set`
//! reports an unwritable key, `get` reports the absence of a VALUE, and
//! only `set` is entitled to assert that a key does not exist.
//!
//! bd-p7wjm: `get` previously answered "Unknown config key" for any key
//! outside `config set`'s typed validation table, so it refused 83 of the
//! 117 keys declared in src/config/merge.rs while `config show` printed
//! every one of them with a value and a source. The two surfaces answer
//! different questions -- `set` is bounded by what it can type-check and
//! write, `get` by what the merged configuration actually holds -- and the
//! read path had been gated on the write path's table.
//!
//! tests/property_pack_metamorphic.rs covers the happy path for
//! `config set search.graph_weight` and `config get graph.ppr.alpha`, both
//! inside that table; the refusal text is pinned here instead.
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
    let repair = error["repair"].as_str().unwrap_or_default();

    // bd-p7wjm: `get` and `set` no longer say the same thing, and this
    // assertion used to require that they did.
    //
    // It pinned "Unknown config key `X`." plus a repair naming `graph.*` for
    // BOTH subcommands. That text was false for `get`, which resolves against
    // the merged configuration: 83 of the 117 keys declared in
    // src/config/merge.rs were reported as unknown while `config show`
    // printed them with a value and a source. Measured end-to-end against the
    // shipped 0.14.2 binary, 76 of the 110 keys `config show` prints were
    // refused by `config get` -- the same 70%, and the same 34 accepted keys
    // (graph 26, search 6, memory 2) by both counts.
    //
    // The repair was wrong in the opposite direction: it recommended
    // `graph.*`, which is not the excluded prefix but 26 of the 34 accepted
    // ones. It pointed users at the only prefix that already worked.
    //
    // The two subcommands are now pinned separately because they answer
    // different questions. `set` genuinely cannot write outside its typed
    // surface, so "not a key `ee config set` can write" is true. `get` cannot
    // distinguish a misspelling from a valid-but-unset key -- only keys WITH
    // a value are in the merged report -- so it must not assert
    // non-existence.
    match label {
        "set" => {
            ensure(
                message.contains(&format!("`{expected_key}` is not a config key")),
                format!("set must report the key as unwritable, not unknown; got {message}"),
            )?;
            ensure(
                repair.contains("ee config show --json"),
                format!("set repair must point at the full key listing; got {repair}"),
            )?;
            // Naming `graph.*` is correct -- it is 26 of the 34 writable keys.
            // The old hint's defect was pointing at a FILTERED listing, so a
            // user who mistyped `cache.pack_l2.enabled` was sent to enumerate
            // graph keys. The repair must offer the full listing instead.
            ensure(
                !repair.contains("config show graph"),
                format!(
                    "set repair must not send the user to a filtered `config show graph.*` \
                     listing; the full listing is what answers `which keys exist`; got {repair}"
                ),
            )?;
        }
        _ => {
            ensure(
                message.contains(&format!("No value for config key `{expected_key}`")),
                format!(
                    "get must report absence of a VALUE, not absence of the key; got {message}"
                ),
            )?;
            ensure(
                !message.contains("Unknown config key"),
                format!(
                    "get must not assert the key does not exist -- it cannot tell a \
                     misspelling from an unset key; got {message}"
                ),
            )?;
            ensure(
                repair.contains("ee config show --json"),
                format!("get repair must point at the full key listing; got {repair}"),
            )?;
        }
    }
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

/// `config get` returns a value for a key outside the settable surface.
/// bd-p7wjm.
///
/// This is the assertion the bead is actually about. Changing the error text
/// alone would leave the defect intact: 83 of the 117 keys declared in
/// src/config/merge.rs were REFUSED by `config get` while `config show`
/// printed them with a value and a source, because the read path gated on
/// `config_key_spec` -- a table that exists for `config set`'s value
/// validation. That table matches 8 keys directly and delegates its `_` arm
/// to `graph_key_spec`, which matches 26 more: 34 accepted, 83 refused.
///
/// `cache.pack_l2.enabled` is the live instance that exposed it. It is
/// declared at merge.rs:81, parsed at file.rs:480, policed at file.rs:1598
/// and emitted by `to_show_report()` at merge.rs:502 -- and `config get`
/// answered "Unknown config key". It is deliberately a key `config set`
/// still cannot write, so this proves READ was decoupled from WRITE rather
/// than the two surfaces being merged.
#[test]
fn config_get_returns_a_key_outside_the_settable_surface() -> TestResult {
    let workspace = unique_workspace("get-nonsettable")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    fs::write(
        workspace.join(".ee").join("config.toml"),
        "[cache.pack_l2]\nenabled = true\n",
    )
    .map_err(|error| format!("write config.toml: {error}"))?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "config",
        "get",
        "cache.pack_l2.enabled",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "config get must succeed for a set key outside the settable surface; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout must be JSON: {error}"))?;
    ensure(
        parsed["data"]["key"].as_str() == Some("cache.pack_l2.enabled"),
        format!("response must echo the requested key; got {parsed}"),
    )?;
    ensure(
        parsed["data"]["value"].as_str() == Some("true"),
        format!("response must carry the configured value; got {parsed}"),
    )?;

    // The write surface is deliberately unchanged: this key is readable and
    // still not settable. If a later change makes `config set` accept it,
    // that is a separate decision and this assertion should fail loudly
    // rather than pass quietly.
    let set_output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "config",
        "set",
        "cache.pack_l2.enabled",
        "false",
    ])?;
    ensure(
        !set_output.status.success(),
        "config set must still refuse a key outside its typed surface".to_owned(),
    )?;
    Ok(())
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
