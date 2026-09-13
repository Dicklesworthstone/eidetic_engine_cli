//! bd-i0iiw.3 - read-only harness hook install-audit coverage.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Output, Stdio};

use ee::hooks::{HarnessHookInstallOptions, HarnessHookTarget, generate_harness_hook_install};
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

#[cfg(unix)]
fn isolated_command(program: &str, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_EMBED_MODEL_DIR", root.join("no-model"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("EE_AMBIENT_CONTEXT", "true")
        .env("EE_AMBIENT_CONTEXT_VERBOSITY", "standard")
        .env("EE_AMBIENT_CONTEXT_STATE_DIR", root.join(".ee/hook-state"))
        .env("RUST_LOG", "off");
    command
}

#[cfg(unix)]
fn successful(output: Output) -> Result<Value, String> {
    if !output.status.success() {
        return Err(format!(
            "command failed: {:?}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "invalid JSON: {error}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[cfg(unix)]
#[test]
fn workspace_daemon_serves_real_hook_reads_and_falls_back_without_crossing_stores() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let root = temp.path();
    let binary = env!("CARGO_BIN_EXE_ee");
    let other = root.join("other");
    fs::create_dir_all(&other).map_err(|error| error.to_string())?;
    for (workspace, content) in [
        (root, "Release alpha checksums. anchor:path:src/release.rs"),
        (
            other.as_path(),
            "Release beta signatures. anchor:path:src/release.rs",
        ),
    ] {
        successful(
            isolated_command(binary, workspace)
                .args(["init", "--workspace", ".", "--json"])
                .output()
                .map_err(|error| error.to_string())?,
        )?;
        successful(
            isolated_command(binary, workspace)
                .args([
                    "remember",
                    content,
                    "--level",
                    "procedural",
                    "--kind",
                    "rule",
                    "--json",
                ])
                .output()
                .map_err(|error| error.to_string())?,
        )?;
    }
    let orient = [
        "orient",
        "release",
        "--fast",
        "--include-primer",
        "--format",
        "hook",
        "--fields",
        "command,ambientContext",
        "--max-tokens",
        "1072",
        "--use-daemon",
    ];
    let recall = [
        "recall",
        "--path",
        "src/release.rs",
        "--budget-tokens",
        "400",
        "--format",
        "hook",
        "--fields",
        "command,ambientContext",
        "--use-daemon",
    ];
    let read = |workspace: &Path, args: &[&str], socket: Option<&Path>| -> Result<Value, String> {
        let mut command = isolated_command(binary, workspace);
        command.args(args);
        if let Some(socket) = socket {
            command.arg("--daemon-socket").arg(socket);
        }
        successful(command.output().map_err(|error| error.to_string())?)
    };
    let has_fallback = |response: &Value| {
        response["degraded"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["code"] == "daemon_memory_read_fallback")
        })
    };
    let missing = read(root, &orient, Some(&root.join("missing.sock")))?;
    assert!(
        has_fallback(&missing),
        "missing daemon must be observable: {missing}"
    );
    assert!(
        missing["data"]["ambientContext"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("alpha checksums")
    );
    successful(
        isolated_command(binary, root)
            .env("EE_DAEMON_WARM", "off")
            .args(["daemon", "start", "--json"])
            .output()
            .map_err(|error| error.to_string())?,
    )?;
    let checks = std::panic::catch_unwind(|| -> TestResult {
        for args in [&orient[..], &recall[..]] {
            let warm = read(root, args, None)?;
            assert!(
                !has_fallback(&warm),
                "same-workspace RPC must execute successfully: {warm}"
            );
            let text = warm["data"]["ambientContext"]["text"]
                .as_str()
                .ok_or("ambient text missing")?;
            assert!(
                text.contains("alpha checksums"),
                "real memory missing: {warm}"
            );
            assert!(!text.contains("beta signatures"));
            let mismatch = read(
                &other,
                args,
                Some(&ee::daemon::workspace_daemon_socket_path(root)),
            )?;
            assert!(
                has_fallback(&mismatch),
                "wrong-workspace daemon must be refused: {mismatch}"
            );
            let text = mismatch["data"]["ambientContext"]["text"]
                .as_str()
                .ok_or("fallback text missing")?;
            assert!(
                text.contains("beta signatures"),
                "fallback must read the requested workspace: {mismatch}"
            );
            assert!(!text.contains("alpha checksums"));
        }
        // Exercise both actual generated memory-read snippets against the daemon.
        let mut install = options(
            HarnessHookTarget::Codex,
            &root.join("managed-hooks.json"),
            false,
            false,
        );
        install.ee_binary_path = Some(PathBuf::from(binary));
        let report = generate_harness_hook_install(&install).map_err(|error| error.message())?;
        for (event_name, surface, tool_input) in [
            (
                "SessionStart",
                "session_start_orient",
                serde_json::json!({}),
            ),
            (
                "PreToolUse",
                "pre_edit_recall",
                serde_json::json!({"file_path": root.join("src/release.rs")}),
            ),
        ] {
            let snippet = report
                .snippets
                .iter()
                .find(|snippet| snippet.event == event_name)
                .ok_or("memory-read snippet missing")?;
            let mut child = isolated_command("sh", root)
                .args(["-c", &snippet.command])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| error.to_string())?;
            child.stdin.take().ok_or("hook stdin missing")?.write_all(serde_json::json!({"cwd": root, "session_id": "daemon", "task": "release", "hook_event_name": event_name, "tool_name": "Edit", "tool_input": tool_input}).to_string().as_bytes()).map_err(|error| error.to_string())?;
            let hook = successful(
                child
                    .wait_with_output()
                    .map_err(|error| error.to_string())?,
            )?;
            assert!(
                hook["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("alpha checksums")
            );
            let state: Value = serde_json::from_slice(
                &fs::read(root.join(format!(".ee/hook-state/{surface}.last.json")))
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(state["outcome"], "emitted");
            assert!(
                !state["degradedCodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|code| code == "daemon_memory_read_fallback")
            );
        }
        Ok(())
    });
    let stopped = successful(
        isolated_command(binary, root)
            .args(["daemon", "stop", "--json"])
            .output()
            .map_err(|error| error.to_string())?,
    );
    stopped?;
    match checks {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(unix)]
#[test]
fn generated_session_hook_delivers_context_and_records_runtime_outcomes() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let root = temp.path();
    let binary = env!("CARGO_BIN_EXE_ee");
    successful(
        isolated_command(binary, root)
            .args(["init", "--workspace", ".", "--json"])
            .output()
            .map_err(|error| error.to_string())?,
    )?;
    for content in [
        "Verify release checksums before publishing.",
        "Run release tests on the target host.",
        "Keep release artifacts reproducible.",
    ] {
        successful(
            isolated_command(binary, root)
                .args([
                    "remember",
                    content,
                    "--workspace",
                    ".",
                    "--level",
                    "procedural",
                    "--kind",
                    "rule",
                    "--source",
                    "file://release-notes.md",
                    "--json",
                ])
                .output()
                .map_err(|error| error.to_string())?,
        )?;
    }
    let settings = root.join("hooks.json");
    let mut install = options(HarnessHookTarget::Codex, &settings, true, false);
    install.ee_binary_path = Some(PathBuf::from(binary));
    let report = generate_harness_hook_install(&install).map_err(|error| error.message())?;
    let snippet = report
        .snippets
        .iter()
        .find(|snippet| snippet.event == "SessionStart")
        .ok_or("SessionStart snippet missing")?;
    let invoke = |command: &str, session: &str| -> Result<Output, String> {
        let mut child = isolated_command("sh", root)
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let event = serde_json::json!({"cwd": root, "session_id": session, "task": "prepare release", "hook_event_name": "SessionStart"});
        child
            .stdin
            .take()
            .ok_or("missing stdin")?
            .write_all(event.to_string().as_bytes())
            .map_err(|error| error.to_string())?;
        child.wait_with_output().map_err(|error| error.to_string())
    };
    let response = successful(invoke(&snippet.command, "first")?)?;
    let context = response["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .ok_or("hook must inject context")?;
    assert!(
        context.contains("Verify release checksums"),
        "actual stored rule must reach the hook: {context}"
    );
    assert!(
        context.contains("file://release-notes.md"),
        "context must preserve original source provenance"
    );
    assert!(
        !context.contains("output_budget_unsatisfiable"),
        "hook must not inject a withheld-payload diagnostic"
    );
    assert!(
        ee::pack::estimate_tokens_default(context) <= 1200,
        "full injected context, including header, must fit the installed budget"
    );
    let state_path = root.join(".ee/hook-state/session_start_orient.last.json");
    let state: Value =
        serde_json::from_slice(&fs::read(&state_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    assert_eq!(state["outcome"], "emitted");
    assert_eq!(state["emittedBytes"], context.len());
    let duplicate = invoke(&snippet.command, "first")?;
    assert!(duplicate.status.success());
    assert!(
        duplicate.stdout.is_empty(),
        "same-session context must be deduplicated"
    );

    // A real exec failure must remain fail-open but be visible even though
    // the installed settings still contain the fresh, working snippet.
    let mut broken = install.clone();
    broken.install = false;
    broken.ee_binary_path = Some(root.join("missing-ee"));
    let broken_report = generate_harness_hook_install(&broken).map_err(|error| error.message())?;
    let broken_command = &broken_report
        .snippets
        .iter()
        .find(|snippet| snippet.event == "SessionStart")
        .ok_or("broken test snippet missing")?
        .command;
    let failed = invoke(broken_command, "second")?;
    assert!(
        failed.status.success(),
        "hook failures must leave the harness running"
    );
    assert!(failed.stdout.is_empty());
    let audit = successful(
        isolated_command(binary, root)
            .args(["hook", "status", "--settings-path"])
            .arg(&settings)
            .args(["--ee-binary", binary, "--json"])
            .output()
            .map_err(|error| error.to_string())?,
    )?;
    assert_eq!(audit["data"]["installAudit"]["status"], "fresh");
    assert!(
        audit["data"]["installAudit"]["findings"]
            .as_array()
            .ok_or("missing findings")?
            .iter()
            .any(|finding| finding["code"] == "hook_invocation_failed")
    );
    assert_eq!(
        audit["data"]["lastInvocations"][0]["outcome"],
        "command_error"
    );
    Ok(())
}

fn options(
    target: HarnessHookTarget,
    settings_path: &Path,
    install: bool,
    undo: bool,
) -> HarnessHookInstallOptions {
    HarnessHookInstallOptions {
        target,
        workspace: settings_path
            .parent()
            .unwrap_or_else(|| Path::new("/tmp"))
            .to_path_buf(),
        settings_path: Some(settings_path.to_path_buf()),
        install,
        undo,
        ee_binary_path: Some(PathBuf::from("/usr/local/bin/ee")),
    }
}

fn audit_status(target: HarnessHookTarget, settings_path: &Path) -> Result<String, String> {
    let report = generate_harness_hook_install(&options(target, settings_path, false, false))
        .map_err(|error| error.message())?;
    Ok(report.install_audit.status)
}

#[test]
fn install_audit_reports_missing_fresh_and_docs() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let settings_path = temp.path().join("codex-hooks.json");

    let missing = generate_harness_hook_install(&options(
        HarnessHookTarget::Codex,
        &settings_path,
        false,
        false,
    ))
    .map_err(|error| error.message())?;
    if missing.install_audit.status != "missing_hook" {
        return Err(format!(
            "missing config should report missing_hook, got {}",
            missing.install_audit.status
        ));
    }
    if missing.install_audit.hook_missing_count == 0 {
        return Err("missing audit should count missing installable hooks".to_owned());
    }
    if !missing.written_paths.is_empty() || settings_path.exists() {
        return Err("read-only audit must not write a missing settings file".to_owned());
    }
    for doc_id in ["recall_hooks", "primer_hooks", "journal_hooks"] {
        if !missing
            .install_audit
            .docs
            .iter()
            .any(|doc| doc.id == doc_id)
        {
            return Err(format!("missing audit doc link {doc_id}"));
        }
    }
    if !missing
        .install_audit
        .repair_plan
        .iter()
        .any(|repair| repair.action == "install_or_refresh_hooks" && repair.mutates_state)
    {
        return Err("missing audit should emit explicit install repair plan".to_owned());
    }

    generate_harness_hook_install(&options(
        HarnessHookTarget::Codex,
        &settings_path,
        true,
        false,
    ))
    .map_err(|error| error.message())?;
    let fresh = generate_harness_hook_install(&options(
        HarnessHookTarget::Codex,
        &settings_path,
        false,
        false,
    ))
    .map_err(|error| error.message())?;
    if fresh.install_audit.status != "fresh" {
        return Err(format!(
            "installed hooks should report fresh, got {}",
            fresh.install_audit.status
        ));
    }
    if fresh.install_audit.hook_fresh_count == 0 || fresh.install_audit.hook_stale_count != 0 {
        return Err("fresh audit should count fresh hooks and no stale hooks".to_owned());
    }
    if !fresh.install_audit.repair_plan.is_empty() {
        return Err("fresh audit should not emit repair actions".to_owned());
    }

    Ok(())
}

#[test]
fn install_audit_reclaims_exact_commands_without_metadata() -> TestResult {
    for target in [HarnessHookTarget::ClaudeCode, HarnessHookTarget::Codex] {
        let temp = TempDir::new().map_err(|error| error.to_string())?;
        let settings_path = temp.path().join("settings.json");
        let install = options(target, &settings_path, true, false);
        let initial = generate_harness_hook_install(&install).map_err(|error| error.message())?;
        let mut document: Value =
            serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        for groups in document["hooks"].as_object_mut().unwrap().values_mut() {
            for group in groups.as_array_mut().unwrap() {
                group.as_object_mut().unwrap().remove("eeManaged");
            }
        }
        fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();

        let repaired = generate_harness_hook_install(&install).map_err(|error| error.message())?;
        assert_eq!(repaired.install_audit.status, "fresh");
        assert_eq!(repaired.install_audit.hook_fresh_count, 4);
        let repaired_bytes = fs::read(&settings_path).unwrap();
        let repaired_document: Value = serde_json::from_slice(&repaired_bytes).unwrap();
        for snippet in &initial.snippets {
            let groups = repaired_document["hooks"][&snippet.event]
                .as_array()
                .unwrap();
            assert_eq!(
                groups.len(),
                1,
                "metadata loss must not append another group"
            );
            assert_eq!(groups[0]["eeManaged"], initial.markers.entry_marker);
            let hooks = groups[0]["hooks"].as_array().unwrap();
            assert_eq!(hooks.len(), 1);
            assert_eq!(hooks[0]["type"], "command");
            assert_eq!(hooks[0]["command"], snippet.command);
        }
        let repeated = generate_harness_hook_install(&install).map_err(|error| error.message())?;
        assert!(repeated.written_paths.is_empty());
        assert_eq!(fs::read(&settings_path).unwrap(), repaired_bytes);
    }
    Ok(())
}

#[test]
fn install_audit_detects_and_repairs_duplicate_commands_per_event() -> TestResult {
    for target in [HarnessHookTarget::ClaudeCode, HarnessHookTarget::Codex] {
        for (same_group, strip_first_marker, strip_duplicate_marker) in [
            (false, false, false),
            (false, false, true),
            (false, true, true),
            (true, false, false),
            (true, true, true),
        ] {
            let temp = TempDir::new().map_err(|error| error.to_string())?;
            let settings_path = temp.path().join("settings.json");
            let install = options(target, &settings_path, true, false);
            generate_harness_hook_install(&install).map_err(|error| error.message())?;
            let mut document: Value =
                serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
            for groups in document["hooks"].as_object_mut().unwrap().values_mut() {
                let groups = groups.as_array_mut().unwrap();
                let mut duplicate = groups[0].clone();
                if strip_duplicate_marker {
                    duplicate.as_object_mut().unwrap().remove("eeManaged");
                }
                if strip_first_marker {
                    groups[0].as_object_mut().unwrap().remove("eeManaged");
                }
                if same_group {
                    groups[0]["hooks"]
                        .as_array_mut()
                        .unwrap()
                        .push(duplicate["hooks"][0].clone());
                } else {
                    groups.push(duplicate);
                }
            }
            let duplicated_bytes = serde_json::to_vec_pretty(&document).unwrap();
            fs::write(&settings_path, &duplicated_bytes).unwrap();
            let audit =
                generate_harness_hook_install(&options(target, &settings_path, false, false))
                    .map_err(|error| error.message())?;
            assert!(audit.read_only);
            assert_eq!(fs::read(&settings_path).unwrap(), duplicated_bytes);
            assert_eq!(audit.install_audit.status, "stale_hook");
            assert_eq!(audit.install_audit.hook_fresh_count, 0);
            assert_eq!(audit.install_audit.hook_stale_count, 4);
            assert_eq!(audit.install_audit.hook_missing_count, 0);
            let duplicates: Vec<_> = audit
                .install_audit
                .findings
                .iter()
                .filter(|finding| finding.code == "duplicate_hook")
                .collect();
            assert_eq!(duplicates.len(), 4, "each duplicate event needs a finding");
            assert!(
                duplicates
                    .iter()
                    .all(|finding| finding.message.starts_with("2 ee-managed"))
            );
            assert!(!audit.install_audit.repair_plan.is_empty());

            let repaired =
                generate_harness_hook_install(&install).map_err(|error| error.message())?;
            assert_eq!(repaired.install_audit.status, "fresh");
            let repaired_bytes = fs::read(&settings_path).unwrap();
            let repaired_document: Value = serde_json::from_slice(&repaired_bytes).unwrap();
            for snippet in &repaired.snippets {
                let groups = repaired_document["hooks"][&snippet.event]
                    .as_array()
                    .unwrap();
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0]["hooks"].as_array().unwrap().len(), 1);
                assert_eq!(groups[0]["hooks"][0]["command"], snippet.command);
            }
            let repeated =
                generate_harness_hook_install(&install).map_err(|error| error.message())?;
            assert!(repeated.written_paths.is_empty());
            assert_eq!(fs::read(&settings_path).unwrap(), repaired_bytes);
        }
    }
    Ok(())
}

#[test]
fn install_audit_preserves_mixed_groups_and_near_matching_user_hooks() -> TestResult {
    for (marked_group, singleton_user_hook) in [(false, false), (true, false), (true, true)] {
        let temp = TempDir::new().map_err(|error| error.to_string())?;
        let settings_path = temp.path().join("settings.json");
        let install = options(HarnessHookTarget::ClaudeCode, &settings_path, true, false);
        let generated = generate_harness_hook_install(&options(
            HarnessHookTarget::ClaudeCode,
            &settings_path,
            false,
            false,
        ))
        .map_err(|error| error.message())?;
        let snippet = generated
            .snippets
            .iter()
            .find(|snippet| snippet.event == "SessionStart")
            .unwrap();
        let mut user_hooks = serde_json::json!([
            {"type": "command", "command": format!("{} # user extension", snippet.command), "timeout": 77},
            {"type": "command", "command": format!("echo {}", generated.markers.entry_marker)},
            {"type": "prompt", "command": snippet.command, "prompt": "Preserve this prompt", "timeout": 73}
        ]);
        if singleton_user_hook {
            user_hooks.as_array_mut().unwrap().truncate(1);
        }
        let user_group = serde_json::json!({
            "matcher": "compact", "timeout": 42, "description": "user group settings",
            "hooks": user_hooks
        });
        let mut mixed_group = user_group.clone();
        mixed_group["hooks"].as_array_mut().unwrap().insert(
            1,
            serde_json::json!({
                "type": "command", "command": snippet.command, "timeout": snippet.timeout_seconds
            }),
        );
        if marked_group {
            mixed_group["eeManaged"] = generated.markers.entry_marker.clone().into();
        }
        let other_group = serde_json::json!({
            "eeManaged": format!("{}-user", generated.markers.entry_marker),
            "hooks": [{"type": "command", "command": "echo unrelated", "timeout": 91}],
            "description": generated.markers.entry_marker
        });
        let unrelated_setting = serde_json::json!({"theme": "user-choice"});
        let document = serde_json::json!({
            "userSettings": unrelated_setting,
            "hooks": {"SessionStart": [mixed_group, other_group]}
        });
        fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        let repaired = generate_harness_hook_install(&install).map_err(|error| error.message())?;
        assert_eq!(repaired.install_audit.status, "fresh");
        let repaired_bytes = fs::read(&settings_path).unwrap();
        let repaired_document: Value = serde_json::from_slice(&repaired_bytes).unwrap();
        assert_eq!(repaired_document["userSettings"], unrelated_setting);
        let groups = repaired_document["hooks"]["SessionStart"]
            .as_array()
            .unwrap();
        assert_eq!(groups.len(), 3);
        assert_eq!(
            groups[0], user_group,
            "preserve every unrelated hook and group setting"
        );
        assert_eq!(
            groups[1], other_group,
            "marker substrings are not ownership"
        );
        assert_eq!(groups[2]["hooks"].as_array().unwrap().len(), 1);
        assert_eq!(groups[2]["hooks"][0]["command"], snippet.command);
        let repeated = generate_harness_hook_install(&install).map_err(|error| error.message())?;
        assert!(repeated.written_paths.is_empty());
        assert_eq!(fs::read(&settings_path).unwrap(), repaired_bytes);
    }
    Ok(())
}

#[test]
fn install_audit_rejects_malformed_settings_without_rewriting_them() -> TestResult {
    for (contents, message) in [
        ("{invalid", "not valid JSON"),
        ("[]", "must be a JSON object"),
        (r#"{"hooks":[]}"#, "`hooks` must be an object"),
        (
            r#"{"hooks":{"PostToolUse":{}}}"#,
            "`PostToolUse` must be an array",
        ),
    ] {
        let temp = TempDir::new().map_err(|error| error.to_string())?;
        let settings_path = temp.path().join("settings.json");
        fs::write(&settings_path, contents).unwrap();
        let error = generate_harness_hook_install(&options(
            HarnessHookTarget::ClaudeCode,
            &settings_path,
            true,
            false,
        ))
        .unwrap_err();
        assert_eq!(error.code(), "configuration");
        assert!(error.message().contains(message), "{}", error.message());
        assert_eq!(fs::read_to_string(&settings_path).unwrap(), contents);
    }
    Ok(())
}

#[test]
fn install_audit_reports_stale_managed_hooks() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let settings_path = temp.path().join("claude-settings.json");
    generate_harness_hook_install(&options(
        HarnessHookTarget::ClaudeCode,
        &settings_path,
        true,
        false,
    ))
    .map_err(|error| error.message())?;

    let mut document: Value =
        serde_json::from_slice(&fs::read(&settings_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    document["hooks"]["PreToolUse"][0]["hooks"][0]["command"] =
        Value::String("python3 -c 'print(\"stale\")'".to_owned());
    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let report = generate_harness_hook_install(&options(
        HarnessHookTarget::ClaudeCode,
        &settings_path,
        false,
        false,
    ))
    .map_err(|error| error.message())?;
    if report.install_audit.status != "stale_hook" {
        return Err(format!(
            "modified managed hook should report stale_hook, got {}",
            report.install_audit.status
        ));
    }
    if !report
        .install_audit
        .findings
        .iter()
        .any(|finding| finding.code == "stale_hook")
    {
        return Err("stale audit should include a stale_hook finding".to_owned());
    }
    if !report
        .install_audit
        .repair_plan
        .iter()
        .any(|repair| repair.action == "install_or_refresh_hooks")
    {
        return Err("stale audit should include refresh repair action".to_owned());
    }
    Ok(())
}

#[test]
fn install_audit_reports_unsupported_and_unwritable_config() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let gemini_path = temp.path().join("gemini-settings.json");
    if audit_status(HarnessHookTarget::Gemini, &gemini_path)? != "unsupported_harness_version" {
        return Err("Gemini should report unsupported_harness_version".to_owned());
    }

    let readonly_path = temp.path().join("readonly-codex-hooks.json");
    fs::write(&readonly_path, "{}\n").map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(&readonly_path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&readonly_path, permissions).map_err(|error| error.to_string())?;

    let status = audit_status(HarnessHookTarget::Codex, &readonly_path)?;
    let mut permissions = fs::metadata(&readonly_path)
        .map_err(|error| error.to_string())?
        .permissions();
    // Test cleanup: restore writability so the tempdir can be removed; the
    // broad-permission caveat of set_readonly(false) is irrelevant here.
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&readonly_path, permissions).map_err(|error| error.to_string())?;

    if status != "config_not_writable" {
        return Err(format!(
            "read-only settings file should report config_not_writable, got {status}"
        ));
    }
    Ok(())
}
