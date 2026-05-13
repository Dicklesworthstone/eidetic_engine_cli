//! D5 field-selector contract tests (bd-17c65.4.5).
//!
//! The implementation and docs/schemas field_presets blocks must stay in
//! lock-step. These tests use a compact matrix fixture so every documented
//! surface is checked across all four presets.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use ee::output::{
    FieldProfile, FieldSelector, apply_field_selector_to_json, error_response_json,
    field_preset_names_for_command,
};
use serde_json::{Map, Value, json};

type TestResult = Result<(), String>;

const PRESETS: &[(&str, FieldProfile)] = &[
    ("minimal", FieldProfile::Minimal),
    ("summary", FieldProfile::Summary),
    ("standard", FieldProfile::Standard),
    ("full", FieldProfile::Full),
];

#[derive(Debug)]
struct Surface {
    surface: String,
    command: String,
    schema_file: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path(relative: &str) -> PathBuf {
    repo_root().join("tests").join("fixtures").join(relative)
}

fn schema_path(file_name: &str) -> PathBuf {
    repo_root().join("docs").join("schemas").join(file_name)
}

fn read_json(path: PathBuf) -> Result<Value, String> {
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn matrix_surfaces() -> Result<Vec<Surface>, String> {
    let matrix = read_json(fixture_path("field_presets/matrix.json"))?;
    let surfaces = matrix
        .get("surfaces")
        .and_then(Value::as_array)
        .ok_or("matrix missing surfaces array")?;
    surfaces
        .iter()
        .map(|surface| {
            Ok(Surface {
                surface: required_str(surface, "surface")?.to_string(),
                command: required_str(surface, "command")?.to_string(),
                schema_file: required_str(surface, "schema_file")?.to_string(),
            })
        })
        .collect()
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {key} in {value}"))
}

fn schema_preset_fields(schema: &Value, preset: &str) -> Result<Vec<String>, String> {
    schema
        .pointer(&format!("/field_presets/{preset}"))
        .and_then(Value::as_array)
        .ok_or_else(|| format!("schema missing field_presets.{preset}"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("field_presets.{preset} contains non-string {value}"))
        })
        .collect()
}

#[test]
fn schema_field_presets_match_runtime_matrix() -> TestResult {
    let surfaces = matrix_surfaces()?;
    let expected_rows = surfaces.len() * PRESETS.len();
    let mut checked_rows = 0_usize;

    for surface in surfaces {
        let schema = read_json(schema_path(&surface.schema_file))?;
        for (preset_name, profile) in PRESETS {
            let schema_fields = schema_preset_fields(&schema, preset_name)?;
            let runtime_fields = field_preset_names_for_command(&surface.command, *profile)
                .iter()
                .map(|field| (*field).to_string())
                .collect::<Vec<_>>();
            if schema_fields != runtime_fields {
                return Err(format!(
                    "{} {preset_name}: docs/schemas/{} field_presets differ from runtime: schema={schema_fields:?} runtime={runtime_fields:?}",
                    surface.surface, surface.schema_file
                ));
            }
            checked_rows += 1;
        }
    }

    if checked_rows == expected_rows && checked_rows >= 40 {
        Ok(())
    } else {
        Err(format!(
            "field selector matrix checked {checked_rows} rows; expected at least 40"
        ))
    }
}

#[test]
fn field_selector_presets_apply_to_every_matrix_row() -> TestResult {
    for surface in matrix_surfaces()? {
        let schema = read_json(schema_path(&surface.schema_file))?;
        let response = synthetic_response(&schema, &surface.command)?;
        for (preset_name, _) in PRESETS {
            let selector = FieldSelector::parse(preset_name);
            let selected = apply_field_selector_to_json(&response, &selector).map_err(|error| {
                format!("{} {preset_name} selector failed: {error}", surface.surface)
            })?;
            let selected_json: Value = serde_json::from_str(&selected)
                .map_err(|error| format!("{} {preset_name} JSON: {error}", surface.surface))?;
            let declared = schema_preset_fields(&schema, preset_name)?;
            if declared.iter().any(|field| field == "*") {
                if selected_json.get("data")
                    != serde_json::from_str::<Value>(&response)
                        .map_err(|error| error.to_string())?
                        .get("data")
                {
                    return Err(format!(
                        "{} full preset must preserve data",
                        surface.surface
                    ));
                }
            } else {
                let observed = selected_names(&selected_json["data"], &declared);
                let expected = declared.into_iter().collect::<BTreeSet<_>>();
                if observed != expected {
                    return Err(format!(
                        "{} {preset_name}: selected names differ: observed={observed:?} expected={expected:?}",
                        surface.surface
                    ));
                }
            }
            if selected_json.get("schema").and_then(Value::as_str) != Some("ee.response.v1") {
                return Err(format!(
                    "{} {preset_name}: envelope schema missing",
                    surface.surface
                ));
            }
            if selected_json.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(format!(
                    "{} {preset_name}: envelope success missing",
                    surface.surface
                ));
            }
            if selected_json.get("fields").and_then(Value::as_str) != Some(*preset_name) {
                return Err(format!(
                    "{} {preset_name}: fields indicator missing",
                    surface.surface
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn explicit_list_and_preset_additions_are_precise() -> TestResult {
    let schema = read_json(schema_path("ee.status.v1.json"))?;
    let response = synthetic_response(&schema, "status")?;

    let explicit =
        apply_field_selector_to_json(&response, &FieldSelector::parse("command,version"))
            .map_err(|error| error.to_string())?;
    let explicit_json: Value =
        serde_json::from_str(&explicit).map_err(|error| error.to_string())?;
    assert_data_keys(&explicit_json, &["command", "version"], "explicit list")?;

    let mixed =
        apply_field_selector_to_json(&response, &FieldSelector::parse("preset=summary,runtime"))
            .map_err(|error| error.to_string())?;
    let mixed_json: Value = serde_json::from_str(&mixed).map_err(|error| error.to_string())?;
    assert_data_keys(
        &mixed_json,
        &["command", "version", "workspace", "capabilities", "runtime"],
        "preset plus addition",
    )
}

#[test]
fn unknown_and_conflicting_fields_have_structured_usage_codes() -> TestResult {
    let schema = read_json(schema_path("ee.status.v1.json"))?;
    let response = synthetic_response(&schema, "status")?;

    let unknown = apply_field_selector_to_json(&response, &FieldSelector::parse("missingField"))
        .expect_err("unknown field must fail");
    let unknown_json: Value =
        serde_json::from_str(&error_response_json(&unknown)).map_err(|error| error.to_string())?;
    if unknown_json.pointer("/error/code").and_then(Value::as_str) != Some("usage_unknown_field") {
        return Err(format!("unknown field code mismatch: {unknown_json}"));
    }
    if unknown_json
        .pointer("/error/details/acceptedFields")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Err("unknown field error must include acceptedFields".to_string());
    }

    let conflict =
        apply_field_selector_to_json(&response, &FieldSelector::parse("minimal,summary"))
            .expect_err("conflicting presets must fail");
    let conflict_json: Value =
        serde_json::from_str(&error_response_json(&conflict)).map_err(|error| error.to_string())?;
    if conflict_json.pointer("/error/code").and_then(Value::as_str)
        != Some("usage_conflicting_presets")
    {
        return Err(format!("conflicting preset code mismatch: {conflict_json}"));
    }
    Ok(())
}

fn assert_data_keys(value: &Value, expected: &[&str], label: &str) -> TestResult {
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{label}: missing data object"))?;
    let observed = data.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if observed == expected {
        Ok(())
    } else {
        Err(format!(
            "{label}: data keys differ observed={observed:?} expected={expected:?}"
        ))
    }
}

fn selected_names(value: &Value, declared: &[String]) -> BTreeSet<String> {
    let declared = declared.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    collect_selected_names(value, &declared, &mut observed);
    observed
}

fn collect_selected_names(
    value: &Value,
    declared: &BTreeSet<&str>,
    observed: &mut BTreeSet<String>,
) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if declared.contains(key.as_str()) {
                    observed.insert(key.clone());
                }
                collect_selected_names(child, declared, observed);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_selected_names(item, declared, observed);
            }
        }
        _ => {}
    }
}

fn synthetic_response(schema: &Value, command: &str) -> Result<String, String> {
    let data_schema = schema
        .pointer("/properties/data")
        .ok_or("schema missing /properties/data")?;
    let mut data = sample_object_from_schema(data_schema);
    data.insert("command".to_string(), Value::String(command.to_string()));
    enrich_surface_samples(command, &mut data);
    let response = json!({
        "schema": "ee.response.v1",
        "success": true,
        "data": data,
    });
    Ok(response.to_string())
}

fn sample_object_from_schema(schema: &Value) -> Map<String, Value> {
    let mut object = Map::new();
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (key, property_schema) in properties {
            object.insert(key.clone(), sample_value_for_schema(key, property_schema));
        }
    }
    object
}

fn sample_value_for_schema(key: &str, schema: &Value) -> Value {
    if let Some(constant) = schema.get("const") {
        return constant.clone();
    }
    match schema.get("type") {
        Some(Value::String(kind)) => sample_value_for_type(key, kind, schema),
        Some(Value::Array(kinds)) => {
            if kinds.iter().any(|kind| kind.as_str() == Some("string")) {
                Value::String(format!("{key}-sample"))
            } else if kinds.iter().any(|kind| kind.as_str() == Some("integer")) {
                json!(1)
            } else if kinds.iter().any(|kind| kind.as_str() == Some("number")) {
                json!(1.0)
            } else if kinds.iter().any(|kind| kind.as_str() == Some("boolean")) {
                json!(true)
            } else {
                Value::Null
            }
        }
        _ => {
            if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
                one_of
                    .first()
                    .map(|schema| sample_value_for_schema(key, schema))
                    .unwrap_or(Value::Null)
            } else {
                Value::String(format!("{key}-sample"))
            }
        }
    }
}

fn sample_value_for_type(key: &str, kind: &str, schema: &Value) -> Value {
    match kind {
        "string" => Value::String(format!("{key}-sample")),
        "integer" => json!(1),
        "number" => json!(1.0),
        "boolean" => json!(true),
        "array" => {
            let item_schema = schema.get("items").unwrap_or(&Value::Null);
            Value::Array(vec![sample_value_for_schema(key, item_schema)])
        }
        "object" => {
            let object = sample_object_from_schema(schema);
            if object.is_empty() {
                json!({"sample": true})
            } else {
                Value::Object(object)
            }
        }
        _ => Value::Null,
    }
}

fn enrich_surface_samples(command: &str, data: &mut Map<String, Value>) {
    match command {
        "memory show" => {
            data.insert(
                "memory".to_string(),
                json!({
                    "id": "mem_00000000000000000000000001",
                    "level": "procedural",
                    "kind": "rule",
                    "content": "Run cargo fmt --check.",
                    "confidence": 0.9
                }),
            );
        }
        "curate candidates" => {
            data.insert(
                "candidates".to_string(),
                json!([{
                    "candidateId": "cand_000000000000000000000001",
                    "content": "Run cargo fmt --check.",
                    "score": 0.9
                }]),
            );
        }
        _ => {}
    }
}
