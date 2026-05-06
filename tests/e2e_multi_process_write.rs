//! E2E coverage for multi-process durable write contention.
//!
//! NO MOCKS. Real ee binary, real workspace database, real concurrent processes.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use ee::db::DbConnection;
use serde_json::Value;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const WRITER_COUNT: usize = 2;

struct EeOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    json: Value,
}

struct RememberedMemory {
    memory_id: String,
    audit_id: String,
    workspace_id: String,
    database_path: PathBuf,
}

#[test]
fn concurrent_remember_processes_serialize_durable_writes() -> TestResult {
    let artifact_dir = unique_artifact_dir("remember-contention")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    let init = run_ee_json(&workspace, ["init"], "init")?;
    assert_success(&init, "init")?;

    let run_id = unique_run_id()?;
    let start = Arc::new(Barrier::new(WRITER_COUNT));
    let handles: Vec<_> = (0..WRITER_COUNT)
        .map(|index| {
            let content = format!(
                "v4bq durable write contention run {run_id} process {index} must be persisted"
            );
            spawn_remember_process(Arc::clone(&start), workspace.clone(), content)
        })
        .collect();

    let mut remembered = Vec::with_capacity(WRITER_COUNT);
    for (index, handle) in handles.into_iter().enumerate() {
        let output = handle
            .join()
            .map_err(|_| format!("remember subprocess thread {index} panicked"))??;
        let parsed = parse_json_output(output, &format!("remember subprocess {index}"))?;
        assert_success(&parsed, &format!("remember subprocess {index}"))?;
        assert_no_lock_contention_error(&parsed.stderr, index)?;
        remembered.push(parse_remembered_memory(parsed, index)?);
    }

    let workspace_ids: BTreeSet<_> = remembered
        .iter()
        .map(|memory| memory.workspace_id.as_str())
        .collect();
    ensure_equal(
        &workspace_ids.len(),
        &1,
        "concurrent writes should target one workspace",
    )?;
    let workspace_id = remembered
        .first()
        .ok_or_else(|| "no remember outputs collected".to_owned())?
        .workspace_id
        .clone();
    let database_path = remembered
        .first()
        .ok_or_else(|| "no remember outputs collected".to_owned())?
        .database_path
        .clone();

    let memory_ids: BTreeSet<_> = remembered
        .iter()
        .map(|memory| memory.memory_id.as_str())
        .collect();
    ensure_equal(
        &memory_ids.len(),
        &WRITER_COUNT,
        "each concurrent process should store a distinct memory",
    )?;
    let audit_ids: BTreeSet<_> = remembered
        .iter()
        .map(|memory| memory.audit_id.as_str())
        .collect();
    ensure_equal(
        &audit_ids.len(),
        &WRITER_COUNT,
        "each durable write should have a distinct audit entry",
    )?;

    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let memories = connection
        .list_memories(&workspace_id, None, false)
        .map_err(|error| error.to_string())?;
    let stored_memory_ids: BTreeSet<_> = memories
        .iter()
        .filter(|memory| memory.content.contains(&run_id))
        .map(|memory| memory.id.as_str())
        .collect();
    ensure_equal(
        &stored_memory_ids,
        &memory_ids,
        "serialized durable row set should match subprocess reports",
    )?;

    for memory in &remembered {
        let audit = connection
            .get_audit(&memory.audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("missing audit entry {}", memory.audit_id))?;
        ensure_equal(
            &audit.workspace_id.as_deref(),
            &Some(workspace_id.as_str()),
            "audit workspace",
        )?;
        ensure_equal(&audit.action.as_str(), &"memory.create", "audit action")?;
        ensure_equal(
            &audit.target_type.as_deref(),
            &Some("memory"),
            "audit target type",
        )?;
        ensure_equal(
            &audit.target_id.as_deref(),
            &Some(memory.memory_id.as_str()),
            "audit target id",
        )?;
        ensure(
            audit
                .details
                .as_deref()
                .is_some_and(|details| !details.is_empty()),
            format!("audit details should be populated for {}", memory.memory_id),
        )?;
    }

    let integrity = connection
        .check_integrity()
        .map_err(|error| error.to_string())?;
    ensure(
        integrity.passed,
        format!(
            "database integrity_check should pass after contention; issues: {:?}",
            integrity.issues
        ),
    )?;
    connection.close().map_err(|error| error.to_string())
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("e2e-multi-process-write")
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

fn spawn_remember_process(
    start: Arc<Barrier>,
    workspace: PathBuf,
    content: String,
) -> thread::JoinHandle<Result<Output, String>> {
    thread::spawn(move || {
        start.wait();
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--workspace")
            .arg(workspace)
            .arg("--json")
            .arg("remember")
            .arg(content)
            .args(["--level", "procedural", "--kind", "fact"])
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY")
            .env("NO_COLOR", "1")
            .output()
            .map_err(|error| format!("failed to run ee remember: {error}"))
    })
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

fn parse_remembered_memory(output: EeOutput, index: usize) -> Result<RememberedMemory, String> {
    let memory_id = json_string(&output.json, "/data/memory_id", index)?;
    let audit_id = json_string(&output.json, "/data/audit_id", index)?;
    let workspace_id = json_string(&output.json, "/data/workspace_id", index)?;
    let database_path = PathBuf::from(json_string(&output.json, "/data/database_path", index)?);
    Ok(RememberedMemory {
        memory_id,
        audit_id,
        workspace_id,
        database_path,
    })
}

fn json_string(json: &Value, pointer: &str, index: usize) -> Result<String, String> {
    json.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("remember subprocess {index} missing {pointer}: {json}"))
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

fn assert_no_lock_contention_error(stderr: &str, index: usize) -> TestResult {
    let lower = stderr.to_ascii_lowercase();
    ensure(
        !lower.contains("database is locked")
            && !lower.contains("sqlite_busy")
            && !lower.contains("database locked")
            && !lower.contains("panicked"),
        format!("remember subprocess {index} leaked write contention failure: {stderr:?}"),
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
