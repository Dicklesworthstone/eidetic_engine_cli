//! Regression coverage for removal of the bash shell-interceptor surface.
//!
//! `ee preflight check` remains available as an explicit advisory memory
//! query, but `ee hook preflight-shell` must not be exposed or generated.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn bash_preflight_shell_interceptor_is_not_a_public_command() -> Result<(), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["hook", "preflight-shell", "--shell", "bash"])
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Err(format!(
            "removed ee hook preflight-shell unexpectedly succeeded: {}",
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    Ok(())
}

#[test]
fn explicit_preflight_memory_query_remains_available() -> Result<(), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["preflight", "check", "--help"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "explicit advisory preflight query disappeared: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("advisory") {
        return Err(format!(
            "preflight check help must describe advisory memory semantics: {stdout}"
        ));
    }
    Ok(())
}

#[rustfmt::skip]
#[cfg(any())]
mod removed_preflight_shell_interceptor_contract {
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use ee::hooks::{
    PREFLIGHT_HOOK_SHELL_SCHEMA_V1, PreflightHookShell, PreflightHookShellOptions,
    generate_preflight_shell_snippet,
};
use serde_json::Value;
use tempfile::{Builder as TempDirBuilder, TempDir};

type TestResult = Result<(), String>;

/// Emit a tracing checkpoint with the bd-3usjw.58 standard field set so
/// the closure-lint / tracing-fields gate sees structured evidence in
/// every file the bd-3usjw.7 FILE SURFACE declares. Mirrors the
/// `trace_trauma_guard_hook_helper` shape used in
/// `src/hooks/installer.rs`.
fn trace_bash_preflight_hook(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "tests/preflight_hook_bash",
        request_id = "preflight_hook_bash_integration",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.7"),
        surface = "trauma_guard_hook_helper",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "preflight bash hook test checkpoint"
    );
}

fn bash_or_skip() -> Option<String> {
    let bash = std::env::var("EE_TEST_BASH").unwrap_or_else(|_| "bash".to_owned());
    let probe = Command::new(&bash).arg("--version").output();
    match probe {
        Ok(out) if out.status.success() => Some(bash),
        _ => None,
    }
}

fn worker_local_tempdir(prefix: &str) -> Result<TempDir, String> {
    // RCH workers are Linux hosts. If the Mac-side TMPDIR points at the
    // USB-NVMe mount, TempDir::new() inherits a path that does not exist on
    // the worker after sync. Keep these integration temp files worker-local.
    let tmp_root = Path::new("/tmp");
    if tmp_root.is_dir() {
        TempDirBuilder::new()
            .prefix(prefix)
            .tempdir_in(tmp_root)
            .map_err(|e| e.to_string())
    } else {
        TempDirBuilder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|e| e.to_string())
    }
}

fn write_snippet_to_temp(dir: &Path, ee_binary_path: &Path) -> Result<(PathBuf, String), String> {
    let started = Instant::now();
    trace_bash_preflight_hook("input", 0, &[]);
    let options = PreflightHookShellOptions {
        shell: Some(PreflightHookShell::Bash),
        ee_binary_path: Some(ee_binary_path.to_path_buf()),
        install_dir: Some(dir.to_path_buf()),
    };
    let report = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
    let snippet_path = dir.join("preflight.bash");
    fs::write(&snippet_path, &report.snippet).map_err(|e| e.to_string())?;
    trace_bash_preflight_hook(
        "persistence",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        &[],
    );
    Ok((snippet_path, report.version))
}

fn write_stub_ee_binary(dir: &Path, severity: &str, exit_code: i32) -> Result<PathBuf, String> {
    // The stub mimics `ee preflight check --cmd "<cmd>" --json` by emitting
    // a minimal preflight JSON envelope and exiting with the requested code.
    // Exit 7 exercises the historical policy-denied status. The shell hook
    // must treat every status as advisory and remain fail-open.
    let stub_path = dir.join("ee");
    let script = format!(
        r#"#!/usr/bin/env bash
# Stub `ee` binary for trauma_guard_hook_helper integration tests.
# Mimics the preflight-check exit-code + JSON contract.
echo "{{\"schema\":\"ee.preflight.v1\",\"severity\":\"{severity}\",\"message\":\"test-destructive-pattern-fired\"}}"
exit {exit_code}
"#
    );
    fs::write(&stub_path, script).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&stub_path)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub_path, perms).map_err(|e| e.to_string())?;
    }
    Ok(stub_path)
}

#[test]
fn bash_preflight_shell_cli_json_emits_real_service_hook_report() -> TestResult {
    let Some(bash) = bash_or_skip() else {
        eprintln!("skipping: bash not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-bash-cli-")?;
    let install_dir = temp.path().join("hooks");
    fs::create_dir_all(&install_dir).map_err(|e| e.to_string())?;
    let real_ee = PathBuf::from(env!("CARGO_BIN_EXE_ee"));

    let output = Command::new(&real_ee)
        .arg("hook")
        .arg("preflight-shell")
        .arg("--shell")
        .arg("bash")
        .arg("--ee-binary")
        .arg(&real_ee)
        .arg("--install-dir")
        .arg(&install_dir)
        .arg("--json")
        .env("HOME", temp.path().join("home"))
        .env("EE_NO_COLOR", "1")
        .output()
        .map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "ee hook preflight-shell failed: status={:?} stdout={stdout} stderr={stderr}",
            output.status.code()
        ));
    }
    if !stderr.is_empty() {
        return Err(format!(
            "ee hook preflight-shell must keep JSON-mode diagnostics out of stderr; stderr={stderr}"
        ));
    }

    let value: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("parse JSON: {e}"))?;
    let envelope_schema = value
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing response schema in {value}"))?;
    if !envelope_schema.starts_with("ee.response.v") {
        return Err(format!(
            "unexpected response schema for hook preflight-shell: {envelope_schema}"
        ));
    }
    if value.get("success") != Some(&Value::Bool(true)) {
        return Err(format!(
            "expected success=true in CLI JSON envelope: {value}"
        ));
    }

    let data = value
        .get("data")
        .ok_or_else(|| format!("missing data object in CLI JSON envelope: {value}"))?;
    if data.get("schema").and_then(Value::as_str) != Some(PREFLIGHT_HOOK_SHELL_SCHEMA_V1) {
        return Err(format!(
            "unexpected hook report schema: {:?}",
            data.get("schema")
        ));
    }
    if data.get("shell").and_then(Value::as_str) != Some("bash") {
        return Err(format!("unexpected shell in hook report: {data}"));
    }
    let real_ee_display = real_ee.display().to_string();
    if data.get("ee_binary_path").and_then(Value::as_str) != Some(real_ee_display.as_str()) {
        return Err(format!(
            "CLI report did not preserve requested ee binary path {real_ee_display}: {data}"
        ));
    }
    let expected_install_path = install_dir.join("preflight.bash").display().to_string();
    if data.get("install_path").and_then(Value::as_str) != Some(expected_install_path.as_str()) {
        return Err(format!(
            "CLI report did not preserve requested install path {expected_install_path}: {data}"
        ));
    }
    let snippet = data
        .get("snippet")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing snippet in hook report: {data}"))?;
    if !snippet.contains(&format!("EE_PREFLIGHT_HOOK_BINARY='{real_ee_display}'")) {
        return Err(format!(
            "snippet did not embed requested real ee binary path {real_ee_display}:\n{snippet}"
        ));
    }

    let snippet_path = temp.path().join("cli-preflight.bash");
    fs::write(&snippet_path, snippet).map_err(|e| e.to_string())?;
    let syntax = Command::new(&bash)
        .arg("-n")
        .arg(&snippet_path)
        .output()
        .map_err(|e| e.to_string())?;
    if !syntax.status.success() {
        return Err(format!(
            "bash -n failed for CLI-generated snippet: stdout={} stderr={}",
            String::from_utf8_lossy(&syntax.stdout),
            String::from_utf8_lossy(&syntax.stderr),
        ));
    }
    Ok(())
}

#[test]
fn bash_snippet_syntax_check_passes() -> TestResult {
    let Some(bash) = bash_or_skip() else {
        eprintln!("skipping: bash not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-bash-")?;
    let stub_path = write_stub_ee_binary(temp.path(), "high", 7)?;
    let (snippet_path, _version) = write_snippet_to_temp(temp.path(), &stub_path)?;

    let output = Command::new(&bash)
        .arg("-n")
        .arg(&snippet_path)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "bash -n failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

#[test]
fn bash_snippet_source_defines_hook_function_and_activation_flag() -> TestResult {
    let Some(bash) = bash_or_skip() else {
        eprintln!("skipping: bash not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-bash-")?;
    let stub_path = write_stub_ee_binary(temp.path(), "high", 0)?;
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;

    // Forcing PS1 makes the interactive-shell guard inside the snippet pass,
    // so the function is actually installed (not short-circuited).
    let script = format!(
        "PS1=test\nsource {snippet}\ndeclare -F __ee_preflight_hook_check >/dev/null \
         && echo HOOK_DEFINED\necho ACTIVE=${{EE_PREFLIGHT_HOOK_ACTIVE:-unset}}\n",
        snippet = snippet_path.display(),
    );
    let output = Command::new(&bash)
        .arg("-c")
        .arg(&script)
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "subshell failed: status={:?} stdout={stdout} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    if !stdout.contains("HOOK_DEFINED") {
        return Err(format!(
            "__ee_preflight_hook_check not defined after source; stdout={stdout}"
        ));
    }
    if !stdout.contains("ACTIVE=1") {
        return Err(format!(
            "EE_PREFLIGHT_HOOK_ACTIVE not set after source; stdout={stdout}"
        ));
    }
    Ok(())
}

#[test]
fn bash_snippet_invokes_stub_ee_with_expected_arguments_via_function_call() -> TestResult {
    let Some(bash) = bash_or_skip() else {
        eprintln!("skipping: bash not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-bash-")?;
    // Stub records its argv so the test can verify the snippet calls
    // `ee preflight check --cmd <cmd> --json` with the right shape.
    let stub_path = temp.path().join("ee");
    let argv_log = temp.path().join("stub_argv.txt");
    let script_body = format!(
        r#"#!/usr/bin/env bash
printf '%s\n' "$@" > {argv}
echo '{{"schema":"ee.preflight.v1","severity":"high","message":"test-fire"}}'
exit 7
"#,
        argv = argv_log.display(),
    );
    fs::write(&stub_path, script_body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&stub_path)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub_path, perms).map_err(|e| e.to_string())?;
    }
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;

    // Direct-call the hook function with a synthetic BASH_COMMAND. This
    // exercises the advisory path without relying on an interactive tty.
    let script = format!(
        "PS1=test\nsource {snippet}\nBASH_COMMAND='rm -rf /tmp/test' \
         __ee_preflight_hook_check",
        snippet = snippet_path.display(),
    );
    let output = Command::new(&bash)
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stderr.contains("[ee preflight advisory]") {
        return Err(format!(
            "expected advisory output in stderr; got stderr={stderr}, stdout={stdout}"
        ));
    }
    if !stderr.contains("test-fire") {
        return Err(format!(
            "snippet did not surface the stub-ee message; stderr={stderr}"
        ));
    }
    if !output.status.success() {
        return Err(format!(
            "advisory hook must return success even when ee exits 7; status={:?}, stdout={stdout}, stderr={stderr}",
            output.status.code()
        ));
    }
    if stderr.contains("Proceed anyway?") || stderr.contains("Blocked by user.") {
        return Err(format!(
            "advisory hook must not prompt or claim to block; stderr={stderr}"
        ));
    }

    let argv_text = fs::read_to_string(&argv_log).map_err(|e| e.to_string())?;
    let argv_lines: Vec<&str> = argv_text.lines().collect();
    let expected = ["preflight", "check", "--cmd", "rm -rf /tmp/test", "--json"];
    if argv_lines != expected {
        return Err(format!(
            "stub ee received unexpected argv: {argv_lines:?}, expected {expected:?}"
        ));
    }
    Ok(())
}

#[test]
fn bash_snippet_checks_echo_command_substitution_lines() -> TestResult {
    let Some(bash) = bash_or_skip() else {
        eprintln!("skipping: bash not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-bash-")?;
    let stub_path = temp.path().join("ee");
    let argv_log = temp.path().join("stub_argv.txt");
    let script_body = format!(
        r#"#!/usr/bin/env bash
printf '%s\n' "$@" > {argv}
echo '{{"schema":"ee.preflight.v1","severity":"high","message":"test-fire"}}'
exit 7
"#,
        argv = argv_log.display(),
    );
    fs::write(&stub_path, script_body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&stub_path)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub_path, perms).map_err(|e| e.to_string())?;
    }
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;

    let script = format!(
        "PS1=test\nsource {snippet}\nBASH_COMMAND='echo $(rm -rf /tmp/test)' \
         __ee_preflight_hook_check",
        snippet = snippet_path.display(),
    );
    let output = Command::new(&bash)
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stderr.contains("[ee preflight advisory]") {
        return Err(format!(
            "expected echo command substitution to reach ee preflight; stderr={stderr}, stdout={stdout}"
        ));
    }
    if !output.status.success() {
        return Err(format!(
            "advisory hook must allow echo command substitution after exit 7; status={:?}, stdout={stdout}, stderr={stderr}",
            output.status.code()
        ));
    }

    let argv_text = fs::read_to_string(&argv_log).map_err(|e| e.to_string())?;
    let argv_lines: Vec<&str> = argv_text.lines().collect();
    let expected = [
        "preflight",
        "check",
        "--cmd",
        "echo $(rm -rf /tmp/test)",
        "--json",
    ];
    if argv_lines != expected {
        return Err(format!(
            "stub ee received unexpected argv for echo substitution: {argv_lines:?}, expected {expected:?}"
        ));
    }
    Ok(())
}

#[test]
fn bash_snippet_treats_preflight_exit_7_as_advisory() -> TestResult {
    let Some(bash) = bash_or_skip() else {
        eprintln!("skipping: bash not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-bash-")?;
    let stub_path = write_stub_ee_binary(temp.path(), "medium", 7)?;
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;

    let script = format!(
        "PS1=test\nsource {snippet}\nBASH_COMMAND='rm -rf /tmp/test' \
         __ee_preflight_hook_check",
        snippet = snippet_path.display(),
    );
    let output = Command::new(&bash)
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    if !stderr.contains("severity") || !stderr.contains("medium") {
        return Err(format!(
            "expected medium-severity exit-7 result to be surfaced; stderr={stderr}, stdout={stdout}"
        ));
    }
    if !output.status.success() {
        return Err(format!(
            "exit 7 must remain advisory and return success; status={:?}, stdout={stdout}, stderr={stderr}",
            output.status.code()
        ));
    }
    Ok(())
}

#[test]
fn bash_debug_trap_allows_candidate_command_after_preflight_exit_7() -> TestResult {
    let Some(bash) = bash_or_skip() else {
        eprintln!("skipping: bash not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-bash-")?;
    let stub_path = write_stub_ee_binary(temp.path(), "critical", 7)?;
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;
    let marker = temp.path().join("candidate-ran");

    let script = format!(
        "set -e\nPS1=test\nsource {snippet}\nprintf candidate-ran > {marker}\n",
        snippet = snippet_path.display(),
        marker = marker.display(),
    );
    let output = Command::new(&bash)
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "candidate command failed under errexit after advisory exit 7: status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let marker_text = fs::read_to_string(&marker).map_err(|e| e.to_string())?;
    if marker_text != "candidate-ran" {
        return Err(format!(
            "candidate command did not run after advisory exit 7; marker={marker_text:?}"
        ));
    }
    Ok(())
}

#[test]
fn bash_snippet_is_byte_stable_across_runs_for_pinned_binary_path() -> TestResult {
    let pinned = PathBuf::from("/usr/local/bin/ee");
    let options = PreflightHookShellOptions {
        shell: Some(PreflightHookShell::Bash),
        ee_binary_path: Some(pinned),
        install_dir: Some(PathBuf::from("/home/test/.local/share/ee/hooks")),
    };
    let first = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
    let second = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
    assert_eq!(
        first.snippet, second.snippet,
        "bash snippet must be byte-stable"
    );
    assert_eq!(
        first.version, second.version,
        "bash snippet version must be byte-stable"
    );
    Ok(())
}

#[test]
fn bash_snippet_carries_documented_contract_markers() -> TestResult {
    // Structural golden: instead of pinning byte-exact bytes (which would
    // require a verified RCH run to seed and then thrash on every comment
    // tweak), assert the load-bearing contract markers documented in
    // `tests/golden/hook_preflight_bash.snap`. The inline determinism
    // tests under `src/hooks/installer.rs` already cover byte-stability
    // across runs.
    let pinned = PathBuf::from("/usr/local/bin/ee");
    let options = PreflightHookShellOptions {
        shell: Some(PreflightHookShell::Bash),
        ee_binary_path: Some(pinned),
        install_dir: Some(PathBuf::from("/home/test/.local/share/ee/hooks")),
    };
    let report = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
    let required = [
        "#!/usr/bin/env bash",
        "ee advisory preflight hook",
        "surface=trauma_guard_hook_helper",
        "EE_PREFLIGHT_HOOK_BINARY='/usr/local/bin/ee'",
        "__ee_preflight_hook_check()",
        "preflight check \\\n            --cmd \"$BASH_COMMAND\" --json",
        "[ee preflight advisory]",
        "return 0",
        "trap '__ee_preflight_hook_check' DEBUG",
    ];
    let missing: Vec<&&str> = required
        .iter()
        .filter(|needle| !report.snippet.contains(*needle))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "bash snippet missing required contract markers: {missing:?}\n----- snippet -----\n{}\n-------------------",
            report.snippet
        ));
    }
    let forbidden = [
        "EE_PREFLIGHT_HOOK_BLOCK_SEVERITIES",
        "shopt -s extdebug",
        "Proceed anyway?",
        "Blocked by user.",
        "return 1",
    ];
    let present: Vec<&&str> = forbidden
        .iter()
        .filter(|needle| report.snippet.contains(*needle))
        .collect();
    if !present.is_empty() {
        return Err(format!(
            "bash advisory snippet contains blocking markers {present:?}:\n{}",
            report.snippet
        ));
    }
    Ok(())
}

}
