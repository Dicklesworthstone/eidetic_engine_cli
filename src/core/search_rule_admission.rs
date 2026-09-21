//! Native memory revision and procedural-rule admission from source truth.
//!
//! The index supplies candidate IDs and scores, never rule authority or bodies.
//! Semantic hits have no body to display, and old lexical hits may outlive a
//! tombstone, supersession, workspace move or rule revision. Search intentionally
//! includes draft/deprecated rules for inspection; pack admission is stricter.
//! Memory revisions share this pre-ranking admission point so superseded
//! candidates cannot affect relevance floors, duplicate suppression or hints.
//! Rule bodies, tags, lineage and workspace binding must describe one snapshot;
//! a revision assembled from independently current reads is not a real revision.

#[path = "search_revision_admission.rs"]
pub(super) mod memory_revisions;

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use super::{DbConnection, MeshQueryVisibility, SearchDegradation, SearchHit, SearchOptions};
use crate::models::{RuleId, RuleMaturity};
use crate::search::RuleIndexProjection;

pub(super) fn is_rule_hit(hit: &SearchHit) -> bool {
    hit.doc_id.starts_with("rule_")
        || hit
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("source"))
            .and_then(serde_json::Value::as_str)
            == Some("rule")
}

/// Borrow the caller's pinned source view or own exactly one read snapshot.
/// Opening a read-only connection alone does not pin successive SQL statements.
/// An unavailable snapshot withholds rules, never authorizes indexed metadata.
fn load_projections(
    options: &SearchOptions,
    ids: &BTreeSet<&str>,
    read_connection: Option<&DbConnection>,
    after_rule_rows: impl FnOnce(),
) -> BTreeMap<String, RuleIndexProjection> {
    if ids.is_empty() {
        return BTreeMap::new();
    }
    if let Some(connection) = read_connection {
        // Search/context owns this snapshot. Do not nest BEGIN or release it.
        return projections(options, ids, connection, after_rule_rows);
    }
    let Ok(connection) = DbConnection::open_file_read_only(&options.resolve_database_path())
    else {
        return BTreeMap::new();
    };
    let Ok(snapshot) = memory_revisions::RevisionReadSnapshot::begin(&connection) else {
        return BTreeMap::new();
    };
    let admitted = projections(options, ids, &connection, after_rule_rows);
    if snapshot.finish().is_err() {
        return BTreeMap::new();
    }
    admitted
}

// The boundary permits deterministic real-writer interleavings after native
// bodies and before dependent rows. Production supplies a no-op, not a sleep.
fn projections(
    options: &SearchOptions,
    ids: &BTreeSet<&str>,
    connection: &DbConnection,
    after_rule_rows: impl FnOnce(),
) -> BTreeMap<String, RuleIndexProjection> {
    let Some(workspace) = crate::core::workspace::addressed_workspace_row(
        connection,
        &options.workspace_path,
        &options.resolve_database_path(),
    )
    .ok()
    .flatten() else {
        return BTreeMap::new();
    };
    let rules: Vec<_> = ids
        .iter()
        .filter_map(|id| {
            let canonical = RuleId::from_str(id).ok()?.to_string();
            if canonical != *id {
                return None;
            }
            let rule = connection.get_procedural_rule(id).ok()??;
            if rule.workspace_id != workspace.id || RuleMaturity::from_str(&rule.maturity).is_err()
            {
                return None;
            }
            Some((canonical, rule))
        })
        .collect();
    after_rule_rows();
    rules
        .into_iter()
        .filter_map(|(canonical, rule)| {
            let tags = connection.get_rule_tags(&canonical).ok()?;
            let sources = connection.get_rule_source_memory_ids(&canonical).ok()?;
            // Provenance cannot borrow identities from another workspace either.
            // Source-less rules are legitimate searchable entities, not fake memories.
            if !sources.is_empty() {
                let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
                let rows = connection.get_memories_batch(&refs).ok()?;
                if sources.iter().any(|source| {
                    rows.get(source)
                        .is_none_or(|row| row.workspace_id != workspace.id)
                }) {
                    return None;
                }
            }
            let projection = RuleIndexProjection::new(rule, &workspace.path, tags, sources);
            projection
                .is_search_indexable()
                .then_some((canonical, projection))
        })
        .collect()
}

fn canonical_metadata(projection: &RuleIndexProjection) -> serde_json::Value {
    let document = crate::search::rule_to_document(projection).into_indexable();
    let mut metadata: serde_json::Map<String, serde_json::Value> = document
        .metadata
        .into_iter()
        .map(|(key, value)| (key, serde_json::Value::String(value)))
        .collect();
    metadata.insert(
        "provenance_uri".to_owned(),
        serde_json::Value::String(format!("ee://rule/{}", projection.rule().id)),
    );
    serde_json::Value::Object(metadata)
}

/// Admit memory revisions and hydrate rules before calibration and query hints.
/// Failed authority lookups withhold the affected native entity type only.
/// Never creates a database or repairs the index. Candidate IDs bound the work.
pub(super) fn admit_hits(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
) -> Vec<SearchHit> {
    let hits = memory_revisions::admit_hits(options, hits, degraded, read_connection);
    let ids: BTreeSet<&str> = hits
        .iter()
        .filter(|hit| is_rule_hit(hit))
        .map(|hit| hit.doc_id.as_str())
        .collect();
    if ids.is_empty() {
        return hits;
    }
    let admitted = load_projections(options, &ids, read_connection, || {});
    let mut filtered = 0usize;
    let hits = hits
        .into_iter()
        .filter_map(|mut hit| {
            if !is_rule_hit(&hit) {
                return Some(hit);
            }
            let projection = admitted.get(&hit.doc_id);
            let valid = projection.is_some_and(|projection| {
                // Metadata-free semantic winners may be admitted by live identity.
                // A supplied revision, however, must match in both type and value.
                hit.metadata
                    .as_ref()
                    .and_then(|meta| meta.get("entity_revision"))
                    .is_none_or(|revision| revision.as_str() == Some(projection.entity_revision()))
                    && matches!(
                        super::mesh_query_visibility(hit.metadata.as_ref()),
                        MeshQueryVisibility::Local
                    )
            });
            if !valid {
                filtered += 1;
                return None;
            }
            if let Some(projection) = projection {
                // Replace, do not merge: indexed content, tags, trust, provenance,
                // and fabricated memory IDs must not survive native hydration.
                hit.metadata = Some(canonical_metadata(projection));
            }
            Some(hit)
        })
        .collect();
    if filtered > 0 {
        degraded.push(SearchDegradation {
            code: "rule_live_admission_filtered".to_owned(),
            severity: "low".to_owned(),
            message: format!("Filtered {filtered} indexed rule candidates because current source-of-truth admission could not be verified."),
            repair: Some("ee index rebuild --workspace .".to_owned()),
        });
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::super::{ScoreSource, SearchDedupMode, SearchSourceMode, SpeedMode};
    use super::*;
    use crate::db::{CreateProceduralRuleInput, CreateWorkspaceInput};
    use crate::models::MemoryScope;
    use serde_json::json;

    type TestResult = Result<(), String>;
    const WORKSPACE: &str = "wsp_00000000000000000000000011";
    const OTHER: &str = "wsp_00000000000000000000000012";
    const RULE: &str = "rule_00000000000000000000000011";
    const SECOND: &str = "rule_00000000000000000000000012";
    const BODY: &str =
        "Use an atomic generation pointer before declaring the search index current.";

    fn fixture() -> Result<(tempfile::TempDir, SearchOptions, DbConnection), String> {
        let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root = temp.path().canonicalize().map_err(|e| e.to_string())?;
        let database = root.join("rules.db");
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.migrate().map_err(|e| e.to_string())?;
        for (id, path) in [(WORKSPACE, root.clone()), (OTHER, root.join("other"))] {
            db.insert_workspace(
                id,
                &CreateWorkspaceInput {
                    path: path.display().to_string(),
                    name: None,
                },
            )
            .map_err(|e| e.to_string())?;
        }
        let options = SearchOptions {
            workspace_path: root.clone(),
            database_path: Some(database),
            index_dir: Some(root.join("index")),
            query: "generation pointer".to_owned(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: true,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
        };
        Ok((temp, options, db))
    }

    fn insert(db: &DbConnection, id: &str, workspace: &str, maturity: &str) -> TestResult {
        db.insert_procedural_rule(
            id,
            &CreateProceduralRuleInput {
                workspace_id: workspace.to_owned(),
                content: BODY.to_owned(),
                confidence: 0.6,
                utility: 0.7,
                importance: 0.8,
                trust_class: "human_explicit".to_owned(),
                scope: "workspace".to_owned(),
                scope_pattern: None,
                maturity: maturity.to_owned(),
                protected: true,
                source_memory_ids: Vec::new(),
                tags: vec!["generation".to_owned()],
            },
        )
        .map_err(|e| e.to_string())
    }

    fn hit(id: &str) -> SearchHit {
        SearchHit {
            doc_id: id.to_owned(),
            score: 0.73,
            source: ScoreSource::SemanticFast,
            fast_score: Some(0.73),
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: None,
            explanation: None,
        }
    }

    #[test]
    fn sourceless_semantic_rule_gets_native_body_provenance_and_revision() -> TestResult {
        let (_temp, options, db) = fixture()?;
        insert(&db, RULE, WORKSPACE, "validated")?;
        let before = db.get_procedural_rule(RULE).map_err(|e| e.to_string())?;
        let mut degraded = Vec::new();
        let hits = admit_hits(&options, vec![hit(RULE)], &mut degraded, Some(&db));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, RULE);
        assert_eq!(hits[0].score.to_bits(), 0.73_f32.to_bits());
        let metadata = hits[0].metadata.as_ref().ok_or("metadata")?;
        assert_eq!(metadata["content"], BODY);
        assert_eq!(metadata["source"], "rule");
        assert_eq!(metadata["provenance_uri"], format!("ee://rule/{RULE}"));
        assert_eq!(metadata["maturity"], "validated");
        assert_eq!(metadata["source_memory_count"], "0");
        assert!(
            metadata["entity_revision"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(metadata.get("memory_id").is_none());
        assert!(degraded.is_empty());
        assert_eq!(
            db.get_procedural_rule(RULE).map_err(|e| e.to_string())?,
            before
        );
        Ok(())
    }

    #[test]
    fn indexed_rule_body_and_fabricated_memory_identity_cannot_override_live_fields() -> TestResult
    {
        let (_temp, options, db) = fixture()?;
        insert(&db, RULE, WORKSPACE, "candidate")?;
        let mut candidate = hit(RULE);
        candidate.metadata = Some(
            json!({"content": "obsolete secret body", "maturity": "validated",
            "source_memory_ids": "mem_fake", "memory_id": "mem_fake", "trust_class": "fabricated",
            "provenance_uri": "file:///private/source"}),
        );
        let hits = admit_hits(&options, vec![candidate], &mut Vec::new(), Some(&db));
        assert_eq!(hits.len(), 1);
        let metadata = hits[0].metadata.as_ref().ok_or("metadata")?;
        assert_eq!(metadata["content"], BODY);
        assert_eq!(metadata["maturity"], "candidate");
        assert_eq!(metadata["trust_class"], "human_explicit");
        assert!(!metadata.to_string().contains("obsolete"));
        assert!(!metadata.to_string().contains("mem_fake"));
        assert!(!metadata.to_string().contains("/private/source"));
        Ok(())
    }

    #[test]
    fn tombstoned_superseded_and_foreign_rules_are_withheld_even_when_protected() -> TestResult {
        let (_temp, mut options, db) = fixture()?;
        insert(&db, RULE, WORKSPACE, "validated")?;
        insert(&db, SECOND, OTHER, "validated")?;
        let candidates = vec![hit(RULE), hit(SECOND)];
        assert_eq!(
            admit_hits(&options, candidates.clone(), &mut Vec::new(), Some(&db)).len(),
            1
        );
        for update in [
            format!(
                "UPDATE procedural_rules SET tombstoned_at = '2026-09-01T00:00:00Z' WHERE id = '{RULE}'"
            ),
            format!(
                "UPDATE procedural_rules SET tombstoned_at = NULL, maturity = 'superseded' WHERE id = '{RULE}'"
            ),
            format!(
                "UPDATE procedural_rules SET maturity = 'validated', superseded_by = '{SECOND}' WHERE id = '{RULE}'"
            ),
        ] {
            db.execute_raw(&update).map_err(|e| e.to_string())?;
            options.include_tombstoned = true;
            options.include_stale = true;
            let mut degraded = Vec::new();
            assert!(admit_hits(&options, candidates.clone(), &mut degraded, Some(&db)).is_empty());
            assert_eq!(degraded[0].code, "rule_live_admission_filtered");
            assert!(!degraded[0].message.contains(RULE));
        }
        Ok(())
    }

    #[test]
    fn stale_and_malformed_rule_revisions_are_rejected_but_current_revision_is_hydrated()
    -> TestResult {
        let (_temp, options, db) = fixture()?;
        insert(&db, RULE, WORKSPACE, "validated")?;
        let baseline = admit_hits(&options, vec![hit(RULE)], &mut Vec::new(), Some(&db));
        assert_eq!(baseline.len(), 1);
        let revision = baseline[0].metadata.as_ref().ok_or("metadata")?["entity_revision"].clone();
        let mut valid = hit(RULE);
        valid.metadata = Some(json!({"entity_revision": revision}));
        assert_eq!(
            admit_hits(&options, vec![valid.clone()], &mut Vec::new(), Some(&db)).len(),
            1
        );
        for value in [json!("blake3:stale"), json!(null), json!(42)] {
            let mut invalid = hit(RULE);
            invalid.metadata = Some(json!({"entity_revision": value}));
            assert!(admit_hits(&options, vec![invalid], &mut Vec::new(), Some(&db)).is_empty());
        }
        db.execute_raw(&format!(
            "INSERT INTO rule_tags (rule_id, tag) VALUES ('{RULE}', 'changed')"
        ))
        .map_err(|e| e.to_string())?;
        assert!(admit_hits(&options, vec![valid], &mut Vec::new(), Some(&db)).is_empty());
        let fresh = admit_hits(&options, vec![hit(RULE)], &mut Vec::new(), Some(&db));
        assert_eq!(fresh.len(), 1);
        assert_ne!(
            fresh[0].metadata.as_ref().ok_or("fresh metadata")?["entity_revision"],
            revision
        );
        Ok(())
    }

    #[test]
    fn rule_search_preserves_draft_and_deprecated_inspection_without_pack_promotion() -> TestResult
    {
        let (_temp, options, db) = fixture()?;
        insert(&db, RULE, WORKSPACE, "draft")?;
        insert(&db, SECOND, WORKSPACE, "deprecated")?;
        let hits = admit_hits(
            &options,
            vec![hit(RULE), hit(SECOND)],
            &mut Vec::new(),
            Some(&db),
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].metadata.as_ref().ok_or("metadata")?["maturity"],
            "draft"
        );
        assert_eq!(
            hits[1].metadata.as_ref().ok_or("metadata")?["maturity"],
            "deprecated"
        );
        let ids = BTreeSet::from([RULE, SECOND]);
        assert!(
            load_projections(&options, &ids, Some(&db), || {})
                .values()
                .all(|projection| !projection.is_pack_admissible())
        );
        Ok(())
    }

    #[test]
    fn unavailable_rule_authority_never_creates_a_store_or_discards_unrelated_hits() -> TestResult {
        let (_temp, mut options, db) = fixture()?;
        insert(&db, RULE, WORKSPACE, "validated")?;
        let non_rule = hit("mem_unrelated");
        let missing = options.workspace_path.join("absent.db");
        options.database_path = Some(missing.clone());
        let mut degraded = Vec::new();
        let hits = admit_hits(
            &options,
            vec![hit(RULE), hit("rule_malformed"), non_rule],
            &mut degraded,
            None,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "mem_unrelated");
        assert_eq!(degraded[0].code, "rule_live_admission_filtered");
        assert!(!missing.exists());
        assert!(!degraded[0].message.contains("absent.db"));
        Ok(())
    }

    fn revision_values(
        projections: &BTreeMap<String, RuleIndexProjection>,
    ) -> BTreeMap<String, serde_json::Value> {
        projections
            .iter()
            .map(|(id, projection)| (id.clone(), canonical_metadata(projection)))
            .collect()
    }

    fn commit_sql(db: &DbConnection, statements: &[String]) -> TestResult {
        db.execute_raw("BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        for statement in statements {
            if let Err(error) = db.execute_raw(statement) {
                let _ = db.execute_raw("ROLLBACK");
                return Err(error.to_string());
            }
        }
        db.execute_raw("COMMIT")
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn owned_rule_snapshot_never_synthesizes_a_mixed_body_and_tag_revision() -> TestResult {
        let (_temp, options, writer) = fixture()?;
        insert(&writer, RULE, WORKSPACE, "validated")?;
        insert(&writer, SECOND, WORKSPACE, "candidate")?;
        let ids = BTreeSet::from([RULE, SECOND]);
        let before = revision_values(&load_projections(&options, &ids, None, || {}));
        assert_eq!(before.len(), 2);
        let mut committed = Ok(());
        let captured = load_projections(&options, &ids, None, || {
            committed = commit_sql(
                &writer,
                &[
                    format!(
                        "UPDATE procedural_rules SET content = 'Revised generation guidance.', confidence = 0.4 WHERE id = '{RULE}'"
                    ),
                    format!(
                        "INSERT INTO rule_tags (rule_id, tag) VALUES ('{RULE}', 'revision-two')"
                    ),
                    format!(
                        "UPDATE procedural_rules SET content = 'Revised second rule.' WHERE id = '{SECOND}'"
                    ),
                ],
            );
        });
        committed?;
        assert_eq!(revision_values(&captured), before);
        let after = revision_values(&load_projections(&options, &ids, None, || {}));
        assert_eq!(after[RULE]["content"], "Revised generation guidance.");
        assert_eq!(after[SECOND]["content"], "Revised second rule.");
        assert_ne!(after[RULE]["entity_revision"], before[RULE]["entity_revision"]);
        assert_ne!(after[SECOND]["entity_revision"], before[SECOND]["entity_revision"]);
        let mut indexed = hit(RULE);
        indexed.metadata = Some(before[RULE].clone());
        assert!(admit_hits(&options, vec![indexed], &mut Vec::new(), None).is_empty());
        Ok(())
    }

    #[test]
    fn concurrent_rule_retirement_belongs_to_the_next_source_snapshot() -> TestResult {
        let (_temp, options, writer) = fixture()?;
        insert(&writer, RULE, WORKSPACE, "validated")?;
        let ids = BTreeSet::from([RULE]);
        let mut committed = Ok(());
        let captured = load_projections(&options, &ids, None, || {
            committed = commit_sql(
                &writer,
                &[format!(
                    "UPDATE procedural_rules SET tombstoned_at = '2026-09-20T00:00:00Z' WHERE id = '{RULE}'"
                )],
            );
        });
        committed?;
        assert!(captured.contains_key(RULE));
        let next = load_projections(&options, &ids, None, || {});
        assert!(next.is_empty());
        Ok(())
    }

    #[test]
    fn supplied_rule_snapshot_is_neither_replaced_nor_released() -> TestResult {
        let (_temp, options, writer) = fixture()?;
        insert(&writer, RULE, WORKSPACE, "validated")?;
        let reader = DbConnection::open_file_read_only(&options.resolve_database_path())
            .map_err(|e| e.to_string())?;
        reader.begin_read_snapshot().map_err(|e| e.to_string())?;
        let ids = BTreeSet::from([RULE]);
        let before = revision_values(&load_projections(&options, &ids, Some(&reader), || {}));
        let mut committed = Ok(());
        let captured = load_projections(&options, &ids, Some(&reader), || {
            committed = commit_sql(
                &writer,
                &[format!(
                    "INSERT INTO rule_tags (rule_id, tag) VALUES ('{RULE}', 'later-snapshot')"
                )],
            );
        });
        committed?;
        assert_eq!(revision_values(&captured), before);
        assert_eq!(
            revision_values(&load_projections(&options, &ids, Some(&reader), || {})),
            before,
            "later reads through the caller still see its pinned source view"
        );
        assert!(reader.begin_read_snapshot().is_err(), "caller still owns BEGIN");
        reader.rollback_read_snapshot().map_err(|e| e.to_string())?;
        assert_ne!(
            revision_values(&load_projections(&options, &ids, Some(&reader), || {})),
            before
        );
        Ok(())
    }

    fn seed_lineage(db: &DbConnection, memory: &str, workspace: &str) -> TestResult {
        db.insert_memory(
            memory,
            &crate::db::CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                level: "episodic".to_owned(),
                kind: "note".to_owned(),
                content: "Source observation, not the rule body.".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some(format!("ee://memory/{memory}")),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
                valid_to: None,
            },
        )
        .map_err(|e| e.to_string())
    }

    fn insert_sourced(db: &DbConnection, rule: &str, source: &str) -> TestResult {
        db.insert_procedural_rule(
            rule,
            &CreateProceduralRuleInput {
                workspace_id: WORKSPACE.to_owned(),
                content: BODY.to_owned(),
                confidence: 0.6,
                utility: 0.7,
                importance: 0.8,
                trust_class: "human_explicit".to_owned(),
                scope: "workspace".to_owned(),
                scope_pattern: None,
                maturity: "validated".to_owned(),
                protected: true,
                source_memory_ids: vec![source.to_owned()],
                tags: vec!["generation".to_owned()],
            },
        )
        .map_err(|e| e.to_string())
    }

    #[test]
    fn source_workspace_authority_is_pinned_with_its_rule() -> TestResult {
        let (_temp, options, writer) = fixture()?;
        let source = "mem_00000000000000000000000071";
        seed_lineage(&writer, source, WORKSPACE)?;
        insert_sourced(&writer, RULE, source)?;
        let ids = BTreeSet::from([RULE]);
        let before = revision_values(&load_projections(&options, &ids, None, || {}));
        assert_eq!(before.len(), 1);
        let mut committed = Ok(());
        let captured = load_projections(&options, &ids, None, || {
            committed = commit_sql(
                &writer,
                &[format!(
                    "UPDATE memories SET workspace_id = '{OTHER}' WHERE id = '{source}'"
                )],
            );
        });
        committed?;
        assert_eq!(revision_values(&captured), before);
        assert!(load_projections(&options, &ids, None, || {}).is_empty());
        Ok(())
    }

    #[test]
    fn source_snapshot_reads_do_not_mutate_rules_audits_or_indexes() -> TestResult {
        let (_temp, options, db) = fixture()?;
        insert(&db, RULE, WORKSPACE, "validated")?;
        let rule = db.get_procedural_rule(RULE).map_err(|e| e.to_string())?;
        let audits = db.count_table_rows("audit_log").map_err(|e| e.to_string())?;
        let ids = BTreeSet::from([RULE]);
        for _ in 0..3 {
            assert_eq!(load_projections(&options, &ids, None, || {}).len(), 1);
        }
        assert_eq!(db.get_procedural_rule(RULE).map_err(|e| e.to_string())?, rule);
        assert_eq!(db.count_table_rows("audit_log").map_err(|e| e.to_string())?, audits);
        assert!(!options.workspace_path.join("index").exists());
        assert!(!options.workspace_path.join(".ee").exists());
        Ok(())
    }
}
