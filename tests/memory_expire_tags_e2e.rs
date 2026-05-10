//! Real-binary E2E coverage for memory expiration and tag mutation surfaces.
//!
//! This test runs the public `ee` binary against a temporary workspace, logs
//! every command as JSONL, and compares scrubbed response contracts to golden
//! snapshots.

use serde_json::{Value as JsonValue, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

const BODY_MARKER: &str = "E2E-MEMORY-BODY-MARKER";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_run_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-memory-expire-tags-e2e")
        .join(format!("{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

fn ee_binary_path() -> Result<PathBuf, String> {
    let cargo_path = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
    if cargo_path.exists() {
        return Ok(cargo_path);
    }

    let current_exe = std::env::current_exe()
        .map_err(|error| format!("failed to resolve current test binary: {error}"))?;
    let debug_dir = current_exe.parent().and_then(Path::parent).ok_or_else(|| {
        format!(
            "failed to resolve debug directory from test binary {}",
            current_exe.display()
        )
    })?;
    let sibling = debug_dir.join("ee");
    if sibling.exists() {
        Ok(sibling)
    } else {
        Err(format!(
            "ee binary not found at {} or {}",
            cargo_path.display(),
            sibling.display()
        ))
    }
}

fn run_ee(workspace: &Path, args: &[String]) -> Result<Output, String> {
    Command::new(ee_binary_path()?)
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {:?}: {error}", args))
}

fn parse_stdout_json(output: &Output, context: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context} stdout was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn append_event(
    log_path: &Path,
    step: &str,
    args: &[String],
    output: &Output,
) -> Result<(), String> {
    let event = json!({
        "schema": "ee.memory_expire_tags_e2e_event.v1",
        "step": step,
        "args": args,
        "exitCode": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| format!("failed to open {}: {error}", log_path.display()))?;
    serde_json::to_writer(&mut file, &event)
        .map_err(|error| format!("failed to write event JSON: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("failed to write event newline: {error}"))
}

fn run_step(
    workspace: &Path,
    log_path: &Path,
    step: &str,
    args: Vec<String>,
) -> Result<Output, String> {
    let output = run_ee(workspace, &args)?;
    append_event(log_path, step, &args, &output)?;
    ensure(
        output.status.success(),
        format!(
            "{step} failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(output)
}

fn workspace_args(workspace: &Path) -> Vec<String> {
    vec![
        "--workspace".to_owned(),
        workspace.to_string_lossy().into_owned(),
        "--json".to_owned(),
    ]
}

fn normalize_memory_response(mut value: JsonValue) -> JsonValue {
    if let Some(data) = value.get_mut("data").and_then(JsonValue::as_object_mut) {
        data.insert(
            "memory_id".to_owned(),
            JsonValue::String("<MEMORY_ID>".to_owned()),
        );
        data.insert(
            "workspace_id".to_owned(),
            JsonValue::String("<WORKSPACE_ID>".to_owned()),
        );

        if data.get("audit_id").is_some_and(JsonValue::is_string) {
            data.insert(
                "audit_id".to_owned(),
                JsonValue::String("<AUDIT_ID>".to_owned()),
            );
        }
        if let Some(audit_ids) = data.get_mut("audit_ids").and_then(JsonValue::as_array_mut) {
            for audit_id in audit_ids {
                *audit_id = JsonValue::String("<AUDIT_ID>".to_owned());
            }
        }
        if data.get("index_job_id").is_some_and(JsonValue::is_string) {
            data.insert(
                "index_job_id".to_owned(),
                JsonValue::String("<INDEX_JOB_ID>".to_owned()),
            );
        }
        if data
            .get("previous_tombstoned_at")
            .is_some_and(JsonValue::is_string)
        {
            data.insert(
                "previous_tombstoned_at".to_owned(),
                JsonValue::String("<TIMESTAMP>".to_owned()),
            );
        }
        if data.get("tombstoned_at").is_some_and(JsonValue::is_string) {
            data.insert(
                "tombstoned_at".to_owned(),
                JsonValue::String("<TIMESTAMP>".to_owned()),
            );
        }
    }

    value
}

fn assert_golden(actual: JsonValue, expected: &str, label: &str) -> TestResult {
    let actual = serde_json::to_string_pretty(&actual)
        .map_err(|error| format!("failed to serialize normalized {label}: {error}"))?
        + "\n";
    ensure(
        actual == expected,
        format!("{label} golden mismatch\nexpected:\n{expected}\nactual:\n{actual}"),
    )
}

fn ensure_json_stdout_isolated(output: &Output, context: &str) -> TestResult {
    ensure(
        output.stderr.is_empty(),
        format!(
            "{context} wrote diagnostics to stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn ensure_body_marker_absent(output: &Output, context: &str) -> TestResult {
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        !stdout.contains(BODY_MARKER),
        format!("{context} leaked raw memory body marker in stdout"),
    )
}

#[test]
fn memory_expire_and_tags_are_audited_idempotent_and_logged() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let events_path = run_dir.join("events.jsonl");

    let mut init_args = workspace_args(&workspace);
    init_args.push("init".to_owned());
    run_step(&workspace, &events_path, "init", init_args)?;

    let mut remember_args = workspace_args(&workspace);
    remember_args.extend([
        "remember".to_owned(),
        "--level".to_owned(),
        "procedural".to_owned(),
        "--kind".to_owned(),
        "rule".to_owned(),
        "--tags".to_owned(),
        "release,checks".to_owned(),
        format!("Run cargo fmt --check before release. {BODY_MARKER}"),
    ]);
    let remember_output = run_step(&workspace, &events_path, "remember", remember_args)?;
    let remember_json = parse_stdout_json(&remember_output, "remember")?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember response missing memory_id".to_owned())?
        .to_owned();

    let mut tag_list_args = workspace_args(&workspace);
    tag_list_args.extend(["memory".to_owned(), "tags".to_owned(), memory_id.clone()]);
    let tag_list_output = run_step(&workspace, &events_path, "tags_list", tag_list_args)?;
    ensure_json_stdout_isolated(&tag_list_output, "tags list")?;
    ensure_body_marker_absent(&tag_list_output, "tags list")?;
    let tag_list_json = parse_stdout_json(&tag_list_output, "tags list")?;
    ensure(
        tag_list_json["data"]["status"] == "listed",
        "tag list reports listed",
    )?;
    ensure(
        tag_list_json["data"]["tags"] == json!(["checks", "release"]),
        "tag list returns sorted initial tags",
    )?;

    let mut tag_dry_run_args = workspace_args(&workspace);
    tag_dry_run_args.extend([
        "memory".to_owned(),
        "tags".to_owned(),
        memory_id.clone(),
        "--add".to_owned(),
        "zeta,alpha".to_owned(),
        "--remove".to_owned(),
        "checks".to_owned(),
        "--dry-run".to_owned(),
    ]);
    let tag_dry_run_output = run_step(
        &workspace,
        &events_path,
        "tags_patch_dry_run",
        tag_dry_run_args,
    )?;
    ensure_json_stdout_isolated(&tag_dry_run_output, "tags dry run")?;
    ensure_body_marker_absent(&tag_dry_run_output, "tags dry run")?;
    let tag_dry_run_json = parse_stdout_json(&tag_dry_run_output, "tags dry run")?;
    ensure(
        tag_dry_run_json["data"]["status"] == "would_update",
        "tag dry-run reports planned mutation",
    )?;
    ensure(
        tag_dry_run_json["data"]["tags"] == json!(["alpha", "release", "zeta"]),
        "tag dry-run previews sorted final tags",
    )?;

    let mut tag_apply_args = workspace_args(&workspace);
    tag_apply_args.extend([
        "memory".to_owned(),
        "tags".to_owned(),
        memory_id.clone(),
        "--add".to_owned(),
        "zeta,alpha".to_owned(),
        "--remove".to_owned(),
        "checks".to_owned(),
        "--actor".to_owned(),
        "memory-expire-tags-e2e".to_owned(),
    ]);
    let tag_apply_output = run_step(
        &workspace,
        &events_path,
        "tags_patch_apply",
        tag_apply_args.clone(),
    )?;
    ensure_json_stdout_isolated(&tag_apply_output, "tags apply")?;
    ensure_body_marker_absent(&tag_apply_output, "tags apply")?;
    let tag_apply_json = parse_stdout_json(&tag_apply_output, "tags apply")?;
    ensure(
        tag_apply_json["data"]["status"] == "updated",
        "tag apply reports updated",
    )?;
    ensure(
        tag_apply_json["data"]["audit_ids"]
            .as_array()
            .is_some_and(|ids| ids.len() == 1),
        "tag apply records one audit id",
    )?;
    ensure(
        tag_apply_json["data"]["index_job_id"].is_string(),
        "tag apply queues an index job",
    )?;

    let tag_duplicate_output = run_step(
        &workspace,
        &events_path,
        "tags_patch_duplicate",
        tag_apply_args,
    )?;
    ensure_json_stdout_isolated(&tag_duplicate_output, "tags duplicate")?;
    ensure_body_marker_absent(&tag_duplicate_output, "tags duplicate")?;
    let tag_duplicate_json = parse_stdout_json(&tag_duplicate_output, "tags duplicate")?;
    ensure(
        tag_duplicate_json["data"]["status"] == "unchanged",
        "duplicate tag patch is idempotent",
    )?;

    assert_golden(
        json!({
            "tagList": normalize_memory_response(tag_list_json),
            "tagDryRun": normalize_memory_response(tag_dry_run_json),
            "tagApply": normalize_memory_response(tag_apply_json),
            "tagDuplicate": normalize_memory_response(tag_duplicate_json),
        }),
        include_str!("golden/memory-tags.snap"),
        "memory tags",
    )?;

    let mut expire_dry_run_args = workspace_args(&workspace);
    expire_dry_run_args.extend([
        "memory".to_owned(),
        "expire".to_owned(),
        memory_id.clone(),
        "--reason".to_owned(),
        "e2e-retention-window-ended".to_owned(),
        "--dry-run".to_owned(),
    ]);
    let expire_dry_run_output = run_step(
        &workspace,
        &events_path,
        "expire_dry_run",
        expire_dry_run_args,
    )?;
    ensure_json_stdout_isolated(&expire_dry_run_output, "expire dry run")?;
    ensure_body_marker_absent(&expire_dry_run_output, "expire dry run")?;
    let expire_dry_run_json = parse_stdout_json(&expire_dry_run_output, "expire dry run")?;
    ensure(
        expire_dry_run_json["data"]["status"] == "would_expire",
        "expire dry-run reports planned tombstone",
    )?;

    let mut expire_apply_args = workspace_args(&workspace);
    expire_apply_args.extend([
        "memory".to_owned(),
        "expire".to_owned(),
        memory_id.clone(),
        "--reason".to_owned(),
        "e2e-retention-window-ended".to_owned(),
        "--actor".to_owned(),
        "memory-expire-tags-e2e".to_owned(),
    ]);
    let expire_apply_output =
        run_step(&workspace, &events_path, "expire_apply", expire_apply_args)?;
    ensure_json_stdout_isolated(&expire_apply_output, "expire apply")?;
    ensure_body_marker_absent(&expire_apply_output, "expire apply")?;
    let expire_apply_json = parse_stdout_json(&expire_apply_output, "expire apply")?;
    ensure(
        expire_apply_json["data"]["status"] == "expired",
        "expire apply reports expired",
    )?;
    ensure(
        expire_apply_json["data"]["audit_id"].is_string(),
        "expire apply records audit id",
    )?;
    ensure(
        expire_apply_json["data"]["index_job_id"].is_string(),
        "expire apply queues an index job",
    )?;

    let mut expire_duplicate_args = workspace_args(&workspace);
    expire_duplicate_args.extend([
        "memory".to_owned(),
        "expire".to_owned(),
        memory_id.clone(),
        "--include-tombstoned".to_owned(),
    ]);
    let expire_duplicate_output = run_step(
        &workspace,
        &events_path,
        "expire_duplicate",
        expire_duplicate_args,
    )?;
    ensure_json_stdout_isolated(&expire_duplicate_output, "expire duplicate")?;
    ensure_body_marker_absent(&expire_duplicate_output, "expire duplicate")?;
    let expire_duplicate_json = parse_stdout_json(&expire_duplicate_output, "expire duplicate")?;
    ensure(
        expire_duplicate_json["data"]["status"] == "already_expired",
        "duplicate expire is idempotent",
    )?;

    let mut expired_tag_list_args = workspace_args(&workspace);
    expired_tag_list_args.extend([
        "memory".to_owned(),
        "tags".to_owned(),
        memory_id,
        "--include-tombstoned".to_owned(),
    ]);
    let expired_tag_list_output = run_step(
        &workspace,
        &events_path,
        "expired_tags_list",
        expired_tag_list_args,
    )?;
    ensure_json_stdout_isolated(&expired_tag_list_output, "expired tags list")?;
    ensure_body_marker_absent(&expired_tag_list_output, "expired tags list")?;
    let expired_tag_list_json = parse_stdout_json(&expired_tag_list_output, "expired tags list")?;
    ensure(
        expired_tag_list_json["data"]["tags"] == json!(["alpha", "release", "zeta"]),
        "expired tag list remains read-only and deterministic",
    )?;

    assert_golden(
        json!({
            "expireDryRun": normalize_memory_response(expire_dry_run_json),
            "expireApply": normalize_memory_response(expire_apply_json),
            "expireDuplicate": normalize_memory_response(expire_duplicate_json),
            "expiredTagList": normalize_memory_response(expired_tag_list_json),
        }),
        include_str!("golden/memory-expire.snap"),
        "memory expire",
    )?;

    ensure(events_path.is_file(), "E2E JSONL log exists")
}
