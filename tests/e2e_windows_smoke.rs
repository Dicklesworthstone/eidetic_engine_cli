//! Windows MSVC walking-skeleton smoke test.
//!
//! Bead: bd-3usjw.68. The real Windows CI job runs this target on
//! `windows-latest` with the MSVC target and verifies that core CLI commands
//! operate in a workspace rooted under `%LOCALAPPDATA%`.

#[cfg(windows)]
mod windows {
    use serde_json::Value;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    type TestResult = Result<(), String>;

    fn run_ee(args: &[String]) -> Result<Output, String> {
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .args(args)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY")
            .env("EE_TEST_SEED", "bd-3usjw.68-windows-msvc-cli-smoke")
            .output()
            .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
    }

    fn command(workspace: &Path, args: &[&str]) -> Vec<String> {
        let mut command = vec![
            "--workspace".to_owned(),
            workspace.display().to_string(),
            "--json".to_owned(),
        ];
        command.extend(args.iter().map(|arg| (*arg).to_owned()));
        command
    }

    fn stdout_json(output: &Output, context: &str) -> Result<Value, String> {
        let stdout = String::from_utf8(output.stdout.clone())
            .map_err(|error| format!("{context}: stdout is not UTF-8: {error}"))?;
        serde_json::from_str(&stdout)
            .map_err(|error| format!("{context}: stdout is not JSON: {error}\nstdout: {stdout}"))
    }

    fn run_success(workspace: &Path, args: &[&str], context: &str) -> Result<Value, String> {
        let output = run_ee(&command(workspace, args))?;
        if !output.status.success() {
            return Err(format!(
                "{context} failed with exit {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        stdout_json(&output, context)
    }

    fn path_env(name: &str) -> Result<PathBuf, String> {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("{name} must be set on Windows CI"))
    }

    #[test]
    fn windows_msvc_cli_walking_skeleton_smoke() -> TestResult {
        let appdata = path_env("APPDATA")?;
        let localappdata = path_env("LOCALAPPDATA")?;
        assert!(
            appdata.is_absolute(),
            "APPDATA should resolve to an absolute path: {}",
            appdata.display()
        );
        assert!(
            localappdata.is_absolute(),
            "LOCALAPPDATA should resolve to an absolute path: {}",
            localappdata.display()
        );

        let workspace = localappdata.join("ee-windows-msvc-cli-smoke");
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("create workspace {}: {error}", workspace.display()))?;

        run_success(&workspace, &["init"], "init")?;
        let remember = run_success(
            &workspace,
            &[
                "remember",
                "Run cargo fmt --check before release on Windows MSVC.",
                "--level",
                "procedural",
                "--kind",
                "rule",
            ],
            "remember",
        )?;
        let memory_id = remember
            .pointer("/data/memory_id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("remember missing memory_id: {remember}"))?
            .to_owned();

        run_success(&workspace, &["search", "Windows release fmt"], "search")?;
        run_success(
            &workspace,
            &["context", "prepare Windows release"],
            "context",
        )?;
        run_success(&workspace, &["why", &memory_id], "why")?;
        run_success(&workspace, &["status"], "status")?;
        run_success(&workspace, &["doctor"], "doctor")?;

        let export_dir = workspace.join("export");
        run_success(
            &workspace,
            &[
                "export",
                "--output-dir",
                &export_dir.display().to_string(),
                "--redaction",
                "none",
            ],
            "export",
        )?;

        let backup_dir = workspace.join("backups");
        let backup = run_success(
            &workspace,
            &[
                "backup",
                "create",
                "--output-dir",
                &backup_dir.display().to_string(),
                "--redaction",
                "none",
                "--label",
                "windows-msvc-cli-smoke",
            ],
            "backup create",
        )?;
        let backup_id = backup
            .pointer("/data/backupId")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("backup create missing backupId: {backup}"))?
            .to_owned();
        let restore_dir = workspace.join("restore-side-path");
        run_success(
            &workspace,
            &[
                "backup",
                "restore",
                &backup_id,
                "--output-dir",
                &backup_dir.display().to_string(),
                "--side-path",
                &restore_dir.display().to_string(),
            ],
            "backup restore",
        )?;

        let capsule = workspace.join("handoff-capsule.json");
        run_success(
            &workspace,
            &[
                "handoff",
                "create",
                "--out",
                &capsule.display().to_string(),
                "--profile",
                "resume",
            ],
            "handoff create",
        )?;
        run_success(
            &workspace,
            &["handoff", "resume", &capsule.display().to_string()],
            "handoff resume",
        )?;

        assert!(
            workspace.starts_with(&localappdata),
            "workspace should stay under LOCALAPPDATA"
        );
        Ok(())
    }
}

#[cfg(not(windows))]
#[test]
fn windows_msvc_cli_smoke_is_windows_only() {
    assert!(cfg!(not(windows)));
}
