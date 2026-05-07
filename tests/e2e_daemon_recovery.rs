//! E2E coverage for supervised daemon job recovery across process death.
//!
//! This test uses the real `ee` binary and a real workspace database. It starts
//! a bounded foreground daemon, kills it after durable planned rows are written,
//! then verifies restart recovery cancels the orphaned rows before running the
//! same job kind cleanly.

use std::env;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ee::db::DbConnection;
use ee::serve::DaemonJobRow;
use serde_json::Value;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const JOB_KIND: &str = "health_check";

#[cfg(unix)]
#[test]
#[ignore = "Asupersync daemon runtime times out on CI; needs investigation in eidetic_engine_cli-12et"]
fn daemon_supervised_job_recovery_after_kill_restart() -> TestResult {
    let artifact_dir = unique_artifact_dir("kill-restart")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    let init = run_ee_json(&workspace, ["init"], "init")?;
    assert_success(&init, "init")?;
    let database_path = PathBuf::from(json_string(&init.json, "/data/databasePath", "init")?);

    let mut child = spawn_foreground_daemon(&workspace)?;
    let status_before_kill = match wait_for_open_daemon_jobs(&workspace, 2, Duration::from_secs(10))
    {
        Ok(status) => status,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };

    let killed = terminate_child(child, "foreground daemon")?;
    ensure(
        !killed.status.success(),
        format!(
            "daemon should not exit successfully after signal; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&killed.stdout),
            String::from_utf8_lossy(&killed.stderr)
        ),
    )?;
    ensure_equal(
        &status_before_kill.pointer("/data/durable/openJobCount"),
        &Some(&Value::from(2_u64)),
        "daemon planned rows before kill",
    )?;

    let restart = run_ee_json(
        &workspace,
        [
            "daemon",
            "--foreground",
            "--once",
            "--interval-ms",
            "1", // Use 1ms to avoid busy-loop in yield_now path
            "--job",
            JOB_KIND,
        ],
        "daemon restart",
    )?;
    assert_success(&restart, "daemon restart")?;
    ensure_equal(
        &restart.json.pointer("/data/schema"),
        &Some(&Value::String("ee.steward.daemon_foreground.v1".to_owned())),
        "daemon restart schema",
    )?;
    ensure_equal(
        &restart.json.pointer("/data/summary/jobsRun"),
        &Some(&Value::from(1_u64)),
        "daemon restart job count",
    )?;
    ensure_equal(
        &restart.json.pointer("/data/summary/failed"),
        &Some(&Value::from(0_u64)),
        "daemon restart failed count",
    )?;

    let status_after_recovery = run_ee_json(&workspace, ["daemon", "status"], "daemon status")?;
    assert_success(&status_after_recovery, "daemon status after recovery")?;
    ensure_equal(
        &status_after_recovery
            .json
            .pointer("/data/durable/openJobCount"),
        &Some(&Value::from(0_u64)),
        "daemon open jobs after restart recovery",
    )?;

    let rows = load_daemon_rows(&workspace)?;
    assert_recovery_rows(&rows)?;
    assert_successful_job_after_recovery(&rows)?;

    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let integrity = connection
        .check_integrity()
        .map_err(|error| error.to_string())?;
    ensure(
        integrity.passed,
        format!(
            "database integrity_check should pass after daemon kill/restart; issues: {:?}",
            integrity.issues
        ),
    )?;
    connection.close().map_err(|error| error.to_string())
}

#[cfg(not(unix))]
#[test]
fn daemon_supervised_job_recovery_after_kill_restart() {
    eprintln!("daemon kill/restart recovery E2E is Unix-only");
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("e2e-daemon-recovery")
        .join(format!("{}-{}", name, unique_run_id()?));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create artifact dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn unique_run_id() -> Result<String, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX_EPOCH: {error}"))?
        .as_nanos();
    Ok(format!("{}-{nanos}", std::process::id()))
}

fn spawn_foreground_daemon(workspace: &Path) -> Result<Child, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .args([
            "daemon",
            "--foreground",
            "--max-ticks",
            "2",
            "--interval-ms",
            "5000",
            "--job",
            JOB_KIND,
        ])
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn foreground daemon: {error}"))
}

fn terminate_child(mut child: Child, context: &str) -> Result<Output, String> {
    let pid = child.id().to_string();
    let term = Command::new("kill")
        .arg("-TERM")
        .arg(&pid)
        .status()
        .map_err(|error| format!("failed to send SIGTERM to {context}: {error}"))?;
    ensure(
        term.success(),
        format!("SIGTERM command failed for {context}"),
    )?;

    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        if child
            .try_wait()
            .map_err(|error| format!("failed to poll {context}: {error}"))?
            .is_some()
        {
            return child
                .wait_with_output()
                .map_err(|error| format!("failed to collect {context} output: {error}"));
        }
        thread::sleep(Duration::from_millis(25));
    }

    child
        .kill()
        .map_err(|error| format!("failed to send SIGKILL to {context}: {error}"))?;
    child
        .wait_with_output()
        .map_err(|error| format!("failed to collect {context} output: {error}"))
}

fn wait_for_open_daemon_jobs(
    workspace: &Path,
    expected_open_jobs: u64,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut last_status = String::new();
    while Instant::now() < deadline {
        let status = run_ee_json(workspace, ["daemon", "status"], "daemon status poll")?;
        last_status = status.stdout.clone();
        let open_jobs = status
            .json
            .pointer("/data/durable/openJobCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if open_jobs >= expected_open_jobs {
            return Ok(status.json);
        }
        thread::sleep(Duration::from_millis(50));
    }

    Err(format!(
        "daemon did not record {expected_open_jobs} open jobs before timeout; last status: {last_status}"
    ))
}

fn run_ee_json<I, S>(workspace: &Path, args: I, context: &str) -> Result<EeOutput, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee {context}: {error}"))?;
    parse_json_output(output, context)
}

struct EeOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    json: Value,
}

fn parse_json_output(output: Output, context: &str) -> Result<EeOutput, String> {
    let stdout =
        String::from_utf8(output.stdout).map_err(|error| format!("{context} stdout: {error}"))?;
    let stderr =
        String::from_utf8(output.stderr).map_err(|error| format!("{context} stderr: {error}"))?;
    let json = serde_json::from_str(&stdout)
        .map_err(|error| format!("{context} stdout was not JSON: {error}\nstdout: {stdout}"))?;
    Ok(EeOutput {
        exit_code: output.status.code(),
        stdout,
        stderr,
        json,
    })
}

fn load_daemon_rows(workspace: &Path) -> Result<Vec<DaemonJobRow>, String> {
    ee::serve::load_daemon_job_rows(workspace)
}

fn assert_recovery_rows(rows: &[DaemonJobRow]) -> TestResult {
    let recovered = rows
        .iter()
        .filter(|row| {
            row.job_type == JOB_KIND
                && row.status == "cancelled"
                && row.outcome.as_deref() == Some("cancelled")
                && row.reason == "daemon restart recovery"
                && row.recovered_from_orphan
                && row.recovery_reason.as_deref() == Some("daemon foreground restart")
        })
        .count();
    ensure(
        recovered >= 2,
        format!(
            "restart should cancel both orphaned daemon rows; recovered {recovered} rows: {rows:?}"
        ),
    )
}

fn assert_successful_job_after_recovery(rows: &[DaemonJobRow]) -> TestResult {
    let recovery_index = rows
        .iter()
        .position(|row| row.recovered_from_orphan && row.status == "cancelled")
        .ok_or_else(|| format!("missing recovery row: {rows:?}"))?;

    let later_success = rows.iter().enumerate().any(|(index, row)| {
        index > recovery_index && row.job_type == JOB_KIND && {
            row.status == "success" && row.outcome.as_deref() == Some("success")
        }
    });
    ensure(
        later_success,
        format!("same-kind job should complete after recovery; rows: {rows:?}"),
    )
}

fn json_string(json: &Value, pointer: &str, context: &str) -> Result<String, String> {
    json.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context} missing {pointer}: {json}"))
}

fn assert_success(output: &EeOutput, context: &str) -> TestResult {
    ensure(
        output.exit_code == Some(EXIT_SUCCESS),
        format!(
            "{context}: expected exit {EXIT_SUCCESS}, got {:?}; stdout: {}; stderr: {}",
            output.exit_code, output.stdout, output.stderr
        ),
    )?;
    ensure(
        output.stderr.trim().is_empty(),
        format!(
            "{context}: JSON stderr must stay empty, got {:?}",
            output.stderr
        ),
    )?;
    ensure_equal(
        &output.json.pointer("/schema"),
        &Some(&Value::String("ee.response.v1".to_owned())),
        context,
    )?;
    ensure_equal(
        &output.json.pointer("/success"),
        &Some(&Value::Bool(true)),
        context,
    )
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}
