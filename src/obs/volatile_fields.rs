//! Canonical volatile-field registry for determinism comparisons.
//!
//! These fields legitimately vary between invocations against the same
//! workspace state, so J7-style determinism checks strip them before hashing
//! machine-facing JSON outputs.

use std::collections::BTreeSet;

use serde_json::Value;

use super::test_log::{EventKind, TestEvent, log_event, test_id_or};

/// Canonical list of fields that legitimately vary between invocations against
/// the same workspace state.
pub const VOLATILE_FIELD_NAMES: &[&str] = &[
    "generatedAt",
    "generated_at",
    "last_accessed",
    "last_accessed_at",
    "last_seen_at",
    "last_used_at",
    "audit_ts",
    "elapsedMs",
    "elapsed_ms",
    "startedAt",
    "started_at",
    "endedAt",
    "ended_at",
    "ts",
    "timestamp",
    "ee_binary_hash",
    "databasePath",
    "workspacePath",
    "indexDir",
];

/// Report emitted by a volatile-field strip operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolatileStripReport {
    /// Number of distinct volatile field names removed.
    pub fields_stripped_count: usize,
    /// Distinct volatile field names removed, in registry order.
    pub fields_stripped: Vec<&'static str>,
    /// JSON byte size before stripping, or 0 if serialization failed.
    pub input_bytes: usize,
    /// JSON byte size after stripping, or 0 if serialization failed.
    pub output_bytes: usize,
}

/// Return true when `field_name` is registered as volatile.
#[must_use]
pub fn is_volatile_field_name(field_name: &str) -> bool {
    canonical_field_name(field_name).is_some()
}

/// Recursively remove volatile fields from a JSON value and emit a structured
/// test-log event when the J1 log harness is configured.
pub fn strip_volatile_fields(value: &mut Value) -> VolatileStripReport {
    let input_bytes = serialized_len(value);
    let mut stripped = BTreeSet::new();
    strip_volatile_fields_inner(value, &mut stripped);
    let output_bytes = serialized_len(value);
    let fields_stripped = VOLATILE_FIELD_NAMES
        .iter()
        .copied()
        .filter(|field| stripped.contains(field))
        .collect::<Vec<_>>();
    let report = VolatileStripReport {
        fields_stripped_count: fields_stripped.len(),
        fields_stripped,
        input_bytes,
        output_bytes,
    };
    log_volatile_strip(&report);
    report
}

fn strip_volatile_fields_inner(value: &mut Value, stripped: &mut BTreeSet<&'static str>) {
    match value {
        Value::Object(object) => {
            let keys = object
                .keys()
                .filter_map(|key| canonical_field_name(key))
                .collect::<Vec<_>>();
            for key in keys {
                object.remove(key);
                stripped.insert(key);
            }
            for child in object.values_mut() {
                strip_volatile_fields_inner(child, stripped);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_volatile_fields_inner(item, stripped);
            }
        }
        _ => {}
    }
}

fn canonical_field_name(field_name: &str) -> Option<&'static str> {
    VOLATILE_FIELD_NAMES
        .iter()
        .copied()
        .find(|registered| *registered == field_name)
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

fn log_volatile_strip(report: &VolatileStripReport) {
    let fields = report
        .fields_stripped
        .iter()
        .map(|field| Value::String((*field).to_owned()))
        .collect::<Vec<_>>();
    let event = TestEvent::new(test_id_or("volatile_field_strip"), EventKind::VolatileStrip)
        .with_field(
            "fields_stripped_count",
            u64::try_from(report.fields_stripped_count).unwrap_or(u64::MAX),
        )
        .with_field("fields_stripped", Value::Array(fields))
        .with_field(
            "input_bytes",
            u64::try_from(report.input_bytes).unwrap_or(u64::MAX),
        )
        .with_field(
            "output_bytes",
            u64::try_from(report.output_bytes).unwrap_or(u64::MAX),
        );
    log_event(event);
}

#[cfg(test)]
mod tests {
    use super::{VOLATILE_FIELD_NAMES, is_volatile_field_name, strip_volatile_fields};

    type TestResult = Result<(), String>;

    #[test]
    fn registry_names_are_unique() -> TestResult {
        let mut names = std::collections::BTreeSet::new();
        for name in VOLATILE_FIELD_NAMES {
            if name.trim().is_empty() {
                return Err("empty volatile field name".to_owned());
            }
            if !names.insert(name) {
                return Err(format!("duplicate volatile field name: {name}"));
            }
        }
        Ok(())
    }

    #[test]
    fn strip_volatile_fields_recurses_and_reports() -> TestResult {
        let mut value = serde_json::json!({
            "schema": "ee.response.v1",
            "generatedAt": "2026-05-13T00:00:00Z",
            "data": {
                "items": [
                    {"id": "mem_a", "elapsedMs": 12, "content": "keep"},
                    {"id": "mem_b", "last_seen_at": "2026-05-13T00:00:01Z"}
                ],
                "workspacePath": "/tmp/ws"
            }
        });
        let report = strip_volatile_fields(&mut value);
        if value.pointer("/generatedAt").is_some()
            || value.pointer("/data/items/0/elapsedMs").is_some()
            || value.pointer("/data/items/1/last_seen_at").is_some()
            || value.pointer("/data/workspacePath").is_some()
        {
            return Err(format!("volatile fields were not stripped: {value}"));
        }
        if value
            .pointer("/data/items/0/content")
            .and_then(|v| v.as_str())
            != Some("keep")
        {
            return Err("non-volatile content was stripped".to_owned());
        }
        for expected in ["generatedAt", "elapsedMs", "last_seen_at", "workspacePath"] {
            if !report.fields_stripped.contains(&expected) {
                return Err(format!("report missing stripped field {expected}"));
            }
        }
        Ok(())
    }

    #[test]
    fn registry_predicate_matches_list() {
        assert!(is_volatile_field_name("generatedAt"));
        assert!(is_volatile_field_name("last_accessed_at"));
        assert!(!is_volatile_field_name("content"));
    }
}
