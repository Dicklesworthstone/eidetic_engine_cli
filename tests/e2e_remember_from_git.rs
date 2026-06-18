//! bd-2vq2z.8: real-binary e2e coverage for `ee remember --from-*`.
//!
//! This pins the frictionless git-capture path end to end:
//!
//! * build a real git repository under `target/`
//! * run the real `ee` binary for `--from-commit`, `--from-diff`, and
//!   `--from-worktree`
//! * assert dry-run is the default and `--apply` persists through the normal
//!   audited remember route
//! * assert secret-like diff content is redacted before persistence
//! * inspect the real FrankenSQLite DB for path and symbol memory anchors
//! * emit structured `ee.test_event.v1` JSONL evidence for every step

#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::db::DbConnection;
use ee::models::{MemoryAnchorKind, MemoryAnchorSource};
use ee::obs::test_log::{EventKind, LogLevel, TestEvent, excerpt_stderr, hash_bytes, log_event_to};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const TEST_ID: &str = "e2e_remember_from_git";
const RAW_SECRET: &str = "sk-proj-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

fn emit_event(log_path: &Path, event: TestEvent) -> TestResult {
    if log_event_to(log_path, LogLevel::Verbose, &event) {
        Ok(())
    } else {
        Err(format!(
            "failed to write structured test event to {}",
            log_path.display()
        ))
    }
}

fn emit_note(log_path: &Path, phase: &str, details: Value) -> TestResult {
    emit_event(
        log_path,
        TestEvent::new(TEST_ID, EventKind::Note)
            .with_field("phase", phase)
            .with_field("details", details),
    )
}

fn assert_logged(log_path: &Path, label: &str, condition: bool, details: Value) -> TestResult {
    let kind = if condition {
        EventKind::AssertOk
    } else {
        EventKind::AssertFail
    };
    emit_event(
        log_path,
        TestEvent::new(TEST_ID, kind)
            .with_field("label", label)
            .with_field("details", details.clone()),
    )?;
    if condition {
        Ok(())
    } else {
        Err(format!("{label} assertion failed: {details}"))
    }
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-remember-from-git")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn run_logged(
    log_path: &Path,
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<Output, String> {
    let mut start_event = TestEvent::new(TEST_ID, EventKind::CommandStart);
    start_event.command = Some(program.to_owned());
    start_event.args = args.iter().map(|arg| (*arg).to_owned()).collect();
    if let Some(cwd) = cwd {
        start_event =
            start_event.with_field("cwdHash", hash_bytes(cwd.display().to_string().as_bytes()));
    }
    emit_event(log_path, start_event)?;

    let started = Instant::now();
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .env("NO_COLOR", "1")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run {program} {}: {error}", args.join(" ")))?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    let mut end_event = TestEvent::new(TEST_ID, EventKind::CommandEnd);
    end_event.command = Some(program.to_owned());
    end_event.args = args.iter().map(|arg| (*arg).to_owned()).collect();
    end_event.exit_code = output.status.code();
    end_event.elapsed_ms = Some(elapsed_ms);
    end_event.stdout_hash = Some(hash_bytes(&output.stdout));
    end_event.stderr_excerpt = Some(excerpt_stderr(&output.stderr, 4096));
    emit_event(log_path, end_event)?;

    Ok(output)
}

fn run_git(log_path: &Path, repo: &Path, args: &[&str]) -> TestResult {
    let output = run_logged(log_path, "git", args, Some(repo))?;
    assert_logged(
        log_path,
        "git_command_success",
        output.status.success(),
        json!({
            "args": args,
            "status": output.status.code(),
            "stderr": excerpt_stderr(&output.stderr, 4096),
        }),
    )
}

fn run_ee_logged(log_path: &Path, args: &[&str]) -> Result<Output, String> {
    run_logged(log_path, env!("CARGO_BIN_EXE_ee"), args, None)
}

fn run_ee_json(log_path: &Path, args: &[&str]) -> Result<(Output, Value), String> {
    let output = run_ee_logged(log_path, args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "ee {} stdout must be JSON: {error}; stdout={}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout)
        )
    })?;
    Ok((output, parsed))
}

fn write_file(path: &Path, content: &str) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(path, content).map_err(|error| error.to_string())
}

fn seed_git_repo(repo: &Path, log_path: &Path) -> TestResult {
    run_logged(
        log_path,
        "git",
        &["init", "--initial-branch=main"],
        Some(repo),
    )
    .and_then(|output| {
        assert_logged(
            log_path,
            "git_init_main_success",
            output.status.success(),
            json!({
                "status": output.status.code(),
                "stderr": excerpt_stderr(&output.stderr, 4096),
            }),
        )
    })?;
    run_git(
        log_path,
        repo,
        &["config", "user.email", "ee-e2e@example.test"],
    )?;
    run_git(log_path, repo, &["config", "user.name", "ee e2e"])?;

    write_file(
        &repo.join("src/lib.rs"),
        r#"pub fn capture_fixture_state() -> &'static str {
    "initial"
}
"#,
    )?;
    run_git(log_path, repo, &["add", "src/lib.rs"])?;
    run_git(
        log_path,
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "initial capture fixture",
        ],
    )?;

    write_file(
        &repo.join("src/lib.rs"),
        &format!(
            r#"pub fn capture_fixture_state() -> &'static str {{
    "fixed"
}}

pub fn repaired_capture_path() -> &'static str {{
    "anchors should include this symbol"
}}

pub const CAPTURE_TEST_OPENAI_KEY: &str = "{RAW_SECRET}";
"#
        ),
    )?;
    run_git(log_path, repo, &["add", "src/lib.rs"])?;
    run_git(
        log_path,
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "fix capture git memory redaction",
            "-m",
            "Rationale: captured diff should become a redacted durable memory candidate.",
        ],
    )?;

    emit_note(
        log_path,
        "seed_git_repo",
        json!({
            "repoHash": hash_bytes(repo.display().to_string().as_bytes()),
            "trackedFile": "src/lib.rs",
            "rawSecretHash": hash_bytes(RAW_SECRET.as_bytes()),
        }),
    )
}

fn init_ee_workspace(workspace: &Path, log_path: &Path) -> TestResult {
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;
    let (output, parsed) =
        run_ee_json(log_path, &["--workspace", workspace_arg, "--json", "init"])?;
    assert_logged(
        log_path,
        "ee_init_success",
        output.status.success() && parsed["schema"].as_str() == Some("ee.response.v2"),
        json!({
            "status": output.status.code(),
            "schema": parsed["schema"],
            "success": parsed["success"],
            "stdoutHash": hash_bytes(&output.stdout),
        }),
    )
}

fn assert_remember_envelope(log_path: &Path, parsed: &Value, label: &str) -> TestResult {
    assert_logged(
        log_path,
        label,
        parsed["schema"].as_str() == Some("ee.response.v2")
            && parsed["success"] == Value::Bool(true)
            && parsed["data"]["command"].as_str() == Some("remember"),
        json!({
            "schema": parsed["schema"],
            "success": parsed["success"],
            "command": parsed["data"]["command"],
        }),
    )
}

fn content(parsed: &Value) -> Result<&str, String> {
    parsed["data"]["content"]
        .as_str()
        .ok_or_else(|| "remember response missing data.content".to_owned())
}

fn memory_id(parsed: &Value) -> Result<&str, String> {
    parsed["data"]["memoryId"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .ok_or_else(|| "remember response missing memory id".to_owned())
}

fn assert_commit_capture_response(log_path: &Path, parsed: &Value, persisted: bool) -> TestResult {
    assert_remember_envelope(log_path, parsed, "remember_commit_envelope")?;
    let body = content(parsed)?;
    assert_logged(
        log_path,
        "remember_commit_mode_and_kind",
        parsed["data"]["kind"].as_str() == Some("failure")
            && parsed["data"]["level"].as_str() == Some("episodic")
            && body.contains("Mode: commit.")
            && body.contains("fix capture git memory redaction"),
        json!({
            "kind": parsed["data"]["kind"],
            "level": parsed["data"]["level"],
            "containsMode": body.contains("Mode: commit."),
            "containsMessage": body.contains("fix capture git memory redaction"),
        }),
    )?;
    assert_logged(
        log_path,
        "remember_commit_redacts_secret",
        body.contains("[REDACTED:")
            && !body.contains(RAW_SECRET)
            && !parsed.to_string().contains(RAW_SECRET),
        json!({
            "contentHasRedaction": body.contains("[REDACTED:"),
            "contentHasRawSecret": body.contains(RAW_SECRET),
            "rawSecretHash": hash_bytes(RAW_SECRET.as_bytes()),
        }),
    )?;
    assert_logged(
        log_path,
        "remember_commit_surfaces_anchors_and_fingerprint",
        body.contains("ee-anchor:path:src/lib.rs")
            && body.contains("ee-anchor:symbol:repaired_capture_path")
            && body.contains("Diff fingerprint: blake3:"),
        json!({
            "pathAnchor": body.contains("ee-anchor:path:src/lib.rs"),
            "symbolAnchor": body.contains("ee-anchor:symbol:repaired_capture_path"),
            "fingerprint": body.contains("Diff fingerprint: blake3:"),
        }),
    )?;
    let tags = parsed["data"]["tags"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_logged(
        log_path,
        "remember_commit_tags_and_source",
        tags.iter().any(|tag| tag.as_str() == Some("from-commit"))
            && tags.iter().any(|tag| tag.as_str() == Some("rust"))
            && parsed["data"]["source"]
                .as_str()
                .is_some_and(|source| source.starts_with("git-sha://")),
        json!({
            "tags": tags,
            "source": parsed["data"]["source"],
        }),
    )?;
    assert_logged(
        log_path,
        "remember_commit_persistence_mode",
        parsed["data"]["persisted"] == Value::Bool(persisted)
            && parsed["data"]["dry_run"] == Value::Bool(!persisted),
        json!({
            "expectedPersisted": persisted,
            "persisted": parsed["data"]["persisted"],
            "dryRun": parsed["data"]["dry_run"],
        }),
    )
}

fn assert_diff_capture_response(log_path: &Path, parsed: &Value) -> TestResult {
    assert_remember_envelope(log_path, parsed, "remember_diff_envelope")?;
    let body = content(parsed)?;
    assert_logged(
        log_path,
        "remember_diff_dry_run_and_source",
        parsed["data"]["persisted"] == Value::Bool(false)
            && parsed["data"]["dry_run"] == Value::Bool(true)
            && parsed["data"]["source"]
                .as_str()
                .is_some_and(|source| source.starts_with("git-sha://diff/HEAD~1/"))
            && body.contains("Mode: diff."),
        json!({
            "persisted": parsed["data"]["persisted"],
            "dryRun": parsed["data"]["dry_run"],
            "source": parsed["data"]["source"],
            "containsMode": body.contains("Mode: diff."),
        }),
    )
}

fn assert_worktree_capture_response(log_path: &Path, parsed: &Value) -> TestResult {
    assert_remember_envelope(log_path, parsed, "remember_worktree_envelope")?;
    let body = content(parsed)?;
    assert_logged(
        log_path,
        "remember_worktree_dry_run_and_source",
        parsed["data"]["persisted"] == Value::Bool(false)
            && parsed["data"]["dry_run"] == Value::Bool(true)
            && parsed["data"]["source"]
                .as_str()
                .is_some_and(|source| source.starts_with("git-sha://diff/working-tree/"))
            && body.contains("Git working tree diff captured")
            && body.contains("ee-anchor:symbol:working_tree_capture_probe"),
        json!({
            "persisted": parsed["data"]["persisted"],
            "dryRun": parsed["data"]["dry_run"],
            "source": parsed["data"]["source"],
            "containsWorkingTree": body.contains("Git working tree diff captured"),
            "symbolAnchor": body.contains("ee-anchor:symbol:working_tree_capture_probe"),
        }),
    )
}

fn assert_persisted_anchors(workspace: &Path, log_path: &Path, memory_id: &str) -> TestResult {
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let anchors = connection
        .list_memory_anchors(memory_id)
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    let kinds = anchors
        .iter()
        .map(|anchor| anchor.anchor_kind)
        .collect::<BTreeSet<_>>();
    let all_hashes_stable = anchors.iter().all(|anchor| {
        anchor.anchor_value_hash.starts_with("blake3:")
            && anchor.captured_span_hash.starts_with("blake3:")
            && anchor.source == MemoryAnchorSource::Remember
    });
    assert_logged(
        log_path,
        "remember_apply_persisted_path_and_symbol_anchors",
        kinds.contains(&MemoryAnchorKind::Path)
            && kinds.contains(&MemoryAnchorKind::Symbol)
            && all_hashes_stable,
        json!({
            "memoryId": memory_id,
            "anchorCount": anchors.len(),
            "kinds": anchors.iter().map(|anchor| anchor.anchor_kind.as_str()).collect::<Vec<_>>(),
            "redactedValues": anchors.iter().map(|anchor| anchor.redacted_anchor_value.as_str()).collect::<Vec<_>>(),
            "allHashesStable": all_hashes_stable,
            "databasePathHash": hash_bytes(database_path.display().to_string().as_bytes()),
        }),
    )
}

#[test]
fn remember_from_git_real_binary_captures_dry_run_apply_and_anchors() -> TestResult {
    let workspace = unique_workspace("capture")?;
    let log_path = workspace.join("remember_from_git_events.jsonl");
    emit_note(
        &log_path,
        "start",
        json!({
            "workspaceHash": hash_bytes(workspace.display().to_string().as_bytes()),
            "eeBinaryHash": hash_bytes(env!("CARGO_BIN_EXE_ee").as_bytes()),
        }),
    )?;

    seed_git_repo(&workspace, &log_path)?;
    init_ee_workspace(&workspace, &log_path)?;

    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;
    let (dry_output, dry_commit) = run_ee_json(
        &log_path,
        &[
            "--workspace",
            workspace_arg,
            "--json",
            "remember",
            "--from-commit",
            "HEAD",
        ],
    )?;
    assert_logged(
        &log_path,
        "remember_from_commit_dry_run_exit",
        dry_output.status.success(),
        json!({
            "status": dry_output.status.code(),
            "stdoutHash": hash_bytes(&dry_output.stdout),
            "stderr": excerpt_stderr(&dry_output.stderr, 4096),
        }),
    )?;
    assert_commit_capture_response(&log_path, &dry_commit, false)?;

    let (apply_output, applied_commit) = run_ee_json(
        &log_path,
        &[
            "--workspace",
            workspace_arg,
            "--json",
            "remember",
            "--from-commit",
            "HEAD",
            "--apply",
        ],
    )?;
    assert_logged(
        &log_path,
        "remember_from_commit_apply_exit",
        apply_output.status.success(),
        json!({
            "status": apply_output.status.code(),
            "stdoutHash": hash_bytes(&apply_output.stdout),
            "stderr": excerpt_stderr(&apply_output.stderr, 4096),
        }),
    )?;
    assert_commit_capture_response(&log_path, &applied_commit, true)?;
    let persisted_memory_id = memory_id(&applied_commit)?.to_owned();
    assert_persisted_anchors(&workspace, &log_path, &persisted_memory_id)?;

    let (diff_output, diff_capture) = run_ee_json(
        &log_path,
        &[
            "--workspace",
            workspace_arg,
            "--json",
            "remember",
            "--from-diff",
            "HEAD~1",
        ],
    )?;
    assert_logged(
        &log_path,
        "remember_from_diff_exit",
        diff_output.status.success(),
        json!({
            "status": diff_output.status.code(),
            "stdoutHash": hash_bytes(&diff_output.stdout),
            "stderr": excerpt_stderr(&diff_output.stderr, 4096),
        }),
    )?;
    assert_diff_capture_response(&log_path, &diff_capture)?;

    write_file(
        &workspace.join("src/lib.rs"),
        &format!(
            r#"pub fn capture_fixture_state() -> &'static str {{
    "fixed"
}}

pub fn repaired_capture_path() -> &'static str {{
    "anchors should include this symbol"
}}

pub fn working_tree_capture_probe() -> &'static str {{
    "uncommitted working-tree anchor"
}}

pub const CAPTURE_TEST_OPENAI_KEY: &str = "{RAW_SECRET}";
"#
        ),
    )?;
    emit_note(
        &log_path,
        "working_tree_modified",
        json!({
            "path": "src/lib.rs",
            "expectedSymbol": "working_tree_capture_probe",
        }),
    )?;

    let (worktree_output, worktree_capture) = run_ee_json(
        &log_path,
        &[
            "--workspace",
            workspace_arg,
            "--json",
            "remember",
            "--from-worktree",
        ],
    )?;
    assert_logged(
        &log_path,
        "remember_from_worktree_exit",
        worktree_output.status.success(),
        json!({
            "status": worktree_output.status.code(),
            "stdoutHash": hash_bytes(&worktree_output.stdout),
            "stderr": excerpt_stderr(&worktree_output.stderr, 4096),
        }),
    )?;
    assert_worktree_capture_response(&log_path, &worktree_capture)?;

    emit_note(
        &log_path,
        "complete",
        json!({
            "persistedMemoryId": persisted_memory_id,
            "logPathHash": hash_bytes(log_path.display().to_string().as_bytes()),
        }),
    )
}
