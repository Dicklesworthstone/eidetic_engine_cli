//! No-mock drift-sentinel replay coverage.
//!
//! This test exercises the real `ee` binary against an isolated workspace and
//! verifies that source-backed provenance drift is surfaced through the
//! read-only drift report plus search result hints without mutating memory rows.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::db::{CreatePackItemInput, CreatePackRecordInput, DbConnection, StoredMemory};
use ee::models::DomainError;
use ee::output::error_response_json;
use ee::policy::redact_secret_like_content;
use serde::Serialize;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const BEAD_ID: &str = "bd-1z1fd.5";
const SCENARIO_ID: &str = "drift_sentinel_no_mock_replay";
const QUERY: &str = "drift sentinel evidence replay";
const BASELINE_GOLDEN: &str =
    include_str!("fixtures/golden/memory_drift_no_mock_baseline.json.golden");
const CHANGED_GOLDEN: &str =
    include_str!("fixtures/golden/memory_drift_no_mock_changed.json.golden");

#[derive(Debug)]
struct CommandOutput {
    json: Value,
    stdout: String,
    stderr: String,
    elapsed_ms: u128,
    exit_code: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestEvent {
    schema: &'static str,
    bead_id: &'static str,
    scenario: &'static str,
    phase: &'static str,
    status: &'static str,
    elapsed_ms: u128,
    scrubbed_workspace_path_hash: String,
    response_hashes: BTreeMap<String, String>,
    artifact_hashes: BTreeMap<String, String>,
    degraded_codes: Vec<String>,
    schema_validation_status: String,
    mutation_guard: Option<String>,
    details: Value,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: T, expected: T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn unique_log_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-memory-drift-no-mock-logs")
        .join(format!("{SCENARIO_ID}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create log dir {}: {error}", dir.display()))?;
    Ok(dir)
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

fn hash_text(text: &str) -> String {
    format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex())
}

fn hash_json_serializable<T>(name: &str, value: &T) -> Result<String, String>
where
    T: Serialize + ?Sized,
{
    let serialized = serde_json::to_string(value)
        .map_err(|error| format!("{name} serialization failed before hashing: {error}"))?;
    Ok(hash_text(&serialized))
}

fn hash_json_value(name: &str, value: &Value) -> Result<String, String> {
    hash_json_serializable(name, value)
}

fn memory_drift_json_artifact_error(
    code: &'static str,
    name: &str,
    stage: &'static str,
    message: String,
) -> DomainError {
    DomainError::UsageCodeWithDetails {
        code,
        message,
        repair: Some("Regenerate the memory drift JSON artifact before hashing it.".to_owned()),
        details_json: json!({
            "artifact": name,
            "stage": stage,
        })
        .to_string(),
    }
}

fn hash_json_artifact_reader(name: &str, mut reader: impl Read) -> Result<String, DomainError> {
    let mut text = String::new();
    reader.read_to_string(&mut text).map_err(|error| {
        memory_drift_json_artifact_error(
            "memory_drift_json_read_failed",
            name,
            "before_hashing",
            format!("{name} JSON read failed before hashing: {error}"),
        )
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|error| {
        memory_drift_json_artifact_error(
            "memory_drift_malformed_json",
            name,
            "before_hashing",
            format!("{name} JSON parse failed before hashing: {error}"),
        )
    })?;
    hash_json_value(name, &value).map_err(|error| {
        memory_drift_json_artifact_error("memory_drift_json_hash_failed", name, "hashing", error)
    })
}

fn hash_json_artifact_serializable<T>(name: &str, value: &T) -> Result<String, DomainError>
where
    T: Serialize + ?Sized,
{
    hash_json_serializable(name, value).map_err(|error| {
        memory_drift_json_artifact_error("memory_drift_json_hash_failed", name, "hashing", error)
    })
}

struct SerializationFailure;

impl Serialize for SerializationFailure {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(serde::ser::Error::custom(
            "forced memory-drift hash serialization failure",
        ))
    }
}

fn hash_file(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn scrubbed_workspace_hash(workspace: &Path) -> String {
    hash_text(&workspace.display().to_string())
}

fn run_ee(
    workspace: &Path,
    artifact_dir: &Path,
    step: &str,
    args: &[&str],
) -> Result<CommandOutput, String> {
    let started = Instant::now();
    let embed_model_dir = artifact_dir.join("empty-embed-model");
    fs::create_dir_all(&embed_model_dir).map_err(|error| {
        format!(
            "failed to create isolated empty embedding model dir {}: {error}",
            embed_model_dir.display()
        )
    })?;
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_EMBED_MODEL_DIR", &embed_model_dir)
        .env_remove("EE_EMBED_MODEL_PATH")
        .env_remove("EE_EMBED_DEDUP_ENABLED")
        .env_remove("EE_EMBED_DEDUP_HAMMING_K")
        .env_remove("EE_EMBED_DEDUP_COSINE_FLOOR")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("failed to run ee {step}: {error}"))?;
    let elapsed_ms = started.elapsed().as_millis();
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("ee {step} stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("ee {step} stderr was not UTF-8: {error}"))?;
    let stdout_path = artifact_dir.join(format!("{step}.stdout.json"));
    let stderr_path = artifact_dir.join(format!("{step}.stderr.txt"));
    write_text(&stdout_path, &stdout)?;
    write_text(&stderr_path, &stderr)?;

    let exit_code = output.status.code().unwrap_or(-1);
    ensure(
        exit_code == 0,
        format!("ee {step} exited {exit_code}; stderr={stderr}"),
    )?;
    ensure(
        stderr.trim().is_empty(),
        format!("ee {step} wrote unexpected stderr: {stderr}"),
    )?;
    let json = serde_json::from_str(&stdout)
        .map_err(|error| format!("ee {step} stdout must be JSON: {error}; stdout={stdout}"))?;
    Ok(CommandOutput {
        json,
        stdout,
        stderr,
        elapsed_ms,
        exit_code,
    })
}

#[test]
fn memory_drift_hashing_emits_error_envelope_for_malformed_json_artifact() -> TestResult {
    let malformed = br#"{"schema":"ee.memory_drift.report.v1","data":{"items":[}"#;
    let error = match hash_json_artifact_reader("changedReport", &malformed[..]) {
        Ok(_) => return Err("malformed JSON unexpectedly hashed successfully".to_string()),
        Err(error) => error,
    };
    let envelope_text = error_response_json(&error);
    let envelope: Value = serde_json::from_str(&envelope_text).map_err(|error| {
        format!("malformed JSON error envelope must be JSON: {error}; envelope={envelope_text}")
    })?;
    ensure_equal(
        envelope.pointer("/schema").and_then(Value::as_str),
        Some("ee.error.v2"),
        "malformed JSON error envelope schema",
    )?;
    ensure_equal(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("memory_drift_malformed_json"),
        "malformed JSON error code",
    )?;
    ensure_equal(
        envelope
            .pointer("/error/details/artifact")
            .and_then(Value::as_str),
        Some("changedReport"),
        "malformed JSON artifact detail",
    )?;
    ensure_equal(
        envelope
            .pointer("/error/details/stage")
            .and_then(Value::as_str),
        Some("before_hashing"),
        "malformed JSON stage detail",
    )?;
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("malformed JSON envelope missing message: {envelope}"))?;
    ensure(
        message.contains("changedReport JSON parse failed before hashing"),
        format!("malformed JSON error should name the fail-loud path, got: {message}"),
    )?;
    ensure(
        message.contains("expected") || message.contains("line"),
        format!("malformed JSON error should include parser context, got: {message}"),
    )?;
    ensure(
        !message.contains("blake3:"),
        format!("malformed JSON must fail before producing a hash, got: {message}"),
    )
}

#[test]
fn memory_drift_hashing_emits_error_envelope_for_json_hash_serialization_failure() -> TestResult {
    let error = match hash_json_artifact_serializable("changedReport", &SerializationFailure) {
        Ok(_) => return Err("serialization failure unexpectedly hashed successfully".to_string()),
        Err(error) => error,
    };
    let envelope_text = error_response_json(&error);
    let envelope: Value = serde_json::from_str(&envelope_text).map_err(|error| {
        format!(
            "serialization failure error envelope must be JSON: {error}; envelope={envelope_text}"
        )
    })?;
    ensure_equal(
        envelope.pointer("/schema").and_then(Value::as_str),
        Some("ee.error.v2"),
        "serialization failure error envelope schema",
    )?;
    ensure_equal(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("memory_drift_json_hash_failed"),
        "serialization failure error code",
    )?;
    ensure_equal(
        envelope
            .pointer("/error/details/artifact")
            .and_then(Value::as_str),
        Some("changedReport"),
        "serialization failure artifact detail",
    )?;
    ensure_equal(
        envelope
            .pointer("/error/details/stage")
            .and_then(Value::as_str),
        Some("hashing"),
        "serialization failure stage detail",
    )?;
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("serialization failure envelope missing message: {envelope}"))?;
    ensure(
        message.contains("changedReport serialization failed before hashing"),
        format!("serialization failure error should name the hash path, got: {message}"),
    )?;
    ensure(
        message.contains("forced memory-drift hash serialization failure"),
        format!("serialization failure error should preserve the serializer error, got: {message}"),
    )?;
    ensure(
        !message.contains("blake3:"),
        format!("serialization failure must fail before producing a hash, got: {message}"),
    )
}

fn string_at(value: &Value, pointer: &str, context: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context}: missing string at {pointer}; value={value}"))
}

fn degraded_codes(value: &Value) -> Vec<String> {
    let mut codes = value
        .pointer("/degraded")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            value
                .pointer("/data/degraded")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .filter_map(|entry| entry.get("code").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    codes.sort();
    codes.dedup();
    codes
}

fn memory_drift_codes(value: &Value) -> Vec<String> {
    let mut codes = degraded_codes(value);
    codes.extend(
        value
            .pointer("/data/items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("degradedCode").and_then(Value::as_str))
            .map(str::to_owned),
    );
    codes.sort();
    codes.dedup();
    codes
}

fn validate_docs_schema(schema_name: &str, title: &str) -> TestResult {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join(schema_name);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read schema {}: {error}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("failed to parse schema {}: {error}", path.display()))?;
    ensure_equal(
        value.get("title").and_then(Value::as_str),
        Some(title),
        &format!("{schema_name} title"),
    )
}

fn validate_memory_drift_response(value: &Value) -> TestResult {
    validate_docs_schema(
        "ee.memory_drift.report.v1.json",
        "ee.memory_drift.report.v1",
    )?;
    ensure_equal(
        value.get("schema").and_then(Value::as_str),
        Some("ee.response.v2"),
        "memory drift envelope schema",
    )?;
    ensure_equal(
        value.pointer("/data/schema").and_then(Value::as_str),
        Some("ee.memory_drift.report.v1"),
        "memory drift data schema",
    )?;
    ensure(
        value.pointer("/data/summary/totalMemories").is_some(),
        "memory drift report must include summary",
    )?;
    ensure(
        value
            .pointer("/data/items")
            .and_then(Value::as_array)
            .is_some(),
        "memory drift report must include items",
    )
}

fn validate_search_response(value: &Value) -> TestResult {
    validate_docs_schema("ee.search.v1.json", "ee.search.v1")?;
    ensure_equal(
        value.get("schema").and_then(Value::as_str),
        Some("ee.response.v2"),
        "search envelope schema",
    )?;
    ensure_equal(
        value.pointer("/data/command").and_then(Value::as_str),
        Some("search"),
        "search data command",
    )?;
    ensure(
        value
            .pointer("/data/results")
            .and_then(Value::as_array)
            .is_some(),
        "search response must include results array",
    )
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn update_provenance_status(
    database_path: &Path,
    memory_id: &str,
    status: &str,
    note: &str,
) -> TestResult {
    let connection = DbConnection::open_file(database_path).map_err(|error| {
        format!(
            "failed to open database {}: {error}",
            database_path.display()
        )
    })?;
    connection
        .execute_raw(&format!(
            "UPDATE memories SET provenance_verification_status = {}, provenance_verified_at = '2026-05-20T00:00:00Z', provenance_verification_note = {} WHERE id = {}",
            sql_string(status),
            sql_string(note),
            sql_string(memory_id),
        ))
        .map_err(|error| format!("failed to update provenance status for {memory_id}: {error}"))
}

fn memory_rows(database_path: &Path, ids: &[&str]) -> Result<Vec<StoredMemory>, String> {
    let connection = DbConnection::open_file(database_path).map_err(|error| {
        format!(
            "failed to open database {}: {error}",
            database_path.display()
        )
    })?;
    ids.iter()
        .map(|id| {
            connection
                .get_memory(id)
                .map_err(|error| format!("failed to read memory {id}: {error}"))?
                .ok_or_else(|| format!("memory {id} missing"))
        })
        .collect()
}

fn normalize_id(memory_id: &str, changed_id: &str, stable_id: &str) -> String {
    if memory_id == changed_id {
        "mem_changed".to_owned()
    } else if memory_id == stable_id {
        "mem_stable".to_owned()
    } else {
        format!(
            "mem_other_{}",
            &hash_text(memory_id)["blake3:".len()..][..12]
        )
    }
}

fn normalize_command(command: &str, changed_id: &str, stable_id: &str) -> String {
    command
        .replace(changed_id, "mem_changed")
        .replace(stable_id, "mem_stable")
}

fn normalize_drift_report(
    value: &Value,
    changed_id: &str,
    stable_id: &str,
) -> Result<Value, String> {
    validate_memory_drift_response(value)?;
    let data = value
        .get("data")
        .ok_or_else(|| "memory drift response missing data".to_owned())?;
    let mut items = data
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "memory drift response missing items".to_owned())?
        .iter()
        .map(|item| {
            let memory_id = item
                .get("memoryId")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("memory drift item missing memoryId: {item}"))?;
            Ok(json!({
                "memoryId": normalize_id(memory_id, changed_id, stable_id),
                "driftStatus": item.get("driftStatus").cloned().unwrap_or(Value::Null),
                "topReason": item.get("topReason").cloned().unwrap_or(Value::Null),
                "evidenceCount": item.get("evidenceCount").cloned().unwrap_or(Value::Null),
                "degradedCode": item.get("degradedCode").cloned().unwrap_or(Value::Null),
                "severity": item.get("severity").cloned().unwrap_or(Value::Null),
                "revalidationCommand": item
                    .get("revalidationCommand")
                    .and_then(Value::as_str)
                    .map(|command| normalize_command(command, changed_id, stable_id))
                    .unwrap_or_else(|| "<missing>".to_owned()),
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    items.sort_by(|left, right| {
        left.get("memoryId")
            .and_then(Value::as_str)
            .cmp(&right.get("memoryId").and_then(Value::as_str))
    });
    let action_kinds = data
        .get("recoveryActions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| action.get("kind").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(json!({
        "schema": value.get("schema").cloned().unwrap_or(Value::Null),
        "success": value.get("success").cloned().unwrap_or(Value::Null),
        "data": {
            "schema": data.get("schema").cloned().unwrap_or(Value::Null),
            "mode": data.get("mode").cloned().unwrap_or(Value::Null),
            "summary": data.get("summary").cloned().unwrap_or(Value::Null),
            "items": items,
            "recoveryActionKinds": action_kinds,
            "degraded": data.get("degraded").cloned().unwrap_or(Value::Null),
        },
        "degradedCodes": memory_drift_codes(value),
    }))
}

fn normalize_search(value: &Value, changed_id: &str, stable_id: &str) -> Result<Value, String> {
    validate_search_response(value)?;
    let data = value
        .get("data")
        .ok_or_else(|| "search response missing data".to_owned())?;
    let mut results = data
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "search response missing results".to_owned())?
        .iter()
        .map(|result| {
            let doc_id = result
                .get("docId")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("search result missing docId: {result}"))?;
            let drift_hint = result.get("driftHint").map(|hint| {
                let mut scrubbed = hint.clone();
                let normalized_command = scrubbed
                    .get("revalidationCommand")
                    .and_then(Value::as_str)
                    .map(|command| normalize_command(command, changed_id, stable_id));
                if let (Some(command), Some(map)) = (normalized_command, scrubbed.as_object_mut()) {
                    map.insert("revalidationCommand".to_owned(), Value::String(command));
                }
                scrubbed
            });
            Ok(json!({
                "docId": normalize_id(doc_id, changed_id, stable_id),
                "source": result.get("source").cloned().unwrap_or(Value::Null),
                "hasDriftHint": drift_hint.is_some(),
                "driftHint": drift_hint.unwrap_or(Value::Null),
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    results.sort_by(|left, right| {
        left.get("docId")
            .and_then(Value::as_str)
            .cmp(&right.get("docId").and_then(Value::as_str))
    });
    Ok(json!({
        "schema": value.get("schema").cloned().unwrap_or(Value::Null),
        "success": value.get("success").cloned().unwrap_or(Value::Null),
        "command": data.get("command").cloned().unwrap_or(Value::Null),
        "status": data.get("status").cloned().unwrap_or(Value::Null),
        "query": data.get("query").cloned().unwrap_or(Value::Null),
        "resultCount": data.get("resultCount").cloned().unwrap_or(Value::Null),
        "results": results,
        "degradedCodes": degraded_codes(value),
    }))
}

fn compare_golden(actual: &Value, expected_text: &str, name: &str) -> TestResult {
    let expected: Value = serde_json::from_str(expected_text)
        .map_err(|error| format!("failed to parse {name} golden: {error}"))?;
    ensure_equal(actual, &expected, name)
}

fn assert_no_raw_workspace_path(value: &Value, workspace: &Path, context: &str) -> TestResult {
    let encoded = serde_json::to_string(value)
        .map_err(|error| format!("failed to encode {context} for redaction check: {error}"))?;
    let raw_path = workspace.display().to_string();
    ensure(
        !encoded.contains(&raw_path),
        format!("{context} leaked raw workspace path {raw_path}"),
    )
}

fn emit_event(
    log_path: &Path,
    workspace: &Path,
    phase: &'static str,
    elapsed_ms: u128,
    response_hashes: BTreeMap<String, String>,
    artifact_hashes: BTreeMap<String, String>,
    degraded_codes: Vec<String>,
    schema_validation_status: impl Into<String>,
    mutation_guard: Option<String>,
    details: Value,
) -> TestResult {
    append_jsonl(
        log_path,
        &TestEvent {
            schema: "ee.test_event.v1",
            bead_id: BEAD_ID,
            scenario: SCENARIO_ID,
            phase,
            status: "pass",
            elapsed_ms,
            scrubbed_workspace_path_hash: scrubbed_workspace_hash(workspace),
            response_hashes,
            artifact_hashes,
            degraded_codes,
            schema_validation_status: schema_validation_status.into(),
            mutation_guard,
            details,
        },
    )
}

fn map_of(entries: &[(&str, String)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn assert_required_events(log_path: &Path) -> TestResult {
    let text = fs::read_to_string(log_path).map_err(|error| {
        format!(
            "failed to read test event log {}: {error}",
            log_path.display()
        )
    })?;
    let events = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("event log JSONL must parse: {error}"))?;
    let phases = events
        .iter()
        .filter_map(|event| event.get("phase").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let expected = [
        "setup",
        "baseline_snapshot",
        "source_change",
        "drift_report",
        "pack_or_search_probe",
        "assertion",
        "cleanup",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    ensure_equal(&phases, &expected, "logged ee.test_event.v1 phases")?;
    for event in events {
        ensure_equal(
            event.get("schema").and_then(Value::as_str),
            Some("ee.test_event.v1"),
            "test event schema",
        )?;
        ensure(
            event.get("scrubbedWorkspacePathHash").is_some(),
            format!("event missing workspace path hash: {event}"),
        )?;
        ensure(
            event.get("responseHashes").is_some(),
            format!("event missing responseHashes: {event}"),
        )?;
        ensure(
            event.get("artifactHashes").is_some(),
            format!("event missing artifactHashes: {event}"),
        )?;
        ensure(
            event.get("degradedCodes").is_some(),
            format!("event missing degradedCodes: {event}"),
        )?;
        ensure(
            event.get("elapsedMs").and_then(Value::as_u64).is_some(),
            format!("event missing elapsedMs: {event}"),
        )?;
    }
    Ok(())
}

#[test]
fn recent_pack_old_only_archived_source_drift_does_not_block_current_claim_window() -> TestResult {
    let log_dir = unique_log_dir()?;
    let artifact_dir = log_dir.join("old-only-artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("create old-only artifact dir: {error}"))?;
    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-memory-drift-old-only-no-mock-")
        .tempdir()
        .map_err(|error| format!("create old-only workspace: {error}"))?;
    let workspace = workspace_temp.path();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");

    let init = run_ee(
        workspace,
        &artifact_dir,
        "old_only_01_init",
        &["--workspace", &workspace_arg, "--json", "init"],
    )?;
    ensure_equal(init.exit_code, 0, "old-only init exit")?;
    let remembered = run_ee(
        workspace,
        &artifact_dir,
        "old_only_02_remember",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "Archived close-the-gap plan evidence moved to docs/archive.",
        ],
    )?;
    let memory_id = string_at(&remembered.json, "/data/memory_id", "old-only remember")?;
    update_provenance_status(
        &database_path,
        &memory_id,
        "missing",
        "historical CLOSE_THE_GAP_PLAN.md provenance moved to docs/archive/close_the_gap_2026-05.md",
    )?;

    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open old-only db: {error}"))?;
    let stored = connection
        .get_memory(&memory_id)
        .map_err(|error| format!("read old-only memory: {error}"))?
        .ok_or_else(|| "old-only memory disappeared".to_owned())?;
    let pack_id_hash = blake3::hash(b"old archived plan pack").to_hex().to_string();
    let pack_id = format!("pack_{}", &pack_id_hash[..26]);
    connection
        .insert_pack_record_at(
            &pack_id,
            &CreatePackRecordInput {
                workspace_id: stored.workspace_id,
                query: "coordination snapshot path smoke".to_owned(),
                profile: "balanced".to_owned(),
                max_tokens: 512,
                used_tokens: 32,
                item_count: 1,
                omitted_count: 0,
                pack_hash: format!("blake3:{pack_id_hash}"),
                degraded_json: None,
                created_by: Some("no-mock-regression".to_owned()),
            },
            &[CreatePackItemInput {
                pack_id: pack_id.clone(),
                memory_id: memory_id.clone(),
                rank: 1,
                section: "procedural_rules".to_owned(),
                estimated_tokens: 32,
                relevance: 0.03,
                utility: 0.5,
                combined_score: None,
                attempt_family_multiplicity: None,
                why: "historical archived-plan selection".to_owned(),
                diversity_key: None,
                provenance_json: "{}".to_owned(),
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
            }],
            &[],
            "2000-01-01T00:00:00Z",
        )
        .map_err(|error| format!("insert old-only pack: {error}"))?;
    connection
        .close()
        .map_err(|error| format!("close old-only db: {error}"))?;

    let report = run_ee(
        workspace,
        &artifact_dir,
        "old_only_03_recent_pack_drift",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "memory",
            "drift",
            "--mode",
            "recent-pack-items",
        ],
    )?;
    ensure_equal(report.exit_code, 0, "old-only drift report exit")?;
    ensure_equal(
        report
            .json
            .pointer("/data/summary/totalMemories")
            .and_then(Value::as_u64),
        Some(0),
        "strictly historical pack selection count",
    )?;
    ensure_equal(
        report
            .json
            .pointer("/data/items")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "strictly historical pack selection items",
    )?;
    ensure(
        !memory_drift_codes(&report.json)
            .iter()
            .any(|code| code == "memory_drift_source_unverifiable"),
        format!(
            "old archived pack must not retain claim-authority degradation: {}",
            report.stdout
        ),
    )?;
    Ok(())
}

#[test]
fn memory_drift_no_mock_replay_surfaces_changed_source_without_mutation() -> TestResult {
    let log_dir = unique_log_dir()?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir).map_err(|error| {
        format!(
            "failed to create artifact dir {}: {error}",
            artifact_dir.display()
        )
    })?;
    let events_path = log_dir.join("events.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-memory-drift-no-mock-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");
    let changed_source = workspace.join("src").join("drift_replay.rs");
    let stable_source = workspace.join("docs").join("drift_policy.md");

    let changed_before =
        "pub fn drift_replay_contract() -> &'static str {\n    \"baseline-source\"\n}\n";
    let changed_after =
        "pub fn drift_replay_contract() -> &'static str {\n    \"changed-source\"\n}\n";
    let stable_text = "# Drift sentinel policy\n\nStable source evidence remains current.\n";
    write_text(&changed_source, changed_before)?;
    write_text(&stable_source, stable_text)?;

    let init = run_ee(
        &workspace,
        &artifact_dir,
        "01_init",
        &["--workspace", &workspace_arg, "--json", "init"],
    )?;
    ensure_equal(
        init.json.get("schema").and_then(Value::as_str),
        Some("ee.response.v2"),
        "init schema",
    )?;
    emit_event(
        &events_path,
        &workspace,
        "setup",
        init.elapsed_ms,
        map_of(&[("init", hash_text(&init.stdout))]),
        map_of(&[
            ("changedSource", hash_file(&changed_source)?),
            ("stableSource", hash_file(&stable_source)?),
        ]),
        degraded_codes(&init.json),
        "passed",
        None,
        json!({
            "databasePathHash": hash_text(&database_path.display().to_string()),
            "initExitCode": init.exit_code,
            "stderrHash": hash_text(&init.stderr),
        }),
    )?;

    let changed_source_arg = format!("file://{}", changed_source.display());
    let stable_source_arg = format!("file://{}", stable_source.display());
    let changed_memory = run_ee(
        &workspace,
        &artifact_dir,
        "02_remember_changed",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "drift,sentinel,replay",
            "--source",
            &changed_source_arg,
            "Drift sentinel replay memory depends on changed source evidence for replay validation.",
        ],
    )?;
    let changed_id = string_at(&changed_memory.json, "/data/memory_id", "changed remember")?;
    let stable_memory = run_ee(
        &workspace,
        &artifact_dir,
        "03_remember_stable",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "drift,sentinel,replay",
            "--source",
            &stable_source_arg,
            "Drift sentinel stable memory depends on unchanged source evidence for replay validation.",
        ],
    )?;
    let stable_id = string_at(&stable_memory.json, "/data/memory_id", "stable remember")?;

    let changed_before_hash = hash_file(&changed_source)?;
    let stable_hash = hash_file(&stable_source)?;
    update_provenance_status(
        &database_path,
        &changed_id,
        "verified",
        &format!("no-mock baseline source hash {changed_before_hash}"),
    )?;
    update_provenance_status(
        &database_path,
        &stable_id,
        "verified",
        &format!("no-mock baseline source hash {stable_hash}"),
    )?;

    let index = run_ee(
        &workspace,
        &artifact_dir,
        "04_index_rebuild",
        &["--workspace", &workspace_arg, "--json", "index", "rebuild"],
    )?;
    ensure_equal(
        index.json.get("schema").and_then(Value::as_str),
        Some("ee.response.v2"),
        "index schema",
    )?;

    let baseline_report = run_ee(
        &workspace,
        &artifact_dir,
        "05_memory_drift_baseline",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "memory",
            "drift",
            "--mode",
            "all-memories",
        ],
    )?;
    let baseline_search = run_ee(
        &workspace,
        &artifact_dir,
        "06_search_baseline",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "search",
            QUERY,
            "--limit",
            "5",
            "--relevance-floor",
            "0.0",
            "--source-mode",
            "lexical_only",
        ],
    )?;
    let baseline_golden = json!({
        "schema": "ee.memory_drift.no_mock_replay.golden.v1",
        "phase": "baseline",
        "driftReport": normalize_drift_report(&baseline_report.json, &changed_id, &stable_id)?,
        "search": normalize_search(&baseline_search.json, &changed_id, &stable_id)?,
    });
    compare_golden(
        &baseline_golden,
        BASELINE_GOLDEN,
        "baseline drift replay golden",
    )?;
    assert_no_raw_workspace_path(&baseline_golden, &workspace, "baseline golden")?;
    assert_no_raw_workspace_path(&baseline_report.json, &workspace, "baseline drift report")?;
    assert_no_raw_workspace_path(&baseline_search.json, &workspace, "baseline search")?;
    emit_event(
        &events_path,
        &workspace,
        "baseline_snapshot",
        baseline_report.elapsed_ms + baseline_search.elapsed_ms,
        map_of(&[
            ("driftReport", hash_text(&baseline_report.stdout)),
            ("search", hash_text(&baseline_search.stdout)),
        ]),
        map_of(&[
            (
                "baselineGolden",
                hash_json_value("baselineGolden", &baseline_golden)?,
            ),
            ("changedSource", changed_before_hash.clone()),
            ("stableSource", stable_hash.clone()),
        ]),
        memory_drift_codes(&baseline_report.json)
            .into_iter()
            .chain(degraded_codes(&baseline_search.json))
            .collect(),
        "passed",
        None,
        json!({
            "changedMemoryId": normalize_id(&changed_id, &changed_id, &stable_id),
            "stableMemoryId": normalize_id(&stable_id, &changed_id, &stable_id),
        }),
    )?;

    write_text(&changed_source, changed_after)?;
    let changed_after_hash = hash_file(&changed_source)?;
    ensure(
        changed_before_hash != changed_after_hash,
        "controlled source mutation must change source hash",
    )?;
    update_provenance_status(
        &database_path,
        &changed_id,
        "mismatch",
        &format!("no-mock source hash changed from {changed_before_hash} to {changed_after_hash}"),
    )?;
    let memory_rows_before_report = memory_rows(&database_path, &[&changed_id, &stable_id])?;
    emit_event(
        &events_path,
        &workspace,
        "source_change",
        0,
        BTreeMap::new(),
        map_of(&[
            ("changedSourceBefore", changed_before_hash.clone()),
            ("changedSourceAfter", changed_after_hash.clone()),
            ("stableSource", stable_hash.clone()),
        ]),
        Vec::new(),
        "not_applicable",
        Some(
            "controlled provenance validation updated one status before read-only probes"
                .to_owned(),
        ),
        json!({
            "changedSourceHashChanged": true,
            "stableSourceHashPreserved": stable_hash == hash_file(&stable_source)?,
        }),
    )?;

    let changed_report = run_ee(
        &workspace,
        &artifact_dir,
        "07_memory_drift_changed",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "memory",
            "drift",
            "--mode",
            "all-memories",
        ],
    )?;
    let normalized_changed_report =
        normalize_drift_report(&changed_report.json, &changed_id, &stable_id)?;
    ensure_equal(
        normalized_changed_report
            .pointer("/data/summary/changed")
            .and_then(Value::as_u64),
        Some(1),
        "changed drift report summary",
    )?;
    ensure_equal(
        normalized_changed_report
            .pointer("/data/summary/current")
            .and_then(Value::as_u64),
        Some(1),
        "stable drift report summary",
    )?;
    emit_event(
        &events_path,
        &workspace,
        "drift_report",
        changed_report.elapsed_ms,
        map_of(&[("driftReport", hash_text(&changed_report.stdout))]),
        map_of(&[(
            "changedReport",
            hash_json_value("changedReport", &normalized_changed_report)?,
        )]),
        memory_drift_codes(&changed_report.json),
        "passed",
        None,
        json!({
            "changedMemoryStatus": "changed",
            "stableMemoryStatus": "current",
        }),
    )?;

    let changed_search = run_ee(
        &workspace,
        &artifact_dir,
        "08_search_changed",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "search",
            QUERY,
            "--limit",
            "5",
            "--relevance-floor",
            "0.0",
            "--source-mode",
            "lexical_only",
        ],
    )?;
    let normalized_changed_search =
        normalize_search(&changed_search.json, &changed_id, &stable_id)?;
    let changed_golden = json!({
        "schema": "ee.memory_drift.no_mock_replay.golden.v1",
        "phase": "changed_source",
        "driftReport": normalized_changed_report,
        "search": normalized_changed_search,
    });
    compare_golden(
        &changed_golden,
        CHANGED_GOLDEN,
        "changed-source drift replay golden",
    )?;
    assert_no_raw_workspace_path(&changed_golden, &workspace, "changed-source golden")?;
    assert_no_raw_workspace_path(
        &changed_report.json,
        &workspace,
        "changed-source drift report",
    )?;
    assert_no_raw_workspace_path(&changed_search.json, &workspace, "changed-source search")?;

    let results = changed_golden
        .pointer("/search/results")
        .and_then(Value::as_array)
        .ok_or_else(|| "changed golden search results missing".to_owned())?;
    let changed_result = results
        .iter()
        .find(|result| result.get("docId").and_then(Value::as_str) == Some("mem_changed"))
        .ok_or_else(|| "search did not return changed memory".to_owned())?;
    let stable_result = results
        .iter()
        .find(|result| result.get("docId").and_then(Value::as_str) == Some("mem_stable"))
        .ok_or_else(|| "search did not return stable memory".to_owned())?;
    ensure_equal(
        changed_result
            .pointer("/driftHint/driftStatus")
            .and_then(Value::as_str),
        Some("changed"),
        "changed search result drift hint",
    )?;
    ensure_equal(
        stable_result.get("hasDriftHint").and_then(Value::as_bool),
        Some(false),
        "stable search result remains drift-hint free",
    )?;
    emit_event(
        &events_path,
        &workspace,
        "pack_or_search_probe",
        changed_search.elapsed_ms,
        map_of(&[("search", hash_text(&changed_search.stdout))]),
        map_of(&[(
            "changedGolden",
            hash_json_value("changedGolden", &changed_golden)?,
        )]),
        degraded_codes(&changed_search.json),
        "passed",
        None,
        json!({
            "probe": "search",
            "changedMemoryHasDriftHint": true,
            "stableMemoryHasDriftHint": false,
        }),
    )?;

    let memory_rows_after_report = memory_rows(&database_path, &[&changed_id, &stable_id])?;
    ensure_equal(
        &memory_rows_after_report,
        &memory_rows_before_report,
        "memory drift/search read probes must not mutate memory rows",
    )?;
    let changed_codes = degraded_codes(&changed_search.json);
    ensure(
        changed_codes.contains(&"memory_drift_source_changed".to_owned()),
        format!("changed search must emit memory_drift_source_changed, got {changed_codes:?}"),
    )?;
    emit_event(
        &events_path,
        &workspace,
        "assertion",
        0,
        BTreeMap::new(),
        map_of(&[("eventLog", hash_file(&events_path)?)]),
        changed_codes,
        "passed",
        Some("memory rows identical before and after read-only drift/search probes".to_owned()),
        json!({
            "mutationGuardRows": memory_rows_after_report.len(),
            "goldensMatched": true,
            "redactionSafe": true,
        }),
    )?;

    emit_event(
        &events_path,
        &workspace,
        "cleanup",
        0,
        BTreeMap::new(),
        map_of(&[("eventLog", hash_file(&events_path)?)]),
        Vec::new(),
        "not_applicable",
        None,
        json!({
            "tempWorkspaceRetainedUntilTestDrop": true,
            "artifactLogDir": redact_secret_like_content(&log_dir.display().to_string()).content,
        }),
    )?;
    assert_required_events(&events_path)?;

    Ok(())
}

// ===========================================================================
// bd-3sh42: true read-only memory-drift collection under a held writer flock.
//
// The old bounded-probe collector intentionally emitted
// `memory_drift_lock_contention` while `.ee/ee.write.lock` was held. bd-3sh42
// replaced that interim strategy with a genuine read-only database open. The
// tests below pin the replacement contract:
//
// | Req | Clause (source) | Covered by |
// | --- | --- | --- |
// | RO-MUST-1 | A held workspace write-owner flock does not block a read-only drift collector | matrix test, held-lock phase |
// | RO-MUST-2 | The held-lock read remains non-mutating | matrix test, fingerprint guard |
// | RO-MUST-3 | Claim-gate collection does not resurrect the obsolete lock degradation | claim-gate test |
// ===========================================================================

// Only the cfg(unix) held-lock scenarios below consume these; without the
// gate they become dead code on Windows, where `-D warnings` fails the build.
#[cfg(unix)]
const LOCK_CONTENTION_CODE: &str = "memory_drift_lock_contention";

/// Unix only: `rustix::fs` (and the advisory `flock` this harness holds) does
/// not exist on Windows, and an unconditional reference here stopped the whole
/// test crate from compiling on `x86_64-pc-windows-msvc`. The two held-lock
/// scenarios below are gated the same way; Windows has no equivalent lock to
/// hold, so they are skipped there rather than faked.
#[cfg(unix)]
fn hold_workspace_write_lock(workspace: &Path) -> Result<fs::File, String> {
    let lock_path = workspace.join(".ee").join("ee.write.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("failed to open {}: {error}", lock_path.display()))?;
    rustix::fs::flock(
        &lock_file,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .map_err(|error| format!("failed to flock {}: {error}", lock_path.display()))?;
    Ok(lock_file)
}

/// Hash every regular file under `.ee` except the advisory lock file itself
/// (whose open/flock state the test legitimately manipulates). Sorted map so
/// two fingerprints compare deterministically.
#[cfg(unix)]
fn ee_state_fingerprint(workspace: &Path) -> Result<BTreeMap<String, String>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<(), String> {
        let entries =
            fs::read_dir(dir).map_err(|error| format!("read_dir {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("read_dir entry: {error}"))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("file_type {}: {error}", path.display()))?;
            if file_type.is_dir() {
                walk(root, &path, out)?;
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| format!("strip_prefix {}: {error}", path.display()))?
                    .to_string_lossy()
                    .into_owned();
                if relative == "ee.write.lock" {
                    continue;
                }
                out.insert(relative, hash_file(&path)?);
            }
        }
        Ok(())
    }
    let ee_dir = workspace.join(".ee");
    let mut fingerprint = BTreeMap::new();
    walk(&ee_dir, &ee_dir, &mut fingerprint)?;
    Ok(fingerprint)
}

/// Run ee tolerating nonzero stderr diagnostics: swarm surfaces may write
/// human progress to stderr while still emitting one stable JSON envelope on
/// stdout. Exit code must still be zero.
#[cfg(unix)]
fn run_ee_swarm(
    workspace: &Path,
    artifact_dir: &Path,
    step: &str,
    args: &[&str],
) -> Result<CommandOutput, String> {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("failed to run ee {step}: {error}"))?;
    let elapsed_ms = started.elapsed().as_millis();
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("ee {step} stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("ee {step} stderr was not UTF-8: {error}"))?;
    write_text(&artifact_dir.join(format!("{step}.stdout.json")), &stdout)?;
    write_text(&artifact_dir.join(format!("{step}.stderr.txt")), &stderr)?;
    let exit_code = output.status.code().unwrap_or(-1);
    ensure(
        exit_code == 0,
        format!("ee {step} exited {exit_code}; stderr={stderr}"),
    )?;
    let json = serde_json::from_str(&stdout)
        .map_err(|error| format!("ee {step} stdout must be JSON: {error}; stdout={stdout}"))?;
    Ok(CommandOutput {
        json,
        stdout,
        stderr,
        elapsed_ms,
        exit_code,
    })
}

#[cfg(unix)]
fn degraded_entry_with_code<'a>(value: &'a Value, code: &str) -> Option<&'a Value> {
    value
        .get("degraded")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|entry| entry.get("code").and_then(Value::as_str) == Some(code))
}

#[cfg(unix)]
#[test]
fn memory_drift_true_read_only_collection_ignores_writer_flock_without_mutation() -> TestResult {
    let log_dir = unique_log_dir()?;
    let artifact_dir = log_dir.join("lock-contention-artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("lock-contention-events.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-memory-drift-lock-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();

    let init = run_ee(
        &workspace,
        &artifact_dir,
        "lc01_init",
        &["--workspace", &workspace_arg, "--json", "init"],
    )?;
    let remember = run_ee(
        &workspace,
        &artifact_dir,
        "lc02_remember",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "remember",
            "Lock-contention conformance memory: collection must say evidence was not inspected.",
            "--level",
            "procedural",
            "--kind",
            "rule",
        ],
    )?;
    emit_event(
        &events_path,
        &workspace,
        "setup",
        init.elapsed_ms + remember.elapsed_ms,
        map_of(&[
            ("init", hash_text(&init.stdout)),
            ("remember", hash_text(&remember.stdout)),
        ]),
        BTreeMap::new(),
        Vec::new(),
        "validated",
        None,
        json!({"branch": "lc_setup"}),
    )?;

    // LC-MUST-4 control: no lock held, the contention code must be absent.
    let control = run_ee_swarm(
        &workspace,
        &artifact_dir,
        "lc03_brief_control",
        &["--workspace", &workspace_arg, "--json", "swarm", "brief"],
    )?;
    ensure(
        degraded_entry_with_code(&control.json, LOCK_CONTENTION_CODE).is_none(),
        "control swarm brief without a held lock must not emit the lock-contention code",
    )?;
    emit_event(
        &events_path,
        &workspace,
        "baseline_snapshot",
        control.elapsed_ms,
        map_of(&[("brief_control", hash_text(&control.stdout))]),
        BTreeMap::new(),
        degraded_codes(&control.json),
        "validated",
        None,
        json!({"branch": "lc_control_no_lock"}),
    )?;

    let pre_state = ee_state_fingerprint(&workspace)?;

    // Hold the workspace write lock exactly the way the DB layer does:
    // a second open file description with an exclusive non-blocking flock.
    let lock_file = hold_workspace_write_lock(&workspace)?;

    let under_writer_lock = run_ee_swarm(
        &workspace,
        &artifact_dir,
        "lc04_brief_contended",
        &["--workspace", &workspace_arg, "--json", "swarm", "brief"],
    )?;
    ensure(
        degraded_entry_with_code(&under_writer_lock.json, LOCK_CONTENTION_CODE).is_none(),
        "true read-only collection must not join the workspace write-owner flock",
    )?;
    emit_event(
        &events_path,
        &workspace,
        "drift_report",
        under_writer_lock.elapsed_ms,
        map_of(&[(
            "brief_under_writer_lock",
            hash_text(&under_writer_lock.stdout),
        )]),
        BTreeMap::new(),
        degraded_codes(&under_writer_lock.json),
        "validated",
        None,
        json!({"branch": "ro_held_writer_lock_read_succeeds"}),
    )?;

    // RO-MUST-2: the concurrent read-only command mutated nothing while the
    // lock stayed held the whole time.
    let post_state = ee_state_fingerprint(&workspace)?;
    ensure_equal(
        &post_state,
        &pre_state,
        "workspace .ee state under a held writer flock (RO-MUST-2 no-mutation proof)",
    )?;
    emit_event(
        &events_path,
        &workspace,
        "pack_or_search_probe",
        0,
        BTreeMap::new(),
        map_of(&[(
            "ee_state_fingerprint",
            hash_json_serializable("fingerprint", &post_state)?,
        )]),
        Vec::new(),
        "validated",
        Some("ee-state-fingerprint-equal".to_owned()),
        json!({"branch": "lc_no_mutation_guard", "fileCount": post_state.len()}),
    )?;

    // Releasing the lock leaves the same read-only posture.
    drop(lock_file);
    let recovered = run_ee_swarm(
        &workspace,
        &artifact_dir,
        "lc05_brief_recovered",
        &["--workspace", &workspace_arg, "--json", "swarm", "brief"],
    )?;
    ensure(
        degraded_entry_with_code(&recovered.json, LOCK_CONTENTION_CODE).is_none(),
        "released lock must clear the lock-contention code (LC-MUST-4 recovery)",
    )?;
    emit_event(
        &events_path,
        &workspace,
        "source_change",
        recovered.elapsed_ms,
        map_of(&[("brief_recovered", hash_text(&recovered.stdout))]),
        BTreeMap::new(),
        degraded_codes(&recovered.json),
        "validated",
        None,
        json!({"branch": "lc_recovered_no_lock"}),
    )?;

    emit_event(
        &events_path,
        &workspace,
        "assertion",
        0,
        BTreeMap::new(),
        BTreeMap::new(),
        Vec::new(),
        "validated",
        None,
        json!({"branch": "ro_matrix_complete", "matrix": ["RO-MUST-1", "RO-MUST-2"]}),
    )?;
    emit_event(
        &events_path,
        &workspace,
        "cleanup",
        0,
        BTreeMap::new(),
        BTreeMap::new(),
        Vec::new(),
        "validated",
        None,
        json!({"branch": "lc_cleanup"}),
    )?;
    assert_required_events(&events_path)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn memory_drift_claim_gate_read_collection_ignores_writer_flock() -> TestResult {
    let log_dir = unique_log_dir()?;
    let artifact_dir = log_dir.join("lock-gate-artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-memory-drift-lock-gate-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();

    run_ee(
        &workspace,
        &artifact_dir,
        "lg01_init",
        &["--workspace", &workspace_arg, "--json", "init"],
    )?;

    let _lock_file = hold_workspace_write_lock(&workspace)?;
    let gate = run_ee_swarm(
        &workspace,
        &artifact_dir,
        "lg02_work_packet_gate",
        &[
            "--workspace",
            &workspace_arg,
            "--json",
            "swarm",
            "work-packet",
            "--claim-gate",
            "--candidate",
            "bd-koag5-fixture-candidate",
        ],
    )?;

    // RO-MUST-3: source authority may still block for unrelated missing
    // inputs, but the true read-only collector cannot report the obsolete
    // workspace-flock degradation.
    ensure(
        degraded_entry_with_code(&gate.json, LOCK_CONTENTION_CODE).is_none(),
        "claim-gate memory-drift collection must ignore the workspace write-owner flock",
    )?;
    Ok(())
}
