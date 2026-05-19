use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root().join("scripts/rch_verify.sh")
}

fn target_tmp_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/rch-verify-contract"))
}

fn run_script_with_env(
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(std::process::ExitStatus, String, String), String> {
    run_script_with_env_in_dir(args, envs, &repo_root())
}

fn run_script_with_env_in_dir(
    args: &[&str],
    envs: &[(&str, &str)],
    cwd: &Path,
) -> Result<(std::process::ExitStatus, String, String), String> {
    let mut command = Command::new("bash");
    command
        .arg(script_path())
        .args(args)
        .env("RCH_VERIFY_NOW", "2026-05-16T04:40:00.000000Z")
        .current_dir(cwd);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("run rch verifier wrapper: {error}"))?;
    Ok((
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn run_script(args: &[&str]) -> Result<(std::process::ExitStatus, String, String), String> {
    run_script_with_env(args, &[])
}

fn run_json(args: &[&str]) -> Result<Value, String> {
    let (status, stdout, stderr) = run_script(args)?;
    if !status.success() {
        return Err(format!(
            "script failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    serde_json::from_str(&stdout).map_err(|error| format!("parse wrapper JSON: {error}"))
}

fn degraded_contains(report: &Value, expected: &str) -> Result<bool, String> {
    Ok(report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?
        .iter()
        .any(|code| code == expected))
}

fn source_degraded_contains(report: &Value, expected: &str) -> Result<bool, String> {
    Ok(report["source_state_degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing source-state degraded codes".to_owned())?
        .iter()
        .any(|code| code == expected))
}

fn worker_degraded_contains(report: &Value, expected: &str) -> Result<bool, String> {
    Ok(report["worker_state_degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing worker-state degraded codes".to_owned())?
        .iter()
        .any(|code| code == expected))
}

fn unique_tmp_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    target_tmp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
}

fn git(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("run git {args:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_status_porcelain_v2(workspace: &Path) -> Result<String, String> {
    git(
        workspace,
        &[
            "status",
            "--porcelain=v2",
            "--untracked-files=all",
            "--ignored=no",
        ],
    )
}

fn assert_git_status_unchanged(
    workspace: &Path,
    before: &str,
    context: &str,
) -> Result<(), String> {
    let after = git_status_porcelain_v2(workspace)?;
    if after != before {
        return Err(format!(
            "{context} mutated caller checkout status\nbefore:\n{before}\nafter:\n{after}"
        ));
    }
    Ok(())
}

fn seed_git_workspace(label: &str) -> Result<PathBuf, String> {
    let workspace = unique_tmp_path(label);
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("create workspace {}: {error}", workspace.display()))?;
    git(&workspace, &["init"])?;
    git(&workspace, &["config", "user.name", "RCH Verify Test"])?;
    git(
        &workspace,
        &["config", "user.email", "rch-verify-test@example.invalid"],
    )?;
    fs::write(workspace.join("tracked.txt"), "seed\n")
        .map_err(|error| format!("write tracked fixture: {error}"))?;
    fs::write(workspace.join(".gitignore"), "._*\n")
        .map_err(|error| format!("write fixture gitignore: {error}"))?;
    git(&workspace, &["add", ".gitignore", "tracked.txt"])?;
    git(&workspace, &["commit", "-m", "seed"])?;
    Ok(workspace)
}

fn write_fake_rch(name: &str, body: &str) -> Result<PathBuf, String> {
    let dir = target_tmp_dir();
    fs::create_dir_all(&dir).map_err(|error| format!("create target temp dir: {error}"))?;
    let path = dir.join(name);
    fs::write(&path, body).map_err(|error| format!("write fake rch: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path)
            .map_err(|error| format!("stat fake rch: {error}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions)
            .map_err(|error| format!("chmod fake rch: {error}"))?;
    }
    Ok(path)
}

fn write_fake_build_admission_ee(name: &str, admitted: bool) -> Result<PathBuf, String> {
    let status = if admitted { "true" } else { "false" };
    let degraded = if admitted {
        "[]"
    } else {
        r#"[{"code":"build_admission_denied","severity":"medium","message":"workspace below threshold","repair":"ask human before cleanup"}]"#
    };
    write_fake_rch(
        name,
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
cat <<'JSON'
{{"schema":"ee.response.v1","success":true,"data":{{"schema":"ee.build_admission.diagnostics.v1","admitted":{status},"minFreeBytes":1073741824,"checks":[{{"label":"workspace","path":"/tmp/ws","bytesAvailable":1024,"minFreeBytes":1073741824,"admitted":{status},"externalRequired":false,"external":false}},{{"label":"cargo_target","path":"/Volumes/USBNVME16TB/temp_agent_space/cargo-target","bytesAvailable":9000000000000,"minFreeBytes":1073741824,"admitted":true,"externalRequired":true,"external":true}}],"degraded":{degraded}}}}}
JSON
"#,
        ),
    )
}

fn write_fake_build_admission_candidate(
    path: &Path,
    version_stdout: &str,
    admitted: bool,
) -> Result<(), String> {
    let status = if admitted { "true" } else { "false" };
    let degraded = if admitted {
        "[]"
    } else {
        r#"[{"code":"build_admission_denied","severity":"medium","message":"workspace below threshold","repair":"ask human before cleanup"}]"#
    };
    fs::write(
        path,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${{1:-}}" = "--version" ]; then
  printf '%s\n' {version_stdout:?}
  exit 0
fi
cat <<'JSON'
{{"schema":"ee.response.v1","success":true,"data":{{"schema":"ee.build_admission.diagnostics.v1","admitted":{status},"minFreeBytes":1073741824,"checks":[{{"label":"workspace","path":"/tmp/ws","bytesAvailable":9000000000,"minFreeBytes":1073741824,"admitted":{status},"externalRequired":false,"external":false}}],"degraded":{degraded}}}}}
JSON
"#,
        ),
    )
    .map_err(|error| format!("write fake ee candidate {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("stat fake ee candidate: {error}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("chmod fake ee candidate: {error}"))?;
    }
    Ok(())
}

#[test]
fn script_is_syntax_valid_and_uses_explicit_rch_exec() -> TestResult {
    let output = Command::new("bash")
        .arg("-n")
        .arg(script_path())
        .output()
        .map_err(|error| format!("bash -n failed to start: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }

    let text =
        fs::read_to_string(script_path()).map_err(|error| format!("read wrapper: {error}"))?;
    if !text.contains("\"$RCH_BIN\" \"exec\" \"--\"") {
        return Err("wrapper must use explicit rch exec".to_owned());
    }
    if text.contains("CARGO_TARGET_DIR=/Volumes/USBNVME16TB") {
        return Err("remote command must not embed the Mac USB target path".to_owned());
    }
    Ok(())
}

#[test]
fn script_body_avoids_forbidden_git_and_cleanup_operations() -> TestResult {
    let text =
        fs::read_to_string(script_path()).map_err(|error| format!("read wrapper: {error}"))?;
    let forbidden = [
        "git worktree",
        "git stash",
        "git reset",
        "git checkout",
        "git clean",
        "rm -rf",
        "rm -f",
    ];
    let mut in_policy_matcher = false;
    let mut violations = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("contains_forbidden_text()") {
            in_policy_matcher = true;
        }
        if in_policy_matcher {
            if trimmed == "}" {
                in_policy_matcher = false;
            }
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        for pattern in forbidden {
            if trimmed.contains(pattern) {
                violations.push(format!(
                    "line {} contains `{pattern}`: {trimmed}",
                    index + 1
                ));
            }
        }
    }

    if !violations.is_empty() {
        return Err(format!(
            "rch verifier wrapper must not use forbidden Git operations or deletion cleanup:\n{}",
            violations.join("\n")
        ));
    }
    Ok(())
}

#[test]
fn dry_run_accepts_focused_cargo_test_and_builds_remote_env() -> TestResult {
    let report = run_json(&[
        "--dry-run",
        "--",
        "cargo",
        "test",
        "--lib",
        "output::streaming",
        "--",
        "--nocapture",
    ])?;

    if report["schema"] != "ee.rch.verify.v1" {
        return Err("unexpected schema".to_owned());
    }
    if report["success"] != true {
        return Err("dry-run cargo test should succeed".to_owned());
    }
    if report["command_kind"] != "cargo_test" {
        return Err(format!("wrong command kind: {report}"));
    }
    if report["remote_required"] != true || report["would_offload"] != true {
        return Err("dry-run did not declare remote-only offload".to_owned());
    }
    let invocation = report["rch_invocation"]
        .as_array()
        .ok_or_else(|| "missing rch invocation".to_owned())?;
    let invocation_text = invocation
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if !invocation_text.contains("rch exec -- env TMPDIR=/tmp") {
        return Err(format!("unexpected invocation: {invocation_text}"));
    }
    if invocation_text.contains("/Volumes/USBNVME16TB") {
        return Err("dry-run remote invocation leaked Mac-only USB path".to_owned());
    }
    Ok(())
}

#[test]
fn strict_clean_tree_dry_run_reports_clean_source_state() -> TestResult {
    let workspace = seed_git_workspace("rch-strict-clean")?;
    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--require-clean-tree",
            "--dry-run",
            "--",
            "cargo",
            "test",
            "--lib",
            "strict_clean_tree_smoke",
        ],
        &[],
        &workspace,
    )?;
    if !status.success() {
        return Err(format!(
            "strict clean dry-run failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse strict clean: {error}"))?;
    if report["verification_attribution"] != "strict_clean_tree" {
        return Err(format!("wrong attribution for clean tree: {report}"));
    }
    if report["git_head"].as_str().map(str::len) != Some(40)
        || report["git_tree"].as_str().map(str::len) != Some(40)
    {
        return Err(format!("missing git source identity: {report}"));
    }
    if report["dirty_status_hash"]
        != "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    {
        return Err(format!(
            "clean tree should have empty status hash: {report}"
        ));
    }
    if report["dirty_summary"]["total"] != 0
        || report["dirty_paths_sample"] != serde_json::json!([])
        || report["source_state_degraded_codes"] != serde_json::json!([])
    {
        return Err(format!("clean tree reported dirty source state: {report}"));
    }
    if report["rch_invocation"]
        .as_array()
        .ok_or_else(|| "missing rch invocation".to_owned())?
        .is_empty()
    {
        return Err(format!("clean dry-run should still plan RCH: {report}"));
    }
    if !degraded_contains(&report, "rch_verify_dry_run")? {
        return Err(format!(
            "clean dry-run missing dry-run degradation: {report}"
        ));
    }
    Ok(())
}

#[test]
fn strict_clean_tree_refuses_tracked_dirty_source_before_rch() -> TestResult {
    let workspace = seed_git_workspace("rch-strict-tracked-dirty")?;
    fs::write(workspace.join("tracked.txt"), "dirty\n")
        .map_err(|error| format!("dirty tracked fixture: {error}"))?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let invocation_log = unique_tmp_path("rch-fake-refusal-invocations");
    let fake_rch = write_fake_rch(
        "fake-rch-should-not-run.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf 'REMOTE SHOULD NOT RUN\n'
printf '[RCH] remote css (0.1s)\n'
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--require-clean-tree",
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "strict_clean_tree_dirty_smoke",
        ],
        &[("FAKE_RCH_INVOCATIONS", invocation_log_arg)],
        &workspace,
    )?;
    assert_git_status_unchanged(
        &workspace,
        &before_status,
        "strict dirty-tree fake RCH refusal",
    )?;
    if status.success() {
        return Err(format!(
            "strict dirty tree should fail before RCH\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    if invocation_log.exists() {
        let invocations = fs::read_to_string(&invocation_log)
            .map_err(|error| format!("read refusal invocation log: {error}"))?;
        if !invocations.is_empty() {
            return Err(format!(
                "strict dirty-tree refusal should not invoke fake RCH: {invocations:?}"
            ));
        }
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse dirty tracked: {error}"))?;
    if report["status"] != "source_state_refused"
        || report["verification_attribution"] != "live_dirty_checkout"
        || report["exit_code"] != 1
        || report["elapsed_ms"] != 0
    {
        return Err(format!("unexpected dirty tracked refusal: {report}"));
    }
    if report["dirty_summary"]["tracked"] != 1 || report["dirty_summary"]["total"] != 1 {
        return Err(format!("tracked dirty counts were not precise: {report}"));
    }
    if report["dirty_summary"]["tracked_staged"] != 0
        || report["dirty_summary"]["tracked_unstaged"] != 1
    {
        return Err(format!(
            "unstaged tracked dirty counts were not precise: {report}"
        ));
    }
    for expected in [
        "rch_verify_dirty_tree_refused",
        "rch_verify_dirty_tracked_paths",
        "rch_verify_dirty_unstaged_paths",
    ] {
        if !degraded_contains(&report, expected)? || !source_degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in dirty refusal: {report}"));
        }
    }
    if report["worker_state_degraded_codes"] != serde_json::json!([]) {
        return Err(format!(
            "dirty source refusal should not report worker-state codes: {report}"
        ));
    }
    if report["rch_invocation"] != serde_json::json!([]) {
        return Err(format!(
            "strict refusal should not build RCH invocation: {report}"
        ));
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout tail".to_owned())?;
    if stdout_tail.contains("REMOTE SHOULD NOT RUN") {
        return Err(format!("strict refusal invoked fake RCH: {report}"));
    }
    Ok(())
}

#[test]
fn strict_clean_tree_refuses_staged_dirty_source_before_rch() -> TestResult {
    let workspace = seed_git_workspace("rch-strict-staged-dirty")?;
    fs::write(workspace.join("tracked.txt"), "staged dirty\n")
        .map_err(|error| format!("dirty staged fixture: {error}"))?;
    git(&workspace, &["add", "tracked.txt"])?;

    let (status, stdout, _stderr) = run_script_with_env_in_dir(
        &[
            "--require-clean-tree",
            "--dry-run",
            "--",
            "cargo",
            "test",
            "--lib",
            "strict_clean_tree_staged_smoke",
        ],
        &[],
        &workspace,
    )?;
    if status.success() {
        return Err("strict staged dirty tree should fail before dry-run planning".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse staged dirty: {error}"))?;
    if report["status"] != "source_state_refused" {
        return Err(format!(
            "staged dirty tree was not source refused: {report}"
        ));
    }
    if report["dirty_summary"]["tracked"] != 1 || report["dirty_summary"]["total"] != 1 {
        return Err(format!("staged dirty counts were not precise: {report}"));
    }
    if report["dirty_summary"]["tracked_staged"] != 1
        || report["dirty_summary"]["tracked_unstaged"] != 0
    {
        return Err(format!(
            "staged tracked dirty counts were not precise: {report}"
        ));
    }
    if !source_degraded_contains(&report, "rch_verify_dirty_tracked_paths")? {
        return Err(format!("missing staged tracked degradation: {report}"));
    }
    if !source_degraded_contains(&report, "rch_verify_dirty_staged_paths")? {
        return Err(format!("missing staged-state degradation: {report}"));
    }
    if degraded_contains(&report, "rch_verify_dry_run")? {
        return Err(format!(
            "strict source refusal should happen before dry-run proof: {report}"
        ));
    }
    Ok(())
}

#[test]
fn strict_clean_tree_classifies_beads_scratch_and_secret_risk_paths() -> TestResult {
    let workspace = seed_git_workspace("rch-strict-path-classes")?;
    fs::create_dir_all(workspace.join(".beads"))
        .map_err(|error| format!("create .beads fixture: {error}"))?;
    fs::write(workspace.join(".beads/issues.jsonl"), "{}\n")
        .map_err(|error| format!("write beads fixture: {error}"))?;
    fs::write(workspace.join("ubs.json"), "{}\n")
        .map_err(|error| format!("write ubs scratch fixture: {error}"))?;
    fs::write(workspace.join(".plan-drift-report.json"), "{}\n")
        .map_err(|error| format!("write plan drift scratch fixture: {error}"))?;
    fs::write(workspace.join("test_ln_1p.rs"), "fn main() {}\n")
        .map_err(|error| format!("write line-probe scratch fixture: {error}"))?;
    fs::write(workspace.join("credential-note.txt"), "redacted fixture\n")
        .map_err(|error| format!("write secret-risk path fixture: {error}"))?;

    let (status, stdout, _stderr) = run_script_with_env_in_dir(
        &[
            "--require-clean-tree",
            "--dry-run",
            "--",
            "cargo",
            "test",
            "--lib",
            "strict_clean_tree_path_classes",
        ],
        &[],
        &workspace,
    )?;
    if status.success() {
        return Err("strict path-class dirty tree should fail before RCH".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse path classes: {error}"))?;
    let summary = &report["dirty_summary"];
    if summary["total"] != 5
        || summary["beads"] != 1
        || summary["scratch"] != 3
        || summary["secret_risk"] != 1
    {
        return Err(format!("unexpected path classification counts: {report}"));
    }
    for expected in [
        "rch_verify_dirty_tree_refused",
        "rch_verify_dirty_beads_metadata",
        "rch_verify_dirty_untracked_scratch",
        "rch_verify_dirty_untracked_paths",
    ] {
        if !source_degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in source codes: {report}"));
        }
    }
    let sample = report["dirty_paths_sample"]
        .as_array()
        .ok_or_else(|| "missing dirty path sample".to_owned())?;
    for expected_path in [
        ".beads/issues.jsonl",
        ".plan-drift-report.json",
        "credential-note.txt",
        "test_ln_1p.rs",
        "ubs.json",
    ] {
        if !sample.iter().any(|entry| entry["path"] == expected_path) {
            return Err(format!("sample missing {expected_path}: {report}"));
        }
    }
    Ok(())
}

#[test]
fn strict_clean_tree_fake_rch_invokes_once_and_preserves_clean_checkout() -> TestResult {
    let workspace = seed_git_workspace("rch-strict-clean-fake-rch")?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let invocation_log = unique_tmp_path("rch-fake-invocations");
    let fake_rch = write_fake_rch(
        "fake-rch-records-invocation.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--require-clean-tree",
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "strict_clean_tree_fake_rch_smoke",
        ],
        &[
            ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "trj"),
            (
                "RCH_VERIFY_STATUS_JSON",
                r#"{"data":{"daemon":{"recent_builds":[]}}}"#,
            ),
        ],
        &workspace,
    )?;
    assert_git_status_unchanged(&workspace, &before_status, "strict clean-tree fake RCH")?;
    if !status.success() {
        return Err(format!(
            "strict clean fake RCH run failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse strict clean fake RCH: {error}"))?;
    if report["status"] != "remote_pass"
        || report["verification_attribution"] != "strict_clean_tree"
        || report["worker_id"] != "trj"
    {
        return Err(format!("unexpected strict clean fake-RCH report: {report}"));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read invocation log: {error}"))?;
    let lines = invocations.lines().collect::<Vec<_>>();
    if lines.len() != 1 {
        return Err(format!(
            "strict clean-tree should invoke fake RCH once, got {}: {lines:?}",
            lines.len()
        ));
    }
    if !lines[0].contains("exec -- env TMPDIR=/tmp CARGO_TARGET_DIR=/tmp/ee-rch-verify-target cargo test --lib strict_clean_tree_fake_rch_smoke") {
        return Err(format!("fake RCH invocation did not preserve explicit remote command: {lines:?}"));
    }
    Ok(())
}

#[test]
fn event_log_records_source_state_and_fake_rch_invocation_count() -> TestResult {
    let workspace = seed_git_workspace("rch-event-log-fake-rch")?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let invocation_log = unique_tmp_path("rch-event-log-invocations");
    let event_log = unique_tmp_path("rch-event-log").join("events.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-event-log.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf 'remote event-log ok\n'
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let event_log_arg = event_log
        .to_str()
        .ok_or_else(|| "event log path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--bead-id",
            "bd-9ygik.3",
            "--require-clean-tree",
            "--event-log",
            event_log_arg,
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "event_log_fake_rch_smoke",
        ],
        &[
            ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "trj"),
            (
                "RCH_VERIFY_STATUS_JSON",
                r#"{"data":{"daemon":{"recent_builds":[]}}}"#,
            ),
        ],
        &workspace,
    )?;
    assert_git_status_unchanged(&workspace, &before_status, "event-log fake RCH")?;
    if !status.success() {
        return Err(format!(
            "event-log fake RCH run failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse report: {error}"))?;
    if report["status"] != "remote_pass"
        || report["verification_attribution"] != "strict_clean_tree"
        || report["command_hash"].as_str().map(str::len) != Some(64)
    {
        return Err(format!("unexpected event-log proof report: {report}"));
    }

    let event_text =
        fs::read_to_string(&event_log).map_err(|error| format!("read event log: {error}"))?;
    let rows = event_text.lines().collect::<Vec<_>>();
    if rows.len() != 1 {
        return Err(format!("expected one event row, got {}", rows.len()));
    }
    let event: Value =
        serde_json::from_str(rows[0]).map_err(|error| format!("parse event row: {error}"))?;
    if event["schema"] != "ee.test_event.v1"
        || event["kind"] != "command_end"
        || event["test_id"] != "bd-9ygik.3"
        || event["command"] != "scripts/rch_verify.sh"
        || event["exit_code"] != 0
    {
        return Err(format!(
            "event row does not match test-event basics: {event}"
        ));
    }
    if event["stdout_hash"]
        .as_str()
        .is_none_or(|hash| !hash.starts_with("sha256:") || hash.len() != 71)
    {
        return Err(format!("event row missing stdout hash: {event}"));
    }
    let fields = &event["fields"];
    if fields["status"] != "remote_pass"
        || fields["bead_id"] != "bd-9ygik.3"
        || fields["verification_attribution"] != "strict_clean_tree"
        || fields["source_state_degraded_codes"] != serde_json::json!([])
        || fields["worker_state_degraded_codes"] != serde_json::json!([])
        || fields["fake_rch_invoked"] != true
        || fields["fake_rch_invocation_count"] != 1
        || fields["dirty_status_hash"]
            != "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    {
        return Err(format!("event row missing source/fake-RCH fields: {event}"));
    }
    Ok(())
}

#[test]
fn event_log_records_source_refusal_without_fake_rch_invocation() -> TestResult {
    let workspace = seed_git_workspace("rch-event-log-source-refusal")?;
    fs::write(workspace.join("tracked.txt"), "dirty source state\n")
        .map_err(|error| format!("dirty tracked fixture: {error}"))?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let invocation_log = unique_tmp_path("rch-event-log-refusal-invocations");
    let event_log = unique_tmp_path("rch-event-log-refusal").join("events.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-event-log-refusal-should-not-run.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let event_log_arg = event_log
        .to_str()
        .ok_or_else(|| "event log path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--bead-id",
            "bd-9ygik.3",
            "--require-clean-tree",
            "--event-log",
            event_log_arg,
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "event_log_source_refusal_smoke",
        ],
        &[("FAKE_RCH_INVOCATIONS", invocation_log_arg)],
        &workspace,
    )?;
    assert_git_status_unchanged(&workspace, &before_status, "event-log source refusal")?;
    if status.success() {
        return Err(format!(
            "dirty strict-clean tree should refuse before RCH\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    if invocation_log.exists() {
        let invocations = fs::read_to_string(&invocation_log)
            .map_err(|error| format!("read refusal invocation log: {error}"))?;
        if !invocations.is_empty() {
            return Err(format!(
                "source refusal should not invoke fake RCH: {invocations:?}"
            ));
        }
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse report: {error}"))?;
    if report["status"] != "source_state_refused"
        || report["verification_attribution"] != "live_dirty_checkout"
        || report["exit_code"] != 1
        || report["dirty_summary"]["tracked_unstaged"] != 1
    {
        return Err(format!("unexpected source-refusal proof report: {report}"));
    }
    for expected in [
        "rch_verify_dirty_tree_refused",
        "rch_verify_dirty_tracked_paths",
        "rch_verify_dirty_unstaged_paths",
    ] {
        if !source_degraded_contains(&report, expected)? || !degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in source-refusal report: {report}"
            ));
        }
    }

    let event_text =
        fs::read_to_string(&event_log).map_err(|error| format!("read event log: {error}"))?;
    let rows = event_text.lines().collect::<Vec<_>>();
    if rows.len() != 1 {
        return Err(format!(
            "expected one source-refusal event row, got {}",
            rows.len()
        ));
    }
    let event: Value =
        serde_json::from_str(rows[0]).map_err(|error| format!("parse event row: {error}"))?;
    if event["schema"] != "ee.test_event.v1"
        || event["kind"] != "command_end"
        || event["test_id"] != "bd-9ygik.3"
        || event["exit_code"] != 1
    {
        return Err(format!("event row does not record refusal basics: {event}"));
    }
    let fields = &event["fields"];
    if fields["status"] != "source_state_refused"
        || fields["bead_id"] != "bd-9ygik.3"
        || fields["verification_attribution"] != "live_dirty_checkout"
        || fields["fake_rch_invoked"] != false
        || fields["fake_rch_invocation_count"] != 0
        || fields["deterministic_rerun_hash"] != report["dirty_status_hash"]
        || fields["first_failure_diagnosis"] != "source_state_refused"
    {
        return Err(format!(
            "event row missing refusal/fake-RCH fields: {event}"
        ));
    }
    for expected in [
        "rch_verify_dirty_tree_refused",
        "rch_verify_dirty_tracked_paths",
        "rch_verify_dirty_unstaged_paths",
    ] {
        if !fields["source_state_degraded_codes"]
            .as_array()
            .ok_or_else(|| "missing event source-state degraded codes".to_owned())?
            .iter()
            .any(|code| code == expected)
        {
            return Err(format!("event row missing {expected}: {event}"));
        }
    }
    Ok(())
}

#[test]
fn committed_tree_manifest_ignores_dirty_checkout_and_runs_from_export() -> TestResult {
    let workspace = seed_git_workspace("rch-committed-tree-dirty")?;
    fs::write(workspace.join("tracked.txt"), "dirty live checkout\n")
        .map_err(|error| format!("dirty tracked fixture: {error}"))?;
    fs::write(workspace.join("credential-note.txt"), "redacted fixture\n")
        .map_err(|error| format!("write untracked secret-risk fixture: {error}"))?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let invocation_log = unique_tmp_path("rch-committed-tree-invocations");
    let fake_rch = write_fake_rch(
        "fake-rch-committed-tree-runs-from-export.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf 'PWD=%s\n' "$PWD"
printf 'tracked=%s\n' "$(cat tracked.txt)"
test ! -e credential-note.txt
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;

    let args = [
        "--committed-tree",
        "--treeish",
        "HEAD",
        "--rch-bin",
        fake_rch_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "committed_tree_smoke",
    ];
    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &args,
        &[("FAKE_RCH_INVOCATIONS", invocation_log_arg)],
        &workspace,
    )?;
    assert_git_status_unchanged(&workspace, &before_status, "committed-tree preflight")?;
    if !status.success() {
        return Err(format!(
            "committed-tree mode should run from the generated source export\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read committed-tree invocation log: {error}"))?;
    if invocations.lines().count() != 1 {
        return Err(format!(
            "committed-tree mode should invoke fake RCH once: {invocations:?}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse committed-tree report: {error}"))?;
    if report["status"] != "remote_pass"
        || report["verification_attribution"] != "committed_tree"
        || report["requested_treeish"] != "HEAD"
        || report["resolved_commit"].as_str().map(str::len) != Some(40)
        || report["git_tree"].as_str().map(str::len) != Some(40)
    {
        return Err(format!("unexpected committed-tree report: {report}"));
    }
    if report["dirty_summary"]["total"] != 0
        || report["dirty_paths_sample"] != serde_json::json!([])
    {
        return Err(format!(
            "committed-tree source proof should exclude live dirty paths: {report}"
        ));
    }
    if report["source_manifest_file_count"] != 2 || report["source_manifest_byte_count"] == 0 {
        return Err(format!("unexpected committed manifest counts: {report}"));
    }
    for expected in ["dirty_tracked", "untracked", "ignored"] {
        if !report["source_manifest_excluded_path_classes"]
            .as_array()
            .ok_or_else(|| "missing excluded path classes".to_owned())?
            .iter()
            .any(|class| class == expected)
        {
            return Err(format!("missing excluded class {expected}: {report}"));
        }
    }
    if source_degraded_contains(&report, "rch_verify_committed_tree_unsupported")?
        || degraded_contains(&report, "rch_verify_committed_tree_unsupported")?
    {
        return Err(format!(
            "simple committed-tree fixture unexpectedly remained unsupported: {report}"
        ));
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout_tail".to_owned())?;
    if !stdout_tail.contains("tracked=seed") || stdout_tail.contains("credential-note") {
        return Err(format!(
            "committed-tree verifier did not run from clean committed export: {report}"
        ));
    }
    let first_manifest_hash = report["source_manifest_hash"]
        .as_str()
        .ok_or_else(|| "missing source manifest hash".to_owned())?
        .to_owned();

    fs::write(workspace.join("tracked.txt"), "different dirty content\n")
        .map_err(|error| format!("rewrite dirty tracked fixture: {error}"))?;
    fs::write(workspace.join("new-token-file.txt"), "redacted fixture\n")
        .map_err(|error| format!("write second untracked fixture: {error}"))?;
    let (second_status, second_stdout, _second_stderr) = run_script_with_env_in_dir(
        &args,
        &[("FAKE_RCH_INVOCATIONS", invocation_log_arg)],
        &workspace,
    )?;
    if !second_status.success() {
        return Err("second committed-tree run should still succeed".to_owned());
    }
    let second_report: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse second committed-tree report: {error}"))?;
    if second_report["source_manifest_hash"] != first_manifest_hash {
        return Err(format!(
            "committed-tree manifest changed when only dirty live checkout changed:\nfirst={report}\nsecond={second_report}"
        ));
    }
    Ok(())
}

#[test]
fn committed_tree_reports_path_dependency_unsupported() -> TestResult {
    let workspace = seed_git_workspace("rch-committed-tree-path-dep")?;
    fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"rch_path_dep_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nfixture_dep = { path = \"../fixture_dep\" }\n",
    )
    .map_err(|error| format!("write path-dep Cargo.toml: {error}"))?;
    git(&workspace, &["add", "Cargo.toml"])?;
    git(&workspace, &["commit", "-m", "add path dependency"])?;

    let (status, stdout, _stderr) = run_script_with_env_in_dir(
        &[
            "--committed-tree",
            "--treeish",
            "HEAD",
            "--dry-run",
            "--",
            "cargo",
            "test",
            "--lib",
            "committed_tree_path_dep",
        ],
        &[],
        &workspace,
    )?;
    if status.success() {
        return Err("committed-tree path dependency mode should be unsupported".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse path-dependency committed-tree report: {error}"))?;
    for expected in [
        "rch_verify_committed_tree_unsupported",
        "rch_verify_committed_tree_path_deps_unsupported",
    ] {
        if !source_degraded_contains(&report, expected)? || !degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in committed-tree report: {report}"
            ));
        }
    }
    if degraded_contains(&report, "rch_verify_dry_run")? {
        return Err(format!(
            "committed-tree source refusal should happen before dry-run proof: {report}"
        ));
    }
    Ok(())
}

#[test]
fn committed_tree_unresolved_ref_refuses_before_rch() -> TestResult {
    let workspace = seed_git_workspace("rch-committed-tree-missing-ref")?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let invocation_log = unique_tmp_path("rch-committed-tree-missing-ref-invocations");
    let fake_rch = write_fake_rch(
        "fake-rch-committed-tree-missing-ref-should-not-run.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--committed-tree",
            "--treeish",
            "refs/heads/does-not-exist",
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "committed_tree_missing_ref_smoke",
        ],
        &[("FAKE_RCH_INVOCATIONS", invocation_log_arg)],
        &workspace,
    )?;
    assert_git_status_unchanged(&workspace, &before_status, "committed-tree unresolved ref")?;
    if status.success() {
        return Err(format!(
            "unresolved committed-tree ref should refuse before RCH\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    if invocation_log.exists() {
        let invocations = fs::read_to_string(&invocation_log)
            .map_err(|error| format!("read unresolved-ref invocation log: {error}"))?;
        if !invocations.is_empty() {
            return Err(format!(
                "unresolved committed-tree ref should not invoke fake RCH: {invocations:?}"
            ));
        }
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse unresolved committed-tree report: {error}"))?;
    if report["status"] != "committed_tree_unsupported"
        || report["verification_attribution"] != "committed_tree"
        || report["requested_treeish"] != "refs/heads/does-not-exist"
        || !report["resolved_commit"].is_null()
        || !report["git_tree"].is_null()
        || report["source_manifest_file_count"] != 0
    {
        return Err(format!(
            "unexpected unresolved committed-tree report: {report}"
        ));
    }
    for expected in [
        "rch_verify_committed_tree_ref_unresolved",
        "rch_verify_committed_tree_unsupported",
    ] {
        if !source_degraded_contains(&report, expected)? || !degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in unresolved committed-tree report: {report}"
            ));
        }
    }
    Ok(())
}

#[test]
fn first_remote_invocation_passes_requested_workers() -> TestResult {
    let fake_rch = write_fake_rch(
        "fake-rch-workers.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'RCH_WORKERS=%s\n' "${RCH_WORKERS:-}"
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "graph::algorithms::run_with_budget_emits_algorithm_compute_telemetry",
        ],
        &[
            ("RCH_WORKERS", "trj"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css,trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css,trj,csd"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "fake rch invocation failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse requested-workers proof: {error}"))?;
    if report["worker_id"] != "trj" {
        return Err(format!("fake rch worker was not detected: {report}"));
    }
    if report["requested_workers"] != serde_json::json!(["trj"]) {
        return Err(format!("requested workers missing from proof: {report}"));
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout_tail".to_owned())?;
    if !stdout_tail.contains("RCH_WORKERS=trj") {
        return Err(format!(
            "first invocation did not receive RCH_WORKERS: {report}"
        ));
    }
    if degraded_contains(&report, "rch_verify_worker_filter_ignored")? {
        return Err(format!(
            "requested worker should not trip filter ignored: {report}"
        ));
    }
    Ok(())
}

#[test]
fn dry_run_accepts_cargo_fmt_only_when_checking() -> TestResult {
    let report = run_json(&["--dry-run", "--", "cargo", "fmt", "--check"])?;
    if report["command_kind"] != "cargo_fmt_check" {
        return Err(format!(
            "cargo fmt --check classified incorrectly: {report}"
        ));
    }
    if report["would_offload"] != false {
        return Err("cargo fmt --check should not claim RCH offload".to_owned());
    }

    let (status, stdout, _stderr) = run_script(&["--dry-run", "--", "cargo", "fmt"])?;
    if status.success() {
        return Err("cargo fmt without --check should be refused".to_owned());
    }
    let rejected: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse rejection: {error}"))?;
    if rejected["degraded_codes"][0] != "rch_verify_refused_unknown_command" {
        return Err(format!("unexpected rejection: {rejected}"));
    }
    Ok(())
}

#[test]
fn dry_run_rejects_unknown_and_forbidden_commands_by_default() -> TestResult {
    let (status, stdout, _stderr) = run_script(&["--dry-run", "--", "echo", "hello"])?;
    if status.success() {
        return Err("unknown command should be refused without --allow-raw".to_owned());
    }
    let rejected: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse rejection: {error}"))?;
    if rejected["degraded_codes"][0] != "rch_verify_refused_unknown_command" {
        return Err(format!("unexpected unknown-command rejection: {rejected}"));
    }

    let (status, stdout, _stderr) = run_script(&["--dry-run", "--", "cargo", "test", "rm -rf"])?;
    if status.success() {
        return Err("forbidden command text should be refused".to_owned());
    }
    let forbidden: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse forbidden: {error}"))?;
    if forbidden["degraded_codes"][0] != "rch_verify_refused_forbidden_command" {
        return Err(format!(
            "unexpected forbidden-command rejection: {forbidden}"
        ));
    }
    Ok(())
}

#[test]
fn dry_run_json_is_deterministic_for_same_input() -> TestResult {
    let args = [
        "--dry-run",
        "--",
        "cargo",
        "clippy",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ];
    let first = run_json(&args)?;
    let second = run_json(&args)?;
    if first != second {
        return Err(format!(
            "dry-run proof is not deterministic:\n{first}\n{second}"
        ));
    }
    if first["command_kind"] != "cargo_clippy" {
        return Err("cargo clippy classified incorrectly".to_owned());
    }
    Ok(())
}

#[test]
fn synthetic_remote_transcript_extracts_worker_id() -> TestResult {
    let (status, stdout, stderr) = run_script_with_env(
        &["--", "cargo", "test", "--test", "rch_verify_contract"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=900\nremote test ok\n[RCH] remote trj (12.3s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "123"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "fake transcript run failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse transcript: {error}"))?;
    if report["worker_id"] != "trj" {
        return Err(format!("worker id was not extracted: {report}"));
    }
    if report["elapsed_ms"] != 123 {
        return Err("fake elapsed_ms was not preserved".to_owned());
    }
    if report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?
        .iter()
        .any(|code| code == "rch_verify_remote_marker_missing")
    {
        return Err("remote marker was present but missing-marker degradation emitted".to_owned());
    }
    if report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?
        .iter()
        .any(|code| code == "rch_verify_capacity_or_timeout")
    {
        return Err(
            "successful timeout-text transcript should not be capacity degraded".to_owned(),
        );
    }
    Ok(())
}

#[test]
fn build_admission_denial_refuses_before_rch() -> TestResult {
    let fake_ee = write_fake_build_admission_ee("fake-ee-admission-denied.sh", false)?;
    let fake_rch = write_fake_rch(
        "fake-rch-admission-should-not-run.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote css (1.0s)\n'
"#,
    )?;
    let invocation_log = unique_tmp_path("rch-admission-denied-invocations");
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;

    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--rch-bin",
            fake_rch_arg,
            "--build-admission-ee-bin",
            fake_ee_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "admission_denied_smoke",
        ],
        &[("FAKE_RCH_INVOCATIONS", invocation_log_arg)],
    )?;
    if status.success() {
        return Err("build-admission denial should refuse before RCH".to_owned());
    }
    if invocation_log.exists() {
        let invocations = fs::read_to_string(&invocation_log)
            .map_err(|error| format!("read invocation log: {error}"))?;
        if !invocations.is_empty() {
            return Err(format!(
                "build-admission denial invoked fake RCH: {invocations:?}"
            ));
        }
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse admission denial: {error}"))?;
    if report["status"] != "build_admission_refused"
        || report["exit_code"] != 1
        || report["build_admission"]["status"] != "denied"
        || report["build_admission"]["admitted"] != false
    {
        return Err(format!("unexpected build-admission refusal: {report}"));
    }
    if !degraded_contains(&report, "rch_verify_build_admission_denied")? {
        return Err(format!("missing build-admission degraded code: {report}"));
    }
    if !report["worker_id"].is_null() {
        return Err(format!("denial should not have a worker id: {report}"));
    }
    Ok(())
}

#[test]
fn build_admission_pass_is_recorded_and_allows_rch() -> TestResult {
    let fake_ee = write_fake_build_admission_ee("fake-ee-admission-pass.sh", true)?;
    let fake_rch = write_fake_rch(
        "fake-rch-admission-pass.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote css (1.0s)\n'
"#,
    )?;
    let invocation_log = unique_tmp_path("rch-admission-pass-invocations");
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--rch-bin",
            fake_rch_arg,
            "--build-admission-ee-bin",
            fake_ee_arg,
            "--summary",
            "--",
            "cargo",
            "test",
            "--lib",
            "admission_pass_smoke",
        ],
        &[
            ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "build-admission pass should allow fake RCH\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read invocation log: {error}"))?;
    if !invocations.contains("exec -- env TMPDIR=/tmp CARGO_TARGET_DIR=/tmp/ee-rch-verify-target cargo test --lib admission_pass_smoke") {
        return Err(format!("fake RCH did not receive expected invocation: {invocations}"));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse admission pass: {error}"))?;
    if report["status"] != "remote_pass"
        || report["worker_id"] != "css"
        || report["build_admission"]["status"] != "passed"
        || report["build_admission"]["admitted"] != true
    {
        return Err(format!("unexpected build-admission pass report: {report}"));
    }
    if degraded_contains(&report, "rch_verify_build_admission_denied")? {
        return Err(format!("pass reported admission denial: {report}"));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    if !summary.contains("build_admission: `passed` admitted=`true`") {
        return Err(format!("summary missing build-admission line: {summary}"));
    }
    Ok(())
}

#[test]
fn build_admission_auto_candidate_skips_empty_version_binary() -> TestResult {
    let target_dir = unique_tmp_path("rch-admission-candidates");
    let debug_dir = target_dir.join("debug");
    let release_dir = target_dir.join("release");
    fs::create_dir_all(&debug_dir).map_err(|error| format!("create debug dir: {error}"))?;
    fs::create_dir_all(&release_dir).map_err(|error| format!("create release dir: {error}"))?;
    let empty_version_candidate = debug_dir.join("ee");
    let valid_candidate = release_dir.join("ee");
    write_fake_build_admission_candidate(&empty_version_candidate, "", false)?;
    write_fake_build_admission_candidate(&valid_candidate, "ee 0.0.0-test", true)?;

    let fake_rch = write_fake_rch(
        "fake-rch-admission-auto-candidate.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote css (1.0s)\n'
"#,
    )?;
    let invocation_log = unique_tmp_path("rch-admission-auto-candidate-invocations");
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let target_dir_arg = target_dir
        .to_str()
        .ok_or_else(|| "target dir path is not utf-8".to_owned())?;
    let valid_candidate_arg = valid_candidate
        .to_str()
        .ok_or_else(|| "valid candidate path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--rch-bin",
            fake_rch_arg,
            "--summary",
            "--",
            "cargo",
            "test",
            "--lib",
            "admission_auto_candidate_smoke",
        ],
        &[
            ("CARGO_TARGET_DIR", target_dir_arg),
            ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "auto candidate admission should allow fake RCH\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse admission auto-candidate report: {error}"))?;
    if report["status"] != "remote_pass"
        || report["build_admission"]["status"] != "passed"
        || report["build_admission"]["ee_bin"] != valid_candidate_arg
    {
        return Err(format!(
            "auto candidate should skip empty --version binary and use release candidate: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_local_fallback_refusal_is_not_worker_id() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--test", "rch_verify_contract"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] local (dependency preflight RCH-E327: Path dependency topology policy failed.)\n[RCH] remote required; refusing local fallback (dependency preflight failed)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "42"),
        ],
    )?;
    if status.success() {
        return Err("local fallback refusal should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse fallback: {error}"))?;
    if !report["worker_id"].is_null() {
        return Err(format!(
            "fallback marker was misread as worker id: {report}"
        ));
    }
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "fallback should be an environment failure: {report}"
        ));
    }
    let degraded = report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?;
    for expected in [
        "rch_verify_topology_blocked",
        "rch_verify_local_fallback_refused",
        "rch_verify_remote_marker_missing",
    ] {
        if !degraded.iter().any(|code| code == expected) {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    Ok(())
}

#[test]
fn synthetic_remote_test_failure_with_timeout_env_is_remote_failure() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--",
            "cargo",
            "test",
            "--lib",
            "why_toon_matches_json_contract",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=900\nrunning 1 test\ntest cli::tests::why_toon_matches_json_contract ... FAILED\nError: \"expected Number(12), got Number(12.0)\"\n[RCH] remote trj failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "195544"),
        ],
    )?;
    if status.success() {
        return Err("remote Rust test failure should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse remote failure: {error}"))?;
    if report["worker_id"] != "trj" {
        return Err(format!("remote failure should retain worker id: {report}"));
    }
    if report["status"] != "remote_failure" {
        return Err(format!(
            "remote Rust test failure should not be capacity: {report}"
        ));
    }
    let degraded = report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?;
    if !degraded
        .iter()
        .any(|code| code == "rch_verify_remote_command_failed")
    {
        return Err(format!("missing remote failure degraded code: {report}"));
    }
    if degraded
        .iter()
        .any(|code| code == "rch_verify_capacity_or_timeout")
    {
        return Err(format!("remote test failure was misclassified: {report}"));
    }
    Ok(())
}

#[test]
fn synthetic_pre_cargo_disk_full_extracts_selected_worker() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--lib", "task_frame"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "2026-05-16T12:58:58Z INFO Selected worker: csd at ubuntu@csd (8 slots, speed 50.0)\nrsync: [receiver] mkstemp \"/data/projects/eidetic_engine_cli/.rchignore.XXXXXX\" failed: No space left on device (28)\n[RCH] remote required; refusing local fallback (remote pipeline failed)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "1998"),
            ("RCH_VERIFY_DISABLE_DISK_FULL_RETRY", "1"),
        ],
    )?;
    if status.success() {
        return Err("disk-full transcript should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse disk-full: {error}"))?;
    if report["worker_id"] != "csd" {
        return Err(format!("selected worker was not extracted: {report}"));
    }
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "disk-full local-fallback refusal should be environment failure: {report}"
        ));
    }
    for expected in [
        "rch_verify_remote_command_failed",
        "rch_verify_worker_disk_full",
        "rch_verify_local_fallback_refused",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    if degraded_contains(&report, "rch_verify_remote_marker_missing")? {
        return Err(format!(
            "selected-worker transcript should not be remote-marker missing: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_disk_full_retry_stops_when_quarantine_is_ignored() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--lib", "qos"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "INFO Selected worker: csd at ubuntu@csd (8 slots, speed 50.0)\nrsync: write failed on \"/data/projects/eidetic_engine_cli/.rchignore\": No space left on device (28)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "20"),
            ("RCH_VERIFY_HEALTHY_WORKERS", "css,trj"),
            (
                "RCH_VERIFY_FAKE_RETRY_OUTPUT",
                "INFO Selected worker: csd at ubuntu@csd (8 slots, speed 50.0)\nrsync: write failed on \"/data/projects/eidetic_engine_cli/.rchignore\": No space left on device (28)\n",
            ),
            ("RCH_VERIFY_FAKE_RETRY_EXIT_CODE", "1"),
        ],
    )?;
    if status.success() {
        return Err("ignored quarantine retry should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse retry: {error}"))?;
    if report["worker_id"] != "csd" {
        return Err(format!(
            "retry worker should record ignored quarantine: {report}"
        ));
    }
    for expected in [
        "rch_verify_worker_disk_full",
        "rch_verify_retry_after_worker_disk_full",
        "rch_verify_worker_quarantine_ignored",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout_tail".to_owned())?;
    if !stdout_tail.contains("retrying once with RCH_WORKERS=css,trj") {
        return Err(format!("retry note missing from stdout tail: {report}"));
    }
    Ok(())
}

#[test]
fn synthetic_disk_full_retry_respects_requested_workers() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--lib", "qos"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "INFO Selected worker: css at ubuntu@css (8 slots, speed 50.0)\nrsync: write failed on \"/data/projects/eidetic_engine_cli/.beads/issues.jsonl\": No space left on device (28)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "20"),
            ("RCH_VERIFY_HEALTHY_WORKERS", "css,trj,csd"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css,trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css,trj,csd"),
            ("RCH_WORKERS", "trj"),
            (
                "RCH_VERIFY_FAKE_RETRY_OUTPUT",
                "INFO Selected worker: trj at ubuntu@trj (4 slots, speed 50.0)\nremote test ok\n[RCH] remote trj (1.0s)\n",
            ),
            ("RCH_VERIFY_FAKE_RETRY_EXIT_CODE", "0"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "requested-worker retry should succeed through trj\nstdout:\n{stdout}\n"
        ));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse requested retry: {error}"))?;
    if report["status"] != "remote_pass" || report["worker_id"] != "trj" {
        return Err(format!("unexpected requested retry report: {report}"));
    }
    if report["requested_workers"] != serde_json::json!(["trj"]) {
        return Err(format!("requested worker list was not preserved: {report}"));
    }
    for expected in [
        "rch_verify_worker_disk_full",
        "rch_verify_retry_after_worker_disk_full",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    if degraded_contains(&report, "rch_verify_worker_filter_ignored")? {
        return Err(format!(
            "successful requested-worker retry should not report filter ignored: {report}"
        ));
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout_tail".to_owned())?;
    if !stdout_tail.contains("retrying once with RCH_WORKERS=trj")
        || stdout_tail.contains("RCH_WORKERS=trj,csd")
    {
        return Err(format!(
            "retry note did not stay constrained to requested worker: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_worker_filter_ignored_reports_requested_and_configured_workers() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--bead-id",
            "bd-filter",
            "--summary",
            "--no-write",
            "--",
            "cargo",
            "test",
            "--lib",
            "serve_foreground",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "INFO Selected worker: csd at ubuntu@csd (8 slots, speed 50.0)\nrsync: write failed on \"/data/projects/eidetic_engine_cli/.rchignore\": No space left on device (28)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "44"),
            ("RCH_VERIFY_DISABLE_DISK_FULL_RETRY", "1"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css,trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css,trj,csd"),
            ("RCH_WORKERS", "css,trj"),
        ],
    )?;
    if status.success() {
        return Err("filtered-out worker failure should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse filter report: {error}"))?;
    if report["status"] != "rch_environment_failure" || report["worker_id"] != "csd" {
        return Err(format!("unexpected worker-filter status: {report}"));
    }
    for expected in [
        "rch_verify_worker_disk_full",
        "rch_verify_worker_filter_ignored",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
        if !worker_degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in worker-state codes: {report}"
            ));
        }
    }
    if report["source_state_degraded_codes"] != serde_json::json!([]) {
        return Err(format!(
            "worker failure should keep source-state codes empty: {report}"
        ));
    }
    if worker_degraded_contains(&report, "rch_verify_remote_command_failed")? {
        return Err(format!(
            "generic remote failure should not be listed as worker-state: {report}"
        ));
    }
    if report["requested_workers"] != serde_json::json!(["css", "trj"])
        || report["configured_workers"] != serde_json::json!(["css", "trj"])
        || report["daemon_workers"] != serde_json::json!(["css", "trj", "csd"])
    {
        return Err(format!(
            "worker inventory arrays were not emitted: {report}"
        ));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    for expected in [
        "requested_workers: `css, trj`",
        "configured_workers: `css, trj`",
        "daemon_workers: `css, trj, csd`",
        "worker_state_degraded_codes: `rch_verify_worker_disk_full`, `rch_verify_worker_filter_ignored`",
    ] {
        if !summary.contains(expected) {
            return Err(format!("summary missing {expected}: {summary}"));
        }
    }
    Ok(())
}

#[test]
fn synthetic_stale_daemon_disk_full_preflight_does_not_run_cargo() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--",
            "cargo",
            "test",
            "--lib",
            "log_event_to_rejects_symlinked",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "INFO Selected worker: css at ubuntu@css (8 slots, speed 50.0)\n[RCH] remote css (1.0s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css,trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css,trj,csd"),
            ("RCH_VERIFY_DISK_FULL_WORKERS", "csd"),
        ],
    )?;
    if status.success() {
        return Err("stale daemon preflight should fail before Cargo".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse preflight: {error}"))?;
    if report["status"] != "rch_environment_failure" || report["worker_id"] != "csd" {
        return Err(format!("unexpected preflight report: {report}"));
    }
    for expected in [
        "rch_verify_remote_command_failed",
        "rch_verify_worker_disk_full",
        "rch_verify_worker_filter_ignored",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout tail".to_owned())?;
    if !stdout_tail.contains("stale daemon worker(s)") || stdout_tail.contains("[RCH] remote css") {
        return Err(format!(
            "preflight did not short-circuit fake Cargo run: {report}"
        ));
    }
    if report["elapsed_ms"] != 0 {
        return Err(format!(
            "preflight should not measure remote execution: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_recent_failed_excluded_daemon_preflight_does_not_need_override() -> TestResult {
    let status_json = r#"{
        "data": {
            "daemon": {
                "recent_builds": [
                    {"worker_id": "csd", "exit_code": 1, "duration_ms": 2342},
                    {"worker_id": "css", "exit_code": 101, "duration_ms": 188436}
                ]
            }
        }
    }"#;
    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--",
            "cargo",
            "bench",
            "--bench",
            "context_with_ppr",
            "--",
            "--sample-size",
            "10",
            "--measurement-time",
            "5",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "INFO Selected worker: css at ubuntu@css (8 slots, speed 50.0)\n[RCH] remote css (1.0s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css,trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css,trj,csd"),
            ("RCH_VERIFY_STATUS_JSON", status_json),
        ],
    )?;
    if status.success() {
        return Err("recent failed excluded worker preflight should fail before Cargo".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse recent failure preflight: {error}"))?;
    if report["status"] != "rch_environment_failure" || report["worker_id"] != "csd" {
        return Err(format!("unexpected recent failure preflight: {report}"));
    }
    for expected in [
        "rch_verify_remote_command_failed",
        "rch_verify_worker_filter_ignored",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    if degraded_contains(&report, "rch_verify_worker_disk_full")? {
        return Err(format!(
            "recent fast failure without disk-full transcript should not claim disk-full: {report}"
        ));
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout tail".to_owned())?;
    if !stdout_tail.contains("recently failed fast") || stdout_tail.contains("[RCH] remote css") {
        return Err(format!(
            "recent failure preflight did not short-circuit fake Cargo run: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_recent_failed_requested_worker_preflight_honors_rch_workers() -> TestResult {
    let status_json = r#"{
        "data": {
            "daemon": {
                "recent_builds": [
                    {"worker_id": "css", "exit_code": 101, "duration_ms": 52903},
                    {"worker_id": "trj", "exit_code": 0, "duration_ms": 2000}
                ]
            }
        }
    }"#;
    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--bead-id",
            "bd-requested-worker",
            "--summary",
            "--no-write",
            "--",
            "cargo",
            "test",
            "--test",
            "g5_curate_decay_e2e",
            "--",
            "--nocapture",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "INFO Selected worker: trj at ubuntu@trj (4 slots, speed 50.0)\n[RCH] remote trj (1.0s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css,trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css,trj,csd"),
            ("RCH_VERIFY_STATUS_JSON", status_json),
            ("RCH_WORKERS", "trj"),
        ],
    )?;
    if status.success() {
        return Err("recent failed worker outside RCH_WORKERS should fail before Cargo".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse requested-worker preflight: {error}"))?;
    if report["status"] != "rch_environment_failure" || report["worker_id"] != "css" {
        return Err(format!("unexpected requested-worker preflight: {report}"));
    }
    if report["requested_workers"] != serde_json::json!(["trj"])
        || report["configured_workers"] != serde_json::json!(["css", "trj"])
        || report["daemon_workers"] != serde_json::json!(["css", "trj", "csd"])
    {
        return Err(format!(
            "worker inventory arrays were not preserved: {report}"
        ));
    }
    for expected in [
        "rch_verify_remote_command_failed",
        "rch_verify_worker_filter_ignored",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    if degraded_contains(&report, "rch_verify_worker_disk_full")? {
        return Err(format!(
            "recent requested-worker failure without disk-full transcript should not claim disk-full: {report}"
        ));
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout tail".to_owned())?;
    if !stdout_tail.contains("excluded from requested workers")
        || !stdout_tail.contains("recently failed fast")
        || stdout_tail.contains("[RCH] remote trj")
    {
        return Err(format!(
            "requested-worker preflight did not short-circuit fake Cargo run: {report}"
        ));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    if !summary.contains("requested_workers: `trj`")
        || !summary.contains("configured_workers: `css, trj`")
        || !summary.contains("daemon_workers: `css, trj, csd`")
    {
        return Err(format!("summary missing worker arrays: {summary}"));
    }
    if report["elapsed_ms"] != 0 {
        return Err(format!(
            "preflight should not measure remote execution: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_compile_error_is_not_worker_disk_full() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--lib", "support_bundle"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "error[E0277]: the trait bound `&str: Borrow<String>` is not satisfied\n  --> src/core/support_bundle.rs:1339:44\n[RCH] remote css failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "3000"),
        ],
    )?;
    if status.success() {
        return Err("compile-error transcript should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse compile: {error}"))?;
    if report["status"] != "remote_failure" {
        return Err(format!(
            "compile error should remain remote failure: {report}"
        ));
    }
    if degraded_contains(&report, "rch_verify_worker_disk_full")? {
        return Err(format!(
            "compile error was misclassified as disk full: {report}"
        ));
    }
    if report["first_error_file"] != "src/core/support_bundle.rs"
        || report["first_error_line"] != 1339
    {
        return Err(format!("compile error location not extracted: {report}"));
    }
    Ok(())
}

#[test]
fn synthetic_cargo_workspace_inheritance_failure_is_worker_topology() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--",
            "cargo",
            "test",
            "--test",
            "rch_verify_contract",
            "strict_clean_tree",
            "--",
            "--nocapture",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "error: failed to load manifest for dependency `frankensearch`\n\nCaused by:\n  failed to parse manifest at `/data/projects/frankensearch/frankensearch/Cargo.toml`\n\nCaused by:\n  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`\n\nCaused by:\n  `workspace.package.license-file` was not defined\n[RCH] remote vmi1227854 failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "2400"),
        ],
    )?;
    if status.success() {
        return Err("workspace inheritance transcript should preserve non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse workspace inheritance report: {error}"))?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "workspace inheritance should be routed as RCH environment failure: {report}"
        ));
    }
    if report["worker_id"] != "vmi1227854" {
        return Err(format!("worker id should be preserved: {report}"));
    }
    for expected in [
        "rch_verify_remote_command_failed",
        "rch_verify_cargo_workspace_inheritance_blocked",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    if !worker_degraded_contains(&report, "rch_verify_cargo_workspace_inheritance_blocked")? {
        return Err(format!(
            "workspace inheritance code should be worker-state degraded: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_e0583_for_tracked_module_is_remote_checkout_incomplete() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--test", "context_stream"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "error[E0583]: file not found for module `cache`\n  --> src/lib.rs:4:1\n   |\n4  | pub mod cache;\n   | ^^^^^^^^^^^^^^\n   |\n   = help: to create the module `cache`, create file \"src/cache.rs\" or \"src/cache/mod.rs\"\n[RCH] remote css failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "4000"),
            (
                "RCH_VERIFY_GIT_LS_FILES",
                "src/main.rs\nsrc/cache/mod.rs\nsrc/lib.rs\nsrc/cache/pack_l2.rs\n",
            ),
        ],
    )?;
    if status.success() {
        return Err(
            "remote-checkout-incomplete transcript should preserve non-zero exit".to_owned(),
        );
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse e0583: {error}"))?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "tracked missing module should be environment failure: {report}"
        ));
    }
    if report["worker_id"] != "css" {
        return Err(format!("worker id should be preserved: {report}"));
    }
    if !degraded_contains(&report, "rch_verify_remote_checkout_incomplete")? {
        return Err(format!("missing remote checkout degradation: {report}"));
    }
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout_tail".to_owned())?;
    if !stdout_tail.contains("remote checkout missing tracked files: src/cache/mod.rs") {
        return Err(format!("missing tracked path note: {report}"));
    }
    Ok(())
}

#[test]
fn synthetic_e0583_for_untracked_module_remains_remote_failure() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "test", "--test", "context_stream"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "error[E0583]: file not found for module `phantom`\n  --> src/lib.rs:99:1\n   |\n99 | pub mod phantom;\n   | ^^^^^^^^^^^^^^^^\n   |\n   = help: to create the module `phantom`, create file \"src/phantom.rs\" or \"src/phantom/mod.rs\"\n[RCH] remote css failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "4000"),
            (
                "RCH_VERIFY_GIT_LS_FILES",
                "src/main.rs\nsrc/cache/mod.rs\nsrc/lib.rs\n",
            ),
        ],
    )?;
    if status.success() {
        return Err("real missing local module should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse local e0583: {error}"))?;
    if report["status"] != "remote_failure" {
        return Err(format!(
            "untracked missing module should stay code failure: {report}"
        ));
    }
    if degraded_contains(&report, "rch_verify_remote_checkout_incomplete")? {
        return Err(format!(
            "untracked missing module was misclassified: {report}"
        ));
    }
    Ok(())
}

#[test]
fn critical_checkout_manifest_from_synthetic_git_ls_files_is_deterministic() -> TestResult {
    let (status, stdout, stderr) = run_script_with_env(
        &["--dry-run", "--", "cargo", "test"],
        &[
            ("RCH_VERIFY_PRINT_CRITICAL_MANIFEST", "1"),
            (
                "RCH_VERIFY_GIT_LS_FILES",
                "README.md\nsrc/search/index.rs\nsrc/main.rs\nsrc/cache/pack_l2.rs\nsrc/lib.rs\nsrc/cache/mod.rs\nsrc/cli/mod.rs\nsrc/db.rs\ndocs/design.md\n",
            ),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "manifest test hook failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let lines = stdout.lines().collect::<Vec<_>>();
    let expected = vec![
        "src/cache/mod.rs",
        "src/cli/mod.rs",
        "src/db.rs",
        "src/lib.rs",
        "src/main.rs",
    ];
    if lines != expected {
        return Err(format!("unexpected critical manifest: {lines:?}"));
    }
    Ok(())
}

#[test]
fn synthetic_remote_transcript_writes_ledger_and_summary() -> TestResult {
    let dir = target_tmp_dir().join(format!("rch-verify-ledger-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let ledger = dir.join("runs.jsonl");
    let ledger_arg = ledger.display().to_string();
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--bead-id",
            "bd-test",
            "--ledger",
            &ledger_arg,
            "--summary",
            "--",
            "cargo",
            "test",
            "--test",
            "rch_verify_contract",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "error[E0425]: cannot find value `stderr` in this scope\n  --> tests/rch_verify_contract.rs:42:9\nremote test ok\n[RCH] remote css (1.0s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "1000"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "ledger run failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse report: {error}"))?;
    if report["status"] != "remote_pass" || report["bead_id"] != "bd-test" {
        return Err(format!("unexpected report status/bead: {report}"));
    }
    if report["command_hash"].as_str().map(str::len) != Some(64) {
        return Err(format!("missing sha256 command hash: {report}"));
    }
    if report["first_error_file"] != "tests/rch_verify_contract.rs"
        || report["first_error_line"] != 42
    {
        return Err(format!("first error location was not extracted: {report}"));
    }
    let error_codes = report["error_codes"]
        .as_array()
        .ok_or_else(|| "missing error codes".to_owned())?;
    if !error_codes.iter().any(|code| code == "E0425") {
        return Err(format!("missing rust error code: {report}"));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    if !summary.contains("worker_id: `css`")
        || !summary.contains("bead_id: `bd-test`")
        || !summary.contains("first_error: `tests/rch_verify_contract.rs:42`")
    {
        return Err(format!("summary missing expected fields: {summary}"));
    }

    let ledger_text =
        fs::read_to_string(&ledger).map_err(|error| format!("read ledger: {error}"))?;
    let rows = ledger_text.lines().collect::<Vec<_>>();
    if rows.len() != 1 {
        return Err(format!("expected one ledger row, got {}", rows.len()));
    }
    let row: Value =
        serde_json::from_str(rows[0]).map_err(|error| format!("parse ledger row: {error}"))?;
    if row["schema"] != "ee.rch.verify.ledger.v1"
        || row["status"] != "remote_pass"
        || row["worker_id"] != "css"
        || row["first_error_file"] != "tests/rch_verify_contract.rs"
        || row["first_error_line"] != 42
    {
        return Err(format!("unexpected ledger row: {row}"));
    }
    if row["command_hash"].as_str().map(str::len) != Some(64) {
        return Err(format!("ledger row missing command hash: {row}"));
    }
    Ok(())
}

#[test]
fn ledger_no_write_renders_summary_without_appending() -> TestResult {
    let dir = target_tmp_dir().join(format!("rch-verify-no-write-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let ledger = dir.join("runs.jsonl");
    let ledger_arg = ledger.display().to_string();
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--bead-id",
            "bd-test",
            "--ledger",
            &ledger_arg,
            "--summary",
            "--no-write",
            "--",
            "cargo",
            "test",
            "--test",
            "rch_verify_contract",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] local (dependency preflight RCH-E327: Path dependency topology policy failed.)\n[RCH] remote required; refusing local fallback (dependency preflight failed)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "20"),
        ],
    )?;
    if status.success() {
        return Err("no-write local fallback should preserve non-zero exit".to_owned());
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse report: {error}"))?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!("unexpected no-write status: {report}"));
    }
    let degraded = report["degraded_codes"]
        .as_array()
        .ok_or_else(|| "missing degraded codes".to_owned())?;
    if !degraded
        .iter()
        .any(|code| code == "rch_verify_ledger_write_suppressed")
    {
        return Err(format!("missing no-write degradation: {report}"));
    }
    if ledger.exists() {
        return Err(format!(
            "no-write should not create ledger file; stderr was {stderr}"
        ));
    }
    Ok(())
}
