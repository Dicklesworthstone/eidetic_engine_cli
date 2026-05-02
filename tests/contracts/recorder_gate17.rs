//! Gate 17 recorder event spine contract coverage.
//!
//! Freezes the public JSON shape for recorder events, import plans, tail
//! snapshots, and JSONL follow events. The fixtures are deterministic so the
//! gate protects append-only provenance, redaction accounting, and hash-chain
//! fields without depending on wall-clock IDs.

use ee::core::recorder::{
    RECORDER_EVENT_RESPONSE_SCHEMA_V1, RECORDER_IMPORT_PLAN_SCHEMA_V1,
    RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1, RECORDER_TAIL_SCHEMA_V1, RecorderEventReport,
    RecorderEventSummary, RecorderImportEventPlan, RecorderImportPlanReport,
    RecorderTailFollowEvent, RecorderTailReport,
};
use ee::models::{ImportSourceType, RecorderEventChainStatus, RecorderEventType, RedactionStatus};
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

fn ensure_json_equal(actual: Option<&JsonValue>, expected: JsonValue, context: &str) -> TestResult {
    let actual = actual.ok_or_else(|| format!("{context}: missing JSON field"))?;
    ensure_equal(actual, &expected, context)
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("recorder")
        .join(format!("{name}.json.golden"))
}

fn pretty_json(value: &JsonValue) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| format!("json render failed: {error}"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("failed to create golden dir {}: {error}", parent.display())
            })?;
        }
        fs::write(&path, actual)
            .map_err(|error| format!("failed to write golden {}: {error}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    let expected = expected.strip_suffix('\n').unwrap_or(&expected);
    ensure(
        actual == expected,
        format!(
            "recorder golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{actual}"
        ),
    )
}

fn recorder_event_report_fixture() -> RecorderEventReport {
    RecorderEventReport {
        schema: RECORDER_EVENT_RESPONSE_SCHEMA_V1,
        event_id: "evt_gate17_tool_result".to_string(),
        run_id: "run_gate17".to_string(),
        sequence: 2,
        event_type: RecorderEventType::ToolResult,
        timestamp: "2026-01-02T03:04:05Z".to_string(),
        payload_hash: Some("blake3:payloadhashgate17".to_string()),
        payload_bytes: 48,
        payload_accepted: true,
        redaction_status: RedactionStatus::Full,
        redaction_classes: vec!["api_key".to_string(), "token".to_string()],
        placeholder_count: 2,
        redacted_bytes: 48,
        previous_event_hash: Some("blake3:previousgate17".to_string()),
        event_hash: "blake3:eventhashgate17".to_string(),
        chain_status: RecorderEventChainStatus::Linked,
        dry_run: true,
    }
}

fn recorder_import_plan_fixture() -> RecorderImportPlanReport {
    RecorderImportPlanReport {
        schema: RECORDER_IMPORT_PLAN_SCHEMA_V1,
        source_type: ImportSourceType::Cass,
        source_id: "cass://gate17-session".to_string(),
        input_path: Some("fixtures/cass/gate17-view.json".to_string()),
        connector: "cass_view_json",
        run_id: "run_gate17_import".to_string(),
        agent_id: "codex-gate17".to_string(),
        session_id: Some("cass-gate17".to_string()),
        workspace_id: Some("wsp_gate17".to_string()),
        started_at: "1970-01-01T00:00:00Z".to_string(),
        ended_at: None,
        dry_run: true,
        events_discovered: 2,
        events_mapped: 2,
        events_rejected: 0,
        payload_bytes: 96,
        redacted_count: 1,
        redacted_bytes: 48,
        redaction_classes: vec!["token".to_string()],
        chain_complete: true,
        events: vec![
            RecorderImportEventPlan {
                action: "would_record",
                source_span_id: "cass://gate17-session:41".to_string(),
                source_line_start: 41,
                source_line_end: 41,
                event_id: "evt_gate17_import_0001".to_string(),
                sequence: 1,
                event_type: RecorderEventType::UserMessage,
                timestamp: "1970-01-01T00:00:00Z".to_string(),
                payload_hash: Some("blake3:userpayloadgate17".to_string()),
                payload_bytes: 48,
                redaction_status: RedactionStatus::Full,
                redaction_classes: vec!["token".to_string()],
                redacted_bytes: 48,
                previous_event_hash: None,
                event_hash: "blake3:importeventhash0001".to_string(),
                chain_status: RecorderEventChainStatus::Root,
            },
            RecorderImportEventPlan {
                action: "would_record",
                source_span_id: "cass://gate17-session:42".to_string(),
                source_line_start: 42,
                source_line_end: 42,
                event_id: "evt_gate17_import_0002".to_string(),
                sequence: 2,
                event_type: RecorderEventType::ToolCall,
                timestamp: "1970-01-01T00:00:00Z".to_string(),
                payload_hash: Some("blake3:toolpayloadgate17".to_string()),
                payload_bytes: 48,
                redaction_status: RedactionStatus::None,
                redaction_classes: Vec::new(),
                redacted_bytes: 0,
                previous_event_hash: Some("blake3:importeventhash0001".to_string()),
                event_hash: "blake3:importeventhash0002".to_string(),
                chain_status: RecorderEventChainStatus::Linked,
            },
        ],
        warnings: Vec::new(),
    }
}

fn recorder_tail_report_fixture() -> RecorderTailReport {
    RecorderTailReport {
        schema: RECORDER_TAIL_SCHEMA_V1,
        run_id: "run_gate17".to_string(),
        events: vec![
            RecorderEventSummary {
                event_id: "evt_gate17_0001".to_string(),
                sequence: 1,
                event_type: RecorderEventType::UserMessage,
                timestamp: "2026-01-02T03:04:04Z".to_string(),
                redacted: true,
            },
            RecorderEventSummary {
                event_id: "evt_gate17_0002".to_string(),
                sequence: 2,
                event_type: RecorderEventType::ToolResult,
                timestamp: "2026-01-02T03:04:05Z".to_string(),
                redacted: false,
            },
        ],
        total_events: 2,
        has_more: false,
    }
}

#[test]
fn gate17_recorder_event_response_matches_golden() -> TestResult {
    let report = recorder_event_report_fixture();
    let value = report.data_json();
    let rendered = pretty_json(&value)?;

    ensure_json_equal(
        value.get("schema"),
        serde_json::json!(RECORDER_EVENT_RESPONSE_SCHEMA_V1),
        "event response schema",
    )?;
    ensure_json_equal(
        value.get("chainStatus"),
        serde_json::json!("linked"),
        "event chain status",
    )?;
    ensure(
        !rendered.contains("gate17 raw payload marker"),
        "event output must not expose raw payload text",
    )?;
    assert_golden("gate17_event_response", &rendered)
}

#[test]
fn gate17_recorder_import_plan_matches_golden() -> TestResult {
    let report = recorder_import_plan_fixture();
    let value = report.data_json();
    let rendered = pretty_json(&value)?;

    ensure_json_equal(
        value.get("schema"),
        serde_json::json!(RECORDER_IMPORT_PLAN_SCHEMA_V1),
        "import plan schema",
    )?;
    ensure_json_equal(
        value
            .get("summary")
            .and_then(|summary| summary.get("chainComplete")),
        serde_json::json!(true),
        "import chain complete",
    )?;
    ensure_json_equal(
        value
            .get("events")
            .and_then(JsonValue::as_array)
            .and_then(|events| events.first())
            .and_then(|event| event.get("redactionStatus")),
        serde_json::json!("full"),
        "first import event redacted",
    )?;
    ensure(
        !rendered.contains("token gate17"),
        "import plan must not expose raw CASS payload text",
    )?;
    assert_golden("gate17_import_plan", &rendered)
}

#[test]
fn gate17_recorder_tail_and_follow_outputs_match_golden() -> TestResult {
    let tail = recorder_tail_report_fixture();
    let follow = RecorderTailFollowEvent {
        schema: RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1,
        run_id: "run_gate17".to_string(),
        event_id: "evt_gate17_0003".to_string(),
        sequence: 3,
        event_type: RecorderEventType::StateChange,
        timestamp: "2026-01-02T03:04:06Z".to_string(),
        redacted: true,
        payload_preview: Some("[redacted:token]".to_string()),
    };
    let follow_json: JsonValue = serde_json::from_str(&follow.to_jsonl())
        .map_err(|error| format!("follow JSONL was not JSON: {error}"))?;
    let combined = serde_json::json!({
        "tail": tail.data_json(),
        "followLine": follow_json,
    });
    let rendered = pretty_json(&combined)?;

    ensure_json_equal(
        combined.get("tail").and_then(|tail| tail.get("schema")),
        serde_json::json!(RECORDER_TAIL_SCHEMA_V1),
        "tail schema",
    )?;
    ensure_json_equal(
        combined
            .get("followLine")
            .and_then(|follow_line| follow_line.get("schema")),
        serde_json::json!(RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1),
        "follow line schema",
    )?;
    assert_golden("gate17_tail_and_follow", &rendered)
}
