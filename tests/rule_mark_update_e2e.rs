//! Real-binary E2E coverage for procedural rule lifecycle and update surfaces.
//!
//! This test runs the public `ee` binary against a temporary workspace, logs
//! every command as JSONL, and compares scrubbed response contracts to a
//! golden snapshot.

use ee::db::DbConnection;
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
        .join("ee-rule-mark-update-e2e")
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
        "schema": "ee.rule_mark_update_e2e_event.v1",
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

fn scrub_rule(rule: &mut JsonValue) {
    let Some(rule) = rule.as_object_mut() else {
        return;
    };
    for key in ["id", "workspaceId"] {
        if rule.get(key).is_some_and(JsonValue::is_string) {
            let replacement = if key == "id" {
                "<RULE_ID>"
            } else {
                "<WORKSPACE_ID>"
            };
            rule.insert(key.to_owned(), JsonValue::String(replacement.to_owned()));
        }
    }
    for key in ["createdAt", "updatedAt", "lastValidatedAt"] {
        if rule.get(key).is_some_and(JsonValue::is_string) {
            rule.insert(key.to_owned(), JsonValue::String("<TIMESTAMP>".to_owned()));
        }
    }
    if let Some(source_ids) = rule
        .get_mut("sourceMemoryIds")
        .and_then(JsonValue::as_array_mut)
    {
        for id in source_ids {
            if id.is_string() {
                *id = JsonValue::String("<SOURCE_MEMORY_ID>".to_owned());
            }
        }
    }
}

fn normalize_rule_response(mut value: JsonValue) -> JsonValue {
    if let Some(data) = value.get_mut("data").and_then(JsonValue::as_object_mut) {
        data.insert(
            "ruleId".to_owned(),
            JsonValue::String("<RULE_ID>".to_owned()),
        );
        data.insert(
            "workspaceId".to_owned(),
            JsonValue::String("<WORKSPACE_ID>".to_owned()),
        );
        data.insert(
            "workspacePath".to_owned(),
            JsonValue::String("<WORKSPACE>".to_owned()),
        );
        data.insert(
            "databasePath".to_owned(),
            JsonValue::String("<DATABASE>".to_owned()),
        );
        if data.get("auditId").is_some_and(JsonValue::is_string) {
            data.insert(
                "auditId".to_owned(),
                JsonValue::String("<AUDIT_ID>".to_owned()),
            );
        }
        if data.get("indexJobId").is_some_and(JsonValue::is_string) {
            data.insert(
                "indexJobId".to_owned(),
                JsonValue::String("<INDEX_JOB_ID>".to_owned()),
            );
        }
        if let Some(rule) = data.get_mut("previousRule") {
            scrub_rule(rule);
        }
        if let Some(rule) = data.get_mut("rule") {
            scrub_rule(rule);
        }
    }
    value
}

fn scrubbed_optional_timestamp(value: &JsonValue) -> JsonValue {
    if value.is_string() {
        JsonValue::String("<TIMESTAMP>".to_owned())
    } else {
        JsonValue::Null
    }
}

fn rule_counter_projection(value: &JsonValue) -> JsonValue {
    json!({
        "schema": value["schema"],
        "success": value["success"],
        "data": {
            "schema": value["data"]["schema"],
            "command": value["data"]["command"],
            "found": value["data"]["found"],
            "rule": {
                "maturity": value["data"]["rule"]["maturity"],
                "confidence": value["data"]["rule"]["confidence"],
                "utility": value["data"]["rule"]["utility"],
                "positiveFeedbackCount": value["data"]["rule"]["positiveFeedbackCount"],
                "negativeFeedbackCount": value["data"]["rule"]["negativeFeedbackCount"],
                "validationPasses": value["data"]["rule"]["validationPasses"],
                "validationContradictions": value["data"]["rule"]["validationContradictions"],
                "lastValidatedAt": scrubbed_optional_timestamp(&value["data"]["rule"]["lastValidatedAt"]),
            },
        },
    })
}

fn rule_counter_at(value: &JsonValue, key: &str) -> Option<i64> {
    value["data"]["rule"][key].as_i64()
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

#[test]
fn rule_mark_and_update_are_audited_and_idempotent() -> TestResult {
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
        "semantic".to_owned(),
        "--kind".to_owned(),
        "fact".to_owned(),
        "Source memory for rule lifecycle evidence.".to_owned(),
    ]);
    let remember_output = run_step(&workspace, &events_path, "remember_source", remember_args)?;
    let remember_json = parse_stdout_json(&remember_output, "remember source")?;
    let source_memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember response missing memory_id".to_owned())?
        .to_owned();

    let mut rule_add_args = workspace_args(&workspace);
    rule_add_args.extend([
        "rule".to_owned(),
        "add".to_owned(),
        "--maturity".to_owned(),
        "candidate".to_owned(),
        "--scope".to_owned(),
        "workspace".to_owned(),
        "--tag".to_owned(),
        "release".to_owned(),
        "--source-memory".to_owned(),
        source_memory_id.clone(),
        "Run cargo fmt --check before release.".to_owned(),
    ]);
    let rule_add_output = run_step(&workspace, &events_path, "rule_add", rule_add_args)?;
    let rule_add_json = parse_stdout_json(&rule_add_output, "rule add")?;
    let rule_id = rule_add_json["data"]["ruleId"]
        .as_str()
        .ok_or_else(|| "rule add response missing ruleId".to_owned())?
        .to_owned();

    let mark_args = |dry_run: bool| {
        let mut args = workspace_args(&workspace);
        args.extend([
            "rule".to_owned(),
            "mark".to_owned(),
            rule_id.clone(),
            "--trigger".to_owned(),
            "validation_passed".to_owned(),
            "--helpful-outcomes".to_owned(),
            "1".to_owned(),
            "--validation-passes".to_owned(),
            "1".to_owned(),
            "--review-approved".to_owned(),
            "--actor".to_owned(),
            "rule-mark-update-e2e".to_owned(),
        ]);
        if dry_run {
            args.push("--dry-run".to_owned());
        }
        args
    };

    let mark_dry_run_output = run_step(
        &workspace,
        &events_path,
        "rule_mark_dry_run",
        mark_args(true),
    )?;
    let mark_dry_run_json = parse_stdout_json(&mark_dry_run_output, "rule mark dry-run")?;
    ensure(
        mark_dry_run_json["data"]["status"] == "would_mark",
        "rule mark dry-run reports planned mark",
    )?;
    ensure(
        mark_dry_run_json["data"]["previousRule"]["validationPasses"] == 0,
        "rule mark dry-run starts with zero validation passes",
    )?;
    ensure(
        mark_dry_run_json["data"]["rule"]["validationPasses"] == 1,
        "rule mark dry-run previews validation pass increment",
    )?;
    ensure(
        mark_dry_run_json["data"]["rule"]["positiveFeedbackCount"] == 0,
        "rule mark dry-run does not count validation as positive feedback",
    )?;

    let mark_apply_output = run_step(
        &workspace,
        &events_path,
        "rule_mark_apply",
        mark_args(false),
    )?;
    let mark_apply_json = parse_stdout_json(&mark_apply_output, "rule mark apply")?;
    ensure(
        mark_apply_json["data"]["status"] == "marked",
        "rule mark apply reports marked",
    )?;
    ensure(
        mark_apply_json["data"]["auditId"].is_string(),
        "rule mark records audit id",
    )?;
    ensure(
        mark_apply_json["data"]["indexJobId"].is_string(),
        "rule mark queues index job",
    )?;
    ensure(
        mark_apply_json["data"]["rule"]["validationPasses"] == 1,
        "rule mark apply increments validation passes",
    )?;
    ensure(
        mark_apply_json["data"]["rule"]["positiveFeedbackCount"] == 0,
        "rule mark apply keeps outcome feedback separate",
    )?;

    let update_args = |dry_run: bool| {
        let mut args = workspace_args(&workspace);
        args.extend([
            "rule".to_owned(),
            "update".to_owned(),
            rule_id.clone(),
            "--content".to_owned(),
            "Run cargo clippy --all-targets -- -D warnings before release.".to_owned(),
            "--scope".to_owned(),
            "directory".to_owned(),
            "--scope-pattern".to_owned(),
            "src/**".to_owned(),
            "--confidence".to_owned(),
            "0.92".to_owned(),
            "--utility".to_owned(),
            "0.66".to_owned(),
            "--importance".to_owned(),
            "0.7".to_owned(),
            "--protect".to_owned(),
            "--tag".to_owned(),
            "release".to_owned(),
            "--tag".to_owned(),
            "lint".to_owned(),
            "--source-memory".to_owned(),
            source_memory_id.clone(),
            "--actor".to_owned(),
            "rule-mark-update-e2e".to_owned(),
        ]);
        if dry_run {
            args.push("--dry-run".to_owned());
        }
        args
    };

    let update_dry_run_output = run_step(
        &workspace,
        &events_path,
        "rule_update_dry_run",
        update_args(true),
    )?;
    let update_dry_run_json = parse_stdout_json(&update_dry_run_output, "rule update dry-run")?;
    ensure(
        update_dry_run_json["data"]["status"] == "would_update",
        "rule update dry-run reports planned update",
    )?;

    let update_apply_output = run_step(
        &workspace,
        &events_path,
        "rule_update_apply",
        update_args(false),
    )?;
    let update_apply_json = parse_stdout_json(&update_apply_output, "rule update apply")?;
    ensure(
        update_apply_json["data"]["status"] == "updated",
        "rule update apply reports updated",
    )?;
    ensure(
        update_apply_json["data"]["auditId"].is_string(),
        "rule update records audit id",
    )?;
    ensure(
        update_apply_json["data"]["indexJobId"].is_string(),
        "rule update queues index job",
    )?;

    let duplicate_output = run_step(
        &workspace,
        &events_path,
        "rule_update_duplicate",
        update_args(false),
    )?;
    let duplicate_json = parse_stdout_json(&duplicate_output, "rule update duplicate")?;
    ensure(
        duplicate_json["data"]["status"] == "unchanged",
        "duplicate rule update is idempotent",
    )?;
    ensure(
        duplicate_json["data"]["auditId"].is_null(),
        "duplicate rule update does not add an audit",
    )?;

    let connection = DbConnection::open_file(workspace.join(".ee").join("ee.db"))
        .map_err(|error| error.to_string())?;
    let rule = connection
        .get_procedural_rule(&rule_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "updated rule missing from DB".to_owned())?;
    ensure(rule.maturity == "validated", "DB rule is validated")?;
    ensure(
        rule.validation_passes == 1,
        "DB rule records validation pass count",
    )?;
    ensure(
        rule.validation_contradictions == 0,
        "DB rule starts with zero validation contradictions",
    )?;
    ensure(
        rule.positive_feedback_count == 0,
        "DB rule does not count validation as positive feedback",
    )?;
    ensure(rule.protected, "DB rule is protected")?;
    ensure(rule.scope == "directory", "DB rule scope updated")?;
    ensure(
        rule.scope_pattern.as_deref() == Some("src/**"),
        "DB rule pattern updated",
    )?;
    let audits = connection
        .list_audit_by_target("rule", &rule_id, None)
        .map_err(|error| error.to_string())?;
    ensure(
        audits.len() == 3,
        "DB has rule add, mark, and update audit rows only",
    )?;
    connection.close().map_err(|error| error.to_string())?;

    assert_golden(
        json!({
            "markDryRun": normalize_rule_response(mark_dry_run_json),
            "markApply": normalize_rule_response(mark_apply_json),
            "updateDryRun": normalize_rule_response(update_dry_run_json),
            "updateApply": normalize_rule_response(update_apply_json),
            "updateDuplicate": normalize_rule_response(duplicate_json),
        }),
        include_str!("golden/rule-mark-update.snap"),
        "rule mark update",
    )?;

    ensure(events_path.is_file(), "E2E JSONL log exists")
}

#[test]
fn rule_mark_validation_counter_golden_pins_non_interference() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let events_path = run_dir.join("validation-events.jsonl");

    let mut init_args = workspace_args(&workspace);
    init_args.push("init".to_owned());
    run_step(&workspace, &events_path, "init", init_args)?;

    let mut remember_args = workspace_args(&workspace);
    remember_args.extend([
        "remember".to_owned(),
        "--level".to_owned(),
        "semantic".to_owned(),
        "--kind".to_owned(),
        "fact".to_owned(),
        "Source memory for validation counter golden.".to_owned(),
    ]);
    let remember_output = run_step(&workspace, &events_path, "remember_source", remember_args)?;
    let remember_json = parse_stdout_json(&remember_output, "remember source")?;
    let source_memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember response missing memory_id".to_owned())?
        .to_owned();

    let mut rule_add_args = workspace_args(&workspace);
    rule_add_args.extend([
        "rule".to_owned(),
        "add".to_owned(),
        "--maturity".to_owned(),
        "candidate".to_owned(),
        "--scope".to_owned(),
        "workspace".to_owned(),
        "--source-memory".to_owned(),
        source_memory_id,
        "Validate rule counters independently from outcome feedback.".to_owned(),
    ]);
    let rule_add_output = run_step(&workspace, &events_path, "rule_add", rule_add_args)?;
    let rule_add_json = parse_stdout_json(&rule_add_output, "rule add")?;
    let rule_id = rule_add_json["data"]["ruleId"]
        .as_str()
        .ok_or_else(|| "rule add response missing ruleId".to_owned())?
        .to_owned();

    let show_rule = |step: &str| -> Result<JsonValue, String> {
        let mut args = workspace_args(&workspace);
        args.extend(["rule".to_owned(), "show".to_owned(), rule_id.clone()]);
        let output = run_step(&workspace, &events_path, step, args)?;
        parse_stdout_json(&output, step)
    };

    let mark_rule = |step: &str, trigger: &str, extra_args: &[&str]| -> Result<JsonValue, String> {
        let mut args = workspace_args(&workspace);
        args.extend([
            "rule".to_owned(),
            "mark".to_owned(),
            rule_id.clone(),
            "--trigger".to_owned(),
            trigger.to_owned(),
            "--actor".to_owned(),
            "rule-validation-counter-golden".to_owned(),
        ]);
        args.extend(extra_args.iter().map(|arg| (*arg).to_owned()));
        let output = run_step(&workspace, &events_path, step, args)?;
        parse_stdout_json(&output, step)
    };

    let show_initial = show_rule("show_initial")?;
    let first_validation = mark_rule("validation_passed_1", "validation_passed", &[])?;
    ensure(
        first_validation["data"]["rule"]["validationPasses"] == 1,
        "first validation_passed increments validation passes from 0 to 1",
    )?;
    let show_after_first = show_rule("show_after_first_validation")?;

    let second_validation = mark_rule("validation_passed_2", "validation_passed", &[])?;
    ensure(
        second_validation["data"]["rule"]["validationPasses"] == 2,
        "second validation_passed increments validation passes from 1 to 2",
    )?;
    let show_after_second = show_rule("show_after_second_validation")?;

    let helpful = mark_rule("outcome_helpful", "outcome_helpful", &[])?;
    ensure(
        helpful["data"]["rule"]["validationPasses"] == 2,
        "outcome_helpful leaves validation passes unchanged",
    )?;
    ensure(
        helpful["data"]["rule"]["positiveFeedbackCount"] == 1,
        "outcome_helpful increments positive feedback",
    )?;
    let show_after_helpful = show_rule("show_after_helpful_outcome")?;

    let override_validation = mark_rule(
        "validation_passed_override",
        "validation_passed",
        &["--validation-passes", "5"],
    )?;
    ensure(
        override_validation["data"]["rule"]["validationPasses"] == 7,
        "validation_passed --validation-passes 5 adds five validation passes",
    )?;
    let show_after_override = show_rule("show_after_validation_override")?;

    ensure(
        rule_counter_at(&show_initial, "validationPasses") == Some(0),
        "initial show starts with zero validation passes",
    )?;
    ensure(
        rule_counter_at(&show_after_first, "validationPasses") == Some(1),
        "first show records one validation pass",
    )?;
    ensure(
        rule_counter_at(&show_after_second, "validationPasses") == Some(2),
        "second show records two validation passes",
    )?;
    ensure(
        rule_counter_at(&show_after_helpful, "validationPasses") == Some(2),
        "helpful outcome show keeps validation passes at two",
    )?;
    ensure(
        rule_counter_at(&show_after_helpful, "positiveFeedbackCount") == Some(1),
        "helpful outcome show increments positive feedback",
    )?;
    ensure(
        rule_counter_at(&show_after_override, "validationPasses") == Some(7),
        "override show records seven validation passes",
    )?;
    ensure(
        rule_counter_at(&show_after_override, "positiveFeedbackCount") == Some(1),
        "override show preserves positive feedback count",
    )?;

    assert_golden(
        json!({
            "showInitial": rule_counter_projection(&show_initial),
            "showAfterFirstValidation": rule_counter_projection(&show_after_first),
            "showAfterSecondValidation": rule_counter_projection(&show_after_second),
            "showAfterHelpfulOutcome": rule_counter_projection(&show_after_helpful),
            "showAfterValidationOverride": rule_counter_projection(&show_after_override),
        }),
        include_str!("golden/rule-mark-validation.snap"),
        "rule mark validation counter",
    )?;

    ensure(
        events_path.is_file(),
        "validation counter E2E JSONL log exists",
    )
}
