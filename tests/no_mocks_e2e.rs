//! No-mocks end-to-end coverage for the walking-skeleton memory loop.
//!
//! This test runs the real `ee` binary against an isolated workspace, writes
//! every command result to structured JSONL, and asserts that the durable
//! FrankenSQLite database plus Frankensearch-derived index can support
//! init -> remember -> search -> context -> why without mocks.

use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::db::{DatabaseConfig, DbConnection, PACK_REPLAY_LEDGER_SCHEMA_V1};
use ee::policy::redact_secret_like_content;

type TestResult = Result<(), String>;

#[derive(Clone, Debug)]
struct StepSpec {
    name: &'static str,
    args: Vec<String>,
    expected_exit_code: i32,
    expected_schema: &'static str,
    expect_clean_stderr: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandEvent {
    schema: &'static str,
    scenario_id: &'static str,
    step: String,
    command: &'static str,
    args: Vec<String>,
    env_overrides: Vec<LoggedEnvOverride>,
    cwd: String,
    workspace: String,
    started_at_unix_ms: u128,
    elapsed_ms: u128,
    exit_code: i32,
    stdout: String,
    stderr: String,
    stdout_artifact_path: String,
    stderr_artifact_path: String,
    stdout_json_valid: bool,
    stdout_schema: Option<String>,
    schema_validation_status: &'static str,
    golden_validation_status: &'static str,
    redaction_status: &'static str,
    first_failure: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoggedEnvOverride {
    name: String,
    value: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SummaryEvent {
    schema: &'static str,
    scenario_id: &'static str,
    event: &'static str,
    command_count: usize,
    workspace: String,
    database_path: String,
    index_metadata_path: String,
    rule_memory_id: String,
    failure_memory_id: String,
    pack_hash: String,
    pack_ledger_hashes: Vec<String>,
    pack_ledger_pack_ids: Vec<String>,
    context_item_ids: Vec<String>,
}

#[derive(Clone, Copy)]
struct SecretProbe {
    class: &'static str,
    raw: &'static str,
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
    T: std::fmt::Debug + PartialEq + ?Sized,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn unique_log_dir(scenario_id: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-no-mocks-e2e-logs")
        .join(format!("{scenario_id}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create log dir {}: {error}", dir.display()))?;
    Ok(dir)
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

fn append_jsonl<T>(path: &Path, value: &T) -> Result<(), String>
where
    T: Serialize,
{
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open JSONL log {}: {error}", path.display()))?;
    serde_json::to_writer(&mut file, value)
        .map_err(|error| format!("failed to serialize JSONL event: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("failed to write JSONL newline: {error}"))
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

fn first_failure(
    expected_exit_code: i32,
    expected_schema: &str,
    expect_clean_stderr: bool,
    event: &CommandEvent,
) -> Option<String> {
    if event.exit_code != expected_exit_code {
        return Some(format!(
            "exit_code: expected {}, got {}",
            expected_exit_code, event.exit_code
        ));
    }
    if !event.stdout_json_valid {
        return Some("stdout is not valid JSON".to_owned());
    }
    if event.stdout_schema.as_deref() != Some(expected_schema) {
        return Some(format!(
            "schema: expected {}, got {:?}",
            expected_schema, event.stdout_schema
        ));
    }
    if expect_clean_stderr && !event.stderr.is_empty() {
        return Some("stderr was not empty".to_owned());
    }
    None
}

fn stream_snippet(text: &str) -> String {
    let trimmed = text.trim();
    let mut snippet = trimmed.chars().take(1200).collect::<String>();
    if trimmed.chars().nth(1200).is_some() {
        snippet.push_str("...");
    }
    snippet
}

fn run_step(
    scenario_id: &'static str,
    log_path: &Path,
    artifact_dir: &Path,
    workspace: &Path,
    spec: StepSpec,
) -> Result<(CommandEvent, JsonValue), String> {
    run_step_with_env(scenario_id, log_path, artifact_dir, workspace, spec, &[])
}

fn run_step_with_env(
    scenario_id: &'static str,
    log_path: &Path,
    artifact_dir: &Path,
    workspace: &Path,
    spec: StepSpec,
    envs: &[(&str, OsString)],
) -> Result<(CommandEvent, JsonValue), String> {
    let started_at_unix_ms = unix_ms_now()?;
    let start = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(&spec.args);
    if !envs.iter().any(|(key, _)| *key == "PATH") {
        command.env("PATH", default_tool_path()?);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to execute step {}: {error}", spec.name))?;
    let elapsed_ms = start.elapsed().as_millis();

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout UTF-8 decode failed for {}: {error}", spec.name))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr UTF-8 decode failed for {}: {error}", spec.name))?;
    let step_slug = sanitize_step_name(spec.name);
    let stdout_path = artifact_dir.join(format!("{step_slug}.stdout.json"));
    let stderr_path = artifact_dir.join(format!("{step_slug}.stderr.log"));
    write_text(&stdout_path, &stdout)?;
    write_text(&stderr_path, &stderr)?;

    let parsed_stdout = serde_json::from_str::<JsonValue>(&stdout).ok();
    let stdout_schema = parsed_stdout.as_ref().and_then(schema_from_json);
    let logged_args = spec
        .args
        .iter()
        .map(|arg| redact_secret_like_content(arg).content)
        .collect::<Vec<_>>();
    let env_overrides = envs
        .iter()
        .map(|(key, value)| LoggedEnvOverride {
            name: (*key).to_owned(),
            value: redact_secret_like_content(&value.to_string_lossy()).content,
        })
        .collect::<Vec<_>>();
    let schema_validation_status =
        if parsed_stdout.is_some() && stdout_schema.as_deref() == Some(spec.expected_schema) {
            "passed"
        } else {
            "failed"
        };
    let mut event = CommandEvent {
        schema: "ee.e2e.command_event.v1",
        scenario_id,
        step: spec.name.to_owned(),
        command: "ee",
        args: logged_args,
        env_overrides,
        cwd: env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_owned()),
        workspace: workspace.display().to_string(),
        started_at_unix_ms,
        elapsed_ms,
        exit_code: output.status.code().unwrap_or(-1),
        stdout,
        stderr,
        stdout_artifact_path: stdout_path.display().to_string(),
        stderr_artifact_path: stderr_path.display().to_string(),
        stdout_json_valid: parsed_stdout.is_some(),
        stdout_schema,
        schema_validation_status,
        golden_validation_status: "not_applicable",
        redaction_status: "not_checked",
        first_failure: None,
    };
    event.first_failure = first_failure(
        spec.expected_exit_code,
        spec.expected_schema,
        spec.expect_clean_stderr,
        &event,
    );
    append_jsonl(log_path, &event)?;

    let parsed = parsed_stdout.ok_or_else(|| {
        format!(
            "{} stdout was not valid JSON; see {}",
            event.step, event.stdout_artifact_path
        )
    })?;
    if let Some(failure) = &event.first_failure {
        return Err(format!(
            "{} failed no-mocks contract: {}; stdout={}; stderr={}; log={}",
            event.step,
            failure,
            stream_snippet(&event.stdout),
            stream_snippet(&event.stderr),
            log_path.display()
        ));
    }
    Ok((event, parsed))
}

fn string_at(value: &JsonValue, pointer: &str, context: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context} missing string at {pointer}"))
}

fn memory_ids_from_context(value: &JsonValue) -> Result<Vec<String>, String> {
    value
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "context pack items missing".to_owned())?
        .iter()
        .map(|item| {
            item.get("memoryId")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "context pack item missing memoryId".to_owned())
        })
        .collect()
}

fn assert_context_pack_ledgers_persisted(
    database_path: &Path,
    query: &str,
    pack_hash: &str,
    selected_item_ids: &[String],
    min_records: usize,
) -> Result<(Vec<String>, Vec<String>), String> {
    let anchor_memory_id = selected_item_ids
        .first()
        .ok_or_else(|| "context pack selected no anchor memory".to_owned())?;
    let connection = DbConnection::open(DatabaseConfig::file(database_path.to_path_buf()))
        .map_err(|error| format!("open database for pack ledger inspection: {error}"))?;
    let history = connection
        .list_pack_records_for_memory(anchor_memory_id, 20)
        .map_err(|error| format!("list pack records for ledger inspection: {error}"))?;

    let mut pack_ids = BTreeSet::new();
    let mut ledger_hashes = BTreeSet::new();
    for (record, _item) in history
        .iter()
        .filter(|(record, _item)| record.query == query && record.pack_hash == pack_hash)
    {
        ensure_equal(
            &record.created_by.as_deref(),
            &Some("ee context"),
            "context pack ledger created_by",
        )?;
        let ledger_json = record
            .ledger_json
            .as_ref()
            .ok_or_else(|| format!("pack record {} missing ledger_json", record.id))?;
        let ledger_hash = record
            .ledger_hash
            .as_ref()
            .ok_or_else(|| format!("pack record {} missing ledger_hash", record.id))?;
        ensure(
            ledger_hash.starts_with("blake3:"),
            format!(
                "pack record {} ledger_hash must be blake3-prefixed",
                record.id
            ),
        )?;

        let ledger: JsonValue = serde_json::from_str(ledger_json)
            .map_err(|error| format!("pack record {} ledger JSON malformed: {error}", record.id))?;
        ensure_equal(
            &ledger.pointer("/schema"),
            &Some(&json!(PACK_REPLAY_LEDGER_SCHEMA_V1)),
            "pack ledger schema",
        )?;
        ensure_equal(
            &ledger.pointer("/ledgerHash"),
            &Some(&json!(ledger_hash.as_str())),
            "pack ledger hash field",
        )?;
        ensure_equal(
            &ledger.pointer("/packId"),
            &Some(&json!(record.id.as_str())),
            "pack ledger pack id",
        )?;
        ensure_equal(
            &ledger.pointer("/packHash"),
            &Some(&json!(pack_hash)),
            "pack ledger pack hash",
        )?;
        ensure_equal(
            &ledger.pointer("/request/query/text"),
            &Some(&json!(query)),
            "pack ledger query text",
        )?;
        ensure_equal(
            &ledger.pointer("/candidateCounts/selected"),
            &Some(&json!(record.item_count)),
            "pack ledger selected count",
        )?;
        ensure_equal(
            &ledger.pointer("/candidateCounts/omitted"),
            &Some(&json!(record.omitted_count)),
            "pack ledger omitted count",
        )?;

        let ledger_item_ids = json_array(&ledger, "/selectedItems", "pack ledger selected items")?
            .iter()
            .map(|item| {
                item.get("memoryId")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| "pack ledger selected item missing memoryId".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        ensure_equal(
            &ledger_item_ids,
            &selected_item_ids.to_vec(),
            "pack ledger selected memory order",
        )?;

        pack_ids.insert(record.id.clone());
        ledger_hashes.insert(ledger_hash.clone());
    }
    connection.close().map_err(|error| error.to_string())?;

    ensure(
        pack_ids.len() >= min_records,
        format!(
            "expected at least {min_records} persisted pack ledgers for query {query:?}, got {}",
            pack_ids.len()
        ),
    )?;

    Ok((
        ledger_hashes.into_iter().collect(),
        pack_ids.into_iter().collect(),
    ))
}

fn first_new_pack_id(pack_ids: &[String], already_seen: &[String]) -> Result<String, String> {
    pack_ids
        .iter()
        .find(|pack_id| !already_seen.contains(*pack_id))
        .or_else(|| pack_ids.first())
        .cloned()
        .ok_or_else(|| "expected at least one persisted pack id".to_owned())
}

fn json_string<'a>(value: &'a JsonValue, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{context} missing string at {pointer}"))
}

fn json_array<'a>(
    value: &'a JsonValue,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<JsonValue>, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{context} missing array at {pointer}"))
}

fn json_string_vec(value: &JsonValue, pointer: &str, context: &str) -> Result<Vec<String>, String> {
    json_array(value, pointer, context)?
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context} has non-string value at {pointer}[{index}]"))
        })
        .collect()
}

fn first_pack_quality_comparison<'a>(
    value: &'a JsonValue,
    context: &str,
) -> Result<&'a JsonValue, String> {
    json_array(value, "/data/report/comparisons", context)?
        .first()
        .ok_or_else(|| format!("{context} missing first pack-quality comparison"))
}

fn pack_quality_degraded_codes(value: &JsonValue, context: &str) -> Result<Vec<String>, String> {
    let mut codes = json_array(value, "/data/degradedBranches", context)?
        .iter()
        .filter_map(|branch| branch.get("code").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    codes.sort();
    Ok(codes)
}

fn cass_sessions_array<'a>(
    value: &'a JsonValue,
    context: &str,
) -> Result<&'a Vec<JsonValue>, String> {
    value
        .get("sessions")
        .and_then(JsonValue::as_array)
        .or_else(|| value.pointer("/data/sessions").and_then(JsonValue::as_array))
        .or_else(|| value.get("hits").and_then(JsonValue::as_array))
        .or_else(|| value.as_array())
        .ok_or_else(|| {
            let payload =
                serde_json::to_string(value).unwrap_or_else(|error| format!("<unprintable: {error}>"));
            format!(
                "{context} missing sessions array at /sessions, /data/sessions, or top level; payload={payload}"
            )
        })
}

fn cass_session_source_path(value: &JsonValue) -> Option<&str> {
    value
        .get("path")
        .or_else(|| value.get("source_path"))
        .and_then(JsonValue::as_str)
}

fn degradation_codes(value: &JsonValue) -> Result<Vec<String>, String> {
    degradation_codes_at(value, "/data/degraded", "status degraded")
}

fn hostile_network_envs(log_dir: &Path) -> Vec<(&'static str, OsString)> {
    vec![
        ("HTTP_PROXY", OsString::from("http://127.0.0.1:9")),
        ("HTTPS_PROXY", OsString::from("http://127.0.0.1:9")),
        ("ALL_PROXY", OsString::from("socks5://127.0.0.1:9")),
        ("NO_PROXY", OsString::from("")),
        (
            "EE_CASS_BINARY",
            log_dir.join("missing-cass").into_os_string(),
        ),
        (
            "CASS_DATA_DIR",
            log_dir.join("missing-cass-data").into_os_string(),
        ),
        ("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", OsString::from("1")),
        ("NO_COLOR", OsString::from("1")),
    ]
}

fn degradation_codes_at(
    value: &JsonValue,
    pointer: &str,
    context: &str,
) -> Result<Vec<String>, String> {
    let mut codes = json_array(value, pointer, context)?
        .iter()
        .filter_map(|item| item.get("code").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    codes.sort();
    Ok(codes)
}

fn ensure_no_ansi(text: &str, context: &str) -> TestResult {
    ensure(
        !text.contains("\u{1b}["),
        format!("{context} must not contain ANSI escape sequences"),
    )
}

fn egress_secret_probes() -> Vec<SecretProbe> {
    vec![
        SecretProbe {
            class: "anthropic_api_key",
            raw: "sk-ant-api03-redactionegressredactionegressredactionegress",
        },
        SecretProbe {
            class: "aws_secret_access_key",
            raw: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        },
        SecretProbe {
            class: "jwt_token",
            raw: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.Rq8IjqberX03cRIZHg7v0Rq8IjqberX03cRIZHg7v0",
        },
        SecretProbe {
            class: "pem_private_key_header",
            raw: "-----BEGIN RSA PRIVATE KEY-----",
        },
        SecretProbe {
            class: "pem_private_key_body",
            raw: "MIIEowIBAAKCAQEAredactionegressredactionegressredaction",
        },
    ]
}

fn ensure_text_omits_secret_probes(
    surface: &str,
    stream: &str,
    artifact_path: &str,
    text: &str,
    probes: &[SecretProbe],
) -> TestResult {
    for probe in probes {
        ensure(
            !text.contains(probe.raw),
            format!(
                "{surface} {stream} leaked secret class {}; artifact={artifact_path}",
                probe.class
            ),
        )?;
    }
    Ok(())
}

fn ensure_step_omits_secret_probes(
    surface: &str,
    event: &CommandEvent,
    json_payload: &JsonValue,
    probes: &[SecretProbe],
) -> TestResult {
    ensure_text_omits_secret_probes(
        surface,
        "stdout",
        &event.stdout_artifact_path,
        &event.stdout,
        probes,
    )?;
    ensure_text_omits_secret_probes(
        surface,
        "stderr",
        &event.stderr_artifact_path,
        &event.stderr,
        probes,
    )?;
    ensure_text_omits_secret_probes(
        surface,
        "json",
        &event.stdout_artifact_path,
        &json_payload.to_string(),
        probes,
    )?;
    let logged_args = serde_json::to_string(&event.args)
        .map_err(|error| format!("failed to serialize logged argv: {error}"))?;
    ensure_text_omits_secret_probes(
        surface,
        "logged-argv",
        "commands.jsonl",
        &logged_args,
        probes,
    )
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct SwarmBriefE2eCase {
    id: &'static str,
    workspace: PathBuf,
    args: Vec<String>,
    envs: Vec<(&'static str, OsString)>,
    agent_mail_snapshot_path: Option<PathBuf>,
    expected_degraded_codes: Vec<&'static str>,
    expected_recommendation_ids: Vec<String>,
    expected_recommendation_kinds: Vec<&'static str>,
    expected_reason_codes: Vec<&'static str>,
    expected_ready_source_reason_codes: Vec<(&'static str, &'static str)>,
    expected_source_statuses: Vec<(&'static str, &'static str)>,
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq)]
struct CoordinationSnapshot {
    beads_status_digest: Option<Vec<String>>,
    git_status_digest: Option<String>,
    agent_mail_snapshot_hash: Option<String>,
    ee_db_exists: bool,
    support_bundle_exists: bool,
}

#[cfg(unix)]
fn run_tool(
    program: &str,
    args: &[&str],
    cwd: &Path,
    envs: &[(&str, OsString)],
    context: &str,
) -> Result<String, String> {
    let mut command = Command::new(tool_program(program));
    command.current_dir(cwd).args(args).env("CI", "1");
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run {program} for {context}: {error}"))?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{program} {context} stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("{program} {context} stderr was not UTF-8: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "{program} {context} failed with exit {:?}; stdout={}; stderr={}",
            output.status.code(),
            stream_snippet(&stdout),
            stream_snippet(&stderr)
        ),
    )?;
    Ok(stdout)
}

#[cfg(unix)]
fn tool_program(program: &str) -> OsString {
    if program == "br" {
        let local_br = Path::new("/home/ubuntu/.local/bin/br");
        if local_br.is_file() {
            return local_br.as_os_str().to_os_string();
        }
    }
    OsString::from(program)
}

#[cfg(unix)]
fn default_tool_path() -> Result<OsString, String> {
    let mut entries = vec![PathBuf::from("/home/ubuntu/.local/bin")];
    if let Some(existing) = env::var_os("PATH") {
        entries.extend(env::split_paths(&existing));
    }
    env::join_paths(entries).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn path_with_front(front: &Path) -> Result<OsString, String> {
    let mut entries = vec![
        front.to_path_buf(),
        PathBuf::from("/home/ubuntu/.local/bin"),
    ];
    if let Some(existing) = env::var_os("PATH") {
        entries.extend(env::split_paths(&existing));
    }
    env::join_paths(entries).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn write_fixture_executable(bin_dir: &Path, name: &str, body: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(bin_dir).map_err(|error| {
        format!(
            "failed to create fixture bin {}: {error}",
            bin_dir.display()
        )
    })?;
    let path = bin_dir.join(name);
    write_text(&path, body)?;
    let mut permissions = fs::metadata(&path)
        .map_err(|error| {
            format!(
                "failed to stat fixture executable {}: {error}",
                path.display()
            )
        })?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).map_err(|error| {
        format!(
            "failed to chmod fixture executable {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

#[cfg(unix)]
fn setup_git_workspace(workspace: &Path) -> TestResult {
    fs::create_dir_all(workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    run_tool("git", &["init", "-q"], workspace, &[], "init workspace git")?;
    run_tool(
        "git",
        &["config", "user.email", "swarm-brief-e2e@example.invalid"],
        workspace,
        &[],
        "configure git email",
    )?;
    run_tool(
        "git",
        &["config", "user.name", "Swarm Brief E2E"],
        workspace,
        &[],
        "configure git user",
    )?;
    write_text(
        &workspace.join(".gitignore"),
        "# bv (beads viewer) local config and caches\n.bv/\n",
    )?;
    Ok(())
}

#[cfg(unix)]
fn commit_workspace_state(workspace: &Path, message: &str) -> TestResult {
    run_tool(
        "git",
        &["add", "-A"],
        workspace,
        &[],
        "stage workspace baseline",
    )?;
    run_tool(
        "git",
        &["commit", "--allow-empty", "-q", "-m", message],
        workspace,
        &[],
        "commit workspace baseline",
    )
    .map(|_| ())
}

#[cfg(unix)]
fn init_beads_workspace(workspace: &Path) -> TestResult {
    run_tool(
        "br",
        &["init", "--prefix", "swarm"],
        workspace,
        &[],
        "init beads",
    )
    .map(|_| ())
}

#[cfg(unix)]
fn create_bead(workspace: &Path, title: &str, status: Option<&str>) -> Result<String, String> {
    let mut args = vec![
        "create",
        title,
        "--type",
        "test",
        "--priority",
        "1",
        "--description",
        "swarm brief e2e fixture bead",
        "--json",
    ];
    if let Some(status) = status {
        args.push("--status");
        args.push(status);
    }
    let output = run_tool("br", &args, workspace, &[], "create bead")?;
    let value = serde_json::from_str::<JsonValue>(&output)
        .map_err(|error| format!("br create output was not JSON: {error}; output={output}"))?;
    string_at(&value, "/id", "created bead")
}

#[cfg(unix)]
fn seed_ready_bead(workspace: &Path, title: &str) -> Result<String, String> {
    init_beads_workspace(workspace)?;
    create_bead(workspace, title, None)
}

#[cfg(unix)]
fn seed_blocked_beads(workspace: &Path) -> Result<(String, String), String> {
    init_beads_workspace(workspace)?;
    let blocker = create_bead(
        workspace,
        "[swarm-brief][owner] Active owner follow-up",
        None,
    )?;
    run_tool(
        "br",
        &["update", &blocker, "--status", "in_progress", "--json"],
        workspace,
        &[],
        "mark blocker in progress",
    )?;
    let blocked = create_bead(
        workspace,
        "[swarm-brief][critical] Blocked critical path",
        None,
    )?;
    run_tool(
        "br",
        &["dep", "add", &blocked, &blocker, "--json"],
        workspace,
        &[],
        "add blocked dependency",
    )?;
    Ok((blocked, blocker))
}

#[cfg(unix)]
fn write_agent_mail_snapshot(
    path: &Path,
    reservation_path: Option<&str>,
    include_secret_probe: bool,
) -> TestResult {
    let subject = if include_secret_probe {
        "token ghp_abcdefghijklmnopqrstuvwxyz1234567890"
    } else {
        "swarm brief handoff"
    };
    let holder = if include_secret_probe {
        "Agent ghp_abcdefghijklmnopqrstuvwxyz1234567890"
    } else {
        "IndigoBrook"
    };
    let body = if include_secret_probe {
        r#""body_md":"raw secret body","#
    } else {
        ""
    };
    let reservations = reservation_path.map_or_else(
        || "[]".to_string(),
        |path| {
            format!(
                r#"[{{"path_pattern":"{path}","holder":"{holder}","exclusive":true,"expires_ts":"2026-05-09T20:00:00Z"}}]"#
            )
        },
    );
    write_text(
        path,
        &format!(
            r#"{{
  "file_reservations": {reservations},
  "inbox": [{{"mailbox":"QuietFrog","unread_count":1,"ack_required_count":0}}],
  "threads": [{{"thread_id":"eidetic_engine_cli-8x4x","subject":"{subject}",{body}"message_count":2,"last_activity_at":"2026-05-09T19:00:00Z"}}]
}}"#
        ),
    )
}

#[cfg(unix)]
fn file_hash(path: &Path) -> Result<Option<String>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    Ok(Some(format!("blake3:{}", blake3::hash(&bytes).to_hex())))
}

#[cfg(unix)]
fn beads_status_digest(workspace: &Path) -> Result<Option<Vec<String>>, String> {
    if !workspace.join(".beads").is_dir() {
        return Ok(None);
    }
    let output = run_tool(
        "br",
        &["list", "--json"],
        workspace,
        &[],
        "read beads digest",
    )?;
    let value = serde_json::from_str::<JsonValue>(&output)
        .map_err(|error| format!("br list output was not JSON: {error}"))?;
    let issues = value
        .as_array()
        .or_else(|| value.get("issues").and_then(JsonValue::as_array))
        .ok_or_else(|| "br list output must contain an issues array".to_string())?;
    let mut digest = issues
        .iter()
        .map(|item| {
            let id = item.get("id").and_then(JsonValue::as_str).unwrap_or("");
            let status = item.get("status").and_then(JsonValue::as_str).unwrap_or("");
            let title = item.get("title").and_then(JsonValue::as_str).unwrap_or("");
            format!("{id}:{status}:{title}")
        })
        .collect::<Vec<_>>();
    digest.sort();
    Ok(Some(digest))
}

#[cfg(unix)]
fn git_status_digest(workspace: &Path) -> Result<Option<String>, String> {
    if !workspace.join(".git").is_dir() {
        return Ok(None);
    }
    run_tool(
        "git",
        &["status", "--short", "--untracked-files=all"],
        workspace,
        &[],
        "read git digest",
    )
    .map(Some)
}

#[cfg(unix)]
fn coordination_snapshot(
    workspace: &Path,
    agent_mail_snapshot_path: Option<&Path>,
) -> Result<CoordinationSnapshot, String> {
    Ok(CoordinationSnapshot {
        beads_status_digest: beads_status_digest(workspace)?,
        git_status_digest: git_status_digest(workspace)?,
        agent_mail_snapshot_hash: agent_mail_snapshot_path
            .map(file_hash)
            .transpose()?
            .flatten(),
        ee_db_exists: workspace.join(".ee").join("ee.db").exists(),
        support_bundle_exists: workspace.join(".ee").join("support").exists()
            || workspace.join("support-bundles").exists(),
    })
}

#[cfg(unix)]
fn swarm_brief_degraded_codes(value: &JsonValue) -> Vec<String> {
    let mut codes = value
        .pointer("/data/degraded")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("code").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    codes.sort();
    codes
}

#[cfg(unix)]
fn swarm_brief_recommendations(value: &JsonValue) -> Result<&Vec<JsonValue>, String> {
    value
        .pointer("/data/recommendations")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "swarm brief output missing /data/recommendations".to_string())
}

#[cfg(unix)]
fn swarm_brief_recommendation_ids(value: &JsonValue) -> Result<Vec<String>, String> {
    let mut ids = swarm_brief_recommendations(value)?
        .iter()
        .filter_map(|item| item.get("id").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
}

#[cfg(unix)]
fn swarm_brief_recommendation_kinds(value: &JsonValue) -> Result<Vec<String>, String> {
    let mut kinds = swarm_brief_recommendations(value)?
        .iter()
        .filter_map(|item| item.get("kind").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    kinds.sort();
    kinds.dedup();
    Ok(kinds)
}

#[cfg(unix)]
fn swarm_brief_reason_codes(value: &JsonValue) -> Result<Vec<String>, String> {
    let mut codes = Vec::new();
    for recommendation in swarm_brief_recommendations(value)? {
        if let Some(items) = recommendation
            .get("reasonCodes")
            .and_then(JsonValue::as_array)
        {
            codes.extend(
                items
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(str::to_owned),
            );
        }
    }
    codes.sort();
    codes.dedup();
    Ok(codes)
}

#[cfg(unix)]
fn swarm_brief_source_statuses(value: &JsonValue) -> Result<Vec<(String, String)>, String> {
    let mut statuses = value
        .pointer("/data/sources")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "swarm brief output missing /data/sources".to_string())?
        .iter()
        .map(|source| {
            let name = source
                .get("source")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned();
            let status = source
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned();
            (name, status)
        })
        .collect::<Vec<_>>();
    statuses.sort();
    Ok(statuses)
}

#[cfg(unix)]
fn swarm_brief_source_log(value: &JsonValue) -> Result<Vec<JsonValue>, String> {
    value
        .pointer("/data/sources")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "swarm brief output missing /data/sources".to_string())
        .map(|sources| {
            sources
                .iter()
                .map(|source| {
                    json!({
                        "source": source.get("source").cloned().unwrap_or(JsonValue::Null),
                        "status": source.get("status").cloned().unwrap_or(JsonValue::Null),
                        "freshness": source.get("freshness").cloned().unwrap_or(JsonValue::Null),
                        "provenance": source.get("provenance").cloned().unwrap_or(JsonValue::Null),
                        "itemCount": source.get("itemCount").cloned().unwrap_or(JsonValue::Null),
                    })
                })
                .collect()
        })
}

#[cfg(unix)]
fn ensure_contains_strings(
    actual: &[String],
    expected: &[impl AsRef<str>],
    context: &str,
) -> TestResult {
    for expected_item in expected {
        let expected_item = expected_item.as_ref();
        ensure(
            actual
                .iter()
                .any(|actual_item| actual_item == expected_item),
            format!("{context}: expected {expected_item:?} in {actual:?}"),
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_source_statuses(
    actual: &[(String, String)],
    expected: &[(&str, &str)],
    reason_codes: &[String],
    context: &str,
) -> TestResult {
    for (source, status) in expected {
        if *status == "ready_or_skipped" {
            let matched_status = actual
                .iter()
                .find(|(actual_source, _)| actual_source == source)
                .map(|(_, actual_status)| actual_status.as_str());
            ensure(
                matches!(matched_status, Some("ready" | "skipped")),
                format!("{context}: expected source {source}:ready_or_skipped in {actual:?}"),
            )?;
            if matched_status == Some("skipped") {
                ensure(
                    reason_codes
                        .iter()
                        .any(|reason_code| reason_code == "source_status:skipped"),
                    format!(
                        "{context}: skipped source {source} missing source_status:skipped reason code in {reason_codes:?}"
                    ),
                )?;
            }
            continue;
        }
        ensure(
            actual.iter().any(|(actual_source, actual_status)| {
                actual_source == source && actual_status == status
            }),
            format!("{context}: expected source {source}:{status} in {actual:?}"),
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_swarm_stdout_is_machine_only(
    event: &CommandEvent,
    value: &JsonValue,
    probes: &[SecretProbe],
) -> TestResult {
    ensure_no_ansi(&event.stdout, "swarm brief stdout")?;
    ensure_no_ansi(&event.stderr, "swarm brief stderr")?;
    ensure_step_omits_secret_probes("swarm brief", event, value, probes)?;
    ensure(
        !event.stdout.contains("error:") && !event.stdout.contains("\nNext:\n"),
        "swarm brief JSON stdout must not contain human diagnostics",
    )?;
    ensure(
        event.stderr.is_empty(),
        "swarm brief JSON stderr must remain empty",
    )
}

#[cfg(unix)]
fn run_swarm_brief_case(
    case: SwarmBriefE2eCase,
    events_path: &Path,
    artifact_dir: &Path,
    probes: &[SecretProbe],
) -> TestResult {
    let before = coordination_snapshot(&case.workspace, case.agent_mail_snapshot_path.as_deref())?;
    let (event, value) = run_step_with_env(
        "swarm_brief_logged_coordination",
        events_path,
        artifact_dir,
        &case.workspace,
        StepSpec {
            name: case.id,
            args: case.args,
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &case.envs,
    )?;
    ensure_swarm_stdout_is_machine_only(&event, &value, probes)?;
    ensure_equal(
        &value.pointer("/data/schema"),
        &Some(&json!("ee.swarm.brief.v1")),
        "swarm brief data schema",
    )?;
    ensure_equal(
        &value.pointer("/data/redactionStatus"),
        &Some(&json!("paths_counts_subjects_only_no_content")),
        "swarm brief redaction status",
    )?;

    let degraded_codes = swarm_brief_degraded_codes(&value);
    ensure_contains_strings(
        &degraded_codes,
        &case.expected_degraded_codes,
        &format!("{} degraded codes", case.id),
    )?;
    let recommendation_ids = swarm_brief_recommendation_ids(&value)?;
    ensure_contains_strings(
        &recommendation_ids,
        &case.expected_recommendation_ids,
        &format!("{} recommendation ids", case.id),
    )?;
    let recommendation_kinds = swarm_brief_recommendation_kinds(&value)?;
    ensure_contains_strings(
        &recommendation_kinds,
        &case.expected_recommendation_kinds,
        &format!("{} recommendation kinds", case.id),
    )?;
    let reason_codes = swarm_brief_reason_codes(&value)?;
    ensure_contains_strings(
        &reason_codes,
        &case.expected_reason_codes,
        &format!("{} reason codes", case.id),
    )?;
    let source_statuses = swarm_brief_source_statuses(&value)?;
    ensure_source_statuses(
        &source_statuses,
        &case.expected_source_statuses,
        &reason_codes,
        &format!("{} source statuses", case.id),
    )?;
    for (source, reason_code) in &case.expected_ready_source_reason_codes {
        if source_statuses
            .iter()
            .any(|(actual_source, actual_status)| {
                actual_source == source && actual_status == "ready"
            })
        {
            ensure_contains_strings(
                &reason_codes,
                &[*reason_code],
                &format!("{} ready source reason codes", case.id),
            )?;
        }
    }

    let after = coordination_snapshot(&case.workspace, case.agent_mail_snapshot_path.as_deref())?;
    ensure_equal(
        &after.beads_status_digest,
        &before.beads_status_digest,
        "swarm brief must not mutate Beads statuses",
    )?;
    ensure_equal(
        &after.git_status_digest,
        &before.git_status_digest,
        "swarm brief must not mutate git state",
    )?;
    ensure_equal(
        &after.agent_mail_snapshot_hash,
        &before.agent_mail_snapshot_hash,
        "swarm brief must not mutate Agent Mail snapshot fixture",
    )?;
    ensure_equal(
        &after.ee_db_exists,
        &before.ee_db_exists,
        "swarm brief must not create or mutate EE DB rows",
    )?;
    ensure_equal(
        &after.support_bundle_exists,
        &before.support_bundle_exists,
        "swarm brief must not create support bundles",
    )?;
    ensure(
        !Path::new(&event.stdout_artifact_path).starts_with(&case.workspace)
            && !Path::new(&event.stderr_artifact_path).starts_with(&case.workspace),
        "swarm brief harness artifacts must stay outside the tested workspace",
    )?;

    append_jsonl(
        events_path,
        &json!({
            "schema": "ee.e2e.swarm_brief_scenario.v1",
            "scenarioId": case.id,
            "commandEventLog": events_path.display().to_string(),
            "workspace": case.workspace.display().to_string(),
            "stdoutArtifactPath": event.stdout_artifact_path,
            "stderrArtifactPath": event.stderr_artifact_path,
            "schemaValidationStatus": event.schema_validation_status,
            "goldenValidationStatus": "not_applicable",
            "redactionStatus": value.pointer("/data/redactionStatus").and_then(JsonValue::as_str).unwrap_or("missing"),
            "sourceFreshness": swarm_brief_source_log(&value)?,
            "degradationCodes": degraded_codes,
            "recommendationIds": recommendation_ids,
            "recommendationKinds": recommendation_kinds,
            "reasonCodes": reason_codes,
            "mutationChecks": {
                "beadsStatus": before.beads_status_digest == after.beads_status_digest,
                "gitStatus": before.git_status_digest == after.git_status_digest,
                "agentMailSnapshot": before.agent_mail_snapshot_hash == after.agent_mail_snapshot_hash,
                "eeDbExists": before.ee_db_exists == after.ee_db_exists,
                "supportBundleExists": before.support_bundle_exists == after.support_bundle_exists,
                "artifactPathsOutsideWorkspace": true
            },
            "firstFailureDiagnosis": event.first_failure,
        }),
    )
}

#[cfg(unix)]
fn real_cass_binary_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("EE_CASS_BINARY") {
        let path = PathBuf::from(path);
        ensure(
            path.is_absolute(),
            format!("EE_CASS_BINARY must be absolute for no-mocks CASS e2e: {path:?}"),
        )?;
        ensure(
            path.file_name().and_then(|name| name.to_str()) == Some("cass"),
            format!("EE_CASS_BINARY must point to a real cass executable: {path:?}"),
        )?;
        ensure(path.is_file(), format!("cass binary not found: {path:?}"))?;
        return path.canonicalize().map_err(|error| error.to_string());
    }

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join("cass");
            if candidate.is_file() {
                return candidate.canonicalize().map_err(|error| error.to_string());
            }
        }
    }

    Err("real cass binary not found on PATH".to_owned())
}

#[cfg(unix)]
fn path_with_binary_parent(binary: &Path) -> Result<OsString, String> {
    let parent = binary
        .parent()
        .ok_or_else(|| format!("cass binary has no parent directory: {binary:?}"))?;
    let mut entries = vec![parent.to_path_buf()];
    if let Some(existing) = env::var_os("PATH") {
        entries.extend(env::split_paths(&existing));
    }
    env::join_paths(entries).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn write_codex_cass_fixture_session(
    codex_home: &Path,
    workspace: &Path,
) -> Result<PathBuf, String> {
    let sessions_dir = codex_home
        .join("sessions")
        .join("2026")
        .join("05")
        .join("06");
    fs::create_dir_all(&sessions_dir).map_err(|error| error.to_string())?;
    let session_path = sessions_dir.join("rollout-x65f-cass-import.jsonl");
    let workspace_path = workspace.display().to_string();
    let records = [
        json!({
            "timestamp": "2026-05-06T03:40:00Z",
            "type": "session_meta",
            "payload": {
                "id": "x65f-cass-import-fixture",
                "cwd": workspace_path,
                "cli_version": "0.42.0"
            }
        }),
        json!({
            "timestamp": "2026-05-06T03:40:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": "x65f cass import fixture release evidence alpha"
                    }
                ]
            }
        }),
        json!({
            "timestamp": "2026-05-06T03:40:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "x65f imported CASS evidence remains durable and searchable"
                    }
                ]
            }
        }),
    ];

    let mut jsonl = String::new();
    for record in records {
        jsonl.push_str(&serde_json::to_string(&record).map_err(|error| error.to_string())?);
        jsonl.push('\n');
    }
    write_text(&session_path, &jsonl)?;
    Ok(session_path)
}

#[cfg(unix)]
fn run_cass_json(
    args: &[OsString],
    cwd: &Path,
    envs: &[(&str, OsString)],
    context: &str,
) -> Result<JsonValue, String> {
    let mut command = Command::new("cass");
    command.current_dir(cwd).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run cass for {context}: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        format!(
            "cass {context} failed with exit {:?}: {stderr}",
            output.status.code()
        ),
    )?;
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cass {context} stdout must be JSON: {error}"))
}

#[cfg(unix)]
#[test]
fn no_mocks_swarm_brief_logged_coordination_scenarios() -> TestResult {
    let scenario_id = "swarm_brief_logged_coordination";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");
    let workspaces_dir = log_dir.join("workspaces");
    fs::create_dir_all(&workspaces_dir)
        .map_err(|error| format!("failed to create workspaces dir: {error}"))?;

    let rch_low_bin = log_dir.join("fixture-bin-rch-low");
    write_fixture_executable(
        &rch_low_bin,
        "rch",
        "#!/bin/sh\nprintf '%s\\n' '{\"queue_depth\":0,\"active_builds\":0}'\n",
    )?;
    let rch_low_path = path_with_front(&rch_low_bin)?;

    let rch_high_bin = log_dir.join("fixture-bin-rch-high");
    write_fixture_executable(
        &rch_high_bin,
        "rch",
        "#!/bin/sh\nprintf '%s\\n' '{\"queue_depth\":9,\"active_builds\":12}'\n",
    )?;
    let rch_high_path = path_with_front(&rch_high_bin)?;

    let rch_unavailable_bin = log_dir.join("fixture-bin-rch-unavailable");
    write_fixture_executable(
        &rch_unavailable_bin,
        "rch",
        "#!/bin/sh\necho 'fixture rch unavailable' >&2\nexit 127\n",
    )?;
    let rch_unavailable_path = path_with_front(&rch_unavailable_bin)?;

    let bv_unavailable_bin = log_dir.join("fixture-bin-bv-unavailable");
    write_fixture_executable(
        &bv_unavailable_bin,
        "bv",
        "#!/bin/sh\necho 'fixture bv unavailable' >&2\nexit 127\n",
    )?;
    let bv_unavailable_path = path_with_front(&bv_unavailable_bin)?;

    let br_locked_bin = log_dir.join("fixture-bin-br-locked");
    write_fixture_executable(
        &br_locked_bin,
        "br",
        "#!/bin/sh\nprintf '%s\\n' '{\"error\":\"database is locked\"}'\n",
    )?;
    let br_locked_path = path_with_front(&br_locked_bin)?;

    let make_args = |workspace: &Path, sources: &str, snapshot: Option<&Path>, timeout_ms: u64| {
        let mut args = vec![
            "--workspace".to_owned(),
            workspace.display().to_string(),
            "--json".to_owned(),
            "--fields".to_owned(),
            "full".to_owned(),
            "swarm".to_owned(),
            "brief".to_owned(),
            "--sources".to_owned(),
            sources.to_owned(),
            "--command-timeout-ms".to_owned(),
            timeout_ms.to_string(),
        ];
        if let Some(snapshot) = snapshot {
            args.push("--agent-mail-snapshot".to_owned());
            args.push(snapshot.display().to_string());
        }
        args
    };

    let clean_workspace = workspaces_dir.join("clean-ready");
    setup_git_workspace(&clean_workspace)?;
    let clean_bead_id = seed_ready_bead(
        &clean_workspace,
        "[swarm-brief][e2e] Clean ready coordination work",
    )?;
    let clean_mail = clean_workspace.join("agent-mail-snapshot.json");
    write_agent_mail_snapshot(&clean_mail, None, false)?;
    commit_workspace_state(&clean_workspace, "seed clean swarm brief baseline")?;

    let blocked_workspace = workspaces_dir.join("blocked-critical-path");
    setup_git_workspace(&blocked_workspace)?;
    let (_blocked_bead_id, blocker_bead_id) = seed_blocked_beads(&blocked_workspace)?;
    commit_workspace_state(&blocked_workspace, "seed blocked swarm brief baseline")?;

    let reservation_workspace = workspaces_dir.join("reservation-conflict");
    setup_git_workspace(&reservation_workspace)?;
    let reservation_bead_id = seed_ready_bead(
        &reservation_workspace,
        "[swarm-brief][e2e] Reservation conflict on swarm brief core",
    )?;
    let reservation_mail = reservation_workspace.join("agent-mail-snapshot.json");
    write_agent_mail_snapshot(&reservation_mail, Some("src/core/swarm_brief.rs"), false)?;
    commit_workspace_state(
        &reservation_workspace,
        "seed reservation swarm brief baseline",
    )?;

    let dirty_workspace = workspaces_dir.join("dirty-worktree");
    setup_git_workspace(&dirty_workspace)?;
    let dirty_bead_id = seed_ready_bead(
        &dirty_workspace,
        "[swarm-brief][e2e] Dirty worktree overlap on swarm brief core",
    )?;
    let dirty_surface = dirty_workspace
        .join("src")
        .join("core")
        .join("swarm_brief.rs");
    write_text(&dirty_surface, "baseline swarm brief fixture\n")?;
    run_tool(
        "git",
        &["add", "-f", "src/core/swarm_brief.rs"],
        &dirty_workspace,
        &[],
        "force-add dirty surface baseline",
    )?;
    commit_workspace_state(&dirty_workspace, "seed dirty swarm brief baseline")?;
    write_text(&dirty_surface, "dirty swarm brief fixture\n")?;

    let bv_unavailable_workspace = workspaces_dir.join("bv-unavailable");
    setup_git_workspace(&bv_unavailable_workspace)?;
    let bv_unavailable_bead_id = seed_ready_bead(
        &bv_unavailable_workspace,
        "[swarm-brief][e2e] BV unavailable still reports ready Beads",
    )?;
    commit_workspace_state(
        &bv_unavailable_workspace,
        "seed bv unavailable swarm brief baseline",
    )?;

    let beads_locked_workspace = workspaces_dir.join("beads-locked");
    setup_git_workspace(&beads_locked_workspace)?;
    let _locked_bead_id = seed_ready_bead(
        &beads_locked_workspace,
        "[swarm-brief][e2e] Beads locked fixture",
    )?;
    commit_workspace_state(
        &beads_locked_workspace,
        "seed beads locked swarm brief baseline",
    )?;

    let agent_mail_unavailable_workspace = workspaces_dir.join("agent-mail-unavailable");
    setup_git_workspace(&agent_mail_unavailable_workspace)?;
    commit_workspace_state(
        &agent_mail_unavailable_workspace,
        "seed agent mail unavailable swarm brief baseline",
    )?;

    let rch_unavailable_workspace = workspaces_dir.join("rch-unavailable");
    setup_git_workspace(&rch_unavailable_workspace)?;
    commit_workspace_state(
        &rch_unavailable_workspace,
        "seed rch unavailable swarm brief baseline",
    )?;

    let high_pressure_workspace = workspaces_dir.join("high-resource-pressure");
    setup_git_workspace(&high_pressure_workspace)?;
    commit_workspace_state(
        &high_pressure_workspace,
        "seed high pressure swarm brief baseline",
    )?;

    let ambiguous_selected_workspace = workspaces_dir.join("ambiguous-selected");
    setup_git_workspace(&ambiguous_selected_workspace)?;
    let ambiguous_selected_bead_id = seed_ready_bead(
        &ambiguous_selected_workspace,
        "[swarm-brief][e2e] Explicit workspace wins ambiguous environment",
    )?;
    commit_workspace_state(
        &ambiguous_selected_workspace,
        "seed selected ambiguous swarm brief baseline",
    )?;

    let ambiguous_env_workspace = workspaces_dir.join("ambiguous-env");
    setup_git_workspace(&ambiguous_env_workspace)?;
    let _ambiguous_env_bead_id = seed_ready_bead(
        &ambiguous_env_workspace,
        "[swarm-brief][e2e] Wrong environment workspace candidate",
    )?;
    commit_workspace_state(
        &ambiguous_env_workspace,
        "seed environment ambiguous swarm brief baseline",
    )?;

    let redaction_workspace = workspaces_dir.join("redaction-probe");
    setup_git_workspace(&redaction_workspace)?;
    let redaction_bead_id = seed_ready_bead(
        &redaction_workspace,
        "Use token ghp_abcdefghijklmnopqrstuvwxyz1234567890 in swarm brief",
    )?;
    let redaction_mail = redaction_workspace.join("agent-mail-snapshot.json");
    write_agent_mail_snapshot(&redaction_mail, Some("src/core/swarm_brief.rs"), true)?;
    commit_workspace_state(&redaction_workspace, "seed redaction swarm brief baseline")?;

    let probes = vec![
        SecretProbe {
            class: "github_token",
            raw: "ghp_abcdefghijklmnopqrstuvwxyz1234567890",
        },
        SecretProbe {
            class: "agent_mail_body",
            raw: "raw secret body",
        },
    ];

    let cases = vec![
        SwarmBriefE2eCase {
            id: "clean_workspace_ready_work",
            workspace: clean_workspace.clone(),
            args: make_args(
                &clean_workspace,
                "git,beads,bv,agent-mail,rch,host-profile",
                Some(&clean_mail),
                2_000,
            ),
            envs: vec![("PATH", rch_low_path.clone())],
            agent_mail_snapshot_path: Some(clean_mail),
            expected_degraded_codes: Vec::new(),
            expected_recommendation_ids: vec![format!("rec.candidate.{clean_bead_id}")],
            expected_recommendation_kinds: vec!["safe_surface_candidate"],
            expected_reason_codes: vec!["ready_bead_available"],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![
                ("agent_mail", "ready"),
                ("beads", "ready"),
                ("bv", "ready_or_skipped"),
                ("git", "ready"),
                ("rch", "ready"),
            ],
        },
        SwarmBriefE2eCase {
            id: "no_ready_work_blocked_critical_path",
            workspace: blocked_workspace.clone(),
            args: make_args(&blocked_workspace, "beads,bv", None, 2_000),
            envs: Vec::new(),
            agent_mail_snapshot_path: None,
            expected_degraded_codes: Vec::new(),
            expected_recommendation_ids: vec![
                "rec.work_selection.no_ready_beads".to_owned(),
                format!("rec.in_progress_follow_up.{blocker_bead_id}"),
            ],
            expected_recommendation_kinds: vec!["stale_in_progress_follow_up", "work_selection"],
            expected_reason_codes: vec![
                "no_ready_work",
                "in_progress_owner_follow_up",
                "in_progress_without_assignee",
            ],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![("beads", "ready"), ("bv", "ready_or_skipped")],
        },
        SwarmBriefE2eCase {
            id: "active_reservation_conflict",
            workspace: reservation_workspace.clone(),
            args: make_args(
                &reservation_workspace,
                "git,beads,bv,agent-mail",
                Some(&reservation_mail),
                2_000,
            ),
            envs: Vec::new(),
            agent_mail_snapshot_path: Some(reservation_mail),
            expected_degraded_codes: Vec::new(),
            expected_recommendation_ids: vec![format!("rec.candidate.{reservation_bead_id}")],
            expected_recommendation_kinds: vec![
                "candidate_blocked_by_surface_conflict",
                "file_surface_conflict",
            ],
            expected_reason_codes: vec![
                "active_exclusive_reservation",
                "bead_reservation_overlap",
                "candidate_blocked_by_surface_conflict",
            ],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![
                ("agent_mail", "ready"),
                ("beads", "ready"),
                ("bv", "ready_or_skipped"),
            ],
        },
        SwarmBriefE2eCase {
            id: "dirty_worktree_overlap",
            workspace: dirty_workspace.clone(),
            args: make_args(&dirty_workspace, "git,beads,bv", None, 2_000),
            envs: Vec::new(),
            agent_mail_snapshot_path: None,
            expected_degraded_codes: Vec::new(),
            expected_recommendation_ids: vec![format!("rec.candidate.{dirty_bead_id}")],
            expected_recommendation_kinds: vec!["candidate_blocked_by_surface_conflict"],
            expected_reason_codes: vec![
                "dirty_worktree_path",
                "dirty_bead_overlap",
                "candidate_blocked_by_surface_conflict",
            ],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![
                ("beads", "ready"),
                ("bv", "ready_or_skipped"),
                ("git", "ready"),
            ],
        },
        SwarmBriefE2eCase {
            id: "bv_unavailable",
            workspace: bv_unavailable_workspace.clone(),
            args: make_args(&bv_unavailable_workspace, "beads,bv", None, 2_000),
            envs: vec![("PATH", bv_unavailable_path)],
            agent_mail_snapshot_path: None,
            expected_degraded_codes: vec!["bv_unavailable"],
            expected_recommendation_ids: vec![
                format!("rec.candidate.{bv_unavailable_bead_id}"),
                "rec.degraded.bv.bv_unavailable".to_owned(),
            ],
            expected_recommendation_kinds: vec!["safe_surface_candidate", "degraded_capability"],
            expected_reason_codes: vec!["bv_unavailable", "ready_bead_available"],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![("beads", "ready"), ("bv", "unavailable")],
        },
        SwarmBriefE2eCase {
            id: "beads_stale_locked",
            workspace: beads_locked_workspace.clone(),
            args: make_args(&beads_locked_workspace, "beads", None, 250),
            envs: vec![("PATH", br_locked_path)],
            agent_mail_snapshot_path: None,
            expected_degraded_codes: vec!["beads_unavailable"],
            expected_recommendation_ids: vec!["rec.degraded.beads.beads_unavailable".to_owned()],
            expected_recommendation_kinds: vec!["degraded_capability"],
            expected_reason_codes: vec!["beads_unavailable"],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![("beads", "unavailable")],
        },
        SwarmBriefE2eCase {
            id: "agent_mail_unavailable",
            workspace: agent_mail_unavailable_workspace.clone(),
            args: make_args(&agent_mail_unavailable_workspace, "agent-mail", None, 2_000),
            envs: Vec::new(),
            agent_mail_snapshot_path: None,
            expected_degraded_codes: vec!["agent_mail_unavailable"],
            expected_recommendation_ids: vec![
                "rec.degraded.agent_mail.agent_mail_unavailable".to_owned(),
            ],
            expected_recommendation_kinds: vec!["degraded_capability"],
            expected_reason_codes: vec!["agent_mail_unavailable"],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![("agent_mail", "not_configured")],
        },
        SwarmBriefE2eCase {
            id: "rch_unavailable",
            workspace: rch_unavailable_workspace.clone(),
            args: make_args(&rch_unavailable_workspace, "rch", None, 2_000),
            envs: vec![("PATH", rch_unavailable_path)],
            agent_mail_snapshot_path: None,
            expected_degraded_codes: vec!["rch_unavailable"],
            expected_recommendation_ids: vec!["rec.degraded.rch.rch_unavailable".to_owned()],
            expected_recommendation_kinds: vec!["degraded_capability"],
            expected_reason_codes: vec!["rch_unavailable"],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![("rch", "unavailable")],
        },
        SwarmBriefE2eCase {
            id: "high_resource_pressure",
            workspace: high_pressure_workspace.clone(),
            args: make_args(&high_pressure_workspace, "rch,host-profile", None, 2_000),
            envs: vec![("PATH", rch_high_path)],
            agent_mail_snapshot_path: None,
            expected_degraded_codes: Vec::new(),
            expected_recommendation_ids: vec!["rec.resource_pressure.use_rch_for_cargo".to_owned()],
            expected_recommendation_kinds: vec!["resource_pressure"],
            expected_reason_codes: vec![
                "cargo_verification_must_use_rch",
                "resource_pressure_high",
            ],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![("rch", "ready"), ("host_profile", "ready")],
        },
        SwarmBriefE2eCase {
            id: "workspace_ambiguity_explicit_wins",
            workspace: ambiguous_selected_workspace.clone(),
            args: make_args(&ambiguous_selected_workspace, "git,beads", None, 2_000),
            envs: vec![(
                "EE_WORKSPACE",
                ambiguous_env_workspace.as_os_str().to_os_string(),
            )],
            agent_mail_snapshot_path: None,
            expected_degraded_codes: Vec::new(),
            expected_recommendation_ids: vec![format!(
                "rec.candidate.{ambiguous_selected_bead_id}"
            )],
            expected_recommendation_kinds: vec!["safe_surface_candidate"],
            expected_reason_codes: vec!["ready_bead_available"],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![("beads", "ready"), ("git", "ready")],
        },
        SwarmBriefE2eCase {
            id: "redaction_probe_coordination_text",
            workspace: redaction_workspace.clone(),
            args: make_args(
                &redaction_workspace,
                "beads,agent-mail",
                Some(&redaction_mail),
                2_000,
            ),
            envs: Vec::new(),
            agent_mail_snapshot_path: Some(redaction_mail),
            expected_degraded_codes: Vec::new(),
            expected_recommendation_ids: vec![format!("rec.candidate.{redaction_bead_id}")],
            expected_recommendation_kinds: vec![
                "candidate_blocked_by_surface_conflict",
                "file_surface_conflict",
            ],
            expected_reason_codes: vec![
                "active_exclusive_reservation",
                "bead_reservation_overlap",
                "candidate_blocked_by_surface_conflict",
                "ready_bead_available",
            ],
            expected_ready_source_reason_codes: Vec::new(),
            expected_source_statuses: vec![("agent_mail", "ready"), ("beads", "ready")],
        },
    ];
    let case_count = cases.len();
    ensure_equal(&case_count, &11_usize, "swarm brief scenario count")?;
    for case in cases {
        run_swarm_brief_case(case, &events_path, &artifact_dir, &probes)?;
    }

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read swarm brief JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    ensure_equal(
        &events_text
            .matches("\"schema\":\"ee.e2e.swarm_brief_scenario.v1\"")
            .count(),
        &case_count,
        "swarm brief scenario JSONL event count",
    )?;
    ensure(
        !events_text.contains("ghp_") && !events_text.contains("raw secret body"),
        "swarm brief E2E logs must be safe for support-bundle inclusion",
    )?;
    Ok(())
}

#[test]
fn no_mocks_status_json_conformance_logs_capabilities_and_degradations() -> TestResult {
    let scenario_id = "lp4p6_status_json_conformance";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-status-conformance-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");

    let (pre_event, pre_status) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_status_before_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "status".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_no_ansi(&pre_event.stdout, "pre-init status stdout")?;
    ensure_equal(
        &pre_status.pointer("/data/command"),
        &Some(&JsonValue::String("status".to_owned())),
        "pre-init status command",
    )?;
    ensure(
        json_string(&pre_status, "/data/version", "pre-init status")? == env!("CARGO_PKG_VERSION"),
        "pre-init status must report package version",
    )?;
    ensure_equal(
        &json_string(&pre_status, "/data/capabilities/storage", "pre-init status")?,
        &"pending",
        "pre-init storage capability",
    )?;
    ensure_equal(
        &json_string(&pre_status, "/data/capabilities/search", "pre-init status")?,
        &"pending",
        "pre-init search capability",
    )?;
    let pre_codes = degradation_codes(&pre_status)?;
    ensure(
        pre_codes
            .iter()
            .any(|code| code == "storage_not_initialized"),
        format!("pre-init status must diagnose missing storage, got {pre_codes:?}"),
    )?;
    ensure(
        pre_codes
            .iter()
            .any(|code| code == "search_waiting_for_storage"),
        format!("pre-init status must diagnose search waiting for storage, got {pre_codes:?}"),
    )?;

    let (_init_event, _init_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure(
        database_path.is_file(),
        "init must create real database file",
    )?;

    let (post_event, post_status) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_status_after_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "status".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_no_ansi(&post_event.stdout, "post-init status stdout")?;
    ensure_equal(
        &json_string(
            &post_status,
            "/data/capabilities/runtime",
            "post-init status",
        )?,
        &"ready",
        "post-init runtime capability",
    )?;
    ensure_equal(
        &json_string(
            &post_status,
            "/data/capabilities/storage",
            "post-init status",
        )?,
        &"ready",
        "post-init storage capability",
    )?;
    ensure(
        json_string(&post_status, "/data/workspace/root", "post-init status")? == workspace_arg,
        "post-init status must report selected workspace root",
    )?;
    ensure(
        post_status.pointer("/data/memoryHealth").is_some(),
        "post-init status must include memory health object",
    )?;
    ensure(
        post_status.pointer("/data/curationHealth").is_some(),
        "post-init status must include curation health object",
    )?;
    ensure(
        post_status.pointer("/data/feedbackHealth").is_some(),
        "post-init status must include feedback health object",
    )?;
    ensure(
        !json_array(&post_status, "/data/derivedAssets", "post-init status")?.is_empty(),
        "post-init status must report derived assets",
    )?;

    append_jsonl(
        &events_path,
        &json!({
            "schema": "ee.e2e.summary_event.v1",
            "scenarioId": scenario_id,
            "event": "summary",
            "commandCount": 3,
            "workspace": workspace.display().to_string(),
            "databasePath": database_path.display().to_string(),
            "preInitDegradationCodes": pre_codes,
            "postInitStorageCapability": json_string(
                &post_status,
                "/data/capabilities/storage",
                "post-init status",
            )?,
            "stdoutArtifactPaths": [
                pre_event.stdout_artifact_path,
                post_event.stdout_artifact_path,
            ],
        }),
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read status JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    let event_lines = events_text.lines().collect::<Vec<_>>();
    ensure_equal(
        &event_lines.len(),
        &4_usize,
        "status conformance JSONL event count",
    )?;
    for (index, line) in event_lines.iter().enumerate() {
        let event: JsonValue = serde_json::from_str(line)
            .map_err(|error| format!("status JSONL event {index} must parse: {error}"))?;
        ensure(
            event.get("schema").is_some(),
            format!("status JSONL event {index} must include schema"),
        )?;
    }

    Ok(())
}

#[test]
fn no_mocks_core_memory_loop_succeeds_with_hostile_network_env() -> TestResult {
    let scenario_id = "local_first_hostile_network";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");
    let envs = hostile_network_envs(&log_dir);

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-local-first-network-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let local_first_memory =
        "Local-first invariant: init remember search context why status must not require network.";

    let run_local_first_step = |name: &'static str, args: Vec<String>| {
        run_step_with_env(
            scenario_id,
            &events_path,
            &artifact_dir,
            &workspace,
            StepSpec {
                name,
                args,
                expected_exit_code: 0,
                expected_schema: "ee.response.v1",
                expect_clean_stderr: true,
            },
            &envs,
        )
    };

    let (_init_event, init_json) = run_local_first_step(
        "01_init_with_hostile_network_env",
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "init".to_owned(),
        ],
    )?;
    ensure_equal(
        &init_json.pointer("/data/command"),
        &Some(&json!("init")),
        "local-first init command",
    )?;

    let (_remember_event, remember_json) = run_local_first_step(
        "02_remember_with_hostile_network_env",
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "remember".to_owned(),
            "--level".to_owned(),
            "procedural".to_owned(),
            "--kind".to_owned(),
            "rule".to_owned(),
            "--tags".to_owned(),
            "local-first,network-isolation".to_owned(),
            "--source".to_owned(),
            "file://tests/no_mocks_e2e.rs#L2133".to_owned(),
            local_first_memory.to_owned(),
        ],
    )?;
    let memory_id = string_at(&remember_json, "/data/memory_id", "local-first remember")?;

    let (_search_event, search_json) = run_local_first_step(
        "03_search_with_hostile_network_env",
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "search".to_owned(),
            local_first_memory.to_owned(),
            "--limit".to_owned(),
            "5".to_owned(),
            "--relevance-floor".to_owned(),
            "0.0".to_owned(),
        ],
    )?;
    let search_ids = json_array(&search_json, "/data/results", "local-first search")?
        .iter()
        .filter_map(|result| result.get("docId").and_then(JsonValue::as_str))
        .collect::<Vec<_>>();
    ensure(
        search_ids.iter().any(|doc_id| *doc_id == memory_id),
        format!("local-first search should return remembered memory, got {search_ids:?}"),
    )?;

    let (_context_event, context_json) = run_local_first_step(
        "04_context_with_hostile_network_env",
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "context".to_owned(),
            local_first_memory.to_owned(),
            "--max-tokens".to_owned(),
            "1200".to_owned(),
        ],
    )?;
    let context_ids = memory_ids_from_context(&context_json)?;
    ensure(
        context_ids.iter().any(|doc_id| doc_id == &memory_id),
        format!("local-first context should include remembered memory, got {context_ids:?}"),
    )?;

    let (_why_event, why_json) = run_local_first_step(
        "05_why_with_hostile_network_env",
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "why".to_owned(),
            memory_id.clone(),
        ],
    )?;
    ensure_equal(
        &why_json.pointer("/data/found"),
        &Some(&json!(true)),
        "local-first why found",
    )?;
    ensure_equal(
        &why_json.pointer("/data/memoryId"),
        &Some(&json!(memory_id.as_str())),
        "local-first why memory id",
    )?;

    let (_status_event, status_json) = run_local_first_step(
        "06_status_with_hostile_network_env",
        vec![
            "--workspace".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "status".to_owned(),
        ],
    )?;
    ensure_equal(
        &json_string(
            &status_json,
            "/data/capabilities/storage",
            "local-first status",
        )?,
        &"ready",
        "local-first status storage capability",
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read local-first JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    ensure_equal(
        &events_text.lines().count(),
        &6_usize,
        "local-first command event count",
    )?;
    ensure(
        events_text.contains("HTTP_PROXY") && events_text.contains("EE_CASS_BINARY"),
        "local-first command log must record hostile network/CASS env overrides",
    )
}

#[cfg(unix)]
#[test]
fn no_mocks_import_cass_fixture_sessions_stores_spans_and_searches() -> TestResult {
    let scenario_id = "x65f_cass_import_e2e";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace = log_dir.join("workspace");
    let home = log_dir.join("home");
    let codex_home = log_dir.join("codex-home");
    let cass_data_dir = log_dir.join("cass-data");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    fs::create_dir_all(&codex_home).map_err(|error| error.to_string())?;
    fs::create_dir_all(&cass_data_dir).map_err(|error| error.to_string())?;

    let cass_binary = real_cass_binary_path()?;
    let cass_path = path_with_binary_parent(&cass_binary)?;
    let session_path = write_codex_cass_fixture_session(&codex_home, &workspace)?;
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");
    let database_arg = database_path.display().to_string();
    let cass_data_arg = cass_data_dir.display().to_string();
    let session_arg = session_path.display().to_string();
    let envs = vec![
        ("HOME", home.as_os_str().to_owned()),
        ("CODEX_HOME", codex_home.as_os_str().to_owned()),
        ("CASS_DATA_DIR", cass_data_dir.as_os_str().to_owned()),
        ("CASS_IGNORE_SOURCES_CONFIG", OsString::from("1")),
        ("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", OsString::from("1")),
        ("CASS_INDEX_NO_PROGRESS_EVENTS", OsString::from("1")),
        ("NO_COLOR", OsString::from("1")),
        ("EE_CASS_BINARY", cass_binary.as_os_str().to_owned()),
        ("PATH", cass_path),
    ];

    let cass_index = run_cass_json(
        &[
            OsString::from("index"),
            OsString::from("--full"),
            OsString::from("--data-dir"),
            OsString::from(cass_data_arg.clone()),
            OsString::from("--json"),
        ],
        &workspace,
        &envs,
        "index fixture sessions",
    )?;
    ensure_equal(
        &cass_index.pointer("/success"),
        &Some(&json!(true)),
        "cass fixture index success",
    )?;
    ensure_equal(
        &cass_index.pointer("/conversations"),
        &Some(&json!(1)),
        "cass fixture conversation count",
    )?;

    let cass_sessions = run_cass_json(
        &[
            OsString::from("sessions"),
            OsString::from("--workspace"),
            OsString::from(workspace_arg.clone()),
            OsString::from("--json"),
            OsString::from("--data-dir"),
            OsString::from(cass_data_arg.clone()),
            OsString::from("--limit"),
            OsString::from("5"),
        ],
        &workspace,
        &envs,
        "sessions fixture discovery",
    )?;
    let cass_session = cass_sessions_array(&cass_sessions, "cass sessions")?
        .first()
        .ok_or_else(|| "cass fixture discovery returned no sessions".to_owned())?;
    ensure_equal(
        &cass_session_source_path(cass_session),
        &Some(session_arg.as_str()),
        "cass fixture session path",
    )?;
    ensure_equal(
        &cass_session.get("workspace"),
        &Some(&json!(workspace_arg)),
        "cass fixture workspace path",
    )?;

    let (_init_event, _init_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;

    let (_import_event, import_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_import_cass",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "import".to_owned(),
                "cass".to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
                "--limit".to_owned(),
                "5".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    ensure_equal(
        &import_json.pointer("/data/status"),
        &Some(&json!("completed")),
        "CASS import status",
    )?;
    ensure_equal(
        &import_json.pointer("/data/sessionsImported"),
        &Some(&json!(1)),
        "CASS sessions imported",
    )?;
    ensure(
        import_json
            .pointer("/data/spansImported")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0)
            >= 3,
        "CASS import must capture evidence spans from the fixture session",
    )?;
    ensure_equal(
        &import_json.pointer("/data/sessions/0/sourcePath"),
        &Some(&json!(session_arg)),
        "CASS import report source path",
    )?;

    let connection = DbConnection::open(DatabaseConfig::file(database_path.clone()))
        .map_err(|error| error.to_string())?;
    let workspaces = connection
        .list_workspaces()
        .map_err(|error| error.to_string())?;
    ensure_equal(&workspaces.len(), &1_usize, "stored workspace count")?;
    ensure_equal(&workspaces[0].path, &workspace_arg, "stored workspace path")?;
    let sessions = connection
        .list_sessions(&workspaces[0].id)
        .map_err(|error| error.to_string())?;
    ensure_equal(&sessions.len(), &1_usize, "stored CASS session count")?;
    ensure_equal(
        &sessions[0].source_path,
        &Some(session_arg.clone()),
        "stored CASS session source path",
    )?;
    ensure_equal(
        &sessions[0].agent_name,
        &Some("codex".to_owned()),
        "stored CASS session agent",
    )?;
    let spans = connection
        .list_evidence_spans_for_session(&sessions[0].id)
        .map_err(|error| error.to_string())?;
    ensure(
        spans.len() >= 3,
        format!("expected at least 3 imported spans, got {}", spans.len()),
    )?;
    let span_text = spans
        .iter()
        .map(|span| span.excerpt.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    ensure(
        span_text.contains("x65f cass import fixture release evidence alpha"),
        "stored evidence spans must include the fixture user text",
    )?;
    ensure(
        span_text.contains("x65f imported CASS evidence remains durable and searchable"),
        "stored evidence spans must include the fixture assistant text",
    )?;
    ensure(
        spans.iter().all(|span| {
            span.cass_span_id.starts_with(session_arg.as_str()) && !span.content_hash.is_empty()
        }),
        "evidence spans must preserve CASS source IDs and content hashes",
    )?;
    let stored_session_id = sessions[0].id.clone();
    connection.close().map_err(|error| error.to_string())?;

    let (_index_event, index_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_index_rebuild",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "index".to_owned(),
                "rebuild".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    ensure_equal(
        &index_json.pointer("/data/sessions_indexed"),
        &Some(&json!(1)),
        "indexed CASS session count",
    )?;

    let (_search_event, search_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_search_imported_session",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "search".to_owned(),
                "CASS session codex".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    let search_results = json_array(&search_json, "/data/results", "CASS import search")?;
    ensure(
        search_results.iter().any(|result| {
            result
                .get("docId")
                .and_then(JsonValue::as_str)
                .is_some_and(|doc_id| doc_id.eq(stored_session_id.as_str()))
        }),
        "search must retrieve the imported CASS session document",
    )?;

    Ok(())
}

#[test]
fn no_mocks_public_outputs_redact_secret_like_memory_content() -> TestResult {
    let scenario_id = "redaction_egress_public_outputs";
    let probes = egress_secret_probes();
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-redaction-egress-workspace-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let marker = "rynf redaction egress anchor";
    let jwt = probes
        .iter()
        .find(|probe| probe.class == "jwt_token")
        .map(|probe| probe.raw)
        .ok_or_else(|| "jwt probe missing".to_owned())?;
    let raw_content = format!(
        "{marker}. Use API token {}; AWS_SECRET_ACCESS_KEY={}; Authorization: Bearer {jwt}; PEM block {} {} -----END RSA PRIVATE KEY-----.",
        probes[0].raw, probes[1].raw, probes[3].raw, probes[4].raw
    );
    let redaction_report = redact_secret_like_content(&raw_content);
    ensure(
        redaction_report.redacted,
        "redaction fixture must exercise real secret-like content",
    )?;
    ensure_text_omits_secret_probes(
        "redacted fixture",
        "content",
        "in-memory",
        &redaction_report.content,
        &probes,
    )?;
    let redacted_content = redaction_report
        .content
        .replace("Use API token ", "Scanner alpha placeholder ")
        .replace("AWS_SECRET_ACCESS_KEY=", "Scanner beta placeholder ")
        .replace("Authorization: Bearer ", "Scanner gamma placeholder ")
        .replace("PEM block ", "Scanner delta placeholder ")
        .replace("[REDACTED:anthropic_api_key]", "[REDACTED:alpha]")
        .replace("[REDACTED:aws_secret_access_key]", "[REDACTED:beta]")
        .replace("[REDACTED:bearer_token]", "[REDACTED:gamma]")
        .replace("[REDACTED:pem_block]", "[REDACTED:delta]");
    let storage_policy_check = redact_secret_like_content(&redacted_content);
    ensure(
        !storage_policy_check.redacted,
        format!(
            "redacted fixture must be accepted by remember policy, still matched {:?}",
            storage_policy_check.redacted_reasons
        ),
    )?;

    let (_init_event, _init_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;

    let (deny_event, deny_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_reject_raw_secret_like_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "redaction,egress".to_owned(),
                "--source".to_owned(),
                "file://tests/no_mocks_e2e.rs#L1050".to_owned(),
                raw_content,
            ],
            expected_exit_code: 7,
            expected_schema: "ee.error.v2",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("remember policy denial", &deny_event, &deny_json, &probes)?;
    ensure_equal(
        &deny_json.pointer("/error/code"),
        &Some(&JsonValue::String("policy_denied".to_owned())),
        "raw secret-like remember denial code",
    )?;

    let (remember_event, remember_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_remember_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "redaction,egress".to_owned(),
                "--source".to_owned(),
                "file://tests/no_mocks_e2e.rs#L1050".to_owned(),
                redacted_content,
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("remember", &remember_event, &remember_json, &probes)?;
    let memory_id = string_at(
        &remember_json,
        "/data/memory_id",
        "remember redaction egress",
    )?;

    let (index_event, index_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_index_rebuild",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "index".to_owned(),
                "rebuild".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("index rebuild", &index_event, &index_json, &probes)?;

    let (search_event, search_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "05_search_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "search".to_owned(),
                marker.to_owned(),
                "--limit".to_owned(),
                "5".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("search", &search_event, &search_json, &probes)?;
    let search_results = json_array(&search_json, "/data/results", "redaction egress search")?;
    ensure(
        search_results
            .iter()
            .any(|result| result.get("docId").and_then(JsonValue::as_str) == Some(&memory_id)),
        "search should retrieve the redacted memory by non-secret marker",
    )?;

    let (context_event, context_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "06_context_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                marker.to_owned(),
                "--max-tokens".to_owned(),
                "2000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("context", &context_event, &context_json, &probes)?;
    let context_item_ids = memory_ids_from_context(&context_json)?;
    ensure(
        context_item_ids.iter().any(|id| id == &memory_id),
        "context should select the redacted memory by non-secret marker",
    )?;

    let (why_event, why_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "07_why_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "why".to_owned(),
                memory_id,
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("why", &why_event, &why_json, &probes)?;
    ensure_equal(
        &why_json.pointer("/data/found"),
        &Some(&JsonValue::Bool(true)),
        "why should find the redacted memory",
    )?;

    let (support_event, support_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "08_support_bundle_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "support".to_owned(),
                "bundle".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes(
        "support bundle dry-run",
        &support_event,
        &support_json,
        &probes,
    )?;
    ensure_equal(
        &support_json.pointer("/data/schema"),
        &Some(&JsonValue::String("ee.support_bundle.v1".to_owned())),
        "support bundle dry-run schema",
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    ensure_text_omits_secret_probes(
        "command event log",
        "jsonl",
        &events_path.display().to_string(),
        &events_text,
        &probes,
    )
}

#[test]
fn no_mocks_pack_replay_diff_freshness_and_egress_are_logged() -> TestResult {
    let scenario_id = "dmu0_pack_replay_diff_freshness_egress";
    let probes = egress_secret_probes();
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-dmu0-no-mocks-workspace-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");
    let source_path = workspace.join("freshness-source.md");
    let query_file_path = workspace.join("dmu0-query.eeq.json");
    let marker = "dmu0 replay freshness egress anchor";
    let source_content = format!("{marker} safe source evidence line");
    write_text(&source_path, &source_content)?;
    let source_uri = format!("file://{}#L1", source_path.display());
    let jwt = probes
        .iter()
        .find(|probe| probe.class == "jwt_token")
        .map(|probe| probe.raw)
        .ok_or_else(|| "jwt probe missing".to_owned())?;
    let secret_envs = [("EE_E2E_SECRET_PROBE", OsString::from(probes[0].raw))];
    let raw_content = format!(
        "{marker}. Raw token {}; AWS_SECRET_ACCESS_KEY={}; Authorization: Bearer {jwt}; PEM block {} {} -----END RSA PRIVATE KEY-----.",
        probes[0].raw, probes[1].raw, probes[3].raw, probes[4].raw
    );
    let redaction_report = redact_secret_like_content(&raw_content);
    ensure(
        redaction_report.redacted,
        "redaction fixture must exercise real secret-like content",
    )?;
    let redacted_content = redaction_report
        .content
        .replace("Raw token ", "Scanner alpha placeholder ")
        .replace("AWS_SECRET_ACCESS_KEY=", "Scanner beta placeholder ")
        .replace("Authorization: Bearer ", "Scanner gamma placeholder ")
        .replace("PEM block ", "Scanner delta placeholder ")
        .replace("[REDACTED:anthropic_api_key]", "[REDACTED:alpha]")
        .replace("[REDACTED:aws_secret_access_key]", "[REDACTED:beta]")
        .replace("[REDACTED:bearer_token]", "[REDACTED:gamma]")
        .replace("[REDACTED:pem_block]", "[REDACTED:delta]");
    let storage_policy_check = redact_secret_like_content(&redacted_content);
    ensure(
        !storage_policy_check.redacted,
        format!(
            "redacted fixture must be accepted by remember policy, still matched {:?}",
            storage_policy_check.redacted_reasons
        ),
    )?;

    let mut command_count = 0_usize;

    let (_init_event, _init_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &secret_envs,
    )?;
    command_count += 1;

    let (deny_event, deny_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_reject_raw_secret_like_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "dmu0,replay,egress".to_owned(),
                "--source".to_owned(),
                source_uri.clone(),
                raw_content,
            ],
            expected_exit_code: 7,
            expected_schema: "ee.error.v2",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("remember policy denial", &deny_event, &deny_json, &probes)?;

    let (source_event, source_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_remember_source_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "dmu0,replay,freshness".to_owned(),
                "--source".to_owned(),
                source_uri,
                source_content.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("remember source", &source_event, &source_json, &probes)?;
    let source_memory_id = string_at(&source_json, "/data/memory_id", "source remember")?;

    let (redacted_event, redacted_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_remember_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "dmu0,replay,egress".to_owned(),
                "--source".to_owned(),
                "agent-mail://eidetic_engine_cli-dmu0#egress".to_owned(),
                redacted_content,
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "remember redacted",
        &redacted_event,
        &redacted_json,
        &probes,
    )?;
    let redacted_memory_id = string_at(&redacted_json, "/data/memory_id", "redacted remember")?;

    let (index_event, index_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "05_index_rebuild",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "index".to_owned(),
                "rebuild".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("index rebuild", &index_event, &index_json, &probes)?;

    let query = marker.to_owned();
    let query_file_json = serde_json::to_string_pretty(&json!({
        "version": "ee.query.v1",
        "query": {
            "text": query.as_str(),
            "mode": "hybrid"
        },
        "budget": {
            "maxTokens": 4000,
            "candidatePool": 20
        },
        "output": {
            "format": "json",
            "profile": "compact",
            "explain": true
        }
    }))
    .map_err(|error| format!("failed to serialize dmu0 query file: {error}"))?;
    write_text(&query_file_path, &query_file_json)?;
    let query_file_arg = query_file_path.display().to_string();

    let (context_before_event, context_before_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "06_context_before_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                query.clone(),
                "--max-tokens".to_owned(),
                "4000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "context before source change",
        &context_before_event,
        &context_before_json,
        &probes,
    )?;
    let before_codes = degradation_codes(&context_before_json)?;
    ensure(
        !before_codes
            .iter()
            .any(|code| code == "context_evidence_freshness_changed_source"),
        format!("fresh source should not report changed_source before mutation: {before_codes:?}"),
    )?;
    let before_pack_hash = string_at(&context_before_json, "/data/pack/hash", "context before")?;
    let before_item_ids = memory_ids_from_context(&context_before_json)?;
    ensure(
        before_item_ids.iter().any(|id| id == &source_memory_id),
        format!(
            "context before mutation must select source memory {source_memory_id}, got {before_item_ids:?}"
        ),
    )?;
    ensure(
        before_item_ids.iter().any(|id| id == &redacted_memory_id),
        format!(
            "context before mutation must select redacted memory {redacted_memory_id}, got {before_item_ids:?}"
        ),
    )?;
    let (_before_ledger_hashes, before_pack_ids) = assert_context_pack_ledgers_persisted(
        &database_path,
        &query,
        &before_pack_hash,
        &before_item_ids,
        1,
    )?;
    let before_pack_id = first_new_pack_id(&before_pack_ids, &[])?;

    let (pack_query_before_event, pack_query_before_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "06b_pack_query_file_before_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "--query-file".to_owned(),
                query_file_arg.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "pack query-file before source change",
        &pack_query_before_event,
        &pack_query_before_json,
        &probes,
    )?;
    let pack_query_before_codes = degradation_codes(&pack_query_before_json)?;
    ensure(
        !pack_query_before_codes
            .iter()
            .any(|code| code == "context_evidence_freshness_changed_source"),
        format!(
            "query-file pack should not report changed_source before mutation: {pack_query_before_codes:?}"
        ),
    )?;
    let pack_query_before_hash = string_at(
        &pack_query_before_json,
        "/data/pack/hash",
        "pack query-file before",
    )?;
    let pack_query_before_item_ids = memory_ids_from_context(&pack_query_before_json)?;
    ensure(
        pack_query_before_item_ids
            .iter()
            .any(|id| id == &source_memory_id),
        format!(
            "pack query-file before mutation must select source memory {source_memory_id}, got {pack_query_before_item_ids:?}"
        ),
    )?;
    ensure(
        pack_query_before_item_ids
            .iter()
            .any(|id| id == &redacted_memory_id),
        format!(
            "pack query-file before mutation must select redacted memory {redacted_memory_id}, got {pack_query_before_item_ids:?}"
        ),
    )?;
    let (_pack_query_before_ledger_hashes, pack_query_before_pack_ids) =
        assert_context_pack_ledgers_persisted(
            &database_path,
            &query,
            &pack_query_before_hash,
            &pack_query_before_item_ids,
            1,
        )?;
    let pack_query_before_pack_id = first_new_pack_id(
        &pack_query_before_pack_ids,
        std::slice::from_ref(&before_pack_id),
    )?;

    write_text(
        &source_path,
        &format!("{marker} safe source evidence changed after first pack"),
    )?;

    let (context_after_event, context_after_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "07_context_after_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                query.clone(),
                "--max-tokens".to_owned(),
                "4000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "context after source change",
        &context_after_event,
        &context_after_json,
        &probes,
    )?;
    let after_codes = degradation_codes(&context_after_json)?;
    ensure(
        after_codes
            .iter()
            .any(|code| code == "context_evidence_freshness_changed_source"),
        format!("context after mutation must report changed_source, got {after_codes:?}"),
    )?;
    let after_pack_hash = string_at(&context_after_json, "/data/pack/hash", "context after")?;
    let after_item_ids = memory_ids_from_context(&context_after_json)?;
    ensure(
        after_item_ids.iter().any(|id| id == &source_memory_id),
        format!(
            "context after mutation must select source memory {source_memory_id}, got {after_item_ids:?}"
        ),
    )?;
    let (_after_ledger_hashes, after_pack_ids) = assert_context_pack_ledgers_persisted(
        &database_path,
        &query,
        &after_pack_hash,
        &after_item_ids,
        1,
    )?;
    let after_pack_id = first_new_pack_id(&after_pack_ids, std::slice::from_ref(&before_pack_id))?;

    let (pack_query_after_event, pack_query_after_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "07b_pack_query_file_after_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "--query-file".to_owned(),
                query_file_arg,
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "pack query-file after source change",
        &pack_query_after_event,
        &pack_query_after_json,
        &probes,
    )?;
    let pack_query_after_codes = degradation_codes(&pack_query_after_json)?;
    ensure(
        pack_query_after_codes
            .iter()
            .any(|code| code == "context_evidence_freshness_changed_source"),
        format!(
            "pack query-file after mutation must report changed_source, got {pack_query_after_codes:?}"
        ),
    )?;
    let pack_query_after_hash = string_at(
        &pack_query_after_json,
        "/data/pack/hash",
        "pack query-file after",
    )?;
    let pack_query_after_item_ids = memory_ids_from_context(&pack_query_after_json)?;
    ensure(
        pack_query_after_item_ids
            .iter()
            .any(|id| id == &source_memory_id),
        format!(
            "pack query-file after mutation must select source memory {source_memory_id}, got {pack_query_after_item_ids:?}"
        ),
    )?;
    let (_pack_query_after_ledger_hashes, pack_query_after_pack_ids) =
        assert_context_pack_ledgers_persisted(
            &database_path,
            &query,
            &pack_query_after_hash,
            &pack_query_after_item_ids,
            1,
        )?;
    let pack_query_after_pack_id = first_new_pack_id(
        &pack_query_after_pack_ids,
        &[
            before_pack_id.clone(),
            after_pack_id.clone(),
            pack_query_before_pack_id.clone(),
        ],
    )?;

    let (why_event, why_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "08_why_after_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "why".to_owned(),
                source_memory_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("why after source change", &why_event, &why_json, &probes)?;
    let why_codes = degradation_codes(&why_json)?;
    ensure(
        why_codes
            .iter()
            .any(|code| code == "why_evidence_freshness_changed_source"),
        format!("why after mutation must report changed_source, got {why_codes:?}"),
    )?;

    let (replay_before_event, replay_before_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "09_pack_replay_before_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "replay".to_owned(),
                before_pack_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.pack.replay.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "pack replay before",
        &replay_before_event,
        &replay_before_json,
        &probes,
    )?;
    ensure_equal(
        &replay_before_json.pointer("/data/replay/status"),
        &Some(&json!("available")),
        "pack replay before status",
    )?;

    let (replay_after_event, replay_after_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "10_pack_replay_after_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "replay".to_owned(),
                after_pack_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.pack.replay.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "pack replay after",
        &replay_after_event,
        &replay_after_json,
        &probes,
    )?;
    ensure_equal(
        &replay_after_json.pointer("/data/replay/status"),
        &Some(&json!("available")),
        "pack replay after status",
    )?;
    let replay_after_codes = degradation_codes_at(
        &replay_after_json,
        "/data/replay/degraded",
        "pack replay after ledger degraded",
    )?;
    ensure(
        replay_after_codes
            .iter()
            .any(|code| code == "context_evidence_freshness_changed_source"),
        format!(
            "pack replay after mutation must expose ledger freshness degradation, got {replay_after_codes:?}"
        ),
    )?;

    let (pack_query_replay_after_event, pack_query_replay_after_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "10b_pack_query_file_replay_after_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "replay".to_owned(),
                pack_query_after_pack_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.pack.replay.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "pack query-file replay after",
        &pack_query_replay_after_event,
        &pack_query_replay_after_json,
        &probes,
    )?;
    ensure_equal(
        &pack_query_replay_after_json.pointer("/data/replay/status"),
        &Some(&json!("available")),
        "pack query-file replay after status",
    )?;
    let pack_query_replay_after_codes = degradation_codes_at(
        &pack_query_replay_after_json,
        "/data/replay/degraded",
        "pack query-file replay after ledger degraded",
    )?;
    ensure(
        pack_query_replay_after_codes
            .iter()
            .any(|code| code == "context_evidence_freshness_changed_source"),
        format!(
            "pack query-file replay after mutation must expose ledger freshness degradation, got {pack_query_replay_after_codes:?}"
        ),
    )?;

    let (diff_event, diff_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "11_pack_diff_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "diff".to_owned(),
                before_pack_id.clone(),
                after_pack_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.pack.diff.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("pack diff", &diff_event, &diff_json, &probes)?;
    ensure_equal(
        &diff_json.pointer("/data/diff/summary/replayable"),
        &Some(&json!(true)),
        "pack diff replayable summary",
    )?;
    let likely_causes = json_array(&diff_json, "/data/diff/likelyCauses", "pack diff causes")?;
    ensure(
        likely_causes
            .iter()
            .any(|cause| cause.as_str() == Some("degradation_changed")),
        format!("pack diff must explain freshness degradation change, got {likely_causes:?}"),
    )?;

    let (pack_query_diff_event, pack_query_diff_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "11b_pack_query_file_diff_source_change",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "diff".to_owned(),
                pack_query_before_pack_id.clone(),
                pack_query_after_pack_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.pack.diff.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "pack query-file diff",
        &pack_query_diff_event,
        &pack_query_diff_json,
        &probes,
    )?;
    ensure_equal(
        &pack_query_diff_json.pointer("/data/diff/summary/replayable"),
        &Some(&json!(true)),
        "pack query-file diff replayable summary",
    )?;
    let pack_query_likely_causes = json_array(
        &pack_query_diff_json,
        "/data/diff/likelyCauses",
        "pack query-file diff causes",
    )?;
    ensure(
        pack_query_likely_causes
            .iter()
            .any(|cause| cause.as_str() == Some("degradation_changed")),
        format!(
            "pack query-file diff must explain freshness degradation change, got {pack_query_likely_causes:?}"
        ),
    )?;

    append_jsonl(
        &events_path,
        &json!({
            "schema": "ee.e2e.summary_event.v1",
            "scenarioId": scenario_id,
            "event": "summary",
            "commandCount": command_count,
            "workspace": workspace.display().to_string(),
            "databasePath": database_path.display().to_string(),
            "sanitizedEnvOverrides": [],
            "schemaValidationStatus": "passed",
            "goldenValidationStatus": "not_applicable",
            "redactionStatus": "passed",
            "firstFailure": null,
            "sourceMemoryId": source_memory_id,
            "redactedMemoryId": redacted_memory_id,
            "beforePackId": before_pack_id,
            "afterPackId": after_pack_id,
            "packQueryBeforePackId": pack_query_before_pack_id,
            "packQueryAfterPackId": pack_query_after_pack_id,
            "beforePackHash": before_pack_hash,
            "afterPackHash": after_pack_hash,
            "packQueryBeforePackHash": pack_query_before_hash,
            "packQueryAfterPackHash": pack_query_after_hash,
            "freshnessCodes": {
                "contextAfter": after_codes,
                "whyAfter": why_codes,
                "replayAfter": replay_after_codes,
                "packQueryAfter": pack_query_after_codes,
                "packQueryReplayAfter": pack_query_replay_after_codes
            }
        }),
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    ensure_text_omits_secret_probes(
        "dmu0 command event log",
        "jsonl",
        &events_path.display().to_string(),
        &events_text,
        &probes,
    )?;
    let event_lines = events_text.lines().collect::<Vec<_>>();
    ensure_equal(
        &event_lines.len(),
        &(command_count + 1),
        "dmu0 JSONL event count includes commands plus summary",
    )?;
    let mut saw_sanitized_env_override = false;
    for (index, line) in event_lines.iter().take(command_count).enumerate() {
        let event: JsonValue = serde_json::from_str(line)
            .map_err(|error| format!("dmu0 JSONL command event {index} must parse: {error}"))?;
        ensure_equal(
            &event.pointer("/schemaValidationStatus"),
            &Some(&json!("passed")),
            "dmu0 command schema validation status",
        )?;
        let env_overrides = json_array(&event, "/envOverrides", "dmu0 command env overrides")?;
        saw_sanitized_env_override |= env_overrides.iter().any(|override_value| {
            override_value.get("name").and_then(JsonValue::as_str) == Some("EE_E2E_SECRET_PROBE")
                && override_value
                    .get("value")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|value| value.contains("[REDACTED:"))
        });
        ensure(
            event.pointer("/stdoutArtifactPath").is_some()
                && event.pointer("/stderrArtifactPath").is_some()
                && event.pointer("/firstFailure").is_some(),
            "dmu0 command event must capture artifact paths and first-failure field",
        )?;
    }
    ensure(
        saw_sanitized_env_override,
        "dmu0 command log must include a sanitized secret-like env override",
    )?;

    Ok(())
}

#[test]
fn no_mocks_pack_quality_sentinel_scenarios_are_logged() -> TestResult {
    let scenario_id = "mccc_pack_quality_logged_no_mocks";
    let probes = egress_secret_probes();
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-mccc-pack-quality-workspace-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");
    let index_metadata_path = workspace.join(".ee").join("index").join("meta.json");
    let query_file_path = workspace.join("mccc-release-query.eeq.json");
    let secret_envs = [("EE_E2E_SECRET_PROBE", OsString::from(probes[0].raw))];

    let mut command_count = 0_usize;

    let (init_event, init_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &secret_envs,
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("mccc init", &init_event, &init_json, &probes)?;
    ensure(database_path.is_file(), "init must create real EE database")?;

    let (rule_event, rule_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_remember_release_rule_fixture_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "release,cargo,verification,mccc".to_owned(),
                "--source".to_owned(),
                "file://tests/fixtures/eval/release_failure/source_memory.json#L1".to_owned(),
                "Before release, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`; verify GitHub release assets before pushing to main."
                    .to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("mccc remember rule", &rule_event, &rule_json, &probes)?;
    let rule_memory_id = string_at(&rule_json, "/data/memory_id", "mccc rule memory")?;

    let (failure_event, failure_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_remember_release_failure_fixture_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "episodic".to_owned(),
                "--kind".to_owned(),
                "failure".to_owned(),
                "--tags".to_owned(),
                "release,clippy,workflow,mccc".to_owned(),
                "--source".to_owned(),
                "file://tests/fixtures/eval/release_failure/source_memory.json#L2".to_owned(),
                "A previous release attempt failed because clippy was skipped after a formatting-only change; the release workflow rejected `cargo clippy --all-targets -- -D warnings` with an unused import before artifacts could be published."
                    .to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "mccc remember failure",
        &failure_event,
        &failure_json,
        &probes,
    )?;
    let failure_memory_id = string_at(&failure_json, "/data/memory_id", "mccc failure memory")?;

    let (index_event, index_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_index_rebuild",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "index".to_owned(),
                "rebuild".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("mccc index rebuild", &index_event, &index_json, &probes)?;
    ensure(
        index_metadata_path.is_file(),
        "index rebuild must publish real index metadata",
    )?;

    let query = "prepare release";
    let query_file_json = serde_json::to_string_pretty(&json!({
        "version": "ee.query.v1",
        "query": {
            "text": query,
            "mode": "hybrid"
        },
        "budget": {
            "maxTokens": 4000,
            "candidatePool": 20
        },
        "output": {
            "format": "json",
            "profile": "compact",
            "explain": true
        }
    }))
    .map_err(|error| format!("failed to serialize mccc query file: {error}"))?;
    write_text(&query_file_path, &query_file_json)?;
    let query_file_arg = query_file_path.display().to_string();

    let (context_event, context_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "05_context_release_fixture_memories",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                query.to_owned(),
                "--max-tokens".to_owned(),
                "4000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("mccc context", &context_event, &context_json, &probes)?;
    let context_pack_hash = string_at(&context_json, "/data/pack/hash", "mccc context")?;
    let context_item_ids = memory_ids_from_context(&context_json)?;
    ensure(
        context_item_ids.iter().any(|id| id == &rule_memory_id)
            && context_item_ids.iter().any(|id| id == &failure_memory_id),
        format!(
            "context pack must select both fixture memories; got {context_item_ids:?}, wanted {rule_memory_id} and {failure_memory_id}"
        ),
    )?;
    let (context_ledger_hashes, context_pack_ids) = assert_context_pack_ledgers_persisted(
        &database_path,
        query,
        &context_pack_hash,
        &context_item_ids,
        1,
    )?;
    let context_pack_id = first_new_pack_id(&context_pack_ids, &[])?;

    let (pack_event, pack_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "06_pack_query_file_release_fixture_memories",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "pack".to_owned(),
                "--query-file".to_owned(),
                query_file_arg,
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes("mccc pack query-file", &pack_event, &pack_json, &probes)?;
    let pack_query_hash = string_at(&pack_json, "/data/pack/hash", "mccc pack query-file")?;
    let pack_query_item_ids = memory_ids_from_context(&pack_json)?;
    ensure(
        pack_query_item_ids.iter().any(|id| id == &rule_memory_id)
            && pack_query_item_ids
                .iter()
                .any(|id| id == &failure_memory_id),
        format!(
            "query-file pack must select both fixture memories; got {pack_query_item_ids:?}, wanted {rule_memory_id} and {failure_memory_id}"
        ),
    )?;
    let (pack_query_ledger_hashes, pack_query_pack_ids) = assert_context_pack_ledgers_persisted(
        &database_path,
        query,
        &pack_query_hash,
        &pack_query_item_ids,
        1,
    )?;
    let pack_query_pack_id =
        first_new_pack_id(&pack_query_pack_ids, std::slice::from_ref(&context_pack_id))?;

    let (passing_event, passing_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "07_pack_quality_release_failure_passing_fixture",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "eval".to_owned(),
                "run".to_owned(),
                "release_failure".to_owned(),
                "--pack-quality".to_owned(),
                "--scenario".to_owned(),
                "usr_pre_task_brief".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "mccc passing pack-quality",
        &passing_event,
        &passing_json,
        &probes,
    )?;
    ensure_equal(
        &passing_json.pointer("/success"),
        &Some(&json!(true)),
        "passing pack-quality success flag",
    )?;
    ensure_equal(
        &passing_json.pointer("/data/report/fixture_id"),
        &Some(&json!("fx.release_failure.v1")),
        "passing pack-quality fixture id",
    )?;
    ensure_equal(
        &passing_json.pointer("/data/report/aggregate_verdict"),
        &Some(&json!("within")),
        "passing pack-quality verdict",
    )?;
    let passing_comparison =
        first_pack_quality_comparison(&passing_json, "passing pack-quality report")?;
    ensure_equal(
        &passing_comparison.pointer("/case_id"),
        &Some(&json!("pq.release_failure.context.v1")),
        "passing pack-quality case id",
    )?;
    ensure(
        json_array(
            &passing_json,
            "/data/artifactPaths",
            "passing pack-quality artifacts",
        )?
        .iter()
        .any(|path| {
            path.get("stdout").and_then(JsonValue::as_str)
                == Some("target/ee-e2e/usr_pre_task_brief/<run-id>/04-context.stdout.json")
        }),
        "passing pack-quality report must include fixture stdout artifact path",
    )?;

    let (failing_event, failing_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "08_pack_quality_data_size_tiers_intentional_failure",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "eval".to_owned(),
                "run".to_owned(),
                "data_size_tiers".to_owned(),
                "--pack-quality".to_owned(),
                "--scenario".to_owned(),
                "usr_context_medium_workspace".to_owned(),
            ],
            expected_exit_code: 9,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_step_omits_secret_probes(
        "mccc failing pack-quality",
        &failing_event,
        &failing_json,
        &probes,
    )?;
    ensure_equal(
        &failing_json.pointer("/success"),
        &Some(&json!(false)),
        "failing pack-quality success flag",
    )?;
    ensure_equal(
        &failing_json.pointer("/data/report/fixture_id"),
        &Some(&json!("fx.data_size_tiers.v1")),
        "failing pack-quality fixture id",
    )?;
    ensure_equal(
        &failing_json.pointer("/data/report/aggregate_verdict"),
        &Some(&json!("regression")),
        "failing pack-quality verdict",
    )?;
    let failing_comparison =
        first_pack_quality_comparison(&failing_json, "failing pack-quality report")?;
    ensure_equal(
        &failing_comparison.pointer("/scenario_id"),
        &Some(&json!("usr_context_medium_workspace")),
        "failing pack-quality scenario id",
    )?;
    let failing_failure_reasons = json_string_vec(
        failing_comparison,
        "/failure_reasons",
        "failing pack-quality failure reasons",
    )?;
    ensure(
        !failing_failure_reasons.is_empty(),
        "intentionally failing fixture must include machine-readable failure reasons",
    )?;
    let failing_omitted_critical = json_string_vec(
        failing_comparison,
        "/omitted_critical_found",
        "failing pack-quality omitted critical ids",
    )?;
    ensure(
        !failing_omitted_critical.is_empty(),
        "intentionally failing fixture must expose rejected critical memory IDs",
    )?;

    let mut ledger_hashes = context_ledger_hashes.clone();
    ledger_hashes.extend(pack_query_ledger_hashes.clone());
    ledger_hashes.sort();
    ledger_hashes.dedup();
    let mut pack_ids = vec![context_pack_id.clone(), pack_query_pack_id.clone()];
    pack_ids.sort();
    pack_ids.dedup();
    let mut pack_hashes = vec![context_pack_hash.clone(), pack_query_hash.clone()];
    pack_hashes.sort();
    pack_hashes.dedup();
    let passing_selected_ids = json_string_vec(
        passing_comparison,
        "/actual_selected_ids",
        "passing pack-quality selected ids",
    )?;
    let failing_selected_ids = json_string_vec(
        failing_comparison,
        "/actual_selected_ids",
        "failing pack-quality selected ids",
    )?;
    let failing_unexpected_ids = json_string_vec(
        failing_comparison,
        "/unexpected_ids",
        "failing pack-quality unexpected ids",
    )?;
    let rejected_memory_ids = failing_omitted_critical
        .iter()
        .chain(failing_unexpected_ids.iter())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut degradation_codes =
        pack_quality_degraded_codes(&passing_json, "passing pack-quality degraded branches")?;
    degradation_codes.extend(pack_quality_degraded_codes(
        &failing_json,
        "failing pack-quality degraded branches",
    )?);
    degradation_codes.sort();
    degradation_codes.dedup();

    append_jsonl(
        &events_path,
        &json!({
            "schema": "ee.e2e.pack_quality_summary.v1",
            "scenarioId": scenario_id,
            "event": "summary",
            "commandCount": command_count,
            "workspace": workspace.display().to_string(),
            "databasePath": database_path.display().to_string(),
            "indexMetadataPath": index_metadata_path.display().to_string(),
            "sanitizedEnvOverrides": ["EE_E2E_SECRET_PROBE"],
            "fixtureIds": [
                "fx.release_failure.v1",
                "fx.data_size_tiers.v1"
            ],
            "scenarioIds": [
                "usr_pre_task_brief",
                "usr_context_medium_workspace"
            ],
            "loadedFixtureMemoryIds": [
                rule_memory_id,
                failure_memory_id
            ],
            "selectedMemoryIds": {
                "workspaceContext": context_item_ids,
                "workspacePack": pack_query_item_ids,
                "packQualityPassing": passing_selected_ids,
                "packQualityFailing": failing_selected_ids
            },
            "rejectedMemoryIds": rejected_memory_ids,
            "packIds": pack_ids,
            "packHashes": pack_hashes,
            "ledgerHashes": ledger_hashes,
            "schemaValidationStatus": "passed",
            "goldenValidationStatus": "not_applicable",
            "redactionStatus": "passed",
            "degradationCodes": degradation_codes,
            "firstFailure": null,
            "firstFailureDiagnosis": failing_failure_reasons.first(),
            "packQuality": {
                "passing": {
                    "fixtureId": "fx.release_failure.v1",
                    "scenarioId": "usr_pre_task_brief",
                    "verdict": "within",
                    "stdoutArtifactPath": passing_event.stdout_artifact_path,
                    "stderrArtifactPath": passing_event.stderr_artifact_path
                },
                "failing": {
                    "fixtureId": "fx.data_size_tiers.v1",
                    "scenarioId": "usr_context_medium_workspace",
                    "verdict": "regression",
                    "exitCode": failing_event.exit_code,
                    "stdoutArtifactPath": failing_event.stdout_artifact_path,
                    "stderrArtifactPath": failing_event.stderr_artifact_path,
                    "failureReasons": failing_failure_reasons
                }
            }
        }),
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read mccc JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    ensure_text_omits_secret_probes(
        "mccc command event log",
        "jsonl",
        &events_path.display().to_string(),
        &events_text,
        &probes,
    )?;
    let event_lines = events_text.lines().collect::<Vec<_>>();
    ensure_equal(
        &event_lines.len(),
        &(command_count + 1),
        "mccc JSONL event count includes commands plus summary",
    )?;
    let mut saw_sanitized_env_override = false;
    for (index, line) in event_lines.iter().take(command_count).enumerate() {
        let event: JsonValue = serde_json::from_str(line)
            .map_err(|error| format!("mccc JSONL command event {index} must parse: {error}"))?;
        ensure_equal(
            &event.pointer("/schema"),
            &Some(&json!("ee.e2e.command_event.v1")),
            "mccc command event schema",
        )?;
        ensure_equal(
            &event.pointer("/schemaValidationStatus"),
            &Some(&json!("passed")),
            "mccc command schema validation status",
        )?;
        ensure(
            event.pointer("/command").is_some()
                && event.pointer("/args").is_some()
                && event.pointer("/cwd").is_some()
                && event.pointer("/workspace").is_some()
                && event.pointer("/elapsedMs").is_some()
                && event.pointer("/exitCode").is_some()
                && event.pointer("/stdoutArtifactPath").is_some()
                && event.pointer("/stderrArtifactPath").is_some()
                && event.pointer("/goldenValidationStatus").is_some()
                && event.pointer("/redactionStatus").is_some()
                && event.pointer("/firstFailure").is_some(),
            "mccc command event must capture command, cwd/workspace, timing, exit, artifact, validation, redaction, and first-failure fields",
        )?;
        saw_sanitized_env_override |=
            json_array(&event, "/envOverrides", "mccc command env overrides")?
                .iter()
                .any(|override_value| {
                    override_value.get("name").and_then(JsonValue::as_str)
                        == Some("EE_E2E_SECRET_PROBE")
                        && override_value
                            .get("value")
                            .and_then(JsonValue::as_str)
                            .is_some_and(|value| value.contains("[REDACTED:"))
                });
    }
    ensure(
        saw_sanitized_env_override,
        "mccc command log must include a sanitized secret-like env override",
    )?;
    let summary: JsonValue = serde_json::from_str(
        event_lines
            .last()
            .ok_or_else(|| "mccc JSONL log missing summary event".to_owned())?,
    )
    .map_err(|error| format!("mccc summary event must parse: {error}"))?;
    ensure_equal(
        &summary.pointer("/schema"),
        &Some(&json!("ee.e2e.pack_quality_summary.v1")),
        "mccc summary schema",
    )?;
    ensure(
        !json_array(&summary, "/packIds", "mccc summary pack ids")?.is_empty()
            && !json_array(&summary, "/packHashes", "mccc summary pack hashes")?.is_empty()
            && !json_array(&summary, "/ledgerHashes", "mccc summary ledger hashes")?.is_empty(),
        "mccc summary must include pack IDs, pack hashes, and ledger hashes",
    )?;
    ensure(
        summary.pointer("/firstFailureDiagnosis").is_some(),
        "mccc summary must include stable first-failure diagnosis",
    )?;

    Ok(())
}

#[test]
fn no_mocks_init_remember_search_context_why_with_jsonl_command_events() -> TestResult {
    let scenario_id = "phase3_no_mocks_memory_loop";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-no-mocks-workspace-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");
    let index_metadata_path = workspace.join(".ee").join("index").join("meta.json");

    let mut command_count = 0_usize;

    let (_init_event, _init_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure(
        database_path.is_file(),
        "init must create real FrankenSQLite database",
    )?;

    let (_rule_event, rule_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_remember_rule",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "release,verification".to_owned(),
                "--source".to_owned(),
                "file://tests/no_mocks_e2e.rs#L1".to_owned(),
                "Run cargo fmt --check, cargo clippy --all-targets -- -D warnings, and cargo test before release.".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let rule_memory_id = string_at(&rule_json, "/data/memory_id", "remember rule")?;

    let (_failure_event, failure_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_remember_failure",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "episodic".to_owned(),
                "--kind".to_owned(),
                "failure".to_owned(),
                "--tags".to_owned(),
                "release,clippy,workflow".to_owned(),
                "--source".to_owned(),
                "file://tests/no_mocks_e2e.rs#L2".to_owned(),
                "Release validation failed because clippy warnings were skipped before tagging."
                    .to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let failure_memory_id = string_at(&failure_json, "/data/memory_id", "remember failure")?;

    let (_index_event, _index_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_index_rebuild",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "index".to_owned(),
                "rebuild".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure(
        index_metadata_path.is_file(),
        "index rebuild must publish real Frankensearch metadata",
    )?;

    let (_search_event, search_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "05_search",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "search".to_owned(),
                "release clippy".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let search_results = search_json
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "search results missing".to_owned())?;
    ensure(
        !search_results.is_empty(),
        "search must return real indexed results",
    )?;
    let search_ids = search_results
        .iter()
        .filter_map(|result| result.get("docId").and_then(JsonValue::as_str))
        .collect::<Vec<_>>();
    ensure(
        search_ids.contains(&rule_memory_id.as_str())
            && search_ids.contains(&failure_memory_id.as_str()),
        format!(
            "search must surface both remembered records; got {search_ids:?}, wanted {rule_memory_id} and {failure_memory_id}"
        ),
    )?;

    let context_query = "prepare a release after clippy failure";
    let (_context_first_event, context_first_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "06_context_first",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                context_query.to_owned(),
                "--max-tokens".to_owned(),
                "4000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let first_pack_hash = string_at(&context_first_json, "/data/pack/hash", "context first")?;
    let first_item_ids = memory_ids_from_context(&context_first_json)?;
    ensure(
        first_pack_hash.starts_with("blake3:"),
        format!("pack hash must use blake3 prefix, got {first_pack_hash}"),
    )?;
    ensure(
        !first_item_ids.is_empty(),
        "context pack must select at least one memory",
    )?;

    let (_context_second_event, context_second_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "07_context_second",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                context_query.to_owned(),
                "--max-tokens".to_owned(),
                "4000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let second_pack_hash = string_at(&context_second_json, "/data/pack/hash", "context second")?;
    let second_item_ids = memory_ids_from_context(&context_second_json)?;
    ensure_equal(
        &second_pack_hash,
        &first_pack_hash,
        "pack hash deterministic across repeated context calls",
    )?;
    ensure_equal(
        &second_item_ids,
        &first_item_ids,
        "context item order deterministic across repeated context calls",
    )?;
    let (pack_ledger_hashes, pack_ledger_pack_ids) = assert_context_pack_ledgers_persisted(
        &database_path,
        context_query,
        &first_pack_hash,
        &first_item_ids,
        2,
    )?;

    let (_why_event, why_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "08_why",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "why".to_owned(),
                rule_memory_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_equal(
        &why_json.pointer("/data/found"),
        &Some(&JsonValue::Bool(true)),
        "why should find the remembered rule",
    )?;
    ensure_equal(
        &why_json.pointer("/data/memoryId"),
        &Some(&JsonValue::String(rule_memory_id.clone())),
        "why should echo requested memory id",
    )?;
    ensure(
        why_json.pointer("/data/storage/origin").is_some(),
        "why must explain storage origin",
    )?;
    ensure(
        why_json.pointer("/data/retrieval/confidence").is_some(),
        "why must explain retrieval confidence",
    )?;

    append_jsonl(
        &events_path,
        &SummaryEvent {
            schema: "ee.e2e.summary_event.v1",
            scenario_id,
            event: "summary",
            command_count,
            workspace: workspace.display().to_string(),
            database_path: database_path.display().to_string(),
            index_metadata_path: index_metadata_path.display().to_string(),
            rule_memory_id,
            failure_memory_id,
            pack_hash: first_pack_hash,
            pack_ledger_hashes,
            pack_ledger_pack_ids,
            context_item_ids: first_item_ids,
        },
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    let event_lines = events_text.lines().collect::<Vec<_>>();
    ensure_equal(
        &event_lines.len(),
        &(command_count + 1),
        "JSONL event count includes commands plus summary",
    )?;
    for (index, line) in event_lines.iter().take(command_count).enumerate() {
        let event: JsonValue = serde_json::from_str(line)
            .map_err(|error| format!("JSONL command event {index} must parse: {error}"))?;
        ensure(
            event.get("stdout").is_some()
                && event.get("stderr").is_some()
                && event.get("exitCode").is_some(),
            format!("JSONL command event {index} must capture stdout/stderr/exitCode"),
        )?;
    }

    Ok(())
}
