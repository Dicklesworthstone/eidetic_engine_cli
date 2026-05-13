#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ee::obs::test_log::{EventKind, LogLevel, TEST_EVENT_SCHEMA_V1, TestEvent, log_event_to};
use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn schema_path() -> PathBuf {
    repo_root().join("docs/schemas/test_event_v1.json")
}

fn schema_json() -> Result<Value, String> {
    let text = std::fs::read_to_string(schema_path()).map_err(|error| error.to_string())?;
    serde_json::from_str(&text)
        .map_err(|error| format!("test event schema is invalid JSON: {error}"))
}

fn schema_kinds(schema: &Value) -> Result<BTreeSet<String>, String> {
    let values = schema
        .pointer("/properties/kind/enum")
        .and_then(Value::as_array)
        .ok_or("schema missing /properties/kind/enum")?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("kind enum entry is not a string: {value}"))
        })
        .collect()
}

fn producer_kinds() -> BTreeSet<String> {
    EventKind::all()
        .into_iter()
        .map(|kind| kind.as_str().to_owned())
        .collect()
}

fn validate_event(value: &Value, allowed_kinds: &BTreeSet<String>) -> TestResult {
    let object = value
        .as_object()
        .ok_or_else(|| format!("event is not an object: {value}"))?;
    let schema = object
        .get("schema")
        .and_then(Value::as_str)
        .ok_or("event missing schema")?;
    if schema != TEST_EVENT_SCHEMA_V1 {
        return Err(format!("schema mismatch: {schema}"));
    }
    let timestamp = object
        .get("ts")
        .and_then(Value::as_str)
        .ok_or("event missing ts")?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map_err(|error| format!("event ts is not RFC3339: {error}"))?;
    object
        .get("test_id")
        .and_then(Value::as_str)
        .ok_or("event missing test_id")?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("event missing kind")?;
    if !allowed_kinds.contains(kind) {
        return Err(format!("event kind {kind} is not registered in schema"));
    }
    validate_kind_fields(kind, object.get("fields"))?;
    Ok(())
}

fn validate_kind_fields(kind: &str, fields: Option<&Value>) -> TestResult {
    let required: &[&str] = match kind {
        "assert_ok" => &["label"],
        "assert_fail" => &["label", "expected", "actual"],
        "pack_hash_components" => &[
            "pack_request_hash",
            "draft_items_hash",
            "degraded_summary_hash",
            "rendered_text_hash",
            "composite_hash",
        ],
        "db_generation_observed" => &["command", "health", "db_generation", "index_generation"],
        "volatile_strip" => &[
            "fields_stripped_count",
            "fields_stripped",
            "input_bytes",
            "output_bytes",
        ],
        "schema_gate" => &[
            "target_schema",
            "log_lines_checked",
            "kinds_observed",
            "orphans_in_schema",
            "orphans_in_src",
        ],
        "field_selector" => &[
            "surface",
            "preset",
            "explicit_fields_count",
            "fields_in_response",
            "rejected_field_count",
            "elapsed_us",
        ],
        "bench_iteration" => &["operation", "status", "profile", "workload_tier"],
        "artifact_manifest" => &[
            "manifest_schema",
            "phase",
            "binary_path",
            "binary_hash",
            "binary_hash_status",
            "source_hash",
            "command_hash",
            "command_arg_count",
            "execution_substrate",
            "local_host",
            "worker_host",
            "target_directory",
            "fixture_filter",
            "log_path",
            "retention_manifest_path",
            "artifact_manifest_hash",
        ],
        _ => &[],
    };
    if required.is_empty() {
        return Ok(());
    }
    let Some(fields) = fields.and_then(Value::as_object) else {
        return Err(format!("{kind} event missing fields object"));
    };
    for key in required {
        if !fields.contains_key(*key) {
            return Err(format!("{kind} event missing fields.{key}"));
        }
    }
    Ok(())
}

fn collect_jsonl_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(())
}

#[test]
fn test_event_schema_kind_inventory_matches_rust_producer() -> TestResult {
    let schema = schema_json()?;
    let schema_kinds = schema_kinds(&schema)?;
    let producer_kinds = producer_kinds();
    let orphans_in_schema = schema_kinds
        .difference(&producer_kinds)
        .cloned()
        .collect::<Vec<_>>();
    let orphans_in_src = producer_kinds
        .difference(&schema_kinds)
        .cloned()
        .collect::<Vec<_>>();
    if !orphans_in_schema.is_empty() || !orphans_in_src.is_empty() {
        return Err(format!(
            "test-event kind inventory drift: schema_only={orphans_in_schema:?} src_only={orphans_in_src:?}"
        ));
    }
    Ok(())
}

#[test]
fn active_test_event_logs_match_schema_inventory() -> TestResult {
    let schema = schema_json()?;
    let allowed_kinds = schema_kinds(&schema)?;
    let mut files = Vec::new();
    collect_jsonl_files(&repo_root().join("tests/logs/active"), &mut files)?;
    let mut checked = 0_usize;
    let mut kinds_observed = BTreeSet::new();
    for path in files {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        for (line_index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(line).map_err(|error| {
                format!(
                    "{}:{} invalid JSON: {error}",
                    path.display(),
                    line_index + 1
                )
            })?;
            validate_event(&value, &allowed_kinds)
                .map_err(|error| format!("{}:{} {error}", path.display(), line_index + 1))?;
            if let Some(kind) = value.get("kind").and_then(Value::as_str) {
                kinds_observed.insert(kind.to_owned());
            }
            checked = checked.saturating_add(1);
        }
    }

    let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log_path = tmp.path().join("schema_gate.jsonl");
    let event = TestEvent::new("test_event_schema_gate", EventKind::SchemaGate)
        .with_field("target_schema", TEST_EVENT_SCHEMA_V1)
        .with_field(
            "log_lines_checked",
            u64::try_from(checked).unwrap_or(u64::MAX),
        )
        .with_field(
            "kinds_observed",
            Value::Array(kinds_observed.into_iter().map(Value::String).collect()),
        )
        .with_field("orphans_in_schema", Value::Array(Vec::new()))
        .with_field("orphans_in_src", Value::Array(Vec::new()));
    if !log_event_to(&log_path, LogLevel::Verbose, &event) {
        return Err("schema gate event was not emitted".to_owned());
    }
    let emitted = std::fs::read_to_string(&log_path).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(emitted.trim()).map_err(|error| error.to_string())?;
    validate_event(&value, &allowed_kinds)
}
