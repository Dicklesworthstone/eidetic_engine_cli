//! Windows MSVC walking-skeleton smoke test.
//!
//! Bead: bd-3usjw.68. The real Windows CI job runs this target on
//! `windows-latest` with the MSVC target and verifies that core CLI commands
//! operate in a workspace rooted under `%LOCALAPPDATA%`.

mod windows {
    use serde_json::Value;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    type TestResult = Result<(), String>;

    fn run_ee(args: &[String]) -> Result<Output, String> {
        let temp = tempfile::tempdir().map_err(|error| format!("isolated tempdir: {error}"))?;
        let iso = args
            .windows(2)
            .find_map(|window| {
                (window[0] == "--workspace").then(|| {
                    let ws = Path::new(&window[1]);
                    let name = ws.file_name().and_then(|n| n.to_str()).unwrap_or("ws");
                    ws.parent()
                        .unwrap_or(ws)
                        .join(format!(".ee-isolated-{name}"))
                })
            })
            .unwrap_or_else(|| temp.path().to_path_buf());
        let exe = std::env::var_os("EE_BINARY")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ee")));
        Command::new(exe)
            .args(args)
            .env("HOME", iso.join("home"))
            .env("XDG_DATA_HOME", iso.join("xdg-data"))
            .env("XDG_CONFIG_HOME", iso.join("xdg-config"))
            .env("APPDATA", iso.join("appdata-roaming"))
            .env("LOCALAPPDATA", iso.join("appdata-local"))
            .env("EE_EMBED_DOWNLOAD", "off")
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

    fn stdout_json_with_schema(
        output: &Output,
        expected_schema: &str,
        context: &str,
    ) -> Result<Value, String> {
        let stdout = String::from_utf8(output.stdout.clone())
            .map_err(|error| format!("{context}: stdout is not UTF-8: {error}"))?;
        let parsed: Value = serde_json::from_str(&stdout)
            .map_err(|error| format!("{context}: stdout is not JSON: {error}\nstdout: {stdout}"))?;
        if expected_schema == "ee.response.v2" {
            if parsed.get("schema").and_then(Value::as_str) != Some("ee.response.v2")
                || parsed.get("success").and_then(Value::as_bool) != Some(true)
            {
                return Err(format!(
                    "{context}: expected ee.response.v2 success=true envelope, got: {parsed}"
                ));
            }
        } else if parsed.get("schema").and_then(Value::as_str) != Some(expected_schema) {
            return Err(format!(
                "{context}: expected schema={expected_schema}, got: {parsed}"
            ));
        }
        Ok(parsed)
    }

    fn run_success(workspace: &Path, args: &[&str], context: &str) -> Result<Value, String> {
        run_schema_success(workspace, args, "ee.response.v2", context)
    }

    fn run_schema_success(
        workspace: &Path,
        args: &[&str],
        expected_schema: &str,
        context: &str,
    ) -> Result<Value, String> {
        let output = run_ee(&command(workspace, args))?;
        if !output.status.success() {
            return Err(format!(
                "{context} failed with exit {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        stdout_json_with_schema(&output, expected_schema, context)
    }

    fn path_env_or_temp(name: &str, fallback_root: &Path, leaf: &str) -> Result<PathBuf, String> {
        if let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(value));
        }
        if cfg!(windows) {
            return Err(format!("{name} must be set on Windows CI"));
        }
        let dir = fallback_root.join(leaf);
        fs::create_dir_all(&dir)
            .map_err(|error| format!("create fallback {name} dir {}: {error}", dir.display()))?;
        Ok(dir)
    }

    #[test]
    fn windows_msvc_cli_walking_skeleton_smoke() -> TestResult {
        let fallback_temp =
            tempfile::tempdir().map_err(|error| format!("appdata tempdir: {error}"))?;
        let appdata = path_env_or_temp("APPDATA", fallback_temp.path(), "AppDataRoaming")?;
        let localappdata = path_env_or_temp("LOCALAPPDATA", fallback_temp.path(), "AppDataLocal")?;
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
        run_success(&workspace, &["pack", "prepare Windows release"], "pack")?;
        run_success(&workspace, &["why", &memory_id], "why")?;
        run_success(&workspace, &["status"], "status")?;
        run_success(&workspace, &["doctor"], "doctor")?;
        let created = run_success(
            &workspace,
            &["team", "create", "--name", "Windows Smoke"],
            "team create",
        )?;
        let team_id = created
            .pointer("/data/team/teamId")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("team create missing teamId: {created}"))?;
        assert!(
            team_id.starts_with("team_"),
            "team create should persist a team_ id: {created}"
        );
        assert_eq!(
            created.pointer("/data/created").and_then(Value::as_bool),
            Some(true),
            "team create should mint a local genesis: {created}"
        );
        run_success(&workspace, &["team", "status"], "team status")?;
        run_success(&workspace, &["team", "doctor"], "team doctor")?;

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
        let restore_dir = localappdata.join("ee-windows-msvc-cli-smoke-restore-side-path");
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
        let handoff_created = run_schema_success(
            &workspace,
            &[
                "handoff",
                "create",
                "--out",
                &capsule.display().to_string(),
                "--profile",
                "resume",
            ],
            "ee.handoff.create.v1",
            "handoff create",
        )?;
        assert!(
            handoff_created
                .get("capsule_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("hcap_")),
            "handoff create should emit capsule_id: {handoff_created}"
        );
        let handoff_resumed = run_schema_success(
            &workspace,
            &["handoff", "resume", &capsule.display().to_string()],
            "ee.handoff.resume.v1",
            "handoff resume",
        )?;
        assert!(
            handoff_resumed
                .get("capsule_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("hcap_")),
            "handoff resume should emit capsule_id: {handoff_resumed}"
        );

        assert!(
            workspace.starts_with(&localappdata),
            "workspace should stay under LOCALAPPDATA"
        );
        Ok(())
    }
}

#[test]
fn powershell_smoke_preserves_machine_stream_and_artifact_contracts() {
    let script = include_str!("../scripts/e2e_windows_smoke.ps1");

    assert!(
        script.contains("& $EeBinary @allArgs 1> $stdoutPath 2> $stderrPath"),
        "PowerShell smoke must capture native stdout and stderr separately"
    );
    assert!(
        !script.contains("$output = & $EeBinary @allArgs 2>&1"),
        "PowerShell smoke must not merge diagnostics into machine JSON"
    );
    assert!(
        script.contains("$json.schema -ne \"ee.response.v2\"")
            && script.contains("$json.success -ne $true"),
        "PowerShell smoke must validate the canonical success envelope"
    );
    assert!(
        script.contains("-Name \"11-handoff-create\" -ExpectedSchema \"ee.handoff.create.v1\"")
            && script
                .contains("-Name \"12-handoff-resume\" -ExpectedSchema \"ee.handoff.resume.v1\""),
        "PowerShell smoke must validate direct handoff create/resume schemas"
    );
    assert!(
        script.contains("Invoke-Ee -Name \"04-pack\" -Arguments @(\"pack\""),
        "PowerShell smoke must exercise the canonical pack command"
    );
    assert!(
        script.contains("Invoke-Ee -Name \"07b-team-create\" -Arguments @(\"team\", \"create\""),
        "PowerShell smoke must create a local team through the hardened Windows key store"
    );
    assert!(
        script.contains("remember response did not contain data.memory_id"),
        "PowerShell smoke must fail clearly when remember omits its identifier"
    );
    assert!(
        !script.contains("Remove-Item"),
        "PowerShell smoke must refuse stale artifacts instead of deleting them"
    );
    assert!(
        script.contains("$RestoreSidePath = \"$WorkspaceRoot-restore-side-path\"")
            && !script.contains("(Join-Path $WorkspaceRoot \"restore-side-path\")"),
        "PowerShell smoke must place backup restore --side-path outside WorkspaceRoot"
    );
}
