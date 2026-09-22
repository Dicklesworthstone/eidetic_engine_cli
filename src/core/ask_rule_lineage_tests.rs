//! Real-store regression coverage for rule lineage independent of parent admission.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{AskRequest, ask_data_json, evaluate_ask};
use crate::db::{CreateMemoryInput, CreateProceduralRuleInput, CreateWorkspaceInput};

const WORKSPACE: &str = "wsp_00000000000000000000000081";
const OTHER: &str = "wsp_00000000000000000000000082";
const BODY: &str = "Run cargo fmt before every release tag.";
const PARENT_BODY: &str = "PRIVATE-LINEAGE-CANARY: historical incident, not current guidance.";

struct Fixture {
    _root: tempfile::TempDir,
    database: std::path::PathBuf,
    db: DbConnection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().canonicalize().unwrap();
        let database = physical.join("ask.db");
        let db = DbConnection::open_file(&database).unwrap();
        db.migrate().unwrap();
        for (id, path) in [
            (WORKSPACE, physical.clone()),
            (OTHER, physical.join("other")),
        ] {
            db.insert_workspace(
                id,
                &CreateWorkspaceInput {
                    path: path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
        }
        Self {
            _root: root,
            database,
            db,
        }
    }

    fn parent(&self, n: usize, actor: &str) -> String {
        let id = format!("mem_{n:026}");
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: WORKSPACE.to_owned(),
                    content: PARENT_BODY.to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some(format!("agent:{actor}")),
                    provenance_uri: Some(format!("agent://{actor}/lineage-test")),
                    tags: Vec::new(),
                    valid_from: Some("2000-01-01T00:00:00Z".to_owned()),
                    valid_to: Some("2001-01-01T00:00:00Z".to_owned()),
                },
            )
            .unwrap();
        id
    }

    fn rule(&self, n: usize, sources: &[String]) -> String {
        let id = format!("rule_{n:026}");
        self.db
            .insert_procedural_rule(
                &id,
                &CreateProceduralRuleInput {
                    workspace_id: WORKSPACE.to_owned(),
                    content: BODY.to_owned(),
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    scope: "global".to_owned(),
                    scope_pattern: None,
                    maturity: "candidate".to_owned(),
                    protected: false,
                    source_memory_ids: sources.to_vec(),
                    tags: Vec::new(),
                },
            )
            .unwrap();
        id
    }

    fn load(&self, scope: MemoryScope) -> AskCorpus {
        load_with_scope(&self.db, scope).unwrap()
    }
}

fn reference() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn scope_context(scope: MemoryScope) -> MemoryScopeContext {
    MemoryScopeContext {
        scope,
        strict_scope: false,
        current_agent: Some("Alice".to_owned()),
        team_members: BTreeSet::from(["Bob".to_owned()]),
    }
}

fn load_with_scope(db: &DbConnection, scope: MemoryScope) -> Result<AskCorpus, DomainError> {
    load_corpus_with_scope_boundary(
        db,
        WORKSPACE,
        reference(),
        || Ok(scope_context(scope)),
        || Ok(()),
    )
}

fn candidate_ids(corpus: &AskCorpus) -> BTreeSet<&str> {
    corpus
        .candidates
        .iter()
        .map(|candidate| candidate.memory_id.as_str())
        .collect()
}

fn answer_json(corpus: &AskCorpus) -> serde_json::Value {
    let report = evaluate_ask(
        &AskRequest {
            question: "Which command must run before every release tag?".to_owned(),
            contradictions: corpus.contradictions.clone(),
            native_sources: corpus.native_sources.clone(),
            ..AskRequest::default()
        },
        &corpus.candidates,
    );
    ask_data_json(&report)
}

#[test]
fn retiring_an_incident_does_not_hide_its_independently_active_learned_rule() {
    for update in [
        "tombstoned_at = '2002-01-01T00:00:00Z'",
        "superseded_at = '2002-01-01T00:00:00Z'",
        "valid_to = '2001-01-01T00:00:00Z'",
    ] {
        let f = Fixture::new();
        let parent = f.parent(1, "Alice");
        let rule = f.rule(1, std::slice::from_ref(&parent));
        f.db.execute_raw(&format!(
            "UPDATE memories SET {update} WHERE id = '{parent}'"
        ))
        .unwrap();
        f.db.insert_memory_seal(
            &parent,
            &crate::models::memory_seal_commitment(PARENT_BODY.as_bytes()),
            "2000-01-01T00:00:00Z",
        )
        .unwrap();
        let before = f.db.get_memory(&parent).unwrap();
        let audits = f.db.count_table_rows("audit_log").unwrap();
        for scope in [MemoryScope::SelfOnly, MemoryScope::Team] {
            let corpus = f.load(scope);
            assert_eq!(candidate_ids(&corpus), BTreeSet::from([rule.as_str()]));
            assert_eq!(corpus.candidates[0].content, BODY);
            assert!(corpus.native_sources.contains_key(&rule));
            let output = answer_json(&corpus).to_string();
            assert!(output.contains(&rule) && output.contains(BODY));
            assert!(!output.contains(PARENT_BODY));
            assert!(!output.contains(&parent));
        }
        assert_eq!(f.db.get_memory(&parent).unwrap(), before);
        assert_eq!(f.db.count_table_rows("audit_log").unwrap(), audits);
    }
}

#[test]
fn all_retired_parents_must_be_attributed_not_just_one_authorized_contributor() {
    let f = Fixture::new();
    let alice = f.parent(1, "Alice");
    let bob = f.parent(2, "Bob");
    let eve = f.parent(3, "Eve");
    let own = f.rule(1, std::slice::from_ref(&alice));
    let team = f.rule(2, &[alice, bob.clone()]);
    let mixed = f.rule(3, &[bob, eve]);
    let unowned = f.rule(4, &[]);
    f.db.execute_raw("UPDATE memories SET tombstoned_at = '2002-01-01T00:00:00Z'")
        .unwrap();
    let self_only = f.load(MemoryScope::SelfOnly);
    assert_eq!(candidate_ids(&self_only), BTreeSet::from([own.as_str()]));
    let team_corpus = f.load(MemoryScope::Team);
    assert_eq!(
        candidate_ids(&team_corpus),
        BTreeSet::from([own.as_str(), team.as_str()])
    );
    let public = f.load(MemoryScope::Verified);
    assert_eq!(
        candidate_ids(&public),
        BTreeSet::from([
            own.as_str(),
            team.as_str(),
            mixed.as_str(),
            unowned.as_str()
        ])
    );
}

#[test]
fn foreign_parent_ownership_rejects_only_its_rule_in_every_memory_scope() {
    let f = Fixture::new();
    let parent = f.parent(1, "Alice");
    let foreign = f.rule(1, std::slice::from_ref(&parent));
    let good_parent = f.parent(2, "Alice");
    let good = f.rule(2, &[good_parent]);
    let sourceless = f.rule(3, &[]);
    f.db.execute_raw(&format!(
        "UPDATE memories SET workspace_id = '{OTHER}' WHERE id = '{parent}'"
    ))
    .unwrap();
    for scope in [
        MemoryScope::Workspace,
        MemoryScope::Swarm,
        MemoryScope::Verified,
        MemoryScope::Global,
        MemoryScope::SelfOnly,
        MemoryScope::Team,
    ] {
        let corpus = f.load(scope);
        let ids = candidate_ids(&corpus);
        assert!(ids.contains(good.as_str()));
        assert!(!ids.contains(foreign.as_str()));
        assert_eq!(
            ids.contains(sourceless.as_str()),
            !matches!(scope, MemoryScope::SelfOnly | MemoryScope::Team)
        );
        assert!(!answer_json(&corpus).to_string().contains(&foreign));
    }
}

#[test]
fn missing_and_noncanonical_parent_ids_never_gain_lineage_authority() {
    let f = Fixture::new();
    let parent = f.parent(1, "Alice");
    let missing = "mem_00000000000000000000009999";
    let requested = BTreeSet::from([parent.as_str(), missing, "mem_not_an_id"]);
    for scope in [
        MemoryScope::Workspace,
        MemoryScope::SelfOnly,
        MemoryScope::Team,
    ] {
        let lineage =
            load_rule_lineage(&f.db, WORKSPACE, &scope_context(scope), &requested).unwrap();
        assert_eq!(lineage.owned, BTreeSet::from([parent.clone()]));
        if scope == MemoryScope::Workspace {
            assert!(lineage.attributed.is_empty());
        } else {
            assert_eq!(lineage.attributed, BTreeSet::from([parent.clone()]));
        }
    }
}

#[test]
fn source_less_rules_do_not_need_a_parent_query_and_keep_native_scope_policy() {
    let f = Fixture::new();
    let rule = f.rule(1, &[]);
    for scope in [
        MemoryScope::Workspace,
        MemoryScope::Verified,
        MemoryScope::Global,
    ] {
        assert_eq!(
            candidate_ids(&f.load(scope)),
            BTreeSet::from([rule.as_str()])
        );
    }
    for scope in [MemoryScope::SelfOnly, MemoryScope::Team] {
        assert!(f.load(scope).candidates.is_empty());
    }
    f.db.execute_raw("ALTER TABLE memories RENAME TO unavailable_lineage_memories")
        .unwrap();
    let lineage = load_rule_lineage(
        &f.db,
        WORKSPACE,
        &scope_context(MemoryScope::SelfOnly),
        &BTreeSet::new(),
    )
    .expect("empty lineage performs no parent storage read");
    assert!(lineage.owned.is_empty() && lineage.attributed.is_empty());
}

#[test]
fn source_attribution_and_rule_body_are_read_from_the_same_snapshot() {
    for update in [
        "trust_subclass = 'agent:Eve'".to_owned(),
        format!("workspace_id = '{OTHER}'"),
    ] {
        let f = Fixture::new();
        let parent = f.parent(1, "Alice");
        let rule = f.rule(1, std::slice::from_ref(&parent));
        let reader = DbConnection::open_file_read_only(&f.database).unwrap();
        let corpus = load_corpus_with_scope_boundary(
            &reader,
            WORKSPACE,
            reference(),
            || Ok(scope_context(MemoryScope::SelfOnly)),
            || {
                f.db.execute_raw(&format!(
                    "UPDATE memories SET {update} WHERE id = '{parent}'"
                ))
                .unwrap();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(candidate_ids(&corpus), BTreeSet::from([rule.as_str()]));
        assert!(
            load_with_scope(&reader, MemoryScope::SelfOnly)
                .unwrap()
                .candidates
                .is_empty()
        );
        reader
            .begin_read_snapshot()
            .expect("ask released its owned snapshot");
        reader.rollback_read_snapshot().unwrap();
    }
}

#[test]
fn multi_page_lineage_checks_the_last_parent_and_does_not_mutate_history() {
    let f = Fixture::new();
    let mut parents = Vec::new();
    f.db.with_transaction(|| {
        for n in 1..=513 {
            parents.push(f.parent(n, "Alice"));
        }
        Ok(())
    })
    .unwrap();
    let rule = f.rule(1, &parents);
    let audits = f.db.count_table_rows("audit_log").unwrap();
    let memories = f.db.count_table_rows("memories").unwrap();
    f.db.execute_raw("UPDATE memories SET tombstoned_at = '2002-01-01T00:00:00Z'")
        .unwrap();
    for scope in [MemoryScope::SelfOnly, MemoryScope::Workspace] {
        assert_eq!(
            candidate_ids(&f.load(scope)),
            BTreeSet::from([rule.as_str()])
        );
    }
    f.db.execute_raw(&format!(
        "UPDATE memories SET trust_subclass = 'agent:Eve' WHERE id = '{}'",
        parents[512]
    ))
    .unwrap();
    assert!(f.load(MemoryScope::SelfOnly).candidates.is_empty());
    assert_eq!(f.load(MemoryScope::Workspace).candidates.len(), 1);
    f.db.execute_raw(&format!(
        "UPDATE memories SET workspace_id = '{OTHER}' WHERE id = '{}'",
        parents[512]
    ))
    .unwrap();
    assert!(f.load(MemoryScope::Workspace).candidates.is_empty());
    assert_eq!(f.db.count_table_rows("audit_log").unwrap(), audits);
    assert_eq!(f.db.count_table_rows("memories").unwrap(), memories);
}

#[test]
fn parent_storage_failure_withholds_the_rule_without_exposing_storage_details() {
    let f = Fixture::new();
    let parent = f.parent(1, "Alice");
    f.rule(1, std::slice::from_ref(&parent));
    f.db.execute_raw("ALTER TABLE memories RENAME TO unavailable_lineage_memories")
        .unwrap();
    let error = load_rule_lineage(
        &f.db,
        WORKSPACE,
        &scope_context(MemoryScope::Workspace),
        &BTreeSet::from([parent.as_str()]),
    )
    .err()
    .expect("parent lookup is required, not silently source-less");
    assert!(matches!(error, DomainError::Storage { .. }));
    let diagnostic = format!("{error:?}");
    assert!(!diagnostic.contains(PARENT_BODY));
    assert!(!diagnostic.contains(&parent));
    assert!(!diagnostic.contains("unavailable_lineage_memories"));
}
