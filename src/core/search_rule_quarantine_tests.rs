//! Real-store native-rule review-hold regressions. No indexed metadata is authority.

use super::super::{ScoreSource, SearchDedupMode, SearchSourceMode, SpeedMode};
use super::*;
use crate::db::{CreateProceduralRuleInput, CreateWorkspaceInput, StoredFeedbackQuarantine};
use crate::models::MemoryScope;

const WORKSPACE: &str = "wsp_00000000000000000000000111";
const FOREIGN: &str = "wsp_00000000000000000000000112";
const RULE: &str = "rule_00000000000000000000000111";
const SECOND: &str = "rule_00000000000000000000000112";
const TIME: &str = "2026-01-01T00:00:00Z";
const BODY: &str = "Publish an atomic generation before reporting indexed success.";
type TestResult = Result<(), String>;

fn fixture() -> Result<(tempfile::TempDir, SearchOptions, DbConnection), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database = root.join("rules.db");
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    db.migrate().map_err(|error| error.to_string())?;
    for (id, path) in [(WORKSPACE, root.clone()), (FOREIGN, root.join("foreign"))] {
        db.insert_workspace(
            id,
            &CreateWorkspaceInput {
                path: path.display().to_string(),
                name: None,
            },
        )
        .map_err(|error| error.to_string())?;
    }
    let options = SearchOptions {
        workspace_path: root.clone(),
        database_path: Some(database),
        index_dir: Some(root.join("index")),
        query: "atomic generation".to_owned(),
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

fn insert_rule(db: &DbConnection, id: &str, sources: Vec<String>) -> TestResult {
    db.insert_procedural_rule(
        id,
        &CreateProceduralRuleInput {
            workspace_id: WORKSPACE.to_owned(),
            content: BODY.to_owned(),
            confidence: 0.9,
            utility: 0.9,
            importance: 0.9,
            trust_class: "human_explicit".to_owned(),
            scope: "workspace".to_owned(),
            scope_pattern: None,
            maturity: "validated".to_owned(),
            protected: true,
            source_memory_ids: sources,
            tags: vec!["generation".to_owned()],
        },
    )
    .map_err(|error| error.to_string())
}

fn hold(
    db: &DbConnection,
    ordinal: usize,
    workspace: &str,
    kind: &str,
    target: &str,
) -> TestResult {
    db.insert_feedback_quarantine_for_recovery(&StoredFeedbackQuarantine {
        id: format!("fq_{ordinal:026}"),
        workspace_id: workspace.to_owned(),
        source_id: "private-rule-quarantine-source-canary".to_owned(),
        target_type: kind.to_owned(),
        target_id: target.to_owned(),
        signal: "harmful".to_owned(),
        weight: 0.5,
        source_type: "outcome_observed".to_owned(),
        proposed_event_id: Some(format!("fb_{ordinal:026}")),
        recorded_at: TIME.to_owned(),
        reason: "private-rule-quarantine-reason-canary".to_owned(),
        event_reason: None,
        evidence_json: None,
        session_id: None,
        raw_event_hash: format!("blake3:{ordinal:064x}"),
        status: "pending".to_owned(),
        reviewed_at: None,
        reviewed_by: None,
        released_feedback_event_id: None,
    })
    .map_err(|error| error.to_string())
}

fn reject(db: &DbConnection, ordinal: usize) -> TestResult {
    db.execute_raw(&format!(
        "UPDATE feedback_quarantine SET status = 'rejected', reviewed_at = '{TIME}', reviewed_by = 'test-reviewer' WHERE id = 'fq_{ordinal:026}'"
    ))
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn hit(id: &str) -> SearchHit {
    SearchHit {
        doc_id: id.to_owned(),
        score: 0.9,
        source: ScoreSource::SemanticFast,
        fast_score: Some(0.9),
        quality_score: None,
        lexical_score: None,
        rerank_score: None,
        metadata: None,
        explanation: None,
    }
}

#[test]
fn protected_native_rule_hold_precedes_hydration_and_ignores_indexed_trust() -> TestResult {
    let (_temp, mut options, db) = fixture()?;
    insert_rule(&db, RULE, Vec::new())?;
    insert_rule(&db, SECOND, Vec::new())?;
    let before = db
        .get_procedural_rule(RULE)
        .map_err(|error| error.to_string())?;
    hold(&db, 1, WORKSPACE, "rule", RULE)?;
    options.include_tombstoned = true;
    options.include_expired = true;
    options.include_future = true;
    options.include_stale = true;
    for reference in ["2020-01-01T00:00:00Z", "2030-01-01T00:00:00Z"] {
        options.as_of = Some(
            reference
                .parse()
                .map_err(|error: chrono::ParseError| error.to_string())?,
        );
        let mut indexed = hit(RULE);
        indexed.metadata = Some(serde_json::json!({
            "protected": true,
            "maturity": "validated",
            "trust_class": "human_explicit",
            "quarantined": false,
            "content": BODY
        }));
        let mut degraded = Vec::new();
        let admitted = admit_hits(
            &options,
            vec![indexed, hit(SECOND), hit("evidence_unrelated")],
            &mut degraded,
            Some(&db),
        );
        assert_eq!(
            admitted
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            vec![SECOND, "evidence_unrelated"]
        );
        assert_eq!(admitted[0].score.to_bits(), 0.9_f32.to_bits());
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "rule_live_admission_filtered");
        let public = format!("{} {:?}", degraded[0].message, degraded[0].repair);
        for private in [RULE, BODY, "private-rule-quarantine"] {
            assert!(!public.contains(private));
        }
        assert!(public.contains("ee outcome quarantine"));
    }
    assert_eq!(
        db.get_procedural_rule(RULE)
            .map_err(|error| error.to_string())?,
        before
    );
    Ok(())
}

#[test]
fn native_rule_holds_require_exact_target_kind_and_owning_workspace() -> TestResult {
    let (_temp, options, db) = fixture()?;
    insert_rule(&db, RULE, Vec::new())?;
    hold(&db, 1, FOREIGN, "rule", RULE)?;
    hold(&db, 2, WORKSPACE, "memory", RULE)?;
    hold(&db, 3, WORKSPACE, "rule", RULE)?;
    reject(&db, 3)?;
    assert_eq!(
        admit_hits(&options, vec![hit(RULE)], &mut Vec::new(), Some(&db)).len(),
        1
    );
    hold(&db, 4, WORKSPACE, "rule", RULE)?;
    assert!(admit_hits(&options, vec![hit(RULE)], &mut Vec::new(), Some(&db)).is_empty());
    Ok(())
}

#[test]
fn native_rule_quarantine_neither_aliases_nor_inherits_source_memory_feedback() -> TestResult {
    let (_temp, options, db) = fixture()?;
    let source = "mem_00000000000000000000000111";
    db.insert_memory(
        source,
        &crate::db::CreateMemoryInput {
            workspace_id: WORKSPACE.to_owned(),
            level: "episodic".to_owned(),
            kind: "note".to_owned(),
            content: "Private observation; independently reviewed rule has its own body.".to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some(format!("ee://memory/{source}")),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: Some(TIME.to_owned()),
            valid_to: None,
        },
    )
    .map_err(|error| error.to_string())?;
    insert_rule(&db, RULE, vec![source.to_owned()])?;
    hold(&db, 1, WORKSPACE, "memory", source)?;
    let admitted = admit_hits(
        &options,
        vec![hit(source), hit(RULE)],
        &mut Vec::new(),
        Some(&db),
    );
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].doc_id, RULE);
    let metadata = admitted[0]
        .metadata
        .as_ref()
        .ok_or("missing rule metadata")?;
    assert_eq!(metadata["content"], BODY);
    assert!(!metadata.to_string().contains("Private observation"));
    reject(&db, 1)?;
    hold(&db, 2, WORKSPACE, "rule", RULE)?;
    let admitted = admit_hits(
        &options,
        vec![hit(RULE), hit(source)],
        &mut Vec::new(),
        Some(&db),
    );
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].doc_id, source);
    Ok(())
}

#[test]
fn all_native_rule_holds_must_be_reviewed_without_mutating_source_or_index() -> TestResult {
    let (_temp, options, db) = fixture()?;
    insert_rule(&db, RULE, Vec::new())?;
    hold(&db, 1, WORKSPACE, "rule", RULE)?;
    hold(&db, 2, WORKSPACE, "rule", RULE)?;
    let rule = db
        .get_procedural_rule(RULE)
        .map_err(|error| error.to_string())?;
    let audits = db
        .count_table_rows("audit_log")
        .map_err(|error| error.to_string())?;
    let pending = db
        .list_feedback_quarantine(WORKSPACE, Some("pending"))
        .map_err(|error| error.to_string())?;
    let ids = BTreeSet::from([RULE]);
    for _ in 0..3 {
        assert!(load_projections(&options, &ids, None, || {}).is_empty());
    }
    assert_eq!(
        db.list_feedback_quarantine(WORKSPACE, Some("pending"))
            .map_err(|error| error.to_string())?,
        pending
    );
    assert_eq!(
        db.count_table_rows("audit_log")
            .map_err(|error| error.to_string())?,
        audits
    );
    assert!(!options.workspace_path.join("index").exists());
    reject(&db, 1)?;
    assert!(load_projections(&options, &ids, None, || {}).is_empty());
    reject(&db, 2)?;
    assert_eq!(load_projections(&options, &ids, None, || {}).len(), 1);
    assert_eq!(
        db.get_procedural_rule(RULE)
            .map_err(|error| error.to_string())?,
        rule
    );
    assert!(!options.workspace_path.join("index").exists());
    Ok(())
}

#[test]
fn owned_rule_snapshot_pins_both_new_holds_and_reviewed_holds() -> TestResult {
    for initially_held in [false, true] {
        let (_temp, options, writer) = fixture()?;
        insert_rule(&writer, RULE, Vec::new())?;
        if initially_held {
            hold(&writer, 1, WORKSPACE, "rule", RULE)?;
        }
        let ids = BTreeSet::from([RULE]);
        let mut committed = Ok(());
        let captured = load_projections(&options, &ids, None, || {
            committed = if initially_held {
                reject(&writer, 1)
            } else {
                hold(&writer, 1, WORKSPACE, "rule", RULE)
            };
        });
        committed?;
        assert_eq!(captured.contains_key(RULE), !initially_held);
        let current = load_projections(&options, &ids, None, || {});
        assert_eq!(current.contains_key(RULE), initially_held);
    }
    Ok(())
}

#[test]
fn native_rule_hold_does_not_replace_or_release_a_callers_snapshot() -> TestResult {
    let (_temp, options, writer) = fixture()?;
    insert_rule(&writer, RULE, Vec::new())?;
    let reader = DbConnection::open_file_read_only(&options.resolve_database_path())
        .map_err(|error| error.to_string())?;
    reader
        .begin_read_snapshot()
        .map_err(|error| error.to_string())?;
    let ids = BTreeSet::from([RULE]);
    assert_eq!(
        load_projections(&options, &ids, Some(&reader), || {}).len(),
        1
    );
    hold(&writer, 1, WORKSPACE, "rule", RULE)?;
    assert_eq!(
        load_projections(&options, &ids, Some(&reader), || {}).len(),
        1
    );
    assert!(reader.begin_read_snapshot().is_err());
    reader
        .rollback_read_snapshot()
        .map_err(|error| error.to_string())?;
    assert!(load_projections(&options, &ids, Some(&reader), || {}).is_empty());
    Ok(())
}

#[test]
fn unavailable_native_rule_quarantine_fails_closed_without_discarding_evidence() -> TestResult {
    let (_temp, options, db) = fixture()?;
    insert_rule(&db, RULE, Vec::new())?;
    db.execute_raw("ALTER TABLE feedback_quarantine RENAME TO unavailable_feedback_quarantine")
        .map_err(|error| error.to_string())?;
    let mut degraded = Vec::new();
    let admitted = admit_hits(
        &options,
        vec![hit(RULE), hit("evidence_unrelated")],
        &mut degraded,
        None,
    );
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].doc_id, "evidence_unrelated");
    assert_eq!(degraded.len(), 1);
    assert_eq!(degraded[0].code, "rule_live_admission_filtered");
    assert!(!degraded[0].message.contains("unavailable_feedback_quarantine"));
    db.execute_raw("ALTER TABLE unavailable_feedback_quarantine RENAME TO feedback_quarantine")
        .map_err(|error| error.to_string())?;
    assert_eq!(
        admit_hits(&options, vec![hit(RULE)], &mut Vec::new(), None).len(),
        1
    );
    Ok(())
}

#[test]
fn native_rule_quarantine_pages_are_candidate_bounded_and_deduplicate_holds() -> TestResult {
    let (_temp, _options, db) = fixture()?;
    let ids = (1..=RULE_RELATION_PAGE_SIZE + 1)
        .map(|index| format!("rule_{index:026}"))
        .collect::<Vec<_>>();
    for (index, id) in ids.iter().enumerate() {
        insert_rule(&db, id, Vec::new())?;
        if index % 2 == 0 {
            hold(&db, index * 2 + 1, WORKSPACE, "rule", id)?;
            hold(&db, index * 2 + 2, WORKSPACE, "rule", id)?;
        }
    }
    let requested = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let relations =
        load_relations(&db, &requested, WORKSPACE).map_err(|error| error.to_string())?;
    let expected = ids.iter().step_by(2).cloned().collect::<BTreeSet<_>>();
    assert_eq!(relations.pending_quarantine, expected);
    let subset = load_relations(&db, &[ids[1].as_str()], WORKSPACE)
        .map_err(|error| error.to_string())?;
    assert!(subset.pending_quarantine.is_empty());
    let empty = load_relations(&db, &[], WORKSPACE).map_err(|error| error.to_string())?;
    assert!(empty.pending_quarantine.is_empty());
    Ok(())
}
