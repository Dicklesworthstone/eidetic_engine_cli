#![allow(clippy::unwrap_used, clippy::expect_used)] // test code may unwrap/expect
use ee::core::search::{TypedMemoryFieldFilter, TypedMemoryFieldOperator};
use ee::db::StoredMemory;
use ee::models::MemoryKind;
use ee::models::memory::{
    TYPED_MEMORY_FIELDS_SCHEMA_V2, canonicalize_typed_memory_fields_json,
    typed_memory_index_metadata_from_json,
};
use ee::search::MemoryDocumentBuilder;

#[test]
fn typed_fields_registry_v2_validates_new_fields_and_v1_sidecars() {
    let canonical = canonicalize_typed_memory_fields_json(
        &MemoryKind::Decision,
        r#"{"schema":"ee.memory.typed_fields.v1","kind":"decision","fields":{"options":["local","remote"],"chosen":"remote","supersedes":"mem_old","revisit_by":"2026-07-01T12:00:00Z"}}"#,
    )
    .expect("v1 decision sidecar should validate through v2");
    let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");

    assert_eq!(parsed["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V2);
    assert_eq!(parsed["kind"], "decision");
    assert_eq!(parsed["fields"]["chosen"], "remote");
    assert_eq!(parsed["fields"]["revisit_by"], "2026-07-01T12:00:00Z");

    let rule = canonicalize_typed_memory_fields_json(
        &MemoryKind::Rule,
        r#"{"condition":"release prep","action":"run remote proof","exceptions":["docs only","read-only review"]}"#,
    )
    .expect("rule registry fields should validate");
    let rule: serde_json::Value = serde_json::from_str(&rule).expect("rule JSON");
    assert_eq!(rule["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V2);
    assert_eq!(rule["fields"]["condition"], "release prep");
    assert_eq!(rule["fields"]["exceptions"][1], "read-only review");
}

#[test]
fn typed_fields_registry_indexes_only_registry_marked_fields() {
    let metadata = typed_memory_index_metadata_from_json(
        &MemoryKind::Decision,
        r#"{"chosen":"RCH remote","rationale":"avoid local cargo","supersedes":"mem_old","revisit_by":"2026-07-01T12:00:00Z"}"#,
    )
    .expect("indexed field metadata should extract");

    assert_eq!(
        metadata.get("typed_field.chosen"),
        Some(&"RCH remote".to_owned())
    );
    assert_eq!(
        metadata.get("typed_field.supersedes"),
        Some(&"mem_old".to_owned())
    );
    assert!(!metadata.contains_key("typed_field.rationale"));
    assert!(!metadata.contains_key("typed_field.revisit_by"));
}

#[test]
fn typed_fields_registry_document_builder_attaches_indexed_metadata() {
    let memory = StoredMemory {
        id: "mem_01234567890123456789012345".to_string(),
        workspace_id: "wsp_01234567890123456789012345".to_string(),
        level: "procedural".to_string(),
        kind: "convention".to_string(),
        content: "Scope: Rust CLI tests. Pattern: keep registry tests close.".to_string(),
        workflow_id: None,
        confidence: 0.9,
        utility: 0.7,
        importance: 0.8,
        provenance_uri: None,
        trust_class: "human_explicit".to_string(),
        trust_subclass: None,
        provenance_chain_hash: None,
        provenance_chain_hash_version: ee::db::PROVENANCE_CHAIN_HASH_VERSION.to_string(),
        provenance_verification_status: ee::db::PROVENANCE_STATUS_UNVERIFIED.to_string(),
        provenance_verified_at: None,
        provenance_verification_note: None,
        created_at: "2026-06-14T00:00:00Z".to_string(),
        updated_at: "2026-06-14T00:00:00Z".to_string(),
        tombstoned_at: None,
        valid_from: None,
        valid_to: None,
    };
    let indexable = MemoryDocumentBuilder::new()
        .with_typed_fields_json(
            r#"{"scope":"Rust CLI tests","pattern":"keep registry tests close"}"#,
        )
        .build(&memory)
        .into_indexable();

    assert_eq!(
        indexable.metadata.get("typed_field.scope"),
        Some(&"Rust CLI tests".to_owned())
    );
    assert!(!indexable.metadata.contains_key("typed_field.pattern"));
}

#[test]
fn typed_fields_registry_filter_parser_supports_three_operators() {
    let exact = TypedMemoryFieldFilter::parse("family=aggressive prefetch")
        .expect("exact filter should parse");
    assert_eq!(exact.field, "family");
    assert_eq!(exact.value, "aggressive prefetch");
    assert_eq!(exact.operator, TypedMemoryFieldOperator::Exact);

    let contains = TypedMemoryFieldFilter::parse("command~cargo test -- --nocapture=1")
        .expect("contains filter should parse");
    assert_eq!(contains.field, "command");
    assert_eq!(contains.value, "cargo test -- --nocapture=1");
    assert_eq!(contains.operator, TypedMemoryFieldOperator::Contains);

    let prefix = TypedMemoryFieldFilter::parse("reverted-at-sha^9af3c21~literal=kept")
        .expect("prefix filter should parse");
    assert_eq!(prefix.field, "reverted_at_sha");
    assert_eq!(prefix.value, "9af3c21~literal=kept");
    assert_eq!(prefix.operator, TypedMemoryFieldOperator::Prefix);
}
