use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct E2eWorkspace {
    path: PathBuf,
    log_path: PathBuf,
}

impl E2eWorkspace {
    fn create(test_name: &str) -> Result<Self, String> {
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock before UNIX_EPOCH: {error}"))?
            .as_nanos();
        let counter = WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join("ee-review-e2e").join(format!(
            "{test_name}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path)
            .map_err(|error| format!("failed to create workspace {}: {error}", path.display()))?;
        let log_path = path.join("subscribe_filter_modes.events.jsonl");
        Ok(Self { path, log_path })
    }

    fn as_str(&self) -> Result<&str, String> {
        self.path
            .to_str()
            .ok_or_else(|| format!("workspace path is not UTF-8: {}", self.path.display()))
    }

    fn log(&self, phase: &str, payload: Value) -> TestResult {
        log_event(&self.log_path, phase, payload)
    }
}

fn run_ee(workspace: &E2eWorkspace, phase: &str, args: &[&str]) -> Result<Output, String> {
    workspace.log(
        phase,
        json!({
            "event": "command_start",
            "argv": args,
        }),
    )?;
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    workspace.log(
        phase,
        json!({
            "event": "command_finish",
            "argv": args,
            "status": output.status.code(),
            "success": output.status.success(),
            "elapsedMs": started.elapsed().as_millis(),
            "stdoutBytes": output.stdout.len(),
            "stderrBytes": output.stderr.len(),
        }),
    )?;
    Ok(output)
}

fn log_event(path: &Path, phase: &str, payload: Value) -> TestResult {
    let entry = json!({
        "schema": "ee.test_event.v1",
        "suite": "subscribe_filter_modes_e2e",
        "phase": phase,
        "payload": payload,
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open log {}: {error}", path.display()))?;
    writeln!(file, "{entry}")
        .map_err(|error| format!("failed to write log {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn expect_success(output: &Output, label: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn expect_failure(output: &Output, label: &str) -> TestResult {
    ensure(
        !output.status.success(),
        format!("{label} unexpectedly succeeded"),
    )
}

fn stdout_json(output: &Output, label: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\n{stdout}"))
}

fn remember(workspace: &E2eWorkspace, content: &str, tags: &str) -> TestResult {
    let workspace_path = workspace.as_str()?;
    let output = run_ee(
        workspace,
        "remember",
        &[
            "--workspace",
            workspace_path,
            "remember",
            content,
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            tags,
            "--no-auto-link",
            "--no-propose-candidates",
            "--json",
        ],
    )?;
    expect_success(&output, "remember")
}

fn subscribe_poll(workspace: &E2eWorkspace, cursor: u64, filter: &str) -> Result<Value, String> {
    let cursor = cursor.to_string();
    let workspace_path = workspace.as_str()?;
    let output = run_ee(
        workspace,
        "subscribe_poll",
        &[
            "--workspace",
            workspace_path,
            "subscribe",
            "poll",
            "--cursor",
            &cursor,
            "--filter",
            filter,
            "--json",
        ],
    )?;
    expect_success(&output, "subscribe poll")?;
    stdout_json(&output, "subscribe poll")
}

fn deltas<'a>(value: &'a Value, label: &str) -> Result<&'a [Value], String> {
    value["data"]["deltas"]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{label}: data.deltas should be an array"))
}

fn assert_delta_contract(delta: &Value, workspace_id: &str) -> TestResult {
    ensure(
        delta["schema"] == json!("ee.memory.delta.v1"),
        format!("delta schema should be stable: {delta}"),
    )?;
    ensure(
        delta["kind"] == json!("created"),
        format!("remembered memories should produce created deltas: {delta}"),
    )?;
    ensure(
        delta["workspaceId"] == json!(workspace_id),
        format!("delta workspaceId should match poll workspace: {delta}"),
    )?;
    ensure(
        delta["trustClass"] == json!("human_explicit"),
        format!("remembered memories should be human_explicit: {delta}"),
    )?;
    ensure(
        json_array_contains(&delta["levels"], "procedural"),
        format!("delta should carry procedural level: {delta}"),
    )?;
    ensure(
        json_array_contains(&delta["kinds"], "rule"),
        format!("delta should carry rule kind: {delta}"),
    )?;
    ensure(
        json_array_contains(&delta["tags"], "subscribe"),
        format!("delta should carry subscribe tag: {delta}"),
    )?;
    ensure(
        json_array_contains(&delta["changedFields"], "tags"),
        format!("delta should carry tags changed field: {delta}"),
    )
}

fn json_array_contains(value: &Value, expected: &str) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == expected))
}

#[test]
fn subscribe_poll_filters_workspace_and_trust_with_real_cli() -> TestResult {
    let workspace = E2eWorkspace::create("workspace-trust")?;
    let workspace_path = workspace.as_str()?;
    workspace.log(
        "setup",
        json!({
            "event": "workspace_created",
            "workspace": workspace_path,
        }),
    )?;

    let init = run_ee(
        &workspace,
        "init",
        &["--workspace", workspace_path, "init", "--json"],
    )?;
    expect_success(&init, "init")?;

    remember(
        &workspace,
        "Subscribe review e2e workspace trust alpha.",
        "subscribe,trust",
    )?;
    remember(
        &workspace,
        "Subscribe review e2e workspace trust beta.",
        "subscribe,workspace",
    )?;

    let search = run_ee(
        &workspace,
        "search",
        &[
            "--workspace",
            workspace_path,
            "search",
            "Subscribe review e2e workspace trust alpha",
            "--json",
        ],
    )?;
    expect_success(&search, "search")?;
    let search_json = stdout_json(&search, "search")?;
    ensure(
        search_json["success"] == json!(true),
        format!("search should exercise the real retrieval path: {search_json}"),
    )?;

    let base_filter =
        "LEVEL=procedural,KIND=rule,TAG=subscribe,TRUST_CLASS=human_explicit,CHANGED_FIELDS=tags";
    let first = subscribe_poll(&workspace, 0, base_filter)?;
    let workspace_id = first["data"]["workspaceId"]
        .as_str()
        .ok_or_else(|| format!("poll workspaceId should be a string: {}", first["data"]))?
        .to_owned();
    let first_deltas = deltas(&first, "first poll")?;
    ensure(
        first["data"]["deltaCount"] == json!(2),
        format!(
            "base filter should return both remembered deltas: {}",
            first["data"]
        ),
    )?;
    for delta in first_deltas {
        assert_delta_contract(delta, &workspace_id)?;
    }
    let next_cursor = first["data"]["nextCursor"]
        .as_u64()
        .ok_or_else(|| "first poll nextCursor should be u64".to_string())?;
    workspace.log(
        "assert_base_filter",
        json!({
            "workspaceId": workspace_id,
            "deltaCount": first["data"]["deltaCount"],
            "nextCursor": next_cursor,
        }),
    )?;

    let correct_workspace_filter = format!("{base_filter},WORKSPACE_ID={workspace_id}");
    let correct_workspace = subscribe_poll(&workspace, 0, &correct_workspace_filter)?;
    ensure(
        correct_workspace["data"]["deltaCount"] == json!(2),
        format!(
            "matching WORKSPACE_ID should keep both deltas: {}",
            correct_workspace["data"]
        ),
    )?;

    let wrong_workspace = subscribe_poll(
        &workspace,
        0,
        &format!("{base_filter},WORKSPACE_ID=wsp_review_subscribe_wrong_workspace"),
    )?;
    ensure(
        wrong_workspace["data"]["deltaCount"] == json!(0),
        format!(
            "nonmatching WORKSPACE_ID should filter all deltas: {}",
            wrong_workspace["data"]
        ),
    )?;
    ensure(
        wrong_workspace["data"]["nextCursor"].as_u64() == Some(next_cursor),
        format!(
            "filtered audit windows should still advance nextCursor: {}",
            wrong_workspace["data"]
        ),
    )?;

    let no_replay = subscribe_poll(&workspace, next_cursor, base_filter)?;
    ensure(
        no_replay["data"]["deltaCount"] == json!(0),
        format!(
            "polling from nextCursor should not replay deltas: {}",
            no_replay["data"]
        ),
    )?;
    workspace.log(
        "pass",
        json!({
            "workspaceId": workspace_id,
            "nextCursor": next_cursor,
            "logPath": workspace.log_path.display().to_string(),
        }),
    )
}

#[test]
fn subscribe_poll_reports_invalid_filter_as_json_error() -> TestResult {
    let workspace = E2eWorkspace::create("invalid-filter")?;
    let workspace_path = workspace.as_str()?;
    let init = run_ee(
        &workspace,
        "init",
        &["--workspace", workspace_path, "init", "--json"],
    )?;
    expect_success(&init, "init")?;

    let output = run_ee(
        &workspace,
        "invalid_filter",
        &[
            "--workspace",
            workspace_path,
            "subscribe",
            "poll",
            "--filter",
            "TRUST_CLASS=peer_guess",
            "--json",
        ],
    )?;
    expect_failure(&output, "invalid subscribe filter")?;
    let error_json = stdout_json(&output, "invalid subscribe filter")?;
    ensure(
        error_json["schema"] == json!("ee.error.v2"),
        format!("invalid filter should use error schema v2: {error_json}"),
    )?;
    ensure(
        error_json["error"]["code"] == json!("subscribe_filter_invalid"),
        format!("invalid filter should use stable code: {error_json}"),
    )?;
    ensure(
        error_json["error"]["details"]["recovery"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        format!("invalid filter should include structured recovery: {error_json}"),
    )?;
    workspace.log(
        "pass_invalid_filter",
        json!({
            "errorCode": error_json["error"]["code"],
            "repair": error_json["error"]["repair"],
        }),
    )
}
