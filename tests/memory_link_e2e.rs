//! Real-binary E2E coverage for the durable memory-link surface.
//!
//! This test runs the public `ee` binary against a temporary workspace, logs
//! every command as JSONL, and compares scrubbed response contracts to a golden
//! snapshot.

use ee::db::DbConnection;
use serde_json::{Value as JsonValue, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

const BODY_MARKER: &str = "E2E-MEMORY-LINK-BODY-MARKER";

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
        .join("ee-memory-link-e2e")
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
        "schema": "ee.memory_link_e2e_event.v1",
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

fn scrub_link_item(item: &mut JsonValue) {
    let Some(item) = item.as_object_mut() else {
        return;
    };
    if item.get("link_id").is_some_and(JsonValue::is_string) {
        item.insert(
            "link_id".to_owned(),
            JsonValue::String("<LINK_ID>".to_owned()),
        );
    }
    if item
        .get("source_memory_id")
        .is_some_and(JsonValue::is_string)
    {
        item.insert(
            "source_memory_id".to_owned(),
            JsonValue::String("<SOURCE_MEMORY_ID>".to_owned()),
        );
    }
    if item
        .get("target_memory_id")
        .is_some_and(JsonValue::is_string)
    {
        item.insert(
            "target_memory_id".to_owned(),
            JsonValue::String("<TARGET_MEMORY_ID>".to_owned()),
        );
    }
    if item.get("created_at").is_some_and(JsonValue::is_string) {
        item.insert(
            "created_at".to_owned(),
            JsonValue::String("<TIMESTAMP>".to_owned()),
        );
    }
}

fn normalize_memory_link_response(mut value: JsonValue) -> JsonValue {
    if let Some(data) = value.get_mut("data").and_then(JsonValue::as_object_mut) {
        data.insert(
            "memory_id".to_owned(),
            JsonValue::String("<SOURCE_MEMORY_ID>".to_owned()),
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
        if let Some(link) = data.get_mut("link") {
            scrub_link_item(link);
        }
        if let Some(links) = data.get_mut("links").and_then(JsonValue::as_array_mut) {
            for link in links {
                scrub_link_item(link);
            }
        }
    }
    value
}

fn canonicalize_json(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Array(values) => {
            JsonValue::Array(values.into_iter().map(canonicalize_json).collect())
        }
        JsonValue::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));

            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key, canonicalize_json(value));
            }
            JsonValue::Object(sorted)
        }
        value => value,
    }
}

fn assert_golden(actual: JsonValue, expected: &str, label: &str) -> TestResult {
    let actual = serde_json::to_string_pretty(&canonicalize_json(actual))
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
fn memory_link_create_list_and_duplicate_are_audited_and_logged() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let events_path = run_dir.join("events.jsonl");

    let mut init_args = workspace_args(&workspace);
    init_args.push("init".to_owned());
    run_step(&workspace, &events_path, "init", init_args)?;

    let mut remember_source_args = workspace_args(&workspace);
    remember_source_args.extend([
        "remember".to_owned(),
        "--level".to_owned(),
        "semantic".to_owned(),
        "--kind".to_owned(),
        "fact".to_owned(),
        "--tags".to_owned(),
        "graph,links".to_owned(),
        format!("Source memory for explicit link creation. {BODY_MARKER}"),
    ]);
    let source_output = run_step(
        &workspace,
        &events_path,
        "remember_source",
        remember_source_args,
    )?;
    let source_json = parse_stdout_json(&source_output, "remember source")?;
    let source_memory_id = source_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "source remember response missing memory_id".to_owned())?
        .to_owned();

    let mut remember_target_args = workspace_args(&workspace);
    remember_target_args.extend([
        "remember".to_owned(),
        "--level".to_owned(),
        "procedural".to_owned(),
        "--kind".to_owned(),
        "rule".to_owned(),
        "--tags".to_owned(),
        "graph,links".to_owned(),
        format!("Target memory supported by the source memory. {BODY_MARKER}"),
    ]);
    let target_output = run_step(
        &workspace,
        &events_path,
        "remember_target",
        remember_target_args,
    )?;
    let target_json = parse_stdout_json(&target_output, "remember target")?;
    let target_memory_id = target_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "target remember response missing memory_id".to_owned())?
        .to_owned();

    let mut initial_list_args = workspace_args(&workspace);
    initial_list_args.extend([
        "memory".to_owned(),
        "link".to_owned(),
        source_memory_id.clone(),
    ]);
    let initial_list_output = run_step(
        &workspace,
        &events_path,
        "link_initial_list",
        initial_list_args,
    )?;
    ensure_json_stdout_isolated(&initial_list_output, "initial link list")?;
    ensure_body_marker_absent(&initial_list_output, "initial link list")?;
    let initial_list_json = parse_stdout_json(&initial_list_output, "initial link list")?;
    ensure(
        initial_list_json["data"]["status"] == "listed",
        "initial link list reports listed",
    )?;
    ensure(
        initial_list_json["data"]["links"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "initial link list is empty",
    )?;

    let mut link_dry_run_args = workspace_args(&workspace);
    link_dry_run_args.extend([
        "memory".to_owned(),
        "link".to_owned(),
        source_memory_id.clone(),
        target_memory_id.clone(),
        "--relation".to_owned(),
        "supports".to_owned(),
        "--weight".to_owned(),
        "0.75".to_owned(),
        "--confidence".to_owned(),
        "0.9".to_owned(),
        "--evidence-count".to_owned(),
        "2".to_owned(),
        "--metadata".to_owned(),
        r#"{"reason":"e2e explicit support"}"#.to_owned(),
        "--actor".to_owned(),
        "memory-link-e2e".to_owned(),
        "--dry-run".to_owned(),
    ]);
    let link_dry_run_output = run_step(
        &workspace,
        &events_path,
        "link_create_dry_run",
        link_dry_run_args,
    )?;
    ensure_json_stdout_isolated(&link_dry_run_output, "link dry run")?;
    ensure_body_marker_absent(&link_dry_run_output, "link dry run")?;
    let link_dry_run_json = parse_stdout_json(&link_dry_run_output, "link dry run")?;
    ensure(
        link_dry_run_json["data"]["status"] == "would_create",
        "link dry-run reports planned creation",
    )?;

    let mut link_apply_args = workspace_args(&workspace);
    link_apply_args.extend([
        "memory".to_owned(),
        "link".to_owned(),
        source_memory_id.clone(),
        target_memory_id.clone(),
        "--relation".to_owned(),
        "supports".to_owned(),
        "--weight".to_owned(),
        "0.75".to_owned(),
        "--confidence".to_owned(),
        "0.9".to_owned(),
        "--evidence-count".to_owned(),
        "2".to_owned(),
        "--metadata".to_owned(),
        r#"{"reason":"e2e explicit support"}"#.to_owned(),
        "--actor".to_owned(),
        "memory-link-e2e".to_owned(),
    ]);
    let link_apply_output = run_step(
        &workspace,
        &events_path,
        "link_create_apply",
        link_apply_args.clone(),
    )?;
    ensure_json_stdout_isolated(&link_apply_output, "link apply")?;
    ensure_body_marker_absent(&link_apply_output, "link apply")?;
    let link_apply_json = parse_stdout_json(&link_apply_output, "link apply")?;
    ensure(
        link_apply_json["data"]["status"] == "created",
        "link apply reports created",
    )?;
    let link_id = link_apply_json["data"]["link"]["link_id"]
        .as_str()
        .ok_or_else(|| "link apply response missing link_id".to_owned())?
        .to_owned();
    ensure(
        link_apply_json["data"]["audit_id"].is_string(),
        "link apply records audit id",
    )?;

    let duplicate_output = run_step(
        &workspace,
        &events_path,
        "link_create_duplicate",
        link_apply_args,
    )?;
    ensure_json_stdout_isolated(&duplicate_output, "link duplicate")?;
    ensure_body_marker_absent(&duplicate_output, "link duplicate")?;
    let duplicate_json = parse_stdout_json(&duplicate_output, "link duplicate")?;
    ensure(
        duplicate_json["data"]["status"] == "already_exists",
        "duplicate link is idempotent",
    )?;
    ensure(
        duplicate_json["data"]["link"]["link_id"] == link_id,
        "duplicate reports existing link id",
    )?;

    let mut relation_list_args = workspace_args(&workspace);
    relation_list_args.extend([
        "memory".to_owned(),
        "link".to_owned(),
        source_memory_id.clone(),
        "--relation".to_owned(),
        "supports".to_owned(),
    ]);
    let relation_list_output = run_step(
        &workspace,
        &events_path,
        "link_relation_list",
        relation_list_args,
    )?;
    ensure_json_stdout_isolated(&relation_list_output, "relation link list")?;
    ensure_body_marker_absent(&relation_list_output, "relation link list")?;
    let relation_list_json = parse_stdout_json(&relation_list_output, "relation link list")?;
    ensure(
        relation_list_json["data"]["links"]
            .as_array()
            .is_some_and(|links| links.len() == 1),
        "relation link list returns one link",
    )?;

    let connection = DbConnection::open_file(workspace.join(".ee").join("ee.db"))
        .map_err(|error| error.to_string())?;
    let links = connection
        .list_all_memory_links(None)
        .map_err(|error| error.to_string())?;
    ensure(links.len() == 1, "DB has exactly one memory link row")?;
    let audits = connection
        .list_audit_by_target("memory_link", &link_id, None)
        .map_err(|error| error.to_string())?;
    ensure(
        audits.len() == 1,
        "DB has exactly one memory-link audit row",
    )?;
    connection.close().map_err(|error| error.to_string())?;

    assert_golden(
        json!({
            "initialList": normalize_memory_link_response(initial_list_json),
            "dryRun": normalize_memory_link_response(link_dry_run_json),
            "apply": normalize_memory_link_response(link_apply_json),
            "duplicate": normalize_memory_link_response(duplicate_json),
            "relationList": normalize_memory_link_response(relation_list_json),
        }),
        include_str!("golden/memory-link.snap"),
        "memory link",
    )?;

    ensure(events_path.is_file(), "E2E JSONL log exists")
}
