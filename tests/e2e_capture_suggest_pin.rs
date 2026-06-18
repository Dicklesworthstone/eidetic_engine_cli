//! bd-2vq2z.7 follow-up: real-binary pin test for `ee capture suggest`.
//!
//! The unit tests in `core::curate` cover the pure suggestion builder. This
//! integration test exercises the shipped CLI route against the real `ee`
//! binary and a real FrankenSQLite database:
//!
//! * seed a CASS session and evidence spans in an initialized workspace DB
//! * run `ee capture suggest --from-session` and `--from-recent`
//! * assert the response is read-only and includes explicit accept/reject
//!   commands instead of silently mutating memory
//! * assert invalid edge arguments return a structured usage repair
//! * emit `ee.test_event.v1` JSONL evidence for setup, commands, assertions,
//!   and the durable no-mutation check

#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::db::{CreateEvidenceSpanInput, CreateSessionInput, DbConnection};
use ee::models::{EvidenceId, SessionId, WorkspaceId};
use ee::obs::test_log::{EventKind, LogLevel, TestEvent, excerpt_stderr, hash_bytes, log_event_to};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const TEST_ID: &str = "e2e_capture_suggest_pin";
const CASS_SESSION_ID: &str = "cass-capture-pin-session-a";

fn emit_event(log_path: &Path, event: TestEvent) -> TestResult {
    if log_event_to(log_path, LogLevel::Verbose, &event) {
        Ok(())
    } else {
        Err(format!(
            "failed to write structured test event to {}",
            log_path.display()
        ))
    }
}

fn emit_note(log_path: &Path, phase: &str, details: Value) -> TestResult {
    emit_event(
        log_path,
        TestEvent::new(TEST_ID, EventKind::Note)
            .with_field("phase", phase)
            .with_field("details", details),
    )
}

fn assert_logged(log_path: &Path, label: &str, condition: bool, details: Value) -> TestResult {
    let kind = if condition {
        EventKind::AssertOk
    } else {
        EventKind::AssertFail
    };
    emit_event(
        log_path,
        TestEvent::new(TEST_ID, kind)
            .with_field("label", label)
            .with_field("details", details.clone()),
    )?;
    if condition {
        Ok(())
    } else {
        Err(format!("{label} assertion failed: {details}"))
    }
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-capture-suggest-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn stable_workspace_id(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let hash = blake3::hash(format!("workspace:{}", canonical.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn run_ee_logged(log_path: &Path, args: &[&str]) -> Result<Output, String> {
    let mut start_event = TestEvent::new(TEST_ID, EventKind::CommandStart);
    start_event.command = Some("ee".to_owned());
    start_event.args = args.iter().map(|arg| (*arg).to_owned()).collect();
    emit_event(log_path, start_event)?;

    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    let mut event = TestEvent::new(TEST_ID, EventKind::CommandEnd);
    event.command = Some("ee".to_owned());
    event.args = args.iter().map(|arg| (*arg).to_owned()).collect();
    event.exit_code = output.status.code();
    event.elapsed_ms = Some(elapsed_ms);
    event.stdout_hash = Some(hash_bytes(&output.stdout));
    event.stderr_excerpt = Some(excerpt_stderr(&output.stderr, 4096));
    emit_event(log_path, event)?;

    Ok(output)
}

fn run_ee_json(log_path: &Path, args: &[&str]) -> Result<(Output, Value), String> {
    let output = run_ee_logged(log_path, args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "ee {} stdout must be JSON: {error}; stdout={}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout)
        )
    })?;
    Ok((output, parsed))
}

fn init_workspace(workspace: &Path, log_path: &Path) -> TestResult {
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;
    let (output, parsed) =
        run_ee_json(log_path, &["--workspace", workspace_arg, "--json", "init"])?;
    assert_logged(
        log_path,
        "ee_init_success",
        output.status.success() && parsed["success"] == Value::Bool(true),
        json!({
            "status": output.status.code(),
            "schema": parsed["schema"],
            "stdoutHash": hash_bytes(&output.stdout),
        }),
    )
}

fn session_input(workspace_id: &str) -> CreateSessionInput {
    CreateSessionInput {
        workspace_id: workspace_id.to_owned(),
        cass_session_id: CASS_SESSION_ID.to_owned(),
        source_path: Some("/tmp/cass/capture-pin-session.jsonl".to_owned()),
        agent_name: Some("codex".to_owned()),
        model: Some("gpt-5".to_owned()),
        started_at: Some("2026-06-18T00:00:00Z".to_owned()),
        ended_at: Some("2026-06-18T00:12:00Z".to_owned()),
        message_count: 8,
        token_count: Some(1200),
        content_hash: format!(
            "blake3:{}",
            blake3::hash(CASS_SESSION_ID.as_bytes()).to_hex()
        ),
        metadata_json: Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.to_owned()),
    }
}

fn evidence_span_input(
    workspace_id: &str,
    session_id: &str,
    cass_span_id: &str,
    start_line: u32,
    excerpt: &str,
) -> CreateEvidenceSpanInput {
    CreateEvidenceSpanInput {
        workspace_id: workspace_id.to_owned(),
        session_id: session_id.to_owned(),
        memory_id: None,
        cass_span_id: cass_span_id.to_owned(),
        span_kind: "message".to_owned(),
        start_line,
        end_line: start_line + 1,
        start_byte: Some(start_line.saturating_mul(100)),
        end_byte: Some(start_line.saturating_mul(100).saturating_add(80)),
        role: Some("assistant".to_owned()),
        excerpt: excerpt.to_owned(),
        content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
        metadata_json: Some(r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.to_owned()),
    }
}

fn seed_capture_session(workspace: &Path, log_path: &Path) -> Result<(PathBuf, String), String> {
    let database_path = workspace.join(".ee").join("ee.db");
    let workspace_id = stable_workspace_id(workspace);
    let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(0x2_7_0001)).to_string();
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_session(&session_id, &session_input(&workspace_id))
        .map_err(|error| error.to_string())?;

    for (index, excerpt) in [
        "Capture testing fixture says e2e tests must keep structured logs for every command.",
        "Capture testing evidence says real-binary assertions must verify read-only suggestions.",
        "Capture testing notes require explicit accept and reject commands before storing memory.",
    ]
    .iter()
    .enumerate()
    {
        let evidence_id = EvidenceId::from_uuid(uuid::Uuid::from_u128(
            0x2_7_1000 + u128::try_from(index).map_err(|error| error.to_string())?,
        ))
        .to_string();
        connection
            .insert_evidence_span(
                &evidence_id,
                &evidence_span_input(
                    &workspace_id,
                    &session_id,
                    &format!("capture-pin-{index}"),
                    u32::try_from(index + 1).map_err(|error| error.to_string())?,
                    excerpt,
                ),
            )
            .map_err(|error| error.to_string())?;
    }

    let before_count = connection
        .list_curation_candidates(&workspace_id, None, None, None)
        .map_err(|error| error.to_string())?
        .len();
    connection.close().map_err(|error| error.to_string())?;

    emit_note(
        log_path,
        "seed",
        json!({
            "workspaceId": workspace_id,
            "sessionId": session_id,
            "cassSessionId": CASS_SESSION_ID,
            "evidenceSpanCount": 3,
            "curationCandidateCountBefore": before_count,
            "databasePathHash": hash_bytes(database_path.display().to_string().as_bytes()),
        }),
    )?;

    Ok((database_path, workspace_id))
}

fn curation_candidate_count(database_path: &Path, workspace_id: &str) -> Result<usize, String> {
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    let count = connection
        .list_curation_candidates(workspace_id, None, None, None)
        .map_err(|error| error.to_string())?
        .len();
    connection.close().map_err(|error| error.to_string())?;
    Ok(count)
}

fn assert_capture_report(
    log_path: &Path,
    parsed: &Value,
    expected_source: &str,
    requested_session: Option<&str>,
) -> TestResult {
    assert_logged(
        log_path,
        "capture_response_envelope",
        parsed["schema"].as_str() == Some("ee.response.v2")
            && parsed["success"] == Value::Bool(true)
            && parsed["data"]["schema"].as_str() == Some("ee.capture_suggestions.v1"),
        json!({
            "schema": parsed["schema"],
            "success": parsed["success"],
            "innerSchema": parsed["data"]["schema"],
        }),
    )?;

    let data = &parsed["data"];
    assert_logged(
        log_path,
        "capture_selection_source",
        data["selection"]["source"].as_str() == Some(expected_source)
            && data["selection"]["requestedSessionId"].as_str() == requested_session,
        json!({
            "expectedSource": expected_source,
            "actualSource": data["selection"]["source"],
            "requestedSessionId": data["selection"]["requestedSessionId"],
        }),
    )?;
    assert_logged(
        log_path,
        "capture_read_only_contract",
        data["readOnly"] == Value::Bool(true) && data["durableMutation"] == Value::Bool(false),
        json!({
            "readOnly": data["readOnly"],
            "durableMutation": data["durableMutation"],
        }),
    )?;
    assert_logged(
        log_path,
        "capture_candidate_present",
        data["candidateCount"]
            .as_u64()
            .is_some_and(|count| count >= 1)
            && data["candidates"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
        json!({
            "candidateCount": data["candidateCount"],
            "suppressedCount": data["suppressedCount"],
        }),
    )?;

    let candidate = data["candidates"]
        .as_array()
        .and_then(|rows| rows.first())
        .ok_or_else(|| format!("capture report missing first candidate: {data}"))?;
    let tags = candidate["proposedFields"]["tags"]
        .as_array()
        .ok_or_else(|| format!("capture candidate tags must be an array: {candidate}"))?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    assert_logged(
        log_path,
        "capture_candidate_fields",
        candidate["dedupeStatus"]["status"].as_str() == Some("unique")
            && candidate["proposedFields"]["level"].as_str() == Some("procedural")
            && tags.contains("ambient-capture")
            && candidate["evidence"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
        json!({
            "candidateId": candidate["candidateId"],
            "dedupeStatus": candidate["dedupeStatus"]["status"],
            "level": candidate["proposedFields"]["level"],
            "tags": candidate["proposedFields"]["tags"],
            "evidenceCount": candidate["evidence"].as_array().map(Vec::len),
        }),
    )?;
    let accept_command = candidate["acceptCommand"].as_str().unwrap_or_default();
    let reject_command = candidate["rejectCommand"].as_str().unwrap_or_default();
    assert_logged(
        log_path,
        "capture_explicit_accept_reject_commands",
        accept_command.contains("ee review session")
            && accept_command.contains("ee curate accept")
            && reject_command.contains("ee review session")
            && reject_command.contains("ee curate reject"),
        json!({
            "acceptCommand": accept_command,
            "rejectCommand": reject_command,
        }),
    )
}

fn assert_usage_error(log_path: &Path, parsed: &Value) -> TestResult {
    let error = &parsed["error"];
    let message = error["message"].as_str().unwrap_or_default();
    let repair = error["repair"].as_str().unwrap_or_default();
    assert_logged(
        log_path,
        "capture_invalid_max_usage_error",
        parsed["schema"].as_str() == Some("ee.error.v2")
            && error["code"].as_str() == Some("usage")
            && message.contains("capture suggest --max")
            && message.contains("greater than zero")
            && repair.contains("ee capture suggest --help"),
        json!({
            "schema": parsed["schema"],
            "code": error["code"],
            "message": message,
            "repair": repair,
        }),
    )
}

fn assert_event_log(log_path: &Path) -> TestResult {
    let body = fs::read_to_string(log_path)
        .map_err(|error| format!("read structured log {}: {error}", log_path.display()))?;
    let events = body
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("structured log must be JSONL: {error}"))?;
    let phases = events
        .iter()
        .filter_map(|event| event["fields"]["phase"].as_str())
        .collect::<BTreeSet<_>>();
    for phase in [
        "setup",
        "seed",
        "act_from_session",
        "act_from_recent",
        "act_invalid_max",
    ] {
        if !phases.contains(phase) {
            return Err(format!("structured log missing phase {phase}: {body}"));
        }
    }
    let has_command_end = events
        .iter()
        .any(|event| event["kind"].as_str() == Some("command_end"));
    let has_assertions = events.iter().any(|event| {
        matches!(
            event["kind"].as_str(),
            Some("assert_ok") | Some("assert_fail")
        )
    });
    if !has_command_end || !has_assertions {
        return Err(format!(
            "structured log must include command_end and assertion events: {body}"
        ));
    }
    for event in events {
        if event["schema"].as_str() != Some("ee.test_event.v1") {
            return Err(format!("event has wrong schema: {event}"));
        }
    }
    Ok(())
}

#[test]
fn capture_suggest_real_binary_is_read_only_and_repairable() -> TestResult {
    let workspace = unique_workspace("read-only")?;
    let log_path = workspace.join("capture-suggest-events.jsonl");
    emit_note(
        &log_path,
        "setup",
        json!({
            "workspacePathHash": hash_bytes(workspace.display().to_string().as_bytes()),
            "binary": env!("CARGO_BIN_EXE_ee"),
        }),
    )?;
    init_workspace(&workspace, &log_path)?;
    let (database_path, workspace_id) = seed_capture_session(&workspace, &log_path)?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_owned())?;

    emit_note(
        &log_path,
        "act_from_session",
        json!({"cassSessionId": CASS_SESSION_ID}),
    )?;
    let (session_output, session_report) = run_ee_json(
        &log_path,
        &[
            "--workspace",
            workspace_arg,
            "--json",
            "capture",
            "suggest",
            "--from-session",
            CASS_SESSION_ID,
            "--max",
            "1",
            "--min-confidence",
            "0.50",
        ],
    )?;
    assert_logged(
        &log_path,
        "capture_from_session_exit_success",
        session_output.status.success(),
        json!({"status": session_output.status.code()}),
    )?;
    assert_capture_report(
        &log_path,
        &session_report,
        "from_session",
        Some(CASS_SESSION_ID),
    )?;

    emit_note(&log_path, "act_from_recent", json!({"max": 1}))?;
    let (recent_output, recent_report) = run_ee_json(
        &log_path,
        &[
            "--workspace",
            workspace_arg,
            "--json",
            "capture",
            "suggest",
            "--from-recent",
            "--max",
            "1",
        ],
    )?;
    assert_logged(
        &log_path,
        "capture_from_recent_exit_success",
        recent_output.status.success(),
        json!({"status": recent_output.status.code()}),
    )?;
    assert_capture_report(&log_path, &recent_report, "from_recent", None)?;

    emit_note(&log_path, "act_invalid_max", json!({"max": 0}))?;
    let (invalid_output, invalid_report) = run_ee_json(
        &log_path,
        &[
            "--workspace",
            workspace_arg,
            "--json",
            "capture",
            "suggest",
            "--from-session",
            CASS_SESSION_ID,
            "--max",
            "0",
        ],
    )?;
    assert_logged(
        &log_path,
        "capture_invalid_max_exits_nonzero",
        !invalid_output.status.success(),
        json!({"status": invalid_output.status.code()}),
    )?;
    assert_usage_error(&log_path, &invalid_report)?;

    let after_count = curation_candidate_count(&database_path, &workspace_id)?;
    assert_logged(
        &log_path,
        "capture_suggest_persists_no_candidates",
        after_count == 0,
        json!({
            "curationCandidateCountAfter": after_count,
            "databasePathHash": hash_bytes(database_path.display().to_string().as_bytes()),
        }),
    )?;
    assert_event_log(&log_path)
}
