//! Real-binary pin test for the default concise `ee doctor --json` contract.
//!
//! Unit coverage already pins the formatter. This E2E exercises the compiled
//! binary so Clap routing, stdout/stderr discipline, and the concise/full JSON
//! split stay wired together.

use std::process::{Command, Output};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn log_event(kind: &str, label: &str, fields: Value) {
    eprintln!(
        "{}",
        json!({
            "schema": "ee.test_event.v1",
            "suite": "e2e_doctor_concise_default",
            "kind": kind,
            "label": label,
            "fields": fields,
        })
    );
}

fn ensure(condition: bool, label: &str, details: Value) -> TestResult {
    let details_text = details.to_string();
    log_event(
        "assertion",
        label,
        json!({
            "passed": condition,
            "details": details,
        }),
    );
    if condition {
        Ok(())
    } else {
        Err(format!("{label}: {details_text}"))
    }
}

fn preview(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(400).collect()
}

fn run_ee(label: &str, args: &[&str]) -> Result<Output, String> {
    log_event(
        "command_start",
        label,
        json!({
            "command": "ee",
            "argv": args,
        }),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env("EE_NO_COLOR", "1")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_DATABASE_PATH")
        .env_remove("EE_INDEX_DIR")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    log_event(
        "command_end",
        label,
        json!({
            "exitCode": output.status.code(),
            "success": output.status.success(),
            "stdoutBytes": output.stdout.len(),
            "stderrBytes": output.stderr.len(),
            "stdoutPreview": preview(&output.stdout),
            "stderrPreview": preview(&output.stderr),
        }),
    );
    Ok(output)
}

fn parse_json(label: &str, output: &Output) -> Result<Value, String> {
    let value = serde_json::from_slice::<Value>(&output.stdout)
        .map_err(|error| format!("{label} stdout must be JSON: {error}"))?;
    log_event(
        "json_parse",
        label,
        json!({
            "schema": value.get("schema"),
            "success": value.get("success"),
            "fields": value.get("fields"),
        }),
    );
    Ok(value)
}

fn json_array_len(value: &Value, pointer: &str, label: &str) -> Result<usize, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| format!("{label}: {pointer} must be an array; got {value}"))
}

#[test]
fn doctor_default_json_is_concise_and_full_json_keeps_diagnostics() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_arg = workspace
        .path()
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;
    log_event(
        "workspace",
        "temp_workspace",
        json!({
            "workspace": workspace_arg,
        }),
    );

    let init_output = run_ee("init", &["--workspace", workspace_arg, "init", "--json"])?;
    ensure(
        init_output.status.success(),
        "init succeeds before doctor pin test",
        json!({
            "stdout": preview(&init_output.stdout),
            "stderr": preview(&init_output.stderr),
        }),
    )?;
    ensure(
        init_output.stderr.is_empty(),
        "init keeps stderr empty",
        json!({
            "stderr": preview(&init_output.stderr),
        }),
    )?;
    let init_json = parse_json("init", &init_output)?;
    ensure(
        init_json["schema"].as_str() == Some("ee.response.v2")
            && init_json["success"].as_bool() == Some(true),
        "init returns successful response envelope",
        json!({
            "schema": init_json.get("schema"),
            "success": init_json.get("success"),
        }),
    )?;

    let concise_output = run_ee(
        "doctor_concise",
        &["--workspace", workspace_arg, "doctor", "--json"],
    )?;
    ensure(
        concise_output.status.success(),
        "default doctor json succeeds",
        json!({
            "stdout": preview(&concise_output.stdout),
            "stderr": preview(&concise_output.stderr),
        }),
    )?;
    ensure(
        concise_output.stderr.is_empty(),
        "default doctor json keeps stderr empty",
        json!({
            "stderr": preview(&concise_output.stderr),
        }),
    )?;
    let concise = parse_json("doctor_concise", &concise_output)?;
    ensure(
        concise["schema"].as_str() == Some("ee.response.v2")
            && concise["success"].as_bool() == Some(true),
        "default doctor uses successful response envelope",
        json!({
            "schema": concise.get("schema"),
            "success": concise.get("success"),
        }),
    )?;
    ensure(
        concise["fields"].as_str() == Some("doctor_concise"),
        "default doctor fields are concise",
        json!({
            "fields": concise.get("fields"),
        }),
    )?;
    ensure(
        concise["data"]["mode"].as_str() == Some("concise")
            && concise["data"]["fullCommand"].as_str() == Some("ee doctor --full --json"),
        "default doctor points agents at full diagnostics",
        json!({
            "mode": concise.pointer("/data/mode"),
            "fullCommand": concise.pointer("/data/fullCommand"),
        }),
    )?;

    let core_len = json_array_len(&concise, "/data/coreChecks", "concise doctor")?;
    let actionable_len = json_array_len(&concise, "/data/actionable", "concise doctor")?;
    ensure(
        core_len > 0,
        "default doctor includes core checks",
        json!({
            "coreCheckCount": core_len,
        }),
    )?;
    ensure(
        concise["data"]["coreChecks"]
            .as_array()
            .is_some_and(|checks| {
                checks
                    .iter()
                    .all(|check| check["tier"].as_str() == Some("core"))
            }),
        "default doctor coreChecks are core-tier only",
        json!({
            "coreChecks": concise.pointer("/data/coreChecks"),
        }),
    )?;
    ensure(
        concise["data"]["advisorySummary"].is_object()
            && concise["data"]["advisorySummary"]["summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("ee doctor --full --json"))
            && concise["data"]["advisorySummary"]["fullCommand"].as_str()
                == Some("ee doctor --full --json"),
        "default doctor summarizes advisory diagnostics",
        json!({
            "actionableCount": actionable_len,
            "advisorySummary": concise.pointer("/data/advisorySummary"),
        }),
    )?;
    let permanent_capability_gaps = concise["data"]["permanentCapabilityGaps"]
        .as_array()
        .ok_or_else(|| {
            format!("default doctor permanentCapabilityGaps must be an array; got {concise}")
        })?;
    ensure(
        permanent_capability_gaps.iter().any(|gap| {
            gap["name"].as_str() == Some("reranker_posture")
                && gap["permanent"].as_bool() == Some(true)
                && gap.get("repair").is_none()
                && gap["message"].as_str().is_some_and(|message| {
                    message.contains("Network download and bundled installation are unavailable")
                })
        }),
        "default doctor lists the permanent reranker capability gap without a fake automatic repair",
        json!({
            "permanentCapabilityGaps": permanent_capability_gaps,
        }),
    )?;

    let concise_human_output = run_ee(
        "doctor_concise_human",
        &["--workspace", workspace_arg, "doctor"],
    )?;
    ensure(
        concise_human_output.status.success(),
        "default human doctor succeeds",
        json!({
            "stdout": preview(&concise_human_output.stdout),
            "stderr": preview(&concise_human_output.stderr),
        }),
    )?;
    ensure(
        concise_human_output.stderr.is_empty(),
        "default human doctor keeps stderr empty",
        json!({
            "stderr": preview(&concise_human_output.stderr),
        }),
    )?;
    let concise_human = String::from_utf8(concise_human_output.stdout)
        .map_err(|error| format!("default human doctor stdout must be UTF-8: {error}"))?;
    ensure(
        !concise_human.contains("--from-file /path/to/")
            && concise_human.contains("Network download and bundled installation are unavailable"),
        "default human doctor states the permanent gap without a placeholder repair",
        json!({
            "stdout": concise_human,
        }),
    )?;
    for omitted_key in [
        "checks",
        "advisories",
        "singleFlight",
        "flightRecorder",
        "qos",
        "rchWorkerPressure",
        "verificationPosture",
        "verificationLedger",
        "hostCalibration",
        "meshAutoEnrollment",
    ] {
        ensure(
            concise["data"].get(omitted_key).is_none(),
            "default doctor omits full diagnostic firehose",
            json!({
                "omittedKey": omitted_key,
            }),
        )?;
    }

    let full_output = run_ee(
        "doctor_full",
        &["--workspace", workspace_arg, "doctor", "--full", "--json"],
    )?;
    ensure(
        full_output.status.success(),
        "full doctor json succeeds",
        json!({
            "stdout": preview(&full_output.stdout),
            "stderr": preview(&full_output.stderr),
        }),
    )?;
    ensure(
        full_output.stderr.is_empty(),
        "full doctor json keeps stderr empty",
        json!({
            "stderr": preview(&full_output.stderr),
        }),
    )?;
    let full = parse_json("doctor_full", &full_output)?;
    ensure(
        full["schema"].as_str() == Some("ee.response.v2")
            && full["success"].as_bool() == Some(true)
            && full["fields"].as_str() == Some("full"),
        "full doctor uses exhaustive response envelope",
        json!({
            "schema": full.get("schema"),
            "success": full.get("success"),
            "fields": full.get("fields"),
        }),
    )?;
    ensure(
        json_array_len(&full, "/data/checks", "full doctor")? >= core_len,
        "full doctor includes exhaustive checks",
        json!({
            "conciseCoreCheckCount": core_len,
            "fullCheckCount": json_array_len(&full, "/data/checks", "full doctor")?,
        }),
    )?;
    ensure(
        full["data"]["meshAutoEnrollment"]["schema"].as_str()
            == Some("ee.doctor.mesh_auto_enrollment.v1")
            && json_array_len(&full, "/data/meshAutoEnrollment/checks", "full doctor mesh")? == 15,
        "full doctor includes mesh auto-enrollment diagnostics",
        json!({
            "meshSchema": full.pointer("/data/meshAutoEnrollment/schema"),
            "meshCheckCount": json_array_len(&full, "/data/meshAutoEnrollment/checks", "full doctor mesh")?,
        }),
    )?;
    ensure(
        full["data"]["rchWorkerPressure"]["schema"].as_str() == Some("ee.rch.worker_pressure.v1")
            && full["data"]["hostCalibration"]["schema"].as_str()
                == Some("ee.host_calibration.posture.v1")
            && full["data"]["verificationPosture"].is_object()
            && full["data"]["verificationLedger"].is_object(),
        "full doctor keeps advisory subsystem diagnostics",
        json!({
            "rchWorkerPressureSchema": full.pointer("/data/rchWorkerPressure/schema"),
            "hostCalibrationSchema": full.pointer("/data/hostCalibration/schema"),
            "hasVerificationPosture": full.pointer("/data/verificationPosture").is_some(),
            "hasVerificationLedger": full.pointer("/data/verificationLedger").is_some(),
        }),
    )?;
    ensure(
        concise_output.stdout.len() < full_output.stdout.len(),
        "default concise doctor output is smaller than full output",
        json!({
            "conciseBytes": concise_output.stdout.len(),
            "fullBytes": full_output.stdout.len(),
        }),
    )
}

#[test]
fn doctor_read_only_modes_honor_environment_and_walk_up_workspace_resolution() -> TestResult {
    let env_workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let unrelated_cwd = tempfile::tempdir().map_err(|error| error.to_string())?;
    let env_workspace_arg = env_workspace
        .path()
        .to_str()
        .ok_or_else(|| "environment workspace path must be UTF-8".to_owned())?;
    let env_output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["doctor", "--list-runs"])
        .current_dir(unrelated_cwd.path())
        .env("EE_NO_COLOR", "1")
        .env("EE_WORKSPACE", env_workspace.path())
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("run doctor with EE_WORKSPACE: {error}"))?;
    ensure(
        env_output.status.success() && env_output.stderr.is_empty(),
        "doctor list-runs succeeds through EE_WORKSPACE",
        json!({
            "stdout": preview(&env_output.stdout),
            "stderr": preview(&env_output.stderr),
        }),
    )?;
    let env_json = parse_json("doctor_env_workspace", &env_output)?;
    ensure(
        env_json["workspace"].as_str() == Some(env_workspace_arg),
        "doctor list-runs selects EE_WORKSPACE ahead of cwd",
        json!({
            "expected": env_workspace_arg,
            "actual": env_json.get("workspace"),
        }),
    )?;

    let walk_up_workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    std::fs::create_dir(walk_up_workspace.path().join(".ee"))
        .map_err(|error| format!("create walk-up marker: {error}"))?;
    let nested = walk_up_workspace.path().join("nested").join("deeper");
    std::fs::create_dir_all(&nested)
        .map_err(|error| format!("create nested walk-up cwd: {error}"))?;
    let walk_up_workspace_arg = walk_up_workspace
        .path()
        .to_str()
        .ok_or_else(|| "walk-up workspace path must be UTF-8".to_owned())?;
    let walk_up_output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["doctor", "--list-runs"])
        .current_dir(&nested)
        .env("EE_NO_COLOR", "1")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("run doctor with walk-up workspace: {error}"))?;
    ensure(
        walk_up_output.status.success() && walk_up_output.stderr.is_empty(),
        "doctor list-runs succeeds through walk-up discovery",
        json!({
            "stdout": preview(&walk_up_output.stdout),
            "stderr": preview(&walk_up_output.stderr),
        }),
    )?;
    let walk_up_json = parse_json("doctor_walk_up_workspace", &walk_up_output)?;
    ensure(
        walk_up_json["workspace"].as_str() == Some(walk_up_workspace_arg),
        "doctor list-runs selects ancestor .ee workspace ahead of nested cwd",
        json!({
            "expected": walk_up_workspace_arg,
            "actual": walk_up_json.get("workspace"),
        }),
    )
}

/// How the real store is damaged for the bd-xa6ud / bd-wswg0 worlds.
#[derive(Clone, Copy)]
enum DamagedStore {
    /// `.ee/ee.db` is a zero-byte file.
    Empty,
    /// `.ee/ee.db` holds only the first 8192 bytes of the real database.
    Truncated,
}

/// Build a real store (init, remember, index rebuild), then damage it. The
/// original database and its sidecars are MOVED to `.fixture_baseline`, never
/// deleted.
fn damaged_store_workspace(damage: DamagedStore) -> Result<tempfile::TempDir, String> {
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let arg = workspace
        .path()
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;
    for (label, args) in [
        ("init", vec!["--workspace", arg, "init", "--json"]),
        (
            "remember",
            vec![
                "--workspace",
                arg,
                "remember",
                "--json",
                "Run the storage self-check before a release.",
            ],
        ),
        (
            "index rebuild",
            vec!["--workspace", arg, "index", "rebuild", "--json"],
        ),
    ] {
        let output = run_ee(label, &args)?;
        ensure(
            output.status.success(),
            "real store setup step succeeds",
            json!({ "step": label, "stderr": preview(&output.stderr) }),
        )?;
    }
    // Provision the doctor runtime lock on the healthy store, before any
    // damage, so its first creation is not counted as a change by --fix.
    let provision = run_ee(
        "provision doctor lock",
        &["--workspace", arg, "doctor", "--fix", "--json"],
    )?;
    ensure(
        workspace.path().join(".ee").join(".doctor.lock").is_file(),
        "doctor --fix on the healthy store provisions .ee/.doctor.lock",
        json!({
            "exitCode": provision.status.code(),
            "stderr": preview(&provision.stderr),
        }),
    )?;
    let store = workspace.path().join(".ee");
    let baseline = workspace.path().join(".fixture_baseline");
    std::fs::create_dir_all(&baseline).map_err(|error| error.to_string())?;
    for name in ["ee.db", "ee.db-wal", "ee.db-shm"] {
        let path = store.join(name);
        if path.exists() {
            std::fs::rename(&path, baseline.join(name)).map_err(|error| error.to_string())?;
        }
    }
    let damaged = match damage {
        DamagedStore::Empty => Vec::new(),
        DamagedStore::Truncated => {
            let original =
                std::fs::read(baseline.join("ee.db")).map_err(|error| error.to_string())?;
            ensure(
                original.len() > 8192,
                "real database is larger than the truncation point",
                json!({ "bytes": original.len() }),
            )?;
            original[..8192].to_vec()
        }
    };
    std::fs::write(store.join("ee.db"), damaged).map_err(|error| error.to_string())?;
    Ok(workspace)
}

/// Every regular file under `.ee`, as relative path -> (bytes, blake3). A new
/// sidecar, a grown database or any rewritten store file changes it.
fn store_fingerprint(workspace: &std::path::Path) -> Result<Value, String> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        files: &mut serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.is_dir() {
                walk(root, &path, files)?;
            } else if path.is_file() {
                let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .display()
                    .to_string();
                files.insert(
                    relative,
                    json!([bytes.len(), blake3::hash(&bytes).to_hex().to_string()]),
                );
            }
        }
        Ok(())
    }
    let store = workspace.join(".ee");
    let mut files = serde_json::Map::new();
    walk(&store, &store, &mut files)?;
    Ok(Value::Object(files))
}

/// `.ee/ee.write.lock` holds a zero-padded decimal acquisition counter and a
/// newline. Anything else is reported as unreadable rather than guessed at.
fn write_lock_counter(workspace: &std::path::Path, label: &str) -> Result<u64, String> {
    let text = std::fs::read_to_string(workspace.join(".ee").join("ee.write.lock"))
        .map_err(|error| format!("{label}: ee.write.lock unreadable: {error}"))?;
    let digits = text.trim_end_matches('\n');
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "{label}: ee.write.lock is not a decimal counter: {text:?}"
        ));
    }
    digits
        .parse::<u64>()
        .map_err(|error| format!("{label}: ee.write.lock counter: {error}"))
}

fn check_by_name<'a>(doctor: &'a Value, name: &str) -> Option<&'a Value> {
    doctor
        .pointer("/data/checks")
        .and_then(Value::as_array)
        .and_then(|checks| checks.iter().find(|check| check["name"] == name))
}

fn assert_damaged_store_is_reported_and_left_untouched(
    damage: DamagedStore,
    database_code: &str,
    fixer_code: &str,
) -> TestResult {
    let workspace = damaged_store_workspace(damage)?;
    let arg = workspace
        .path()
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;
    let before = store_fingerprint(workspace.path())?;
    let counter_before = write_lock_counter(workspace.path(), "baseline")?;

    let doctor = run_ee(
        "doctor full",
        &["--workspace", arg, "doctor", "--full", "--json"],
    )?;
    let doctor_json = parse_json("doctor full", &doctor)?;
    let database = check_by_name(&doctor_json, "database");
    ensure(
        database.and_then(|check| check["errorCode"].as_str()) == Some(database_code),
        "database check names the damaged store, not pending migrations",
        json!({ "expected": database_code, "database": database }),
    )?;
    ensure(
        doctor_json.pointer("/data/posture") == Some(&json!("blocked")),
        "a damaged store blocks the doctor posture",
        json!({ "posture": doctor_json.pointer("/data/posture") }),
    )?;
    let search_index = check_by_name(&doctor_json, "search_index");
    ensure(
        search_index.and_then(|check| check["errorCode"].as_str()) != Some("EE-E300"),
        "search index is not misreported as missing while the database is unreadable",
        json!({ "search_index": search_index }),
    )?;

    let fix = run_ee(
        "doctor fix",
        &["--workspace", arg, "doctor", "--fix", "--json"],
    )?;
    let fix_text = String::from_utf8_lossy(&fix.stdout).into_owned();
    ensure(
        fix.status.code() == Some(6) && !fix_text.contains("doctor_runtime_io"),
        "doctor --fix reports unresolved core recovery instead of success or a runtime crash",
        json!({
            "exitCode": fix.status.code(),
            "stdout": preview(&fix.stdout),
            "stderr": preview(&fix.stderr),
        }),
    )?;
    let fix_json = parse_json("doctor fix", &fix)?;
    ensure(
        fix_json.pointer("/data/status") == Some(&json!("completed_partial"))
            && fix_json.pointer("/data/fixerDispatchPending") == Some(&json!(true))
            && fix_json
                .pointer("/data/unresolvedCoreChecks")
                .and_then(Value::as_array)
                .is_some_and(|checks| {
                    checks.iter().any(|check| {
                        check["name"] == "database" && check["errorCode"] == database_code
                    })
                }),
        "required database recovery remains visible in the fix summary",
        json!({"data": fix_json.get("data")}),
    )?;
    let results = fix_json
        .pointer("/data/fixerResults")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    ensure(
        results.iter().any(|result| {
            result["findingCode"] == fixer_code && result["outcome"] == "guidance_recorded"
        }),
        "doctor --fix records database guidance",
        json!({ "expected": fixer_code, "fixerResults": results }),
    )?;
    ensure(
        !results.iter().any(|result| {
            result["findingCode"].as_str().is_some_and(|code| {
                code.starts_with("search_index") || code.starts_with("schema_migration")
            })
        }),
        "no index or migration repair runs against an unreadable store",
        json!({ "fixerResults": results }),
    )?;

    let recheck = run_ee(
        "doctor full after fix",
        &["--workspace", arg, "doctor", "--full", "--json"],
    )?;
    let recheck_json = parse_json("doctor full after fix", &recheck)?;
    ensure(
        recheck_json.pointer("/data/posture") == Some(&json!("blocked")),
        "guidance does not repair the store, so posture stays blocked after --fix",
        json!({ "posture": recheck_json.pointer("/data/posture") }),
    )?;

    // Byte identity for every .ee file, the provisioned .doctor.lock included,
    // except ee.write.lock: a monotonic acquisition counter that an honest run
    // advances, so it is checked by its semantics (exists; counter >= baseline).
    let after = store_fingerprint(workspace.path())?;
    let counter_after = write_lock_counter(workspace.path(), "after --fix")?;
    log_event(
        "runtime_locks",
        "doctor runtime lock files before and after",
        json!({
            "doctorLock": [before.get(".doctor.lock"), after.get(".doctor.lock")],
            "writeLockCounter": [counter_before, counter_after],
        }),
    );
    ensure(
        counter_after >= counter_before,
        "ee.write.lock still exists and its acquisition counter did not go backwards",
        json!({ "before": counter_before, "after": counter_after }),
    )?;
    let without_write_lock = |fingerprint: &Value| {
        let mut files = fingerprint.as_object().cloned().unwrap_or_default();
        files.remove("ee.write.lock");
        Value::Object(files)
    };
    ensure(
        without_write_lock(&after) == without_write_lock(&before),
        "doctor and doctor --fix leave every other .ee file byte-identical",
        json!({ "before": before, "after": after }),
    )?;
    Ok(())
}

// bd-wswg0 + bd-xa6ud: a zero-byte database is a data-loss finding (EE-E206),
// never a pending migration, and --fix only records guidance.
#[test]
fn doctor_reports_an_empty_database_as_data_loss_and_fix_records_guidance() -> TestResult {
    assert_damaged_store_is_reported_and_left_untouched(
        DamagedStore::Empty,
        "EE-E206",
        "database_empty",
    )
}

// bd-xa6ud: a truncated database (EE-E202) gets guidance from --fix, not an
// index repair that crashes with doctor_runtime_io.
#[test]
fn doctor_fix_records_guidance_for_a_truncated_database() -> TestResult {
    assert_damaged_store_is_reported_and_left_untouched(
        DamagedStore::Truncated,
        "EE-E202",
        "database_corrupted",
    )
}
