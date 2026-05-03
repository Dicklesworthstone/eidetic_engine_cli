use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::core::degraded_honesty::{
    validate_no_fake_success_output, validate_no_unsupported_evidence_claims,
};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const UNSATISFIED_DEGRADED_MODE_EXIT: i32 = 7;

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
    if actual == expected {
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

    for pointer in ["/degraded", "/data/degraded"] {
        if let Some(items) = value.pointer(pointer).and_then(Value::as_array) {
            for item in items {
                if let Some(code) = item.get("code").and_then(Value::as_str) {
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
    if args.iter().any(|arg| arg == "context") {
        "context, pack, search, why"
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
    } else if args.iter().any(|arg| arg == "handoff") {
        "handoff"
    } else if args.iter().any(|arg| arg == "recorder") {
        "recorder"
    } else if args.iter().any(|arg| arg == "demo") {
        "demo"
    } else {
        "unknown"
    }
}

fn side_effect_class(args: &[String]) -> &'static str {
    if args.iter().any(|arg| arg == "context") {
        "audited pack write when storage is available; storage error before mutation here"
    } else if args.windows(2).any(
        |window| matches!(window, [first, second] if first == "diag" && second == "quarantine"),
    ) {
        "read-only, conservative abstention; no source trust state read"
    } else if args
        .iter()
        .any(|arg| arg == "capabilities" || arg == "certificate")
    {
        "read-only, idempotent"
    } else if args.iter().any(|arg| arg == "claim") {
        "read-only, conservative abstention; no claim manifest parse or verification result"
    } else if args.iter().any(|arg| arg == "rehearse") {
        "unavailable before sandbox mutation"
    } else if args.iter().any(|arg| arg == "learn") {
        "conservative abstention; no learning agenda, uncertainty, summary, proposal, or experiment template emitted"
    } else if args.iter().any(|arg| arg == "lab") {
        "unavailable before lab episode capture, replay, or counterfactual mutation"
    } else if args.iter().any(|arg| arg == "economy" || arg == "causal") {
        "read-only, conservative abstention"
    } else if args.iter().any(|arg| arg == "procedure") {
        "conservative abstention; no procedure mutation or artifact write"
    } else if args.iter().any(|arg| arg == "situation") {
        "conservative abstention; no situation routing, link, or recommendation mutation"
    } else if args.iter().any(|arg| arg == "plan") {
        "conservative abstention; no goal classification or recipe explanation"
    } else if args.iter().any(|arg| arg == "handoff") {
        "conservative abstention; no continuity capsule write"
    } else if args.iter().any(|arg| arg == "recorder") {
        "read-only, conservative abstention; no recorder tail or follow snapshot"
    } else if args.iter().any(|arg| arg == "demo") {
        "conservative abstention; no demo execution, verification, or artifact write"
    } else {
        "unknown"
    }
}

fn run_ee_logged(
    name: &str,
    workspace: Option<&Path>,
    args: Vec<String>,
) -> Result<LoggedCommand, String> {
    let dossier_dir = unique_artifact_dir(name)?;
    let stdout_path = dossier_dir.join("stdout.json");
    let stderr_path = dossier_dir.join("stderr.txt");
    let log_path = dossier_dir.join("e2e-log.json");
    let cwd = env::current_dir().map_err(|error| format!("failed to resolve cwd: {error}"))?;

    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(&args)
        .current_dir(&cwd)
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
fn certificate_verify_degrades_instead_of_reporting_mock_success() -> TestResult {
    let result = run_ee_logged(
        "certificate-verify-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "certificate".to_owned(),
            "verify".to_owned(),
            "cert_pack_001".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "certificate unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "certificate JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "certificate degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "certificate degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("certificate_store_unavailable"),
        "certificate degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("certificate_store_unavailable"),
        "certificate degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-v76q"),
        "certificate follow-up bead",
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
        json!(["certificate_store_unavailable"]),
        "logged certificate degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee doctor --json"),
        "logged certificate repair command",
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
fn claim_commands_degrade_instead_of_reporting_empty_placeholder_results() -> TestResult {
    let workspace_root = unique_artifact_dir("claim-unavailable-workspace")?;
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
            "claim-list-unavailable",
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
            "claim-show-unavailable",
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
            "claim-verify-unavailable",
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
            &UNSATISFIED_DEGRADED_MODE_EXIT,
            &format!("{command} unavailable exit code"),
        )?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON degraded response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} degraded stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!("ee.response.v1"),
            &format!("{command} degraded response schema"),
        )?;
        ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
        ensure_json_pointer(
            &result.parsed,
            "/data/command",
            json!(command),
            &format!("{command} command label"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("claim_verification_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("claim_verification_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-v76q"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!(
                "read-only, conservative abstention; no claim manifest parse or verification result"
            ),
            &format!("{command} side-effect class"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/evidenceIds",
            json!([]),
            &format!("{command} evidence ids"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sourceIds",
            json!([]),
            &format!("{command} source ids"),
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
                "degraded {command} output should not count as unsupported success: {unsupported_claims:?}"
            ),
        )?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!(["claim_verification_unavailable"]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            json!("ee status --json"),
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
            json!(
                "read-only, conservative abstention; no claim manifest parse or verification result"
            ),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
}

#[test]
fn diag_quarantine_degrades_instead_of_reporting_placeholder_health() -> TestResult {
    let result = run_ee_logged(
        "diag-quarantine-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "diag".to_owned(),
            "quarantine".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "diag quarantine unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "diag quarantine JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "diag quarantine degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "diag quarantine degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/command",
        json!("diag quarantine"),
        "diag quarantine command label",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("quarantine_trust_state_unavailable"),
        "diag quarantine degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("quarantine_trust_state_unavailable"),
        "diag quarantine degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-5g6d"),
        "diag quarantine follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sideEffectClass",
        json!("read-only, conservative abstention; no source trust state read"),
        "diag quarantine side-effect class",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "diag quarantine evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "diag quarantine source ids",
    )?;

    let fake_success =
        validate_no_fake_success_output("diag quarantine", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded diag quarantine output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("diag quarantine", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded diag quarantine output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["quarantine_trust_state_unavailable"]),
        "logged diag quarantine degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee status --json"),
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
        json!("read-only, conservative abstention; no source trust state read"),
        "logged diag quarantine side-effect class",
    )
}

#[test]
fn rehearse_run_degrades_instead_of_reporting_simulated_success() -> TestResult {
    let result = run_ee_logged(
        "rehearse-run-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "rehearse".to_owned(),
            "run".to_owned(),
            "--profile".to_owned(),
            "quick".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "rehearse unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "rehearse JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "rehearse degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "rehearse degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("rehearsal_unavailable"),
        "rehearse degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("rehearsal_unavailable"),
        "rehearse degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-nd65"),
        "rehearse follow-up bead",
    )?;

    let fake_success =
        validate_no_fake_success_output("rehearse run", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded rehearse output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("rehearse run", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded rehearse output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["rehearsal_unavailable"]),
        "logged rehearse degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee rehearse plan --json"),
        "logged rehearse repair command",
    )?;
    ensure_json_pointer(
        &log_json,
        "/commandBoundaryMatrixRow",
        json!("rehearse"),
        "logged rehearse boundary matrix row",
    )?;
    ensure_json_pointer(
        &log_json,
        "/sideEffectClass",
        json!("unavailable before sandbox mutation"),
        "logged rehearse side-effect class",
    )
}

#[test]
fn learn_read_and_proposal_commands_degrade_instead_of_reporting_seed_templates() -> TestResult {
    let commands = [
        (
            "learn-agenda-unavailable",
            "learn agenda",
            vec![
                "--json".to_owned(),
                "learn".to_owned(),
                "agenda".to_owned(),
                "--limit".to_owned(),
                "2".to_owned(),
            ],
        ),
        (
            "learn-uncertainty-unavailable",
            "learn uncertainty",
            vec![
                "--json".to_owned(),
                "learn".to_owned(),
                "uncertainty".to_owned(),
                "--min-uncertainty".to_owned(),
                "0.3".to_owned(),
            ],
        ),
        (
            "learn-experiment-propose-unavailable",
            "learn experiment propose",
            vec![
                "--json".to_owned(),
                "learn".to_owned(),
                "experiment".to_owned(),
                "propose".to_owned(),
            ],
        ),
        (
            "learn-experiment-run-unavailable",
            "learn experiment run",
            vec![
                "--json".to_owned(),
                "learn".to_owned(),
                "experiment".to_owned(),
                "run".to_owned(),
                "--id".to_owned(),
                "exp_database_contract_fixture".to_owned(),
                "--dry-run".to_owned(),
            ],
        ),
        (
            "learn-summary-unavailable",
            "learn summary",
            vec![
                "--json".to_owned(),
                "learn".to_owned(),
                "summary".to_owned(),
            ],
        ),
    ];

    for (artifact_name, command, args) in commands {
        let result = run_ee_logged(artifact_name, None, args)?;

        ensure_equal(
            &result.exit_code,
            &UNSATISFIED_DEGRADED_MODE_EXIT,
            &format!("{command} unavailable exit code"),
        )?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON degraded response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} degraded stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!("ee.response.v1"),
            &format!("{command} degraded response schema"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/success",
            json!(false),
            &format!("{command} success flag"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("learning_records_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("learning_records_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/repair",
            json!("ee learn observe <experiment-id> --dry-run --json"),
            &format!("{command} repair command"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-evah"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!(
                "conservative abstention; no learning agenda, uncertainty, summary, proposal, or experiment template emitted"
            ),
            &format!("{command} side-effect class"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/evidenceIds",
            json!([]),
            &format!("{command} evidence ids"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sourceIds",
            json!([]),
            &format!("{command} source ids"),
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
                "degraded {command} output should not count as unsupported success: {unsupported_claims:?}"
            ),
        )?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!(["learning_records_unavailable"]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            json!("ee learn observe <experiment-id> --dry-run --json"),
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("learn"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!(
                "conservative abstention; no learning agenda, uncertainty, summary, proposal, or experiment template emitted"
            ),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
}

#[test]
fn lab_replay_degrades_instead_of_reporting_generated_replay_success() -> TestResult {
    let result = run_ee_logged(
        "lab-replay-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "lab".to_owned(),
            "replay".to_owned(),
            "ep_fixture_001".to_owned(),
            "--dry-run".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "lab unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "lab JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "lab degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "lab degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("lab_replay_unavailable"),
        "lab degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("lab_replay_unavailable"),
        "lab degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-db4z"),
        "lab follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sideEffectClass",
        json!("unavailable before lab episode capture, replay, or counterfactual mutation"),
        "lab side-effect class",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "lab evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "lab source ids",
    )?;

    let fake_success = validate_no_fake_success_output("lab replay", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded lab output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("lab replay", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded lab output should not count as unsupported success: {unsupported_claims:?}"
        ),
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
        json!("ee status --json"),
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
        json!("unavailable before lab episode capture, replay, or counterfactual mutation"),
        "logged lab side-effect class",
    )
}

#[test]
fn economy_report_degrades_instead_of_reporting_seed_metrics() -> TestResult {
    let result = run_ee_logged(
        "economy-report-unavailable",
        None,
        vec![
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
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "economy degraded response schema",
    )?;
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
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["economy_metrics_unavailable"]),
        "logged economy degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee status --json"),
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
fn causal_trace_degrades_instead_of_reporting_generated_chains() -> TestResult {
    let result = run_ee_logged(
        "causal-trace-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "causal".to_owned(),
            "trace".to_owned(),
            "--run-id".to_owned(),
            "run_fixture_001".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "causal unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "causal JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "causal degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "causal degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("causal_evidence_unavailable"),
        "causal degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("causal_evidence_unavailable"),
        "causal degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-dz00"),
        "causal follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "causal evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "causal source ids",
    )?;

    let fake_success =
        validate_no_fake_success_output("causal trace", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded causal output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("causal trace", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded causal output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["causal_evidence_unavailable"]),
        "logged causal degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee status --json"),
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
        json!("read-only, conservative abstention"),
        "logged causal side-effect class",
    )
}

#[test]
fn procedure_list_degrades_instead_of_reporting_generated_records() -> TestResult {
    let result = run_ee_logged(
        "procedure-list-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "procedure".to_owned(),
            "list".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "procedure unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "procedure JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "procedure degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "procedure degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("procedure_store_unavailable"),
        "procedure degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("procedure_store_unavailable"),
        "procedure degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-q5vf"),
        "procedure follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sideEffectClass",
        json!("conservative abstention; no procedure mutation or artifact write"),
        "procedure side-effect class",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "procedure evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "procedure source ids",
    )?;

    let fake_success =
        validate_no_fake_success_output("procedure list", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded procedure output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("procedure list", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded procedure output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["procedure_store_unavailable"]),
        "logged procedure degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee status --json"),
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
        json!("conservative abstention; no procedure mutation or artifact write"),
        "logged procedure side-effect class",
    )
}

#[test]
fn situation_classify_degrades_instead_of_reporting_builtin_routing() -> TestResult {
    let result = run_ee_logged(
        "situation-classify-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "situation".to_owned(),
            "classify".to_owned(),
            "fix failing release workflow".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "situation unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "situation JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "situation degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "situation degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("situation_decisioning_unavailable"),
        "situation degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("situation_decisioning_unavailable"),
        "situation degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-6cks"),
        "situation follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sideEffectClass",
        json!("conservative abstention; no situation routing, link, or recommendation mutation"),
        "situation side-effect class",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "situation evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "situation source ids",
    )?;

    let fake_success =
        validate_no_fake_success_output("situation classify", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded situation output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("situation classify", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded situation output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["situation_decisioning_unavailable"]),
        "logged situation degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee status --json"),
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
        json!("conservative abstention; no situation routing, link, or recommendation mutation"),
        "logged situation side-effect class",
    )
}

#[test]
fn plan_goal_and_explain_degrade_instead_of_reporting_builtin_reasoning() -> TestResult {
    let commands = [
        (
            "plan-goal-unavailable",
            "plan goal",
            vec![
                "--json".to_owned(),
                "plan".to_owned(),
                "goal".to_owned(),
                "--goal".to_owned(),
                "fix failing release workflow".to_owned(),
            ],
        ),
        (
            "plan-explain-unavailable",
            "plan explain",
            vec![
                "--json".to_owned(),
                "plan".to_owned(),
                "explain".to_owned(),
                "init-workspace".to_owned(),
            ],
        ),
    ];

    for (artifact_name, command, args) in commands {
        let result = run_ee_logged(artifact_name, None, args)?;

        ensure_equal(
            &result.exit_code,
            &UNSATISFIED_DEGRADED_MODE_EXIT,
            &format!("{command} unavailable exit code"),
        )?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON degraded response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} degraded stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!("ee.response.v1"),
            &format!("{command} degraded response schema"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/success",
            json!(false),
            &format!("{command} success flag"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("plan_decisioning_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("plan_decisioning_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/repair",
            json!("ee plan recipe list --json"),
            &format!("{command} repair command"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-6cks"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!("conservative abstention; no goal classification or recipe explanation"),
            &format!("{command} side-effect class"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/evidenceIds",
            json!([]),
            &format!("{command} evidence ids"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sourceIds",
            json!([]),
            &format!("{command} source ids"),
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
                "degraded {command} output should not count as unsupported success: {unsupported_claims:?}"
            ),
        )?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!(["plan_decisioning_unavailable"]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            json!("ee plan recipe list --json"),
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
            json!("conservative abstention; no goal classification or recipe explanation"),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
}

#[test]
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
fn recorder_tail_degrades_instead_of_reporting_stubbed_empty_events() -> TestResult {
    let result = run_ee_logged(
        "recorder-tail-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "recorder".to_owned(),
            "tail".to_owned(),
            "run_fixture_001".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "recorder tail unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "recorder tail JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "recorder tail degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "recorder tail degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("recorder_tail_unavailable"),
        "recorder tail degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("recorder_tail_unavailable"),
        "recorder tail degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-6xzc"),
        "recorder tail follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sideEffectClass",
        json!("read-only, conservative abstention; no recorder tail or follow snapshot"),
        "recorder tail side-effect class",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "recorder tail evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "recorder tail source ids",
    )?;

    let fake_success =
        validate_no_fake_success_output("recorder tail", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded recorder tail output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("recorder tail", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded recorder tail output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["recorder_tail_unavailable"]),
        "logged recorder tail degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee status --json"),
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
        json!("read-only, conservative abstention; no recorder tail or follow snapshot"),
        "logged recorder tail side-effect class",
    )
}

#[test]
fn demo_commands_degrade_instead_of_reporting_pending_placeholders() -> TestResult {
    let workspace_root = unique_artifact_dir("demo-unavailable-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    fs::write(
        workspace.join("demo.yaml"),
        "demos:\n  - id: demo_fixture_001\n    title: placeholder execution must not pass\n",
    )
    .map_err(|error| format!("failed to write demo.yaml: {error}"))?;
    let workspace_arg = workspace.display().to_string();

    let cases = [
        (
            "demo-list-unavailable",
            "demo list",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "demo".to_owned(),
                "list".to_owned(),
            ],
        ),
        (
            "demo-run-unavailable",
            "demo run",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "demo".to_owned(),
                "run".to_owned(),
                "demo_fixture_001".to_owned(),
                "--dry-run".to_owned(),
            ],
        ),
        (
            "demo-verify-unavailable",
            "demo verify",
            vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "demo".to_owned(),
                "verify".to_owned(),
                "demo_fixture_001".to_owned(),
            ],
        ),
    ];

    for (name, command, args) in cases {
        let result = run_ee_logged(name, Some(&workspace), args)?;
        ensure_equal(
            &result.exit_code,
            &UNSATISFIED_DEGRADED_MODE_EXIT,
            &format!("{command} unavailable exit code"),
        )?;
        ensure(
            result.stderr.is_empty(),
            format!("{command} JSON degraded response must keep stderr empty"),
        )?;
        ensure_no_ansi(&result.stdout, &format!("{command} degraded stdout"))?;
        ensure_json_pointer(
            &result.parsed,
            "/schema",
            json!("ee.response.v1"),
            &format!("{command} degraded response schema"),
        )?;
        ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
        ensure_json_pointer(
            &result.parsed,
            "/data/command",
            json!(command),
            &format!("{command} command label"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("demo_execution_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("demo_execution_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-jp06.1"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!("conservative abstention; no demo execution, verification, or artifact write"),
            &format!("{command} side-effect class"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/evidenceIds",
            json!([]),
            &format!("{command} evidence ids"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sourceIds",
            json!([]),
            &format!("{command} source ids"),
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
                "degraded {command} output should not count as unsupported success: {unsupported_claims:?}"
            ),
        )?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!(["demo_execution_unavailable"]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            json!("ee status --json"),
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("demo"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!("conservative abstention; no demo execution, verification, or artifact write"),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
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
