//! Static contract tests for the first context-delta documentation slice.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use eidetic_engine_cli::core::context_delta::{
    ContextDeltaItemSnapshot, ContextDeltaOptions, ContextDeltaPackSnapshot, compute_context_delta,
};

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

/// Returns the JSON-Pointer-style key names a schema permits on a
/// closed `additionalProperties: false` object at the given pointer.
fn schema_property_names(schema: &Value, properties_pointer: &str) -> Result<Vec<String>, String> {
    schema
        .pointer(properties_pointer)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("missing properties at {properties_pointer}"))?
        .keys()
        .map(|name| Ok::<_, String>(name.clone()))
        .collect()
}

/// Builds a representative envelope using only the public compute API,
/// then asserts every serialized key matches the v1 schema's closed
/// property set. This is the contract test the original review finding
/// said was missing: it would have caught the prior `{schema, …}` flat
/// envelope, the missing `serverDecision`, and the `{old, new}` object
/// field-change shape immediately.
#[test]
fn context_delta_rust_envelope_matches_schema_property_set() -> TestResult {
    let schema_text = read_repo_file("docs/schemas/ee.context.delta.v1.json")?;
    let schema: Value =
        serde_json::from_str(&schema_text).map_err(|error| format!("schema parse: {error}"))?;

    let prior_item = ContextDeltaItemSnapshot::new("mem_a")
        .with_field("contentHash", Value::String("old".to_string()))
        .with_field("estimatedTokens", serde_json::json!(10));
    let new_item = ContextDeltaItemSnapshot::new("mem_a")
        .with_field("contentHash", Value::String("new".to_string()))
        .with_field("estimatedTokens", serde_json::json!(12));
    let prior = ContextDeltaPackSnapshot::new("h1", 1, 1024, 320, vec![prior_item]);
    let new = ContextDeltaPackSnapshot::new("h2", 2, 1100, 360, vec![new_item]);
    let envelope = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
        .map_err(|error| format!("compute_context_delta: {error}"))?;
    let serialized =
        serde_json::to_value(&envelope).map_err(|error| format!("serialize envelope: {error}"))?;

    let envelope_object = serialized
        .as_object()
        .ok_or_else(|| "envelope must serialize as a JSON object".to_string())?;
    let envelope_keys = envelope_object
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_envelope = schema_property_names(&schema, "/properties")?;
    for key in &envelope_keys {
        if !allowed_envelope.iter().any(|allowed| allowed == key) {
            return Err(format!(
                "envelope key `{key}` is not in the v1 schema property set ({allowed_envelope:?})"
            ));
        }
    }
    for required in ["schema", "success", "data", "degraded"] {
        if !envelope_keys.contains(required) {
            return Err(format!("envelope missing required key `{required}`"));
        }
    }

    let data_object = serialized
        .pointer("/data")
        .and_then(Value::as_object)
        .ok_or_else(|| "data must be an object".to_string())?;
    let data_keys = data_object
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_data = schema_property_names(&schema, "/$defs/contextDelta/properties")?;
    for key in &data_keys {
        if !allowed_data.iter().any(|allowed| allowed == key) {
            return Err(format!(
                "data key `{key}` is not in the v1 contextDelta property set ({allowed_data:?})"
            ));
        }
    }
    for required in [
        "priorPackHash",
        "newPackHash",
        "items",
        "tokenSavings",
        "serverDecision",
    ] {
        if !data_keys.contains(required) {
            return Err(format!("data missing required key `{required}`"));
        }
    }

    let server_decision = serialized
        .pointer("/data/serverDecision")
        .and_then(Value::as_object)
        .ok_or_else(|| "serverDecision must be an object".to_string())?;
    let server_keys = server_decision
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_server = schema_property_names(
        &schema,
        "/$defs/contextDelta/properties/serverDecision/properties",
    )?;
    for key in &server_keys {
        if !allowed_server.iter().any(|allowed| allowed == key) {
            return Err(format!(
                "serverDecision key `{key}` is not in the v1 schema property set ({allowed_server:?})"
            ));
        }
    }
    for required in [
        "computedFromServerVerifiedPackRecord",
        "deltaChained",
        "format",
    ] {
        if !server_keys.contains(required) {
            return Err(format!("serverDecision missing required key `{required}`"));
        }
    }

    let field_change = serialized
        .pointer("/data/items/modified/0/fieldChanges/contentHash")
        .ok_or_else(|| "modified item field change missing".to_string())?;
    let pair = field_change.as_array().ok_or_else(|| {
        format!("ordinary field change must serialize as a JSON array, got {field_change}")
    })?;
    if pair.len() != 2 {
        return Err(format!(
            "ordinary field change must be a two-element [old, new] array; got {pair:?}"
        ));
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
