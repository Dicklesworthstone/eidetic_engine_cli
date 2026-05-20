use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.prompt_budget_report.v1.json";
const TRACE_PATH: &str = "tests/fixtures/golden/perf_forensics/prompt_budget_trace.jsonl";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_json(path: &str) -> Result<Value, String> {
    let path = repo_root().join(path);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

#[test]
fn prompt_budget_report_schema_is_redaction_safe() -> TestResult {
    let schema = load_json(SCHEMA_PATH)?;

    if schema["properties"].get("rawQuery").is_some()
        || schema["properties"].get("rawMemoryBody").is_some()
        || schema["properties"].get("rawTask").is_some()
    {
        return Err(
            "prompt-budget report schema must not expose raw prompt/query/memory fields".into(),
        );
    }

    let categories = schema["$defs"]["wasteCategory"]["properties"]["category"]["enum"]
        .as_array()
        .ok_or("missing waste category enum")?;
    for required in [
        "repeated_context_bytes",
        "bulky_optional_json_fields",
        "redundant_search_why_context_sequence",
        "unchanged_pack_resent_full",
        "degraded_retry_no_output_change",
    ] {
        if !categories
            .iter()
            .any(|entry| entry.as_str() == Some(required))
        {
            return Err(format!("missing waste category {required}"));
        }
    }

    Ok(())
}

#[test]
fn prompt_budget_report_detects_pack_diet_waste() -> TestResult {
    let trace_path = repo_root().join(TRACE_PATH);
    let report = ee::core::perf_forensics::prompt_budget_report(&trace_path)
        .map_err(|error| error.to_string())?;

    if report.schema != "ee.prompt_budget_report.v1" {
        return Err(format!("unexpected schema: {}", report.schema));
    }
    if report.total_events != 4 {
        return Err(format!("expected 4 events, got {}", report.total_events));
    }
    if report.avoidable_bytes == 0 {
        return Err("expected non-zero avoidable bytes".into());
    }
    if report
        .top_waste_categories
        .iter()
        .all(|category| category.category != "unchanged_pack_resent_full")
    {
        return Err(format!(
            "expected unchanged pack resend category, got {:?}",
            report.top_waste_categories
        ));
    }
    if report
        .top_waste_categories
        .iter()
        .all(|category| category.category != "redundant_search_why_context_sequence")
    {
        return Err(format!(
            "expected redundant search/why/context category, got {:?}",
            report.top_waste_categories
        ));
    }
    if report
        .suggested_actions
        .iter()
        .all(|action| !action.command.contains("ee pack replay"))
    {
        return Err("expected pack replay diet recommendation".into());
    }

    let json = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    for forbidden in ["prepare release", "rawQuery", "rawMemoryBody", "rawTask"] {
        if json.contains(forbidden) {
            return Err(format!(
                "report leaked forbidden raw content marker {forbidden}"
            ));
        }
    }

    Ok(())
}
