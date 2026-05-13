use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root().join("scripts/rch_recover_verification.sh")
}

fn target_tmp_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/rch-recover-contract"))
}

fn write_fixture(dir: &Path, name: &str, text: &str) -> Result<PathBuf, String> {
    let path = dir.join(name);
    fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

fn run_report(args: &[String]) -> Result<Value, String> {
    let output = Command::new("bash")
        .arg(script_path())
        .args(args)
        .env("RCH_RECOVERY_NOW", "2026-05-13T20:00:00.000000Z")
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run recovery helper: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "recovery helper failed with {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse recovery report JSON: {error}"))
}

fn status_fixture(active: &str, recent: &str) -> String {
    format!(
        r#"{{
  "api_version": "1.0",
  "command": "status",
  "success": true,
  "data": {{
    "daemon": {{
      "active_builds": [{active}],
      "recent_builds": [{recent}]
    }}
  }}
}}"#
    )
}

#[test]
fn script_is_syntax_valid_and_non_destructive() -> TestResult {
    let output = Command::new("bash")
        .arg("-n")
        .arg(script_path())
        .output()
        .map_err(|error| format!("bash -n failed to start: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }

    let text = fs::read_to_string(script_path())
        .map_err(|error| format!("read recovery helper: {error}"))?;
    for forbidden in [
        "rm -rf",
        "rm -f",
        "unlink ",
        "git reset",
        "git clean",
        "git checkout",
        "git stash",
    ] {
        if text.contains(forbidden) {
            return Err(format!("recovery helper must not contain `{forbidden}`"));
        }
    }
    Ok(())
}

#[test]
fn stranded_active_job_with_artifact_is_not_pass_evidence() -> TestResult {
    let dir = target_tmp_dir().join(format!("rch-recover-stranded-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let status = write_fixture(
        &dir,
        "status.json",
        &status_fixture(
            r#"{
        "id": 123,
        "project_id": "eidetic_engine_cli-aaced065",
        "worker_id": "csd",
        "command": "cargo test --test e2e_retention_contract -- --nocapture",
        "started_at": "2026-05-13T20:04:38.322886+00:00",
        "last_heartbeat_at": null,
        "heartbeat_age_secs": null,
        "last_progress_at": null,
        "progress_age_secs": null
      }"#,
            "",
        ),
    )?;
    let artifacts = write_fixture(
        &dir,
        "artifacts.txt",
        "/data/projects/eidetic_engine_cli/.rch-target-csd-job-123-abc/debug/deps/e2e_retention_contract-abc123\n",
    )?;
    let log = write_fixture(
        &dir,
        "rch.log",
        "CARGO_TARGET_DIR=/data/projects/eidetic_engine_cli/.rch-target-csd-job-123-abc cargo test\n",
    )?;

    let report = run_report(&[
        "--job-id".to_owned(),
        "123".to_owned(),
        "--status-json".to_owned(),
        status.display().to_string(),
        "--artifact-list".to_owned(),
        artifacts.display().to_string(),
        "--rch-output-log".to_owned(),
        log.display().to_string(),
        "--local-exit-code".to_owned(),
        "-1".to_owned(),
        "--expected-command".to_owned(),
        "cargo test --test e2e_retention_contract -- --nocapture".to_owned(),
        "--remote-project-root".to_owned(),
        "/data/projects/eidetic_engine_cli".to_owned(),
    ])?;

    if report["schema"] != "ee.rch.recovery_report.v1" {
        return Err("unexpected recovery report schema".to_owned());
    }
    if report["status"] != "indeterminate_recovered_artifact" {
        return Err(format!(
            "artifact-only stranded job must not be pass evidence: {report}"
        ));
    }
    if report["safe_for_closure_evidence"] != false {
        return Err("artifact-only stranded job marked safe for closure".to_owned());
    }
    if report["artifacts"]["artifact_count"] != 1 {
        return Err("artifact count not reported".to_owned());
    }
    if report["manual_inspection_commands"]
        .as_array()
        .ok_or_else(|| "manual commands missing".to_owned())?
        .is_empty()
    {
        return Err("manual inspection commands should be provided".to_owned());
    }
    Ok(())
}

#[test]
fn recent_zero_exit_job_is_explicit_pass_evidence() -> TestResult {
    let dir = target_tmp_dir().join(format!("rch-recover-pass-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let command = "cargo clippy --all-targets -- -D warnings";
    let status = write_fixture(
        &dir,
        "status.json",
        &status_fixture(
            "",
            &format!(
                r#"{{
        "id": 130,
        "project_id": "eidetic_engine_cli-aaced065",
        "worker_id": "css",
        "command": "{command}",
        "started_at": "2026-05-13T20:00:00+00:00",
        "completed_at": "2026-05-13T20:01:00+00:00",
        "exit_code": 0,
        "duration_ms": 60000,
        "location": "remote"
      }}"#
            ),
        ),
    )?;
    let report = run_report(&[
        "--job-id".to_owned(),
        "130".to_owned(),
        "--status-json".to_owned(),
        status.display().to_string(),
        "--expected-command".to_owned(),
        command.to_owned(),
    ])?;
    if report["status"] != "pass" {
        return Err(format!("zero exit job should report pass: {report}"));
    }
    if report["safe_for_closure_evidence"] != true {
        return Err("explicit pass was not marked safe for closure evidence".to_owned());
    }
    Ok(())
}

#[test]
fn command_mismatch_blocks_recovery_confidence() -> TestResult {
    let dir = target_tmp_dir().join(format!("rch-recover-mismatch-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let status = write_fixture(
        &dir,
        "status.json",
        &status_fixture(
            "",
            r#"{
        "id": 131,
        "project_id": "eidetic_engine_cli-aaced065",
        "worker_id": "trj",
        "command": "cargo test --lib recall --quiet",
        "started_at": "2026-05-13T20:00:00+00:00",
        "completed_at": "2026-05-13T20:02:00+00:00",
        "exit_code": 0,
        "duration_ms": 120000,
        "location": "remote"
      }"#,
        ),
    )?;
    let report = run_report(&[
        "--job-id".to_owned(),
        "131".to_owned(),
        "--status-json".to_owned(),
        status.display().to_string(),
        "--expected-command".to_owned(),
        "cargo test --test e2e_retention_contract -- --nocapture".to_owned(),
    ])?;
    if report["status"] != "ambiguous_command_mismatch" {
        return Err(format!("command mismatch should block recovery: {report}"));
    }
    if report["unsafe_ambiguity"] != true {
        return Err("command mismatch did not mark unsafe_ambiguity".to_owned());
    }
    if report["safe_for_closure_evidence"] != false {
        return Err("command mismatch was marked safe for closure".to_owned());
    }
    Ok(())
}
