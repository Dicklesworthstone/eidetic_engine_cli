//! bd-3usjw.7 — `ee hook preflight-shell --shell zsh` integration tests.
//!
//! Mirror of `tests/preflight_hook_bash.rs` for the zsh flavor. Notes on
//! mechanism: zsh's `preexec` hook fires before each user command but,
//! unlike bash's DEBUG trap with `shopt -s extdebug`, it cannot natively
//! cancel the upcoming command. The snippet documents this and uses
//! `kill -INT $$` as the blocking mechanism in interactive shells.

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
    // without needing an interactive zsh subshell. The /dev/tty read fails
    // (no tty), so the prompt defaults to N and the function reaches
    // `kill -INT $$`. We run the call inside `( ... )` so the SIGINT
    // terminates only the subshell, not the test runner.
    let script = format!(
        "PS1=test\nsource {snippet}\n\
         ( __ee_preflight_hook_check 'rm -rf /tmp/test' ) || true\n\
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
    if !stderr.contains("[ee preflight]") {
        return Err(format!(
            "expected '[ee preflight]' warning in stderr; got stderr={stderr}, stdout={stdout}"
        ));
    }
    if !stderr.contains("test-fire") {
        return Err(format!(
            "snippet did not surface the stub-ee message; stderr={stderr}"
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
fn zsh_snippet_treats_preflight_exit_7_as_authoritative() -> TestResult {
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

    if !stderr.contains("severity=medium") {
        return Err(format!(
            "expected medium-severity exit-7 result to be surfaced; stderr={stderr}, stdout={stdout}"
        ));
    }
    if output.status.success() {
        return Err(format!(
            "expected zsh hook to interrupt the shell on policy-denied exit 7; stdout={stdout}, stderr={stderr}"
        ));
    }
    if !stderr.contains("Blocked by user.") {
        return Err(format!(
            "expected zsh hook to reach the default-block path; stdout={stdout}, stderr={stderr}"
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
        "surface=trauma_guard_hook_helper",
        "EE_PREFLIGHT_HOOK_BINARY='/usr/local/bin/ee'",
        "autoload -Uz add-zsh-hook",
        "__ee_preflight_hook_check()",
        "preflight check \\\n            --cmd \"$_ee_cmd\" --json",
        "add-zsh-hook preexec __ee_preflight_hook_check",
        "kill -INT $$",
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
    if report
        .snippet
        .contains("EE_PREFLIGHT_HOOK_BLOCK_SEVERITIES")
    {
        return Err(format!(
            "zsh snippet must not embed a stale client-side severity allowlist:\n{}",
            report.snippet
        ));
    }
    Ok(())
}
