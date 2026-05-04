//! EE-TST-005 logged advanced subsystem scenarios.
//!
//! Captures structured, replay-friendly logs for recorder, preflight,
//! procedure, lab, economy, learning, and causal commands.

use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

#[derive(Clone, Debug)]
struct StepSpec {
    subsystem: &'static str,
    name: &'static str,
    args: Vec<String>,
    expected_schema_contains: &'static str,
    expected_exit_code: i32,
    expect_clean_stderr: bool,
}

#[derive(Clone, Debug, Serialize)]
struct SchemaValidation {
    status: String,
    expected_contains: String,
    actual_schema: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct GoldenValidation {
    status: String,
    reason: String,
}

#[derive(Clone, Debug, Serialize)]
struct SanitizedEnvOverride {
    name: String,
    value: String,
}

#[derive(Clone, Debug, Serialize)]
struct CommandLog {
    schema: String,
    subsystem: String,
    step_name: String,
    command: String,
    args: Vec<String>,
    cwd: String,
    workspace: String,
    env_override_names: Vec<String>,
    env_sanitized: Vec<SanitizedEnvOverride>,
    started_at_unix_ms: u128,
    ended_at_unix_ms: u128,
    elapsed_ms: u128,
    exit_code: i32,
    stdout_artifact_path: String,
    stderr_artifact_path: String,
    stdout_json_valid: bool,
    stderr_is_empty: bool,
    schema_validation: SchemaValidation,
    golden_validation: GoldenValidation,
    redaction_status: String,
    evidence_ids: Vec<String>,
    degradation_codes: Vec<String>,
    mutation_summary: String,
    first_failure: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ScenarioValidation {
    schema_validation: String,
    golden_validation: String,
    stdout_stderr_isolation: String,
}

#[derive(Clone, Debug, Serialize)]
struct ScenarioSummary {
    schema: String,
    scenario_id: String,
    workspace: String,
    command_count: usize,
    subsystems_covered: Vec<String>,
    environment_overrides: Vec<String>,
    commands: Vec<CommandLog>,
    validation: ScenarioValidation,
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
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn unique_scenario_dir(scenario_id: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-advanced-e2e-logs")
        .join(format!("{scenario_id}-{}-{now}", std::process::id()));
    fs::create_dir_all(&root)
        .map_err(|error| format!("failed to create scenario dir {}: {error}", root.display()))?;
    Ok(root)
}

fn unix_ms_now() -> Result<u128, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_millis())
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create parent {}: {error}", parent.display()))?;
    }
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn sanitize_step_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn schema_from_json(value: &JsonValue) -> Option<String> {
    value
        .get("schema")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

fn collect_string_fields(value: &JsonValue, key_suffix: &str, out: &mut Vec<String>) {
    match value {
        JsonValue::Object(map) => {
            for (key, child) in map {
                if key.ends_with(key_suffix) {
                    match child {
                        JsonValue::String(text) => out.push(text.clone()),
                        JsonValue::Array(items) => {
                            for item in items {
                                if let Some(text) = item.as_str() {
                                    out.push(text.to_owned());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                collect_string_fields(child, key_suffix, out);
            }
        }
        JsonValue::Array(items) => {
            for child in items {
                collect_string_fields(child, key_suffix, out);
            }
        }
        _ => {}
    }
}

fn extract_evidence_ids(value: Option<&JsonValue>) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(value) = value {
        collect_string_fields(value, "Id", &mut ids);
        collect_string_fields(value, "Ids", &mut ids);
        collect_string_fields(value, "_id", &mut ids);
        collect_string_fields(value, "_ids", &mut ids);
        ids.sort();
        ids.dedup();
    }
    ids
}

fn extract_degradation_codes(value: Option<&JsonValue>) -> Vec<String> {
    let mut codes = Vec::new();
    if let Some(value) = value {
        collect_string_fields(value, "code", &mut codes);
        collect_string_fields(value, "Code", &mut codes);
        collect_string_fields(value, "Codes", &mut codes);
        if let Some(warnings) = value
            .pointer("/data/warnings")
            .and_then(JsonValue::as_array)
        {
            for warning in warnings {
                if let Some((code, _)) = warning.as_str().and_then(|text| text.split_once(':')) {
                    if code.ends_with("_unavailable") {
                        codes.push(code.to_owned());
                    }
                }
            }
        }
        codes.sort();
        codes.dedup();
    }
    codes
}

fn extract_redaction_status(value: Option<&JsonValue>) -> String {
    let candidates = [
        "/redactionStatus",
        "/redaction_status",
        "/data/redactionStatus",
        "/data/redaction_status",
    ];
    for pointer in candidates {
        if let Some(status) = value
            .and_then(|json| json.pointer(pointer))
            .and_then(JsonValue::as_str)
        {
            return status.to_owned();
        }
    }
    "not_reported".to_owned()
}

fn mutation_summary(spec: &StepSpec) -> String {
    if spec.args.iter().any(|arg| arg == "--dry-run") {
        "dry_run_no_mutation_expected".to_owned()
    } else if spec.name == "init_workspace" {
        "durable_write_expected".to_owned()
    } else {
        "read_only".to_owned()
    }
}

fn first_failure_diagnosis(
    exit_code: i32,
    parsed_stdout: Option<&JsonValue>,
    stdout: &str,
    stderr: &str,
    expected_schema_contains: &str,
) -> Option<String> {
    if parsed_stdout.is_none() {
        let trimmed = stdout.trim_start();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return Some("stdout_json_parse_failed".to_owned());
        }
        return Some("stdout_pollution".to_owned());
    }

    let actual_schema = parsed_stdout.and_then(schema_from_json);
    if !actual_schema
        .as_deref()
        .is_some_and(|schema| schema.contains(expected_schema_contains))
    {
        return Some(format!(
            "schema_mismatch:{}",
            actual_schema.unwrap_or_else(|| "missing".to_owned())
        ));
    }

    if exit_code == 0 {
        return None;
    }
    if parsed_stdout.is_some_and(|json| {
        json.pointer("/success") == Some(&JsonValue::Bool(false))
            && json
                .pointer("/data/degraded")
                .and_then(JsonValue::as_array)
                .is_some_and(|degraded| !degraded.is_empty())
    }) {
        return None;
    }
    if let Some(code) = parsed_stdout
        .and_then(|json| json.pointer("/error/code"))
        .and_then(JsonValue::as_str)
    {
        return Some(format!("error.code={code}"));
    }
    let line = stderr.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        Some("non-zero exit with empty diagnostics".to_owned())
    } else {
        Some(line.to_owned())
    }
}

fn run_logged_step(
    scenario_dir: &Path,
    workspace: &Path,
    env_overrides: &[(&str, &str)],
    spec: &StepSpec,
) -> Result<CommandLog, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(&spec.args);
    for (key, value) in env_overrides {
        command.env(key, value);
    }

    let started_at_unix_ms = unix_ms_now()?;
    let start = Instant::now();
    let output = command
        .output()
        .map_err(|error| format!("failed to execute step {}: {error}", spec.name))?;
    let elapsed_ms = start.elapsed().as_millis();
    let ended_at_unix_ms = unix_ms_now()?;

    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout UTF-8 decode failed for {}: {error}", spec.name))?;
    let stderr = String::from_utf8(output.stderr.clone())
        .map_err(|error| format!("stderr UTF-8 decode failed for {}: {error}", spec.name))?;
    let step_slug = sanitize_step_name(spec.name);
    let stdout_path = scenario_dir.join(format!("{step_slug}.stdout.json"));
    let stderr_path = scenario_dir.join(format!("{step_slug}.stderr.log"));
    write_text(&stdout_path, &stdout)?;
    write_text(&stderr_path, &stderr)?;

    let parsed_stdout = serde_json::from_str::<JsonValue>(&stdout).ok();
    let actual_schema = parsed_stdout.as_ref().and_then(schema_from_json);
    let schema_ok = actual_schema
        .as_deref()
        .is_some_and(|schema| schema.contains(spec.expected_schema_contains));
    let stderr_is_empty = stderr.is_empty();
    let exit_code = output.status.code().unwrap_or(-1);
    let first_failure = first_failure_diagnosis(
        exit_code,
        parsed_stdout.as_ref(),
        &stdout,
        &stderr,
        spec.expected_schema_contains,
    );

    Ok(CommandLog {
        schema: "ee.e2e.boundary_log.v1".to_owned(),
        subsystem: spec.subsystem.to_owned(),
        step_name: spec.name.to_owned(),
        command: "ee".to_owned(),
        args: spec.args.clone(),
        cwd: env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_owned()),
        workspace: workspace.display().to_string(),
        env_override_names: env_overrides
            .iter()
            .map(|(key, _)| (*key).to_owned())
            .collect(),
        env_sanitized: env_overrides
            .iter()
            .map(|(key, _)| SanitizedEnvOverride {
                name: (*key).to_owned(),
                value: "<redacted>".to_owned(),
            })
            .collect(),
        started_at_unix_ms,
        ended_at_unix_ms,
        elapsed_ms,
        exit_code,
        stdout_artifact_path: stdout_path.display().to_string(),
        stderr_artifact_path: stderr_path.display().to_string(),
        stdout_json_valid: parsed_stdout.is_some(),
        stderr_is_empty,
        schema_validation: SchemaValidation {
            status: if schema_ok {
                "passed".to_owned()
            } else {
                "failed".to_owned()
            },
            expected_contains: spec.expected_schema_contains.to_owned(),
            actual_schema,
        },
        golden_validation: GoldenValidation {
            status: "not_applicable".to_owned(),
            reason: "runtime scenario contains non-deterministic IDs/timestamps".to_owned(),
        },
        redaction_status: extract_redaction_status(parsed_stdout.as_ref()),
        evidence_ids: extract_evidence_ids(parsed_stdout.as_ref()),
        degradation_codes: extract_degradation_codes(parsed_stdout.as_ref()),
        mutation_summary: mutation_summary(spec),
        first_failure,
    })
}

fn read_logged_stdout_json(log: &CommandLog) -> Result<JsonValue, String> {
    let stdout = fs::read_to_string(&log.stdout_artifact_path).map_err(|error| {
        format!(
            "failed to read stdout artifact {}: {error}",
            log.stdout_artifact_path
        )
    })?;
    serde_json::from_str(&stdout).map_err(|error| {
        format!(
            "stdout artifact {} is not valid JSON: {error}",
            log.stdout_artifact_path
        )
    })
}

fn json_string_at(value: &JsonValue, pointer: &str, context: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context} missing string at {pointer}"))
}

fn json_string_array_at(
    value: &JsonValue,
    pointer: &str,
    context: &str,
) -> Result<Vec<String>, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{context} missing array at {pointer}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context} contains non-string item at {pointer}"))
        })
        .collect()
}

fn derived_asset<'a>(status_json: &'a JsonValue, name: &str) -> Result<&'a JsonValue, String> {
    status_json
        .pointer("/data/derivedAssets")
        .and_then(JsonValue::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .find(|asset| asset.get("name").and_then(JsonValue::as_str) == Some(name))
        })
        .ok_or_else(|| format!("status output missing derived asset {name}"))
}

#[test]
fn memory_revise_and_status_health_emit_logged_honest_contracts() -> TestResult {
    let scenario_id = "ee_6956_memory_revise_status_honesty";
    let scenario_dir = unique_scenario_dir(scenario_id)?;
    let workspace = scenario_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    let workspace_arg = workspace.display().to_string();
    let env_overrides = [
        ("EE_E2E_TRACE_LEVEL", "contract"),
        ("EE_E2E_REDACT", "strict"),
    ];

    let init_log = run_logged_step(
        &scenario_dir,
        &workspace,
        &env_overrides,
        &StepSpec {
            subsystem: "setup",
            name: "memory_status_init_workspace",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
    )?;
    ensure_equal(&init_log.exit_code, &0, "init exit code")?;
    ensure(init_log.stdout_json_valid, "init stdout JSON")?;
    ensure(init_log.stderr_is_empty, "init stderr clean")?;

    let remember_log = run_logged_step(
        &scenario_dir,
        &workspace,
        &env_overrides,
        &StepSpec {
            subsystem: "memory",
            name: "remember_original_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "Run cargo fmt --check before release.".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--confidence".to_owned(),
                "0.86".to_owned(),
                "--source".to_owned(),
                "file://AGENTS.md#L164-173".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
    )?;
    ensure_equal(&remember_log.exit_code, &0, "remember exit code")?;
    ensure(remember_log.stdout_json_valid, "remember stdout JSON")?;
    ensure(remember_log.stderr_is_empty, "remember stderr clean")?;
    let remember_json = read_logged_stdout_json(&remember_log)?;
    let original_memory_id = json_string_at(&remember_json, "/data/memory_id", "remember")?;
    ensure(
        original_memory_id.starts_with("mem_"),
        format!("unexpected memory id {original_memory_id}"),
    )?;

    let revise_dry_run_log = run_logged_step(
        &scenario_dir,
        &workspace,
        &env_overrides,
        &StepSpec {
            subsystem: "memory",
            name: "memory_revise_dry_run_preview",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "memory".to_owned(),
                "revise".to_owned(),
                original_memory_id.clone(),
                "--content".to_owned(),
                "Run cargo fmt --check and clippy before release.".to_owned(),
                "--confidence".to_owned(),
                "0.91".to_owned(),
                "--source".to_owned(),
                "file://README.md#L267".to_owned(),
                "--reason".to_owned(),
                "correction".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
    )?;
    ensure_equal(&revise_dry_run_log.exit_code, &0, "dry-run exit code")?;
    ensure(revise_dry_run_log.stdout_json_valid, "dry-run stdout JSON")?;
    ensure(revise_dry_run_log.stderr_is_empty, "dry-run stderr clean")?;
    let revise_dry_run_json = read_logged_stdout_json(&revise_dry_run_log)?;
    ensure_equal(
        &revise_dry_run_json["data"]["original_id"],
        &serde_json::json!(original_memory_id),
        "dry-run original memory id",
    )?;
    ensure_equal(
        &revise_dry_run_json["data"]["new_id"],
        &JsonValue::Null,
        "dry-run does not emit stub new_id",
    )?;
    ensure_equal(
        &revise_dry_run_json["data"]["revision_number"],
        &JsonValue::Null,
        "dry-run does not emit stub revision number",
    )?;
    let changed_fields =
        json_string_array_at(&revise_dry_run_json, "/data/changed_fields", "dry-run")?;
    ensure_equal(
        &changed_fields,
        &vec![
            "content".to_owned(),
            "confidence".to_owned(),
            "provenance_uri".to_owned(),
        ],
        "dry-run changed fields",
    )?;
    ensure_equal(
        &revise_dry_run_json["data"]["degraded"],
        &serde_json::json!(["revision_write_unavailable"]),
        "dry-run degraded code",
    )?;

    let revise_write_log = run_logged_step(
        &scenario_dir,
        &workspace,
        &env_overrides,
        &StepSpec {
            subsystem: "memory",
            name: "memory_revise_non_dry_run_policy_denied",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "memory".to_owned(),
                "revise".to_owned(),
                original_memory_id.clone(),
                "--content".to_owned(),
                "Run cargo fmt --check and clippy before release.".to_owned(),
            ],
            expected_schema_contains: "ee.error.v1",
            expected_exit_code: 8,
            expect_clean_stderr: true,
        },
    )?;
    ensure_equal(
        &revise_write_log.exit_code,
        &8,
        "non-dry-run policy exit code",
    )?;
    ensure(revise_write_log.stdout_json_valid, "policy stdout JSON")?;
    ensure(revise_write_log.stderr_is_empty, "policy stderr clean")?;
    ensure(
        revise_write_log
            .first_failure
            .as_ref()
            .is_some_and(|diagnosis| diagnosis == "error.code=policy_denied"),
        format!(
            "policy denial first-failure diagnosis missing: {:?}",
            revise_write_log.first_failure
        ),
    )?;
    let revise_write_json = read_logged_stdout_json(&revise_write_log)?;
    ensure_equal(
        &revise_write_json["error"]["code"],
        &serde_json::json!("policy_denied"),
        "policy denied error code",
    )?;

    let status_log = run_logged_step(
        &scenario_dir,
        &workspace,
        &env_overrides,
        &StepSpec {
            subsystem: "status",
            name: "status_after_memory_revise_preview",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "status".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
    )?;
    ensure_equal(&status_log.exit_code, &0, "status exit code")?;
    ensure(status_log.stdout_json_valid, "status stdout JSON")?;
    ensure(status_log.stderr_is_empty, "status stderr clean")?;
    let status_json = read_logged_stdout_json(&status_log)?;
    let memory_health = status_json
        .pointer("/data/memoryHealth")
        .ok_or_else(|| "status missing memoryHealth".to_owned())?;
    ensure(
        memory_health["status"] != serde_json::json!("unavailable"),
        "workspace status must read DB-backed memory health",
    )?;
    ensure_equal(
        &memory_health["totalCount"],
        &serde_json::json!(1),
        "memory health total count",
    )?;
    ensure_equal(
        &memory_health["activeCount"],
        &serde_json::json!(1),
        "memory health active count",
    )?;
    ensure_equal(
        &memory_health["tombstonedCount"],
        &serde_json::json!(0),
        "memory health tombstoned count",
    )?;
    ensure(
        memory_health["healthScore"].is_number(),
        "memory health score must be computed from real rows",
    )?;
    ensure(
        memory_health["scoreComponents"].is_object(),
        "memory health score components must be logged",
    )?;
    let search_asset = derived_asset(&status_json, "search_index")?;
    ensure(
        search_asset
            .get("status")
            .and_then(JsonValue::as_str)
            .is_some(),
        "search index status must be logged",
    )?;
    ensure(
        search_asset.get("sourceHighWatermark").is_some(),
        "source high watermark field must be logged",
    )?;
    ensure(
        search_asset.get("assetHighWatermark").is_some(),
        "asset high watermark field must be logged",
    )?;

    let database_path = workspace.join(".ee").join("ee.db");
    ensure(
        database_path.is_file(),
        "workspace database file must exist",
    )?;

    let commands = vec![
        init_log.clone(),
        remember_log.clone(),
        revise_dry_run_log.clone(),
        revise_write_log.clone(),
        status_log.clone(),
    ];
    for log in &commands {
        ensure(
            Path::new(&log.stdout_artifact_path).is_file(),
            format!("{} stdout artifact missing", log.step_name),
        )?;
        ensure(
            Path::new(&log.stderr_artifact_path).is_file(),
            format!("{} stderr artifact missing", log.step_name),
        )?;
    }

    let summary = serde_json::json!({
        "schema": "ee.e2e.memory_revise_status_log.v1",
        "scenarioId": scenario_id,
        "workspace": workspace.display().to_string(),
        "database": {
            "path": database_path.display().to_string(),
            "exists": database_path.is_file(),
        },
        "originalMemoryId": original_memory_id,
        "dryRun": {
            "newMemoryId": revise_dry_run_json["data"]["new_id"].clone(),
            "revisionNumber": revise_dry_run_json["data"]["revision_number"].clone(),
            "changedFields": changed_fields,
            "indexJobStatus": revise_dry_run_json["data"]["index_status"].clone(),
            "degraded": revise_dry_run_json["data"]["degraded"].clone(),
        },
        "nonDryRun": {
            "exitCode": revise_write_log.exit_code,
            "firstFailure": revise_write_log.first_failure.clone(),
            "errorCode": revise_write_json["error"]["code"].clone(),
        },
        "statusHealth": {
            "memoryHealth": memory_health.clone(),
            "searchIndex": search_asset.clone(),
        },
        "commands": commands,
    });
    let summary_path = scenario_dir.join("memory-revise-status-summary.json");
    let rendered_summary = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("failed to render summary JSON: {error}"))?;
    write_text(&summary_path, &format!("{rendered_summary}\n"))?;
    ensure(
        summary_path.is_file(),
        "memory revise status summary missing",
    )?;

    let parsed_summary: JsonValue = serde_json::from_str(&rendered_summary)
        .map_err(|error| format!("summary JSON parse failed: {error}"))?;
    ensure_equal(
        &parsed_summary["schema"],
        &serde_json::json!("ee.e2e.memory_revise_status_log.v1"),
        "summary schema",
    )?;
    ensure_equal(
        &parsed_summary["dryRun"]["newMemoryId"],
        &JsonValue::Null,
        "summary dry-run has no fake new memory",
    )?;
    ensure_equal(
        &parsed_summary["nonDryRun"]["errorCode"],
        &serde_json::json!("policy_denied"),
        "summary policy-denied code",
    )?;
    ensure(
        parsed_summary["statusHealth"]["memoryHealth"]["scoreComponents"].is_object(),
        "summary includes memory health components",
    )
}

#[test]
fn advanced_subsystems_emit_logged_json_contracts() -> TestResult {
    let scenario_id = "ee_tst_005_advanced_logged_bundle";
    let scenario_dir = unique_scenario_dir(scenario_id)?;
    let workspace = scenario_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    let workspace_arg = workspace.display().to_string();
    let env_overrides = [
        ("EE_E2E_TRACE_LEVEL", "contract"),
        ("EE_E2E_REDACT", "strict"),
    ];

    let init_spec = StepSpec {
        subsystem: "setup",
        name: "init_workspace",
        args: vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "init".to_owned(),
        ],
        expected_schema_contains: "ee.response.v1",
        expected_exit_code: 0,
        expect_clean_stderr: true,
    };
    let init_log = run_logged_step(&scenario_dir, &workspace, &env_overrides, &init_spec)?;
    ensure_equal(&init_log.exit_code, &0, "init exit code")?;
    ensure(init_log.stdout_json_valid, "init stdout must be valid JSON")?;
    ensure(
        init_log.schema_validation.status == "passed",
        format!(
            "init schema validation failed: {:?}",
            init_log.schema_validation
        ),
    )?;
    ensure(init_log.stderr_is_empty, "init stderr must be empty")?;

    let steps = vec![
        StepSpec {
            subsystem: "recorder",
            name: "recorder_start_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "recorder".to_owned(),
                "start".to_owned(),
                "--agent-id".to_owned(),
                "ee-tst-005-agent".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 7,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "preflight",
            name: "preflight_run",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "preflight".to_owned(),
                "run".to_owned(),
                "deploy production database migration".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 7,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "procedure",
            name: "procedure_list",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "procedure".to_owned(),
                "list".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 7,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "economy",
            name: "economy_prune_plan_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "economy".to_owned(),
                "prune-plan".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "learning",
            name: "learning_experiment_run_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "learn".to_owned(),
                "experiment".to_owned(),
                "run".to_owned(),
                "--id".to_owned(),
                "exp_database_contract_fixture".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.learn.experiment_run.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "causal",
            name: "causal_trace_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "causal".to_owned(),
                "trace".to_owned(),
                "--run-id".to_owned(),
                "run-test-001".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 7,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "causal",
            name: "causal_estimate_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace.display().to_string(),
                "--json".to_owned(),
                "causal".to_owned(),
                "estimate".to_owned(),
                "--artifact-id".to_owned(),
                "art-001".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 7,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "lab",
            name: "lab_capture_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace.display().to_string(),
                "--json".to_owned(),
                "lab".to_owned(),
                "capture".to_owned(),
                "--task-input".to_owned(),
                "prepare release with evidence-only lab replay".to_owned(),
                "--include-memories".to_owned(),
                "--include-actions".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "lab",
            name: "lab_replay_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace.display().to_string(),
                "--json".to_owned(),
                "lab".to_owned(),
                "replay".to_owned(),
                "ep_missing_evidence".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
        StepSpec {
            subsystem: "lab",
            name: "lab_counterfactual_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace.display().to_string(),
                "--json".to_owned(),
                "lab".to_owned(),
                "counterfactual".to_owned(),
                "ep_missing_evidence".to_owned(),
                "--add-memory".to_owned(),
                "mem_evidence_candidate".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_schema_contains: "ee.response.v1",
            expected_exit_code: 0,
            expect_clean_stderr: true,
        },
    ];

    let mut command_logs = Vec::with_capacity(steps.len());
    for spec in &steps {
        let log = run_logged_step(&scenario_dir, &workspace, &env_overrides, spec)?;
        ensure_equal(
            &log.exit_code,
            &spec.expected_exit_code,
            &format!("{} exit code", spec.name),
        )?;
        ensure(
            log.stdout_json_valid,
            format!("{} stdout must be valid JSON", spec.name),
        )?;
        ensure(
            log.schema_validation.status == "passed",
            format!(
                "{} schema validation failed: {:?}",
                spec.name, log.schema_validation
            ),
        )?;
        if spec.expect_clean_stderr {
            ensure(
                log.stderr_is_empty,
                format!("{} stderr must be empty in JSON mode", spec.name),
            )?;
        }
        ensure(
            Path::new(&log.stdout_artifact_path).is_file(),
            format!("{} stdout artifact missing", spec.name),
        )?;
        ensure(
            Path::new(&log.stderr_artifact_path).is_file(),
            format!("{} stderr artifact missing", spec.name),
        )?;
        command_logs.push(log);
    }

    let subsystems_covered: Vec<String> = command_logs
        .iter()
        .map(|entry| entry.subsystem.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ensure_equal(
        &subsystems_covered,
        &vec![
            "causal".to_owned(),
            "economy".to_owned(),
            "lab".to_owned(),
            "learning".to_owned(),
            "preflight".to_owned(),
            "procedure".to_owned(),
            "recorder".to_owned(),
        ],
        "subsystems covered",
    )?;

    let summary = ScenarioSummary {
        schema: "ee.e2e.advanced_subsystems_log.v1".to_owned(),
        scenario_id: scenario_id.to_owned(),
        workspace: workspace.display().to_string(),
        command_count: command_logs.len(),
        subsystems_covered,
        environment_overrides: env_overrides
            .iter()
            .map(|(key, _)| (*key).to_owned())
            .collect(),
        commands: command_logs.clone(),
        validation: ScenarioValidation {
            schema_validation: "all_passed".to_owned(),
            golden_validation: "not_applicable_runtime_scenario".to_owned(),
            stdout_stderr_isolation: "json_stdout_and_clean_stderr".to_owned(),
        },
    };

    let summary_path = scenario_dir.join("scenario-summary.json");
    let rendered_summary = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("failed to render summary JSON: {error}"))?;
    write_text(&summary_path, &format!("{rendered_summary}\n"))?;
    ensure(summary_path.is_file(), "scenario summary file missing")?;

    let parsed_summary: JsonValue = serde_json::from_str(&rendered_summary)
        .map_err(|error| format!("summary JSON parse failed: {error}"))?;
    ensure_equal(
        &parsed_summary["schema"],
        &serde_json::json!("ee.e2e.advanced_subsystems_log.v1"),
        "summary schema",
    )?;
    ensure_equal(
        &parsed_summary["command_count"],
        &serde_json::json!(10),
        "summary command count",
    )?;
    let commands = parsed_summary["commands"]
        .as_array()
        .ok_or_else(|| "summary commands missing".to_string())?;
    let lab_capture = commands
        .iter()
        .find(|entry| entry["step_name"] == serde_json::json!("lab_capture_dry_run"))
        .ok_or_else(|| "lab capture command missing".to_string())?;
    ensure_equal(
        &lab_capture["redaction_status"],
        &serde_json::json!("redacted"),
        "lab capture redaction status",
    )?;
    ensure(
        lab_capture["evidence_ids"]
            .as_array()
            .is_some_and(|ids| !ids.is_empty()),
        "lab capture records evidence IDs",
    )?;
    ensure(
        commands
            .iter()
            .filter(|entry| {
                entry["step_name"] == serde_json::json!("lab_replay_dry_run")
                    || entry["step_name"] == serde_json::json!("lab_counterfactual_dry_run")
            })
            .all(|entry| {
                entry["degradation_codes"]
                    .as_array()
                    .is_some_and(|codes| codes.iter().any(|code| code == "lab_replay_unavailable"))
            }),
        "lab replay/counterfactual commands must record the missing replay evidence code",
    )?;
    ensure(
        parsed_summary["commands"]
            .as_array()
            .is_some_and(|commands| {
                commands
                    .iter()
                    .all(|entry| entry["first_failure"].is_null())
            }),
        "successful scenario commands must not report first-failure diagnoses",
    )?;
    ensure(
        parsed_summary["commands"]
            .as_array()
            .is_some_and(|commands| {
                commands.iter().all(|entry| {
                    entry["schema"] == serde_json::json!("ee.e2e.boundary_log.v1")
                        && entry["started_at_unix_ms"].is_number()
                        && entry["ended_at_unix_ms"].is_number()
                        && entry["env_sanitized"].is_array()
                        && entry["evidence_ids"].is_array()
                        && entry["degradation_codes"].is_array()
                        && entry["mutation_summary"].is_string()
                })
            }),
        "logged commands must include boundary migration contract fields",
    )
}

#[test]
fn advanced_subsystem_failure_log_captures_first_failure_diagnosis() -> TestResult {
    let scenario_dir = unique_scenario_dir("ee_tst_005_advanced_failure_diagnosis")?;
    let workspace = scenario_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();

    let failure_step = StepSpec {
        subsystem: "economy",
        name: "economy_prune_plan_without_dry_run",
        args: vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "economy".to_owned(),
            "prune-plan".to_owned(),
        ],
        expected_schema_contains: "ee.error.v1",
        expected_exit_code: 8,
        expect_clean_stderr: true,
    };

    let log = run_logged_step(&scenario_dir, &workspace, &[], &failure_step)?;
    ensure_equal(&log.exit_code, &8, "failure exit code")?;
    ensure(log.stdout_json_valid, "failure stdout must be valid JSON")?;
    ensure(
        log.schema_validation.status == "passed",
        format!(
            "failure schema validation failed: {:?}",
            log.schema_validation
        ),
    )?;
    ensure(log.stderr_is_empty, "failure JSON stderr must stay empty")?;
    ensure(
        log.first_failure
            .as_ref()
            .is_some_and(|diagnosis| diagnosis.contains("policy_denied")),
        format!(
            "first failure diagnosis must include policy_denied, got {:?}",
            log.first_failure
        ),
    )
}

#[test]
fn boundary_logger_detects_schema_mismatch_on_real_command() -> TestResult {
    let scenario_dir = unique_scenario_dir("ee_boundary_schema_mismatch")?;
    let workspace = scenario_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    let mismatch_step = StepSpec {
        subsystem: "boundary",
        name: "status_schema_mismatch_probe",
        args: vec![
            "--workspace".to_owned(),
            workspace.display().to_string(),
            "--json".to_owned(),
            "status".to_owned(),
        ],
        expected_schema_contains: "ee.not_the_status_schema.v1",
        expected_exit_code: 0,
        expect_clean_stderr: true,
    };

    let log = run_logged_step(&scenario_dir, &workspace, &[], &mismatch_step)?;
    ensure_equal(&log.exit_code, &0, "schema mismatch probe exit code")?;
    ensure(log.stdout_json_valid, "schema mismatch probe stdout JSON")?;
    ensure_equal(
        &log.schema_validation.status,
        &"failed".to_owned(),
        "schema mismatch validation status",
    )?;
    ensure(
        log.first_failure
            .as_ref()
            .is_some_and(|diagnosis| diagnosis.starts_with("schema_mismatch:")),
        format!(
            "schema mismatch must be first failure, got {:?}",
            log.first_failure
        ),
    )
}

#[test]
fn boundary_logger_detects_stdout_pollution() -> TestResult {
    let diagnosis = first_failure_diagnosis(
        0,
        None,
        "progress: loading index\n{\"schema\":\"ee.response.v1\"}\n",
        "",
        "ee.response.v1",
    );

    ensure_equal(
        &diagnosis,
        &Some("stdout_pollution".to_owned()),
        "stdout pollution diagnosis",
    )
}
