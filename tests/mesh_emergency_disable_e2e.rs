#![allow(clippy::expect_used, clippy::unwrap_used)]
//! bd-3mw86: `ee mesh disable --peer` blast-radius contract at the process
//! level.
//!
//! Companion to the unit tests in `src/mesh/emergency_disable.rs`: drives the
//! built `ee` binary end-to-end the way an operator or agent harness would,
//! covering Clap dispatch, exit classes, the `ee.error.v2` envelope with
//! structured recovery, and byte-level immutability of a seeded workspace
//! `.ee/config.toml` across every refused or preview-only invocation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn temp_workspace() -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("ee-mesh-emergency-disable-")
        .tempdir_in("/tmp")
        .map_err(|error| format!("failed to create temp workspace under /tmp: {error}"))
}

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .arg("--workspace")
        .arg(workspace)
        .env_remove("EE_MESH_ENABLED")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn ensure_exit_code(output: &Output, expected: i32, label: &str) -> TestResult {
    let actual = output.status.code();
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "{label}: expected exit {expected}, got {actual:?}; stderr: {}",
            String::from_utf8_lossy(&output.stderr).trim_end(),
        ))
    }
}

fn init_workspace(workspace: &str) -> TestResult {
    let init = run_ee(workspace, &["init", "--json"])?;
    ensure_exit_code(&init, 0, "init")
}

/// Overwrite the workspace config with distinctive bytes so tests can prove
/// a refused or preview-only command left the file byte-identical.
fn seed_config(workspace: &Path) -> Result<(PathBuf, String), String> {
    let ee_dir = workspace.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| format!("create .ee: {error}"))?;
    let config_path = ee_dir.join("config.toml");
    let seeded = "# operator marker: bd-3mw86 e2e seeded config\n[mesh]\nenabled = true\ncommand_mode = \"cache\"\n";
    fs::write(&config_path, seeded).map_err(|error| format!("seed config: {error}"))?;
    Ok((config_path, seeded.to_owned()))
}

fn assert_config_unchanged(config_path: &Path, seeded: &str, label: &str) -> TestResult {
    let after =
        fs::read_to_string(config_path).map_err(|error| format!("{label}: reread: {error}"))?;
    if after == seeded {
        Ok(())
    } else {
        Err(format!(
            "{label}: seeded config mutated.\nbefore:\n{seeded}\nafter:\n{after}"
        ))
    }
}

fn shell_quote_expected(value: &str) -> String {
    if value.is_empty() {
        "''".to_owned()
    } else if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn string_at<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
    label: &str,
) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{label}: missing string at {pointer}; got {value}"))
}

#[test]
fn peer_scope_refusal_is_usage_error_with_full_scope_recovery() -> TestResult {
    let workspace = temp_workspace()?;
    let workspace_str = workspace.path().to_string_lossy().to_string();
    init_workspace(&workspace_str)?;
    let (config_path, seeded) = seed_config(workspace.path())?;

    let output = run_ee(
        &workspace_str,
        &["mesh", "disable", "--peer", "peer_alpha", "--json"],
    )?;
    // Exit class 1 = usage error per the project exit-code contract.
    ensure_exit_code(&output, 1, "peer refusal")?;
    let envelope = stdout_json(&output, "peer refusal")?;
    if string_at(&envelope, "/schema", "peer refusal")? != "ee.error.v2" {
        return Err(format!("peer refusal: unexpected schema: {envelope}"));
    }
    if string_at(&envelope, "/error/code", "peer refusal")? != "usage" {
        return Err(format!("peer refusal: unexpected error code: {envelope}"));
    }
    let message = string_at(&envelope, "/error/message", "peer refusal")?;
    if !message.contains("peer_alpha") {
        return Err(format!(
            "peer refusal: message must name the peer: {message}"
        ));
    }
    // The repair and the structured recovery must reproduce the invoking
    // scope so an agent following them cannot mutate the wrong store.
    let repair = string_at(&envelope, "/error/repair", "peer refusal")?;
    if !repair.contains("ee mesh peer revoke") || !repair.contains(&workspace_str) {
        return Err(format!(
            "peer refusal: repair must carry the revoke command with the resolved workspace: {repair}"
        ));
    }
    let recovery_command = string_at(
        &envelope,
        "/error/details/recovery/0/command",
        "peer refusal",
    )?;
    if !recovery_command.contains("ee mesh peer revoke")
        || !recovery_command.contains(&workspace_str)
    {
        return Err(format!(
            "peer refusal: recovery[0] must be the workspace-scoped revoke command: {recovery_command}"
        ));
    }
    assert_config_unchanged(&config_path, &seeded, "peer refusal")
}

#[test]
fn peer_scope_refusal_recovery_carries_explicit_database_override() -> TestResult {
    let workspace = temp_workspace()?;
    let workspace_str = workspace.path().to_string_lossy().to_string();
    init_workspace(&workspace_str)?;
    let (config_path, seeded) = seed_config(workspace.path())?;
    let database = workspace.path().join("elsewhere").join("custom.db");
    let database_str = database.to_string_lossy().to_string();

    let output = run_ee(
        &workspace_str,
        &[
            "mesh",
            "disable",
            "--peer",
            "peer_alpha",
            "--database",
            &database_str,
            "--json",
        ],
    )?;
    ensure_exit_code(&output, 1, "database override refusal")?;
    let envelope = stdout_json(&output, "database override refusal")?;
    let repair = string_at(&envelope, "/error/repair", "database override refusal")?;
    if !repair.contains("--database") || !repair.contains(&database_str) {
        return Err(format!(
            "database override refusal: repair must carry the explicit database: {repair}"
        ));
    }
    let recovery_command = string_at(
        &envelope,
        "/error/details/recovery/0/command",
        "database override refusal",
    )?;
    if !recovery_command.contains("--database") || !recovery_command.contains(&database_str) {
        return Err(format!(
            "database override refusal: recovery[0] must carry the explicit database: {recovery_command}"
        ));
    }
    assert_config_unchanged(&config_path, &seeded, "database override refusal")
}

#[test]
fn peer_dry_run_previews_refusal_honestly_and_writes_nothing() -> TestResult {
    let workspace = temp_workspace()?;
    let workspace_str = workspace.path().to_string_lossy().to_string();
    init_workspace(&workspace_str)?;
    let (config_path, seeded) = seed_config(workspace.path())?;

    let output = run_ee(
        &workspace_str,
        &[
            "mesh",
            "disable",
            "--peer",
            "peer_alpha",
            "--dry-run",
            "--json",
        ],
    )?;
    ensure_exit_code(&output, 0, "peer dry-run")?;
    let envelope = stdout_json(&output, "peer dry-run")?;
    let data = envelope
        .get("data")
        .ok_or_else(|| format!("peer dry-run: no data: {envelope}"))?;
    if string_at(data, "/scope", "peer dry-run")? != "peer" {
        return Err(format!("peer dry-run: unexpected scope: {data}"));
    }
    if data.pointer("/applied") != Some(&serde_json::Value::Bool(false)) {
        return Err(format!("peer dry-run: applied must be false: {data}"));
    }
    // Honest preview: no durable suspension path exists, so the preview
    // must not claim a suspension, rejected requests, or suspended
    // capabilities (bd-3mw86 review).
    if data.pointer("/newPeerRequestsRejected") != Some(&serde_json::Value::Bool(false)) {
        return Err(format!(
            "peer dry-run: newPeerRequestsRejected must be false for peer scope: {data}"
        ));
    }
    if string_at(data, "/peerCapabilitiesSuspended/0/state", "peer dry-run")? != "unavailable" {
        return Err(format!(
            "peer dry-run: suspension entry must be state=unavailable: {data}"
        ));
    }
    let suspended = data
        .pointer("/peerCapabilitiesSuspended/0/capabilitiesSuspended")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("peer dry-run: missing capabilitiesSuspended: {data}"))?;
    if !suspended.is_empty() {
        return Err(format!(
            "peer dry-run: no capability may be claimed suspended: {data}"
        ));
    }
    // Peer scope leaves workspace posture untouched.
    if data.pointer("/meshEnabledAfter") != data.pointer("/meshEnabledBefore") {
        return Err(format!(
            "peer dry-run: meshEnabledAfter must equal meshEnabledBefore: {data}"
        ));
    }
    if data.pointer("/commandModeAfter") != data.pointer("/commandModeBefore") {
        return Err(format!(
            "peer dry-run: commandModeAfter must equal commandModeBefore: {data}"
        ));
    }
    let config_actions = data
        .pointer("/configActions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("peer dry-run: missing configActions: {data}"))?;
    if !config_actions.is_empty() {
        return Err(format!(
            "peer dry-run: peer scope must plan no config mutations: {data}"
        ));
    }
    let next_commands = data
        .pointer("/nextCommands")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("peer dry-run: missing nextCommands: {data}"))?;
    let has_scoped_revoke = next_commands.iter().any(|command| {
        command.as_str().is_some_and(|command| {
            command.contains("mesh peer revoke peer_alpha") && command.contains(&workspace_str)
        })
    });
    if !has_scoped_revoke {
        return Err(format!(
            "peer dry-run: nextCommands must point at the workspace-scoped durable revoke path: {data}"
        ));
    }
    assert_config_unchanged(&config_path, &seeded, "peer dry-run")
}

#[test]
fn peer_recovery_commands_quote_adversarial_workspace_and_database_paths() -> TestResult {
    let root = temp_workspace()?;
    let workspace = root.path().join("scope $(touch nope) 'workspace'");
    fs::create_dir_all(&workspace).map_err(|error| format!("create adversarial scope: {error}"))?;
    let workspace_str = workspace.to_string_lossy().to_string();
    init_workspace(&workspace_str)?;
    let (config_path, seeded) = seed_config(&workspace)?;
    let database = root.path().join("db `touch nope2` $HOME 'custom'.db");
    let database_str = database.to_string_lossy().to_string();

    let output = run_ee(
        &workspace_str,
        &[
            "mesh",
            "disable",
            "--peer",
            "peer_alpha",
            "--database",
            &database_str,
            "--dry-run",
            "--json",
        ],
    )?;
    ensure_exit_code(&output, 0, "adversarial-scope dry-run")?;
    let envelope = stdout_json(&output, "adversarial-scope dry-run")?;
    let commands = envelope
        .pointer("/data/nextCommands")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("adversarial-scope dry-run: no nextCommands: {envelope}"))?;
    let workspace_argument = shell_quote_expected(&workspace_str);
    let database_argument = shell_quote_expected(&database_str);
    for command in commands {
        let command = command
            .as_str()
            .ok_or_else(|| format!("adversarial-scope command is not a string: {command}"))?;
        if !command.contains(&format!("--workspace {workspace_argument}"))
            || !command.contains(&format!("--database {database_argument}"))
        {
            return Err(format!(
                "adversarial paths must be emitted as inert shell arguments: {command}"
            ));
        }
        if command.contains("--workspace \"") || command.contains("--database \"") {
            return Err(format!(
                "metacharacter-bearing paths must not use expansion-capable double quotes: {command}"
            ));
        }
    }
    assert_config_unchanged(&config_path, &seeded, "adversarial-scope dry-run")
}

#[test]
fn contradictory_peer_and_all_workspaces_scopes_are_rejected() -> TestResult {
    let workspace = temp_workspace()?;
    let workspace_str = workspace.path().to_string_lossy().to_string();
    init_workspace(&workspace_str)?;
    let (config_path, seeded) = seed_config(workspace.path())?;

    let output = run_ee(
        &workspace_str,
        &[
            "mesh",
            "disable",
            "--peer",
            "peer_alpha",
            "--all-workspaces",
            "--json",
        ],
    )?;
    if output.status.success() {
        return Err(
            "conflict: --peer with --all-workspaces must be rejected, but the command succeeded"
                .to_owned(),
        );
    }
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    if !combined.contains("--all-workspaces") {
        return Err(format!(
            "conflict: refusal must name the conflicting flags; got: {combined}"
        ));
    }
    assert_config_unchanged(&config_path, &seeded, "conflict")
}

#[test]
fn workspace_wide_disable_applies_and_reports_truthfully() -> TestResult {
    let workspace = temp_workspace()?;
    let workspace_str = workspace.path().to_string_lossy().to_string();
    init_workspace(&workspace_str)?;
    let (config_path, _seeded) = seed_config(workspace.path())?;

    let output = run_ee(&workspace_str, &["mesh", "disable", "--json"])?;
    ensure_exit_code(&output, 0, "workspace disable")?;
    let envelope = stdout_json(&output, "workspace disable")?;
    let data = envelope
        .get("data")
        .ok_or_else(|| format!("workspace disable: no data: {envelope}"))?;
    if string_at(data, "/scope", "workspace disable")? != "workspace" {
        return Err(format!("workspace disable: unexpected scope: {data}"));
    }
    if data.pointer("/applied") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("workspace disable: applied must be true: {data}"));
    }
    if data.pointer("/meshEnabledAfter") != Some(&serde_json::Value::Bool(false)) {
        return Err(format!(
            "workspace disable: meshEnabledAfter must be false: {data}"
        ));
    }
    let config_text = fs::read_to_string(&config_path)
        .map_err(|error| format!("workspace disable: reread config: {error}"))?;
    if !config_text.contains("enabled = false") || !config_text.contains("command_mode = \"off\"") {
        return Err(format!(
            "workspace disable: config must persist enabled=false and command_mode=off; got:\n{config_text}"
        ));
    }
    Ok(())
}
