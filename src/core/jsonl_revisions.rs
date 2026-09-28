//! Revision identity recovery, separate from the author's validity interval.
//!
//! Prefer an exported supersession timestamp. Older post-V123 archives carry
//! only successor identity; those recover the marker from the successor's
//! validity start (or creation instant). Legacy valid_to derivation remains in
//! the storage layer and cannot overwrite an explicit recovered marker.

use super::{
    BTreeMap, BTreeSet, JsonlImportIssue, TimestampClass, ValidatedMemory,
    normalize_imported_timestamp,
};

fn invalid(reason: &'static str) -> JsonlImportIssue {
    // The stream is untrusted. Never copy source content or identifiers into
    // diagnostics, including cycles or references outside the archive.
    JsonlImportIssue::error(None, "invalid_memory_supersession", reason)
}

/// Imported identities eligible for the pre-V123 expiry-based fallback.
///
/// Call after validation has checked references, family membership and cycles.
/// An explicit nullable marker preserves the stored supersession state. An
/// explicit edge defines the headship of BOTH endpoints; its terminal node
/// can have an author-supplied expiry without being superseded. Protect these
/// nodes even when creation timestamps disagree with the explicit edge order.
/// Do not exclude an entire family: a mixed-era archive may still contain an
/// older, unreferenced ancestor whose only history marker is its expiry.
pub(super) fn legacy_supersession_ids(memories: &[ValidatedMemory<'_>]) -> BTreeSet<String> {
    let mut explicit = BTreeSet::new();
    for memory in memories {
        let record = memory.record;
        if record.superseded_at.is_some() {
            explicit.insert(record.memory_id.as_str());
        }
        if let Some(next) = record.superseded_by.as_deref() {
            explicit.insert(record.memory_id.as_str());
            explicit.insert(next);
        }
        if let Some(prior) = record.supersedes.as_deref() {
            explicit.insert(record.memory_id.as_str());
            explicit.insert(prior);
        }
    }
    memories
        .iter()
        .filter(|memory| {
            let record = memory.record;
            (record.valid_to.is_some() || record.expires_at.is_some())
                && !explicit.contains(record.memory_id.as_str())
        })
        // The writer uses imported IDs, which differ from archive aliases for
        // redacted records. Never pass the unparsed source identity to SQL.
        .map(|memory| memory.id.clone())
        .collect()
}

pub(super) fn supersession_timestamps(
    memories: &[ValidatedMemory<'_>],
) -> Result<BTreeMap<String, String>, JsonlImportIssue> {
    // Source aliases can be distinct yet resolve to the same durable ID: a
    // native ID may equal a redacted record's deterministic remapping. Reject
    // before building lineage/legacy sets or preparing any writes; otherwise
    // one row's history markers and upsert identity can absorb another row.
    let mut imported_ids = BTreeSet::new();
    for memory in memories {
        if !imported_ids.insert(memory.id.as_str()) {
            return Err(JsonlImportIssue::error(
                None,
                "duplicate_imported_memory_id",
                "distinct source records resolve to the same imported memory identity",
            ));
        }
    }
    let by_id: BTreeMap<_, _> = memories
        .iter()
        .map(|memory| (memory.record.memory_id.as_str(), memory.record))
        .collect();
    let mut successors = BTreeMap::<&str, &str>::new();
    let mut predecessors = BTreeMap::<&str, &str>::new();
    let mut markers = BTreeMap::new();
    for memory in memories {
        let record = memory.record;
        if let Some(Some(at)) = &record.superseded_at {
            markers.insert(
                record.memory_id.clone(),
                normalize_imported_timestamp(at, TimestampClass::Validity),
            );
        }
        let edges = record
            .superseded_by
            .as_deref()
            .map(|next| (record.memory_id.as_str(), next))
            .into_iter()
            .chain(
                record
                    .supersedes
                    .as_deref()
                    .map(|prior| (prior, record.memory_id.as_str())),
            );
        for (prior_id, next_id) in edges {
            let prior = by_id
                .get(prior_id)
                .ok_or_else(|| invalid("supersession predecessor is absent from the archive"))?;
            let next = by_id
                .get(next_id)
                .ok_or_else(|| invalid("supersession successor is absent from the archive"))?;
            if prior.superseded_at == Some(None) {
                return Err(invalid(
                    "supersession edge contradicts an explicit unsuperseded predecessor",
                ));
            }
            if prior_id == next_id
                || prior.workspace_id != next.workspace_id
                || prior.logical_id.as_deref().unwrap_or(prior_id)
                    != next.logical_id.as_deref().unwrap_or(next_id)
            {
                return Err(invalid(
                    "supersession must join distinct revisions in one workspace-local family",
                ));
            }
            if successors
                .insert(prior_id, next_id)
                .is_some_and(|existing| existing != next_id)
                || predecessors
                    .insert(next_id, prior_id)
                    .is_some_and(|existing| existing != prior_id)
            {
                return Err(invalid(
                    "supersession declares conflicting successors or predecessors",
                ));
            }
        }
    }
    // Walk each edge at most once. Iteration rather than recursion also handles
    // long histories without consuming the process stack. Do not infer order
    // from random/redacted identifiers or reject legitimate equal timestamps.
    let mut complete = BTreeSet::new();
    for &start in successors.keys() {
        let mut path = BTreeSet::new();
        let mut current = start;
        while !complete.contains(current) {
            if !path.insert(current) {
                return Err(invalid("supersession contains a cycle"));
            }
            let Some(&next) = successors.get(current) else {
                break;
            };
            current = next;
        }
        complete.extend(path);
    }
    // Explicit terminal revisions remain current even when the author gave
    // them an expiry. The legacy lineage gate treats expiry-only rows as
    // possible history; letting that compatibility rule cover two explicit
    // terminals would silently accept disconnected current heads in one
    // family. Tombstoned or explicitly superseded terminals are history.
    let mut current_families = BTreeSet::new();
    for &head_id in predecessors.keys() {
        let head = by_id[head_id];
        if successors.contains_key(head_id)
            || head.superseded_at.as_ref().is_some_and(Option::is_some)
            || head.tombstoned_at.is_some()
        {
            continue;
        }
        let family = (
            head.workspace_id.as_str(),
            head.logical_id.as_deref().unwrap_or(head_id),
        );
        if !current_families.insert(family) {
            return Err(invalid(
                "revision family declares multiple explicit current heads",
            ));
        }
    }
    for (prior_id, next_id) in successors {
        let next = by_id[next_id];
        markers.entry(prior_id.to_owned()).or_insert_with(|| {
            normalize_imported_timestamp(
                next.valid_from.as_deref().unwrap_or(&next.created_at),
                TimestampClass::Validity,
            )
        });
    }
    Ok(markers)
}

#[cfg(test)]
#[path = "jsonl_revision_heads_tests.rs"]
mod head_tests;

#[cfg(test)]
mod identity_tests {
    use super::super::{
        EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1, EXPORT_MEMORY_SCHEMA_V1, JsonValue,
        JsonlImportOptions, MemoryId, RedactionLevel, Uuid, import_jsonl_records,
        import_memory_id, import_verified_backup_jsonl_records, json, parse_jsonl_source,
        validate_memories,
    };
    use crate::models::WorkspaceId;

    type TestResult = Result<(), String>;

    fn text(rows: &[JsonValue]) -> String {
        rows.iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn mixed_identity_rows(collide: bool) -> Result<Vec<JsonValue>, String> {
        let workspace = WorkspaceId::from_uuid(Uuid::from_u128(71)).to_string();
        let mut rows = vec![
            json!({
                "schema": EXPORT_HEADER_SCHEMA_V1, "format_version": 1,
                "created_at": "2026-05-05T00:00:00Z", "workspace_id": workspace,
                "workspace_path": "/source", "export_scope": "memories",
                "redaction_level": "paranoid", "record_count": 3,
                "ee_version": "0.15.2", "export_id": "identity-collision-test",
                "import_source": "native", "trust_level": "validated"
            }),
            json!({
                "schema": EXPORT_MEMORY_SCHEMA_V1,
                "memory_id": "redacted-source-alias", "logical_id": "redacted-source-alias",
                "workspace_id": workspace, "level": "semantic", "kind": "note",
                "content": "Historical deployment guidance retained for recovery.",
                "created_at": "2026-05-01T00:00:00Z",
                "valid_to": "2026-05-02T00:00:00Z",
                "confidence": 0.8, "utility": 0.5, "importance": 0.6,
                "trust_class": "agent_assertion", "redacted": true
            }),
            json!({
                "schema": EXPORT_FOOTER_SCHEMA_V1, "export_id": "identity-collision-test",
                "completed_at": "2026-05-05T00:00:00Z", "total_records": 4,
                "memory_count": 2, "link_count": 0, "tag_count": 0,
                "audit_count": 0, "artifact_count": 0, "success": true
            }),
        ];
        // Use the real remapper, not a mock or a guessed ID encoding. A valid
        // native ID can be chosen to equal an unrelated redacted row's output.
        let parsed = parse_jsonl_source(&text(&rows));
        let alias = parsed.memories.first().ok_or("missing alias record")?;
        let recovered = import_memory_id(alias, RedactionLevel::Paranoid)
            .map_err(|issue| issue.message)?;
        let native_id = if collide {
            recovered
        } else {
            MemoryId::from_uuid(Uuid::from_u128(72)).to_string()
        };
        rows.insert(
            2,
            json!({
                "schema": EXPORT_MEMORY_SCHEMA_V1,
                "memory_id": native_id, "logical_id": native_id,
                "workspace_id": workspace, "level": "semantic", "kind": "note",
                "content": "The current deployment guidance must remain a distinct row.",
                "created_at": "2026-05-03T00:00:00Z", "superseded_at": null,
                "confidence": 0.9, "utility": 0.7, "importance": 0.6,
                "trust_class": "agent_assertion", "redacted": false
            }),
        );
        Ok(rows)
    }

    #[test]
    fn rejects_destination_collisions_before_legacy_headship_inference() -> TestResult {
        for reverse in [false, true] {
            let mut rows = mixed_identity_rows(true)?;
            if reverse {
                rows.swap(1, 2);
            }
            let parsed = parse_jsonl_source(&text(&rows));
            assert!(!parsed.has_errors(), "distinct archive IDs pass parsing");
            assert_ne!(parsed.memories[0].memory_id, parsed.memories[1].memory_id);
            let mapped = parsed
                .memories
                .iter()
                .map(|row| import_memory_id(row, RedactionLevel::Paranoid))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|issue| issue.message)?;
            assert_eq!(mapped[0], mapped[1], "the actual destination collision");
            let issues = validate_memories(&parsed)
                .err()
                .ok_or("colliding destination IDs were accepted")?;
            let issue = issues
                .iter()
                .find(|issue| issue.code == "duplicate_imported_memory_id")
                .ok_or("missing destination identity rejection")?;
            for record in &parsed.memories {
                assert!(!issue.message.contains(&record.memory_id));
                assert!(!issue.message.contains(&record.content));
            }
        }
        Ok(())
    }

    #[test]
    fn collision_rejection_has_no_destination_effects_in_either_import_mode() -> TestResult {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = directory.path().canonicalize().map_err(|error| error.to_string())?;
        for reverse in [false, true] {
            let mut rows = mixed_identity_rows(true)?;
            if reverse {
                rows.swap(1, 2);
            }
            let source = text(&rows);
            for dry_run in [false, true] {
                for backup in [false, true] {
                    let case = format!("{reverse}-{dry_run}-{backup}");
                    let options = JsonlImportOptions {
                        workspace_path: root.join(format!("workspace-{case}")),
                        database_path: Some(root.join(format!("database-{case}/ee.db"))),
                        source_path: root.join(format!("source-{case}.jsonl")),
                        dry_run,
                    };
                    std::fs::write(&options.source_path, &source)
                        .map_err(|error| error.to_string())?;
                    let report = if backup {
                        import_verified_backup_jsonl_records(&options, None)
                    } else {
                        import_jsonl_records(&options)
                    }
                    .map_err(|error| error.to_string())?;
                    assert_eq!(report.status, "rejected", "{case}: {:?}", report.issues);
                    assert!(report.issues.iter().any(|issue| {
                        issue.code == "duplicate_imported_memory_id"
                    }));
                    assert!(!options.workspace_path.exists(), "{case}");
                    let database = options.database_path.as_ref().ok_or("database")?;
                    assert!(!database.exists(), "{case}");
                    assert!(!database.parent().ok_or("database parent")?.exists(), "{case}");
                    assert_eq!(
                        std::fs::read_to_string(&options.source_path)
                            .map_err(|error| error.to_string())?,
                        source
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn distinct_native_and_redacted_identities_preserve_explicit_headship() -> TestResult {
        for reverse in [false, true] {
            let mut rows = mixed_identity_rows(false)?;
            if reverse {
                rows.swap(1, 2);
            }
            let parsed = parse_jsonl_source(&text(&rows));
            assert!(!parsed.has_errors());
            let memories = validate_memories(&parsed).map_err(|issues| format!("{issues:?}"))?;
            assert_eq!(memories.len(), 2);
            assert_ne!(memories[0].id, memories[1].id);
            let current = memories
                .iter()
                .find(|memory| memory.record.superseded_at == Some(None))
                .ok_or("missing explicitly current revision")?;
            assert!(current.supersession_known);
            assert!(current.superseded_at.is_none());
            assert!(!super::legacy_supersession_ids(&memories).contains(&current.id));
        }
        Ok(())
    }
}
