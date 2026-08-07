//! Regression coverage for removal of the zsh shell-interceptor surface.
//!
//! Hook discovery and direct invocation must both prove that ee no longer
//! generates shell command interceptors.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn zsh_preflight_shell_interceptor_is_not_a_public_command() -> Result<(), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["hook", "preflight-shell", "--shell", "zsh"])
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
fn hook_help_does_not_advertise_a_shell_interceptor() -> Result<(), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["hook", "--help"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ee hook --help failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("preflight-shell") {
        return Err(format!(
            "ee hook --help still advertises the removed interceptor: {stdout}"
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

use ee::hooks::{PreflightHookShell, PreflightHookShellOptions, generate_preflight_shell_snippet};
use tempfile::{Builder as TempDirBuilder, TempDir};

type TestResult = Result<(), String>;

/// Emit a tracing checkpoint with the bd-3usjw.58 standard field set so
/// the closure-lint / tracing-fields gate sees structured evidence in
/// every file the bd-3usjw.7 FILE SURFACE declares. Mirrors the
/// `trace_trauma_guard_hook_helper` shape used in
/// `src/hooks/installer.rs`.
fn trace_zsh_preflight_hook(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "tests/preflight_hook_zsh",
        request_id = "preflight_hook_zsh_integration",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.7"),
        surface = "trauma_guard_hook_helper",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "preflight zsh hook test checkpoint"
    );
}

fn zsh_or_skip() -> Option<String> {
    let zsh = std::env::var("EE_TEST_ZSH").unwrap_or_else(|_| "zsh".to_owned());
    let probe = Command::new(&zsh).arg("--version").output();
    match probe {
        Ok(out) if out.status.success() => Some(zsh),
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
    trace_zsh_preflight_hook("input", 0, &[]);
    let options = PreflightHookShellOptions {
        shell: Some(PreflightHookShell::Zsh),
        ee_binary_path: Some(ee_binary_path.to_path_buf()),
        install_dir: Some(dir.to_path_buf()),
    };
    let report = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
    let snippet_path = dir.join("preflight.zsh");
    fs::write(&snippet_path, &report.snippet).map_err(|e| e.to_string())?;
    trace_zsh_preflight_hook(
        "persistence",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        &[],
    );
    Ok((snippet_path, report.version))
}

fn write_stub_ee_binary(dir: &Path, severity: &str, exit_code: i32) -> Result<PathBuf, String> {
    let stub_path = dir.join("ee");
    let script = format!(
        r#"#!/usr/bin/env bash
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
fn zsh_snippet_syntax_check_passes() -> TestResult {
    let Some(zsh) = zsh_or_skip() else {
        eprintln!("skipping: zsh not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-zsh-")?;
    let stub_path = write_stub_ee_binary(temp.path(), "high", 7)?;
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;

    let output = Command::new(&zsh)
        .arg("-n")
        .arg(&snippet_path)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "zsh -n failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

#[test]
fn zsh_snippet_source_defines_hook_function_and_activation_flag() -> TestResult {
    let Some(zsh) = zsh_or_skip() else {
        eprintln!("skipping: zsh not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-zsh-")?;
    let stub_path = write_stub_ee_binary(temp.path(), "high", 7)?;
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;

    let script = format!(
        "PS1=test\nsource {snippet}\nif typeset -f __ee_preflight_hook_check >/dev/null; then \
            print HOOK_DEFINED; fi\nprint ACTIVE=${{EE_PREFLIGHT_HOOK_ACTIVE:-unset}}\n",
        snippet = snippet_path.display(),
    );
    let output = Command::new(&zsh)
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
fn zsh_snippet_invokes_stub_ee_with_expected_arguments_via_function_call() -> TestResult {
    let Some(zsh) = zsh_or_skip() else {
        eprintln!("skipping: zsh not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-zsh-")?;
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

    // Direct-invoke the hook function with the would-be command line. zsh's
    // preexec fires with the typed command line as $1; we mirror that here
    // without needing an interactive zsh subshell.
    let script = format!(
        "PS1=test\nsource {snippet}\n\
         __ee_preflight_hook_check 'rm -rf /tmp/test'\n\
         print rc=$?",
        snippet = snippet_path.display(),
    );
    let output = Command::new(&zsh)
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
    if !output.status.success() || !stdout.contains("rc=0") {
        return Err(format!(
            "advisory hook must return success after ee exits 7; status={:?}, stderr={stderr}, stdout={stdout}",
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
fn zsh_snippet_checks_cd_prefixed_compound_lines() -> TestResult {
    let Some(zsh) = zsh_or_skip() else {
        eprintln!("skipping: zsh not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-zsh-")?;
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
        "PS1=test\nsource {snippet}\n\
         __ee_preflight_hook_check 'cd /tmp && rm -rf /tmp/test'\n\
         print rc=$?",
        snippet = snippet_path.display(),
    );
    let output = Command::new(&zsh)
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stderr.contains("[ee preflight advisory]") {
        return Err(format!(
            "expected cd-prefixed compound command to reach ee preflight; stderr={stderr}, stdout={stdout}"
        ));
    }
    if !output.status.success() || !stdout.contains("rc=0") {
        return Err(format!(
            "advisory hook must allow the compound command after exit 7; status={:?}, stderr={stderr}, stdout={stdout}",
            output.status.code()
        ));
    }

    let argv_text = fs::read_to_string(&argv_log).map_err(|e| e.to_string())?;
    let argv_lines: Vec<&str> = argv_text.lines().collect();
    let expected = [
        "preflight",
        "check",
        "--cmd",
        "cd /tmp && rm -rf /tmp/test",
        "--json",
    ];
    if argv_lines != expected {
        return Err(format!(
            "stub ee received unexpected argv for cd-prefixed compound command: {argv_lines:?}, expected {expected:?}"
        ));
    }
    Ok(())
}

#[test]
fn zsh_snippet_treats_preflight_exit_7_as_advisory() -> TestResult {
    let Some(zsh) = zsh_or_skip() else {
        eprintln!("skipping: zsh not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-zsh-")?;
    let stub_path = write_stub_ee_binary(temp.path(), "medium", 7)?;
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;

    let script = format!(
        "PS1=test\nsource {snippet}\n\
         __ee_preflight_hook_check 'rm -rf /tmp/test'",
        snippet = snippet_path.display(),
    );
    let output = Command::new(&zsh)
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
            "exit 7 must remain advisory and return success; stdout={stdout}, stderr={stderr}"
        ));
    }
    if stderr.contains("Proceed anyway?") || stderr.contains("Blocked by user.") {
        return Err(format!(
            "advisory zsh hook must not prompt or claim to block; stdout={stdout}, stderr={stderr}"
        ));
    }
    Ok(())
}

#[test]
fn zsh_hook_allows_candidate_command_after_preflight_exit_7() -> TestResult {
    let Some(zsh) = zsh_or_skip() else {
        eprintln!("skipping: zsh not available on PATH");
        return Ok(());
    };
    let temp = worker_local_tempdir("ee-preflight-zsh-")?;
    let stub_path = write_stub_ee_binary(temp.path(), "critical", 7)?;
    let (snippet_path, _) = write_snippet_to_temp(temp.path(), &stub_path)?;
    let marker = temp.path().join("candidate-ran");

    let script = format!(
        "set -e\nPS1=test\nsource {snippet}\n\
         __ee_preflight_hook_check 'candidate command'\n\
         print -n candidate-ran > {marker}\n",
        snippet = snippet_path.display(),
        marker = marker.display(),
    );
    let output = Command::new(&zsh)
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
fn zsh_snippet_is_byte_stable_across_runs_for_pinned_binary_path() -> TestResult {
    let pinned = PathBuf::from("/usr/local/bin/ee");
    let options = PreflightHookShellOptions {
        shell: Some(PreflightHookShell::Zsh),
        ee_binary_path: Some(pinned),
        install_dir: Some(PathBuf::from("/home/test/.local/share/ee/hooks")),
    };
    let first = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
    let second = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
    assert_eq!(
        first.snippet, second.snippet,
        "zsh snippet must be byte-stable"
    );
    assert_eq!(
        first.version, second.version,
        "zsh snippet version must be byte-stable"
    );
    Ok(())
}

#[test]
fn zsh_snippet_carries_documented_contract_markers() -> TestResult {
    // Structural golden mirroring `tests/golden/hook_preflight_zsh.snap`.
    let pinned = PathBuf::from("/usr/local/bin/ee");
    let options = PreflightHookShellOptions {
        shell: Some(PreflightHookShell::Zsh),
        ee_binary_path: Some(pinned),
        install_dir: Some(PathBuf::from("/home/test/.local/share/ee/hooks")),
    };
    let report = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
    let required = [
        "#!/usr/bin/env zsh",
        "ee advisory preflight hook",
        "surface=trauma_guard_hook_helper",
        "EE_PREFLIGHT_HOOK_BINARY='/usr/local/bin/ee'",
        "autoload -Uz add-zsh-hook",
        "__ee_preflight_hook_check()",
        "preflight check \\\n            --cmd \"$_ee_cmd\" --json",
        "[ee preflight advisory]",
        "return 0",
        "add-zsh-hook preexec __ee_preflight_hook_check",
    ];
    let missing: Vec<&&str> = required
        .iter()
        .filter(|needle| !report.snippet.contains(*needle))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "zsh snippet missing required contract markers: {missing:?}\n----- snippet -----\n{}\n-------------------",
            report.snippet
        ));
    }
    let forbidden = [
        "EE_PREFLIGHT_HOOK_BLOCK_SEVERITIES",
        "kill -INT $$",
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
            "zsh advisory snippet contains blocking markers {present:?}:\n{}",
            report.snippet
        ));
    }
    Ok(())
}

}
