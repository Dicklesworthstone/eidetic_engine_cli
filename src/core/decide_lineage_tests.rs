//! Real storage and public decision flows, including histories beyond 64 links.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::decide::{
    DecideListOptions, DecideRecordOptions, decide_list, decide_record,
};
use crate::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, MemoryLinkRelation,
    MemoryLinkSource,
};

const TOPIC: &str = "Storage backend";
const TIME: &str = "2020-01-01T00:00:00Z";

#[test]
fn preview_and_record_depths_never_saturate_to_a_fabricated_count() {
    assert_eq!(successor_depth(0).unwrap(), 1);
    assert_eq!(successor_depth(u32::MAX - 1).unwrap(), u32::MAX);
    assert!(matches!(successor_depth(u32::MAX), Err(DomainError::Storage { .. })));
}

struct Fixture {
    db: DbConnection,
    root: tempfile::TempDir,
    workspace: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        std::fs::create_dir(path.join(".ee")).unwrap();
        std::fs::write(path.join(".ee/config.toml"), "[memory]\ninclude_global = false\n")
            .unwrap();
        let db = DbConnection::open_file(path.join(".ee/ee.db")).unwrap();
        db.migrate().unwrap();
        let workspace = crate::core::workspace::stable_workspace_id(&path);
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        Self { db, root, workspace }
    }

    fn seed(&self, number: u128) -> String {
        self.seed_owned(number, &self.workspace, "decision")
    }

    fn seed_owned(&self, number: u128, workspace: &str, kind: &str) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: workspace.to_owned(),
                    level: "semantic".to_owned(),
                    kind: kind.to_owned(),
                    content: format!("Topic: {TOPIC}\nChosen: row store"),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    provenance_uri: Some("manual://decision-lineage".to_owned()),
                    tags: Vec::new(),
                    valid_from: Some(TIME.to_owned()),
                    valid_to: None,
                },
            )
            .unwrap();
        if kind == "decision" {
            self.db
                .set_memory_typed_fields_json(
                    &id,
                    Some(
                        &serde_json::json!({
                            "chosen": "row store",
                            "options": ["row store", "column store"],
                            "rationale": "Keep durable decisions exact."
                        })
                        .to_string(),
                    ),
                )
                .unwrap();
        }
        id
    }

    fn edge(&self, number: usize, child: &str, parent: &str) -> String {
        self.edge_kind(number, child, parent, MemoryLinkRelation::Supersedes, true)
    }

    fn edge_kind(
        &self,
        number: usize,
        child: &str,
        parent: &str,
        relation: MemoryLinkRelation,
        directed: bool,
    ) -> String {
        let id = format!("link_{number:026}");
        self.db
            .insert_memory_link(
                &id,
                &CreateMemoryLinkInput {
                    src_memory_id: child.to_owned(),
                    dst_memory_id: parent.to_owned(),
                    relation,
                    weight: 1.0,
                    confidence: 1.0,
                    directed,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Human,
                    created_by: None,
                    metadata_json: Some("{\"note\":\"PRIVATE-LINEAGE-ANNOTATION\"}".to_owned()),
                },
            )
            .unwrap();
        id
    }

    fn chain(&self, count: usize) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        self.db
            .with_transaction(|| {
                for number in 1..=count {
                    let id = self.seed(number as u128);
                    if let Some(parent) = ids.last() {
                        self.edge(number, &id, parent);
                        self.db.mark_memory_superseded(parent, TIME)?;
                    }
                    ids.push(id);
                }
                Ok(())
            })
            .unwrap();
        ids
    }

    fn list(&self, history: bool, limit: usize) -> Result<super::super::DecideListReport, DomainError> {
        decide_list(&DecideListOptions {
            workspace_path: self.root.path(),
            database_path: None,
            about: None,
            include_superseded: history,
            limit,
            now: None,
        })
    }

    fn depth(&self, root: &str) -> Result<u32, DomainError> {
        self.db.begin_read_snapshot().unwrap();
        let result = chain_depth(&self.db, &self.workspace, root);
        self.db.commit_read_snapshot().unwrap();
        result
    }

    fn request<'a>(&'a self, parent: &'a str, dry_run: bool) -> DecideRecordOptions<'a> {
        DecideRecordOptions {
            workspace_path: self.root.path(),
            database_path: None,
            topic: TOPIC,
            chosen: "column store",
            alternatives: vec!["row store".to_owned()],
            rationale: "Retain the complete recorded history.",
            revisit_by: None,
            supersedes: Some(parent),
            dry_run,
            actor: Some("lineage-test"),
            now: None,
        }
    }

    fn state(&self) -> Vec<Vec<Vec<(String, Value)>>> {
        ["memories", "memory_links", "memory_tags", "audit_log", "search_index_jobs"]
            .into_iter()
            .map(|table| {
                self.db
                    .query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[])
                    .unwrap()
                    .into_iter()
                    .map(|row| {
                        row.iter()
                            .map(|(name, value)| (name.to_owned(), value.clone()))
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }
}

#[test]
fn public_history_and_head_depth_remain_exact_beyond_the_old_64_link_cap() {
    let fixture = Fixture::new();
    let ids = fixture.chain(131);
    let before = fixture.state();
    let heads = fixture.list(false, 1).unwrap();
    assert_eq!(heads.total_count, 1);
    assert_eq!(heads.decisions[0].memory_id, *ids.last().unwrap());
    assert_eq!(heads.decisions[0].chain_depth, 130);
    let history = fixture.list(true, 0).unwrap();
    assert_eq!(history.total_count, 131);
    for (number, id) in ids.iter().enumerate() {
        let item = history.decisions.iter().find(|item| &item.memory_id == id).unwrap();
        assert_eq!(item.chain_depth, number as u32);
    }
    assert_eq!(fixture.state(), before);
    assert!(!history.data_json().to_string().contains("PRIVATE-LINEAGE-ANNOTATION"));
}

#[test]
fn a_real_replacement_and_its_preview_keep_the_full_predecessor_history() {
    let fixture = Fixture::new();
    let ids = fixture.chain(66);
    let previous = ids.last().unwrap();
    let before = fixture.state();
    let preview = decide_record(&fixture.request(previous, true)).unwrap();
    assert!(!preview.persisted);
    assert_eq!(preview.decision.chain_depth, 66);
    assert_eq!(fixture.state(), before);
    let written = decide_record(&fixture.request(previous, false)).unwrap();
    assert!(written.persisted);
    assert_eq!(written.decision.chain_depth, 66);
    assert_eq!(fixture.list(false, 1).unwrap().decisions[0].chain_depth, 66);
    assert_eq!(fixture.list(true, 0).unwrap().total_count, 67);
}

#[test]
fn cycles_withhold_the_entire_public_result_even_when_a_healthy_row_fits_the_limit() {
    let fixture = Fixture::new();
    fixture.seed(1);
    let first = fixture.seed(2);
    let second = fixture.seed(3);
    fixture.edge(1, &first, &second);
    fixture.edge(2, &second, &first);
    let before = fixture.state();
    let error = fixture.list(true, 1).unwrap_err();
    assert!(matches!(error, DomainError::Storage { .. }));
    for private in [first.as_str(), second.as_str(), "PRIVATE-LINEAGE"] {
        assert!(!format!("{error:?}").contains(private));
    }
    assert_eq!(fixture.state(), before);
    fixture.db.begin_read_snapshot().unwrap();
    fixture.db.rollback_read_snapshot().unwrap();
}

#[test]
fn multiple_predecessors_are_not_resolved_by_id_order_or_input_order() {
    for reversed in [false, true] {
        let fixture = Fixture::new();
        let left = fixture.seed(1);
        let right = fixture.seed(2);
        let child = fixture.seed(3);
        let parents = if reversed { [&right, &left] } else { [&left, &right] };
        for (number, parent) in parents.into_iter().enumerate() {
            fixture.edge(number, &child, parent);
        }
        // One-root query hits the bounded fan-out lookahead; a multi-root query
        // must also reject repeated source identities within its row bound.
        assert!(fixture.depth(&child).is_err());
        assert!(fixture.list(true, 0).is_err());
    }
}

#[test]
fn foreign_or_nondecision_ancestors_never_count_as_owned_decision_history() {
    for foreign in [false, true] {
        let fixture = Fixture::new();
        let other = crate::models::WorkspaceId::from_uuid(uuid::Uuid::from_u128(999)).to_string();
        fixture.db.insert_workspace(&other, &CreateWorkspaceInput {
            path: fixture.root.path().join("other").to_string_lossy().into_owned(),
            name: None,
        }).unwrap();
        let parent = fixture.seed_owned(1,
            if foreign { &other } else { &fixture.workspace },
            if foreign { "decision" } else { "fact" });
        let child = fixture.seed(2);
        fixture.edge(1, &child, &parent);
        let error = fixture.depth(&child).unwrap_err();
        assert!(matches!(error, DomainError::Storage { .. }));
        assert!(!format!("{error:?}").contains(&parent));
        assert!(!format!("{error:?}").contains(&other));
    }
}

#[test]
fn ancestry_uses_identity_not_hidden_bodies_sidecars_or_current_lifecycle() {
    let fixture = Fixture::new();
    let ids = fixture.chain(3);
    fixture.db.insert_memory_seal(&ids[1], &format!("blake3:{}", "a".repeat(64)), TIME).unwrap();
    fixture.db.tombstone_memory(&ids[0]).unwrap();
    fixture.db.execute_raw(&format!(
        "UPDATE memories SET content = 'PRIVATE-ANCESTOR-BODY', typed_fields_json = '{{\"chosen\":7}}' WHERE id IN ('{}', '{}')",
        ids[0], ids[1]
    )).unwrap();
    let before = fixture.state();
    let report = fixture.list(false, 0).unwrap();
    assert_eq!(report.decisions.len(), 1);
    assert_eq!(report.decisions[0].chain_depth, 2);
    assert!(!report.data_json().to_string().contains("PRIVATE-ANCESTOR-BODY"));
    assert_eq!(fixture.state(), before);
}

#[test]
fn frontier_pages_deduplicate_roots_and_reuse_shared_ancestry_without_repeat_reads() {
    let fixture = Fixture::new();
    let parent = fixture.seed(1);
    let mut roots = Vec::new();
    fixture.db.with_transaction(|| {
        for number in 1000..1257 {
            let id = fixture.seed(number);
            fixture.edge(number as usize, &id, &parent);
            roots.push(id);
        }
        Ok(())
    }).unwrap();
    let mut inputs = roots.iter().map(String::as_str).collect::<Vec<_>>();
    inputs.extend([roots[0].as_str(), roots[256].as_str()]);
    inputs.reverse();
    let mut lineage = DecisionLineage::new(&fixture.workspace);
    let mut seen = BTreeSet::new();
    let mut sizes = Vec::new();
    fixture.db.begin_read_snapshot().unwrap();
    lineage.load_with_observer(&fixture.db, &inputs, |page| {
        sizes.push(page.len());
        for id in page { assert!(seen.insert(id.clone())); }
        Ok(())
    }).unwrap();
    assert_eq!(seen.len(), 258);
    assert_eq!(sizes, [256, 2]);
    for id in &roots { assert_eq!(lineage.depth(id).unwrap(), 1); }
    lineage.load_with_observer(&fixture.db, &inputs, |_| {
        panic!("completed chains must not repeat storage reads")
    }).unwrap();
    fixture.db.commit_read_snapshot().unwrap();
}

#[test]
fn incoming_undirected_and_other_relations_do_not_invent_ancestry() {
    let fixture = Fixture::new();
    let root = fixture.seed(1);
    let incoming = fixture.seed(2);
    let unrelated = fixture.seed(3);
    fixture.edge(1, &incoming, &root);
    fixture.edge_kind(2, &root, &unrelated, MemoryLinkRelation::Supersedes, false);
    fixture.edge_kind(3, &root, &incoming, MemoryLinkRelation::Related, true);
    assert_eq!(fixture.depth(&root).unwrap(), 0);
    assert_eq!(fixture.depth(&incoming).unwrap(), 1);
}

#[test]
fn unrelated_retired_corruption_does_not_poison_an_independent_current_head() {
    let fixture = Fixture::new();
    let visible = fixture.seed(1);
    let left = fixture.seed(2);
    let right = fixture.seed(3);
    fixture.edge(1, &left, &right);
    fixture.edge(2, &right, &left);
    fixture.db.mark_memory_superseded(&left, TIME).unwrap();
    fixture.db.mark_memory_superseded(&right, TIME).unwrap();
    let report = fixture.list(false, 0).unwrap();
    assert_eq!(report.decisions.len(), 1);
    assert_eq!(report.decisions[0].memory_id, visible);
    assert_eq!(report.decisions[0].chain_depth, 0);
    assert!(fixture.list(true, 0).is_err());
}

#[test]
fn a_concurrent_edge_change_belongs_to_the_next_snapshot_not_the_next_frontier() {
    let fixture = Fixture::new();
    let ids = (1..=6).map(|number| fixture.seed(number)).collect::<Vec<_>>();
    fixture.edge(1, &ids[2], &ids[1]);
    let changed = fixture.edge(2, &ids[1], &ids[0]);
    fixture.edge(3, &ids[3], &ids[4]);
    fixture.edge(4, &ids[4], &ids[5]);
    let reader = DbConnection::open_file_read_only(fixture.root.path().join(".ee/ee.db")).unwrap();
    reader.begin_read_snapshot().unwrap();
    let mut captured = DecisionLineage::new(&fixture.workspace);
    let mut pages = 0;
    captured.load_with_observer(&reader, &[ids[2].as_str()], |_| {
        pages += 1;
        if pages == 1 {
            fixture.db.execute_raw(&format!(
                "UPDATE memory_links SET dst_memory_id = '{}' WHERE id = '{changed}'", ids[3]
            )).unwrap();
        }
        Ok(())
    }).unwrap();
    assert_eq!(captured.depth(&ids[2]).unwrap(), 2);
    assert!(reader.begin_read_snapshot().is_err(), "caller still owns the snapshot");
    reader.commit_read_snapshot().unwrap();
    reader.begin_read_snapshot().unwrap();
    assert_eq!(chain_depth(&reader, &fixture.workspace, &ids[2]).unwrap(), 4);
    reader.commit_read_snapshot().unwrap();
}

#[test]
fn missing_identity_or_storage_is_not_a_zero_depth_and_does_not_release_the_caller() {
    let fixture = Fixture::new();
    let root = fixture.seed(1);
    let missing = MemoryId::from_uuid(uuid::Uuid::from_u128(2)).to_string();
    fixture.db.begin_read_snapshot().unwrap();
    for id in [missing.as_str(), "' OR 1 = 1 --"] {
        assert!(chain_depth(&fixture.db, &fixture.workspace, id).is_err());
        assert!(fixture.db.begin_read_snapshot().is_err());
    }
    fixture.db.commit_read_snapshot().unwrap();
    fixture.db.execute_raw("ALTER TABLE memory_links RENAME TO unavailable_private_links").unwrap();
    fixture.db.begin_read_snapshot().unwrap();
    let error = chain_depth(&fixture.db, &fixture.workspace, &root).unwrap_err();
    assert!(!format!("{error:?}").contains("unavailable_private_links"));
    assert!(fixture.db.begin_read_snapshot().is_err());
    fixture.db.commit_read_snapshot().unwrap();
}

#[test]
fn invalid_predecessor_history_aborts_preview_and_writer_without_partial_replacement() {
    let fixture = Fixture::new();
    let ids = fixture.chain(3);
    // The only current head points into a cycle through a retained predecessor.
    fixture.db.execute_raw(&format!(
        "UPDATE memory_links SET dst_memory_id = '{}' WHERE id = 'link_{:026}'", ids[2], 2
    )).unwrap();
    let before = fixture.state();
    for dry in [true, false] {
        let error = decide_record(&fixture.request(&ids[2], dry)).unwrap_err();
        assert!(matches!(error, DomainError::Storage { .. }));
        assert_eq!(fixture.state(), before);
    }
}
