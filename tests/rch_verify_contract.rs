use ee::shadow::{
    ResourceAdmissionDecision, ResourceCostClass, ResourceQueuePressureBackoffInput,
    ResourceQueuePressureInventory, ResourceQueuePressureLevel, ResourceQueuePressureReasonCode,
    ResourceQueuePressureSourceKind, ResourceQueuePressureSourceRef,
    ResourceQueuePressureSourceState, evaluate_resource_queue_pressure_backoff,
};
use serde_json::Value;
use std::collections::BTreeSet;
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

fn read_repo_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let content =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&content).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn string_set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn string_set_at(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let array = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing string array at {pointer}: {value}"))?;
    let mut set = BTreeSet::new();
    for item in array {
        let text = item
            .as_str()
            .ok_or_else(|| format!("non-string enum item at {pointer}: {item}"))?;
        set.insert(text.to_owned());
    }
    Ok(set)
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

fn selector_probe(report: &Value) -> Result<&Value, String> {
    let probe = report
        .get("selector_admission_probe")
        .ok_or_else(|| format!("missing selector admission probe: {report}"))?;
    if probe["schema"] != "ee.rch.selector_admission_probe.v1" {
        return Err(format!("unexpected selector admission schema: {probe}"));
    }
    Ok(probe)
}

fn workspace_inheritance_transcript() -> &'static str {
    r#"error: failed to load manifest for dependency `frankensearch`

Caused by:
  failed to parse manifest at `/data/projects/frankensearch/frankensearch/Cargo.toml`

Caused by:
  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`

Caused by:
  `workspace.package.license-file` was not defined
[RCH] remote vmi1227854 failed (exit 101)
"#
}

fn unique_path_under(base: &Path, label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    base.join(format!("{label}-{}-{nanos}", std::process::id()))
}

fn unique_tmp_path(label: &str) -> PathBuf {
    unique_path_under(&target_tmp_dir(), label)
}

fn unique_system_tmp_path(label: &str) -> PathBuf {
    unique_path_under(Path::new("/tmp"), label)
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

fn seed_git_workspace_at(workspace: PathBuf) -> Result<PathBuf, String> {
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

fn seed_git_workspace(label: &str) -> Result<PathBuf, String> {
    seed_git_workspace_at(unique_tmp_path(label))
}

fn seed_system_tmp_git_workspace(label: &str) -> Result<PathBuf, String> {
    seed_git_workspace_at(unique_system_tmp_path(label))
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

fn read_invocation_lines(path: &Path) -> Result<Vec<String>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let invocations =
        fs::read_to_string(path).map_err(|error| format!("read invocation log: {error}"))?;
    Ok(invocations.lines().map(str::to_owned).collect())
}

fn remote_exec_invocation_lines(path: &Path) -> Result<Vec<String>, String> {
    Ok(read_invocation_lines(path)?
        .into_iter()
        .filter(|line| line.contains("exec --"))
        .collect())
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
{{"schema":"ee.response.v2","success":true,"data":{{"schema":"ee.build_admission.diagnostics.v1","admitted":{status},"minFreeBytes":1073741824,"checks":[{{"label":"workspace","path":"/tmp/ws","bytesAvailable":1024,"minFreeBytes":1073741824,"admitted":{status},"externalRequired":false,"external":false}},{{"label":"cargo_target","path":"/Volumes/USBNVME16TB/temp_agent_space/cargo-target","bytesAvailable":9000000000000,"minFreeBytes":1073741824,"admitted":true,"externalRequired":true,"external":true}}],"degraded":{degraded}}}}}
JSON
"#,
        ),
    )
}

fn write_fake_proof_broker_ee(name: &str) -> Result<PathBuf, String> {
    write_fake_rch(
        name,
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_EE_INVOCATIONS:?}"
python3 - <<'PY'
import json
import os

verdict = os.environ.get("FAKE_PROOF_VERDICT") or "dispatch_allowed"
next_action_by_verdict = {
    "dispatch_allowed": "launch_single_rch_proof",
    "reuse_existing": "cite_existing_proof",
    "wait_for_inflight": "wait_for_inflight_owner",
    "source_state_mismatch": "rerun_current_source",
    "environment_blocked": "repair_remote_runtime_before_dispatch",
    "proof_unusable": "discard_local_cargo_evidence_and_rerun_remote",
    "unknown_insufficient_evidence": "collect_source_and_environment_evidence",
}
reason_by_verdict = {
    "dispatch_allowed": ["no_equivalent_record", "read_only_admission"],
    "reuse_existing": ["fingerprint_match", "completed_remote_proof"],
    "wait_for_inflight": ["fingerprint_match", "in_flight_owner"],
    "source_state_mismatch": ["command_match", "fingerprint_mismatch"],
    "environment_blocked": ["environment_or_worker_blocked"],
    "proof_unusable": ["local_cargo_tripwire_blocked", "remote_required"],
    "unknown_insufficient_evidence": ["source_fingerprint_missing"],
}
wait_owner = None
if verdict == "wait_for_inflight":
    wait_owner = {
        "agentName": "RubyElk",
        "beadId": "bd-1n3x1.3",
        "mailThreadId": "bd-1n3x1.3",
        "buildSlot": "rch",
        "rchJobId": "rch-job-fake",
    }
payload = {
    "schema": "ee.response.v2",
    "success": True,
    "data": {
        "command": "proof admit",
        "schema": "ee.proof_broker.v1",
        "fingerprint": {
            "fingerprintId": "pfp_fake",
            "commandClass": "cargo_test",
            "commandHash": "blake3:fake-command",
            "normalizedArgvHash": "sha256:fake-argv",
            "sourceTreeFingerprint": "sha256:fake-source",
            "sourceMaterialization": "remote_checkout_unverified",
            "dirtyStatusHash": "sha256:fake-dirty",
            "envFingerprintClass": "class:rch_verify_wrapper",
            "targetProfile": "debug",
            "executionSubstrate": "rch",
            "rchRuntimeClass": "class:rch_runtime_skipped_fake_transcript",
            "workerRequirement": "class:any_worker",
            "localCargoTripwireClass": "class:tripwire_clean",
            "buildAdmissionPosture": "class:admission_skipped",
        },
        "admission": {
            "verdict": verdict,
            "reasonCodes": reason_by_verdict.get(verdict, ["fake_unknown"]),
            "nextAction": next_action_by_verdict.get(verdict, "collect_source_and_environment_evidence"),
            "reuseRunId": "vrun_existing" if verdict == "reuse_existing" else None,
            "waitOwner": wait_owner,
        },
        "ledger": {
            "source": "ledger_json",
            "recordCount": 1,
            "matchedRowId": "prow_fake",
            "matchedState": "completed" if verdict == "reuse_existing" else None,
        },
        "matchedRecord": None,
        "freshness": None,
        "nextCommand": next_action_by_verdict.get(verdict, "collect_source_and_environment_evidence"),
        "readOnly": True,
    },
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
"#,
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
{{"schema":"ee.response.v2","success":true,"data":{{"schema":"ee.build_admission.diagnostics.v1","admitted":{status},"minFreeBytes":1073741824,"checks":[{{"label":"workspace","path":"/tmp/ws","bytesAvailable":9000000000,"minFreeBytes":1073741824,"admitted":{status},"externalRequired":false,"external":false}}],"degraded":{degraded}}}}}
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
fn dry_run_accepts_focused_cargo_test_and_builds_cargo_argv() -> TestResult {
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
    if !invocation_text.contains("rch exec -- cargo test --lib output::streaming") {
        return Err(format!("unexpected invocation: {invocation_text}"));
    }
    if invocation_text.contains("rch exec -- env ") {
        return Err(format!(
            "RCH selector requires cargo to remain the command argv: {invocation_text}"
        ));
    }
    if invocation_text.contains("/Volumes/USBNVME16TB") {
        return Err("dry-run remote invocation leaked Mac-only USB path".to_owned());
    }
    Ok(())
}

#[test]
fn dry_run_reports_worker_inventory_without_selector_failure() -> TestResult {
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--dry-run",
            "--summary",
            "--no-write",
            "--",
            "cargo",
            "test",
            "--lib",
        ],
        &[
            ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1149989"),
            ("RCH_VERIFY_DAEMON_WORKERS", "vmi1149989"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "dry-run worker inventory proof failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse dry-run inventory report: {error}"))?;
    if report["configured_workers"] != serde_json::json!(["vmi1149989"])
        || report["daemon_workers"] != serde_json::json!(["vmi1149989"])
    {
        return Err(format!("dry-run proof lost worker inventory: {report}"));
    }
    let probe = selector_probe(&report)?;
    if probe["status"] != "not_applicable"
        || probe["required_runtime"] != "Rust"
        || !probe["selected_worker"].is_null()
        || !probe["selection_failure_reason"].is_null()
        || probe["workers_vs_selection_contradiction"] != false
    {
        return Err(format!(
            "dry-run selector probe should not report a real selection failure: {probe}"
        ));
    }
    if !stdout.contains("configured_workers: `vmi1149989`")
        || !stdout.contains("daemon_workers: `vmi1149989`")
    {
        return Err(format!(
            "summary omitted dry-run worker inventory:\n{stdout}"
        ));
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
        || report["verification_attribution"] != "source_state_refused"
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
fn dirty_checkout_remote_run_reports_unmaterialized_source() -> TestResult {
    let workspace = seed_git_workspace("rch-dirty-unmaterialized")?;
    fs::write(workspace.join("tracked.txt"), "dirty local patch\n")
        .map_err(|error| format!("dirty tracked fixture: {error}"))?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let invocation_log = unique_tmp_path("rch-dirty-unmaterialized-invocations");
    let fake_rch = write_fake_rch(
        "fake-rch-dirty-unmaterialized.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf 'remote command passed from configured checkout\n'
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
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "dirty_unmaterialized_smoke",
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
    assert_git_status_unchanged(&workspace, &before_status, "dirty unmaterialized fake RCH")?;
    if !status.success() {
        return Err(format!(
            "dirty non-strict fake RCH should preserve remote status\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let report: Value =
        serde_json::from_str(&stdout).map_err(|error| format!("parse dirty remote: {error}"))?;
    if report["status"] != "remote_pass"
        || report["verification_attribution"] != "local_checkout_observed_remote_source_unknown"
        || report["remote_source_materialized"] != false
        || report["source_materialization"] != "remote_checkout_unverified"
        || !report["source_manifest_hash"].is_null()
    {
        return Err(format!(
            "dirty run should not claim local source was materialized remotely: {report}"
        ));
    }
    if report["dirty_summary"]["tracked"] != 1 || report["dirty_summary"]["total"] != 1 {
        return Err(format!("dirty source counts were not precise: {report}"));
    }
    for expected in [
        "rch_verify_dirty_source_not_materialized",
        "rch_verify_dirty_tracked_paths",
        "rch_verify_dirty_unstaged_paths",
    ] {
        if !source_degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in dirty source state: {report}"
            ));
        }
    }
    let invocations = read_invocation_lines(&invocation_log)?;
    let remote_invocations = remote_exec_invocation_lines(&invocation_log)?;
    if remote_invocations.len() != 1 {
        return Err(format!(
            "dirty non-strict run should invoke fake RCH exec once: {invocations:?}"
        ));
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
fn strict_clean_tree_treats_gitignored_files_as_clean() -> TestResult {
    let workspace = seed_git_workspace("rch-strict-clean-gitignored")?;
    // The seed_git_workspace .gitignore patterns include `._*`, so these
    // local-machine artifacts are gitignored and must not trip strict-clean
    // refusal — gitignore is the explicit-allowlist mechanism for crowded
    // shared checkouts (bd-9ygik acceptance: "clean checkout, and explicit
    // allowlists").
    fs::write(workspace.join("._mac_finder_metadata"), "binary\n")
        .map_err(|error| format!("write gitignored metadata fixture: {error}"))?;
    fs::write(workspace.join("._cache_scratch.txt"), "scratch\n")
        .map_err(|error| format!("write gitignored cache fixture: {error}"))?;

    let before_status = git_status_porcelain_v2(&workspace)?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--require-clean-tree",
            "--dry-run",
            "--",
            "cargo",
            "test",
            "--lib",
            "strict_clean_tree_gitignored_allowlist",
        ],
        &[],
        &workspace,
    )?;
    if !status.success() {
        return Err(format!(
            "strict clean dry-run with gitignored files should pass (gitignore is the allowlist) but exited {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse gitignored allowlist report: {error}"))?;
    if report["verification_attribution"] != "strict_clean_tree" {
        return Err(format!(
            "gitignored files should still allow strict-clean attribution: {report}"
        ));
    }
    if report["dirty_summary"]["total"] != 0
        || report["dirty_summary"]["ignored"] != 0
        || report["dirty_summary"]["untracked"] != 0
        || report["dirty_paths_sample"] != serde_json::json!([])
        || report["source_state_degraded_codes"] != serde_json::json!([])
    {
        return Err(format!(
            "gitignored files must be omitted from dirty summary: {report}"
        ));
    }
    if report["dirty_status_hash"]
        != "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    {
        return Err(format!(
            "gitignored-only checkout must hash identically to a fully clean tree: {report}"
        ));
    }

    assert_git_status_unchanged(
        &workspace,
        &before_status,
        "gitignored allowlist strict-clean dry-run",
    )?;
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
    let lines = remote_exec_invocation_lines(&invocation_log)?;
    if lines.len() != 1 {
        let invocations = read_invocation_lines(&invocation_log)?;
        return Err(format!(
            "strict clean-tree should invoke fake RCH exec once, got {}: {invocations:?}",
            lines.len()
        ));
    }
    if !lines[0].contains("exec -- cargo test --lib strict_clean_tree_fake_rch_smoke") {
        return Err(format!(
            "fake RCH invocation did not preserve cargo argv: {lines:?}"
        ));
    }
    if lines[0].contains("exec -- env ") {
        return Err(format!(
            "fake RCH invocation should not hide cargo behind env: {lines:?}"
        ));
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
        || report["verification_attribution"] != "source_state_refused"
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
        || fields["verification_attribution"] != "source_state_refused"
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
    assert_git_status_unchanged(&workspace, &before_status, "committed-tree preflight")?;
    if !status.success() {
        return Err(format!(
            "committed-tree mode should run from the generated source export\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let invocations = read_invocation_lines(&invocation_log)?;
    let remote_invocations = remote_exec_invocation_lines(&invocation_log)?;
    if remote_invocations.len() != 1 {
        return Err(format!(
            "committed-tree mode should invoke fake RCH exec once: {invocations:?}"
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
fn cargo_config_provenance_refuses_external_patch_before_source_attested_locked_rch() -> TestResult
{
    let workspace = seed_system_tmp_git_workspace("rch-cargo-config-blocked")?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let cargo_home = unique_system_tmp_path("rch-cargo-home-patched");
    fs::create_dir_all(&cargo_home)
        .map_err(|error| format!("create patched Cargo home: {error}"))?;
    fs::write(
        cargo_home.join("config.toml"),
        "[patch.crates-io]\nserde = { path = \"../fixture-serde\" }\n",
    )
    .map_err(|error| format!("write patched Cargo config: {error}"))?;

    let export_base = unique_system_tmp_path("rch-cargo-config-export");
    let invocation_log = unique_system_tmp_path("rch-cargo-config-blocked-invocations");
    let ledger_path = unique_system_tmp_path("rch-cargo-config-blocked-ledger.jsonl");
    let event_log_path = unique_system_tmp_path("rch-cargo-config-blocked-events.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-cargo-config-must-not-run.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;

    let cargo_home_arg = cargo_home
        .to_str()
        .ok_or_else(|| "Cargo home path is not utf-8".to_owned())?;
    let export_base_arg = export_base
        .to_str()
        .ok_or_else(|| "export base path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let ledger_path_arg = ledger_path
        .to_str()
        .ok_or_else(|| "ledger path is not utf-8".to_owned())?;
    let event_log_path_arg = event_log_path
        .to_str()
        .ok_or_else(|| "event log path is not utf-8".to_owned())?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake RCH path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--skip-known-blocker",
            "--summary",
            "--ledger",
            ledger_path_arg,
            "--event-log",
            event_log_path_arg,
            "--committed-tree",
            "--treeish",
            "HEAD",
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--locked",
            "--lib",
            "cargo_config_provenance_blocked_smoke",
        ],
        &[
            ("CARGO_HOME", cargo_home_arg),
            ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
            ("RCH_VERIFY_COMMITTED_TREE_BASE", export_base_arg),
        ],
        &workspace,
    )?;
    assert_git_status_unchanged(
        &workspace,
        &before_status,
        "blocked Cargo config provenance preflight",
    )?;
    if status.success() || status.code() != Some(1) {
        return Err(format!(
            "patched Cargo home should fail closed with exit 1, got {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status.code()
        ));
    }
    if !remote_exec_invocation_lines(&invocation_log)?.is_empty()
        || !read_invocation_lines(&invocation_log)?.is_empty()
    {
        return Err("Cargo config refusal must occur before any fake RCH probe".to_owned());
    }
    if stdout.contains(cargo_home_arg) {
        return Err(format!(
            "Cargo config proof leaked the physical Cargo home path: {stdout}"
        ));
    }

    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse blocked Cargo config report: {error}"))?;
    let provenance = &report["cargo_config_provenance"];
    if report["status"] != "rch_environment_failure"
        || report["verification_attribution"] != "committed_tree"
        || report["rch_invocation"] != serde_json::json!([])
        || provenance["schema"] != "ee.rch.cargo_config_provenance.v1"
        || provenance["status"] != "blocked"
        || provenance["source_attested"] != true
        || provenance["command_locked"] != true
        || provenance["cargo_home"] != "<cargo_home>"
        || provenance["cargo_home_explicit"] != true
    {
        return Err(format!(
            "unexpected blocked Cargo config provenance: {report}"
        ));
    }
    if !degraded_contains(&report, "rch_verify_cargo_config_provenance_blocked")?
        || worker_degraded_contains(&report, "rch_verify_cargo_config_provenance_blocked")?
    {
        return Err(format!(
            "Cargo config refusal must be a verifier-environment code, not worker state: {report}"
        ));
    }

    let sources = provenance["sources"]
        .as_array()
        .ok_or_else(|| format!("missing Cargo config sources: {provenance}"))?;
    let source = sources
        .iter()
        .find(|item| item["path"] == "<cargo_home>/config.toml")
        .ok_or_else(|| format!("missing redacted Cargo home config source: {provenance}"))?;
    if source["external"] != true
        || source["effective"] != true
        || source["parse_status"] != "ok"
        || source["resolution_controls"] != serde_json::json!(["patch.crates-io"])
    {
        return Err(format!("unexpected Cargo home source record: {source}"));
    }
    let external_sources = provenance["external_resolution_sources"]
        .as_array()
        .ok_or_else(|| format!("missing external resolution sources: {provenance}"))?;
    let blocking_sources = provenance["blocking_sources"]
        .as_array()
        .ok_or_else(|| format!("missing blocking Cargo sources: {provenance}"))?;
    if external_sources.len() != 1 || blocking_sources.len() != 1 {
        return Err(format!(
            "expected exactly one external blocking source: {provenance}"
        ));
    }
    for pointer in [
        "/cargo_config_provenance/provenance_hash",
        "/cargo_config_provenance/sources/0/path_hash",
        "/cargo_config_provenance/sources/0/content_hash",
    ] {
        let hash = report
            .pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing provenance hash at {pointer}: {report}"))?;
        if !hash.starts_with("sha256:") || hash.len() != 71 {
            return Err(format!("invalid provenance hash at {pointer}: {hash}"));
        }
    }
    if !provenance["repair"]
        .as_str()
        .unwrap_or_default()
        .contains("isolated CARGO_HOME")
    {
        return Err(format!(
            "missing actionable Cargo config repair: {provenance}"
        ));
    }

    let ledger_text = fs::read_to_string(&ledger_path)
        .map_err(|error| format!("read Cargo config proof ledger: {error}"))?;
    let ledger_rows: Vec<&str> = ledger_text
        .lines()
        .filter(|line| !line.is_empty())
        .collect();
    if ledger_rows.len() != 1 {
        return Err(format!(
            "expected one Cargo config ledger row: {ledger_text}"
        ));
    }
    let ledger: Value = serde_json::from_str(ledger_rows[0])
        .map_err(|error| format!("parse Cargo config ledger row: {error}"))?;
    if ledger["status"] != "rch_environment_failure"
        || ledger["cargo_config_provenance"]["status"] != "blocked"
        || ledger["cargo_config_provenance"]["provenance_hash"] != provenance["provenance_hash"]
    {
        return Err(format!(
            "ledger did not retain Cargo config provenance: {ledger}"
        ));
    }

    let event_text = fs::read_to_string(&event_log_path)
        .map_err(|error| format!("read Cargo config event log: {error}"))?;
    let event_rows: Vec<&str> = event_text.lines().filter(|line| !line.is_empty()).collect();
    if event_rows.len() != 1 {
        return Err(format!("expected one Cargo config event row: {event_text}"));
    }
    let event: Value = serde_json::from_str(event_rows[0])
        .map_err(|error| format!("parse Cargo config event row: {error}"))?;
    let fields = &event["fields"];
    if fields["status"] != "rch_environment_failure"
        || fields["fake_rch_invoked"] != false
        || fields["fake_rch_invocation_count"] != 0
        || fields["cargo_config_provenance_status"] != "blocked"
        || fields["cargo_config_provenance_hash"] != provenance["provenance_hash"]
        || fields["cargo_config_blocking_source_count"] != 1
    {
        return Err(format!(
            "event did not retain compact Cargo config provenance: {event}"
        ));
    }
    Ok(())
}

#[test]
fn cargo_config_provenance_accepts_isolated_home_and_committed_project_patch() -> TestResult {
    let workspace = seed_system_tmp_git_workspace("rch-cargo-config-isolated")?;
    fs::create_dir_all(workspace.join(".cargo"))
        .map_err(|error| format!("create project Cargo config directory: {error}"))?;
    fs::write(
        workspace.join(".cargo/config.toml"),
        "[patch.crates-io]\nserde = { path = \"../fixture-serde\" }\n",
    )
    .map_err(|error| format!("write project Cargo config: {error}"))?;
    git(&workspace, &["add", ".cargo/config.toml"])?;
    git(&workspace, &["commit", "-m", "add project Cargo config"])?;
    let before_status = git_status_porcelain_v2(&workspace)?;

    let cargo_home = unique_system_tmp_path("rch-cargo-home-isolated");
    fs::create_dir_all(&cargo_home)
        .map_err(|error| format!("create isolated Cargo home: {error}"))?;
    let export_base = unique_system_tmp_path("rch-cargo-config-clean-export");
    let invocation_log = unique_system_tmp_path("rch-cargo-config-clean-invocations");
    let fake_rch = write_fake_rch(
        "fake-rch-cargo-config-isolated.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;

    let cargo_home_arg = cargo_home
        .to_str()
        .ok_or_else(|| "isolated Cargo home path is not utf-8".to_owned())?;
    let export_base_arg = export_base
        .to_str()
        .ok_or_else(|| "export base path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake RCH path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--skip-known-blocker",
            "--skip-build-admission",
            "--committed-tree",
            "--treeish",
            "HEAD",
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--locked",
            "--lib",
            "cargo_config_provenance_isolated_smoke",
        ],
        &[
            ("CARGO_HOME", cargo_home_arg),
            ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
            ("RCH_VERIFY_COMMITTED_TREE_BASE", export_base_arg),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "trj"),
            (
                "RCH_VERIFY_STATUS_JSON",
                r#"{"data":{"daemon":{"recent_builds":[]}}}"#,
            ),
        ],
        &workspace,
    )?;
    assert_git_status_unchanged(
        &workspace,
        &before_status,
        "isolated Cargo config provenance preflight",
    )?;
    if !status.success() {
        return Err(format!(
            "isolated Cargo home should permit source-attested locked RCH\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let remote_invocations = remote_exec_invocation_lines(&invocation_log)?;
    if remote_invocations.len() != 1
        || !remote_invocations[0]
            .contains("exec -- cargo test --locked --lib cargo_config_provenance_isolated_smoke")
    {
        return Err(format!(
            "isolated Cargo config run should invoke fake RCH exactly once: {remote_invocations:?}"
        ));
    }

    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse isolated Cargo config report: {error}"))?;
    let provenance = &report["cargo_config_provenance"];
    if report["status"] != "remote_pass"
        || report["verification_attribution"] != "committed_tree"
        || provenance["status"] != "clean"
        || provenance["source_attested"] != true
        || provenance["command_locked"] != true
        || provenance["external_resolution_sources"] != serde_json::json!([])
        || provenance["blocking_sources"] != serde_json::json!([])
    {
        return Err(format!(
            "unexpected isolated Cargo config provenance: {report}"
        ));
    }
    let project_config = provenance["sources"]
        .as_array()
        .ok_or_else(|| format!("missing project Cargo config sources: {provenance}"))?
        .iter()
        .find(|item| item["path"] == "<project>/.cargo/config.toml")
        .ok_or_else(|| format!("missing committed project Cargo config: {provenance}"))?;
    if project_config["external"] != false
        || project_config["parse_status"] != "ok"
        || project_config["resolution_controls"] != serde_json::json!(["patch.crates-io"])
    {
        return Err(format!(
            "committed project config was not classified correctly: {project_config}"
        ));
    }
    Ok(())
}

#[test]
fn cargo_config_provenance_observes_unattested_external_patch_without_blocking() -> TestResult {
    let workspace = seed_system_tmp_git_workspace("rch-cargo-config-observed")?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let cargo_home = unique_system_tmp_path("rch-cargo-home-observed");
    fs::create_dir_all(&cargo_home)
        .map_err(|error| format!("create observed Cargo home: {error}"))?;
    fs::write(
        cargo_home.join("config.toml"),
        "[patch.crates-io]\nserde = { path = \"../fixture-serde\" }\n",
    )
    .map_err(|error| format!("write observed Cargo config: {error}"))?;

    let invocation_log = unique_system_tmp_path("rch-cargo-config-observed-invocations");
    let fake_rch = write_fake_rch(
        "fake-rch-cargo-config-observed.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf '[RCH] remote trj (0.1s)\n'
"#,
    )?;
    let cargo_home_arg = cargo_home
        .to_str()
        .ok_or_else(|| "observed Cargo home path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake RCH path is not utf-8".to_owned())?;

    let (status, stdout, stderr) = run_script_with_env_in_dir(
        &[
            "--skip-known-blocker",
            "--skip-build-admission",
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--locked",
            "--lib",
            "cargo_config_provenance_observed_smoke",
        ],
        &[
            ("CARGO_HOME", cargo_home_arg),
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
    assert_git_status_unchanged(
        &workspace,
        &before_status,
        "unattested Cargo config provenance observation",
    )?;
    if !status.success() {
        return Err(format!(
            "unattested external patch should remain observable without refusal\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    if remote_exec_invocation_lines(&invocation_log)?.len() != 1 {
        return Err(format!(
            "unattested external patch should still invoke fake RCH: {:?}",
            read_invocation_lines(&invocation_log)?
        ));
    }

    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse observed Cargo config report: {error}"))?;
    let provenance = &report["cargo_config_provenance"];
    if report["status"] != "remote_pass"
        || provenance["status"] != "observed"
        || provenance["source_attested"] != false
        || provenance["command_locked"] != true
        || provenance["external_resolution_sources"]
            .as_array()
            .map(Vec::len)
            != Some(1)
        || provenance["blocking_sources"] != serde_json::json!([])
    {
        return Err(format!(
            "unexpected unattested Cargo config observation: {report}"
        ));
    }
    if degraded_contains(&report, "rch_verify_cargo_config_provenance_blocked")? {
        return Err(format!(
            "unattested external patch must not emit a refusal code: {report}"
        ));
    }
    Ok(())
}

#[test]
fn committed_tree_event_log_records_manifest_hash_and_fake_rch_count() -> TestResult {
    let workspace = seed_git_workspace("rch-committed-tree-event-log")?;
    fs::write(workspace.join("tracked.txt"), "dirty live checkout\n")
        .map_err(|error| format!("dirty tracked fixture: {error}"))?;
    fs::write(workspace.join("token-draft.txt"), "redacted fixture\n")
        .map_err(|error| format!("write untracked token fixture: {error}"))?;
    let before_status = git_status_porcelain_v2(&workspace)?;
    let invocation_log = unique_tmp_path("rch-committed-tree-event-invocations");
    let event_log = unique_tmp_path("rch-committed-tree-event").join("events.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-committed-tree-event-log.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
printf 'tracked=%s\n' "$(cat tracked.txt)"
test ! -e token-draft.txt
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
            "--committed-tree",
            "--treeish",
            "HEAD",
            "--event-log",
            event_log_arg,
            "--rch-bin",
            fake_rch_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "committed_tree_event_log_smoke",
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
    assert_git_status_unchanged(&workspace, &before_status, "committed-tree event log")?;
    if !status.success() {
        return Err(format!(
            "committed-tree event-log run should succeed from the generated source export\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let invocations = read_invocation_lines(&invocation_log)?;
    let remote_invocations = remote_exec_invocation_lines(&invocation_log)?;
    if remote_invocations.len() != 1 {
        return Err(format!(
            "committed-tree event-log mode should invoke fake RCH exec once: {invocations:?}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse committed-tree event report: {error}"))?;
    if report["status"] != "remote_pass"
        || report["verification_attribution"] != "committed_tree"
        || report["source_manifest_hash"]
            .as_str()
            .is_none_or(|hash| !hash.starts_with("sha256:") || hash.len() != 71)
        || report["resolved_commit"].as_str().map(str::len) != Some(40)
    {
        return Err(format!(
            "unexpected committed-tree event proof report: {report}"
        ));
    }

    let event_text =
        fs::read_to_string(&event_log).map_err(|error| format!("read event log: {error}"))?;
    let rows = event_text.lines().collect::<Vec<_>>();
    if rows.len() != 1 {
        return Err(format!(
            "expected one committed-tree event row, got {}",
            rows.len()
        ));
    }
    let event: Value =
        serde_json::from_str(rows[0]).map_err(|error| format!("parse event row: {error}"))?;
    if event["schema"] != "ee.test_event.v1"
        || event["kind"] != "command_end"
        || event["test_id"] != "bd-9ygik.3"
        || event["exit_code"] != 0
    {
        return Err(format!(
            "event row does not record committed-tree basics: {event}"
        ));
    }
    let fields = &event["fields"];
    if fields["status"] != "remote_pass"
        || fields["bead_id"] != "bd-9ygik.3"
        || fields["verification_attribution"] != "committed_tree"
        || fields["git_head"] != report["resolved_commit"]
        || fields["source_manifest_hash"] != report["source_manifest_hash"]
        || fields["deterministic_rerun_hash"] != report["source_manifest_hash"]
        || fields["fake_rch_invoked"] != true
        || fields["fake_rch_invocation_count"] != 1
        || fields["source_state_degraded_codes"] != serde_json::json!([])
        || fields["worker_state_degraded_codes"] != serde_json::json!([])
    {
        return Err(format!(
            "event row missing committed-tree manifest/fake-RCH fields: {event}"
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
printf 'RCH_WORKER=%s\n' "${RCH_WORKER:-}"
printf 'RCH_WORKERS=%s\n' "${RCH_WORKERS:-}"
printf 'RCH_SOCKET_PATH=%s\n' "${RCH_SOCKET_PATH:-}"
printf 'RCH_BUILD_TIMEOUT_SEC=%s\n' "${RCH_BUILD_TIMEOUT_SEC:-}"
printf 'RCH_TEST_TIMEOUT_SEC=%s\n' "${RCH_TEST_TIMEOUT_SEC:-}"
printf 'RCH_CANONICAL_PROJECT_ROOT=%s\n' "${RCH_CANONICAL_PROJECT_ROOT:-}"
printf 'RCH_ALIAS_PROJECT_ROOT=%s\n' "${RCH_ALIAS_PROJECT_ROOT:-}"
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
            ("RCH_WORKER", "trj"),
            ("RCH_SOCKET_PATH", "/tmp/rch-alt-test.sock"),
            ("RCH_BUILD_TIMEOUT_SEC", "1200"),
            ("RCH_TEST_TIMEOUT_SEC", "1500"),
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
    if !stdout_tail.contains("RCH_WORKER=trj") {
        return Err(format!(
            "first invocation did not receive RCH_WORKER: {report}"
        ));
    }
    if !stdout_tail.contains("RCH_SOCKET_PATH=/tmp/rch-alt-test.sock") {
        return Err(format!(
            "first invocation did not receive RCH_SOCKET_PATH: {report}"
        ));
    }
    if !stdout_tail.contains("RCH_BUILD_TIMEOUT_SEC=1200") {
        return Err(format!(
            "first invocation did not receive RCH_BUILD_TIMEOUT_SEC: {report}"
        ));
    }
    if !stdout_tail.contains("RCH_TEST_TIMEOUT_SEC=1500") {
        return Err(format!(
            "first invocation did not receive RCH_TEST_TIMEOUT_SEC: {report}"
        ));
    }
    let expected_canonical_root = repo_root()
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "repo root has no parent".to_owned())?
        .display()
        .to_string();
    let expected_canonical_line = format!("RCH_CANONICAL_PROJECT_ROOT={expected_canonical_root}");
    if !stdout_tail.contains(&expected_canonical_line) {
        return Err(format!(
            "first invocation did not receive project-root topology: {report}"
        ));
    }
    if !stdout_tail.contains("RCH_ALIAS_PROJECT_ROOT=/data") {
        return Err(format!(
            "first invocation did not receive worker alias topology: {report}"
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
fn remote_compile_defaults_to_long_build_timeout() -> TestResult {
    let fake_rch = write_fake_rch(
        "fake-rch-default-build-timeout.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'RCH_BUILD_TIMEOUT_SEC=%s\n' "${RCH_BUILD_TIMEOUT_SEC:-}"
printf 'RCH_TEST_TIMEOUT_SEC=%s\n' "${RCH_TEST_TIMEOUT_SEC:-}"
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
            "check",
            "--all-targets",
        ],
        &[
            ("RCH_VERIFY_CONFIGURED_WORKERS", "trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "trj"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "fake rch default-timeout invocation failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse default-timeout proof: {error}"))?;
    let stdout_tail = report["stdout_tail"]
        .as_str()
        .ok_or_else(|| "missing stdout_tail".to_owned())?;
    if !stdout_tail.contains("RCH_BUILD_TIMEOUT_SEC=900") {
        return Err(format!(
            "cargo check should receive the default long build timeout: {report}"
        ));
    }
    if !stdout_tail.contains("RCH_TEST_TIMEOUT_SEC=") {
        return Err(format!(
            "test timeout line missing from fake output: {report}"
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
fn selector_admission_probe_schema_pins_required_fields_and_enums() -> TestResult {
    let schema = read_repo_json("docs/schemas/ee.rch.selector_admission_probe.v1.json")?;
    if schema["title"] != "ee.rch.selector_admission_probe.v1" {
        return Err(format!(
            "unexpected selector admission schema title: {schema}"
        ));
    }

    let required = string_set_at(&schema, "/required")?;
    let expected_required = string_set(&[
        "schema",
        "status",
        "required_runtime",
        "workers_reported",
        "daemon_workers_reported",
        "workers_reported_count",
        "daemon_workers_reported_count",
        "selected_worker",
        "selection_failure_reason",
        "workers_vs_selection_contradiction",
        "path_normalization_warning",
        "remote_required",
        "local_fallback_refused",
        "admission_blocker",
    ]);
    if required != expected_required {
        return Err(format!(
            "selector admission required fields drifted:\nexpected={expected_required:?}\nactual={required:?}"
        ));
    }

    let status_enum = string_set_at(&schema, "/properties/status/enum")?;
    let expected_status = string_set(&["selected", "selection_failed", "not_applicable"]);
    if status_enum != expected_status {
        return Err(format!(
            "selector admission status enum drifted:\nexpected={expected_status:?}\nactual={status_enum:?}"
        ));
    }

    let failure_enum = string_set_at(&schema, "/properties/selection_failure_reason/oneOf/0/enum")?;
    let expected_failures = string_set(&[
        "no_workers_with_rust_installed",
        "no_workers_passed_health",
        "topology_blocked",
        "capacity_or_timeout",
        "all_workers_preflight_failed",
        "command_not_offloaded",
        "active_project_exclusion",
        "remote_marker_missing",
        "no_worker_selected",
    ]);
    if failure_enum != expected_failures {
        return Err(format!(
            "selector admission failure enum drifted:\nexpected={expected_failures:?}\nactual={failure_enum:?}"
        ));
    }
    let blocker_required =
        string_set_at(&schema, "/properties/admission_blocker/oneOf/0/required")?;
    let expected_blocker_required = string_set(&["kind", "retry_guidance", "evidence"]);
    if blocker_required != expected_blocker_required {
        return Err(format!(
            "selector blocker required fields drifted:\nexpected={expected_blocker_required:?}\nactual={blocker_required:?}"
        ));
    }
    let worker_posture_enum = string_set_at(
        &schema,
        "/properties/admission_blocker/oneOf/0/properties/worker_posture/enum",
    )?;
    let expected_worker_posture = string_set(&[
        "active",
        "progress_stale",
        "heartbeat_stale",
        "hook_inactive",
    ]);
    if worker_posture_enum != expected_worker_posture {
        return Err(format!(
            "selector blocker worker posture enum drifted:\nexpected={expected_worker_posture:?}\nactual={worker_posture_enum:?}"
        ));
    }

    let fixture = read_repo_json(
        "tests/fixtures/rch_verify_control_plane/selector_admission_probe_selection_failed.json",
    )?;
    let fixture_object = fixture
        .as_object()
        .ok_or_else(|| format!("selector admission fixture must be an object: {fixture}"))?;
    for field in &expected_required {
        if !fixture_object.contains_key(field) {
            return Err(format!(
                "selector admission fixture missing required field {field}"
            ));
        }
    }
    if fixture["schema"] != "ee.rch.selector_admission_probe.v1"
        || fixture["status"] != "selection_failed"
        || fixture["selection_failure_reason"] != "no_workers_with_rust_installed"
        || fixture["local_fallback_refused"] != true
    {
        return Err(format!(
            "selector admission fixture has unexpected values: {fixture}"
        ));
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
    let probe = selector_probe(&report)?;
    if probe["status"] != "selected"
        || probe["required_runtime"] != "Rust"
        || probe["selected_worker"] != "trj"
        || !probe["selection_failure_reason"].is_null()
        || probe["workers_vs_selection_contradiction"] != false
        || probe["local_fallback_refused"] != false
    {
        return Err(format!(
            "selector admission probe did not preserve selected worker: {probe}"
        ));
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
fn client_daemon_version_skew_refuses_before_rch() -> TestResult {
    let invocation_log = unique_tmp_path("rch-version-skew-invocations");
    let fake_rch = write_fake_rch(
        "fake-rch-version-skew.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf 'rch 1.0.24\n'
  exit 0
fi
if [ "${1:-}" = "status" ]; then
  cat <<'JSON'
{"data":{"daemon":{"version":"0.1.3","socket_path":"/tmp/rch.sock","workers":[],"recent_builds":[]}}}
JSON
  exit 0
fi
if [ "${1:-}" = "exec" ]; then
  printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
  printf '[RCH] remote trj (0.1s)\n'
  exit 0
fi
printf 'unexpected fake rch args: %s\n' "$*" >&2
exit 2
"#,
    )?;
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
            "--summary",
            "--",
            "cargo",
            "test",
            "--lib",
            "version_skew_should_not_run",
        ],
        &[("FAKE_RCH_INVOCATIONS", invocation_log_arg)],
    )?;
    if status.success() {
        return Err("client/daemon version skew should refuse before RCH".to_owned());
    }
    if invocation_log.exists() {
        let invocations = fs::read_to_string(&invocation_log)
            .map_err(|error| format!("read version-skew invocation log: {error}"))?;
        if !invocations.is_empty() {
            return Err(format!(
                "version-skew preflight invoked fake RCH exec: {invocations:?}"
            ));
        }
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse version-skew report: {error}"))?;
    if report["status"] != "rch_environment_failure" || report["exit_code"] != 1 {
        return Err(format!("unexpected version-skew status: {report}"));
    }
    if !degraded_contains(&report, "rch_verify_client_daemon_version_skew")?
        || !worker_degraded_contains(&report, "rch_verify_client_daemon_version_skew")?
    {
        return Err(format!(
            "version-skew code should be degraded and worker-state: {report}"
        ));
    }
    let runtime = &report["rch_runtime"];
    if runtime["status"] != "checked"
        || runtime["client_version"] != "1.0.24"
        || runtime["client_compat"] != "1.0"
        || runtime["daemon_version"] != "0.1.3"
        || runtime["daemon_compat"] != "0.1"
        || runtime["daemon_socket_path"] != "/tmp/rch.sock"
    {
        return Err(format!(
            "runtime version details should route deployment work: {report}"
        ));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    if !summary.contains("rch_runtime: `checked` client=`1.0.24` daemon=`0.1.3`") {
        return Err(format!("summary missing runtime skew: {summary}"));
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
        let invocations = read_invocation_lines(&invocation_log)?;
        let remote_invocations = remote_exec_invocation_lines(&invocation_log)?;
        if !remote_invocations.is_empty() {
            return Err(format!(
                "build-admission denial invoked fake RCH exec: {invocations:?}"
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
    if !invocations.contains("exec -- cargo test --lib admission_pass_smoke") {
        return Err(format!(
            "fake RCH did not receive expected invocation: {invocations}"
        ));
    }
    if invocations.contains("exec -- env ") {
        return Err(format!(
            "fake RCH invocation should not hide cargo behind env: {invocations}"
        ));
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
fn summary_reports_local_cargo_process_fixture_without_using_local_cargo() -> TestResult {
    let local_cargo_report = r#"{
        "schema":"ee.rch_local_cargo_tripwire.v1",
        "mode":"probe_processes",
        "status":"bypass_detected",
        "count":1,
        "processes":[{
            "pid":"7193",
            "ppid":"1",
            "elapsed":"02:33:44",
            "command_kind":"cargo",
            "subcommand":"metadata",
            "cwd":"-",
            "manifestPath":"/Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml",
            "workspacePath":"/Users/jemanuel/projects/eidetic_engine_cli",
            "packageCacheLockState":"held",
            "packageCacheLockHeld":true,
            "policyStatus":"local_cargo_read_only_lock_holder",
            "command":"cargo metadata --manifest-path /Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml",
            "reason":"read-only cargo metadata process holds the Cargo package-cache lock and can block RCH verification"
        }],
        "detectedLocalBuilds":[],
        "localBuildPolicy":{"policy":"rch_only","status":"blocked","commandScope":"active_process_scan"}
    }"#;
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--summary",
            "--skip-build-admission",
            "--",
            "cargo",
            "test",
            "--lib",
            "local_cargo_process_fixture",
        ],
        &[
            ("RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON", local_cargo_report),
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "Selected worker: css\n[RCH] remote css (0.1s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "77"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "local-cargo fixture summary should pass remotely with warning; stdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse local cargo report: {error}"))?;
    if report["status"] != "remote_pass" {
        return Err(format!("local-cargo warning changed RCH status: {report}"));
    }
    if report["local_cargo_processes"]["count"] != 1
        || report["local_cargo_processes"]["processes"][0]["packageCacheLockHeld"] != true
    {
        return Err(format!("local cargo process report missing: {report}"));
    }
    if !degraded_contains(&report, "rch_verify_local_cargo_processes_present")? {
        return Err(format!("local cargo degraded code missing: {report}"));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    if !summary
        .contains("local_cargo_processes: `bypass_detected` count=`1` package_cache_locks=`1`")
    {
        return Err(format!(
            "summary missing local cargo process line: {summary}"
        ));
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
fn proof_broker_dispatch_allowed_launches_single_remote_proof() -> TestResult {
    let ledger = unique_tmp_path("proof-broker-dispatch-ledger").join("ledger.json");
    fs::create_dir_all(
        ledger
            .parent()
            .ok_or_else(|| "ledger path missing parent".to_owned())?,
    )
    .map_err(|error| format!("create proof-broker ledger dir: {error}"))?;
    fs::write(&ledger, "[]").map_err(|error| format!("write proof-broker ledger: {error}"))?;
    let fake_ee = write_fake_proof_broker_ee("fake-ee-proof-dispatch.sh")?;
    let fake_ee_log = unique_tmp_path("proof-broker-dispatch-ee-invocations");
    let ledger_arg = ledger
        .to_str()
        .ok_or_else(|| "ledger path is not utf-8".to_owned())?;
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_ee_log_arg = fake_ee_log
        .to_str()
        .ok_or_else(|| "fake ee invocation log is not utf-8".to_owned())?;
    let clean_tripwire = r#"{"schema":"ee.rch_local_cargo_tripwire.v1","mode":"probe_processes","status":"ok","count":0,"processes":[],"detectedLocalBuilds":[]}"#;

    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--skip-build-admission",
            "--proof-broker-ledger",
            ledger_arg,
            "--proof-broker-ee-bin",
            fake_ee_arg,
            "--summary",
            "--",
            "cargo",
            "test",
            "--lib",
            "proof_broker_dispatch",
        ],
        &[
            ("FAKE_EE_INVOCATIONS", fake_ee_log_arg),
            ("FAKE_PROOF_VERDICT", "dispatch_allowed"),
            ("RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON", clean_tripwire),
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "Selected worker: trj\n[RCH] remote trj (0.1s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "31"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "trj"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "dispatch-allowed broker run should pass\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse proof broker dispatch report: {error}"))?;
    if report["status"] != "remote_pass"
        || report["worker_id"] != "trj"
        || report["proof_broker"]["verdict"] != "dispatch_allowed"
        || report["proof_broker"]["remoteCargoLaunched"] != true
    {
        return Err(format!(
            "dispatch admission did not launch one remote proof: {report}"
        ));
    }
    if degraded_contains(&report, "rch_verify_proof_broker_bypassed")? {
        return Err(format!(
            "dispatch admission should not be bypassed: {report}"
        ));
    }
    let invocations = fs::read_to_string(&fake_ee_log)
        .map_err(|error| format!("read fake proof ee invocations: {error}"))?;
    if !invocations.contains("proof admit")
        || !invocations.contains("--local-cargo-tripwire-class class:tripwire_clean")
        || !invocations.contains("--build-admission-posture class:admission_skipped")
        || !invocations.contains("-- cargo test --lib proof_broker_dispatch")
    {
        return Err(format!(
            "proof broker admission did not receive expected fingerprint args: {invocations}"
        ));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    if !summary.contains("proof_broker: `dispatch_allowed` remote_cargo_launched=`true`") {
        return Err(format!("summary missing broker dispatch line: {summary}"));
    }
    let ledger_rows: Value = serde_json::from_str(
        &fs::read_to_string(&ledger).map_err(|error| format!("read broker ledger: {error}"))?,
    )
    .map_err(|error| format!("parse updated broker ledger: {error}"))?;
    let rows = ledger_rows
        .as_array()
        .ok_or_else(|| format!("broker ledger should stay a JSON array: {ledger_rows}"))?;
    if rows.len() != 1
        || rows[0]["schema"] != "ee.proof_broker.v1"
        || rows[0]["state"] != "completed"
        || rows[0]["admission"]["verdict"] != "reuse_existing"
        || rows[0]["admission"]["reuseRunId"]
            .as_str()
            .unwrap_or("")
            .is_empty()
        || rows[0]["rawOutputIncluded"] != false
    {
        return Err(format!(
            "dispatch did not append reusable broker ledger row: {ledger_rows}"
        ));
    }
    Ok(())
}

#[test]
fn proof_broker_reuse_existing_skips_remote_dispatch() -> TestResult {
    let ledger = unique_tmp_path("proof-broker-reuse-ledger").join("ledger.json");
    fs::create_dir_all(
        ledger
            .parent()
            .ok_or_else(|| "ledger path missing parent".to_owned())?,
    )
    .map_err(|error| format!("create proof-broker ledger dir: {error}"))?;
    fs::write(&ledger, "[]").map_err(|error| format!("write proof-broker ledger: {error}"))?;
    let fake_ee = write_fake_proof_broker_ee("fake-ee-proof-reuse.sh")?;
    let fake_ee_log = unique_tmp_path("proof-broker-reuse-ee-invocations");
    let ledger_arg = ledger
        .to_str()
        .ok_or_else(|| "ledger path is not utf-8".to_owned())?;
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_ee_log_arg = fake_ee_log
        .to_str()
        .ok_or_else(|| "fake ee invocation log is not utf-8".to_owned())?;
    let clean_tripwire = r#"{"schema":"ee.rch_local_cargo_tripwire.v1","mode":"probe_processes","status":"ok","count":0,"processes":[],"detectedLocalBuilds":[]}"#;

    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--skip-build-admission",
            "--proof-broker-ledger",
            ledger_arg,
            "--proof-broker-ee-bin",
            fake_ee_arg,
            "--summary",
            "--",
            "cargo",
            "test",
            "--lib",
            "proof_broker_reuse",
        ],
        &[
            ("FAKE_EE_INVOCATIONS", fake_ee_log_arg),
            ("FAKE_PROOF_VERDICT", "reuse_existing"),
            ("RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON", clean_tripwire),
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] remote should-not-run (0.1s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "reuse-existing broker run should return success\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse proof broker reuse report: {error}"))?;
    if report["status"] != "proof_broker_reuse"
        || report["exit_code"] != 0
        || !report["rch_invocation"]
            .as_array()
            .ok_or_else(|| "missing rch_invocation".to_owned())?
            .is_empty()
        || report["proof_broker"]["verdict"] != "reuse_existing"
        || report["proof_broker"]["reuseRunId"] != "vrun_existing"
        || report["proof_broker"]["remoteCargoLaunched"] != false
    {
        return Err(format!(
            "reuse admission did not skip remote dispatch: {report}"
        ));
    }
    if !degraded_contains(&report, "rch_verify_proof_broker_reuse_existing")? {
        return Err(format!("reuse degraded code missing: {report}"));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    if !summary.contains("proof_broker: `reuse_existing` remote_cargo_launched=`false`") {
        return Err(format!("summary missing broker reuse line: {summary}"));
    }
    Ok(())
}

#[test]
fn proof_broker_wait_for_inflight_refuses_before_remote_dispatch() -> TestResult {
    let ledger = unique_tmp_path("proof-broker-wait-ledger").join("ledger.json");
    fs::create_dir_all(
        ledger
            .parent()
            .ok_or_else(|| "ledger path missing parent".to_owned())?,
    )
    .map_err(|error| format!("create proof-broker ledger dir: {error}"))?;
    fs::write(&ledger, "[]").map_err(|error| format!("write proof-broker ledger: {error}"))?;
    let fake_ee = write_fake_proof_broker_ee("fake-ee-proof-wait.sh")?;
    let fake_ee_log = unique_tmp_path("proof-broker-wait-ee-invocations");
    let ledger_arg = ledger
        .to_str()
        .ok_or_else(|| "ledger path is not utf-8".to_owned())?;
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_ee_log_arg = fake_ee_log
        .to_str()
        .ok_or_else(|| "fake ee invocation log is not utf-8".to_owned())?;
    let clean_tripwire = r#"{"schema":"ee.rch_local_cargo_tripwire.v1","mode":"probe_processes","status":"ok","count":0,"processes":[],"detectedLocalBuilds":[]}"#;

    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--skip-build-admission",
            "--proof-broker-ledger",
            ledger_arg,
            "--proof-broker-ee-bin",
            fake_ee_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "proof_broker_wait",
        ],
        &[
            ("FAKE_EE_INVOCATIONS", fake_ee_log_arg),
            ("FAKE_PROOF_VERDICT", "wait_for_inflight"),
            ("RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON", clean_tripwire),
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] remote should-not-run (0.1s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
        ],
    )?;
    if status.success() {
        return Err("wait-for-inflight admission should refuse before RCH".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse proof broker wait report: {error}"))?;
    if report["status"] != "proof_broker_refused"
        || report["exit_code"] != 1
        || report["proof_broker"]["verdict"] != "wait_for_inflight"
        || report["proof_broker"]["waitOwner"]["rchJobId"] != "rch-job-fake"
        || report["proof_broker"]["remoteCargoLaunched"] != false
    {
        return Err(format!("wait admission did not refuse correctly: {report}"));
    }
    if !degraded_contains(&report, "rch_verify_proof_broker_wait_for_inflight")? {
        return Err(format!("wait degraded code missing: {report}"));
    }
    Ok(())
}

#[test]
fn proof_broker_source_mismatch_refuses_before_remote_dispatch() -> TestResult {
    let ledger = unique_tmp_path("proof-broker-source-mismatch-ledger").join("ledger.json");
    fs::create_dir_all(
        ledger
            .parent()
            .ok_or_else(|| "ledger path missing parent".to_owned())?,
    )
    .map_err(|error| format!("create proof-broker ledger dir: {error}"))?;
    fs::write(&ledger, "[]").map_err(|error| format!("write proof-broker ledger: {error}"))?;
    let fake_ee = write_fake_proof_broker_ee("fake-ee-proof-source-mismatch.sh")?;
    let fake_ee_log = unique_tmp_path("proof-broker-source-mismatch-ee-invocations");
    let ledger_arg = ledger
        .to_str()
        .ok_or_else(|| "ledger path is not utf-8".to_owned())?;
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_ee_log_arg = fake_ee_log
        .to_str()
        .ok_or_else(|| "fake ee invocation log is not utf-8".to_owned())?;
    let clean_tripwire = r#"{"schema":"ee.rch_local_cargo_tripwire.v1","mode":"probe_processes","status":"ok","count":0,"processes":[],"detectedLocalBuilds":[]}"#;

    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--skip-build-admission",
            "--proof-broker-ledger",
            ledger_arg,
            "--proof-broker-ee-bin",
            fake_ee_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "proof_broker_source_mismatch",
        ],
        &[
            ("FAKE_EE_INVOCATIONS", fake_ee_log_arg),
            ("FAKE_PROOF_VERDICT", "source_state_mismatch"),
            ("RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON", clean_tripwire),
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] remote should-not-run (0.1s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
        ],
    )?;
    if status.success() {
        return Err("source mismatch admission should refuse before RCH".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse proof broker source mismatch report: {error}"))?;
    if report["status"] != "proof_broker_refused"
        || report["proof_broker"]["verdict"] != "source_state_mismatch"
        || report["proof_broker"]["remoteCargoLaunched"] != false
    {
        return Err(format!(
            "source mismatch admission did not refuse: {report}"
        ));
    }
    if !degraded_contains(&report, "rch_verify_proof_broker_source_state_mismatch")? {
        return Err(format!("source mismatch degraded code missing: {report}"));
    }
    Ok(())
}

#[test]
fn proof_broker_environment_blocked_refuses_before_remote_dispatch() -> TestResult {
    let ledger = unique_tmp_path("proof-broker-environment-blocked-ledger").join("ledger.json");
    fs::create_dir_all(
        ledger
            .parent()
            .ok_or_else(|| "ledger path missing parent".to_owned())?,
    )
    .map_err(|error| format!("create proof-broker ledger dir: {error}"))?;
    fs::write(&ledger, "[]").map_err(|error| format!("write proof-broker ledger: {error}"))?;
    let fake_ee = write_fake_proof_broker_ee("fake-ee-proof-environment-blocked.sh")?;
    let fake_ee_log = unique_tmp_path("proof-broker-environment-blocked-ee-invocations");
    let ledger_arg = ledger
        .to_str()
        .ok_or_else(|| "ledger path is not utf-8".to_owned())?;
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_ee_log_arg = fake_ee_log
        .to_str()
        .ok_or_else(|| "fake ee invocation log is not utf-8".to_owned())?;
    let clean_tripwire = r#"{"schema":"ee.rch_local_cargo_tripwire.v1","mode":"probe_processes","status":"ok","count":0,"processes":[],"detectedLocalBuilds":[]}"#;

    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--skip-build-admission",
            "--proof-broker-ledger",
            ledger_arg,
            "--proof-broker-ee-bin",
            fake_ee_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "proof_broker_environment_blocked",
        ],
        &[
            ("FAKE_EE_INVOCATIONS", fake_ee_log_arg),
            ("FAKE_PROOF_VERDICT", "environment_blocked"),
            ("RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON", clean_tripwire),
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] remote should-not-run (0.1s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
        ],
    )?;
    if status.success() {
        return Err("environment-blocked admission should refuse before RCH".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse proof broker environment-blocked report: {error}"))?;
    if report["status"] != "proof_broker_refused"
        || report["proof_broker"]["verdict"] != "environment_blocked"
        || report["proof_broker"]["remoteCargoLaunched"] != false
        || !report["rch_invocation"]
            .as_array()
            .ok_or_else(|| "missing rch_invocation".to_owned())?
            .is_empty()
    {
        return Err(format!(
            "environment-blocked admission did not refuse before remote dispatch: {report}"
        ));
    }
    if !degraded_contains(&report, "rch_verify_proof_broker_environment_blocked")? {
        return Err(format!(
            "environment-blocked degraded code missing: {report}"
        ));
    }
    let invocations = fs::read_to_string(&fake_ee_log)
        .map_err(|error| format!("read fake proof ee invocations: {error}"))?;
    if !invocations.contains("proof admit")
        || !invocations.contains("-- cargo test --lib proof_broker_environment_blocked")
    {
        return Err(format!(
            "proof broker environment-blocked admission did not receive expected command: {invocations}"
        ));
    }
    Ok(())
}

#[test]
fn proof_broker_local_cargo_bypass_is_unusable_without_bypass() -> TestResult {
    let ledger = unique_tmp_path("proof-broker-local-bypass-ledger").join("ledger.json");
    fs::create_dir_all(
        ledger
            .parent()
            .ok_or_else(|| "ledger path missing parent".to_owned())?,
    )
    .map_err(|error| format!("create proof-broker ledger dir: {error}"))?;
    fs::write(&ledger, "[]").map_err(|error| format!("write proof-broker ledger: {error}"))?;
    let fake_ee = write_fake_proof_broker_ee("fake-ee-proof-local-bypass.sh")?;
    let fake_ee_log = unique_tmp_path("proof-broker-local-bypass-ee-invocations");
    let ledger_arg = ledger
        .to_str()
        .ok_or_else(|| "ledger path is not utf-8".to_owned())?;
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_ee_log_arg = fake_ee_log
        .to_str()
        .ok_or_else(|| "fake ee invocation log is not utf-8".to_owned())?;
    let bypass_tripwire = r#"{"schema":"ee.rch_local_cargo_tripwire.v1","mode":"probe_processes","status":"bypass_detected","count":1,"processes":[{"pid":"123","packageCacheLockHeld":true}],"detectedLocalBuilds":[]}"#;

    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--skip-build-admission",
            "--proof-broker-ledger",
            ledger_arg,
            "--proof-broker-ee-bin",
            fake_ee_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            "proof_broker_local_cargo_bypass",
        ],
        &[
            ("FAKE_EE_INVOCATIONS", fake_ee_log_arg),
            ("FAKE_PROOF_VERDICT", "proof_unusable"),
            ("RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON", bypass_tripwire),
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] remote should-not-run (0.1s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
        ],
    )?;
    if status.success() {
        return Err("local Cargo bypass admission should refuse before RCH".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse proof broker local bypass report: {error}"))?;
    if report["status"] != "proof_broker_refused"
        || report["proof_broker"]["verdict"] != "proof_unusable"
        || report["local_cargo_processes"]["status"] != "bypass_detected"
    {
        return Err(format!("local bypass admission did not refuse: {report}"));
    }
    if !degraded_contains(&report, "rch_verify_proof_broker_proof_unusable")?
        || !degraded_contains(&report, "rch_verify_local_cargo_processes_present")?
    {
        return Err(format!("local bypass degraded codes missing: {report}"));
    }
    let invocations = fs::read_to_string(&fake_ee_log)
        .map_err(|error| format!("read fake proof ee invocations: {error}"))?;
    if !invocations.contains("--local-cargo-tripwire-class class:local_cargo_bypass_detected") {
        return Err(format!(
            "local Cargo bypass class was not sent to proof admission: {invocations}"
        ));
    }
    Ok(())
}

#[test]
fn proof_broker_explicit_bypass_runs_remote_and_records_reason() -> TestResult {
    let ledger = unique_tmp_path("proof-broker-explicit-bypass-ledger").join("ledger.json");
    fs::create_dir_all(
        ledger
            .parent()
            .ok_or_else(|| "ledger path missing parent".to_owned())?,
    )
    .map_err(|error| format!("create proof-broker ledger dir: {error}"))?;
    fs::write(&ledger, "[]").map_err(|error| format!("write proof-broker ledger: {error}"))?;
    let fake_ee = write_fake_proof_broker_ee("fake-ee-proof-explicit-bypass.sh")?;
    let fake_ee_log = unique_tmp_path("proof-broker-explicit-bypass-ee-invocations");
    let ledger_arg = ledger
        .to_str()
        .ok_or_else(|| "ledger path is not utf-8".to_owned())?;
    let fake_ee_arg = fake_ee
        .to_str()
        .ok_or_else(|| "fake ee path is not utf-8".to_owned())?;
    let fake_ee_log_arg = fake_ee_log
        .to_str()
        .ok_or_else(|| "fake ee invocation log is not utf-8".to_owned())?;
    let clean_tripwire = r#"{"schema":"ee.rch_local_cargo_tripwire.v1","mode":"probe_processes","status":"ok","count":0,"processes":[],"detectedLocalBuilds":[]}"#;

    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--skip-build-admission",
            "--proof-broker-ledger",
            ledger_arg,
            "--proof-broker-ee-bin",
            fake_ee_arg,
            "--proof-broker-bypass",
            "human requested emergency rerun",
            "--",
            "cargo",
            "test",
            "--lib",
            "proof_broker_bypass",
        ],
        &[
            ("FAKE_EE_INVOCATIONS", fake_ee_log_arg),
            ("FAKE_PROOF_VERDICT", "wait_for_inflight"),
            ("RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON", clean_tripwire),
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "Selected worker: css\n[RCH] remote css (0.1s)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "0"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "19"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "css"),
            ("RCH_VERIFY_DAEMON_WORKERS", "css"),
        ],
    )?;
    if !status.success() {
        return Err(format!(
            "explicit proof-broker bypass should run remote proof\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse proof broker bypass report: {error}"))?;
    if report["status"] != "remote_pass"
        || report["worker_id"] != "css"
        || report["proof_broker"]["verdict"] != "wait_for_inflight"
        || report["proof_broker"]["remoteCargoLaunched"] != true
        || report["proof_broker"]["bypassReason"] != "human requested emergency rerun"
    {
        return Err(format!(
            "explicit bypass did not run and record reason: {report}"
        ));
    }
    if !degraded_contains(&report, "rch_verify_proof_broker_bypassed")?
        || !degraded_contains(&report, "rch_verify_proof_broker_wait_for_inflight")?
    {
        return Err(format!("explicit bypass degraded codes missing: {report}"));
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
    let probe = selector_probe(&report)?;
    if probe["selected_worker"].is_null()
        && probe["selection_failure_reason"] == "topology_blocked"
        && probe["local_fallback_refused"] == true
    {
        return Ok(());
    }
    Err(format!(
        "selector admission probe did not capture fallback refusal: {probe}"
    ))
}

#[test]
fn selector_admission_probe_flags_reported_workers_without_selection() -> TestResult {
    let (status, stdout, stderr) = run_script_with_env(
        &["--summary", "--no-write", "--", "cargo", "test", "--lib"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] project root normalization warning: canonical /Users/jemanuel/projects/eidetic_engine_cli -> /data/projects/eidetic_engine_cli\n[RCH] local (no workers with Rust installed)\n[RCH] remote required; refusing local fallback (no worker assigned)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "42"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854,vmi1264463"),
            ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
        ],
    )?;
    if status.success() {
        return Err("selector admission failure should preserve non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse selector admission report: {error}\nstderr:\n{stderr}"))?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "selector admission failure should be an environment failure: {report}"
        ));
    }
    let probe = selector_probe(&report)?;
    if probe["status"] != "selection_failed"
        || probe["required_runtime"] != "Rust"
        || !probe["selected_worker"].is_null()
        || probe["selection_failure_reason"] != "no_workers_with_rust_installed"
        || probe["workers_vs_selection_contradiction"] != true
        || probe["remote_required"] != true
        || probe["local_fallback_refused"] != true
    {
        return Err(format!(
            "selector admission probe did not capture selection failure: {probe}"
        ));
    }
    let expected_probe = read_repo_json(
        "tests/fixtures/rch_verify_control_plane/selector_admission_probe_selection_failed.json",
    )?;
    if probe != &expected_probe {
        return Err(format!(
            "selector admission probe does not match conformance fixture:\nexpected={expected_probe}\nactual={probe}"
        ));
    }
    if probe["workers_reported"][0] != "vmi1227854"
        || probe["workers_reported"][1] != "vmi1264463"
        || probe["daemon_workers_reported"][0] != "vmi1227854"
    {
        return Err(format!(
            "selector admission probe did not preserve worker reports: {probe}"
        ));
    }
    let path_warning = probe["path_normalization_warning"]
        .as_str()
        .ok_or_else(|| format!("missing path normalization warning: {probe}"))?;
    if !path_warning.contains("/Users/<redacted>") || path_warning.contains("/Users/jemanuel") {
        return Err(format!(
            "path normalization warning was not redacted: {path_warning}"
        ));
    }
    if !stdout.contains("selector_admission: `selection_failed`") {
        return Err(format!(
            "summary did not include selector admission line:\n{stdout}"
        ));
    }
    Ok(())
}

#[test]
fn selector_admission_probe_classifies_worker_health_threshold_block() -> TestResult {
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--summary",
            "--no-write",
            "--",
            "cargo",
            "build",
            "--bin",
            "ee",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] local (no workers passed health thresholds)\n[RCH] remote required; refusing local fallback (no worker assigned)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "12"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1149989"),
            ("RCH_VERIFY_DAEMON_WORKERS", "vmi1149989"),
        ],
    )?;
    if status.success() {
        return Err("health-threshold selector failure should preserve non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout).map_err(|error| {
        format!("parse health-threshold selector report: {error}\nstderr:\n{stderr}")
    })?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "health-threshold refusal should be an environment failure: {report}"
        ));
    }
    if report["command_kind"] != "cargo_build" {
        return Err(format!(
            "cargo build should be admitted as cargo_build: {report}"
        ));
    }
    for expected in [
        "rch_verify_worker_health_threshold_blocked",
        "rch_verify_local_fallback_refused",
        "rch_verify_capacity_or_timeout",
        "rch_verify_remote_marker_missing",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in health-threshold proof: {report}"
            ));
        }
    }
    if !worker_degraded_contains(&report, "rch_verify_worker_health_threshold_blocked")? {
        return Err(format!(
            "health-threshold code should be worker-state evidence: {report}"
        ));
    }
    let probe = selector_probe(&report)?;
    if probe["status"] != "selection_failed"
        || probe["selection_failure_reason"] != "no_workers_passed_health"
        || probe["workers_vs_selection_contradiction"] != true
        || probe["local_fallback_refused"] != true
    {
        return Err(format!(
            "selector probe did not preserve health-threshold reason: {probe}"
        ));
    }
    Ok(())
}

#[test]
fn selector_admission_probe_classifies_active_project_exclusion() -> TestResult {
    let (status, stdout, stderr) = run_script_with_env(
        &["--summary", "--no-write", "--", "cargo", "test", "--lib"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "[RCH] selection blocked: active_project_exclusion=1 active_build=29879340221071365 progress=stale\n[RCH] remote required; refusing local fallback (no worker assigned)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "21"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "trj"),
            ("RCH_VERIFY_DAEMON_WORKERS", "trj"),
            (
                "RCH_VERIFY_FAKE_QUEUE_JSON",
                r#"{
  "api_version": "1.0",
  "command": "queue",
  "success": true,
    "data": {
    "active_builds": [
      {
        "id": 29879340221071367,
        "command": "cargo test --test unrelated_e2e -- --nocapture",
        "detector_build_age_secs": 999,
        "detector_heartbeat_stale": false,
        "detector_hook_alive": true,
        "detector_progress_stale": false,
        "detector_slots_owned": 1,
        "heartbeat_age_secs": 1,
        "progress_age_secs": 2,
        "worker_id": "unrelated"
      },
      {
        "id": 29879340221071365,
        "command": "cargo test --test error_recall_e2e -- --nocapture",
        "detector_build_age_secs": 327,
        "detector_heartbeat_stale": false,
        "detector_hook_alive": true,
        "detector_progress_stale": true,
        "detector_slots_owned": 2,
        "heartbeat_age_secs": 3,
        "progress_age_secs": 7,
        "worker_id": "trj"
      }
    ],
    "slots_available": 2,
    "slots_total": 4,
    "workers_healthy": 1,
    "workers_total": 1
  }
}"#,
            ),
        ],
    )?;
    if status.success() {
        return Err("active-project exclusion should preserve non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout).map_err(|error| {
        format!("parse active-project selector report: {error}\nstderr:\n{stderr}")
    })?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "active-project exclusion should be an environment failure: {report}"
        ));
    }
    for expected in [
        "rch_verify_local_fallback_refused",
        "rch_verify_capacity_or_timeout",
        "rch_verify_remote_marker_missing",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in active-project exclusion proof: {report}"
            ));
        }
    }
    let probe = selector_probe(&report)?;
    if probe["status"] != "selection_failed"
        || probe["selection_failure_reason"] != "active_project_exclusion"
        || probe["workers_vs_selection_contradiction"] != false
        || probe["local_fallback_refused"] != true
    {
        return Err(format!(
            "selector probe did not preserve active-project exclusion: {probe}"
        ));
    }
    let blocker = probe["admission_blocker"]
        .as_object()
        .ok_or_else(|| format!("missing active admission blocker: {probe}"))?;
    if blocker.get("kind").and_then(Value::as_str) != Some("active_project_exclusion")
        || blocker.get("retry_guidance").and_then(Value::as_str)
            != Some("wait_for_active_build_or_coordinate_with_owner")
    {
        return Err(format!(
            "active admission blocker had unexpected shape: {probe}"
        ));
    }
    let evidence = blocker
        .get("evidence")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("active blocker evidence missing: {probe}"))?;
    if !evidence.contains("active_project_exclusion=1")
        || !evidence.contains("progress=stale")
        || evidence.len() > 320
    {
        return Err(format!("active blocker evidence was not bounded: {probe}"));
    }
    if blocker.get("active_build_id").and_then(Value::as_u64) != Some(29879340221071365)
        || blocker
            .get("active_project_exclusion_count")
            .and_then(Value::as_u64)
            != Some(1)
        || blocker.get("worker_id").and_then(Value::as_str) != Some("trj")
        || blocker.get("worker_posture").and_then(Value::as_str) != Some("progress_stale")
        || blocker.get("heartbeat_age_secs").and_then(Value::as_u64) != Some(3)
        || blocker.get("progress_age_secs").and_then(Value::as_u64) != Some(7)
    {
        return Err(format!(
            "active blocker did not preserve queue build details: {probe}"
        ));
    }
    let command_preview = blocker
        .get("active_command_preview")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("active command preview missing: {probe}"))?;
    let command_hash = blocker
        .get("active_command_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("active command hash missing: {probe}"))?;
    if !command_preview.contains("cargo test --test error_recall_e2e")
        || command_preview.len() > 180
        || !command_hash.starts_with("sha256:")
        || command_hash.len() != "sha256:".len() + 64
        || blocker.get("retry_after_hint").and_then(Value::as_str)
            != Some("after_active_build_completes")
        || blocker.get("next_action").and_then(Value::as_str)
            != Some("wait_for_active_build_or_contact_owner_before_retry")
        || blocker.get("owner_escalation").and_then(Value::as_str)
            != Some("identify_or_contact_active_build_owner_before_cancelling_or_retrying")
    {
        return Err(format!(
            "active blocker did not preserve operator guidance: {probe}"
        ));
    }
    let known_blocker = report["known_blocker"]
        .as_object()
        .ok_or_else(|| format!("active-project exclusion known blocker missing: {report}"))?;
    if known_blocker.get("blocker_kind").and_then(Value::as_str) != Some("active_project_exclusion")
        || known_blocker
            .get("remediation_bead")
            .and_then(Value::as_str)
            != Some("bd-1n3x1.13")
    {
        return Err(format!(
            "active-project exclusion should be a first-class known blocker: {report}"
        ));
    }
    let known_active = known_blocker
        .get("active_project_exclusion")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("known blocker missing active-project details: {report}"))?;
    if known_active
        .get("active_project_exclusion_count")
        .and_then(Value::as_u64)
        != Some(1)
        || known_active.get("active_build_id").and_then(Value::as_u64) != Some(29879340221071365)
        || known_active.get("worker_id").and_then(Value::as_str) != Some("trj")
        || known_active.get("worker_posture").and_then(Value::as_str) != Some("progress_stale")
        || known_active
            .get("progress_age_secs")
            .and_then(Value::as_u64)
            != Some(7)
        || known_active
            .get("active_command_preview")
            .and_then(Value::as_str)
            .map(|value| value.len() <= 180)
            != Some(true)
    {
        return Err(format!(
            "known blocker did not preserve bounded active-project details: {report}"
        ));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    for expected in [
        "failure_reason=`active_project_exclusion`",
        "selector_blocker: `active_project_exclusion`",
        "retry_guidance=`wait_for_active_build_or_coordinate_with_owner`",
        "active_project_exclusion_count=`1`",
        "active_build_id=`29879340221071365`",
        "worker_id=`trj`",
        "worker_posture=`progress_stale`",
        "progress_age_secs=`7`",
        "next_action=`wait_for_active_build_or_contact_owner_before_retry`",
        "remediation_bead: `bd-1n3x1.13`",
        "known_blocker_selector: `active_project_exclusion`",
    ] {
        if !summary.contains(expected) {
            return Err(format!("summary missing {expected}: {summary}"));
        }
    }
    Ok(())
}

#[test]
fn active_project_known_blocker_refusal_keeps_selector_evidence() -> TestResult {
    let store =
        unique_tmp_path("rch-active-project-known-blocker-store").join("known_blockers.jsonl");
    let store_arg = store
        .to_str()
        .ok_or_else(|| "known-blocker store path is not utf-8".to_owned())?;
    let fake_output = "[RCH] local (no admissible workers: insufficient_slots=2,active_project_exclusion=2)\n\
[RCH] remote required; refusing local fallback (no worker assigned)\n";
    let fake_queue = r#"{
  "data": {
    "active_builds": [
      {
        "id": 29882951164493986,
        "command": "cargo test --lib root_readme_license_and_notice_markdown_use_specific_doc_reasons",
        "detector_build_age_secs": 410,
        "detector_heartbeat_stale": false,
        "detector_hook_alive": true,
        "detector_progress_stale": true,
        "detector_slots_owned": 2,
        "heartbeat_age_secs": 2,
        "progress_age_secs": 391,
        "worker_id": "vmi1152480"
      }
    ],
    "slots_available": 0,
    "slots_total": 4,
    "workers_healthy": 2,
    "workers_total": 2
  }
}"#;
    let args = [
        "--known-blocker-store",
        store_arg,
        "--summary",
        "--",
        "cargo",
        "test",
        "--lib",
        "active_project_cache_smoke",
    ];
    let envs = [
        ("RCH_VERIFY_FAKE_OUTPUT", fake_output),
        ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
        ("RCH_VERIFY_FAKE_ELAPSED_MS", "17"),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1156319,vmi1152480"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1156319,vmi1152480"),
        ("RCH_VERIFY_FAKE_QUEUE_JSON", fake_queue),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&args, &envs)?;
    if first_status.success() {
        return Err("first active-project blocker should preserve non-zero exit".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first active known-blocker run: {error}"))?;
    if first["known_blocker"]["blocker_kind"] != "active_project_exclusion"
        || first["known_blocker"]["active_project_exclusion"]["active_project_exclusion_count"] != 2
        || first["known_blocker"]["active_project_exclusion"]["active_build_id"]
            != 29882951164493986u64
        || first["known_blocker"]["active_project_exclusion"]["worker_posture"] != "progress_stale"
    {
        return Err(format!(
            "first run did not record active-project known-blocker evidence: {first}"
        ));
    }

    let (second_status, second_stdout, _second_stderr) = run_script_with_env(&args, &envs)?;
    if second_status.success() {
        return Err("second active-project known blocker should refuse before RCH".to_owned());
    }
    let second: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse second active known-blocker run: {error}"))?;
    if second["status"] != "known_blocker_refused"
        || second["verification_attribution"] != "not_run_known_blocker"
        || second["known_blocker"]["blocker_kind"] != "active_project_exclusion"
        || second["known_blocker"]["remediation_bead"] != "bd-1n3x1.13"
        || second["rch_invocation"] != serde_json::json!([])
        || second["elapsed_ms"] != 0
    {
        return Err(format!(
            "second run did not fail fast with active-project blocker evidence: {second}"
        ));
    }
    let summary = second["summary_markdown"]
        .as_str()
        .ok_or_else(|| "second active known-blocker summary missing".to_owned())?;
    for expected in [
        "known_blocker_selector: `active_project_exclusion`",
        "active_project_exclusion_count=`2`",
        "active_build_id=`29882951164493986`",
        "worker_id=`vmi1152480`",
        "worker_posture=`progress_stale`",
        "next_action=`wait_for_active_build_or_contact_owner_before_retry`",
    ] {
        if !summary.contains(expected) {
            return Err(format!("second summary missing {expected}: {summary}"));
        }
    }
    Ok(())
}

#[test]
fn slot_accounting_fixture_drives_wait_for_rch_queue_pressure() -> TestResult {
    let fixture =
        read_repo_json("tests/fixtures/rch_pressure_telemetry/slot_accounting_inconsistent.json")?;
    if fixture["name"] != "slot_accounting_inconsistent" {
        return Err(format!("unexpected slot-accounting fixture: {fixture}"));
    }

    let input = fixture
        .get("input_rch_status")
        .ok_or_else(|| format!("slot-accounting fixture missing input: {fixture}"))?;
    let expected = fixture
        .get("expected")
        .ok_or_else(|| format!("slot-accounting fixture missing expected block: {fixture}"))?;
    let worker = input["workers"]
        .as_array()
        .and_then(|workers| workers.first())
        .ok_or_else(|| format!("slot-accounting fixture missing worker: {fixture}"))?;

    if input["posture"] != "remote_ready"
        || input["active_builds"].as_array().map_or(1, Vec::len) != 0
        || input["queued_builds"].as_array().map_or(1, Vec::len) != 0
        || input["daemon"]["slots_available"] != 0
        || worker["status"] != "healthy"
        || worker["used_slots"] != worker["total_slots"]
        || worker["pressure_state"] != "telemetry_gap"
        || worker["pressure_reason_code"] != "telemetry_unavailable"
        || expected["source_verdict"] != "no_rust_verdict"
    {
        return Err(format!(
            "slot-accounting fixture no longer captures the remote-ready saturated telemetry gap: {fixture}"
        ));
    }

    let inventory = ResourceQueuePressureInventory::new(vec![
        ResourceQueuePressureSourceRef::new(
            ResourceQueuePressureSourceKind::RchStatus,
            ResourceQueuePressureSourceState::Degraded,
        )
        .with_reason_code(ResourceQueuePressureReasonCode::RchTelemetryGap)
        .with_bounded_preview("remote_ready active_builds=0 queued_builds=0 telemetry_gap"),
        ResourceQueuePressureSourceRef::new(
            ResourceQueuePressureSourceKind::BuildSlotLease,
            ResourceQueuePressureSourceState::Degraded,
        )
        .with_reason_code(ResourceQueuePressureReasonCode::ActiveBuildSlotExhausted)
        .with_bounded_preview("slots_available=0 worker_used_slots=4/4"),
    ]);
    let report = inventory.report();
    let expected_reasons = vec![
        "rch_telemetry_gap".to_owned(),
        "active_build_slot_exhausted".to_owned(),
    ];
    if report.level != ResourceQueuePressureLevel::Saturated
        || report.reason_codes != expected_reasons
    {
        return Err(format!(
            "slot-accounting fixture normalized to the wrong queue pressure report: {report:?}"
        ));
    }

    let advice = evaluate_resource_queue_pressure_backoff(&ResourceQueuePressureBackoffInput {
        queue_pressure: report,
        estimated_cost_class: ResourceCostClass::SwarmHeavy,
        claim_gate_safe_to_claim: false,
    });
    if advice.decision != ResourceAdmissionDecision::WaitForRch
        || advice.can_authorize_claim
        || advice.primary_reason != "rch_telemetry_gap"
        || !advice.blocked_by.contains(&"rch_lane".to_owned())
        || !advice
            .blocked_by
            .contains(&"claim_gate_authority".to_owned())
        || advice.what_would_change != "rch_lane_has_capacity_and_fresh_telemetry"
    {
        return Err(format!(
            "remote_ready slot-accounting contradiction must wait for RCH without authorizing claims: {advice:?}"
        ));
    }

    Ok(())
}

#[test]
fn selector_admission_probe_preserves_daemon_unknown_variant_skew() -> TestResult {
    let (status, stdout, stderr) = run_script_with_env(
        &[
            "--summary",
            "--no-write",
            "--",
            "cargo",
            "test",
            "--test",
            "contracts",
            "dueling_wizards_verify_wiring",
            "--",
            "--nocapture",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "selection request failed: Failed to parse daemon response: unknown variant `no_workers_passed_health`, expected one of `success`, `no_workers_configured`, `all_workers_unreachable`, `all_circuits_open`, `all_workers_busy`, `all_workers_failed_preflight`, `all_workers_failed_convergence`, `no_matching_workers`, `no_workers_with_runtime`, `selection_error`, `affinity_pinned`, `affinity_fallback` at line 1 column 50\n[RCH] remote required; refusing local fallback (no worker assigned)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "15"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1149989"),
            ("RCH_VERIFY_DAEMON_WORKERS", "vmi1149989"),
        ],
    )?;
    if status.success() {
        return Err(
            "daemon unknown-variant selector failure should preserve non-zero exit".to_owned(),
        );
    }
    let report: Value = serde_json::from_str(&stdout).map_err(|error| {
        format!("parse unknown-variant selector report: {error}\nstderr:\n{stderr}")
    })?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "unknown-variant refusal should be an environment failure: {report}"
        ));
    }
    for expected in [
        "rch_verify_client_daemon_version_skew",
        "rch_verify_worker_health_threshold_blocked",
        "rch_verify_local_fallback_refused",
        "rch_verify_remote_marker_missing",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in unknown-variant proof: {report}"
            ));
        }
        if !worker_degraded_contains(&report, expected)? {
            return Err(format!(
                "missing {expected} in worker-state degradation: {report}"
            ));
        }
    }
    let probe = selector_probe(&report)?;
    if probe["status"] != "selection_failed"
        || probe["selection_failure_reason"] != "no_workers_passed_health"
        || probe["workers_vs_selection_contradiction"] != true
        || probe["local_fallback_refused"] != true
    {
        return Err(format!(
            "selector probe did not preserve daemon unknown-variant reason: {probe}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_dependency_planner_ignores_requested_worker_reports_filter_ignored() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &[
            "--bead-id",
            "bd-3bhcb",
            "--summary",
            "--no-write",
            "--",
            "cargo",
            "test",
            "--lib",
            "semantic_readiness",
            "--",
            "--nocapture",
        ],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "  2026-05-24T03:50:43.878558Z  WARN rch::hook: Dependency planner fail-open on vmi1227854 [RCH-E327]: refusing remote Cargo execution and falling back local (Path dependency topology policy failed.)\n[RCH] local (dependency preflight RCH-E327: Path dependency topology policy failed.)\n[RCH] remote required; refusing local fallback (dependency preflight failed)",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "31080"),
            ("RCH_WORKERS", "vmi1264463"),
            ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854,vmi1264463"),
            ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854,vmi1264463"),
        ],
    )?;
    if status.success() {
        return Err("planner worker pin mismatch should preserve non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse planner mismatch report: {error}"))?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!("expected RCH environment failure: {report}"));
    }
    if !report["worker_id"].is_null() {
        return Err(format!(
            "dependency planner worker should not masquerade as remote worker id: {report}"
        ));
    }
    for expected in [
        "rch_verify_topology_blocked",
        "rch_verify_local_fallback_refused",
        "rch_verify_remote_marker_missing",
        "rch_verify_worker_filter_ignored",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    if !worker_degraded_contains(&report, "rch_verify_worker_filter_ignored")? {
        return Err(format!(
            "planner mismatch should be listed as worker-state degradation: {report}"
        ));
    }
    if report["requested_workers"] != serde_json::json!(["vmi1264463"])
        || report["configured_workers"] != serde_json::json!(["vmi1227854", "vmi1264463"])
    {
        return Err(format!("worker inventory arrays missing: {report}"));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "summary missing".to_owned())?;
    for expected in [
        "worker_state_degraded_codes:",
        "rch_verify_worker_filter_ignored",
        "rch_verify_topology_blocked",
        "rch_verify_local_fallback_refused",
        "rch_verify_remote_marker_missing",
    ] {
        if !summary.contains(expected) {
            return Err(format!("summary missing {expected}: {summary}"));
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
fn synthetic_transport_failure_does_not_promote_warning_span_to_first_error() -> TestResult {
    let (status, stdout, _stderr) = run_script_with_env(
        &["--", "cargo", "check", "--all-targets"],
        &[
            (
                "RCH_VERIFY_FAKE_OUTPUT",
                "warning: method `mesh_two_tier_budget` is never used\n  --> tests/../src/mesh/anti_entropy_protocol.rs:130:8\n[RCH] remote vmi1227854 failed [RCH-E104] SSH command timed out after 300s\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "1"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "321731"),
        ],
    )?;
    if status.success() {
        return Err("RCH transport failure should preserve non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse transport failure: {error}"))?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "RCH transport timeout should be environment-blocked: {report}"
        ));
    }
    if report["first_error_file"] != Value::Null || report["first_error_line"] != Value::Null {
        return Err(format!(
            "warning span should not be reported as first_error: {report}"
        ));
    }
    if !report["error_codes"]
        .as_array()
        .ok_or_else(|| "missing error_codes".to_owned())?
        .iter()
        .any(|code| code.as_str() == Some("RCH-E104"))
    {
        return Err(format!("missing RCH-E104 error code: {report}"));
    }
    if !degraded_contains(&report, "rch_verify_remote_transport_timeout")? {
        return Err(format!(
            "transport timeout missing environment degraded code: {report}"
        ));
    }
    if !worker_degraded_contains(&report, "rch_verify_remote_transport_timeout")? {
        return Err(format!(
            "transport timeout should be worker-state evidence: {report}"
        ));
    }
    if report["known_blocker"]["remediation_bead"] != "bd-37ugy" {
        return Err(format!(
            "transport timeout should point at the RCH blocker bead: {report}"
        ));
    }
    if report["summary_markdown"]
        .as_str()
        .unwrap_or_default()
        .contains("first_error:")
    {
        return Err(format!(
            "summary should omit warning-only first_error: {report}"
        ));
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
    let details = &report["cargo_workspace_inheritance"];
    if details["dependency"] != "frankensearch"
        || details["manifest_path"] != "/data/projects/frankensearch/frankensearch/Cargo.toml"
        || details["inherited_field"] != "license-file"
        || details["workspace_field"] != "workspace.package.license-file"
        || details["missing_workspace_field"] != "workspace.package.license-file"
    {
        return Err(format!(
            "workspace inheritance details should route the topology fix: {report}"
        ));
    }
    Ok(())
}

#[test]
fn known_blocker_cache_refuses_second_matching_environment_failure_before_rch() -> TestResult {
    let invocation_log = unique_tmp_path("rch-known-blocker-invocations");
    let store = unique_tmp_path("rch-known-blocker-store").join("known_blockers.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-known-blocker.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf 'rch 1.0.24\n'
  exit 0
fi
if [ "${1:-}" = "status" ]; then
  cat <<'JSON'
{"data":{"daemon":{"version":"1.0.24","socket_path":"/tmp/rch.sock","workers":[],"recent_builds":[]}}}
JSON
  exit 0
fi
if [ "${1:-}" = "exec" ]; then
  printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
  cat <<'TRANSCRIPT'
error: failed to load manifest for dependency `frankensearch`

Caused by:
  failed to parse manifest at `/data/projects/frankensearch/frankensearch/Cargo.toml`

Caused by:
  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`

Caused by:
  `workspace.package.license-file` was not defined
[RCH] remote vmi1227854 failed (exit 101)
TRANSCRIPT
  exit 101
fi
printf 'unexpected fake rch args: %s\n' "$*" >&2
exit 2
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--summary",
        "--",
        "cargo",
        "test",
        "--test",
        "rch_verify_contract",
        "strict_clean_tree",
        "--",
        "--nocapture",
    ];
    let envs = [
        ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
    ];

    let (first_status, first_stdout, first_stderr) = run_script_with_env(&args, &envs)?;
    if first_status.success() {
        return Err("first topology failure should preserve non-zero exit".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first known-blocker run: {error}"))?;
    if first["status"] != "rch_environment_failure"
        || first["known_blocker"]["blocker_kind"] != "cargo_workspace_inheritance"
        || first["known_blocker"]["remediation_bead"] != "bd-17c65.10.17.1.3"
    {
        return Err(format!(
            "first run did not record a workspace-inheritance known blocker:\nstdout={first_stdout}\nstderr={first_stderr}"
        ));
    }
    let first_fingerprint = first["known_blocker"]["blocker_fingerprint"]
        .as_str()
        .ok_or_else(|| format!("first known blocker missing fingerprint: {first}"))?
        .to_owned();
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read first known-blocker invocations: {error}"))?;
    if invocations.lines().count() != 1 {
        return Err(format!(
            "first run should invoke fake RCH once: {invocations:?}"
        ));
    }
    let store_text =
        fs::read_to_string(&store).map_err(|error| format!("read known-blocker store: {error}"))?;
    if store_text.lines().count() != 1 || !store_text.contains(&first_fingerprint) {
        return Err(format!(
            "known-blocker store should contain one active fingerprint: {store_text}"
        ));
    }

    let (second_status, second_stdout, _second_stderr) = run_script_with_env(&args, &envs)?;
    if second_status.success() {
        return Err("second matching known blocker should refuse before RCH".to_owned());
    }
    let second: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse second known-blocker run: {error}"))?;
    if second["status"] != "known_blocker_refused"
        || second["verification_attribution"] != "not_run_known_blocker"
        || second["known_blocker"]["blocker_fingerprint"] != first_fingerprint
        || second["known_blocker"]["override_used"] != false
        || second["rch_invocation"] != serde_json::json!([])
        || second["elapsed_ms"] != 0
    {
        return Err(format!("second run did not fail fast correctly: {second}"));
    }
    if !degraded_contains(&second, "rch_verify_known_blocker_active")?
        || !worker_degraded_contains(&second, "rch_verify_known_blocker_active")?
    {
        return Err(format!(
            "known-blocker refusal missing degraded evidence: {second}"
        ));
    }
    let second_probe = selector_probe(&second)?;
    if second_probe["status"] != "not_applicable"
        || !second_probe["selected_worker"].is_null()
        || !second_probe["selection_failure_reason"].is_null()
        || second_probe["workers_vs_selection_contradiction"] != false
    {
        return Err(format!(
            "known-blocker refusal should not report a selector contradiction: {second_probe}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read second known-blocker invocations: {error}"))?;
    if invocations.lines().count() != 1 {
        return Err(format!(
            "second run should not invoke fake RCH again: {invocations:?}"
        ));
    }
    let summary = second["summary_markdown"]
        .as_str()
        .ok_or_else(|| "known-blocker summary missing".to_owned())?;
    if !summary.contains("known_blocker: `")
        || !summary.contains("remediation_bead: `bd-17c65.10.17.1.3`")
        || !summary.contains("known_blocker_override_used: `false`")
    {
        return Err(format!("summary missing known-blocker fields: {summary}"));
    }
    Ok(())
}

#[test]
fn known_blocker_remote_timeout_change_allows_new_remote_attempt() -> TestResult {
    let invocation_log = unique_tmp_path("rch-known-blocker-timeout-invocations");
    let store = unique_tmp_path("rch-known-blocker-timeout-store").join("known_blockers.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-known-blocker-timeout.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf 'rch 1.0.24\n'
  exit 0
fi
if [ "${1:-}" = "status" ]; then
  cat <<'JSON'
{"data":{"daemon":{"version":"1.0.24","socket_path":"/tmp/rch.sock","workers":[],"recent_builds":[]}}}
JSON
  exit 0
fi
if [ "${1:-}" = "exec" ]; then
  printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
  cat <<'TRANSCRIPT'
error: failed to load manifest for dependency `frankensearch`

Caused by:
  failed to parse manifest at `/data/projects/frankensearch/frankensearch/Cargo.toml`

Caused by:
  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`

Caused by:
  `workspace.package.license-file` was not defined
[RCH] remote vmi1227854 failed (exit 101)
TRANSCRIPT
  exit 101
fi
printf 'unexpected fake rch args: %s\n' "$*" >&2
exit 2
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--",
        "cargo",
        "check",
        "--all-targets",
    ];
    let first_envs = [
        ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
        ("RCH_BUILD_TIMEOUT_SEC", "300"),
    ];
    let second_envs = [
        ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
        ("RCH_BUILD_TIMEOUT_SEC", "900"),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&args, &first_envs)?;
    if first_status.success() {
        return Err("first timeout-key fixture should preserve remote failure".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first timeout-key run: {error}"))?;
    let first_fingerprint = first["known_blocker"]["blocker_fingerprint"]
        .as_str()
        .ok_or_else(|| format!("first timeout-key run missing known blocker: {first}"))?
        .to_owned();
    if first["known_blocker"]["remote_timeout_fingerprint"] != "build:300,test:unset" {
        return Err(format!(
            "first blocker should record the short timeout fingerprint: {first}"
        ));
    }

    let (second_status, second_stdout, _second_stderr) = run_script_with_env(&args, &second_envs)?;
    if second_status.success() {
        return Err("second timeout-key fixture should still preserve remote failure".to_owned());
    }
    let second: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse second timeout-key run: {error}"))?;
    if second["status"] == "known_blocker_refused" {
        return Err(format!(
            "changed timeout must not reuse the short-timeout known blocker: {second}"
        ));
    }
    let second_fingerprint = second["known_blocker"]["blocker_fingerprint"]
        .as_str()
        .ok_or_else(|| format!("second timeout-key run missing known blocker: {second}"))?;
    if second_fingerprint == first_fingerprint {
        return Err(format!(
            "timeout-specific blocker fingerprints should differ: {second}"
        ));
    }
    if second["known_blocker"]["remote_timeout_fingerprint"] != "build:900,test:unset" {
        return Err(format!(
            "second blocker should record the long timeout fingerprint: {second}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read timeout-key invocations: {error}"))?;
    if invocations.lines().count() != 2 {
        return Err(format!(
            "timeout change should launch a second remote attempt: {invocations:?}"
        ));
    }

    let (third_status, third_stdout, _third_stderr) = run_script_with_env(&args, &second_envs)?;
    if third_status.success() {
        return Err("third timeout-key fixture should fail fast".to_owned());
    }
    let third: Value = serde_json::from_str(&third_stdout)
        .map_err(|error| format!("parse third timeout-key run: {error}"))?;
    if third["status"] != "known_blocker_refused"
        || third["known_blocker"]["blocker_fingerprint"] != second_fingerprint
    {
        return Err(format!(
            "matching long-timeout blocker should still fail fast: {third}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read post-refusal timeout-key invocations: {error}"))?;
    if invocations.lines().count() != 2 {
        return Err(format!(
            "third matching run should not invoke fake RCH again: {invocations:?}"
        ));
    }
    Ok(())
}

#[test]
fn known_blocker_source_state_change_allows_new_remote_attempt() -> TestResult {
    let workspace = seed_git_workspace("rch-known-blocker-source-state")?;
    let invocation_log = unique_tmp_path("rch-known-blocker-source-invocations");
    let store = unique_tmp_path("rch-known-blocker-source-store").join("known_blockers.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-known-blocker-source.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf 'rch 1.0.24\n'
  exit 0
fi
if [ "${1:-}" = "status" ]; then
  cat <<'JSON'
{"data":{"daemon":{"version":"1.0.24","socket_path":"/tmp/rch.sock","workers":[],"recent_builds":[]}}}
JSON
  exit 0
fi
if [ "${1:-}" = "exec" ]; then
  printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
  cat <<'TRANSCRIPT'
error: failed to load manifest for dependency `frankensearch`

Caused by:
  failed to parse manifest at `/data/projects/frankensearch/frankensearch/Cargo.toml`

Caused by:
  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`

Caused by:
  `workspace.package.license-file` was not defined
[RCH] remote vmi1227854 failed (exit 101)
TRANSCRIPT
  exit 101
fi
printf 'unexpected fake rch args: %s\n' "$*" >&2
exit 2
"#,
    )?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path is not utf-8".to_owned())?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--project-root",
        workspace_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_source_state_smoke",
    ];
    let envs = [
        ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&args, &envs)?;
    if first_status.success() {
        return Err("first source-state fixture run should fail remotely".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first source-state known-blocker run: {error}"))?;
    let first_source_state_hash = first["known_blocker"]["source_state_hash"]
        .as_str()
        .ok_or_else(|| format!("first run missing known-blocker source hash: {first}"))?
        .to_owned();

    let (second_status, second_stdout, _second_stderr) = run_script_with_env(&args, &envs)?;
    if second_status.success() {
        return Err("second matching source-state fixture should fail-fast".to_owned());
    }
    let second: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse second source-state known-blocker run: {error}"))?;
    if second["status"] != "known_blocker_refused" {
        return Err(format!(
            "unchanged source state should match the blocker: {second}"
        ));
    }

    fs::write(
        workspace.join("tracked.txt"),
        "changed source-state fixture\n",
    )
    .map_err(|error| format!("mutate source-state fixture: {error}"))?;

    let (third_status, third_stdout, _third_stderr) = run_script_with_env(&args, &envs)?;
    if third_status.success() {
        return Err("changed source-state fixture should preserve remote failure".to_owned());
    }
    let third: Value = serde_json::from_str(&third_stdout)
        .map_err(|error| format!("parse changed source-state known-blocker run: {error}"))?;
    if third["status"] == "known_blocker_refused" {
        return Err(format!(
            "changed source state should not reuse the active blocker: {third}"
        ));
    }
    if third["status"] != "rch_environment_failure"
        || third["known_blocker"]["source_state_hash"] == first_source_state_hash
    {
        return Err(format!(
            "changed source state should run RCH and record a distinct blocker: {third}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read source-state invocations: {error}"))?;
    if invocations.lines().count() != 2 {
        return Err(format!(
            "changed source state should launch fake RCH again: {invocations:?}"
        ));
    }
    Ok(())
}

#[test]
fn known_blocker_command_kind_change_allows_new_remote_attempt() -> TestResult {
    let invocation_log = unique_tmp_path("rch-known-blocker-kind-invocations");
    let store = unique_tmp_path("rch-known-blocker-kind-store").join("known_blockers.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-known-blocker-kind.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf 'rch 1.0.24\n'
  exit 0
fi
if [ "${1:-}" = "status" ]; then
  cat <<'JSON'
{"data":{"daemon":{"version":"1.0.24","socket_path":"/tmp/rch.sock","workers":[],"recent_builds":[]}}}
JSON
  exit 0
fi
if [ "${1:-}" = "exec" ]; then
  printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
  cat <<'TRANSCRIPT'
error: failed to load manifest for dependency `frankensearch`

Caused by:
  failed to parse manifest at `/data/projects/frankensearch/frankensearch/Cargo.toml`

Caused by:
  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`

Caused by:
  `workspace.package.license-file` was not defined
[RCH] remote vmi1227854 failed (exit 101)
TRANSCRIPT
  exit 101
fi
printf 'unexpected fake rch args: %s\n' "$*" >&2
exit 2
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let cargo_test_args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_command_kind_smoke",
    ];
    let cargo_check_args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--",
        "cargo",
        "check",
        "--all-targets",
    ];
    let envs = [
        ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&cargo_test_args, &envs)?;
    if first_status.success() {
        return Err("first command-kind fixture run should fail remotely".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first command-kind known-blocker run: {error}"))?;
    if first["command_kind"] != "cargo_test"
        || first["known_blocker"]["blocker_kind"] != "cargo_workspace_inheritance"
    {
        return Err(format!(
            "first run should record cargo-test blocker: {first}"
        ));
    }

    let (second_status, second_stdout, _second_stderr) =
        run_script_with_env(&cargo_check_args, &envs)?;
    if second_status.success() {
        return Err("changed command-kind fixture should preserve remote failure".to_owned());
    }
    let second: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse changed command-kind known-blocker run: {error}"))?;
    if second["status"] == "known_blocker_refused" {
        return Err(format!(
            "changed command kind should not reuse the cargo-test blocker: {second}"
        ));
    }
    if second["command_kind"] != "cargo_check"
        || second["status"] != "rch_environment_failure"
        || second["known_blocker"]["blocker_kind"] != "cargo_workspace_inheritance"
    {
        return Err(format!(
            "changed command kind should run RCH and record a blocker: {second}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read command-kind invocations: {error}"))?;
    if invocations.lines().count() != 2 {
        return Err(format!(
            "changed command kind should launch fake RCH again: {invocations:?}"
        ));
    }
    Ok(())
}

#[test]
fn known_blocker_verifier_mode_change_allows_new_remote_attempt() -> TestResult {
    let workspace = seed_git_workspace("rch-known-blocker-verifier-mode")?;
    let invocation_log = unique_tmp_path("rch-known-blocker-mode-invocations");
    let store = unique_tmp_path("rch-known-blocker-mode-store").join("known_blockers.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-known-blocker-mode.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf 'rch 1.0.24\n'
  exit 0
fi
if [ "${1:-}" = "status" ]; then
  cat <<'JSON'
{"data":{"daemon":{"version":"1.0.24","socket_path":"/tmp/rch.sock","workers":[],"recent_builds":[]}}}
JSON
  exit 0
fi
if [ "${1:-}" = "exec" ]; then
  printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
  cat <<'TRANSCRIPT'
error: failed to load manifest for dependency `frankensearch`

Caused by:
  failed to parse manifest at `/data/projects/frankensearch/frankensearch/Cargo.toml`

Caused by:
  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`

Caused by:
  `workspace.package.license-file` was not defined
[RCH] remote vmi1227854 failed (exit 101)
TRANSCRIPT
  exit 101
fi
printf 'unexpected fake rch args: %s\n' "$*" >&2
exit 2
"#,
    )?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path is not utf-8".to_owned())?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let live_args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--project-root",
        workspace_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_verifier_mode_smoke",
    ];
    let strict_args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--project-root",
        workspace_arg,
        "--require-clean-tree",
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_verifier_mode_smoke",
    ];
    let envs = [
        ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&live_args, &envs)?;
    if first_status.success() {
        return Err("first verifier-mode fixture should fail remotely".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first verifier-mode known-blocker run: {error}"))?;
    if first["verification_attribution"] != "local_checkout_observed_remote_source_unknown"
        || first["known_blocker"]["verifier_source_mode"]
            != "local_checkout_observed_remote_source_unknown"
    {
        return Err(format!(
            "first run should record a remote-source-unknown blocker: {first}"
        ));
    }

    let (second_status, second_stdout, _second_stderr) = run_script_with_env(&strict_args, &envs)?;
    if second_status.success() {
        return Err("strict verifier-mode fixture should preserve remote failure".to_owned());
    }
    let second: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse strict verifier-mode known-blocker run: {error}"))?;
    if second["status"] == "known_blocker_refused" {
        return Err(format!(
            "strict clean mode should not reuse the live-checkout blocker: {second}"
        ));
    }
    if second["verification_attribution"] != "strict_clean_tree"
        || second["known_blocker"]["verifier_source_mode"] != "strict_clean_tree"
    {
        return Err(format!(
            "strict run should record a strict-clean blocker: {second}"
        ));
    }

    let (third_status, third_stdout, _third_stderr) = run_script_with_env(&strict_args, &envs)?;
    if third_status.success() {
        return Err("second strict verifier-mode fixture should fail fast".to_owned());
    }
    let third: Value = serde_json::from_str(&third_stdout)
        .map_err(|error| format!("parse second strict verifier-mode run: {error}"))?;
    if third["status"] != "known_blocker_refused"
        || third["verification_attribution"] != "not_run_known_blocker"
        || third["known_blocker"]["verifier_source_mode"] != "strict_clean_tree"
    {
        return Err(format!("second strict run should fail fast: {third}"));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read verifier-mode invocations: {error}"))?;
    if invocations.lines().count() != 2 {
        return Err(format!(
            "only the live and first strict modes should invoke fake RCH: {invocations:?}"
        ));
    }
    Ok(())
}

#[test]
fn known_blocker_override_runs_rch_and_records_override_evidence() -> TestResult {
    let invocation_log = unique_tmp_path("rch-known-blocker-override-invocations");
    let store = unique_tmp_path("rch-known-blocker-override-store").join("known_blockers.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-known-blocker-override.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf 'rch 1.0.24\n'
  exit 0
fi
if [ "${1:-}" = "status" ]; then
  cat <<'JSON'
{"data":{"daemon":{"version":"1.0.24","socket_path":"/tmp/rch.sock","workers":[],"recent_builds":[]}}}
JSON
  exit 0
fi
if [ "${1:-}" = "exec" ]; then
  printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
  printf 'error: failed to load manifest for dependency `frankensearch`\n'
  printf 'error inheriting `license-file` from workspace root manifest'\''s `workspace.package.license-file`\n'
  printf '`workspace.package.license-file` was not defined\n'
  printf '[RCH] remote vmi1227854 failed (exit 101)\n'
  exit 101
fi
printf 'unexpected fake rch args: %s\n' "$*" >&2
exit 2
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let base_args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_override_smoke",
    ];
    let envs = [
        ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&base_args, &envs)?;
    if first_status.success() {
        return Err("first override fixture run should fail remotely".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first override fixture run: {error}"))?;
    let fingerprint = first["known_blocker"]["blocker_fingerprint"]
        .as_str()
        .ok_or_else(|| format!("first override fixture missing known blocker: {first}"))?
        .to_owned();

    let override_args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--known-blocker-override",
        "--rch-bin",
        fake_rch_arg,
        "--summary",
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_override_smoke",
    ];
    let (override_status, override_stdout, _override_stderr) =
        run_script_with_env(&override_args, &envs)?;
    if override_status.success() {
        return Err("override fixture should still preserve remote non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&override_stdout)
        .map_err(|error| format!("parse known-blocker override run: {error}"))?;
    if report["status"] != "rch_environment_failure"
        || report["known_blocker"]["blocker_fingerprint"] != fingerprint
        || report["known_blocker"]["override_used"] != true
    {
        return Err(format!(
            "override run missing known-blocker evidence: {report}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read override invocations: {error}"))?;
    if invocations.lines().count() != 2 {
        return Err(format!(
            "override should invoke fake RCH after the initial recorded failure: {invocations:?}"
        ));
    }
    let summary = report["summary_markdown"]
        .as_str()
        .ok_or_else(|| "override summary missing".to_owned())?;
    if !summary.contains("known_blocker_override_used: `true`") {
        return Err(format!("summary missing override flag: {summary}"));
    }
    Ok(())
}

#[test]
fn known_blocker_no_write_reports_but_does_not_persist() -> TestResult {
    let store = unique_tmp_path("rch-known-blocker-no-write-store").join("known_blockers.jsonl");
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let args = [
        "--skip-build-admission",
        "--no-write",
        "--known-blocker-store",
        store_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_no_write_smoke",
    ];
    let envs = [
        ("RCH_VERIFY_FAKE_OUTPUT", workspace_inheritance_transcript()),
        ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
        ("RCH_VERIFY_FAIL_FAST_VERSION_SKEW", "0"),
    ];

    let (status, stdout, _stderr) = run_script_with_env(&args, &envs)?;
    if status.success() {
        return Err("no-write known-blocker fixture should preserve remote failure".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse no-write known-blocker report: {error}"))?;
    if report["status"] != "rch_environment_failure"
        || report["known_blocker"]["blocker_kind"] != "cargo_workspace_inheritance"
        || report["known_blocker"]["write_suppressed"] != true
    {
        return Err(format!(
            "no-write report should include suppressed known-blocker evidence: {report}"
        ));
    }
    if store.exists() {
        return Err(format!(
            "no-write must not create known-blocker store: {}",
            store.display()
        ));
    }
    Ok(())
}

#[test]
fn skip_known_blocker_bypasses_active_cache_without_writing_store() -> TestResult {
    let invocation_log = unique_tmp_path("rch-known-blocker-skip-invocations");
    let store = unique_tmp_path("rch-known-blocker-skip-store").join("known_blockers.jsonl");
    let fake_rch = write_fake_rch(
        "fake-rch-known-blocker-skip.sh",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf 'rch 1.0.24\n'
  exit 0
fi
if [ "${1:-}" = "status" ]; then
  cat <<'JSON'
{"data":{"daemon":{"version":"1.0.24","socket_path":"/tmp/rch.sock","workers":[],"recent_builds":[]}}}
JSON
  exit 0
fi
if [ "${1:-}" = "exec" ]; then
  printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
  cat <<'TRANSCRIPT'
error: failed to load manifest for dependency `frankensearch`

Caused by:
  failed to parse manifest at `/data/projects/frankensearch/frankensearch/Cargo.toml`

Caused by:
  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`

Caused by:
  `workspace.package.license-file` was not defined
[RCH] remote vmi1227854 failed (exit 101)
TRANSCRIPT
  exit 101
fi
printf 'unexpected fake rch args: %s\n' "$*" >&2
exit 2
"#,
    )?;
    let fake_rch_arg = fake_rch
        .to_str()
        .ok_or_else(|| "fake rch path is not utf-8".to_owned())?;
    let invocation_log_arg = invocation_log
        .to_str()
        .ok_or_else(|| "invocation log path is not utf-8".to_owned())?;
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let base_args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--rch-bin",
        fake_rch_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_skip_smoke",
    ];
    let skip_args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--skip-known-blocker",
        "--rch-bin",
        fake_rch_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_skip_smoke",
    ];
    let envs = [
        ("FAKE_RCH_INVOCATIONS", invocation_log_arg),
        ("RCH_VERIFY_CONFIGURED_WORKERS", "vmi1227854"),
        ("RCH_VERIFY_DAEMON_WORKERS", "vmi1227854"),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&base_args, &envs)?;
    if first_status.success() {
        return Err("first skip-known-blocker fixture should preserve remote failure".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first skip-known-blocker run: {error}"))?;
    let first_fingerprint = first["known_blocker"]["blocker_fingerprint"]
        .as_str()
        .ok_or_else(|| format!("first run should record an active known blocker: {first}"))?
        .to_owned();
    let store_before =
        fs::read_to_string(&store).map_err(|error| format!("read skip store: {error}"))?;
    if store_before.lines().count() != 1 || !store_before.contains(&first_fingerprint) {
        return Err(format!(
            "first run should write one known-blocker row: {store_before}"
        ));
    }

    let (skip_status, skip_stdout, _skip_stderr) = run_script_with_env(&skip_args, &envs)?;
    if skip_status.success() {
        return Err("skip-known-blocker fixture should preserve remote failure".to_owned());
    }
    let skipped: Value = serde_json::from_str(&skip_stdout)
        .map_err(|error| format!("parse skip-known-blocker run: {error}"))?;
    if skipped["status"] == "known_blocker_refused"
        || degraded_contains(&skipped, "rch_verify_known_blocker_active")?
        || skipped["rch_invocation"] == serde_json::json!([])
    {
        return Err(format!(
            "--skip-known-blocker should bypass the cache and invoke fake RCH: {skipped}"
        ));
    }
    if skipped["status"] != "rch_environment_failure"
        || skipped["known_blocker"]["blocker_kind"] != "cargo_workspace_inheritance"
    {
        return Err(format!(
            "skip-known-blocker should still report remote topology evidence: {skipped}"
        ));
    }
    let store_after = fs::read_to_string(&store)
        .map_err(|error| format!("read skip store after run: {error}"))?;
    if store_after != store_before {
        return Err(format!(
            "--skip-known-blocker must not write the blocker store:\nbefore={store_before}\nafter={store_after}"
        ));
    }

    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read skip invocations: {error}"))?;
    if invocations.lines().count() != 2 {
        return Err(format!(
            "skip-known-blocker should launch fake RCH after the cached failure: {invocations:?}"
        ));
    }

    let (third_status, third_stdout, _third_stderr) = run_script_with_env(&base_args, &envs)?;
    if third_status.success() {
        return Err("post-skip matching known blocker should still fail fast".to_owned());
    }
    let third: Value = serde_json::from_str(&third_stdout)
        .map_err(|error| format!("parse post-skip known-blocker run: {error}"))?;
    if third["status"] != "known_blocker_refused"
        || third["known_blocker"]["blocker_fingerprint"] != first_fingerprint
    {
        return Err(format!(
            "skip run should leave the original blocker usable for later admission: {third}"
        ));
    }
    let invocations = fs::read_to_string(&invocation_log)
        .map_err(|error| format!("read post-skip invocations: {error}"))?;
    if invocations.lines().count() != 2 {
        return Err(format!(
            "post-skip fail-fast should not invoke fake RCH again: {invocations:?}"
        ));
    }
    Ok(())
}

#[test]
fn expired_known_blocker_allows_a_new_remote_attempt() -> TestResult {
    let store = unique_tmp_path("rch-known-blocker-ttl-store").join("known_blockers.jsonl");
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_ttl_smoke",
    ];
    let first_envs = [
        ("RCH_VERIFY_FAKE_OUTPUT", workspace_inheritance_transcript()),
        ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
        ("RCH_VERIFY_FAIL_FAST_VERSION_SKEW", "0"),
        ("RCH_VERIFY_KNOWN_BLOCKER_TTL_SECONDS", "60"),
        ("RCH_VERIFY_NOW", "2026-05-16T04:40:00.000000Z"),
    ];
    let second_envs = [
        ("RCH_VERIFY_FAKE_OUTPUT", workspace_inheritance_transcript()),
        ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
        ("RCH_VERIFY_FAIL_FAST_VERSION_SKEW", "0"),
        ("RCH_VERIFY_KNOWN_BLOCKER_TTL_SECONDS", "60"),
        ("RCH_VERIFY_NOW", "2026-05-16T04:42:00.000000Z"),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&args, &first_envs)?;
    if first_status.success() {
        return Err("first TTL fixture run should preserve remote failure".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first TTL known-blocker run: {error}"))?;
    if first["status"] != "rch_environment_failure" {
        return Err(format!("first TTL run should record blocker: {first}"));
    }

    let (second_status, second_stdout, _second_stderr) = run_script_with_env(&args, &second_envs)?;
    if second_status.success() {
        return Err("expired TTL fixture run should preserve remote failure".to_owned());
    }
    let second: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse second TTL known-blocker run: {error}"))?;
    if second["status"] == "known_blocker_refused" {
        return Err(format!("expired blocker should not fail fast: {second}"));
    }
    if second["status"] != "rch_environment_failure"
        || second["known_blocker"]["blocker_kind"] != "cargo_workspace_inheritance"
    {
        return Err(format!(
            "expired blocker should allow and record a new attempt: {second}"
        ));
    }
    let store_text =
        fs::read_to_string(&store).map_err(|error| format!("read TTL store: {error}"))?;
    if store_text.lines().count() != 1 {
        return Err(format!(
            "expired entries should be compacted before writing replacement: {store_text}"
        ));
    }
    Ok(())
}

#[test]
fn known_blocker_store_respects_max_entries_cap() -> TestResult {
    let store = unique_tmp_path("rch-known-blocker-cap-store").join("known_blockers.jsonl");
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let envs = [
        ("RCH_VERIFY_FAKE_OUTPUT", workspace_inheritance_transcript()),
        ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
        ("RCH_VERIFY_FAIL_FAST_VERSION_SKEW", "0"),
        ("RCH_VERIFY_KNOWN_BLOCKER_MAX_ENTRIES", "2"),
    ];

    for test_name in [
        "known_blocker_cap_first",
        "known_blocker_cap_second",
        "known_blocker_cap_third",
    ] {
        let args = [
            "--skip-build-admission",
            "--known-blocker-store",
            store_arg,
            "--",
            "cargo",
            "test",
            "--lib",
            test_name,
        ];
        let (status, stdout, _stderr) = run_script_with_env(&args, &envs)?;
        if status.success() {
            return Err(format!(
                "cap fixture {test_name} should preserve remote failure"
            ));
        }
        let report: Value = serde_json::from_str(&stdout)
            .map_err(|error| format!("parse cap fixture {test_name}: {error}"))?;
        if report["status"] != "rch_environment_failure" {
            return Err(format!("cap fixture should record blocker: {report}"));
        }
    }

    let store_text =
        fs::read_to_string(&store).map_err(|error| format!("read capped store: {error}"))?;
    let lines: Vec<&str> = store_text.lines().collect();
    if lines.len() != 2 {
        return Err(format!(
            "known-blocker store should be capped at two entries: {store_text}"
        ));
    }
    for line in lines {
        let record: Value =
            serde_json::from_str(line).map_err(|error| format!("parse capped row: {error}"))?;
        if record["schema"] != "ee.rch.known_blocker.v1"
            || record["blocker_kind"] != "cargo_workspace_inheritance"
        {
            return Err(format!("capped row should be a known blocker: {record}"));
        }
    }
    Ok(())
}

#[test]
fn known_blocker_store_is_redacted_and_excludes_unbounded_inputs() -> TestResult {
    let workspace = seed_git_workspace("rch-known-blocker-redaction")?;
    for idx in 0..24 {
        fs::write(
            workspace.join(format!("known-blocker-redaction-untracked-{idx}.txt")),
            "redacted fixture\n",
        )
        .map_err(|error| format!("write redaction dirty path fixture: {error}"))?;
    }
    let store = unique_tmp_path("rch-known-blocker-redaction-store").join("known_blockers.jsonl");
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path is not utf-8".to_owned())?;
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--project-root",
        workspace_arg,
        "--env",
        "API_TOKEN=fixture-token-value",
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_redaction_smoke",
    ];
    let redaction_transcript = r#"error: failed to load manifest for dependency `frankensearch`

Caused by:
  failed to parse manifest at `/Users/jemanuel/private/frankensearch/Cargo.toml`

Caused by:
  error inheriting `license-file` from workspace root manifest's `workspace.package.license-file`

Caused by:
  `workspace.package.license-file` was not defined
diagnostic detail: token=fixture-token-value
[RCH] remote vmi1227854 failed (exit 101)
"#;
    let envs = [
        ("RCH_VERIFY_FAKE_OUTPUT", redaction_transcript),
        ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
        ("RCH_VERIFY_FAIL_FAST_VERSION_SKEW", "0"),
    ];

    let (status, stdout, _stderr) = run_script_with_env(&args, &envs)?;
    if status.success() {
        return Err("redaction fixture should preserve remote failure".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse redaction known-blocker run: {error}"))?;
    if report["known_blocker"]["blocker_kind"] != "cargo_workspace_inheritance" {
        return Err(format!("redaction fixture should record blocker: {report}"));
    }
    let store_text =
        fs::read_to_string(&store).map_err(|error| format!("read redaction store: {error}"))?;
    if !store_text.contains("/Users/<redacted>/private/frankensearch/Cargo.toml") {
        return Err(format!(
            "known-blocker store should keep redacted manifest evidence: {store_text}"
        ));
    }
    for forbidden in [
        "/Users/jemanuel",
        "API_TOKEN",
        "fixture-token-value",
        "diagnostic detail",
        "known-blocker-redaction-untracked-",
        "dirty_paths_sample",
        "remote_env",
        "stdout_tail",
        "stderr_tail",
    ] {
        if store_text.contains(forbidden) {
            return Err(format!(
                "known-blocker store leaked forbidden fixture text {forbidden:?}: {store_text}"
            ));
        }
    }
    Ok(())
}

#[test]
fn known_blocker_refusal_json_is_deterministic_for_same_scrubbed_input() -> TestResult {
    let store =
        unique_tmp_path("rch-known-blocker-deterministic-store").join("known_blockers.jsonl");
    let store_arg = store
        .to_str()
        .ok_or_else(|| "store path is not utf-8".to_owned())?;
    let args = [
        "--skip-build-admission",
        "--known-blocker-store",
        store_arg,
        "--",
        "cargo",
        "test",
        "--lib",
        "known_blocker_deterministic_smoke",
    ];
    let envs = [
        ("RCH_VERIFY_FAKE_OUTPUT", workspace_inheritance_transcript()),
        ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
        ("RCH_VERIFY_FAIL_FAST_VERSION_SKEW", "0"),
    ];

    let (first_status, first_stdout, _first_stderr) = run_script_with_env(&args, &envs)?;
    if first_status.success() {
        return Err("first deterministic fixture should preserve remote failure".to_owned());
    }
    let first: Value = serde_json::from_str(&first_stdout)
        .map_err(|error| format!("parse first deterministic known-blocker run: {error}"))?;
    if first["status"] != "rch_environment_failure" {
        return Err(format!(
            "first deterministic run should record blocker: {first}"
        ));
    }

    let (second_status, second_stdout, _second_stderr) = run_script_with_env(&args, &envs)?;
    if second_status.success() {
        return Err("second deterministic fixture should fail-fast".to_owned());
    }
    let second: Value = serde_json::from_str(&second_stdout)
        .map_err(|error| format!("parse second deterministic known-blocker run: {error}"))?;
    if second["status"] != "known_blocker_refused" {
        return Err(format!(
            "second deterministic run should fail-fast: {second}"
        ));
    }

    let (third_status, third_stdout, _third_stderr) = run_script_with_env(&args, &envs)?;
    if third_status.success() {
        return Err("third deterministic fixture should fail-fast".to_owned());
    }
    let third: Value = serde_json::from_str(&third_stdout)
        .map_err(|error| format!("parse third deterministic known-blocker run: {error}"))?;
    if second != third {
        return Err(format!(
            "known-blocker refusal JSON should be deterministic:\n{second}\n{third}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_cargo_path_dependency_version_failure_is_worker_topology() -> TestResult {
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
                "error: failed to select a version for the requirement `franken-agent-detection = \"^0.1.3\"`\n\
candidate versions found which didn't match: 0.1.2\n\
location searched: /data/projects/franken_agent_detection\n\
required by package `eidetic-engine v0.1.0 (/data/projects/eidetic_engine_cli)`\n\
[RCH] remote vmi1149989 failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "2100"),
        ],
    )?;
    if status.success() {
        return Err("path dependency version transcript should preserve non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse path dependency version report: {error}"))?;
    if report["status"] != "rch_environment_failure" {
        return Err(format!(
            "path dependency version mismatch should route as RCH environment failure: {report}"
        ));
    }
    if report["worker_id"] != "vmi1149989" {
        return Err(format!("worker id should be preserved: {report}"));
    }
    for expected in [
        "rch_verify_remote_command_failed",
        "rch_verify_cargo_path_dependency_version_blocked",
    ] {
        if !degraded_contains(&report, expected)? {
            return Err(format!("missing {expected} in degraded codes: {report}"));
        }
    }
    if !worker_degraded_contains(&report, "rch_verify_cargo_path_dependency_version_blocked")? {
        return Err(format!(
            "path dependency version code should be worker-state degraded: {report}"
        ));
    }
    let details = &report["cargo_path_dependency_version"];
    if details["crate"] != "franken-agent-detection"
        || details["required"] != "^0.1.3"
        || details["candidate_versions"] != serde_json::json!(["0.1.2"])
        || details["location_searched"] != "/data/projects/franken_agent_detection"
    {
        return Err(format!(
            "path dependency version details were not structured: {report}"
        ));
    }
    Ok(())
}

#[test]
fn synthetic_sync_closure_root_count_is_structured() -> TestResult {
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
                "2026-05-19T02:46:29Z INFO Prepared dependency sync manifest for 1 roots\n\
[RCH] remote vmi1149989 failed (exit 101)\n",
            ),
            ("RCH_VERIFY_FAKE_EXIT_CODE", "101"),
            ("RCH_VERIFY_FAKE_ELAPSED_MS", "2100"),
        ],
    )?;
    if status.success() {
        return Err("sync-closure failure transcript should preserve non-zero exit".to_owned());
    }
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse sync-closure report: {error}"))?;
    if report["worker_id"] != "vmi1149989" {
        return Err(format!("worker id should be preserved: {report}"));
    }
    let sync_closure = &report["sync_closure"];
    if sync_closure["source"] != "rch_transcript"
        || sync_closure["last_root_count"] != 1
        || sync_closure["root_counts"][0]["root_count"] != 1
    {
        return Err(format!(
            "sync closure root count should be structured: {report}"
        ));
    }
    let line = sync_closure["root_counts"][0]["line"]
        .as_str()
        .ok_or_else(|| format!("sync closure line missing: {report}"))?;
    if !line.contains("Prepared dependency sync manifest for 1 roots") {
        return Err(format!("sync closure proof line missing: {report}"));
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
