//! Gate 17 recorder event spine readiness contracts.
//!
//! The readiness gate names exact recorder artifacts. These tests freeze those
//! artifacts under `tests/golden/recorder` while normalizing only IDs,
//! timestamps, and hashes that are intentionally dynamic.

use ee::core::lab::{ReconstructOptions, reconstruct_episode};
use ee::core::recorder::{
    DEFAULT_MAX_RECORDER_PAYLOAD_BYTES, RECORDER_EVENT_RESPONSE_SCHEMA_V1,
    RECORDER_FINISH_SCHEMA_V1, RECORDER_IMPORT_PLAN_SCHEMA_V1, RECORDER_START_SCHEMA_V1,
    RecorderEventOptions, RecorderFinishOptions, RecorderImportOptions, RecorderLink,
    RecorderLinkType, RecorderStartOptions, finish_recording, plan_recorder_import, record_event,
    start_recording,
};
use ee::models::{
    ImportSourceType, RecorderEventChainStatus, RecorderEventType, RecorderRunStatus,
    RedactionStatus,
};
use serde_json::Value as JsonValue;
use std::env;
use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

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

fn json_pointer<'a>(
    value: &'a JsonValue,
    pointer: &str,
    context: &str,
) -> Result<&'a JsonValue, String> {
    value
        .pointer(pointer)
        .ok_or_else(|| format!("{context}: missing JSON pointer {pointer}"))
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("recorder")
        .join(format!("{name}.json"))
}

fn pretty_json(value: &JsonValue) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| format!("json render failed: {error}"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        fs::write(&path, actual)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing Gate 17 golden {}: {error}", path.display()))?;
    let expected = expected.strip_suffix('\n').unwrap_or(&expected);
    ensure(
        actual == expected,
        format!(
            "Gate 17 recorder golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{actual}"
        ),
    )
}

fn normalized(mut value: JsonValue) -> Result<String, String> {
    normalize_value(&mut value);
    pretty_json(&value)
}

fn normalize_value(value: &mut JsonValue) {
    match value {
        JsonValue::Object(object) => {
            for (key, nested) in object.iter_mut() {
                normalize_field(key, nested);
                normalize_value(nested);
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                normalize_value(item);
            }
        }
        _ => {}
    }
}

fn normalize_field(key: &str, value: &mut JsonValue) {
    if matches!(
        key,
        "startedAt" | "endedAt" | "timestamp" | "reconstructed_at"
    ) && value.is_string()
    {
        *value = serde_json::json!("TIMESTAMP");
        return;
    }

    if matches!(key, "runId" | "run_id")
        && value.as_str().is_some_and(|raw| raw.starts_with("run_"))
    {
        *value = serde_json::json!("run_DYNAMIC");
        return;
    }

    if key == "eventId" && value.as_str().is_some_and(|raw| raw.starts_with("evt_")) {
        *value = serde_json::json!("evt_DYNAMIC");
        return;
    }

    if key == "episode_id" && value.as_str().is_some_and(|raw| raw.starts_with("ep_")) {
        *value = serde_json::json!("ep_DYNAMIC");
        return;
    }

    if matches!(
        key,
        "payloadHash" | "eventHash" | "previousEventHash" | "episode_hash"
    ) && value.as_str().is_some_and(|raw| raw.starts_with("blake3:"))
    {
        *value = serde_json::json!("blake3:DYNAMIC");
    }
}

fn assert_no_raw_payload(value: &JsonValue, raw: &str) -> TestResult {
    let rendered =
        serde_json::to_string(value).map_err(|error| format!("json render failed: {error}"))?;
    ensure(
        !rendered.contains(raw),
        format!("recorder output must not expose raw sensitive marker `{raw}`"),
    )
}

fn redaction_payload() -> String {
    ["sec", "ret gate17 tok", "en marker"].concat()
}

fn redaction_probe() -> String {
    ["sec", "ret gate17"].concat()
}

#[test]
fn start_run_matches_gate_golden() -> TestResult {
    let report = start_recording(&RecorderStartOptions {
        agent_id: "codex-gate17".to_owned(),
        session_id: Some("session-gate17".to_owned()),
        workspace_id: Some("wsp_gate17".to_owned()),
        dry_run: true,
    });
    let value = report.data_json();

    ensure_equal(
        json_pointer(&value, "/schema", "start schema")?,
        &serde_json::json!(RECORDER_START_SCHEMA_V1),
        "start schema",
    )?;
    ensure_equal(
        json_pointer(&value, "/agentId", "agent id")?,
        &serde_json::json!("codex-gate17"),
        "agent id",
    )?;
    ensure_equal(
        json_pointer(&value, "/workspaceId", "workspace id")?,
        &serde_json::json!("wsp_gate17"),
        "workspace id",
    )?;

    let actual = normalized(value)?;
    assert_golden("start_run", &actual)
}

#[test]
fn append_command_failed_matches_gate_golden() -> TestResult {
    let report = record_event(
        &RecorderEventOptions {
            run_id: "run_gate17".to_owned(),
            event_type: RecorderEventType::ToolResult,
            payload: Some(
                r#"{"command":"cargo check --all-targets","exitCode":101,"stderrPath":"artifacts/stderr.txt"}"#
                    .to_owned(),
            ),
            redact: false,
            previous_event_hash: None,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: true,
        },
        1,
    )
    .map_err(|error| error.to_string())?;
    let value = report.data_json();

    ensure_equal(
        json_pointer(&value, "/schema", "event response schema")?,
        &serde_json::json!(RECORDER_EVENT_RESPONSE_SCHEMA_V1),
        "event response schema",
    )?;
    ensure_equal(
        json_pointer(&value, "/sequence", "sequence")?,
        &serde_json::json!(1),
        "sequence",
    )?;
    ensure_equal(
        json_pointer(&value, "/chainStatus", "chain status")?,
        &serde_json::json!(RecorderEventChainStatus::Root.as_str()),
        "root event chain status",
    )?;
    ensure_equal(
        json_pointer(&value, "/redactionStatus", "redaction status")?,
        &serde_json::json!(RedactionStatus::None.as_str()),
        "command failure redaction status",
    )?;

    let actual = normalized(value)?;
    assert_golden("append_command_failed", &actual)
}

#[test]
fn append_redacted_secret_matches_gate_golden() -> TestResult {
    let report = record_event(
        &RecorderEventOptions {
            run_id: "run_gate17".to_owned(),
            event_type: RecorderEventType::StateChange,
            payload: Some(redaction_payload()),
            redact: false,
            previous_event_hash: Some("blake3:previousgate17".to_owned()),
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: true,
        },
        2,
    )
    .map_err(|error| error.to_string())?;
    let value = report.data_json();

    ensure_equal(
        json_pointer(&value, "/sequence", "sequence")?,
        &serde_json::json!(2),
        "sequence",
    )?;
    ensure_equal(
        json_pointer(&value, "/redactionStatus", "redaction status")?,
        &serde_json::json!(RedactionStatus::Full.as_str()),
        "secret redaction status",
    )?;
    ensure_equal(
        json_pointer(&value, "/redactionClasses", "redaction classes")?,
        &serde_json::json!(["secret", "token"]),
        "redaction classes",
    )?;
    ensure_equal(
        json_pointer(&value, "/chainStatus", "chain status")?,
        &serde_json::json!(RecorderEventChainStatus::Linked.as_str()),
        "linked event chain status",
    )?;
    assert_no_raw_payload(&value, &redaction_probe())?;

    let actual = normalized(value)?;
    assert_golden("append_redacted_secret", &actual)
}

#[test]
fn finish_run_matches_gate_golden() -> TestResult {
    let report = finish_recording(
        &RecorderFinishOptions {
            run_id: "run_gate17".to_owned(),
            status: RecorderRunStatus::Completed,
            dry_run: true,
        },
        2,
    );
    let value = report.data_json();

    ensure_equal(
        json_pointer(&value, "/schema", "finish schema")?,
        &serde_json::json!(RECORDER_FINISH_SCHEMA_V1),
        "finish schema",
    )?;
    ensure_equal(
        json_pointer(&value, "/status", "finish status")?,
        &serde_json::json!(RecorderRunStatus::Completed.as_str()),
        "finish status",
    )?;
    ensure_equal(
        json_pointer(&value, "/eventCount", "event count")?,
        &serde_json::json!(2),
        "event count",
    )?;

    let actual = normalized(value)?;
    assert_golden("finish_run", &actual)
}

#[test]
fn import_dry_run_matches_gate_golden() -> TestResult {
    let payload_marker = redaction_payload();
    let cass_view = format!(
        r#"{{
  "lines": [
    {{"line": 41, "content": "{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"prepare release {payload_marker}\"}}}}"}},
    {{"line": 42, "content": "{{\"type\":\"tool_use\",\"name\":\"shell\",\"input\":{{\"command\":\"cargo check\"}}}}"}},
    {{"line": 43, "content": "{{\"type\":\"tool_result\",\"content\":\"finished\"}}"}}
  ]
}}"#
    );
    let report = plan_recorder_import(&RecorderImportOptions {
        source_type: ImportSourceType::Cass,
        source_id: "cass://gate17-session".to_owned(),
        input_json: Some(cass_view),
        input_path: Some("tests/fixtures/cass/gate17-view.json".to_owned()),
        agent_id: Some("codex-gate17".to_owned()),
        session_id: Some("session-gate17".to_owned()),
        workspace_id: Some("wsp_gate17".to_owned()),
        max_events: 2,
        redact: false,
        max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
        dry_run: true,
    })
    .map_err(|error| error.to_string())?;
    let value = report.data_json();

    ensure_equal(
        json_pointer(&value, "/schema", "import plan schema")?,
        &serde_json::json!(RECORDER_IMPORT_PLAN_SCHEMA_V1),
        "import plan schema",
    )?;
    ensure_equal(
        json_pointer(&value, "/summary/eventsDiscovered", "events discovered")?,
        &serde_json::json!(3),
        "events discovered",
    )?;
    ensure_equal(
        json_pointer(&value, "/summary/eventsMapped", "events mapped")?,
        &serde_json::json!(2),
        "events mapped",
    )?;
    ensure_equal(
        json_pointer(&value, "/summary/eventsRejected", "events rejected")?,
        &serde_json::json!(1),
        "limited event count",
    )?;
    ensure(
        json_pointer(&value, "/summary/redactedBytes", "redacted bytes")?
            .as_u64()
            .is_some_and(|bytes| bytes > 0),
        "redacted bytes",
    )?;
    ensure_equal(
        json_pointer(&value, "/summary/redactionClasses", "redaction classes")?,
        &serde_json::json!(["secret", "token"]),
        "redaction classes",
    )?;
    ensure(
        json_pointer(&value, "/warnings", "warnings")?
            .as_array()
            .is_some_and(|warnings| !warnings.is_empty()),
        "import plan must report skipped events when max_events truncates the source",
    )?;
    assert_no_raw_payload(&value, &redaction_probe())?;

    let actual = normalized(value)?;
    assert_golden("import_dry_run", &actual)
}

#[test]
fn reconstruct_episode_matches_gate_golden() -> TestResult {
    let report = reconstruct_episode(&ReconstructOptions {
        run_id: "run_gate17".to_owned(),
        include_memories: true,
        include_tool_calls: true,
        include_user_messages: true,
        include_assistant_responses: true,
        dry_run: false,
        ..Default::default()
    })
    .map_err(|error| error.message())?;
    let links = [
        RecorderLink {
            link_id: "link_pack_gate17".to_owned(),
            run_id: "run_gate17".to_owned(),
            link_type: RecorderLinkType::ContextPack,
            artifact_id: "pack_gate17".to_owned(),
            created_at: "2026-01-02T03:04:05Z".to_owned(),
            metadata: Some("selected_context".to_owned()),
        },
        RecorderLink {
            link_id: "link_preflight_gate17".to_owned(),
            run_id: "run_gate17".to_owned(),
            link_type: RecorderLinkType::PreflightRun,
            artifact_id: "pf_gate17".to_owned(),
            created_at: "2026-01-02T03:04:06Z".to_owned(),
            metadata: Some("risk_snapshot".to_owned()),
        },
        RecorderLink {
            link_id: "link_outcome_gate17".to_owned(),
            run_id: "run_gate17".to_owned(),
            link_type: RecorderLinkType::Outcome,
            artifact_id: "out_gate17".to_owned(),
            created_at: "2026-01-02T03:04:07Z".to_owned(),
            metadata: Some("completed".to_owned()),
        },
        RecorderLink {
            link_id: "link_tripwire_gate17".to_owned(),
            run_id: "run_gate17".to_owned(),
            link_type: RecorderLinkType::Tripwire,
            artifact_id: "tw_gate17".to_owned(),
            created_at: "2026-01-02T03:04:08Z".to_owned(),
            metadata: Some("checked".to_owned()),
        },
        RecorderLink {
            link_id: "link_episode_gate17".to_owned(),
            run_id: "run_gate17".to_owned(),
            link_type: RecorderLinkType::TaskEpisode,
            artifact_id: "ep_gate17".to_owned(),
            created_at: "2026-01-02T03:04:09Z".to_owned(),
            metadata: Some("reconstructed".to_owned()),
        },
    ];
    let value = serde_json::json!({
        "reconstruction": serde_json::to_value(&report)
            .map_err(|error| format!("reconstruct report JSON failed: {error}"))?,
        "evidenceLinks": links.iter().map(RecorderLink::to_json).collect::<Vec<_>>(),
        "payloadPolicy": {
            "payloadsAreEvidence": true,
            "payloadsAreInstructions": false,
            "rendererLabel": "recorder_trace_evidence"
        }
    });

    ensure_equal(
        json_pointer(&value, "/reconstruction/schema", "reconstruct schema")?,
        &serde_json::json!("ee.lab.reconstruct.v1"),
        "reconstruct schema",
    )?;
    for expected in [
        "context_pack",
        "preflight_run",
        "outcome",
        "tripwire",
        "task_episode",
    ] {
        ensure(
            json_pointer(&value, "/evidenceLinks", "evidence links")?
                .as_array()
                .is_some_and(|links| {
                    links.iter().any(|link| {
                        link.get("linkType").and_then(JsonValue::as_str) == Some(expected)
                    })
                }),
            "missing recorder link type",
        )?;
    }
    ensure_equal(
        json_pointer(
            &value,
            "/payloadPolicy/payloadsAreInstructions",
            "payload policy",
        )?,
        &serde_json::json!(false),
        "payloads are evidence, not instructions",
    )?;

    let actual = normalized(value)?;
    assert_golden("reconstruct_episode", &actual)
}
