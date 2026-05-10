use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::core::degraded_honesty::{
    validate_no_fake_success_output, validate_no_unsupported_evidence_claims,
    validate_repair_command,
};
use ee::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const UNSATISFIED_DEGRADED_MODE_EXIT: i32 = 6;

struct LoggedCommand {
    stdout: String,
    stderr: String,
    exit_code: i32,
    parsed: Value,
    log_path: PathBuf,
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
    if actual.eq(expected) {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_json_pointer(value: &Value, pointer: &str, expected: Value, context: &str) -> TestResult {
    let actual = value
        .pointer(pointer)
        .ok_or_else(|| format!("{context}: missing JSON pointer {pointer}"))?;
    ensure_equal(actual, &expected, context)
}

fn ensure_no_ansi(text: &str, context: &str) -> TestResult {
    ensure(
        !text.contains("\u{1b}["),
        format!("{context} must not contain ANSI styling"),
    )
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX_EPOCH: {error}"))?
        .as_nanos();
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("degraded-honesty")
        .join(format!("{}-{}-{nanos}", name, std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create artifact dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn collect_degradation_codes(value: &Value) -> Vec<String> {
    let mut codes = Vec::new();

    if let Some(code) = value.pointer("/error/code").and_then(Value::as_str) {
        codes.push(code.to_owned());
    }

    for pointer in [
        "/degraded",
        "/data/degraded",
        "/degradations",
        "/data/degradations",
    ] {
        if let Some(items) = value.pointer(pointer).and_then(Value::as_array) {
            for item in items {
                if let Some(code) = item.get("code").and_then(Value::as_str) {
                    codes.push(code.to_owned());
                }
            }
        }
    }

    for pointer in ["/degradationCodes", "/data/degradationCodes"] {
        if let Some(items) = value.pointer(pointer).and_then(Value::as_array) {
            for item in items {
                if let Some(code) = item.as_str() {
                    codes.push(code.to_owned());
                }
            }
        }
    }

    if let Some(items) = value.pointer("/data/warnings").and_then(Value::as_array) {
        for item in items {
            if let Some((code, _)) = item.as_str().and_then(|warning| warning.split_once(':')) {
                if code.ends_with("_unavailable") {
                    codes.push(code.to_owned());
                }
            }
        }
    }

    codes.sort();
    codes.dedup();
    codes
}

fn first_repair_command(value: &Value) -> Option<String> {
    value
        .pointer("/error/repair")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/data/degraded/0/repair")
                .and_then(Value::as_str)
        })
        .or_else(|| value.pointer("/degraded/0/repair").and_then(Value::as_str))
        .map(str::to_owned)
}

fn command_boundary_matrix_row(args: &[String]) -> &'static str {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "context" | "pack" | "search" | "why"))
    {
        "context, pack, search, why"
    } else if args.iter().any(|arg| arg == "graph") {
        "graph"
    } else if args.iter().any(|arg| arg == "audit") {
        "audit"
    } else if args.iter().any(|arg| arg == "memory") {
        "memory, remember"
    } else if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "curate" | "rule"))
    {
        "curate"
    } else if args.iter().any(|arg| arg == "status") {
        "capabilities, check, health, status"
    } else if args.iter().any(|arg| arg == "support") {
        "support bundle"
    } else if args.windows(2).any(
        |window| matches!(window, [first, second] if first == "diag" && second == "quarantine"),
    ) {
        "diag quarantine"
    } else if args.iter().any(|arg| arg == "capabilities") {
        "capabilities, check, health, status"
    } else if args.iter().any(|arg| arg == "certificate") {
        "certificate"
    } else if args.iter().any(|arg| arg == "claim") {
        "claim"
    } else if args.iter().any(|arg| arg == "rehearse") {
        "rehearse"
    } else if args.iter().any(|arg| arg == "learn") {
        "learn"
    } else if args.iter().any(|arg| arg == "lab") {
        "lab"
    } else if args.iter().any(|arg| arg == "economy") {
        "economy"
    } else if args.iter().any(|arg| arg == "causal") {
        "causal"
    } else if args.iter().any(|arg| arg == "procedure") {
        "procedure"
    } else if args.iter().any(|arg| arg == "situation") {
        "situation"
    } else if args.iter().any(|arg| arg == "plan") {
        "plan"
    } else if args.iter().any(|arg| arg == "preflight") {
        "preflight"
    } else if args.iter().any(|arg| arg == "tripwire") {
        "tripwire"
    } else if args.iter().any(|arg| arg == "eval") {
        "eval"
    } else if args.iter().any(|arg| arg == "review") {
        "review"
    } else if args.iter().any(|arg| arg == "handoff") {
        "handoff"
    } else if args.iter().any(|arg| arg == "daemon") {
        "daemon"
    } else if args.iter().any(|arg| arg == "recorder") {
        "recorder"
    } else if args.iter().any(|arg| arg == "demo") {
        "demo"
    } else {
        "unknown"
    }
}

fn side_effect_class(args: &[String]) -> &'static str {
    if args.iter().any(|arg| arg == "context" || arg == "pack") {
        "audited pack write when storage is available; storage error before mutation here"
    } else if args.iter().any(|arg| arg == "search" || arg == "why") {
        "read-only query/explanation; storage or search error before reasoning output"
    } else if args.iter().any(|arg| arg == "graph") {
        "derived graph read/rebuild only; source database remains unchanged on missing storage"
    } else if args.iter().any(|arg| arg == "audit") {
        "read-only persisted audit query or hash-chain verification; no audit log mutation"
    } else if args.iter().any(|arg| arg == "memory") {
        "audited memory mutation only when storage is available; missing storage prevents mutation"
    } else if args.iter().any(|arg| arg == "curate" || arg == "rule") {
        "audited curation/rule mutation only when storage is available; missing storage prevents mutation"
    } else if args.iter().any(|arg| arg == "status") {
        "read-only capability probe; no workspace, database, index, job, or adapter mutation"
    } else if args.iter().any(|arg| arg == "support") {
        if args.iter().any(|arg| arg == "inspect") {
            "read-only support bundle verification; no bundle mutation"
        } else if args.iter().any(|arg| arg == "--dry-run") {
            "dry-run support bundle plan; no archive or manifest files written"
        } else {
            "side-path support bundle artifact write with redaction and manifest verification"
        }
    } else if args.windows(2).any(
        |window| matches!(window, [first, second] if first == "diag" && second == "quarantine"),
    ) {
        "read-only persisted trust-state query; no source trust mutation"
    } else if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "capabilities" | "certificate" | "claim"))
    {
        "read-only, idempotent"
    } else if args
        .windows(2)
        .any(|window| matches!(window, [first, second] if first == "rehearse" && second == "run"))
    {
        "sandboxed rehearsal side-path artifact write; source workspace unchanged"
    } else if args.iter().any(|arg| arg == "rehearse") {
        "read-only rehearsal planning, inspection, or promotion guidance"
    } else if args.iter().any(|arg| arg == "learn") {
        if args.iter().any(|arg| arg == "propose") {
            "audited learning proposal candidate writes when storage is available"
        } else if args.iter().any(|arg| arg == "run") {
            "conservative abstention; no experiment execution without persisted registry input"
        } else {
            "read-only persisted learning ledger report"
        }
    } else if args.iter().any(|arg| arg == "lab") {
        "evidence-only lab report; no behavior inference or durable mutation"
    } else if args.iter().any(|arg| arg == "economy") {
        "read-only, conservative abstention"
    } else if args.iter().any(|arg| arg == "causal") {
        "persisted causal-evidence query; promote-plan writes audited curation candidates when storage is available"
    } else if args.iter().any(|arg| arg == "procedure") {
        "read-only persisted procedure query; no procedure mutation or artifact write"
    } else if args.iter().any(|arg| arg == "situation") {
        "read-only heuristic situation classification; no situation link, routing-state, or recommendation mutation"
    } else if args.iter().any(|arg| arg == "plan") {
        "read-only recipe recommendation/explanation; no plan mutation"
    } else if args.iter().any(|arg| arg == "preflight") {
        if args.iter().any(|arg| arg == "show") {
            "read-only persisted preflight run lookup"
        } else if args.iter().any(|arg| arg == "--dry-run") {
            "dry-run preflight report; no preflight run store mutation"
        } else {
            "workspace-local preflight run store mutation with evidence-backed tripwires"
        }
    } else if args.iter().any(|arg| arg == "tripwire") {
        if args.iter().any(|arg| arg == "check") && !args.iter().any(|arg| arg == "--dry-run") {
            "persisted tripwire check event mutation when the tripwire exists"
        } else {
            "read-only tripwire store query/evaluation; no tripwire mutation"
        }
    } else if args.iter().any(|arg| arg == "eval") {
        "read-only fixture discovery/evaluation report generation; no durable mutation"
    } else if args.iter().any(|arg| arg == "review") {
        "audited curation candidate mutation only when storage is available; missing storage prevents review"
    } else if args.iter().any(|arg| arg == "handoff") {
        "conservative abstention; no continuity capsule write"
    } else if args.iter().any(|arg| arg == "daemon") {
        "foreground daemon writes supervised job rows and runs bounded maintenance handlers"
    } else if args.iter().any(|arg| arg == "recorder") {
        if args.iter().any(|arg| arg == "tail" || arg == "follow") {
            "read-only recorder event stream; no recorder mutation"
        } else if args.iter().any(|arg| arg == "--dry-run") {
            "dry-run recorder report; no recorder store mutation"
        } else {
            "persisted recorder session, event, or finalization mutation through recorder store"
        }
    } else if args.iter().any(|arg| arg == "demo") {
        if args.iter().any(|arg| arg == "verify") {
            "read-only artifact verification; no command execution or artifact write"
        } else if args.iter().any(|arg| arg == "run") && args.iter().any(|arg| arg == "--dry-run") {
            "dry-run plan; no command execution, audit ledger, or artifact write"
        } else if args.iter().any(|arg| arg == "run") {
            "command execution with audit ledger rows and evidence artifacts"
        } else if args.iter().any(|arg| arg == "show") {
            "read-only audit ledger inspection; no demo mutation"
        } else {
            "read-only manifest parse; no command execution or artifact write"
        }
    } else {
        "unknown"
    }
}

fn success_flag(value: &Value) -> bool {
    value
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn ensure_no_fake_or_unsupported_claims(
    command: &str,
    success: bool,
    fixture_mode: bool,
    stdout: &str,
) -> TestResult {
    let fake_success = validate_no_fake_success_output(command, success, fixture_mode, stdout);
    ensure(
        fake_success.passed,
        format!("{command} output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims(command, success, fixture_mode, stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "{command} output should not overclaim unsupported reasoning: {unsupported_claims:?}"
        ),
    )
}

fn ensure_repair_command_is_actionable(value: &Value, context: &str) -> TestResult {
    if let Some(repair) = first_repair_command(value) {
        let report = validate_repair_command(&repair);
        ensure(
            report.passed,
            format!("{context} repair command must be actionable: {report:?}"),
        )?;
    }
    Ok(())
}

fn ensure_logged_contract_shape(result: &LoggedCommand, context: &str) -> TestResult {
    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("{context} e2e log must be JSON: {error}"))?;

    ensure_json_pointer(
        &log_json,
        "/schema",
        json!("ee.degraded_honesty.e2e_log.v1"),
        &format!("{context} e2e log schema"),
    )?;
    ensure(
        log_json
            .pointer("/stdoutPath")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("stdout.json")),
        format!("{context} log must record stdout artifact path"),
    )?;
    ensure(
        log_json
            .pointer("/stderrPath")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("stderr.txt")),
        format!("{context} log must record stderr artifact path"),
    )?;
    ensure(
        log_json
            .pointer("/parsedJsonSchema")
            .and_then(Value::as_str)
            .is_some_and(|schema| schema.starts_with("ee.")),
        format!("{context} log must record parsed JSON schema"),
    )?;
    ensure(
        log_json
            .pointer("/commandBoundaryMatrixRow")
            .and_then(Value::as_str)
            .is_some_and(|row| row != "unknown"),
        format!("{context} log must map to a command-boundary matrix row"),
    )?;
    ensure(
        log_json
            .pointer("/sideEffectClass")
            .and_then(Value::as_str)
            .is_some_and(|side_effect| side_effect != "unknown"),
        format!("{context} log must record a side-effect summary"),
    )?;

    Ok(())
}

fn run_ee_logged(
    name: &str,
    workspace: Option<&Path>,
    args: Vec<String>,
) -> Result<LoggedCommand, String> {
    run_ee_logged_with_env(name, workspace, args, &[])
}

fn run_ee_logged_with_env(
    name: &str,
    workspace: Option<&Path>,
    args: Vec<String>,
    env_overrides: &[(&str, String)],
) -> Result<LoggedCommand, String> {
    let dossier_dir = unique_artifact_dir(name)?;
    let stdout_path = dossier_dir.join("stdout.json");
    let stderr_path = dossier_dir.join("stderr.txt");
    let log_path = dossier_dir.join("e2e-log.json");
    let cwd = env::current_dir().map_err(|error| format!("failed to resolve cwd: {error}"))?;

    let start = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(&args).current_dir(&cwd);
    for (key, value) in env_overrides {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run ee {:?}: {error}", args))?;
    let elapsed = start.elapsed();

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not valid UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not valid UTF-8: {error}"))?;
    fs::write(&stdout_path, &stdout)
        .map_err(|error| format!("failed to write {}: {error}", stdout_path.display()))?;
    fs::write(&stderr_path, &stderr)
        .map_err(|error| format!("failed to write {}: {error}", stderr_path.display()))?;

    let parsed_result: Result<Value, _> = serde_json::from_str(&stdout);
    let (parsed_json_schema, degradation_codes, first_failure_diagnosis) = match &parsed_result {
        Ok(value) => (
            value
                .get("schema")
                .and_then(Value::as_str)
                .unwrap_or("<missing>")
                .to_owned(),
            collect_degradation_codes(value),
            if stderr.is_empty() {
                None
            } else {
                Some("stderr_not_empty".to_owned())
            },
        ),
        Err(error) => (
            "<invalid-json>".to_owned(),
            Vec::new(),
            Some(format!("stdout_json_parse_failed: {error}")),
        ),
    };
    let repair_command = parsed_result.as_ref().ok().and_then(first_repair_command);

    let exit_code = output.status.code().unwrap_or(-1);
    let log = json!({
        "schema": "ee.degraded_honesty.e2e_log.v1",
        "command": "ee",
        "argv": args,
        "cwd": cwd.display().to_string(),
        "workspace": workspace.map(|path| path.display().to_string()),
        "env": {
            "CARGO_TARGET_DIR": env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "<unset>".to_owned()),
            "RUST_LOG": "<redacted>"
        },
        "elapsedMs": elapsed.as_millis(),
        "exitCode": exit_code,
        "stdoutPath": stdout_path.display().to_string(),
        "stderrPath": stderr_path.display().to_string(),
        "parsedJsonSchema": parsed_json_schema,
        "goldenValidation": "not_applicable",
        "redactionStatus": "not_applicable",
        "evidenceIds": [],
        "degradationCodes": degradation_codes,
        "repairCommand": repair_command,
        "commandBoundaryMatrixRow": command_boundary_matrix_row(&args),
        "sideEffectClass": side_effect_class(&args),
        "firstFailureDiagnosis": first_failure_diagnosis
    });
    let mut log_text = serde_json::to_string_pretty(&log)
        .map_err(|error| format!("failed to serialize e2e log: {error}"))?;
    log_text.push('\n');
    fs::write(&log_path, log_text)
        .map_err(|error| format!("failed to write {}: {error}", log_path.display()))?;

    let parsed = parsed_result.map_err(|error| format!("stdout must be JSON: {error}"))?;
    Ok(LoggedCommand {
        stdout,
        stderr,
        exit_code,
        parsed,
        log_path,
    })
}

fn init_workspace(workspace: &Path, context: &str) -> TestResult {
    let workspace_arg = workspace.display().to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["--workspace", &workspace_arg, "--json", "init"])
        .output()
        .map_err(|error| format!("failed to initialize {context} workspace: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "{context} workspace init failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[test]
fn context_without_database_uses_honest_error_envelope_and_e2e_log() -> TestResult {
    let workspace_root = unique_artifact_dir("context-missing-db-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();

    let result = run_ee_logged(
        "context-missing-db",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "context".to_owned(),
            "prepare release".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &3, "storage error exit code")?;
    ensure(
        result.stderr.is_empty(),
        "JSON error path must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "context error stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.error.v1"),
        "error envelope schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/error/code",
        json!("storage"),
        "error code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/error/repair",
        json!("ee init --workspace ."),
        "repair command",
    )?;

    let fake_success = validate_no_fake_success_output("context", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("failed commands should not be treated as fake success: {fake_success:?}"),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/schema",
        json!("ee.degraded_honesty.e2e_log.v1"),
        "e2e log schema",
    )?;
    ensure_json_pointer(&log_json, "/exitCode", json!(3), "logged exit code")?;
    ensure_json_pointer(
        &log_json,
        "/parsedJsonSchema",
        json!("ee.error.v1"),
        "logged parsed schema",
    )?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["storage"]),
        "logged degradation/error code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee init --workspace ."),
        "logged repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("context, pack, search, why"),
        "logged command-boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("audited pack write when storage is available; storage error before mutation here"),
        "logged side-effect class",
    )
}

#[test]
fn successful_capabilities_output_has_no_fake_success_markers() -> TestResult {
    let result = run_ee_logged(
        "capabilities-success",
        None,
        vec!["--json".to_owned(), "capabilities".to_owned()],
    )?;

    ensure_equal(&result.exit_code, &0, "capabilities exit code")?;
    ensure(
        result.stderr.is_empty(),
        "capabilities stderr must be empty",
    )?;
    ensure_no_ansi(&result.stdout, "capabilities stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "response envelope schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(true), "success flag")?;

    let fake_success = validate_no_fake_success_output("capabilities", true, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("capabilities output contains fake success marker: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("capabilities", true, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!("capabilities output contains unsupported evidence claim: {unsupported_claims:?}"),
    )
}

#[test]
fn retrieval_graph_memory_curate_rule_and_status_commands_have_no_fake_contract_logs() -> TestResult
{
    let workspace_root = unique_artifact_dir("boundary-contract-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();
    let query_file = workspace.join("task.eeq.json");
    fs::write(
        &query_file,
        r#"{
          "version": "ee.query.v1",
          "query": {"text": "prepare release", "mode": "hybrid"},
          "budget": {"maxTokens": 1200, "candidatePool": 8},
          "output": {"format": "json", "profile": "compact"}
        }"#,
    )
    .map_err(|error| format!("failed to write {}: {error}", query_file.display()))?;
    let query_file_arg = query_file.display().to_string();

    let status_result = run_ee_logged(
        "status-success-contract",
        None,
        vec!["--json".to_owned(), "status".to_owned()],
    )?;
    ensure_equal(&status_result.exit_code, &0, "status exit code")?;
    ensure(
        status_result.stderr.is_empty(),
        "status JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&status_result.stdout, "status stdout")?;
    ensure_no_fake_or_unsupported_claims(
        "status",
        success_flag(&status_result.parsed),
        false,
        &status_result.stdout,
    )?;
    ensure_logged_contract_shape(&status_result, "status")?;

    let cases = [
        (
            "search-missing-db-contract",
            "search",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "search".to_owned(),
                "prepare release".to_owned(),
            ],
        ),
        (
            "pack-missing-db-contract",
            "pack",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "--query-file".to_owned(),
                query_file_arg,
            ],
        ),
        (
            "why-missing-db-contract",
            "why",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "why".to_owned(),
                "mem_00000000000000000000000000".to_owned(),
            ],
        ),
        (
            "graph-export-missing-db-contract",
            "graph export",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "graph".to_owned(),
                "export".to_owned(),
            ],
        ),
        (
            "graph-neighborhood-missing-db-contract",
            "graph neighborhood",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "graph".to_owned(),
                "neighborhood".to_owned(),
                "mem_00000000000000000000000000".to_owned(),
            ],
        ),
        (
            "memory-revise-missing-db-contract",
            "memory revise",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "memory".to_owned(),
                "revise".to_owned(),
                "mem_00000000000000000000000000".to_owned(),
                "--content".to_owned(),
                "Corrected memory text".to_owned(),
                "--dry-run".to_owned(),
            ],
        ),
        (
            "curate-candidates-missing-db-contract",
            "curate candidates",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "curate".to_owned(),
                "candidates".to_owned(),
            ],
        ),
        (
            "rule-list-missing-db-contract",
            "rule list",
            vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "rule".to_owned(),
                "list".to_owned(),
            ],
        ),
    ];

    for (artifact_name, command, args) in cases {
        let result = run_ee_logged(artifact_name, Some(&workspace), args)?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} stdout"))?;
        ensure(
            result.exit_code != 0 || success_flag(&result.parsed),
            format!("{command} successful exit must set success=true"),
        )?;
        ensure_no_fake_or_unsupported_claims(
            command,
            success_flag(&result.parsed),
            false,
            &result.stdout,
        )?;
        ensure_repair_command_is_actionable(&result.parsed, command)?;
        ensure_logged_contract_shape(&result, command)?;
    }

    Ok(())
}

fn seed_audit_cli_workspace(name: &str) -> Result<(PathBuf, PathBuf), String> {
    let root = unique_artifact_dir(name)?;
    let workspace = root.join("workspace");
    let database = workspace.join(".ee").join("ee.db");
    fs::create_dir_all(
        database
            .parent()
            .ok_or_else(|| format!("database path {} has no parent", database.display()))?,
    )
    .map_err(|error| {
        format!(
            "failed to create database parent for {}: {error}",
            database.display()
        )
    })?;

    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            "wsp_auditcli000000000000000001",
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: Some("audit-cli-contract".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            "mem_auditcli000000000000000001",
            &CreateMemoryInput {
                workspace_id: "wsp_auditcli000000000000000001".to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Run cargo fmt --check before release.".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("file://AGENTS.md".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: Some("test".to_owned()),
                valid_from: None,
                valid_to: None,
                tags: vec!["audit".to_owned()],
            },
        )
        .map_err(|error| error.to_string())?;

    for (id, action, target_type, target_id, actor) in [
        (
            "audit_cli00000000000000000000001",
            "memory.create",
            "memory",
            "mem_auditcli000000000000000001",
            "cod_2",
        ),
        (
            "audit_cli00000000000000000000002",
            "rule.protect",
            "rule",
            "rule_auditcli000000000000000001",
            "cod_2",
        ),
    ] {
        connection
            .insert_audit(
                id,
                &CreateAuditInput {
                    workspace_id: Some("wsp_auditcli000000000000000001".to_owned()),
                    actor: Some(actor.to_owned()),
                    action: action.to_owned(),
                    target_type: Some(target_type.to_owned()),
                    target_id: Some(target_id.to_owned()),
                    details: Some(json!({ "action": action, "target": target_id }).to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
    }

    connection.close().map_err(|error| error.to_string())?;
    Ok((workspace, database))
}

#[test]
fn audit_commands_read_persisted_rows_without_unavailable_sentinel() -> TestResult {
    let (workspace, database) = seed_audit_cli_workspace("audit-real-cli")?;
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let cases = [
        (
            "audit-timeline-real",
            "audit timeline",
            "ee.audit.timeline.v1",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "audit".to_owned(),
                "timeline".to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
                "--surface".to_owned(),
                "memory".to_owned(),
            ],
        ),
        (
            "audit-show-real",
            "audit show",
            "ee.audit.show.v1",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "audit".to_owned(),
                "show".to_owned(),
                "audit_cli00000000000000000000001".to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
            ],
        ),
        (
            "audit-diff-real",
            "audit diff",
            "ee.audit.diff.v1",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "audit".to_owned(),
                "diff".to_owned(),
                "2000-01-01T00:00:00Z".to_owned(),
                "2999-01-01T00:00:00Z".to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
            ],
        ),
        (
            "audit-verify-real",
            "audit verify",
            "ee.audit.verify.v1",
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace_arg,
                "audit".to_owned(),
                "verify".to_owned(),
                "--database".to_owned(),
                database_arg,
            ],
        ),
    ];

    for (artifact_name, command, schema, args) in cases {
        let result = run_ee_logged(artifact_name, Some(&workspace), args)?;

        ensure_equal(
            &result.exit_code,
            &0,
            &format!("{command} persisted audit exit code"),
        )?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} stdout"))?;
        ensure(
            !result.stdout.contains("audit_log_unavailable"),
            format!("{command} must not emit the removed unavailable sentinel"),
        )?;
        ensure(
            collect_degradation_codes(&result.parsed).is_empty(),
            format!("{command} must not emit degradation codes"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!(schema),
            &format!("{command} schema"),
        )?;

        match command {
            "audit timeline" => {
                ensure_json_pointer(
                    &result.parsed,
                    "/entries/0/surface",
                    json!("memory"),
                    "timeline filtered surface",
                )?;
                ensure(
                    result
                        .parsed
                        .pointer("/entries/0/this_row_hash")
                        .and_then(Value::as_str)
                        .is_some_and(|hash| hash.starts_with("blake3:")),
                    "timeline entry must expose persisted row hash",
                )?;
            }
            "audit show" => {
                ensure_json_pointer(
                    &result.parsed,
                    "/linked_snapshot/found",
                    json!(true),
                    "show linked memory snapshot",
                )?;
                ensure_json_pointer(
                    &result.parsed,
                    "/hash_chain_valid",
                    json!(true),
                    "show verifies the surrounding hash chain",
                )?;
            }
            "audit diff" => {
                ensure_json_pointer(&result.parsed, "/row_count", json!(2), "diff row count")?;
            }
            "audit verify" => {
                ensure_json_pointer(
                    &result.parsed,
                    "/integrity_ok",
                    json!(true),
                    "verify integrity flag",
                )?;
                ensure_json_pointer(&result.parsed, "/rows", json!(2), "verify row count")?;
            }
            other => return Err(format!("unknown audit command case: {other}")),
        }

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!([]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("audit"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!(
                "read-only persisted audit query or hash-chain verification; no audit log mutation"
            ),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
}

#[test]
fn support_bundle_commands_create_real_bundles_with_redacted_diagnostics() -> TestResult {
    let artifact_dir = unique_artifact_dir("support-bundle-real-implementation")?;
    let out_dir = artifact_dir.join("bundle-output");
    fs::create_dir_all(&out_dir)
        .map_err(|error| format!("failed to create {}: {error}", out_dir.display()))?;

    let result = run_ee_logged(
        "support-bundle-dry-run",
        None,
        vec![
            "--json".to_owned(),
            "support".to_owned(),
            "bundle".to_owned(),
            "--dry-run".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &0, "support bundle dry-run exit code")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "support bundle dry-run schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/success",
        json!(true),
        "support bundle dry-run success",
    )?;
    ensure(
        result.parsed.pointer("/data/filesCollected").is_some(),
        "support bundle dry-run must report files to collect".to_owned(),
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/dryRun",
        json!(true),
        "support bundle dry-run must report dryRun=true",
    )?;

    let result = run_ee_logged(
        "support-bundle-create",
        None,
        vec![
            "--json".to_owned(),
            "support".to_owned(),
            "bundle".to_owned(),
            "--out".to_owned(),
            out_dir.display().to_string(),
        ],
    )?;

    ensure_equal(&result.exit_code, &0, "support bundle create exit code")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "support bundle create schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/success",
        json!(true),
        "support bundle create success",
    )?;
    ensure(
        result.parsed.pointer("/data/outputPath").is_some(),
        "support bundle create must report output path".to_owned(),
    )?;
    ensure(
        result.parsed.pointer("/data/manifestHash").is_some(),
        "support bundle create must report manifest hash".to_owned(),
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/dryRun",
        json!(false),
        "support bundle create must report dryRun=false",
    )?;

    let output_path = result
        .parsed
        .pointer("/data/outputPath")
        .and_then(|v| v.as_str())
        .ok_or("outputPath must be a string")?;
    let bundle_dir = Path::new(output_path);

    ensure(
        bundle_dir.is_dir(),
        format!("bundle directory {} must exist", bundle_dir.display()),
    )?;
    ensure(
        bundle_dir.join("manifest.json").is_file(),
        "bundle must contain manifest.json".to_owned(),
    )?;
    ensure(
        bundle_dir.join("status.json").is_file(),
        "bundle must contain status.json".to_owned(),
    )?;
    ensure(
        bundle_dir.join("doctor.json").is_file(),
        "bundle must contain doctor.json".to_owned(),
    )?;
    ensure(
        bundle_dir.join("pack_replay_summary.json").is_file(),
        "bundle must contain pack_replay_summary.json".to_owned(),
    )?;
    ensure(
        bundle_dir.join("swarm_brief_summary.json").is_file(),
        "bundle must contain swarm_brief_summary.json".to_owned(),
    )?;
    let pack_replay_summary = fs::read_to_string(bundle_dir.join("pack_replay_summary.json"))
        .map_err(|error| format!("failed to read pack replay support summary: {error}"))?;
    let pack_replay_summary_json: Value = serde_json::from_str(&pack_replay_summary)
        .map_err(|error| format!("pack replay support summary must parse: {error}"))?;
    ensure_json_pointer(
        &pack_replay_summary_json,
        "/schema",
        json!("ee.support_bundle.pack_replay_summary.v1"),
        "support bundle pack replay summary schema",
    )?;
    ensure_json_pointer(
        &pack_replay_summary_json,
        "/redactionStatus",
        json!("ids_hashes_counts_codes_only_no_query_text_no_memory_content"),
        "support bundle pack replay summary redaction posture",
    )?;
    let swarm_brief_summary = fs::read_to_string(bundle_dir.join("swarm_brief_summary.json"))
        .map_err(|error| format!("failed to read swarm brief support summary: {error}"))?;
    let swarm_brief_summary_json: Value = serde_json::from_str(&swarm_brief_summary)
        .map_err(|error| format!("swarm brief support summary must parse: {error}"))?;
    ensure_json_pointer(
        &swarm_brief_summary_json,
        "/schema",
        json!("ee.support_bundle.swarm_brief_summary.v1"),
        "support bundle swarm brief summary schema",
    )?;
    ensure_json_pointer(
        &swarm_brief_summary_json,
        "/redactionStatus",
        json!("counts_hashes_codes_ids_only_no_mail_body_no_raw_queries_no_file_listings"),
        "support bundle swarm brief summary redaction posture",
    )?;
    ensure_json_pointer(
        &swarm_brief_summary_json,
        "/redaction/rawMailBodiesIncluded",
        json!(false),
        "support bundle swarm brief summary omits raw mail bodies",
    )?;
    ensure_json_pointer(
        &swarm_brief_summary_json,
        "/redaction/rawQueryTextIncluded",
        json!(false),
        "support bundle swarm brief summary omits raw query text",
    )?;
    ensure_json_pointer(
        &swarm_brief_summary_json,
        "/redaction/rawProvenanceTextIncluded",
        json!(false),
        "support bundle swarm brief summary omits raw provenance text",
    )?;
    ensure_json_pointer(
        &swarm_brief_summary_json,
        "/redaction/fullFileListingsIncluded",
        json!(false),
        "support bundle swarm brief summary omits full file listings",
    )?;
    ensure(
        swarm_brief_summary_json
            .pointer("/reportHash")
            .and_then(Value::as_str)
            .is_some_and(|hash| hash.starts_with("blake3:")),
        "swarm brief summary must include reportHash".to_owned(),
    )?;
    ensure(
        swarm_brief_summary_json
            .pointer("/counts/readyWorkCount")
            .is_some(),
        "swarm brief summary must include ready work count".to_owned(),
    )?;
    ensure(
        swarm_brief_summary_json
            .pointer("/counts/activeConflictCount")
            .is_some(),
        "swarm brief summary must include active conflict count".to_owned(),
    )?;

    let inspect_result = run_ee_logged(
        "support-bundle-inspect",
        None,
        vec![
            "--json".to_owned(),
            "support".to_owned(),
            "inspect".to_owned(),
            bundle_dir.display().to_string(),
        ],
    )?;

    ensure_equal(&inspect_result.exit_code, &0, "support inspect exit code")?;
    ensure_json_pointer(
        &inspect_result.parsed,
        "/success",
        json!(true),
        "support inspect success",
    )?;
    ensure_json_pointer(
        &inspect_result.parsed,
        "/data/valid",
        json!(true),
        "support inspect must report valid=true for intact bundle",
    )?;

    Ok(())
}

#[test]
fn certificate_verify_reports_not_found_instead_of_mock_success() -> TestResult {
    let workspace_root = unique_artifact_dir("certificate-not-found-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();
    let result = run_ee_logged(
        "certificate-verify-not-found",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "certificate".to_owned(),
            "verify".to_owned(),
            "cert_pack_001".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &0, "certificate read-only exit code")?;
    ensure(
        result.stderr.is_empty(),
        "certificate JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "certificate stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.certificate.verify.v1"),
        "certificate verify response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/result",
        json!("not_found"),
        "certificate not-found result",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/failureCodes",
        json!(["not_found"]),
        "certificate not-found failure code",
    )?;
    let removed_certificate_code = ["certificate", "store", "unavailable"].join("_");
    ensure(
        !result.stdout.contains(&removed_certificate_code),
        "certificate verify must not emit the removed unavailable sentinel",
    )?;

    let fake_success =
        validate_no_fake_success_output("certificate verify", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded certificate output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("certificate verify", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded certificate output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!([]),
        "logged certificate degradation codes",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("certificate"),
        "logged certificate boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("read-only, idempotent"),
        "logged certificate side-effect class",
    )
}

#[test]
fn claim_commands_reject_invalid_claims_without_placeholder_success() -> TestResult {
    let workspace_root = unique_artifact_dir("claim-invalid-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    fs::write(
        workspace.join("claims.yaml"),
        "claims:\n  - id: claim_fixture_001\n    title: placeholder verification must not pass\n",
    )
    .map_err(|error| format!("failed to write claims.yaml: {error}"))?;
    let workspace_arg = workspace.display().to_string();

    let cases = [
        (
            "claim-list-invalid",
            "claim list",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "claim".to_owned(),
                "list".to_owned(),
            ],
        ),
        (
            "claim-show-invalid",
            "claim show",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "claim".to_owned(),
                "show".to_owned(),
                "claim_fixture_001".to_owned(),
            ],
        ),
        (
            "claim-verify-invalid",
            "claim verify",
            vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "claim".to_owned(),
                "verify".to_owned(),
                "claim_fixture_001".to_owned(),
            ],
        ),
    ];

    for (name, command, args) in cases {
        let result = run_ee_logged(name, Some(&workspace), args)?;
        ensure_equal(
            &result.exit_code,
            &1,
            &format!("{command} invalid claim usage exit code"),
        )?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON error response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} error stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!("ee.error.v1"),
            &format!("{command} error response schema"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/error/code",
            json!("usage"),
            &format!("{command} error code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/error/repair",
            json!("Fix .ee/claims.yaml or pass --claims-file <path>."),
            &format!("{command} repair command"),
        )?;

        let fake_success = validate_no_fake_success_output(command, false, false, &result.stdout);
        ensure(
            fake_success.passed,
            format!("degraded {command} output should not be fake success: {fake_success:?}"),
        )?;

        let unsupported_claims =
            validate_no_unsupported_evidence_claims(command, false, false, &result.stdout);
        ensure(
            unsupported_claims.passed,
            format!(
                "invalid {command} output should not count as unsupported success: {unsupported_claims:?}"
            ),
        )?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!(["usage"]),
            &format!("logged {command} error code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            json!("Fix .ee/claims.yaml or pass --claims-file <path>."),
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("claim"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!("read-only, idempotent"),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
}

#[test]
fn diag_quarantine_reports_persisted_state_instead_of_placeholder_health() -> TestResult {
    let workspace_root = unique_artifact_dir("diag-quarantine-missing-db-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();
    let result = run_ee_logged(
        "diag-quarantine-persisted-state",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "diag".to_owned(),
            "quarantine".to_owned(),
            "list".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &0, "diag quarantine read-only exit code")?;
    ensure(
        result.stderr.is_empty(),
        "diag quarantine JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "diag quarantine stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "diag quarantine response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(true), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/command",
        json!("diag quarantine"),
        "diag quarantine command label",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/storageStatus",
        json!("missing"),
        "diag quarantine storage status",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/summary/totalSources",
        json!(0),
        "diag quarantine source count",
    )?;
    let removed_quarantine_code = ["quarantine", "trust", "state", "unavailable"].join("_");
    ensure(
        !result.stdout.contains(&removed_quarantine_code),
        "diag quarantine must not emit the removed unavailable sentinel",
    )?;

    let fake_success =
        validate_no_fake_success_output("diag quarantine", true, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("diag quarantine output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("diag quarantine", true, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "diag quarantine output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["quarantine_database_missing"]),
        "logged diag quarantine degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee init --workspace ."),
        "logged diag quarantine repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("diag quarantine"),
        "logged diag quarantine boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("read-only persisted trust-state query; no source trust mutation"),
        "logged diag quarantine side-effect class",
    )
}

#[test]
fn rehearse_commands_emit_real_sandbox_artifacts_instead_of_degraded_stub() -> TestResult {
    let command_spec = r#"[{
      "id":"cmd_status",
      "command":"status",
      "args":["--json"],
      "expected_effect":"read_only",
      "stop_on_failure":false,
      "idempotency_key":"idem-status-001"
    }]"#;

    let workspace_root = unique_artifact_dir("rehearse-sandbox-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    fs::write(workspace.join("state.txt"), "source")
        .map_err(|error| format!("failed to seed source workspace: {error}"))?;
    let out = unique_artifact_dir("rehearse-sandbox-out")?;
    let workspace_arg = workspace.display().to_string();
    let out_arg = out.display().to_string();

    let plan = run_ee_logged(
        "rehearse-plan-sandbox-ready",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "rehearse".to_owned(),
            "plan".to_owned(),
            "--commands-json".to_owned(),
            command_spec.to_owned(),
        ],
    )?;
    ensure_equal(&plan.exit_code, &0, "rehearse plan exit code")?;
    ensure(
        plan.stderr.is_empty(),
        "rehearse plan must keep stderr empty",
    )?;
    ensure_no_ansi(&plan.stdout, "rehearse plan stdout")?;
    ensure_json_pointer(
        &plan.parsed,
        "/schema",
        json!("ee.response.v1"),
        "rehearse plan response schema",
    )?;
    ensure_json_pointer(&plan.parsed, "/success", json!(true), "plan success flag")?;
    ensure_json_pointer(
        &plan.parsed,
        "/data/schema",
        json!("ee.rehearse.plan.v1"),
        "plan data schema",
    )?;
    ensure_json_pointer(
        &plan.parsed,
        "/data/can_proceed",
        json!(true),
        "plan can proceed",
    )?;
    ensure_json_pointer(
        &plan.parsed,
        "/data/degradation_codes",
        json!([]),
        "plan degradation codes",
    )?;
    let estimated_artifacts = plan
        .parsed
        .pointer("/data/estimated_artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| "plan estimated_artifacts must be an array".to_string())?;
    ensure(
        [
            "manifest.json",
            "source_snapshot.json",
            "sandbox_snapshot.json",
        ]
        .iter()
        .all(|expected| {
            estimated_artifacts
                .iter()
                .any(|artifact| artifact.as_str().is_some_and(|name| name.eq(*expected)))
        }),
        "rehearse plan must advertise manifest and filesystem snapshot artifacts",
    )?;

    let run = run_ee_logged(
        "rehearse-run-sandbox-artifacts",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "rehearse".to_owned(),
            "run".to_owned(),
            "--commands-json".to_owned(),
            command_spec.to_owned(),
            "--out".to_owned(),
            out_arg.clone(),
            "--profile".to_owned(),
            "quick".to_owned(),
        ],
    )?;
    ensure_equal(&run.exit_code, &0, "rehearse run exit code")?;
    ensure(run.stderr.is_empty(), "rehearse run must keep stderr empty")?;
    ensure_no_ansi(&run.stdout, "rehearse run stdout")?;
    ensure_json_pointer(
        &run.parsed,
        "/data/schema",
        json!("ee.rehearse.run.v1"),
        "run data schema",
    )?;
    ensure_json_pointer(
        &run.parsed,
        "/data/overall_result",
        json!("passed"),
        "run overall result",
    )?;
    ensure_json_pointer(
        &run.parsed,
        "/data/degradation_codes",
        json!([]),
        "run degradation codes",
    )?;
    ensure_json_pointer(
        &run.parsed,
        "/data/command_results/0/exit_code",
        json!(0),
        "run command exit code",
    )?;
    let sandbox_path = run
        .parsed
        .pointer("/data/sandbox_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "run sandbox_path missing".to_string())?;
    ensure(
        Path::new(sandbox_path).join("state.txt").is_file(),
        "rehearse run must copy the source workspace into a readable sandbox",
    )?;
    let artifact_paths = run
        .parsed
        .pointer("/data/artifact_paths")
        .ok_or_else(|| "run artifact_paths missing".to_string())?;
    let manifest = artifact_paths
        .get("manifest")
        .and_then(Value::as_str)
        .ok_or_else(|| "run manifest path missing".to_string())?
        .to_owned();
    for artifact_key in ["manifest", "source_snapshot", "sandbox_snapshot"] {
        ensure(
            artifact_paths
                .get(artifact_key)
                .and_then(Value::as_str)
                .is_some_and(|path| Path::new(path).is_file()),
            format!("rehearse run must write {artifact_key} artifact"),
        )?;
    }
    ensure_equal(
        &fs::read_to_string(workspace.join("state.txt"))
            .map_err(|error| format!("failed to read source workspace: {error}"))?,
        &"source".to_owned(),
        "rehearse run must not mutate the source workspace",
    )?;

    let inspect = run_ee_logged(
        "rehearse-inspect-manifest",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "rehearse".to_owned(),
            "inspect".to_owned(),
            manifest.clone(),
        ],
    )?;
    ensure_equal(&inspect.exit_code, &0, "rehearse inspect exit code")?;
    ensure_json_pointer(
        &inspect.parsed,
        "/data/schema",
        json!("ee.rehearse.inspect.v1"),
        "inspect data schema",
    )?;
    ensure_json_pointer(
        &inspect.parsed,
        "/data/integrity_status",
        json!("valid"),
        "inspect integrity status",
    )?;
    ensure_json_pointer(
        &inspect.parsed,
        "/data/command_count",
        json!(1),
        "inspect command count",
    )?;

    let promote = run_ee_logged(
        "rehearse-promote-plan-manifest",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "rehearse".to_owned(),
            "promote-plan".to_owned(),
            manifest,
        ],
    )?;
    ensure_equal(&promote.exit_code, &0, "rehearse promote-plan exit code")?;
    ensure_json_pointer(
        &promote.parsed,
        "/data/schema",
        json!("ee.rehearse.promote_plan.v1"),
        "promote-plan data schema",
    )?;
    ensure_json_pointer(
        &promote.parsed,
        "/data/is_safe",
        json!(true),
        "promote-plan safety",
    )?;

    for (command, result, expected_side_effect) in [
        (
            "rehearse plan",
            &plan,
            "read-only rehearsal planning, inspection, or promotion guidance",
        ),
        (
            "rehearse run",
            &run,
            "sandboxed rehearsal side-path artifact write; source workspace unchanged",
        ),
        (
            "rehearse inspect",
            &inspect,
            "read-only rehearsal planning, inspection, or promotion guidance",
        ),
        (
            "rehearse promote-plan",
            &promote,
            "read-only rehearsal planning, inspection, or promotion guidance",
        ),
    ] {
        let fake_success = validate_no_fake_success_output(command, true, false, &result.stdout);
        ensure(
            fake_success.passed,
            format!("{command} output should not contain fake success markers: {fake_success:?}"),
        )?;
        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("{command} e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!([]),
            &format!("logged {command} degradation codes"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            json!(null),
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("rehearse"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!(expected_side_effect),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
}

#[test]
fn learn_read_and_proposal_commands_use_persisted_ledgers() -> TestResult {
    let (workspace, _database) = seed_audit_cli_workspace("learn-real-empty-ledger")?;
    let workspace_arg = workspace.display().to_string();
    let commands = [
        (
            "learn-agenda-real-empty",
            "learn agenda",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "learn".to_owned(),
                "agenda".to_owned(),
                "--limit".to_owned(),
                "2".to_owned(),
            ],
            "ee.learn.agenda.v1",
            "/items",
        ),
        (
            "learn-uncertainty-real-empty",
            "learn uncertainty",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "learn".to_owned(),
                "uncertainty".to_owned(),
                "--min-uncertainty".to_owned(),
                "0.3".to_owned(),
            ],
            "ee.learn.uncertainty.v1",
            "/items",
        ),
        (
            "learn-experiment-propose-real-empty",
            "learn experiment propose",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "learn".to_owned(),
                "experiment".to_owned(),
                "propose".to_owned(),
            ],
            "ee.learn.experiment_proposal.v1",
            "/proposals",
        ),
        (
            "learn-summary-real-empty",
            "learn summary",
            vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "learn".to_owned(),
                "summary".to_owned(),
            ],
            "ee.learn.summary.v1",
            "/events",
        ),
    ];

    for (artifact_name, command, args, schema, empty_collection_pointer) in commands {
        let result = run_ee_logged(artifact_name, Some(&workspace), args)?;

        ensure_equal(&result.exit_code, &0, &format!("{command} exit code"))?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!(schema),
            &format!("{command} schema"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/success",
            json!(true),
            &format!("{command} success flag"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            empty_collection_pointer,
            json!([]),
            &format!("{command} empty persisted-ledger collection"),
        )?;
        ensure(
            !result.stdout.contains("learning_records_unavailable"),
            format!("{command} must not report the removed learn unavailable sentinel"),
        )?;

        let fake_success = validate_no_fake_success_output(command, true, false, &result.stdout);
        ensure(
            fake_success.passed,
            format!("{command} output should not be fake success: {fake_success:?}"),
        )?;

        let unsupported_claims =
            validate_no_unsupported_evidence_claims(command, true, false, &result.stdout);
        ensure(
            unsupported_claims.passed,
            format!(
                "{command} output should not count as unsupported success: {unsupported_claims:?}"
            ),
        )?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!([]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            json!(null),
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("learn"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_logged_contract_shape(&result, command)?;
    }

    let workspace_arg = workspace.display().to_string();
    let run_result = run_ee_logged(
        "learn-experiment-run-dry-run",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "learn".to_owned(),
            "experiment".to_owned(),
            "run".to_owned(),
            "--id".to_owned(),
            "exp_database_contract_fixture".to_owned(),
            "--dry-run".to_owned(),
        ],
    )?;
    ensure_equal(
        &run_result.exit_code,
        &1,
        "learn experiment run missing proposal exit code",
    )?;
    ensure(
        run_result.stderr.is_empty(),
        "learn experiment run missing proposal JSON stderr empty",
    )?;
    ensure_no_ansi(
        &run_result.stdout,
        "learn experiment run missing proposal stdout",
    )?;
    ensure_json_pointer(
        &run_result.parsed,
        "/schema",
        json!("ee.error.v1"),
        "learn experiment run missing proposal schema",
    )?;
    ensure_json_pointer(
        &run_result.parsed,
        "/error/code",
        json!("not_found"),
        "learn experiment run missing proposal code",
    )?;
    ensure_json_pointer(
        &run_result.parsed,
        "/error/repair",
        json!("Run ee learn experiment propose --json to register experiment definitions."),
        "learn experiment run missing proposal repair",
    )?;
    ensure(
        !run_result
            .stdout
            .contains("experiment_registry_unavailable"),
        "learn experiment run must not report the removed experiment registry sentinel",
    )?;
    let fake_success =
        validate_no_fake_success_output("learn experiment run", false, false, &run_result.stdout);
    ensure(
        fake_success.passed,
        format!(
            "learn experiment run degraded output should not be fake success: {fake_success:?}"
        ),
    )?;
    let unsupported_claims = validate_no_unsupported_evidence_claims(
        "learn experiment run",
        false,
        false,
        &run_result.stdout,
    );
    ensure(
        unsupported_claims.passed,
        format!(
            "learn experiment run degraded output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;
    ensure_logged_contract_shape(&run_result, "learn experiment run")?;

    Ok(())
}

#[test]
fn lab_replay_reports_missing_frozen_inputs_without_generated_success() -> TestResult {
    let result = run_ee_logged(
        "lab-replay-missing-frozen-inputs",
        None,
        vec![
            "--json".to_owned(),
            "lab".to_owned(),
            "replay".to_owned(),
            "ep_missing_evidence".to_owned(),
            "--dry-run".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &0, "lab replay report exit code")?;
    ensure(
        result.stderr.is_empty(),
        "lab JSON replay response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "lab replay stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "lab replay response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(true), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/status",
        json!("episode_not_found"),
        "lab replay status",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/replayEvidenceAvailable",
        json!(false),
        "lab replay evidence availability",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/frozenInputs",
        json!(false),
        "lab replay frozen inputs",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/missingFrozenInputs",
        json!([
            "frozen episode manifest",
            "frozen memory snapshot",
            "frozen action trace"
        ]),
        "lab missing frozen inputs",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/mutableCurrentStateAccess",
        json!([]),
        "lab mutable current-state access",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/warnings/0",
        json!("lab_replay_unavailable: missing frozen episode manifest for ep_missing_evidence"),
        "lab replay warning",
    )?;

    let fake_success = validate_no_fake_success_output("lab replay", true, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("lab replay report should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("lab replay", true, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!("lab replay report should not overclaim evidence: {unsupported_claims:?}"),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["lab_replay_unavailable"]),
        "logged lab degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        Value::Null,
        "logged lab repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("lab"),
        "logged lab boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("evidence-only lab report; no behavior inference or durable mutation"),
        "logged lab side-effect class",
    )
}

#[test]
fn economy_report_degrades_instead_of_reporting_seed_metrics() -> TestResult {
    let workspace_root = unique_artifact_dir("economy-report-unavailable-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();
    let result = run_ee_logged(
        "economy-report-unavailable",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "economy".to_owned(),
            "report".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "economy unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "economy JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "economy degraded stdout")?;
    let schema = result
        .parsed
        .pointer("/schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "economy response missing schema".to_owned())?;
    match schema {
        "ee.error.v1" => {
            ensure_json_pointer(
                &result.parsed,
                "/error/code",
                json!("unsatisfied_degraded_mode"),
                "economy missing-database degraded code",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/error/repair",
                json!("ee init --workspace ."),
                "economy missing-database repair command",
            )?;
        }
        "ee.response.v1" => {
            ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
            ensure_json_pointer(
                &result.parsed,
                "/data/code",
                json!("economy_metrics_unavailable"),
                "economy degraded code",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/data/degraded/0/code",
                json!("economy_metrics_unavailable"),
                "economy degraded array code",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/data/followUpBead",
                json!("eidetic_engine_cli-ve0w"),
                "economy follow-up bead",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/data/evidenceIds",
                json!([]),
                "economy evidence ids",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/data/sourceIds",
                json!([]),
                "economy source ids",
            )?;
        }
        other => {
            return Err(format!(
                "economy response schema must be ee.error.v1 or ee.response.v1, got {other}"
            ));
        }
    }

    let fake_success =
        validate_no_fake_success_output("economy report", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded economy output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("economy report", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded economy output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    let expected_degradation_codes = collect_degradation_codes(&result.parsed);
    let expected_repair = first_repair_command(&result.parsed);
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(expected_degradation_codes),
        "logged economy degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        expected_repair.map_or(Value::Null, Value::String),
        "logged economy repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("economy"),
        "logged economy boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("read-only, conservative abstention"),
        "logged economy side-effect class",
    )
}

#[test]
fn causal_trace_without_failure_id_reports_empty_evidence_query() -> TestResult {
    let workspace_root = unique_artifact_dir("causal-trace-empty-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();

    let init = run_ee_logged(
        "causal-trace-empty-init",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "init".to_owned(),
        ],
    )?;
    ensure_equal(&init.exit_code, &0, "causal trace workspace init exit")?;

    let result = run_ee_logged(
        "causal-trace-empty-query",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "causal".to_owned(),
            "trace".to_owned(),
            "--dry-run".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &0,
        "causal empty evidence query exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "causal JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "causal stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "causal response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(true), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/schema",
        json!("ee.causal.trace.v1"),
        "causal trace data schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/chains",
        json!([]),
        "causal trace must not generate chains without a failure memory id",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degradations/0/code",
        json!("causal_failure_id_required"),
        "causal missing failure-id degradation",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/dryRun",
        json!(true),
        "causal trace dry-run flag",
    )?;

    let fake_success = validate_no_fake_success_output("causal trace", true, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("causal output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("causal trace", true, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!("causal output should not count as unsupported success: {unsupported_claims:?}"),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["causal_failure_id_required"]),
        "logged causal degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        Value::Null,
        "logged causal repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("causal"),
        "logged causal boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!(
            "persisted causal-evidence query; promote-plan writes audited curation candidates when storage is available"
        ),
        "logged causal side-effect class",
    )
}

#[test]
fn procedure_list_reports_persisted_records_without_unavailable_sentinel() -> TestResult {
    let workspace_root = unique_artifact_dir("procedure-list-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    init_workspace(&workspace, "procedure list")?;
    let workspace_arg = workspace.display().to_string();

    let result = run_ee_logged(
        "procedure-list-persisted",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "procedure".to_owned(),
            "list".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &0, "procedure list exit code")?;
    ensure(
        result.stderr.is_empty(),
        "procedure JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "procedure list stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.procedure.list_report.v1"),
        "procedure list response schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/total_count",
        json!(0),
        "procedure total count",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/filtered_count",
        json!(0),
        "procedure filtered count",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/procedures",
        json!([]),
        "procedure empty persisted list",
    )?;
    ensure(
        !result.stdout.contains("procedure_store_unavailable"),
        "procedure list must not emit the removed unavailable sentinel",
    )?;

    ensure_no_fake_or_unsupported_claims("procedure list", true, false, &result.stdout)?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!([]),
        "logged procedure degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        Value::Null,
        "logged procedure repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("procedure"),
        "logged procedure boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("read-only persisted procedure query; no procedure mutation or artifact write"),
        "logged procedure side-effect class",
    )?;
    ensure_logged_contract_shape(&result, "procedure list")
}

#[test]
fn situation_classify_reports_heuristic_routing_without_unavailable_sentinel() -> TestResult {
    let result = run_ee_logged(
        "situation-classify-heuristic",
        None,
        vec![
            "--json".to_owned(),
            "situation".to_owned(),
            "classify".to_owned(),
            "fix failing release workflow".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &0, "situation classify exit code")?;
    ensure(
        result.stderr.is_empty(),
        "situation JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "situation classify stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.situation.classify.v1"),
        "situation classify response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(true), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/command",
        json!("situation classify"),
        "situation classify command label",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/category",
        json!("bug_fix"),
        "situation classify category",
    )?;
    ensure(
        result
            .parsed
            .pointer("/data/signals")
            .and_then(Value::as_array)
            .is_some_and(|signals| !signals.is_empty()),
        "situation classify must report heuristic signals",
    )?;
    ensure(
        !result.stdout.contains("situation_decisioning_unavailable"),
        "situation classify must not emit the removed unavailable sentinel",
    )?;

    ensure_no_fake_or_unsupported_claims("situation classify", true, false, &result.stdout)?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!([]),
        "logged situation degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        Value::Null,
        "logged situation repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("situation"),
        "logged situation boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!(
            "read-only heuristic situation classification; no situation link, routing-state, or recommendation mutation"
        ),
        "logged situation side-effect class",
    )?;
    ensure_logged_contract_shape(&result, "situation classify")
}

#[test]
fn plan_goal_and_explain_report_catalog_reasoning_without_unavailable_sentinel() -> TestResult {
    let commands = [
        (
            "plan-goal-catalog",
            "plan goal",
            "ee.plan.recommend.v1",
            vec![
                "--json".to_owned(),
                "plan".to_owned(),
                "goal".to_owned(),
                "--goal".to_owned(),
                "fix failing release workflow".to_owned(),
            ],
        ),
        (
            "plan-explain-catalog",
            "plan explain",
            "ee.plan.explain.v1",
            vec![
                "--json".to_owned(),
                "plan".to_owned(),
                "explain".to_owned(),
                "init-workspace".to_owned(),
            ],
        ),
    ];

    for (artifact_name, command, schema, args) in commands {
        let result = run_ee_logged(artifact_name, None, args)?;

        ensure_equal(&result.exit_code, &0, &format!("{command} exit code"))?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!(schema),
            &format!("{command} response schema"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/success",
            json!(true),
            &format!("{command} success flag"),
        )?;
        let expected_command = if command == "plan goal" {
            "plan recommend"
        } else {
            "plan explain"
        };
        ensure_json_pointer(
            &result.parsed,
            "/data/command",
            json!(expected_command),
            &format!("{command} command label"),
        )?;
        ensure(
            !result.stdout.contains("plan_decisioning_unavailable"),
            format!("{command} must not emit the removed unavailable sentinel"),
        )?;
        if command == "plan goal" {
            ensure(
                result
                    .parsed
                    .pointer("/data/recommendations")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty()),
                "plan goal must return at least one catalog recommendation",
            )?;
        } else {
            ensure_json_pointer(
                &result.parsed,
                "/data/found",
                json!(true),
                "plan explain found flag",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/data/recipeId",
                json!("init-workspace"),
                "plan explain recipe id",
            )?;
            ensure(
                result
                    .parsed
                    .pointer("/data/evidenceUris")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty()),
                "plan explain must name catalog evidence URIs for maturity claims",
            )?;
        }

        if command == "plan goal" {
            ensure_no_fake_or_unsupported_claims(command, true, false, &result.stdout)?;
        } else {
            let fake_success =
                validate_no_fake_success_output(command, true, false, &result.stdout);
            ensure(
                fake_success.passed,
                format!("{command} output should not be fake success: {fake_success:?}"),
            )?;
        }

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!([]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            Value::Null,
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("plan"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!("read-only recipe recommendation/explanation; no plan mutation"),
            &format!("logged {command} side-effect class"),
        )?;
        ensure_logged_contract_shape(&result, command)?;
    }

    Ok(())
}

#[test]
fn eval_run_and_list_report_fixture_results_without_unavailable_sentinel() -> TestResult {
    let cases = [
        (
            "eval-run-fixture",
            "eval run",
            vec![
                "--json".to_owned(),
                "eval".to_owned(),
                "run".to_owned(),
                "release_failure".to_owned(),
            ],
        ),
        (
            "eval-run-science-fixture",
            "eval run",
            vec![
                "--json".to_owned(),
                "eval".to_owned(),
                "run".to_owned(),
                "release_failure".to_owned(),
                "--science".to_owned(),
            ],
        ),
        (
            "eval-list-fixtures",
            "eval list",
            vec!["--json".to_owned(), "eval".to_owned(), "list".to_owned()],
        ),
    ];

    for (artifact_name, command, args) in cases {
        let result = run_ee_logged(artifact_name, None, args)?;

        ensure_equal(&result.exit_code, &0, &format!("{command} exit code"))?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!("ee.response.v1"),
            &format!("{command} response schema"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/success",
            json!(true),
            &format!("{command} success flag"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/command",
            json!(command),
            &format!("{command} command label"),
        )?;
        ensure(
            !result.stdout.contains("eval_fixtures_unavailable"),
            format!("{command} must not emit the removed unavailable sentinel"),
        )?;

        if command == "eval run" {
            ensure_json_pointer(
                &result.parsed,
                "/data/report/schema",
                json!("ee.eval.report.v1"),
                &format!("{command} report schema"),
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/data/report/fixture_id",
                json!("fx.release_failure.v1"),
                &format!("{command} fixture id"),
            )?;
            ensure(
                result.parsed.pointer("/data/report/metrics").is_some(),
                format!("{command} must emit real evaluation metrics"),
            )?;
        } else {
            ensure(
                result
                    .parsed
                    .pointer("/data/fixtures")
                    .and_then(Value::as_array)
                    .is_some_and(|fixtures| !fixtures.is_empty()),
                "eval list must discover fixture metadata",
            )?;
            ensure(
                result
                    .parsed
                    .pointer("/data/fixtureCount")
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count > 0),
                "eval list must report a positive fixture count",
            )?;
        }

        ensure_no_fake_or_unsupported_claims(command, true, true, &result.stdout)?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!([]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            Value::Null,
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("eval"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!("read-only fixture discovery/evaluation report generation; no durable mutation"),
            &format!("logged {command} side-effect class"),
        )?;
        ensure_logged_contract_shape(&result, command)?;
    }

    Ok(())
}

#[test]
fn eval_run_pack_quality_reports_fixture_comparison_without_unavailable_sentinel() -> TestResult {
    let result = run_ee_logged(
        "eval-run-pack-quality-fixture",
        None,
        vec![
            "--json".to_owned(),
            "eval".to_owned(),
            "run".to_owned(),
            "release_failure".to_owned(),
            "--pack-quality".to_owned(),
            "--scenario".to_owned(),
            "usr_pre_task_brief".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &0, "eval run pack-quality exit code")?;
    ensure(
        result.stderr.is_empty(),
        "eval run pack-quality JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "eval run pack-quality stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "eval run pack-quality response schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/success",
        json!(true),
        "eval run pack-quality success flag",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/command",
        json!("eval run"),
        "eval run pack-quality command label",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/mode",
        json!("pack_quality"),
        "eval run pack-quality mode",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/report/schema",
        json!("ee.eval.pack_quality_report.v1"),
        "eval run pack-quality report schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/report/fixture_id",
        json!("fx.release_failure.v1"),
        "eval run pack-quality fixture id",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/report/aggregate_verdict",
        json!("within"),
        "eval run pack-quality verdict",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/artifactPaths/0/stdout",
        json!("target/ee-e2e/usr_pre_task_brief/<run-id>/04-context.stdout.json"),
        "eval run pack-quality artifact path",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degradedBranches/0/code",
        json!("semantic_disabled"),
        "eval run pack-quality lexical degraded branch",
    )?;
    ensure(
        !result.stdout.contains("eval_fixtures_unavailable"),
        "eval run pack-quality must not emit the removed unavailable sentinel",
    )?;
    ensure_no_fake_or_unsupported_claims("eval run", true, true, &result.stdout)?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("eval"),
        "logged eval run pack-quality boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("read-only fixture discovery/evaluation report generation; no durable mutation"),
        "logged eval run pack-quality side-effect class",
    )?;
    ensure_logged_contract_shape(&result, "eval run")?;

    Ok(())
}

#[test]
fn review_session_reports_storage_error_without_unavailable_sentinel() -> TestResult {
    let workspace_root = unique_artifact_dir("review-session-missing-db-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();

    let result = run_ee_logged(
        "review-session-missing-db",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "review".to_owned(),
            "session".to_owned(),
            "--propose".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &3,
        "review session storage error exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "review session JSON error response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "review session error stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.error.v1"),
        "review session error response schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/error/code",
        json!("storage"),
        "review session storage error code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/error/repair",
        json!("ee init --workspace ."),
        "review session repair command",
    )?;
    ensure(
        !result.stdout.contains("review_evidence_unavailable")
            && !result.stdout.contains("Session review is unavailable"),
        "review session must not emit the removed unavailable sentinel",
    )?;

    let fake_success =
        validate_no_fake_success_output("review session", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded review session output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("review session", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded review session output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["storage"]),
        "logged review session storage error code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee init --workspace ."),
        "logged review session repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("review"),
        "logged review session boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!(
            "audited curation candidate mutation only when storage is available; missing storage prevents review"
        ),
        "logged review session side-effect class",
    )
}

#[test]
fn tripwire_commands_report_store_queries_without_unavailable_sentinel() -> TestResult {
    let workspace_root = unique_artifact_dir("tripwire-readonly-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();
    let tripwire_database_arg = workspace_root
        .join("missing-tripwire-store.ee.db")
        .display()
        .to_string();

    let cases = [
        (
            "tripwire-list-readonly",
            "tripwire list",
            "ee.tripwire.list.v1",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "tripwire".to_owned(),
                "list".to_owned(),
                "--database".to_owned(),
                tripwire_database_arg.clone(),
                "--include-disarmed".to_owned(),
            ],
        ),
        (
            "tripwire-check-readonly",
            "tripwire check",
            "ee.tripwire.check.v1",
            vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "tripwire".to_owned(),
                "check".to_owned(),
                "tw_004".to_owned(),
                "--database".to_owned(),
                tripwire_database_arg,
                "--task-outcome".to_owned(),
                "success".to_owned(),
                "--dry-run".to_owned(),
            ],
        ),
    ];

    for (artifact_name, command, schema, args) in cases {
        let result = run_ee_logged(artifact_name, Some(&workspace), args)?;

        ensure_equal(&result.exit_code, &0, &format!("{command} exit code"))?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!(schema),
            &format!("{command} response schema"),
        )?;
        ensure(
            !result.stdout.contains("tripwire_store_unavailable"),
            format!("{command} must not emit the removed unavailable sentinel"),
        )?;

        if command == "tripwire list" {
            ensure_json_pointer(
                &result.parsed,
                "/tripwires",
                json!([]),
                "tripwire list empty persisted list",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/total_count",
                json!(0),
                "tripwire list total count",
            )?;
        } else {
            ensure_json_pointer(
                &result.parsed,
                "/tripwire_id",
                json!("tw_004"),
                "tripwire check id",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/result",
                json!("not_found"),
                "tripwire check result",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/durable_mutation",
                json!(false),
                "tripwire check durable mutation",
            )?;
            ensure_json_pointer(
                &result.parsed,
                "/degraded",
                json!([]),
                "tripwire check degradation array",
            )?;
        }

        ensure_no_fake_or_unsupported_claims(command, true, false, &result.stdout)?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!([]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            Value::Null,
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!(command.split(' ').next().unwrap_or("unknown")),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!("read-only tripwire store query/evaluation; no tripwire mutation"),
            &format!("logged {command} side-effect class"),
        )?;
        ensure_logged_contract_shape(&result, command)?;
    }

    Ok(())
}

#[test]
#[ignore = "ee handoff create writes real capsules now (h0h1, eidetic_engine_cli-172p) — needs replacement with a positive contract test; tracked in eidetic_engine_cli-oskm follow-up"]
fn handoff_create_degrades_instead_of_writing_placeholder_capsule() -> TestResult {
    let output_dir = unique_artifact_dir("handoff-create-output")?;
    let capsule_path = output_dir.join("handoff.json");
    let result = run_ee_logged(
        "handoff-create-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "handoff".to_owned(),
            "create".to_owned(),
            "--out".to_owned(),
            capsule_path.display().to_string(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "handoff unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "handoff JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "handoff degraded stdout")?;
    ensure(
        !capsule_path.exists(),
        "degraded handoff create must not write a placeholder capsule",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "handoff degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("handoff_unavailable"),
        "handoff degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("handoff_unavailable"),
        "handoff degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-g9dq"),
        "handoff follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sideEffectClass",
        json!("conservative abstention; no continuity capsule write"),
        "handoff side-effect class",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "handoff evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "handoff source ids",
    )?;

    let fake_success =
        validate_no_fake_success_output("handoff create", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded handoff output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("handoff create", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded handoff output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["handoff_unavailable"]),
        "logged handoff degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee status --json"),
        "logged handoff repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("handoff"),
        "logged handoff boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("conservative abstention; no continuity capsule write"),
        "logged handoff side-effect class",
    )
}

#[test]
fn daemon_foreground_runs_real_health_job_without_unavailable_sentinel() -> TestResult {
    let workspace_root = unique_artifact_dir("daemon-foreground-real-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();

    let result = run_ee_logged(
        "daemon-foreground-real",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "daemon".to_owned(),
            "--foreground".to_owned(),
            "--once".to_owned(),
            "--interval-ms".to_owned(),
            "0".to_owned(),
            "--job".to_owned(),
            "health_check".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &0,
        "daemon foreground real health job exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "daemon JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "daemon foreground stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "daemon response schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/success",
        json!(true),
        "daemon foreground success flag",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/schema",
        json!("ee.steward.daemon_foreground.v1"),
        "daemon foreground report schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/command",
        json!("daemon"),
        "daemon command label",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/jobTypes",
        json!(["health_check"]),
        "daemon requested job types",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/summary/jobsRun",
        json!(1),
        "daemon real job count",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/summary/succeeded",
        json!(1),
        "daemon succeeded count",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/summary/failed",
        json!(0),
        "daemon failed count",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/ticks/0/runner/results/0/jobType",
        json!("health_check"),
        "daemon runner job type",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/ticks/0/runner/results/0/outcome",
        json!("success"),
        "daemon runner outcome",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/ticks/0/runner/results/0/details/storageStatus",
        json!("missing"),
        "daemon health job storage status",
    )?;
    ensure(
        !result.stdout.contains("daemon_jobs_unavailable"),
        "daemon real output must not retain the unavailable sentinel",
    )?;
    ensure_no_fake_or_unsupported_claims("daemon foreground", true, false, &result.stdout)?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["daemon_background_mode_unimplemented"]),
        "logged daemon foreground limitation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("Run ee daemon --foreground with an explicit tick limit."),
        "logged daemon repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("daemon"),
        "logged daemon boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("foreground daemon writes supervised job rows and runs bounded maintenance handlers"),
        "logged daemon side-effect class",
    )
}

#[test]
fn recorder_start_event_finish_persist_real_state() -> TestResult {
    let workspace_root = unique_artifact_dir("recorder-real-store-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();
    let init_output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["--workspace", &workspace_arg, "init", "--json"])
        .output()
        .map_err(|error| format!("failed to initialize recorder workspace: {error}"))?;
    ensure(
        init_output.status.success(),
        format!(
            "workspace init for recorder store should succeed: stdout={} stderr={}",
            String::from_utf8_lossy(&init_output.stdout),
            String::from_utf8_lossy(&init_output.stderr)
        ),
    )?;

    let start = run_ee_logged(
        "recorder-start-real-store",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "recorder".to_owned(),
            "start".to_owned(),
            "--agent-id".to_owned(),
            "agent_fixture".to_owned(),
            "--session-id".to_owned(),
            "session_fixture".to_owned(),
        ],
    )?;
    ensure_equal(&start.exit_code, &0, "recorder start exit code")?;
    ensure(start.stderr.is_empty(), "recorder start JSON stderr empty")?;
    ensure_no_ansi(&start.stdout, "recorder start stdout")?;
    ensure_json_pointer(
        &start.parsed,
        "/schema",
        json!("ee.recorder.start.v1"),
        "recorder start schema",
    )?;
    ensure_json_pointer(
        &start.parsed,
        "/agentId",
        json!("agent_fixture"),
        "recorder start agent",
    )?;
    ensure(
        start
            .parsed
            .pointer("/runId")
            .and_then(Value::as_str)
            .is_some_and(|run_id| run_id.starts_with("run_")),
        "recorder start must return a persisted run id",
    )?;
    ensure_no_fake_or_unsupported_claims("recorder start", true, false, &start.stdout)?;
    ensure_logged_contract_shape(&start, "recorder start")?;
    let start_log_text = fs::read_to_string(&start.log_path)
        .map_err(|error| format!("failed to read {}: {error}", start.log_path.display()))?;
    let start_log: Value = serde_json::from_str(&start_log_text)
        .map_err(|error| format!("recorder start e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &start_log,
        "/degradationCodes",
        json!([]),
        "recorder start degradation codes",
    )?;

    let run_id = start
        .parsed
        .pointer("/runId")
        .and_then(Value::as_str)
        .ok_or_else(|| "recorder start missing runId".to_owned())?
        .to_owned();
    let event = run_ee_logged(
        "recorder-event-real-store",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "recorder".to_owned(),
            "event".to_owned(),
            run_id.clone(),
            "--event-type".to_owned(),
            "tool_result".to_owned(),
            "--payload".to_owned(),
            "password marker token marker".to_owned(),
        ],
    )?;
    ensure_equal(&event.exit_code, &0, "recorder event exit code")?;
    ensure(event.stderr.is_empty(), "recorder event JSON stderr empty")?;
    ensure_no_ansi(&event.stdout, "recorder event stdout")?;
    ensure(
        !event.stdout.contains("password marker token marker"),
        "recorder event output must not echo raw sensitive payload",
    )?;
    ensure_json_pointer(
        &event.parsed,
        "/schema",
        json!("ee.recorder.event_response.v1"),
        "recorder event schema",
    )?;
    ensure_json_pointer(
        &event.parsed,
        "/runId",
        json!(run_id.clone()),
        "recorder event run id",
    )?;
    ensure_json_pointer(
        &event.parsed,
        "/sequence",
        json!(1),
        "recorder event sequence",
    )?;
    ensure_json_pointer(
        &event.parsed,
        "/redactionStatus",
        json!("full"),
        "recorder event redaction status",
    )?;
    ensure(
        event
            .parsed
            .pointer("/eventHash")
            .and_then(Value::as_str)
            .is_some_and(|hash| hash.starts_with("blake3:")),
        "recorder event must expose a persisted hash-chain event hash",
    )?;
    ensure_no_fake_or_unsupported_claims("recorder event", true, false, &event.stdout)?;
    ensure_logged_contract_shape(&event, "recorder event")?;

    let finish = run_ee_logged(
        "recorder-finish-real-store",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "recorder".to_owned(),
            "finish".to_owned(),
            run_id,
            "--status".to_owned(),
            "completed".to_owned(),
        ],
    )?;
    ensure_equal(&finish.exit_code, &0, "recorder finish exit code")?;
    ensure(
        finish.stderr.is_empty(),
        "recorder finish JSON stderr empty",
    )?;
    ensure_no_ansi(&finish.stdout, "recorder finish stdout")?;
    ensure_json_pointer(
        &finish.parsed,
        "/schema",
        json!("ee.recorder.finish.v1"),
        "recorder finish schema",
    )?;
    ensure_json_pointer(
        &finish.parsed,
        "/status",
        json!("completed"),
        "recorder finish status",
    )?;
    ensure_json_pointer(
        &finish.parsed,
        "/eventCount",
        json!(1),
        "recorder finish event count",
    )?;
    ensure_no_fake_or_unsupported_claims("recorder finish", true, false, &finish.stdout)?;
    ensure_logged_contract_shape(&finish, "recorder finish")
}

#[test]
fn recorder_tail_reads_initialized_store_without_degraded_sentinel() -> TestResult {
    let workspace_root = unique_artifact_dir("recorder-tail-real-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();
    let init_output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["--workspace", &workspace_arg, "init", "--json"])
        .output()
        .map_err(|error| format!("failed to initialize recorder tail workspace: {error}"))?;
    ensure(
        init_output.status.success(),
        format!(
            "workspace init for recorder tail should succeed: stdout={} stderr={}",
            String::from_utf8_lossy(&init_output.stdout),
            String::from_utf8_lossy(&init_output.stderr)
        ),
    )?;

    let result = run_ee_logged(
        "recorder-tail-real-store",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "recorder".to_owned(),
            "tail".to_owned(),
            "run_fixture_001".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &0,
        "recorder tail initialized store exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "recorder tail JSON response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "recorder tail stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.recorder.tail.v1"),
        "recorder tail response schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/runId",
        json!("run_fixture_001"),
        "recorder tail run id",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/events",
        json!([]),
        "recorder tail initialized empty events",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/totalEvents",
        json!(0),
        "recorder tail total events",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/hasMore",
        json!(false),
        "recorder tail has more flag",
    )?;

    let fake_success =
        validate_no_fake_success_output("recorder tail", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("recorder tail output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("recorder tail", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "recorder tail output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!([]),
        "logged recorder tail degradation codes",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        Value::Null,
        "logged recorder tail repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("recorder"),
        "logged recorder tail boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("read-only recorder event stream; no recorder mutation"),
        "logged recorder tail side-effect class",
    )
}

#[test]
fn demo_commands_parse_manifests_and_verify_real_artifacts() -> TestResult {
    let workspace_root = unique_artifact_dir("demo-real-workspace")?;
    let workspace = workspace_root.join("workspace");
    let artifacts = workspace.join("artifacts");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    fs::create_dir_all(&artifacts).map_err(|error| {
        format!(
            "failed to create artifacts dir {}: {error}",
            artifacts.display()
        )
    })?;
    let artifact_payload = b"{\"schema\":\"ee.response.v1\",\"success\":true}\n";
    let artifact_hash = blake3::hash(artifact_payload).to_hex().to_string();
    fs::write(
        workspace.join("demo.yaml"),
        format!(
            "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000001
    title: real artifact verification
    description: verifies declared artifact bytes instead of placeholders
    tags:
      - gate14
    commands:
      - command: \"printf '%s\\\\n' '{{\\\"schema\\\":\\\"ee.response.v1\\\",\\\"success\\\":true}}' > artifacts/stdout.json\"
        expected_exit_code: 0
        artifact_outputs:
          - path: stdout.json
            blake3_hash: {artifact_hash}
            size_bytes: {}
",
            artifact_payload.len()
        ),
    )
    .map_err(|error| format!("failed to write demo.yaml: {error}"))?;
    let workspace_arg = workspace.display().to_string();
    let demo_id = "demo_00000000000000000000000001".to_owned();
    let evidence_root = workspace_root.join("demo-evidence");

    let init_result = run_ee_logged(
        "demo-init-real",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "init".to_owned(),
        ],
    )?;
    ensure_equal(&init_result.exit_code, &0, "demo init exit code")?;

    let list_result = run_ee_logged(
        "demo-list-real",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "demo".to_owned(),
            "list".to_owned(),
        ],
    )?;
    ensure_equal(&list_result.exit_code, &0, "demo list exit code")?;
    ensure(list_result.stderr.is_empty(), "demo list JSON stderr empty")?;
    ensure_no_ansi(&list_result.stdout, "demo list stdout")?;
    ensure_json_pointer(
        &list_result.parsed,
        "/success",
        json!(true),
        "demo list success",
    )?;
    ensure_json_pointer(
        &list_result.parsed,
        "/data/schema",
        json!("ee.demo.list.v1"),
        "demo list schema",
    )?;
    ensure_json_pointer(
        &list_result.parsed,
        "/data/demos/0/id",
        json!(demo_id.clone()),
        "demo list id",
    )?;

    let run_plan = run_ee_logged(
        "demo-run-dry-run-real",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "demo".to_owned(),
            "run".to_owned(),
            demo_id.clone(),
            "--dry-run".to_owned(),
        ],
    )?;
    ensure_equal(&run_plan.exit_code, &0, "demo run dry-run exit code")?;
    ensure(
        run_plan.stderr.is_empty(),
        "demo run dry-run JSON stderr empty",
    )?;
    ensure_json_pointer(
        &run_plan.parsed,
        "/success",
        json!(true),
        "demo run dry-run success",
    )?;
    ensure_json_pointer(
        &run_plan.parsed,
        "/data/dryRun",
        json!(true),
        "demo run dry-run flag",
    )?;
    ensure_json_pointer(
        &run_plan.parsed,
        "/data/demos/0/commands/0/executed",
        json!(false),
        "demo run dry-run does not execute",
    )?;

    let execution_result = run_ee_logged_with_env(
        "demo-run-executes-real",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "demo".to_owned(),
            "run".to_owned(),
            demo_id.clone(),
        ],
        &[("EE_DEMO_EVIDENCE_ROOT", evidence_root.display().to_string())],
    )?;
    ensure_equal(&execution_result.exit_code, &0, "demo run exit code")?;
    ensure(
        execution_result.stderr.is_empty(),
        "demo run JSON stderr empty",
    )?;
    ensure_json_pointer(
        &execution_result.parsed,
        "/success",
        json!(true),
        "demo run success",
    )?;
    ensure_json_pointer(
        &execution_result.parsed,
        "/data/dryRun",
        json!(false),
        "demo run non-dry-run flag",
    )?;
    ensure_json_pointer(
        &execution_result.parsed,
        "/data/demos/0/commands/0/executed",
        json!(true),
        "demo run executes command",
    )?;
    ensure_json_pointer(
        &execution_result.parsed,
        "/data/demos/0/commands/0/status",
        json!("passed"),
        "demo run command status",
    )?;
    let audit_id = execution_result
        .parsed
        .pointer("/data/auditIds/0")
        .and_then(Value::as_str)
        .ok_or_else(|| "demo run should return first audit id".to_string())?
        .to_owned();
    let evidence_dir = execution_result
        .parsed
        .pointer("/data/demos/0/commands/0/evidenceDir")
        .and_then(Value::as_str)
        .ok_or_else(|| "demo run should return evidence dir".to_string())?;
    ensure(
        Path::new(evidence_dir).join("stdout.txt").is_file(),
        "demo run writes stdout evidence artifact",
    )?;
    ensure(
        Path::new(evidence_dir).join("metadata.json").is_file(),
        "demo run writes metadata evidence artifact",
    )?;

    let show_result = run_ee_logged(
        "demo-show-real",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "demo".to_owned(),
            "show".to_owned(),
            demo_id.clone(),
        ],
    )?;
    ensure_equal(&show_result.exit_code, &0, "demo show exit code")?;
    ensure_json_pointer(
        &show_result.parsed,
        "/data/schema",
        json!("ee.demo.show.v1"),
        "demo show schema",
    )?;
    ensure_json_pointer(
        &show_result.parsed,
        "/data/rowCount",
        json!(1),
        "demo show audit row count",
    )?;
    ensure_json_pointer(
        &show_result.parsed,
        "/data/rows/0/auditId",
        json!(audit_id),
        "demo show audit id",
    )?;

    let verify_result = run_ee_logged(
        "demo-verify-real-artifact",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "demo".to_owned(),
            "verify".to_owned(),
            demo_id.clone(),
        ],
    )?;
    ensure_equal(&verify_result.exit_code, &0, "demo verify exit code")?;
    ensure(
        verify_result.stderr.is_empty(),
        "demo verify JSON stderr empty",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/success",
        json!(true),
        "demo verify success",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/data/checkedArtifacts",
        json!(1),
        "demo verify checked artifact count",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/data/demos/0/artifactResults/0/actualBlake3",
        json!(artifact_hash),
        "demo verify actual artifact hash",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/data/demos/0/artifactResults/0/verified",
        json!(true),
        "demo verify artifact verified",
    )?;

    let no_artifact_demo_id = "demo_00000000000000000000000002".to_owned();
    fs::write(
        workspace.join("demo.yaml"),
        "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000002
    title: missing artifact evidence
    description: verify must not report success without artifact evidence
    commands:
      - command: \"ee status --json\"
        expected_exit_code: 0
",
    )
    .map_err(|error| format!("failed to write no-artifact demo.yaml: {error}"))?;
    let no_artifact_verify = run_ee_logged(
        "demo-verify-no-artifact-evidence",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "demo".to_owned(),
            "verify".to_owned(),
            no_artifact_demo_id,
        ],
    )?;
    ensure_equal(
        &no_artifact_verify.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "demo verify without artifact evidence exit code",
    )?;
    ensure_json_pointer(
        &no_artifact_verify.parsed,
        "/success",
        json!(false),
        "demo verify without artifact evidence success",
    )?;
    ensure_json_pointer(
        &no_artifact_verify.parsed,
        "/data/checkedArtifacts",
        json!(0),
        "demo verify without artifact evidence checked artifacts",
    )?;
    ensure_json_pointer(
        &no_artifact_verify.parsed,
        "/data/failedDemos",
        json!(1),
        "demo verify without artifact evidence failed demos",
    )?;
    ensure_json_pointer(
        &no_artifact_verify.parsed,
        "/data/demos/0/status",
        json!("failed"),
        "demo verify without artifact evidence status",
    )?;
    ensure_json_pointer(
        &no_artifact_verify.parsed,
        "/data/demos/0/verificationError",
        json!("no artifact outputs declared for selected demo"),
        "demo verify without artifact evidence error",
    )?;

    let optional_missing_demo_id = "demo_00000000000000000000000003".to_owned();
    fs::write(
        workspace.join("demo.yaml"),
        format!(
            "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000003
    title: optional missing artifact is not evidence
    description: optional missing artifacts must not prove a demo by themselves
    commands:
      - command: \"ee status --json\"
        expected_exit_code: 0
        artifact_outputs:
          - path: optional.json
            optional: true
            blake3_hash: {artifact_hash}
            size_bytes: {}
",
            artifact_payload.len()
        ),
    )
    .map_err(|error| format!("failed to write optional-missing demo.yaml: {error}"))?;
    let optional_missing_verify = run_ee_logged(
        "demo-verify-optional-missing-artifact",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "demo".to_owned(),
            "verify".to_owned(),
            optional_missing_demo_id,
        ],
    )?;
    ensure_equal(
        &optional_missing_verify.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "demo verify optional missing artifact exit code",
    )?;
    ensure_json_pointer(
        &optional_missing_verify.parsed,
        "/success",
        json!(false),
        "demo verify optional missing artifact success",
    )?;
    ensure_json_pointer(
        &optional_missing_verify.parsed,
        "/data/checkedArtifacts",
        json!(1),
        "demo verify optional missing artifact checked artifacts",
    )?;
    ensure_json_pointer(
        &optional_missing_verify.parsed,
        "/data/optionalMissingArtifacts",
        json!(1),
        "demo verify optional missing artifact count",
    )?;
    ensure_json_pointer(
        &optional_missing_verify.parsed,
        "/data/failedDemos",
        json!(1),
        "demo verify optional missing artifact failed demos",
    )?;
    ensure_json_pointer(
        &optional_missing_verify.parsed,
        "/data/demos/0/status",
        json!("failed"),
        "demo verify optional missing artifact status",
    )?;
    ensure_json_pointer(
        &optional_missing_verify.parsed,
        "/data/demos/0/verificationError",
        json!("no artifact evidence files found for selected demo"),
        "demo verify optional missing artifact error",
    )?;

    for (command, result) in [
        ("demo list", &list_result),
        ("demo run --dry-run", &run_plan),
        ("demo run", &execution_result),
        ("demo show", &show_result),
        ("demo verify", &verify_result),
        ("demo verify without artifacts", &no_artifact_verify),
        (
            "demo verify optional missing artifacts",
            &optional_missing_verify,
        ),
    ] {
        let success = success_flag(&result.parsed);
        let fake_success = validate_no_fake_success_output(command, success, false, &result.stdout);
        ensure(
            fake_success.passed,
            format!("{command} output should not be fake success: {fake_success:?}"),
        )?;

        let unsupported_claims =
            validate_no_unsupported_evidence_claims(command, success, false, &result.stdout);
        ensure(
            unsupported_claims.passed,
            format!(
                "{command} output should not count as unsupported success: {unsupported_claims:?}"
            ),
        )?;
    }

    Ok(())
}

#[test]
fn demo_run_aborts_after_failure_and_rejects_unsafe_steps() -> TestResult {
    let workspace_root = unique_artifact_dir("demo-failure-workspace")?;
    let workspace = workspace_root.join("workspace");
    let artifacts = workspace.join("artifacts");
    fs::create_dir_all(&artifacts).map_err(|error| {
        format!(
            "failed to create artifacts dir {}: {error}",
            artifacts.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();
    let evidence_root = workspace_root.join("demo-evidence");
    let demo_id = "demo_00000000000000000000000011".to_owned();

    let init_result = run_ee_logged(
        "demo-failure-init",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "init".to_owned(),
        ],
    )?;
    ensure_equal(&init_result.exit_code, &0, "demo failure init exit")?;

    fs::write(
        workspace.join("demo.yaml"),
        "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000011
    title: abort after failure
    description: failed first step skips later steps
    commands:
      - command: \"exit 3\"
        expected_exit_code: 0
      - command: \"printf skipped > artifacts/after.txt\"
        expected_exit_code: 0
",
    )
    .map_err(|error| format!("failed to write failing demo.yaml: {error}"))?;

    let failed_run = run_ee_logged_with_env(
        "demo-run-fails-and-skips",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "demo".to_owned(),
            "run".to_owned(),
            demo_id.clone(),
        ],
        &[("EE_DEMO_EVIDENCE_ROOT", evidence_root.display().to_string())],
    )?;
    ensure_equal(&failed_run.exit_code, &1, "failed demo run exit")?;
    ensure_json_pointer(
        &failed_run.parsed,
        "/success",
        json!(false),
        "failed demo run success",
    )?;
    ensure_json_pointer(
        &failed_run.parsed,
        "/data/firstFailure/stepIndex",
        json!(0),
        "failed demo first failure step",
    )?;
    ensure_json_pointer(
        &failed_run.parsed,
        "/data/demos/0/commands/1/executed",
        json!(false),
        "failed demo skips second step",
    )?;
    ensure_json_pointer(
        &failed_run.parsed,
        "/data/demos/0/commands/1/status",
        json!("skipped"),
        "failed demo skipped status",
    )?;
    ensure(
        !artifacts.join("after.txt").exists(),
        "skipped demo step must not write artifact",
    )?;

    fs::write(
        workspace.join("demo.yaml"),
        "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000011
    title: unsafe command
    description: destructive commands are rejected before execution
    commands:
      - command: \"rm -rf target\"
        expected_exit_code: 0
",
    )
    .map_err(|error| format!("failed to write unsafe demo.yaml: {error}"))?;
    let unsafe_run = run_ee_logged_with_env(
        "demo-run-rejects-unsafe",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "demo".to_owned(),
            "run".to_owned(),
            demo_id,
        ],
        &[("EE_DEMO_EVIDENCE_ROOT", evidence_root.display().to_string())],
    )?;
    ensure_equal(&unsafe_run.exit_code, &7, "unsafe demo exit")?;
    ensure_json_pointer(
        &unsafe_run.parsed,
        "/error/code",
        json!("policy_denied"),
        "unsafe demo error code",
    )
}

#[cfg(unix)]
#[test]
fn demo_verify_rejects_symlink_artifact_evidence() -> TestResult {
    let workspace_root = unique_artifact_dir("demo-symlink-artifact-workspace")?;
    let workspace = workspace_root.join("workspace");
    let artifacts = workspace.join("artifacts");
    fs::create_dir_all(&artifacts).map_err(|error| {
        format!(
            "failed to create artifacts dir {}: {error}",
            artifacts.display()
        )
    })?;

    let outside_artifact = workspace_root.join("outside.json");
    let artifact_payload = b"{\"schema\":\"ee.response.v1\",\"success\":true}\n";
    let artifact_hash = blake3::hash(artifact_payload).to_hex().to_string();
    fs::write(&outside_artifact, artifact_payload)
        .map_err(|error| format!("failed to write outside artifact: {error}"))?;
    std::os::unix::fs::symlink(&outside_artifact, artifacts.join("stdout.json"))
        .map_err(|error| format!("failed to create artifact symlink: {error}"))?;

    fs::write(
        workspace.join("demo.yaml"),
        format!(
            "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000001
    title: symlink artifact evidence
    description: symlinked artifacts must not verify outside evidence bytes
    commands:
      - command: \"ee status --json\"
        expected_exit_code: 0
        artifact_outputs:
          - path: stdout.json
            blake3_hash: {artifact_hash}
            size_bytes: {}
",
            artifact_payload.len()
        ),
    )
    .map_err(|error| format!("failed to write demo.yaml: {error}"))?;

    let verify_result = run_ee_logged(
        "demo-verify-symlink-artifact",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace.display().to_string(),
            "--json".to_owned(),
            "demo".to_owned(),
            "verify".to_owned(),
            "demo_00000000000000000000000001".to_owned(),
        ],
    )?;

    ensure_equal(
        &verify_result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "demo verify symlink artifact exit code",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/success",
        json!(false),
        "demo verify symlink artifact success",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/data/failedArtifacts",
        json!(1),
        "demo verify symlink artifact failed artifact count",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/data/failedDemos",
        json!(1),
        "demo verify symlink artifact failed demo count",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/data/demos/0/artifactResults/0/exists",
        json!(true),
        "demo verify symlink artifact exists flag",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/data/demos/0/artifactResults/0/verified",
        json!(false),
        "demo verify symlink artifact verified flag",
    )?;
    ensure_json_pointer(
        &verify_result.parsed,
        "/data/demos/0/artifactResults/0/error",
        json!("artifact path traverses a symbolic link"),
        "demo verify symlink artifact error",
    )
}

#[test]
fn successful_validity_claims_need_concrete_evidence_sources() -> TestResult {
    let outputs = [
        (
            "certificate verify",
            r#"{"schema":"ee.certificate.verify.v1","success":true,"data":{"result":"valid","hashVerified":true,"message":"Certificate verification passed"}}"#,
        ),
        (
            "causal estimate",
            r#"{"schema":"ee.response.v1","success":true,"data":{"uplift":0.12,"confidenceState":"medium"}}"#,
        ),
        (
            "lab replay",
            r#"{"schema":"ee.response.v1","success":true,"data":{"replayOutcome":"success","episodeHashVerified":true}}"#,
        ),
    ];

    for (command, output) in outputs {
        let report = validate_no_unsupported_evidence_claims(command, true, false, output);
        ensure(
            !report.passed,
            format!("{command} should reject unsupported successful evidence claims"),
        )?;
    }

    Ok(())
}

#[test]
fn successful_validity_claims_pass_with_concrete_evidence_sources() -> TestResult {
    let report = validate_no_unsupported_evidence_claims(
        "certificate verify",
        true,
        false,
        r#"{"schema":"ee.certificate.verify.v1","success":true,"data":{"result":"valid","hashVerified":true,"manifestHash":"blake3:manifest","payloadHash":"blake3:payload"}}"#,
    );

    ensure(
        report.passed,
        format!("manifest-backed validity claim should pass: {report:?}"),
    )
}
