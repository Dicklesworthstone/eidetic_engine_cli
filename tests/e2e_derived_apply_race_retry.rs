//! bd-2ri4f: logged E2E for derived-apply race, conflict, and retry.
//!
//! This intentionally exercises the real `ee` binary and a real isolated
//! FrankenSQLite database. The only stub is the external `cass` executable used
//! to seed deterministic evidence spans; derivation proposal, validation,
//! apply, retry, why, and DB inspection all go through public CLI surfaces.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const TEST_ID: &str = "derived_apply_race_retry";
const SOURCE_A_BODY: &str = "bd-2ri4f source A says derived apply must keep evidence provenance.";
const SOURCE_B_BODY: &str = "bd-2ri4f source B says losing candidates must recover cleanly.";
const EVIDENCE_BODY: &str = "bd-2ri4f shared evidence span consumed by one derived memory only.";
const DERIVED_A_BODY: &str = "bd-2ri4f derived memory A wins the evidence race.";
const DERIVED_B_BODY: &str = "bd-2ri4f derived memory B loses the evidence race.";
const DERIVED_REJECTED_BODY: &str =
    "bd-17bob rejected derived memory must not create mutation artifacts.";

#[derive(Debug)]
struct CommandRun {
    output: Output,
    json: Value,
}

#[derive(Debug, Serialize)]
struct TestEvent {
    schema: &'static str,
    ts: String,
    test_id: &'static str,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<Value>,
}

fn ensure(condition: bool, message: impl Into<String>, log_path: &Path) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(format!(
            "{}; JSONL artifact: {}",
            message.into(),
            log_path.display()
        ))
    }
}

fn unique_run_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-derived-apply-race-retry")
        .join(format!("{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create run dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create parent {}: {error}", parent.display()))?;
    }
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open event log {}: {error}", path.display()))?;
    serde_json::to_writer(&mut file, value)
        .map_err(|error| format!("failed to serialize test event: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("failed to write event newline: {error}"))
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn redacted_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|arg| {
            if [
                SOURCE_A_BODY,
                SOURCE_B_BODY,
                EVIDENCE_BODY,
                DERIVED_A_BODY,
                DERIVED_B_BODY,
                DERIVED_REJECTED_BODY,
            ]
            .contains(&arg.as_str())
            {
                "[REDACTED_TEST_BODY]".to_owned()
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn slug(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn log_note(log_path: &Path, phase: &str, mut fields: BTreeMap<String, Value>) -> TestResult {
    fields.insert("phase".to_owned(), json!(phase));
    append_jsonl(
        log_path,
        &TestEvent {
            schema: "ee.test_event.v1",
            ts: now_rfc3339(),
            test_id: TEST_ID,
            kind: "note",
            command: None,
            args: Vec::new(),
            stdout_hash: None,
            stderr_hash: None,
            stderr_excerpt: None,
            exit_code: None,
            elapsed_ms: None,
            fields: Some(Value::Object(fields.into_iter().collect())),
        },
    )
}

fn run_ee_logged(
    log_path: &Path,
    artifact_dir: &Path,
    phase: &str,
    args: &[String],
    envs: &[(&str, OsString)],
    fields: BTreeMap<String, Value>,
) -> Result<CommandRun, String> {
    let started = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    let stdout_hash = hash_bytes(&output.stdout);
    let stderr_hash = hash_bytes(&output.stderr);
    let stdout_path = artifact_dir.join(format!("{}.stdout.json", slug(phase)));
    let stderr_path = artifact_dir.join(format!("{}.stderr.log", slug(phase)));
    write_text(&stdout_path, &String::from_utf8_lossy(&output.stdout))?;
    write_text(&stderr_path, &String::from_utf8_lossy(&output.stderr))?;

    let mut fields = fields;
    fields.insert("phase".to_owned(), json!(phase));
    let logged_args = redacted_args(args);
    fields.insert("argv".to_owned(), json!(logged_args.clone()));
    fields.insert(
        "argvRedaction".to_owned(),
        json!("raw fixture body arguments are redacted"),
    );
    fields.insert(
        "stdoutArtifactPath".to_owned(),
        json!(stdout_path.display().to_string()),
    );
    fields.insert(
        "stderrArtifactPath".to_owned(),
        json!(stderr_path.display().to_string()),
    );

    append_jsonl(
        log_path,
        &TestEvent {
            schema: "ee.test_event.v1",
            ts: now_rfc3339(),
            test_id: TEST_ID,
            kind: "command_end",
            command: Some("ee"),
            args: logged_args,
            stdout_hash: Some(stdout_hash),
            stderr_hash: Some(stderr_hash),
            stderr_excerpt: Some(
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(4096)
                    .collect(),
            ),
            exit_code: Some(output.status.code().unwrap_or(-1)),
            elapsed_ms: Some(elapsed_ms),
            fields: Some(Value::Object(fields.into_iter().collect())),
        },
    )?;

    let json = serde_json::from_slice::<Value>(&output.stdout).unwrap_or(Value::Null);
    Ok(CommandRun { output, json })
}

fn path_with_fake_cass(fake_bin_dir: &Path) -> Result<OsString, String> {
    let mut paths = vec![fake_bin_dir.to_path_buf()];
    if let Some(path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&path));
    }
    env::join_paths(paths).map_err(|error| error.to_string())
}

fn write_fake_cass(
    fake_bin_dir: &Path,
    payload_dir: &Path,
    workspace: &Path,
) -> Result<(PathBuf, Vec<(&'static str, OsString)>), String> {
    fs::create_dir_all(fake_bin_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(payload_dir).map_err(|error| error.to_string())?;
    fs::set_permissions(fake_bin_dir, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("failed to chmod fake cass dir: {error}"))?;

    let session_path = payload_dir.join("rollout-derived-apply-race.jsonl");
    let session_arg = session_path.display().to_string();
    let workspace_arg = workspace.display().to_string();
    let records = [
        json!({
            "timestamp": "2026-05-24T22:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "bd-2ri4f-derived-apply-race",
                "cwd": workspace_arg,
                "cli_version": "0.42.0"
            }
        }),
        json!({
            "timestamp": "2026-05-24T22:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": EVIDENCE_BODY}]
            }
        }),
    ];
    let mut session_jsonl = String::new();
    for record in records {
        session_jsonl.push_str(&serde_json::to_string(&record).map_err(|error| error.to_string())?);
        session_jsonl.push('\n');
    }
    write_text(&session_path, &session_jsonl)?;

    let sessions_json = payload_dir.join("cass-sessions.json");
    write_text(
        &sessions_json,
        &(json!({
            "sessions": [{
                "path": session_arg,
                "agent": "codex",
                "workspace": workspace_arg,
                "started_at": "2026-05-24T22:00:00Z",
                "ended_at": "2026-05-24T22:00:01Z",
                "message_count": 2,
                "token_count": 32
            }]
        })
        .to_string()
            + "\n"),
    )?;

    let view_jsonl = payload_dir.join("cass-view.jsonl");
    let mut view_payload = String::new();
    for (index, content) in session_jsonl.lines().enumerate() {
        view_payload.push_str(
            &json!({
                "line": index + 1,
                "content": content,
            })
            .to_string(),
        );
        view_payload.push('\n');
    }
    write_text(&view_jsonl, &view_payload)?;

    let invocation_log = payload_dir.join("cass-invocations.log");
    let cass_binary = fake_bin_dir.join("cass");
    write_text(
        &cass_binary,
        r#"#!/bin/sh
if [ -n "${CASS_STUB_INVOCATION_LOG:-}" ]; then
  printf '%s\n' "$*" >> "$CASS_STUB_INVOCATION_LOG"
fi
case "${1:-}" in
  index)
    printf '{"success":true,"conversations":1}\n'
    ;;
  sessions)
    cat "$CASS_STUB_SESSIONS_JSON"
    ;;
  view)
    cat "$CASS_STUB_VIEW_JSONL"
    ;;
  *)
    printf 'unexpected cass stub command: %s\n' "$*" >&2
    exit 64
    ;;
esac
"#,
    )?;
    fs::set_permissions(&cass_binary, fs::Permissions::from_mode(0o555))
        .map_err(|error| format!("failed to chmod fake cass: {error}"))?;

    Ok((
        cass_binary.clone(),
        vec![
            ("PATH", path_with_fake_cass(fake_bin_dir)?),
            ("EE_CASS_BINARY", cass_binary.clone().into_os_string()),
            ("CASS_STUB_SESSIONS_JSON", sessions_json.into_os_string()),
            ("CASS_STUB_VIEW_JSONL", view_jsonl.into_os_string()),
            ("CASS_STUB_INVOCATION_LOG", invocation_log.into_os_string()),
        ],
    ))
}

fn ee_args(workspace_arg: &str, tail: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--workspace".to_owned(),
        workspace_arg.to_owned(),
        "--json".to_owned(),
    ];
    args.extend(tail.iter().map(|value| (*value).to_owned()));
    args
}

fn assert_success(run: &CommandRun, label: &str, log_path: &Path) -> TestResult {
    ensure(
        run.output.status.success(),
        format!(
            "{label} must exit successfully; stdout={}; stderr={}",
            String::from_utf8_lossy(&run.output.stdout),
            String::from_utf8_lossy(&run.output.stderr)
        ),
        log_path,
    )?;
    ensure(
        run.json["schema"].as_str() == Some("ee.response.v2")
            || run.json["schema"].as_str() == Some("ee.response.v1"),
        format!("{label} must emit a response envelope; got {}", run.json),
        log_path,
    )
}

fn memory_id_from_remember(
    run: &CommandRun,
    label: &str,
    log_path: &Path,
) -> Result<String, String> {
    run.json["data"]["public_id"]
        .as_str()
        .or_else(|| run.json["data"]["memory_id"].as_str())
        .or_else(|| run.json["data"]["memoryId"].as_str())
        .or_else(|| run.json["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "{label} missing memory id; JSONL artifact: {}",
                log_path.display()
            )
        })
}

fn candidate_id_from_propose(
    run: &CommandRun,
    label: &str,
    log_path: &Path,
) -> Result<String, String> {
    run.json["data"]["candidateId"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "{label} missing candidateId; JSONL artifact: {}",
                log_path.display()
            )
        })
}

fn first_unlinked_evidence_span_id(inspect: &Value, log_path: &Path) -> Result<String, String> {
    inspect
        .pointer("/data/report/rows")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter().find_map(|row| {
                let values = row.get("values")?;
                let id = values.get("id")?.as_str()?;
                let memory_id_is_null = values.get("memory_id").is_none_or(Value::is_null);
                memory_id_is_null.then(|| id.to_owned())
            })
        })
        .ok_or_else(|| {
            format!(
                "db inspect evidence_spans did not expose an unlinked evidence span; JSONL artifact: {}",
                log_path.display()
            )
        })
}

fn command_fields(
    candidate_ids: &[&str],
    source_ids: &[&str],
    evidence_span_ids: &[&str],
    created_memory_id: Option<&str>,
    assertion_result: &str,
) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("candidateIds".to_owned(), json!(candidate_ids)),
        ("sourceIds".to_owned(), json!(source_ids)),
        ("evidenceSpanIds".to_owned(), json!(evidence_span_ids)),
        ("createdMemoryId".to_owned(), json!(created_memory_id)),
        ("assertionResult".to_owned(), json!(assertion_result)),
    ])
}

fn conflict_codes(value: &Value) -> Vec<String> {
    let mut codes = Vec::new();
    if let Some(code) = value.pointer("/error/code").and_then(Value::as_str) {
        codes.push(code.to_owned());
    }
    if let Some(errors) = value
        .pointer("/data/application/errors")
        .and_then(Value::as_array)
    {
        for error in errors {
            if let Some(code) = error.get("code").and_then(Value::as_str) {
                codes.push(code.to_owned());
            }
        }
    }
    if let Some(degraded) = value.get("degraded").and_then(Value::as_array) {
        for item in degraded {
            if let Some(code) = item.get("code").and_then(Value::as_str) {
                codes.push(code.to_owned());
            }
        }
    }
    codes.sort();
    codes.dedup();
    codes
}

fn has_structured_recovery(value: &Value) -> bool {
    value
        .pointer("/error/details/recovery")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
        || value
            .pointer("/data/application/errors")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("repair")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                })
            })
}

fn created_memory_id(run: &CommandRun, log_path: &Path) -> Result<String, String> {
    run.json
        .pointer("/data/application/createdMemoryId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "applied candidate did not expose createdMemoryId; JSONL artifact: {}",
                log_path.display()
            )
        })
}

fn assert_event_log_has_phases(log_path: &Path, expected: &[&str]) -> TestResult {
    let text = fs::read_to_string(log_path)
        .map_err(|error| format!("failed to read event log {}: {error}", log_path.display()))?;
    for phase in expected {
        ensure(
            text.contains(&format!(r#""phase":"{phase}""#)),
            format!("event log missing phase {phase}"),
            log_path,
        )?;
    }
    for raw_body in [
        SOURCE_A_BODY,
        SOURCE_B_BODY,
        EVIDENCE_BODY,
        DERIVED_A_BODY,
        DERIVED_B_BODY,
        DERIVED_REJECTED_BODY,
    ] {
        ensure(
            !text.contains(raw_body),
            "event log must carry hashes and ids, not raw fixture bodies",
            log_path,
        )?;
    }
    Ok(())
}

#[test]
fn rejected_derived_candidate_is_terminal_and_leaves_no_mutation_artifacts() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    let artifact_dir = run_dir.join("artifacts");
    let fake_bin_dir = run_dir.join("bin");
    let cass_payload_dir = run_dir.join("cass");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&artifact_dir).map_err(|error| error.to_string())?;
    let log_path = run_dir.join("derived_reject_terminal.events.jsonl");
    let workspace_arg = workspace.display().to_string();
    let database_arg = workspace.join(".ee").join("ee.db").display().to_string();
    let (_cass_binary, cass_envs) = write_fake_cass(&fake_bin_dir, &cass_payload_dir, &workspace)?;

    log_note(
        &log_path,
        "setup_reject_terminal",
        BTreeMap::from([
            ("workspace".to_owned(), json!(workspace_arg)),
            ("assertionResult".to_owned(), json!("started")),
        ]),
    )?;

    let init = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_setup_init",
        &ee_args(&workspace_arg, &["init"]),
        &[],
        command_fields(&[], &[], &[], None, "workspace_initialized"),
    )?;
    assert_success(&init, "ee init for reject path", &log_path)?;

    let import = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_setup_import_cass",
        &ee_args(
            &workspace_arg,
            &[
                "import",
                "cass",
                "--database",
                &database_arg,
                "--limit",
                "1",
            ],
        ),
        &cass_envs,
        command_fields(&[], &[], &[], None, "evidence_imported"),
    )?;
    assert_success(&import, "ee import cass for reject path", &log_path)?;

    let evidence_inspect = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_evidence_span_lookup",
        &ee_args(
            &workspace_arg,
            &["db", "inspect", "evidence_spans", "--limit", "10"],
        ),
        &[],
        command_fields(&[], &[], &[], None, "evidence_span_discovered"),
    )?;
    assert_success(
        &evidence_inspect,
        "db inspect evidence_spans for reject path",
        &log_path,
    )?;
    let evidence_span_id = first_unlinked_evidence_span_id(&evidence_inspect.json, &log_path)?;

    let source = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_setup_source",
        &ee_args(
            &workspace_arg,
            &[
                "remember",
                "--level",
                "semantic",
                "--kind",
                "fact",
                SOURCE_A_BODY,
            ],
        ),
        &[],
        command_fields(&[], &[], &[&evidence_span_id], None, "source_seeded"),
    )?;
    assert_success(&source, "remember reject source", &log_path)?;
    let source_id = memory_id_from_remember(&source, "reject source", &log_path)?;

    let propose = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_propose",
        &ee_args(
            &workspace_arg,
            &[
                "curate",
                "propose-derived",
                "--level",
                "semantic",
                "--kind",
                "insight",
                "--content",
                DERIVED_REJECTED_BODY,
                "--source-memory",
                &source_id,
                "--source-evidence-span",
                &evidence_span_id,
                "--producer-kind",
                "e2e_test",
            ],
        ),
        &[],
        command_fields(
            &[],
            &[&source_id],
            &[&evidence_span_id],
            None,
            "reject_candidate_proposed",
        ),
    )?;
    assert_success(&propose, "propose reject candidate", &log_path)?;
    let candidate_id = candidate_id_from_propose(&propose, "reject candidate", &log_path)?;

    let reject = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_candidate",
        &ee_args(
            &workspace_arg,
            &[
                "curate",
                "reject",
                &candidate_id,
                "--reason",
                "bd-17bob terminal reject invariant",
            ],
        ),
        &[],
        command_fields(
            &[&candidate_id],
            &[&source_id],
            &[&evidence_span_id],
            None,
            "candidate_rejected",
        ),
    )?;
    assert_success(&reject, "reject derived candidate", &log_path)?;
    ensure(
        reject
            .json
            .pointer("/data/toStatus")
            .and_then(Value::as_str)
            == Some("rejected"),
        format!(
            "reject output must transition candidate to rejected: {}",
            reject.json
        ),
        &log_path,
    )?;
    ensure(
        reject
            .json
            .pointer("/data/auditId")
            .and_then(Value::as_str)
            .is_some(),
        format!(
            "reject output must expose the audit row id: {}",
            reject.json
        ),
        &log_path,
    )?;

    let show = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_show_terminal",
        &ee_args(&workspace_arg, &["curate", "show", &candidate_id]),
        &[],
        command_fields(
            &[&candidate_id],
            &[&source_id],
            &[&evidence_span_id],
            None,
            "terminal_status_visible",
        ),
    )?;
    assert_success(&show, "show rejected derived candidate", &log_path)?;
    ensure(
        show.json
            .pointer("/data/candidate/status")
            .and_then(Value::as_str)
            == Some("rejected"),
        format!(
            "curate show must expose rejected terminal status: {}",
            show.json
        ),
        &log_path,
    )?;
    ensure(
        show.json
            .pointer("/data/plannedApplication/status")
            .and_then(Value::as_str)
            == Some("blocked"),
        format!("curate show must block rejected candidates: {}", show.json),
        &log_path,
    )?;
    ensure(
        show.json
            .pointer("/data/plannedApplication/createdMemoryId")
            .is_none_or(Value::is_null),
        format!(
            "rejected candidate preview must not expose createdMemoryId: {}",
            show.json
        ),
        &log_path,
    )?;
    ensure(
        show.json
            .pointer("/data/plannedApplication/plannedDerivedFromLinks")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        format!(
            "rejected candidate preview must not plan derived links: {}",
            show.json
        ),
        &log_path,
    )?;
    ensure(
        show.json
            .pointer("/data/plannedApplication/plannedEvidenceAttachments")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        format!(
            "rejected candidate preview must not plan evidence attachments: {}",
            show.json
        ),
        &log_path,
    )?;

    let apply = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_apply_blocked",
        &ee_args(&workspace_arg, &["curate", "apply", &candidate_id]),
        &[],
        command_fields(
            &[&candidate_id],
            &[&source_id],
            &[&evidence_span_id],
            None,
            "terminal_apply_blocked",
        ),
    )?;
    assert_success(&apply, "apply rejected derived candidate", &log_path)?;
    ensure(
        apply
            .json
            .pointer("/data/application/status")
            .and_then(Value::as_str)
            == Some("blocked"),
        format!(
            "apply on rejected candidate must be blocked: {}",
            apply.json
        ),
        &log_path,
    )?;
    ensure(
        conflict_codes(&apply.json)
            .iter()
            .any(|code| code == "candidate_status_terminal"),
        format!(
            "blocked apply must explain terminal status; got {}",
            apply.json
        ),
        &log_path,
    )?;
    ensure(
        apply
            .json
            .pointer("/data/mutation/persisted")
            .and_then(Value::as_bool)
            == Some(false),
        format!("blocked apply must not persist mutation: {}", apply.json),
        &log_path,
    )?;
    ensure(
        apply
            .json
            .pointer("/data/application/createdMemoryId")
            .is_none_or(Value::is_null),
        format!(
            "blocked apply must not expose a created memory id: {}",
            apply.json
        ),
        &log_path,
    )?;

    let memories = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_db_memories_invariant",
        &ee_args(
            &workspace_arg,
            &["db", "inspect", "memories", "--limit", "20"],
        ),
        &[],
        command_fields(
            &[&candidate_id],
            &[&source_id],
            &[&evidence_span_id],
            None,
            "no_rejected_memory_created",
        ),
    )?;
    assert_success(&memories, "db inspect memories after reject", &log_path)?;
    ensure(
        memories
            .json
            .pointer("/data/report/rows")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                !rows.iter().any(|row| {
                    row.pointer("/values/content").and_then(Value::as_str)
                        == Some(DERIVED_REJECTED_BODY)
                })
            }),
        format!(
            "rejected derived content must not be persisted as a memory: {}",
            memories.json
        ),
        &log_path,
    )?;

    let links = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_db_links_invariant",
        &ee_args(
            &workspace_arg,
            &["db", "inspect", "memory_links", "--limit", "20"],
        ),
        &[],
        command_fields(
            &[&candidate_id],
            &[&source_id],
            &[&evidence_span_id],
            None,
            "no_derived_links_created",
        ),
    )?;
    assert_success(&links, "db inspect memory_links after reject", &log_path)?;
    ensure(
        links
            .json
            .pointer("/data/report/rows")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        format!("reject path must not create memory links: {}", links.json),
        &log_path,
    )?;

    let evidence_after = run_ee_logged(
        &log_path,
        &artifact_dir,
        "reject_db_evidence_invariant",
        &ee_args(
            &workspace_arg,
            &["db", "inspect", "evidence_spans", "--limit", "10"],
        ),
        &[],
        command_fields(
            &[&candidate_id],
            &[&source_id],
            &[&evidence_span_id],
            None,
            "evidence_left_unattached",
        ),
    )?;
    assert_success(
        &evidence_after,
        "db inspect evidence after reject",
        &log_path,
    )?;
    ensure(
        evidence_after
            .json
            .pointer("/data/report/rows")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    let values = row.get("values").unwrap_or(&Value::Null);
                    values.get("id").and_then(Value::as_str) == Some(evidence_span_id.as_str())
                        && values.get("memory_id").is_none_or(Value::is_null)
                })
            }),
        format!(
            "reject path must leave evidence span unattached: {}",
            evidence_after.json
        ),
        &log_path,
    )?;

    assert_event_log_has_phases(
        &log_path,
        &[
            "setup_reject_terminal",
            "reject_propose",
            "reject_candidate",
            "reject_show_terminal",
            "reject_apply_blocked",
            "reject_db_memories_invariant",
            "reject_db_links_invariant",
            "reject_db_evidence_invariant",
        ],
    )
}

#[test]
fn logged_derived_apply_race_conflict_and_retry() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    let artifact_dir = run_dir.join("artifacts");
    let fake_bin_dir = run_dir.join("bin");
    let cass_payload_dir = run_dir.join("cass");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&artifact_dir).map_err(|error| error.to_string())?;
    let log_path = run_dir.join("derived_apply_race_retry.events.jsonl");
    let workspace_arg = workspace.display().to_string();
    let database_arg = workspace.join(".ee").join("ee.db").display().to_string();
    let (_cass_binary, cass_envs) = write_fake_cass(&fake_bin_dir, &cass_payload_dir, &workspace)?;

    log_note(
        &log_path,
        "setup",
        BTreeMap::from([
            ("workspace".to_owned(), json!(workspace_arg)),
            ("assertionResult".to_owned(), json!("started")),
        ]),
    )?;

    let init = run_ee_logged(
        &log_path,
        &artifact_dir,
        "setup_init",
        &ee_args(&workspace_arg, &["init"]),
        &[],
        command_fields(&[], &[], &[], None, "workspace_initialized"),
    )?;
    assert_success(&init, "ee init", &log_path)?;

    let import = run_ee_logged(
        &log_path,
        &artifact_dir,
        "setup_import_cass",
        &ee_args(
            &workspace_arg,
            &[
                "import",
                "cass",
                "--database",
                &database_arg,
                "--limit",
                "1",
            ],
        ),
        &cass_envs,
        command_fields(&[], &[], &[], None, "evidence_imported"),
    )?;
    assert_success(&import, "ee import cass", &log_path)?;
    ensure(
        import
            .json
            .pointer("/data/spansImported")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            >= 1,
        format!(
            "import must create at least one evidence span: {}",
            import.json
        ),
        &log_path,
    )?;

    let evidence_inspect = run_ee_logged(
        &log_path,
        &artifact_dir,
        "setup_evidence_span_lookup",
        &ee_args(
            &workspace_arg,
            &["db", "inspect", "evidence_spans", "--limit", "10"],
        ),
        &[],
        command_fields(&[], &[], &[], None, "evidence_span_discovered"),
    )?;
    assert_success(&evidence_inspect, "db inspect evidence_spans", &log_path)?;
    let evidence_span_id = first_unlinked_evidence_span_id(&evidence_inspect.json, &log_path)?;

    let source_a = run_ee_logged(
        &log_path,
        &artifact_dir,
        "setup_source_a",
        &ee_args(
            &workspace_arg,
            &[
                "remember",
                "--level",
                "semantic",
                "--kind",
                "fact",
                SOURCE_A_BODY,
            ],
        ),
        &[],
        command_fields(&[], &[], &[&evidence_span_id], None, "source_a_seeded"),
    )?;
    assert_success(&source_a, "remember source A", &log_path)?;
    let source_a_id = memory_id_from_remember(&source_a, "source A", &log_path)?;

    let source_b = run_ee_logged(
        &log_path,
        &artifact_dir,
        "setup_source_b",
        &ee_args(
            &workspace_arg,
            &[
                "remember",
                "--level",
                "semantic",
                "--kind",
                "fact",
                SOURCE_B_BODY,
            ],
        ),
        &[],
        command_fields(
            &[],
            &[&source_a_id],
            &[&evidence_span_id],
            None,
            "source_b_seeded",
        ),
    )?;
    assert_success(&source_b, "remember source B", &log_path)?;
    let source_b_id = memory_id_from_remember(&source_b, "source B", &log_path)?;

    let propose_a = run_ee_logged(
        &log_path,
        &artifact_dir,
        "propose_a",
        &ee_args(
            &workspace_arg,
            &[
                "curate",
                "propose-derived",
                "--level",
                "semantic",
                "--kind",
                "insight",
                "--content",
                DERIVED_A_BODY,
                "--source-memory",
                &source_a_id,
                "--source-evidence-span",
                &evidence_span_id,
                "--producer-kind",
                "e2e_test",
            ],
        ),
        &[],
        command_fields(
            &[],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            None,
            "candidate_a_proposed",
        ),
    )?;
    assert_success(&propose_a, "propose candidate A", &log_path)?;
    let candidate_a_id = candidate_id_from_propose(&propose_a, "candidate A", &log_path)?;

    let propose_b = run_ee_logged(
        &log_path,
        &artifact_dir,
        "propose_b",
        &ee_args(
            &workspace_arg,
            &[
                "curate",
                "propose-derived",
                "--level",
                "semantic",
                "--kind",
                "insight",
                "--content",
                DERIVED_B_BODY,
                "--source-memory",
                &source_b_id,
                "--source-evidence-span",
                &evidence_span_id,
                "--producer-kind",
                "e2e_test",
            ],
        ),
        &[],
        command_fields(
            &[&candidate_a_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            None,
            "candidate_b_proposed",
        ),
    )?;
    assert_success(&propose_b, "propose candidate B", &log_path)?;
    let candidate_b_id = candidate_id_from_propose(&propose_b, "candidate B", &log_path)?;
    ensure(
        candidate_a_id != candidate_b_id,
        "overlapping candidates must have distinct ids before conflict test",
        &log_path,
    )?;

    let validate_a = run_ee_logged(
        &log_path,
        &artifact_dir,
        "validate_a",
        &ee_args(&workspace_arg, &["curate", "validate", &candidate_a_id]),
        &[],
        command_fields(
            &[&candidate_a_id, &candidate_b_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            None,
            "candidate_a_approved",
        ),
    )?;
    assert_success(&validate_a, "validate candidate A", &log_path)?;
    ensure(
        validate_a
            .json
            .pointer("/data/validation/status")
            .and_then(Value::as_str)
            == Some("passed"),
        format!("candidate A validation should pass: {}", validate_a.json),
        &log_path,
    )?;

    let validate_b = run_ee_logged(
        &log_path,
        &artifact_dir,
        "validate_b",
        &ee_args(&workspace_arg, &["curate", "validate", &candidate_b_id]),
        &[],
        command_fields(
            &[&candidate_a_id, &candidate_b_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            None,
            "candidate_b_approved_before_race",
        ),
    )?;
    assert_success(&validate_b, "validate candidate B", &log_path)?;
    ensure(
        validate_b
            .json
            .pointer("/data/validation/status")
            .and_then(Value::as_str)
            == Some("passed"),
        format!(
            "candidate B validation should pass before the race: {}",
            validate_b.json
        ),
        &log_path,
    )?;

    let apply_a = run_ee_logged(
        &log_path,
        &artifact_dir,
        "apply_a",
        &ee_args(&workspace_arg, &["curate", "apply", &candidate_a_id]),
        &[],
        command_fields(
            &[&candidate_a_id, &candidate_b_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            None,
            "candidate_a_applied",
        ),
    )?;
    assert_success(&apply_a, "apply candidate A", &log_path)?;
    ensure(
        apply_a
            .json
            .pointer("/data/application/status")
            .and_then(Value::as_str)
            == Some("applied"),
        format!("candidate A should apply: {}", apply_a.json),
        &log_path,
    )?;
    let created_memory_id = created_memory_id(&apply_a, &log_path)?;

    let apply_b = run_ee_logged(
        &log_path,
        &artifact_dir,
        "apply_b_conflict",
        &ee_args(&workspace_arg, &["curate", "apply", &candidate_b_id]),
        &[],
        command_fields(
            &[&candidate_a_id, &candidate_b_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            Some(&created_memory_id),
            "candidate_b_conflicted",
        ),
    )?;
    assert_success(&apply_b, "apply candidate B conflict report", &log_path)?;
    let codes = conflict_codes(&apply_b.json);
    ensure(
        codes
            .iter()
            .any(|code| code == "derived_source_evidence_already_linked"),
        format!(
            "candidate B must surface derived_source_evidence_already_linked; got {codes:?}: {}",
            apply_b.json
        ),
        &log_path,
    )?;
    ensure(
        apply_b
            .json
            .pointer("/data/mutation/persisted")
            .and_then(Value::as_bool)
            == Some(false),
        format!(
            "candidate B conflict must not persist mutation: {}",
            apply_b.json
        ),
        &log_path,
    )?;

    let retry_a = run_ee_logged(
        &log_path,
        &artifact_dir,
        "retry_a_idempotent",
        &ee_args(&workspace_arg, &["curate", "apply", &candidate_a_id]),
        &[],
        command_fields(
            &[&candidate_a_id, &candidate_b_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            Some(&created_memory_id),
            "candidate_a_replay_idempotent",
        ),
    )?;
    assert_success(&retry_a, "retry candidate A", &log_path)?;
    ensure(
        retry_a
            .json
            .pointer("/data/application/status")
            .and_then(Value::as_str)
            == Some("already_applied"),
        format!("retry A should be idempotent replay: {}", retry_a.json),
        &log_path,
    )?;
    ensure(
        retry_a
            .json
            .pointer("/data/application/createdMemoryId")
            .and_then(Value::as_str)
            == Some(created_memory_id.as_str()),
        format!(
            "retry A must return original created memory id {created_memory_id}: {}",
            retry_a.json
        ),
        &log_path,
    )?;

    let why = run_ee_logged(
        &log_path,
        &artifact_dir,
        "why_created_memory",
        &ee_args(&workspace_arg, &["why", &created_memory_id]),
        &[],
        command_fields(
            &[&candidate_a_id, &candidate_b_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            Some(&created_memory_id),
            "why_provenance_visible",
        ),
    )?;
    assert_success(&why, "why created memory", &log_path)?;
    ensure(
        why.json.pointer("/data/memoryId").and_then(Value::as_str)
            == Some(created_memory_id.as_str()),
        format!(
            "why must describe created memory {created_memory_id}: {}",
            why.json
        ),
        &log_path,
    )?;
    ensure(
        why.json.pointer("/data/storage/origin").is_some(),
        format!("why must expose storage provenance: {}", why.json),
        &log_path,
    )?;
    ensure(
        why.json
            .pointer("/data/links")
            .and_then(Value::as_array)
            .is_some_and(|links| {
                links.iter().any(|link| {
                    link.get("relation").and_then(Value::as_str) == Some("derived_from")
                        && link.get("linkedMemoryId").and_then(Value::as_str)
                            == Some(source_a_id.as_str())
                })
            }),
        format!(
            "why must expose derived_from provenance link to source A: {}",
            why.json
        ),
        &log_path,
    )?;

    let evidence_after = run_ee_logged(
        &log_path,
        &artifact_dir,
        "db_invariant_check",
        &ee_args(
            &workspace_arg,
            &["db", "inspect", "evidence_spans", "--limit", "10"],
        ),
        &[],
        command_fields(
            &[&candidate_a_id, &candidate_b_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            Some(&created_memory_id),
            "evidence_attached_once",
        ),
    )?;
    assert_success(
        &evidence_after,
        "db inspect evidence_spans after apply",
        &log_path,
    )?;
    ensure(
        evidence_after
            .json
            .pointer("/data/report/rows")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    let values = row.get("values").unwrap_or(&Value::Null);
                    values.get("id").and_then(Value::as_str) == Some(evidence_span_id.as_str())
                        && values.get("memory_id").and_then(Value::as_str)
                            == Some(created_memory_id.as_str())
                })
            }),
        format!(
            "evidence span must be attached exactly to created memory {created_memory_id}: {}",
            evidence_after.json
        ),
        &log_path,
    )?;

    let candidates_after = run_ee_logged(
        &log_path,
        &artifact_dir,
        "db_candidate_invariant_check",
        &ee_args(
            &workspace_arg,
            &["db", "inspect", "curation_candidates", "--limit", "20"],
        ),
        &[],
        command_fields(
            &[&candidate_a_id, &candidate_b_id],
            &[&source_a_id, &source_b_id],
            &[&evidence_span_id],
            Some(&created_memory_id),
            "candidate_statuses_stable",
        ),
    )?;
    assert_success(
        &candidates_after,
        "db inspect curation_candidates",
        &log_path,
    )?;
    ensure(
        candidates_after
            .json
            .pointer("/data/report/rows")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                let mut a_applied = false;
                let mut b_approved = false;
                for row in rows {
                    let values = row.get("values").unwrap_or(&Value::Null);
                    match values.get("id").and_then(Value::as_str) {
                        Some(id) if id == candidate_a_id => {
                            a_applied =
                                values.get("status").and_then(Value::as_str) == Some("applied");
                        }
                        Some(id) if id == candidate_b_id => {
                            b_approved =
                                values.get("status").and_then(Value::as_str) == Some("approved");
                        }
                        _ => {}
                    }
                }
                a_applied && b_approved
            }),
        format!(
            "candidate A must be applied and B must remain approved: {}",
            candidates_after.json
        ),
        &log_path,
    )?;

    log_note(
        &log_path,
        "recovery_hint_check",
        BTreeMap::from([
            (
                "candidateIds".to_owned(),
                json!([candidate_a_id, candidate_b_id]),
            ),
            ("sourceIds".to_owned(), json!([source_a_id, source_b_id])),
            ("evidenceSpanIds".to_owned(), json!([evidence_span_id])),
            ("createdMemoryId".to_owned(), json!(created_memory_id)),
            ("degradedErrorCodes".to_owned(), json!(codes)),
            (
                "assertionResult".to_owned(),
                json!(if has_structured_recovery(&apply_b.json) {
                    "structured_recovery_present"
                } else {
                    "structured_recovery_missing"
                }),
            ),
        ]),
    )?;
    ensure(
        has_structured_recovery(&apply_b.json),
        format!(
            "conflict output must include repair/recovery actions: {}",
            apply_b.json
        ),
        &log_path,
    )?;

    assert_event_log_has_phases(
        &log_path,
        &[
            "setup",
            "propose_a",
            "propose_b",
            "validate_a",
            "validate_b",
            "apply_a",
            "apply_b_conflict",
            "retry_a_idempotent",
            "why_created_memory",
            "db_invariant_check",
            "recovery_hint_check",
        ],
    )
}
