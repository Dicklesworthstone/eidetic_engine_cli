//! Typed sidecars use the same redaction and authentication boundary as bodies.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::super::JsonlExporter;
use super::*;
use crate::models::{ExportMemoryRecord, ExportScope};
use serde_json::json;

fn memory(kind: &str, fields: Value) -> ExportMemoryRecord {
    ExportMemoryRecord::builder()
        .memory_id("mem_00000000000000000000000001")
        .workspace_id("ws_00000000000000000000000001")
        .level("semantic")
        .kind(kind)
        .content("An ordinary memory body.")
        .created_at("2026-09-01T00:00:00Z")
        .typed_fields(fields)
        .build()
        .unwrap()
}

#[test]
fn typed_v1_and_bare_objects_export_as_the_same_canonical_v2() {
    let fields =
        json!({"options":["alpha","beta"], "chosen":"alpha", "revisit_by":"2028-01-01T00:00:00Z"});
    let legacy = json!({"schema":"ee.memory.typed_fields.v1", "kind":"decision", "fields":fields});
    assert_eq!(
        canonical("decision", &fields).unwrap(),
        canonical("decision", &legacy).unwrap()
    );
    assert_eq!(
        canonical("decision", &fields).unwrap()["schema"],
        "ee.memory.typed_fields.v2"
    );
}

#[test]
fn typed_values_are_redacted_without_destroying_lists_or_revisit_timestamps() {
    let reference = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(55)).to_string();
    for level in [
        RedactionLevel::Minimal,
        RedactionLevel::Standard,
        RedactionLevel::Strict,
        RedactionLevel::Paranoid,
        RedactionLevel::Full,
    ] {
        let record = memory(
            "decision",
            json!({
                "options":["api_key=typed_secret_canary", "/Users/private-owner/typed-path-canary"],
                "chosen":"api_key=typed_secret_canary", "rationale":"api_key=typed_secret_canary",
                "supersedes":reference, "revisit_by":"2028-01-01T00:00:00Z"
            }),
        );
        let mut bytes = Vec::new();
        JsonlExporter::new(&mut bytes, level, ExportScope::All)
            .write_memory(record)
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("typed_secret_canary"), "{level:?}");
        if level.redacts_paths() {
            assert!(!text.contains("typed-path-canary"), "{level:?}");
        }
        let written: ExportMemoryRecord = serde_json::from_str(&text).unwrap();
        let sidecar = written.typed_fields.unwrap();
        assert_eq!(sidecar["fields"]["revisit_by"], "2028-01-01T00:00:00Z");
        assert_eq!(sidecar["fields"]["options"].as_array().unwrap().len(), 2);
        assert_eq!(
            sidecar["fields"]["supersedes"],
            super::super::redact_identifier(&reference, level)
        );
        canonical("decision", &sidecar).unwrap();
    }
}

#[test]
fn metadata_only_export_cannot_leak_typed_body_material() {
    let record = memory("command", json!({"command":"private-command-canary"}));
    let mut bytes = Vec::new();
    JsonlExporter::new(&mut bytes, RedactionLevel::None, ExportScope::MetadataOnly)
        .write_memory(record)
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["content"], "");
    assert!(value.get("typed_fields").is_none());
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .contains("private-command-canary")
    );
}

#[test]
fn invalid_and_sealed_sidecars_fail_before_a_record_is_written() {
    for fields in [
        json!({"chosen":"wrong kind"}),
        json!({"condition":42}),
        json!({"schema":"unsupported-private-canary","fields":{}}),
        json!({"action":"x".repeat(4097)}),
    ] {
        let mut bytes = Vec::new();
        let error = JsonlExporter::new(&mut bytes, RedactionLevel::None, ExportScope::All)
            .write_memory(memory("rule", fields))
            .unwrap_err();
        assert!(bytes.is_empty());
        assert!(!error.to_string().contains("private-canary"));
    }
    let mut record = memory("rule", json!({"action":"do not expose"}));
    record.content = crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT.to_owned();
    let mut bytes = Vec::new();
    assert!(
        JsonlExporter::new(&mut bytes, RedactionLevel::None, ExportScope::All)
            .write_memory(record)
            .is_err()
    );
    assert!(bytes.is_empty());
}

#[test]
fn authentication_root_commits_to_typed_fields_and_legacy_absence_stays_absent() {
    let root = |fields: Option<Value>| {
        let mut record = memory("rule", json!({"condition":"release"}));
        record.typed_fields = fields;
        let mut bytes = Vec::new();
        let mut exporter = JsonlExporter::new(&mut bytes, RedactionLevel::None, ExportScope::All);
        exporter.write_memory(record).unwrap();
        let root = exporter.finalize_records_root();
        drop(exporter);
        (root, bytes)
    };
    let (a, _) = root(Some(json!({"action":"first"})));
    let (b, _) = root(Some(json!({"action":"second"})));
    let (absent, bytes) = root(None);
    assert_ne!(a, b);
    assert_ne!(a, absent);
    assert_eq!(a.1, 1);
    assert!(
        !String::from_utf8(bytes.clone())
            .unwrap()
            .contains("typed_fields")
    );
    assert!(
        serde_json::from_slice::<ExportMemoryRecord>(&bytes)
            .unwrap()
            .typed_fields
            .is_none()
    );
}
