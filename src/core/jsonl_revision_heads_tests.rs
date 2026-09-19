//! Admission controls for ambiguous explicit revision families.

use super::super::{
    EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1, EXPORT_MEMORY_SCHEMA_V1, JsonValue,
    JsonlImportOptions, MemoryId, Uuid, import_jsonl_records, import_verified_backup_jsonl_records,
    json, parse_jsonl_source, validate_memories,
};
use super::legacy_supersession_ids;
use crate::models::WorkspaceId;

type TestResult = Result<(), String>;

fn disconnected_rows(reverse: bool) -> Vec<JsonValue> {
    let workspace = WorkspaceId::from_uuid(Uuid::from_u128(31)).to_string();
    let root = MemoryId::from_uuid(Uuid::from_u128(1)).to_string();
    let mut rows = vec![json!({
        "schema": EXPORT_HEADER_SCHEMA_V1, "format_version": 1,
        "created_at": "2026-05-05T00:00:00Z", "workspace_id": workspace,
        "workspace_path": "/source", "export_scope": "all",
        "redaction_level": "none", "record_count": 5,
        "ee_version": "0.15.2", "export_id": "explicit-headship-test",
        "import_source": "native", "trust_level": "validated"
    })];
    for ordinal in 1..=4_u128 {
        rows.push(json!({
            "schema": EXPORT_MEMORY_SCHEMA_V1,
            "memory_id": MemoryId::from_uuid(Uuid::from_u128(ordinal)).to_string(),
            "logical_id": root, "workspace_id": workspace,
            "level": "procedural", "kind": "rule",
            "content": "A retained revision of the deployment procedure.",
            "created_at": format!("2026-05-{ordinal:02}T00:00:00Z"),
            "valid_to": "2029-12-01T00:00:00Z",
            "confidence": 0.8, "utility": 0.5, "importance": 0.6,
            "trust_class": "agent_assertion", "redacted": false
        }));
    }
    for (prior, head) in [(1, 2), (3, 4)] {
        if reverse {
            rows[head]["supersedes"] = rows[prior]["memory_id"].clone();
        } else {
            rows[prior]["superseded_by"] = rows[head]["memory_id"].clone();
        }
    }
    rows.push(json!({
        "schema": EXPORT_FOOTER_SCHEMA_V1, "export_id": "explicit-headship-test",
        "completed_at": "2026-05-05T00:00:00Z", "total_records": 6,
        "memory_count": 4, "link_count": 0, "tag_count": 0,
        "audit_count": 0, "artifact_count": 0, "success": true
    }));
    rows
}

fn text(rows: &[JsonValue]) -> String {
    rows.iter()
        .map(JsonValue::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ambiguous_explicit_heads_are_rejected_before_any_destination_write() -> TestResult {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = root.path().canonicalize().map_err(|e| e.to_string())?;
    for reverse in [false, true] {
        let rows = disconnected_rows(reverse);
        let source = text(&rows);
        assert!(!parse_jsonl_source(&source).has_errors());
        for dry_run in [false, true] {
            for backup in [false, true] {
                let case = format!("{reverse}-{dry_run}-{backup}");
                let options = JsonlImportOptions {
                    workspace_path: path.join(format!("workspace-{case}")),
                    database_path: Some(path.join(format!("database-{case}/ee.db"))),
                    source_path: path.join(format!("source-{case}.jsonl")),
                    dry_run,
                };
                std::fs::write(&options.source_path, &source).map_err(|e| e.to_string())?;
                let report = if backup {
                    import_verified_backup_jsonl_records(&options)
                } else {
                    import_jsonl_records(&options)
                }
                .map_err(|e| e.to_string())?;
                assert_eq!(report.status, "rejected", "{case}: {:?}", report.issues);
                let issue = report
                    .issues
                    .iter()
                    .find(|issue| issue.code == "invalid_memory_supersession")
                    .ok_or_else(|| format!("missing headship rejection: {case}"))?;
                assert!(issue.message.contains("multiple explicit current heads"));
                for row in &rows[1..5] {
                    assert!(
                        !issue
                            .message
                            .contains(row["memory_id"].as_str().ok_or("id")?)
                    );
                }
                assert!(!options.workspace_path.exists(), "{case}");
                let database = options.database_path.as_ref().ok_or("database")?;
                assert!(!database.exists(), "{case}");
                assert!(
                    !database.parent().ok_or("database parent")?.exists(),
                    "{case}"
                );
                assert_eq!(
                    std::fs::read_to_string(&options.source_path).map_err(|e| e.to_string())?,
                    source
                );
            }
        }
    }
    Ok(())
}

#[test]
fn distinct_revision_families_can_each_have_an_expiring_explicit_head() -> TestResult {
    for reverse in [false, true] {
        let mut rows = disconnected_rows(reverse);
        let other_root = rows[3]["memory_id"].clone();
        rows[3]["logical_id"] = other_root.clone();
        rows[4]["logical_id"] = other_root;
        let parsed = parse_jsonl_source(&text(&rows));
        let memories = validate_memories(&parsed).map_err(|e| format!("{e:?}"))?;
        assert_eq!(memories.len(), 4);
        assert!(memories[0].superseded_at.is_some());
        assert!(memories[1].superseded_at.is_none());
        assert!(memories[2].superseded_at.is_some());
        assert!(memories[3].superseded_at.is_none());
        assert!(legacy_supersession_ids(&memories).is_empty());
    }
    Ok(())
}

#[test]
fn tombstoned_terminal_does_not_compete_with_the_current_revision() -> TestResult {
    for reverse in [false, true] {
        let mut rows = disconnected_rows(reverse);
        rows[2]["tombstoned_at"] = json!("2026-06-01T00:00:00Z");
        let parsed = parse_jsonl_source(&text(&rows));
        let memories = validate_memories(&parsed).map_err(|e| format!("{e:?}"))?;
        assert_eq!(memories.len(), 4);
        assert!(memories[1].record.tombstoned_at.is_some());
        assert!(memories[3].superseded_at.is_none());
    }
    Ok(())
}

#[test]
fn author_expiry_alone_never_retires_the_only_explicit_head() -> TestResult {
    for reverse in [false, true] {
        let mut rows = disconnected_rows(reverse);
        drop(rows.drain(3..5));
        rows[0]["record_count"] = json!(3);
        rows[3]["total_records"] = json!(4);
        rows[3]["memory_count"] = json!(2);
        let parsed = parse_jsonl_source(&text(&rows));
        let memories = validate_memories(&parsed).map_err(|e| format!("{e:?}"))?;
        assert_eq!(memories.len(), 2);
        assert!(memories[0].superseded_at.is_some());
        assert!(memories[1].superseded_at.is_none());
        assert_eq!(
            memories[1].record.valid_to.as_deref(),
            Some("2029-12-01T00:00:00Z")
        );
        assert!(legacy_supersession_ids(&memories).is_empty());
    }
    Ok(())
}
