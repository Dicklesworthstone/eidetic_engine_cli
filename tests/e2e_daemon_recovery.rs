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

use rustix::process::{Pid, Signal, kill_process};
use tracing::info;

use ee::db::{CreateFeedbackEventInput, DbConnection, audit_actions};
use ee::serve::DaemonJobRow;
use serde_json::Value;

#[path = "support/test_tracing.rs"]
mod test_tracing;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const JOB_KIND: &str = "health_check";

#[cfg(unix)]
#[test]
fn daemon_supervised_job_recovery_after_kill_restart() -> TestResult {
    let trace = test_tracing::init_test_tracing(
        "bd-3usjw.54",
        "daemon_supervised_job_recovery_after_kill_restart",
    );
    trace.setup("daemon_recovery", "created daemon recovery trace guard");

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
    trace.exercise("daemon_recovery", "ee init", "initialized test workspace");
    let database_path = PathBuf::from(json_string(&init.json, "/data/databasePath", "init")?);

    let mut child = spawn_foreground_daemon(&workspace)?;
    trace.exercise("daemon_recovery", child.id(), "spawned foreground daemon");
    let status_before_kill = match wait_for_open_daemon_jobs(&workspace, 2, Duration::from_secs(10))
    {
        Ok(status) => status,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    trace.verify(
        "daemon_recovery",
        status_before_kill
            .pointer("/data/durable/openJobCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        2_u64,
        "daemon wrote durable planned rows before termination",
    );

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
    trace.exercise(
        "daemon_recovery",
        "ee daemon --foreground --once",
        "restarted daemon for orphan recovery",
    );
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
    trace.verify(
        "daemon_recovery",
        status_after_recovery
            .json
            .pointer("/data/durable/openJobCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        0_u64,
        "daemon restart recovered all open durable rows",
    );
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
    trace.verify(
        "daemon_recovery",
        rows.len(),
        ">=3",
        "daemon rows include cancelled orphans plus a later successful job",
    );

    info!(
        jobs_in_flight = 2,
        recovery_outcome = "success",
        "Verified daemon recovery over process death"
    );

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

#[test]
fn maintenance_job_decay_sweep_persists_history_and_mutates_db() -> TestResult {
    let trace = test_tracing::init_test_tracing(
        "bd-3usjw.54",
        "maintenance_job_decay_sweep_persists_history_and_mutates_db",
    );
    trace.setup("maintenance_decay_sweep", "created decay sweep trace guard");

    let artifact_dir = unique_artifact_dir("maintenance-decay-sweep")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    let init = run_ee_json(&workspace, ["init"], "init")?;
    assert_success(&init, "init")?;
    trace.exercise(
        "maintenance_decay_sweep",
        "ee init",
        "initialized decay sweep workspace",
    );
    let database_path = PathBuf::from(json_string(&init.json, "/data/databasePath", "init")?);

    let remembered = run_ee_json(
        &workspace,
        [
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--confidence",
            "0.8",
            "Decay sweep e2e fixture should lose confidence after harmful feedback.",
        ],
        "remember decay fixture",
    )?;
    assert_success(&remembered, "remember decay fixture")?;
    let memory_id = json_string(&remembered.json, "/data/memory_id", "remember")?;
    let workspace_id = json_string(&remembered.json, "/data/workspace_id", "remember")?;
    trace.exercise(
        "maintenance_decay_sweep",
        &memory_id,
        "seeded memory and harmful feedback fixture",
    );

    {
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        for feedback_id in [
            "fb_e1234567890123456789012345",
            "fb_f1234567890123456789012345",
        ] {
            connection
                .insert_feedback_event(
                    feedback_id,
                    &CreateFeedbackEventInput {
                        workspace_id: workspace_id.clone(),
                        target_type: "memory".to_owned(),
                        target_id: memory_id.clone(),
                        signal: "harmful".to_owned(),
                        weight: 1.0,
                        source_type: "outcome_observed".to_owned(),
                        source_id: Some(format!("maintenance-decay-sweep-e2e-{feedback_id}")),
                        reason: Some("force real decay_sweep mutation path".to_owned()),
                        evidence_json: Some(r#"{"fixture":"maintenance_decay_sweep"}"#.to_owned()),
                        session_id: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        connection.close().map_err(|error| error.to_string())?;
    }

    let run = run_ee_json(
        &workspace,
        ["job", "run", "decay_sweep"],
        "job run decay_sweep",
    )?;
    assert_success(&run, "job run decay_sweep")?;
    trace.exercise(
        "maintenance_decay_sweep",
        "ee job run decay_sweep",
        "ran durable decay sweep job",
    );
    ensure_equal(
        &run.json.pointer("/data/schema"),
        &Some(&Value::String("ee.maintenance.run.v1".to_owned())),
        "maintenance run schema",
    )?;
    ensure_equal(
        &run.json.pointer("/data/requestedJob"),
        &Some(&Value::String("decay_sweep".to_owned())),
        "requested job",
    )?;
    ensure_equal(
        &run.json.pointer("/data/durableMutation"),
        &Some(&Value::Bool(true)),
        "durable mutation",
    )?;
    ensure_equal(
        &run.json.pointer("/data/history/persisted"),
        &Some(&Value::Bool(true)),
        "history persisted",
    )?;
    ensure_equal(
        &run.json
            .pointer("/data/results/0/details/summary/appliedCount"),
        &Some(&Value::from(1_u64)),
        "decay applied count",
    )?;

    let job_row_id = json_string(&run.json, "/data/history/rowId", "job run")?;
    let history_path = PathBuf::from(json_string(&run.json, "/data/history/path", "job run")?);
    ensure(
        history_path.exists(),
        format!(
            "maintenance history path should exist: {}",
            history_path.display()
        ),
    )?;

    let list = run_ee_json(
        &workspace,
        ["job", "list", "--kind", "decay_sweep"],
        "job list decay_sweep",
    )?;
    assert_success(&list, "job list decay_sweep")?;
    ensure_equal(
        &list.json.pointer("/data/jobCount"),
        &Some(&Value::from(1_u64)),
        "job list count",
    )?;
    ensure_equal(
        &list.json.pointer("/data/jobs/0/id"),
        &Some(&Value::String(job_row_id.clone())),
        "job list row id",
    )?;
    ensure_equal(
        &list.json.pointer("/data/jobs/0/outcome"),
        &Some(&Value::String("success".to_owned())),
        "job list outcome",
    )?;
    ensure_equal(
        &list.json.pointer("/data/jobs/0/durableMutation"),
        &Some(&Value::Bool(true)),
        "job list durable mutation",
    )?;

    let shown = run_ee_json(&workspace, ["job", "show", &job_row_id], "job show")?;
    assert_success(&shown, "job show")?;
    trace.verify(
        "maintenance_decay_sweep",
        &job_row_id,
        "persisted job row",
        "job show can reload the persisted decay history row",
    );
    ensure_equal(
        &shown.json.pointer("/data/job/id"),
        &Some(&Value::String(job_row_id)),
        "job show row id",
    )?;

    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let memory = connection
        .get_memory(&memory_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "decay fixture memory missing after job".to_owned())?;
    trace.verify(
        "maintenance_decay_sweep",
        memory.confidence,
        "<0.8",
        "decay sweep reduced confidence after harmful feedback",
    );
    ensure(
        memory.confidence < 0.8,
        format!(
            "decay_sweep should reduce confidence below 0.8, got {}",
            memory.confidence
        ),
    )?;
    let feedback = connection
        .list_feedback_events_for_target("memory", &memory_id)
        .map_err(|error| error.to_string())?;
    ensure(
        feedback.iter().all(|event| event.applied_at.is_some()),
        format!("decay_sweep should mark feedback applied: {feedback:?}"),
    )?;
    let audit = connection
        .list_audit_by_target("memory", &memory_id, None)
        .map_err(|error| error.to_string())?;
    ensure(
        audit
            .iter()
            .any(|entry| entry.action == audit_actions::MEMORY_SCORE_DECAY),
        format!("decay_sweep should write memory score decay audit row: {audit:?}"),
    )?;
    connection.close().map_err(|error| error.to_string())
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let mut target_roots = Vec::new();
    for candidate in [
        env::var_os("CARGO_TARGET_TMPDIR"),
        env::var_os("CARGO_TARGET_DIR"),
        Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .into_os_string(),
        ),
    ]
    .into_iter()
    .flatten()
    .map(PathBuf::from)
    {
        if !target_roots.iter().any(|root| root == &candidate) {
            target_roots.push(candidate);
        }
    }

    let artifact_suffix = PathBuf::from("ee-test-artifacts")
        .join("e2e-daemon-recovery")
        .join(format!("{}-{}", name, unique_run_id()?));

    let mut failures = Vec::new();
    for target_dir in target_roots {
        let dir = target_dir.join(&artifact_suffix);
        match fs::create_dir_all(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) => {
                failures.push(format!("{}: {error}", dir.display()));
            }
        }
    }

    Err(format!(
        "failed to create artifact dir in any target root: {}",
        failures.join("; ")
    ))
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
    let raw_pid = child.id() as i32;
    let pid = Pid::from_raw(raw_pid).ok_or_else(|| format!("Invalid PID {raw_pid}"))?;

    info!(daemon_pid = raw_pid, "Sending SIGTERM to {}", context);
    let term_start = Instant::now();

    info!(
        daemon_pid = raw_pid,
        signal_sent = "SIGTERM",
        "Sending daemon termination signal"
    );
    kill_process(pid, Signal::Term)
        .map_err(|error| format!("failed to send SIGTERM to {context}: {error}"))?;

    info!(
        daemon_pid = raw_pid,
        "SIGTERM sent successfully to {}", context
    );

    let deadline = Instant::now() + Duration::from_millis(1500); // Increased timeout to 1.5s
    while Instant::now() < deadline {
        if child
            .try_wait()
            .map_err(|error| format!("failed to poll {context}: {error}"))?
            .is_some()
        {
            let output = child
                .wait_with_output()
                .map_err(|error| format!("failed to collect {context} output: {error}"));

            info!(
                daemon_pid = raw_pid,
                signal_received_ms = term_start.elapsed().as_millis(),
                "Daemon process exited after SIGTERM"
            );
            return output;
        }
        thread::sleep(Duration::from_millis(25));
    }

    info!(
        daemon_pid = raw_pid,
        "Daemon did not exit cleanly, sending SIGKILL"
    );
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
