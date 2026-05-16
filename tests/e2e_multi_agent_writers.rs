//! E2E coverage for multi-agent writer contention.
//!
//! NO MOCKS. Real ee binary, real workspace database, real concurrent processes.

#[path = "support/test_tracing.rs"]
mod test_tracing;

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

use ee::db::{AdvisoryLockId, DbConnection};
use serde_json::Value;

type TestResult = Result<(), String>;

const BEAD_ID: &str = "bd-3usjw.57";
const EXIT_SUCCESS: i32 = 0;
const WRITER_COUNT: usize = 8;
const WRITES_PER_WRITER: usize = 25;
const EXPECTED_WRITE_COUNT: usize = WRITER_COUNT * WRITES_PER_WRITER;
const GOLDEN_TRACE: &str = include_str!("golden/logs/e2e_multi_agent_writers.log");

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
    content: String,
    writer_id: usize,
    sequence: usize,
}

#[test]
fn eight_agents_persist_all_remember_writes_without_leaking_locks() -> TestResult {
    let trace = test_tracing::init_test_tracing(
        BEAD_ID,
        "eight_agents_persist_all_remember_writes_without_leaking_locks",
    );
    trace.setup(
        "multi_agent_writers",
        "created isolated workspace for concurrent remember writers",
    );

    let artifact_dir = unique_artifact_dir("multi-agent-writers")?;
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
        .map(|writer_id| {
            spawn_writer_processes(
                Arc::clone(&start),
                workspace.clone(),
                run_id.clone(),
                writer_id,
            )
        })
        .collect();

    trace.exercise(
        "multi_agent_writers",
        format!("writers={WRITER_COUNT},writes_per_writer={WRITES_PER_WRITER}"),
        "spawned concurrent ee remember writer groups",
    );

    let mut remembered = Vec::with_capacity(EXPECTED_WRITE_COUNT);
    for (writer_id, handle) in handles.into_iter().enumerate() {
        let outputs = handle
            .join()
            .map_err(|_| format!("writer thread {writer_id} panicked"))??;
        ensure_equal(
            &outputs.len(),
            &WRITES_PER_WRITER,
            "each writer thread should report every sequential write",
        )?;
        for (sequence, output) in outputs.into_iter().enumerate() {
            let parsed =
                parse_json_output(output, &format!("writer {writer_id} write {sequence}"))?;
            assert_success(&parsed, &format!("writer {writer_id} write {sequence}"))?;
            assert_no_lock_contention_error(&parsed.stderr, writer_id, sequence)?;
            remembered.push(parse_remembered_memory(parsed, writer_id, sequence)?);
        }
    }

    trace.verify(
        "multi_agent_writers",
        remembered.len(),
        EXPECTED_WRITE_COUNT,
        "all concurrent remember writes returned success",
    );

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
        &EXPECTED_WRITE_COUNT,
        "each write should store a distinct memory",
    )?;
    let audit_ids: BTreeSet<_> = remembered
        .iter()
        .map(|memory| memory.audit_id.as_str())
        .collect();
    ensure_equal(
        &audit_ids.len(),
        &EXPECTED_WRITE_COUNT,
        "each write should create a distinct audit row",
    )?;
    let content_keys: BTreeSet<_> = remembered
        .iter()
        .map(|memory| (memory.writer_id, memory.sequence, memory.content.as_str()))
        .collect();
    ensure_equal(
        &content_keys.len(),
        &EXPECTED_WRITE_COUNT,
        "writer/sequence/content tuples should be unique",
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
        "stored row set should match subprocess reports",
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

    connection
        .ensure_advisory_locks_table()
        .map_err(|error| error.to_string())?;
    for writer_id in 0..WRITER_COUNT {
        let holder = writer_holder_id(writer_id);
        let locks = connection
            .list_locks_by_holder(&holder)
            .map_err(|error| error.to_string())?;
        ensure(
            locks.is_empty(),
            format!("writer holder {holder} should not leak advisory locks: {locks:?}"),
        )?;
    }
    let workspace_lock = AdvisoryLockId::new("workspace", &workspace_id);
    let active_workspace_lock = connection
        .is_lock_held(&workspace_lock)
        .map_err(|error| error.to_string())?;
    ensure(
        active_workspace_lock.is_none(),
        format!("workspace advisory lock should not remain held: {active_workspace_lock:?}"),
    )?;

    let integrity = connection
        .check_integrity()
        .map_err(|error| error.to_string())?;
    ensure(
        integrity.passed,
        format!(
            "database integrity_check should pass after multi-agent writes; issues: {:?}",
            integrity.issues
        ),
    )?;
    connection.close().map_err(|error| error.to_string())?;

    let audit_verify = run_ee_json(&workspace, ["audit", "verify"], "audit verify")?;
    ensure_equal(
        &audit_verify
            .json
            .pointer("/integrity_ok")
            .and_then(Value::as_bool),
        &Some(true),
        "audit verify integrity_ok",
    )?;
    let verified_rows = audit_verify
        .json
        .pointer("/rows")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("audit verify missing /rows: {}", audit_verify.json))?;
    ensure(
        verified_rows >= EXPECTED_WRITE_COUNT as u64,
        format!("audit verify rows {verified_rows} should cover {EXPECTED_WRITE_COUNT} writes"),
    )?;
    let audit_issues = audit_verify
        .json
        .pointer("/issues")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("audit verify missing /issues: {}", audit_verify.json))?;
    ensure(
        audit_issues.is_empty(),
        format!("audit verify reported issues: {audit_issues:?}"),
    )?;

    trace.verify(
        "multi_agent_writers",
        format!("stored={EXPECTED_WRITE_COUNT},audit_integrity_ok=true"),
        "stored=200,audit_integrity_ok=true",
        "verified stored rows, audit chain, and lock cleanup",
    );
    trace.teardown(
        "multi_agent_writers",
        "left e2e artifact workspace in target for inspection",
    );
    let normalized_trace = test_tracing::normalize_trace_jsonl(trace.path())?;
    ensure_equal(
        &normalized_trace.as_str(),
        &GOLDEN_TRACE,
        "normalized multi-agent writer trace",
    )?;
    Ok(())
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("e2e-multi-agent-writers")
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

fn writer_holder_id(writer_id: usize) -> String {
    format!("{BEAD_ID}-writer-{writer_id}")
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
        .env("EE_TEST_SEED", "42")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee {context}: {error}"))?;
    parse_json_output(output, context)
}

fn spawn_writer_processes(
    start: Arc<Barrier>,
    workspace: PathBuf,
    run_id: String,
    writer_id: usize,
) -> thread::JoinHandle<Result<Vec<Output>, String>> {
    thread::spawn(move || {
        start.wait();
        let mut outputs = Vec::with_capacity(WRITES_PER_WRITER);
        for sequence in 0..WRITES_PER_WRITER {
            let content = format!(
                "bd-3usjw.57 deterministic seed 42 run {run_id} writer {writer_id} sequence {sequence}"
            );
            let output = Command::new(env!("CARGO_BIN_EXE_ee"))
                .arg("--workspace")
                .arg(&workspace)
                .arg("--json")
                .arg("remember")
                .arg(content)
                .args(["--level", "procedural", "--kind", "fact"])
                .env_remove("EE_WORKSPACE")
                .env_remove("EE_WORKSPACE_REGISTRY")
                .env("EE_TEST_SEED", "42")
                .env("EE_AGENT_HOLDER_ID", writer_holder_id(writer_id))
                .env("NO_COLOR", "1")
                .output()
                .map_err(|error| {
                    format!("failed to run ee remember for writer {writer_id}: {error}")
                })?;
            outputs.push(output);
        }
        Ok(outputs)
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

fn parse_remembered_memory(
    output: EeOutput,
    writer_id: usize,
    sequence: usize,
) -> Result<RememberedMemory, String> {
    let memory_id = json_string(&output.json, "/data/memory_id", writer_id, sequence)?;
    let audit_id = json_string(&output.json, "/data/audit_id", writer_id, sequence)?;
    let workspace_id = json_string(&output.json, "/data/workspace_id", writer_id, sequence)?;
    let database_path = PathBuf::from(json_string(
        &output.json,
        "/data/database_path",
        writer_id,
        sequence,
    )?);
    let content = json_string(&output.json, "/data/content", writer_id, sequence)
        .or_else(|_| json_string(&output.json, "/data/memory/content", writer_id, sequence))
        .unwrap_or_else(|_| format!("writer {writer_id} sequence {sequence}"));
    Ok(RememberedMemory {
        memory_id,
        audit_id,
        workspace_id,
        database_path,
        content,
        writer_id,
        sequence,
    })
}

fn json_string(
    json: &Value,
    pointer: &str,
    writer_id: usize,
    sequence: usize,
) -> Result<String, String> {
    json.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("writer {writer_id} sequence {sequence} missing {pointer}: {json}"))
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

fn assert_no_lock_contention_error(stderr: &str, writer_id: usize, sequence: usize) -> TestResult {
    let lower = stderr.to_ascii_lowercase();
    ensure(
        !lower.contains("database is locked")
            && !lower.contains("sqlite_busy")
            && !lower.contains("database locked")
            && !lower.contains("panicked"),
        format!(
            "writer {writer_id} sequence {sequence} leaked write contention failure: {stderr:?}"
        ),
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
