//! Real-binary E2E coverage for export and portable playbook surfaces.
//!
//! This test runs the public `ee` binary against temporary workspaces, logs
//! every command as JSONL, and compares scrubbed response contracts to golden
//! snapshots.

use serde_json::{Value as JsonValue, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

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
        .join("ee-export-playbook-e2e")
        .join(format!("{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

fn run_ee(workspace: &Path, args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
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
        "schema": "ee.export_playbook_e2e_event.v1",
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

fn normalize_export_response(mut value: JsonValue) -> JsonValue {
    value["data"]["workspacePath"] = JsonValue::String("<WORKSPACE>".to_owned());
    value["data"]["workspaceId"] = JsonValue::String("<WORKSPACE_ID>".to_owned());
    value["data"]["databasePath"] = JsonValue::String("<DATABASE>".to_owned());
    value["data"]["outputPath"] = JsonValue::String("<EXPORT_DIR>".to_owned());
    value["data"]["manifestPath"] = JsonValue::String("<MANIFEST>".to_owned());
    value["data"]["recordsPath"] = JsonValue::String("<RECORDS>".to_owned());
    value["data"]["manifestHash"] = JsonValue::String("<MANIFEST_HASH>".to_owned());
    value["data"]["recordsHash"] = JsonValue::String("<RECORDS_HASH>".to_owned());
    value["data"]["provenance"]["backupId"] = JsonValue::String("<BACKUP_ID>".to_owned());
    if let Some(artifacts) = value["data"]["artifacts"].as_array_mut() {
        for artifact in artifacts {
            if artifact["hash"].is_string() {
                artifact["hash"] = JsonValue::String("<ARTIFACT_HASH>".to_owned());
            }
            if artifact["sizeBytes"].is_number() {
                artifact["sizeBytes"] = JsonValue::from(0);
            }
        }
    }
    value
}

fn scrub_playbook_rule(rule: &mut JsonValue) {
    if rule["sourceRuleId"].is_string() {
        rule["sourceRuleId"] = JsonValue::String("<RULE_ID>".to_owned());
    }
    if rule["createdAt"].is_string() {
        rule["createdAt"] = JsonValue::String("<TIMESTAMP>".to_owned());
    }
    if rule["updatedAt"].is_string() {
        rule["updatedAt"] = JsonValue::String("<TIMESTAMP>".to_owned());
    }
}

fn normalize_playbook_list_response(mut value: JsonValue) -> JsonValue {
    value["data"]["workspaceId"] = JsonValue::String("<WORKSPACE_ID>".to_owned());
    value["data"]["workspacePath"] = JsonValue::String("<WORKSPACE>".to_owned());
    value["data"]["databasePath"] = JsonValue::String("<DATABASE>".to_owned());
    if let Some(rules) = value["data"]["rules"].as_array_mut() {
        for rule in rules {
            scrub_playbook_rule(rule);
        }
    }
    value
}

fn normalize_playbook_export_response(mut value: JsonValue) -> JsonValue {
    value["data"]["workspaceId"] = JsonValue::String("<WORKSPACE_ID>".to_owned());
    value["data"]["workspacePath"] = JsonValue::String("<WORKSPACE>".to_owned());
    value["data"]["databasePath"] = JsonValue::String("<DATABASE>".to_owned());
    value["data"]["outputPath"] = JsonValue::String("<PLAYBOOK>".to_owned());
    value["data"]["artifactHash"] = JsonValue::String("<ARTIFACT_HASH>".to_owned());
    value["data"]["document"]["exportedAt"] = JsonValue::String("<TIMESTAMP>".to_owned());
    value["data"]["document"]["workspaceId"] = JsonValue::String("<WORKSPACE_ID>".to_owned());
    value["data"]["document"]["workspacePath"] = JsonValue::String("<WORKSPACE>".to_owned());
    if let Some(rules) = value["data"]["document"]["rules"].as_array_mut() {
        for rule in rules {
            scrub_playbook_rule(rule);
        }
    }
    value
}

fn normalize_playbook_import_response(mut value: JsonValue) -> JsonValue {
    value["data"]["workspaceId"] = JsonValue::String("<WORKSPACE_ID>".to_owned());
    value["data"]["workspacePath"] = JsonValue::String("<TARGET_WORKSPACE>".to_owned());
    value["data"]["databasePath"] = JsonValue::String("<TARGET_DATABASE>".to_owned());
    value["data"]["sourcePath"] = JsonValue::String("<PLAYBOOK>".to_owned());
    value["data"]["sourceHash"] = JsonValue::String("<SOURCE_HASH>".to_owned());
    if let Some(decisions) = value["data"]["decisions"].as_array_mut() {
        for decision in decisions {
            if decision["sourceRuleId"].is_string() {
                decision["sourceRuleId"] = JsonValue::String("<SOURCE_RULE_ID>".to_owned());
            }
            if decision["importedRuleId"].is_string() {
                decision["importedRuleId"] = JsonValue::String("<IMPORTED_RULE_ID>".to_owned());
            }
            if decision["auditId"].is_string() {
                decision["auditId"] = JsonValue::String("<AUDIT_ID>".to_owned());
            }
            if decision["indexJobId"].is_string() {
                decision["indexJobId"] = JsonValue::String("<INDEX_JOB_ID>".to_owned());
            }
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

#[test]
fn export_and_playbook_surfaces_round_trip_with_logged_binary_run() -> TestResult {
    let run_dir = unique_run_dir()?;
    let source_workspace = run_dir.join("source-workspace");
    let target_workspace = run_dir.join("target-workspace");
    fs::create_dir_all(&source_workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&target_workspace).map_err(|error| error.to_string())?;
    let events_path = run_dir.join("events.jsonl");
    let export_dir = run_dir.join("record-export");
    let playbook_path = run_dir.join("playbook.json");

    let mut init_source_args = workspace_args(&source_workspace);
    init_source_args.push("init".to_owned());
    run_step(
        &source_workspace,
        &events_path,
        "init_source",
        init_source_args,
    )?;

    let mut remember_args = workspace_args(&source_workspace);
    remember_args.extend([
        "remember".to_owned(),
        "--level".to_owned(),
        "semantic".to_owned(),
        "--kind".to_owned(),
        "fact".to_owned(),
        "Use /data/private/customer-project before release.".to_owned(),
    ]);
    run_step(
        &source_workspace,
        &events_path,
        "remember_path_fact",
        remember_args,
    )?;

    let mut rule_add_args = workspace_args(&source_workspace);
    rule_add_args.extend([
        "rule".to_owned(),
        "add".to_owned(),
        "--maturity".to_owned(),
        "candidate".to_owned(),
        "--scope".to_owned(),
        "workspace".to_owned(),
        "--tag".to_owned(),
        "release".to_owned(),
        "Run cargo fmt --check before release.".to_owned(),
    ]);
    run_step(&source_workspace, &events_path, "rule_add", rule_add_args)?;

    let mut export_args = workspace_args(&source_workspace);
    export_args.extend([
        "export".to_owned(),
        "--output-dir".to_owned(),
        export_dir.to_string_lossy().into_owned(),
        "--redaction".to_owned(),
        "standard".to_owned(),
    ]);
    let export_output = run_step(&source_workspace, &events_path, "export", export_args)?;
    let export_json = parse_stdout_json(&export_output, "export")?;
    ensure(
        export_json["schema"] == "ee.response.v1",
        "export response schema",
    )?;
    ensure(export_json["success"] == true, "export response success")?;
    ensure(
        export_json["data"]["schema"] == "ee.export.report.v1",
        "export report schema",
    )?;
    let records_path = export_json["data"]["recordsPath"]
        .as_str()
        .ok_or_else(|| "export missing recordsPath".to_owned())?;
    let records = fs::read_to_string(records_path)
        .map_err(|error| format!("failed to read {records_path}: {error}"))?;
    ensure(
        !records.contains("/data/private/customer-project"),
        "export records must redact raw path content",
    )?;
    assert_golden(
        normalize_export_response(export_json),
        include_str!("golden/export.snap"),
        "export",
    )?;

    let mut list_args = workspace_args(&source_workspace);
    list_args.extend(["playbook".to_owned(), "list".to_owned()]);
    let list_output = run_step(&source_workspace, &events_path, "playbook_list", list_args)?;
    let list_json = parse_stdout_json(&list_output, "playbook list")?;
    ensure(
        list_json["data"]["schema"] == "ee.playbook.list.v1",
        "playbook list schema",
    )?;
    ensure(
        list_json["data"]["returnedCount"] == 1,
        "playbook list returned one rule",
    )?;
    assert_golden(
        normalize_playbook_list_response(list_json),
        include_str!("golden/playbook-list.snap"),
        "playbook list",
    )?;

    let mut playbook_export_args = workspace_args(&source_workspace);
    playbook_export_args.extend([
        "playbook".to_owned(),
        "export".to_owned(),
        "--out".to_owned(),
        playbook_path.to_string_lossy().into_owned(),
    ]);
    let playbook_export_output = run_step(
        &source_workspace,
        &events_path,
        "playbook_export",
        playbook_export_args,
    )?;
    ensure(playbook_path.is_file(), "playbook export file exists")?;
    let playbook_export_json = parse_stdout_json(&playbook_export_output, "playbook export")?;
    ensure(
        playbook_export_json["data"]["schema"] == "ee.playbook.export.v1",
        "playbook export schema",
    )?;
    ensure(
        playbook_export_json["data"]["exportedCount"] == 1,
        "playbook export count",
    )?;
    let playbook_text = fs::read_to_string(&playbook_path)
        .map_err(|error| format!("failed to read {}: {error}", playbook_path.display()))?;
    ensure(
        !playbook_text.contains("/data/private/customer-project"),
        "playbook export must not include raw memory content",
    )?;
    assert_golden(
        normalize_playbook_export_response(playbook_export_json),
        include_str!("golden/playbook-export.snap"),
        "playbook export",
    )?;

    let mut init_target_args = workspace_args(&target_workspace);
    init_target_args.push("init".to_owned());
    run_step(
        &target_workspace,
        &events_path,
        "init_target",
        init_target_args,
    )?;

    let mut import_dry_run_args = workspace_args(&target_workspace);
    import_dry_run_args.extend([
        "playbook".to_owned(),
        "import".to_owned(),
        "--source".to_owned(),
        playbook_path.to_string_lossy().into_owned(),
    ]);
    let import_dry_run_output = run_step(
        &target_workspace,
        &events_path,
        "playbook_import_dry_run",
        import_dry_run_args,
    )?;
    let import_dry_run_json = parse_stdout_json(&import_dry_run_output, "playbook import dry-run")?;
    ensure(
        import_dry_run_json["data"]["schema"] == "ee.playbook.import.v1",
        "playbook import dry-run schema",
    )?;
    ensure(
        import_dry_run_json["data"]["dryRun"] == true,
        "playbook import defaults to dry-run",
    )?;
    ensure(
        import_dry_run_json["data"]["durableMutation"] == false,
        "playbook import dry-run does not mutate",
    )?;
    ensure(
        import_dry_run_json["data"]["decisions"][0]["status"] == "would_import",
        "playbook import dry-run reports planned import",
    )?;

    let mut import_apply_args = workspace_args(&target_workspace);
    import_apply_args.extend([
        "playbook".to_owned(),
        "import".to_owned(),
        "--source".to_owned(),
        playbook_path.to_string_lossy().into_owned(),
        "--apply".to_owned(),
        "--actor".to_owned(),
        "export-playbook-e2e".to_owned(),
    ]);
    let import_apply_output = run_step(
        &target_workspace,
        &events_path,
        "playbook_import_apply",
        import_apply_args.clone(),
    )?;
    let import_apply_json = parse_stdout_json(&import_apply_output, "playbook import apply")?;
    ensure(
        import_apply_json["data"]["importedCount"] == 1,
        "playbook import apply imports one rule",
    )?;
    ensure(
        import_apply_json["data"]["durableMutation"] == true,
        "playbook import apply mutates durably",
    )?;
    ensure(
        import_apply_json["data"]["decisions"][0]["auditId"].is_string(),
        "playbook import apply records audit id",
    )?;
    ensure(
        import_apply_json["data"]["decisions"][0]["indexJobId"].is_string(),
        "playbook import apply queues index job",
    )?;
    assert_golden(
        normalize_playbook_import_response(import_apply_json),
        include_str!("golden/playbook-import.snap"),
        "playbook import",
    )?;

    let duplicate_output = run_step(
        &target_workspace,
        &events_path,
        "playbook_import_duplicate",
        import_apply_args,
    )?;
    let duplicate_json = parse_stdout_json(&duplicate_output, "playbook import duplicate")?;
    ensure(
        duplicate_json["data"]["duplicateCount"] == 1,
        "playbook import duplicate is idempotent",
    )?;
    ensure(events_path.is_file(), "E2E JSONL log exists")
}
