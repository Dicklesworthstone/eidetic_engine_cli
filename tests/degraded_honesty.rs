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
    } else if args.iter().any(|arg| arg == "audit") {
        "audit"
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
    if args.iter().any(|arg| arg == "context") {
        "audited pack write when storage is available; storage error before mutation here"
    } else if args.iter().any(|arg| arg == "audit") {
        "read-only, conservative abstention; no audit log record or hash-chain verification emitted"
    } else if args.iter().any(|arg| arg == "support") {
        "conservative abstention; no support bundle archive, manifest, or verification emitted"
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
    } else if args.iter().any(|arg| arg == "preflight") {
        "conservative abstention; no preflight run, risk brief, or feedback ledger mutation"
    } else if args.iter().any(|arg| arg == "tripwire") {
        "read-only, conservative abstention; no tripwire store read or event evaluation"
    } else if args.iter().any(|arg| arg == "eval") {
        "read-only, conservative abstention; no fixture discovery, evaluation report, or science metrics emitted"
    } else if args.iter().any(|arg| arg == "review") {
        "read-only, conservative abstention; no session review or curation candidate proposal emitted"
    } else if args.iter().any(|arg| arg == "handoff") {
        "conservative abstention; no continuity capsule write"
    } else if args.iter().any(|arg| arg == "daemon") {
        "conservative abstention; no daemon tick, scheduler ledger, or maintenance job mutation"
    } else if args.iter().any(|arg| arg == "recorder") {
        if args.iter().any(|arg| arg == "tail") {
            "read-only, conservative abstention; no recorder tail or follow snapshot"
        } else {
            "conservative abstention; no recorder session, event, hash-chain, or finish mutation"
        }
    } else if args.iter().any(|arg| arg == "demo") {
        if args.iter().any(|arg| arg == "verify") {
            "read-only artifact verification; no command execution or artifact write"
        } else if args.iter().any(|arg| arg == "run") && args.iter().any(|arg| arg == "--dry-run") {
            "dry-run plan; no command execution, audit ledger, or artifact write"
        } else if args.iter().any(|arg| arg == "run") {
            "conservative abstention; no demo execution, audit ledger, or artifact write"
        } else {
            "read-only manifest parse; no command execution or artifact write"
        }
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
fn audit_commands_degrade_instead_of_reporting_generated_operation_records() -> TestResult {
    let cases = [
        (
            "audit-timeline-unavailable",
            "audit timeline",
            vec![
                "--json".to_owned(),
                "audit".to_owned(),
                "timeline".to_owned(),
            ],
        ),
        (
            "audit-show-unavailable",
            "audit show",
            vec![
                "--json".to_owned(),
                "audit".to_owned(),
                "show".to_owned(),
                "op_fixture_001".to_owned(),
            ],
        ),
        (
            "audit-diff-unavailable",
            "audit diff",
            vec![
                "--json".to_owned(),
                "audit".to_owned(),
                "diff".to_owned(),
                "op_fixture_001".to_owned(),
            ],
        ),
        (
            "audit-verify-unavailable",
            "audit verify",
            vec!["--json".to_owned(), "audit".to_owned(), "verify".to_owned()],
        ),
    ];

    for (artifact_name, command, args) in cases {
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
            "/data/command",
            json!(command),
            &format!("{command} command label"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("audit_log_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("audit_log_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/repair",
            json!("ee status --json"),
            &format!("{command} repair command"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-s43e"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!(
                "read-only, conservative abstention; no audit log record or hash-chain verification emitted"
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
        ensure(
            result.parsed.pointer("/entries").is_none()
                && result.parsed.pointer("/operation").is_none()
                && result.parsed.pointer("/deltas").is_none()
                && result.parsed.pointer("/summary").is_none(),
            format!("{command} must not emit generated audit records or verification summary"),
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
            json!(["audit_log_unavailable"]),
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
            json!("audit"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!(
                "read-only, conservative abstention; no audit log record or hash-chain verification emitted"
            ),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
}

#[test]
fn support_bundle_commands_degrade_instead_of_reporting_placeholder_archive_success() -> TestResult
{
    let artifact_dir = unique_artifact_dir("support-bundle-unavailable-inputs")?;
    let out_dir = artifact_dir.join("bundle-output");
    fs::create_dir_all(&out_dir)
        .map_err(|error| format!("failed to create {}: {error}", out_dir.display()))?;
    let bundle_path = artifact_dir.join("support_bundle.tar.gz");
    fs::write(&bundle_path, b"not a real support bundle")
        .map_err(|error| format!("failed to create {}: {error}", bundle_path.display()))?;

    let cases = [
        (
            "support-bundle-plan-unavailable",
            "support bundle",
            vec![
                "--json".to_owned(),
                "support".to_owned(),
                "bundle".to_owned(),
                "--dry-run".to_owned(),
            ],
            None,
        ),
        (
            "support-bundle-create-unavailable",
            "support bundle",
            vec![
                "--json".to_owned(),
                "support".to_owned(),
                "bundle".to_owned(),
                "--out".to_owned(),
                out_dir.display().to_string(),
            ],
            Some(out_dir.join("support_bundle.tar.gz")),
        ),
        (
            "support-inspect-unavailable",
            "support inspect",
            vec![
                "--json".to_owned(),
                "support".to_owned(),
                "inspect".to_owned(),
                bundle_path.display().to_string(),
                "--verify-hashes".to_owned(),
                "--check-versions".to_owned(),
            ],
            None,
        ),
    ];

    for (artifact_name, command, args, forbidden_output_path) in cases {
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
            "/data/command",
            json!(command),
            &format!("{command} command label"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("support_bundle_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("support_bundle_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/repair",
            json!("ee diag integrity --json"),
            &format!("{command} repair command"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-5g6d"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!(
                "conservative abstention; no support bundle archive, manifest, or verification emitted"
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
        ensure(
            result.parsed.pointer("/filesCollected").is_none()
                && result.parsed.pointer("/outputPath").is_none()
                && result.parsed.pointer("/hashVerified").is_none()
                && result.parsed.pointer("/versionInfo").is_none(),
            format!("{command} must not emit placeholder archive or verification fields"),
        )?;
        if let Some(path) = forbidden_output_path {
            ensure(
                !path.exists(),
                format!(
                    "{command} must not create placeholder bundle {}",
                    path.display()
                ),
            )?;
        }

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
            json!(["support_bundle_unavailable"]),
            &format!("logged {command} degradation code"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/repairCommand",
            json!("ee diag integrity --json"),
            &format!("logged {command} repair command"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/commandBoundaryMatrixRow",
            json!("support bundle"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!(
                "conservative abstention; no support bundle archive, manifest, or verification emitted"
            ),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
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
fn rehearse_commands_degrade_instead_of_reporting_simulated_success() -> TestResult {
    let command_spec = r#"[{
      "id":"cmd_status",
      "command":"status",
      "args":["--json"],
      "expected_effect":"read_only",
      "stop_on_failure":false,
      "idempotency_key":"idem-status-001"
    }]"#;
    let cases = [
        (
            "rehearse-plan-unavailable",
            "rehearse plan",
            vec![
                "--json".to_owned(),
                "rehearse".to_owned(),
                "plan".to_owned(),
                "--commands-json".to_owned(),
                command_spec.to_owned(),
            ],
        ),
        (
            "rehearse-run-unavailable",
            "rehearse run",
            vec![
                "--json".to_owned(),
                "rehearse".to_owned(),
                "run".to_owned(),
                "--profile".to_owned(),
                "quick".to_owned(),
            ],
        ),
        (
            "rehearse-inspect-unavailable",
            "rehearse inspect",
            vec![
                "--json".to_owned(),
                "rehearse".to_owned(),
                "inspect".to_owned(),
                "rrun_fixture_001".to_owned(),
            ],
        ),
        (
            "rehearse-promote-plan-unavailable",
            "rehearse promote-plan",
            vec![
                "--json".to_owned(),
                "rehearse".to_owned(),
                "promote-plan".to_owned(),
                "rrun_fixture_001".to_owned(),
            ],
        ),
    ];

    for (artifact_name, command, args) in cases {
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
            "/data/command",
            json!(command),
            &format!("{command} command label"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("rehearsal_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("rehearsal_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/repair",
            json!("ee status --json"),
            &format!("{command} repair command"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-nd65"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!("unavailable before sandbox mutation"),
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
        ensure(
            result.parsed.pointer("/plan_id").is_none()
                && result.parsed.pointer("/run_id").is_none()
                && result.parsed.pointer("/artifact_id").is_none()
                && result.parsed.pointer("/estimated_artifacts").is_none()
                && result.parsed.pointer("/sandbox_path").is_none()
                && result.parsed.pointer("/can_proceed").is_none()
                && result.parsed.pointer("/next_actions").is_none(),
            format!("{command} must not emit generated rehearsal artifacts or proceed guidance"),
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
            json!(["rehearsal_unavailable"]),
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
            json!("rehearse"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!("unavailable before sandbox mutation"),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
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

    let run_result = run_ee_logged(
        "learn-experiment-run-dry-run",
        None,
        vec![
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
        &0,
        "learn experiment run dry-run exit code",
    )?;
    ensure(
        run_result.stderr.is_empty(),
        "learn experiment run dry-run JSON stderr empty",
    )?;
    ensure_no_ansi(&run_result.stdout, "learn experiment run dry-run stdout")?;
    ensure_json_pointer(
        &run_result.parsed,
        "/schema",
        json!("ee.learn.experiment_run.v1"),
        "learn experiment run dry-run schema",
    )?;
    ensure_json_pointer(
        &run_result.parsed,
        "/dryRun",
        json!(true),
        "learn experiment run dry-run flag",
    )?;
    ensure_json_pointer(
        &run_result.parsed,
        "/status",
        json!("dry_run"),
        "learn experiment run dry-run status",
    )?;
    let fake_success =
        validate_no_fake_success_output("learn experiment run", true, true, &run_result.stdout);
    ensure(
        fake_success.passed,
        format!("learn experiment run dry-run output should not be fake success: {fake_success:?}"),
    )?;

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
fn eval_run_and_list_degrade_instead_of_reporting_no_scenario_stub_success() -> TestResult {
    let cases = [
        (
            "eval-run-unavailable",
            "eval run",
            vec![
                "--json".to_owned(),
                "eval".to_owned(),
                "run".to_owned(),
                "release_failure".to_owned(),
            ],
        ),
        (
            "eval-run-science-unavailable",
            "eval run",
            vec![
                "--json".to_owned(),
                "eval".to_owned(),
                "run".to_owned(),
                "--science".to_owned(),
            ],
        ),
        (
            "eval-list-unavailable",
            "eval list",
            vec!["--json".to_owned(), "eval".to_owned(), "list".to_owned()],
        ),
    ];

    for (artifact_name, command, args) in cases {
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
            "/data/command",
            json!(command),
            &format!("{command} command label"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("eval_fixtures_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("eval_fixtures_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/repair",
            json!("ee status --json"),
            &format!("{command} repair command"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-uiy3"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!(
                "read-only, conservative abstention; no fixture discovery, evaluation report, or science metrics emitted"
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
        ensure(
            result.parsed.pointer("/data/scienceMetrics").is_none(),
            format!("{command} must not emit science metrics without real eval results"),
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
            json!(["eval_fixtures_unavailable"]),
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
            json!("eval"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!(
                "read-only, conservative abstention; no fixture discovery, evaluation report, or science metrics emitted"
            ),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
}

#[test]
fn review_session_degrades_instead_of_reporting_empty_proposal_success() -> TestResult {
    let result = run_ee_logged(
        "review-session-unavailable",
        None,
        vec![
            "--json".to_owned(),
            "review".to_owned(),
            "session".to_owned(),
            "--propose".to_owned(),
        ],
    )?;

    ensure_equal(
        &result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "review session unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "review session JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "review session degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "review session degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/command",
        json!("review session"),
        "review session command label",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("review_evidence_unavailable"),
        "review session degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("review_evidence_unavailable"),
        "review session degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/repair",
        json!("ee import cass --workspace . --dry-run --json"),
        "review session repair command",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-0hjw"),
        "review session follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sideEffectClass",
        json!(
            "read-only, conservative abstention; no session review or curation candidate proposal emitted"
        ),
        "review session side-effect class",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "review session evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "review session source ids",
    )?;
    ensure(
        result.parsed.pointer("/data/candidates").is_none(),
        "review session must not emit empty candidate proposals without CASS evidence",
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
        json!(["review_evidence_unavailable"]),
        "logged review session degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee import cass --workspace . --dry-run --json"),
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
            "read-only, conservative abstention; no session review or curation candidate proposal emitted"
        ),
        "logged review session side-effect class",
    )
}

#[test]
fn preflight_and_tripwire_commands_degrade_instead_of_reporting_fixture_risk_state() -> TestResult {
    let workspace_root = unique_artifact_dir("preflight-tripwire-unavailable-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();

    let cases = [
        (
            "preflight-run-unavailable",
            "preflight run",
            "preflight_evidence_unavailable",
            "eidetic_engine_cli-bijm",
            "conservative abstention; no preflight run, risk brief, or feedback ledger mutation",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "preflight".to_owned(),
                "run".to_owned(),
                "deploy production database migration".to_owned(),
            ],
        ),
        (
            "preflight-show-unavailable",
            "preflight show",
            "preflight_evidence_unavailable",
            "eidetic_engine_cli-bijm",
            "conservative abstention; no preflight run, risk brief, or feedback ledger mutation",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "preflight".to_owned(),
                "show".to_owned(),
                "pf_gate16_contract".to_owned(),
            ],
        ),
        (
            "preflight-close-unavailable",
            "preflight close",
            "preflight_evidence_unavailable",
            "eidetic_engine_cli-bijm",
            "conservative abstention; no preflight run, risk brief, or feedback ledger mutation",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "preflight".to_owned(),
                "close".to_owned(),
                "pf_gate16_contract".to_owned(),
                "--cleared".to_owned(),
                "--task-outcome".to_owned(),
                "success".to_owned(),
                "--feedback".to_owned(),
                "helped".to_owned(),
                "--dry-run".to_owned(),
            ],
        ),
        (
            "tripwire-list-unavailable",
            "tripwire list",
            "tripwire_store_unavailable",
            "eidetic_engine_cli-qmu0",
            "read-only, conservative abstention; no tripwire store read or event evaluation",
            vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "tripwire".to_owned(),
                "list".to_owned(),
                "--include-disarmed".to_owned(),
            ],
        ),
        (
            "tripwire-check-unavailable",
            "tripwire check",
            "tripwire_store_unavailable",
            "eidetic_engine_cli-qmu0",
            "read-only, conservative abstention; no tripwire store read or event evaluation",
            vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "tripwire".to_owned(),
                "check".to_owned(),
                "tw_004".to_owned(),
                "--task-outcome".to_owned(),
                "success".to_owned(),
                "--dry-run".to_owned(),
            ],
        ),
    ];

    for (artifact_name, command, code, follow_up, side_effect, args) in cases {
        let result = run_ee_logged(artifact_name, Some(&workspace), args)?;

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
            "/data/command",
            json!(command),
            &format!("{command} command label"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!(code),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!(code),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/repair",
            json!("ee status --json"),
            &format!("{command} repair command"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!(follow_up),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!(side_effect),
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
            json!([code]),
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
            json!(command.split(' ').next().unwrap_or("unknown")),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!(side_effect),
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
fn daemon_foreground_degrades_instead_of_reporting_simulated_job_success() -> TestResult {
    let result = run_ee_logged(
        "daemon-foreground-unavailable",
        None,
        vec![
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
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "daemon unavailable exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "daemon JSON degraded response must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "daemon degraded stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "daemon degraded response schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(false), "success flag")?;
    ensure_json_pointer(
        &result.parsed,
        "/data/command",
        json!("daemon foreground"),
        "daemon command label",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/code",
        json!("daemon_jobs_unavailable"),
        "daemon degraded code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/degraded/0/code",
        json!("daemon_jobs_unavailable"),
        "daemon degraded array code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/repair",
        json!("ee status --json"),
        "daemon repair command",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/followUpBead",
        json!("eidetic_engine_cli-5g6d"),
        "daemon follow-up bead",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sideEffectClass",
        json!(
            "conservative abstention; no daemon tick, scheduler ledger, or maintenance job mutation"
        ),
        "daemon side-effect class",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/evidenceIds",
        json!([]),
        "daemon evidence ids",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/data/sourceIds",
        json!([]),
        "daemon source ids",
    )?;
    ensure(
        result.parsed.pointer("/data/schema").is_none()
            && result.parsed.pointer("/data/summary").is_none()
            && result.parsed.pointer("/data/jobTypes").is_none()
            && result.parsed.pointer("/data/ticks").is_none()
            && result.parsed.pointer("/data/runs").is_none(),
        "daemon degraded output must not emit scheduler schema, ticks, jobs, or run results",
    )?;
    ensure(
        !result.stdout.contains("health_check") && !result.stdout.contains("itemsProcessed"),
        "daemon degraded stdout must not claim simulated job work",
    )?;

    let fake_success =
        validate_no_fake_success_output("daemon foreground", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("degraded daemon output should not be fake success: {fake_success:?}"),
    )?;

    let unsupported_claims =
        validate_no_unsupported_evidence_claims("daemon foreground", false, false, &result.stdout);
    ensure(
        unsupported_claims.passed,
        format!(
            "degraded daemon output should not count as unsupported success: {unsupported_claims:?}"
        ),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["daemon_jobs_unavailable"]),
        "logged daemon degradation code",
    )?;
    ensure_json_pointer(
        &log_json,
        "/repairCommand",
        json!("ee status --json"),
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
        json!(
            "conservative abstention; no daemon tick, scheduler ledger, or maintenance job mutation"
        ),
        "logged daemon side-effect class",
    )
}

#[test]
fn recorder_start_event_finish_degrade_instead_of_reporting_generated_state() -> TestResult {
    let cases = [
        (
            "recorder-start-store-unavailable",
            vec![
                "--json".to_owned(),
                "recorder".to_owned(),
                "start".to_owned(),
                "--agent-id".to_owned(),
                "agent_fixture".to_owned(),
                "--session-id".to_owned(),
                "session_fixture".to_owned(),
                "--workspace-id".to_owned(),
                "workspace_fixture".to_owned(),
            ],
            "recorder start",
        ),
        (
            "recorder-event-store-unavailable",
            vec![
                "--json".to_owned(),
                "recorder".to_owned(),
                "event".to_owned(),
                "run_fixture_001".to_owned(),
                "--event-type".to_owned(),
                "tool_result".to_owned(),
                "--payload".to_owned(),
                "ok".to_owned(),
                "--previous-event-hash".to_owned(),
                "blake3:previous".to_owned(),
            ],
            "recorder event",
        ),
        (
            "recorder-finish-store-unavailable",
            vec![
                "--json".to_owned(),
                "recorder".to_owned(),
                "finish".to_owned(),
                "run_fixture_001".to_owned(),
                "--status".to_owned(),
                "completed".to_owned(),
            ],
            "recorder finish",
        ),
    ];

    for (name, args, command) in cases {
        let result = run_ee_logged(name, None, args)?;

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
            &format!("{command} command field"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/code",
            json!("recorder_store_unavailable"),
            &format!("{command} degraded code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/degraded/0/code",
            json!("recorder_store_unavailable"),
            &format!("{command} degraded array code"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/followUpBead",
            json!("eidetic_engine_cli-6xzc"),
            &format!("{command} follow-up bead"),
        )?;
        ensure_json_pointer(
            &result.parsed,
            "/data/sideEffectClass",
            json!(
                "conservative abstention; no recorder session, event, hash-chain, or finish mutation"
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
            format!("{command} degraded output should not be fake success: {fake_success:?}"),
        )?;

        let unsupported_claims =
            validate_no_unsupported_evidence_claims(command, false, false, &result.stdout);
        ensure(
            unsupported_claims.passed,
            format!(
                "{command} degraded output should not count as unsupported success: {unsupported_claims:?}"
            ),
        )?;

        let log_text = fs::read_to_string(&result.log_path)
            .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
        let log_json: Value = serde_json::from_str(&log_text)
            .map_err(|error| format!("e2e log must be JSON: {error}"))?;
        ensure_json_pointer(
            &log_json,
            "/degradationCodes",
            json!(["recorder_store_unavailable"]),
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
            json!("recorder"),
            &format!("logged {command} boundary matrix row"),
        )?;
        ensure_json_pointer(
            &log_json,
            "/sideEffectClass",
            json!(
                "conservative abstention; no recorder session, event, hash-chain, or finish mutation"
            ),
            &format!("logged {command} side-effect class"),
        )?;
    }

    Ok(())
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
    fs::write(artifacts.join("stdout.json"), artifact_payload)
        .map_err(|error| format!("failed to write artifact: {error}"))?;
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
    let workspace_arg = workspace.display().to_string();
    let demo_id = "demo_00000000000000000000000001".to_owned();

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

    let execution_result = run_ee_logged(
        "demo-run-execution-unavailable",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "demo".to_owned(),
            "run".to_owned(),
            demo_id.clone(),
        ],
    )?;
    ensure_equal(
        &execution_result.exit_code,
        &UNSATISFIED_DEGRADED_MODE_EXIT,
        "demo run execution unavailable exit code",
    )?;
    ensure_json_pointer(
        &execution_result.parsed,
        "/data/degraded/0/code",
        json!("demo_command_execution_unavailable"),
        "demo run non-dry-run unavailable code",
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
        ("demo verify", &verify_result),
        ("demo run", &execution_result),
        ("demo verify without artifacts", &no_artifact_verify),
        (
            "demo verify optional missing artifacts",
            &optional_missing_verify,
        ),
    ] {
        let fake_success = validate_no_fake_success_output(command, false, false, &result.stdout);
        ensure(
            fake_success.passed,
            format!("{command} output should not be fake success: {fake_success:?}"),
        )?;

        let unsupported_claims =
            validate_no_unsupported_evidence_claims(command, false, false, &result.stdout);
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
