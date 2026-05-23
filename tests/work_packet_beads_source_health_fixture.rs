//! Golden contract: Beads timeout/no-output source health (bd-2z5ly.9.2).
//!
//! This fixture pins the swarm work-packet behavior for a crowded checkout
//! where Beads commands produce no output before a bounded timeout while stale
//! fallback rows are still available. The packet must not treat those fallback
//! rows as fresh claim authority.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/swarm_work_packet/beads_command_timeout_no_output.json")
}

fn read_fixture() -> Result<Value, String> {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("read fixture {}: {error}", path.display()))?;
    serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("parse fixture {}: {error}", path.display()))
}

fn data(value: &Value) -> Result<&Value, String> {
    value
        .get("data")
        .ok_or_else(|| "fixture missing data object".to_owned())
}

fn string_array_at<'a>(value: &'a Value, pointer: &str) -> Result<Vec<&'a str>, String> {
    let array = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{pointer} is not an array"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .ok_or_else(|| format!("{pointer} contains a non-string item: {item}"))
        })
        .collect()
}

fn flatten_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                flatten_strings(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                flatten_strings(item, out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[test]
fn beads_timeout_fixture_marks_source_degraded_and_stale() -> TestResult {
    let fixture = read_fixture()?;
    let data = data(&fixture)?;

    if fixture["schema"] != "ee.response.v2" {
        return Err(format!("wrong response schema: {}", fixture["schema"]));
    }
    if data["schema"] != "ee.swarm.work_packet.v1" {
        return Err(format!("wrong packet schema: {}", data["schema"]));
    }
    if data["beadsSourceHealth"]["status"] != "timeout_no_output" {
        return Err(format!(
            "expected timeout_no_output source health, got {}",
            data["beadsSourceHealth"]["status"]
        ));
    }
    if data["beadsSourceHealth"]["fallbackRowsAuthoritative"] != false {
        return Err("stale fallback rows must not be authoritative".to_owned());
    }

    let source = data["sourceProvenance"]
        .as_array()
        .and_then(|sources| sources.iter().find(|source| source["source"] == "beads"))
        .ok_or_else(|| "missing Beads source provenance".to_owned())?;
    if source["status"] != "degraded" || source["freshness"] != "stale_fallback" {
        return Err(format!(
            "Beads source should be degraded/stale_fallback, got {source}"
        ));
    }
    Ok(())
}

#[test]
fn stale_fallback_never_recommends_autonomous_claim() -> TestResult {
    let fixture = read_fixture()?;
    let data = data(&fixture)?;

    if data["safeToClaim"] != false {
        return Err("fixture must not be safe to claim".to_owned());
    }
    if data["candidateLane"]["decision"] != "blocked" {
        return Err(format!(
            "candidate decision must be blocked, got {}",
            data["candidateLane"]["decision"]
        ));
    }
    if data["candidateLane"]["status"] != "in_progress" {
        return Err("fallback row should model an already-owned lane".to_owned());
    }

    let reasons = string_array_at(data, "/candidateLane/decisionReasons")?;
    for expected in [
        "source_health:beads_timeout_no_output",
        "fallback_row_already_owned",
        "fallback_rows_not_authoritative",
        "agent_mail_reservation_authority_unknown",
    ] {
        if !reasons.contains(&expected) {
            return Err(format!("missing decision reason {expected}: {reasons:?}"));
        }
    }

    let mut strings = Vec::new();
    flatten_strings(data, &mut strings);
    for text in strings {
        if text.contains("br update") && !text.contains("do not run br update") {
            return Err(format!("fixture recommends br update: {text}"));
        }
        if text.contains("br claim") && !text.contains("do not run br update / br claim") {
            return Err(format!("fixture recommends br claim: {text}"));
        }
        if text.contains("--status in_progress") {
            return Err(format!("fixture contains autonomous claim flags: {text}"));
        }
    }
    Ok(())
}

#[test]
fn recovery_actions_are_stable_and_inspection_only() -> TestResult {
    let fixture = read_fixture()?;
    let actions = data(&fixture)?
        .get("requiredActions")
        .and_then(Value::as_array)
        .ok_or_else(|| "requiredActions is not an array".to_owned())?;
    let kinds = actions
        .iter()
        .map(|action| {
            action["kind"]
                .as_str()
                .ok_or_else(|| format!("action kind is not a string: {action}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if kinds != ["bounded_retry", "doctor_probe", "manual_coordination"] {
        return Err(format!("unexpected recovery action order: {kinds:?}"));
    }

    let commands = actions
        .iter()
        .filter_map(|action| action["command"].as_str())
        .collect::<Vec<_>>();
    if !commands.contains(&"br doctor --json") {
        return Err(format!("missing br doctor probe in {commands:?}"));
    }
    if !commands.contains(&"br --no-auto-import --allow-stale ready --json") {
        return Err(format!(
            "missing stale-safe read-only fallback in {commands:?}"
        ));
    }
    Ok(())
}

#[test]
fn fixture_is_scrubbed_and_byte_stable() -> TestResult {
    let first = read_fixture()?;
    let second = read_fixture()?;
    let first_json =
        serde_json::to_string(&first).map_err(|error| format!("serialize first: {error}"))?;
    let second_json =
        serde_json::to_string(&second).map_err(|error| format!("serialize second: {error}"))?;
    if first_json != second_json {
        return Err("fixture serialization is not byte-stable".to_owned());
    }

    let lower = first_json.to_ascii_lowercase();
    for forbidden in [
        "/users/jemanuel",
        "\"pid\"",
        "raw stdout",
        "raw stderr",
        "mail body",
        "file content",
    ] {
        if lower.contains(forbidden) {
            return Err(format!("fixture leaked forbidden detail: {forbidden}"));
        }
    }
    Ok(())
}
