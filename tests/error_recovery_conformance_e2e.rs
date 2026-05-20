use std::process::{Command, Output};

type TestResult = Result<(), String>;

#[derive(Clone, Debug)]
struct ConformanceCase {
    id: &'static str,
    surface: &'static str,
    args: Vec<String>,
}

fn run_ee(args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn log_event(case: &ConformanceCase, event: &str, data: serde_json::Value) {
    eprintln!(
        "{}",
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "suite": "error_recovery_conformance_e2e",
            "test": "storage_database_not_found_recovery_contract",
            "requirementId": case.id,
            "surface": case.surface,
            "event": event,
            "data": data,
        })
    );
}

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\n{stdout}"))
}

fn string_at<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
    context: &str,
) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{context}: missing string at {pointer}"))
}

fn array_at<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{context}: missing array at {pointer}"))
}

fn storage_error_cases(workspace: &str) -> Vec<ConformanceCase> {
    vec![
        ConformanceCase {
            id: "ERR-RECOVERY-STORAGE-001",
            surface: "context",
            args: vec![
                "--workspace".to_owned(),
                workspace.to_owned(),
                "context".to_owned(),
                "missing database recovery conformance".to_owned(),
                "--json".to_owned(),
            ],
        },
        ConformanceCase {
            id: "ERR-RECOVERY-STORAGE-002",
            surface: "subscribe poll",
            args: vec![
                "--workspace".to_owned(),
                workspace.to_owned(),
                "subscribe".to_owned(),
                "poll".to_owned(),
                "--cursor".to_owned(),
                "0".to_owned(),
                "--json".to_owned(),
            ],
        },
    ]
}

fn assert_storage_recovery_contract(case: &ConformanceCase, output: &Output) -> TestResult {
    ensure(
        output.status.code() == Some(3),
        format!(
            "{}: expected storage exit code 3, got {:?}\nstdout:\n{}\nstderr:\n{}",
            case.id,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!("{}: JSON errors must not write stderr", case.id),
    )?;

    let json = stdout_json(output, case.id)?;
    ensure(
        string_at(&json, "/schema", case.id)? == "ee.error.v2",
        format!("{}: error envelope schema must be ee.error.v2", case.id),
    )?;
    ensure(
        string_at(&json, "/error/code", case.id)? == "storage",
        format!("{}: code must be storage", case.id),
    )?;
    ensure(
        string_at(&json, "/error/severity", case.id)? == "high",
        format!("{}: storage errors must be high severity", case.id),
    )?;
    ensure(
        string_at(&json, "/error/message", case.id)?
            .to_ascii_lowercase()
            .contains("database not found"),
        format!("{}: message must identify missing database", case.id),
    )?;

    let recovery = array_at(&json, "/error/details/recovery", case.id)?;
    ensure(
        recovery.len() == 3,
        format!(
            "{}: expected exactly 3 recovery actions, got {}",
            case.id,
            recovery.len()
        ),
    )?;
    ensure(
        recovery
            .windows(2)
            .all(|window| window[0]["priority"].as_u64() < window[1]["priority"].as_u64()),
        format!("{}: recovery priorities must be strictly ordered", case.id),
    )?;

    ensure(
        recovery[0]["priority"] == serde_json::json!(1)
            && recovery[0]["kind"] == serde_json::json!("seed")
            && recovery[0]["command"] == serde_json::json!("ee init --workspace ."),
        format!("{}: first recovery action must run ee init", case.id),
    )?;
    ensure(
        recovery[1]["priority"] == serde_json::json!(2)
            && recovery[1]["kind"] == serde_json::json!("flag")
            && recovery[1]["flagName"] == serde_json::json!("--workspace")
            && recovery[1]["valueHint"] == serde_json::json!("<path>"),
        format!(
            "{}: second recovery action must point at initialized workspace",
            case.id
        ),
    )?;
    ensure(
        recovery[2]["priority"] == serde_json::json!(3)
            && recovery[2]["kind"] == serde_json::json!("env")
            && recovery[2]["envName"] == serde_json::json!("EE_DATABASE_PATH"),
        format!(
            "{}: third recovery action must expose EE_DATABASE_PATH",
            case.id
        ),
    )?;

    log_event(
        case,
        "pass",
        serde_json::json!({
            "schema": json["schema"],
            "code": json["error"]["code"],
            "severity": json["error"]["severity"],
            "recoveryCount": recovery.len(),
        }),
    );
    Ok(())
}

#[test]
fn storage_database_not_found_recovery_contract() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let cases = storage_error_cases(&workspace);

    ensure(
        cases.len() == 2,
        "coverage matrix should exercise two independent storage-backed surfaces",
    )?;

    for case in &cases {
        log_event(
            case,
            "run",
            serde_json::json!({
                "requirementLevel": "MUST",
                "workspaceInitialized": false,
            }),
        );
        let output = run_ee(&case.args)?;
        assert_storage_recovery_contract(case, &output)?;
    }

    Ok(())
}
