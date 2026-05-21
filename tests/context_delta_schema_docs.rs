//! Static contract tests for the first context-delta documentation slice.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(path: &str) -> Result<String, String> {
    fs::read_to_string(repo_root().join(path)).map_err(|error| format!("read {path}: {error}"))
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string at {pointer}"))
}

fn required_fields_at(value: &Value, pointer: &str) -> Result<Vec<String>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing required array at {pointer}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{pointer} contains a non-string item"))
        })
        .collect()
}

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context} missing `{needle}`"))
    }
}

#[test]
fn context_delta_schema_pins_item_diff_and_token_budget_contract() -> TestResult {
    let schema_text = read_repo_file("docs/schemas/ee.context.delta.v1.json")?;
    let schema: Value =
        serde_json::from_str(&schema_text).map_err(|error| format!("schema parse: {error}"))?;

    if string_at(&schema, "/properties/schema/const")? != "ee.context.delta.v1" {
        return Err("schema const must be ee.context.delta.v1".to_owned());
    }

    let description = string_at(&schema, "/description")?;
    for needle in [
        "server verifies",
        "v1 changes are additive only",
        "ee.context.delta.v2",
        "rather than RFC 6902 JSON Patch",
    ] {
        ensure_contains(description, needle, "schema description")?;
    }

    let data_required = required_fields_at(&schema, "/$defs/contextDelta/required")?;
    for field in [
        "priorPackHash",
        "newPackHash",
        "items",
        "tokenSavings",
        "serverDecision",
    ] {
        if !data_required.iter().any(|candidate| candidate == field) {
            return Err(format!("contextDelta.required missing {field}"));
        }
    }

    let item_required = required_fields_at(&schema, "/$defs/itemDiff/required")?;
    for field in ["added", "removed", "modified"] {
        if !item_required.iter().any(|candidate| candidate == field) {
            return Err(format!("itemDiff.required missing {field}"));
        }
    }

    let token_required = required_fields_at(&schema, "/$defs/tokenSavings/required")?;
    for field in [
        "fullBytes",
        "deltaBytes",
        "savedBytes",
        "savedPercent",
        "netPackTokens",
    ] {
        if !token_required.iter().any(|candidate| candidate == field) {
            return Err(format!("tokenSavings.required missing {field}"));
        }
    }

    let server_decision_required = required_fields_at(
        &schema,
        "/$defs/contextDelta/properties/serverDecision/required",
    )?;
    for field in [
        "computedFromServerVerifiedPackRecord",
        "deltaChained",
        "format",
    ] {
        if !server_decision_required
            .iter()
            .any(|candidate| candidate == field)
        {
            return Err(format!("serverDecision.required missing {field}"));
        }
    }

    Ok(())
}

#[test]
fn context_delta_apply_guide_covers_agent_safety_rules() -> TestResult {
    let guide = read_repo_file("docs/agent-ux/context-delta-apply.md")?;
    let first_non_empty = guide
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .ok_or_else(|| "guide has no prose".to_owned())?;

    if first_non_empty != "Delta payloads add to your prompt; they do not replace the base pack." {
        return Err("guide must open with the base+delta prompt-budget warning".to_owned());
    }

    for needle in [
        "data.pack.hash",
        "same workspace",
        "The server never chains deltas",
        "data.tokenSavings.netPackTokens",
        "No-op deltas use empty arrays",
        "Delta v1 is JSON-only",
        "context_delta_format_unsupported",
        "should not add `ee context apply-delta",
        "context_delta_prior_unknown",
    ] {
        ensure_contains(&guide, needle, "context delta apply guide")?;
    }

    Ok(())
}
