#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{
    AskReport, AskRequest, ask_data_json, evaluate_ask, record_ask_retrieval_best_effort,
    render_ask_markdown,
};
use crate::db::{
    CreateMemoryInput, CreateProceduralRuleInput, CreateWorkspaceInput, InsertTeamMemberInput,
};
use crate::models::{RuleId, WorkspaceId};
use crate::pack::PackEntityRef;

const BODY: &str = "Run cargo fmt before every release tag.";
const QUESTION: &str = "Which command must run before every release tag?";

fn fixture() -> (tempfile::TempDir, DbConnection, String) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".ee")).unwrap();
    let db = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
    db.migrate().unwrap();
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([57; 16])).to_string();
    db.insert_workspace(
        &workspace,
        &CreateWorkspaceInput {
            path: root.path().to_string_lossy().into_owned(),
            name: None,
        },
    )
    .unwrap();
    (root, db, workspace)
}

fn memory(db: &DbConnection, workspace: &str, n: usize, actor: &str, body: &str) -> String {
    let id = format!("mem_{n:026}");
    db.insert_memory(
        &id,
        &CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            content: body.to_owned(),
            level: "semantic".to_owned(),
            kind: "note".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: Some(format!("agent:{actor}")),
            provenance_uri: Some(format!("manual://native-test/{n}")),
            tags: Vec::new(),
            valid_from: Some("2000-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        },
    )
    .unwrap();
    id
}

fn rule(db: &DbConnection, workspace: &str, n: usize, body: &str, sources: &[String]) -> String {
    let id = format!("rule_{n:026}");
    db.insert_procedural_rule(
        &id,
        &CreateProceduralRuleInput {
            workspace_id: workspace.to_owned(),
            content: body.to_owned(),
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit".to_owned(),
            scope: "workspace".to_owned(),
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

fn request(corpus: &AskCorpus) -> AskRequest {
    AskRequest {
        question: QUESTION.to_owned(),
        native_sources: corpus.native_sources.clone(),
        contradictions: corpus.contradictions.clone(),
        ..AskRequest::default()
    }
}

fn answer(corpus: &AskCorpus) -> AskReport {
    evaluate_ask(&request(corpus), &corpus.candidates)
}

fn scoped(
    db: &DbConnection,
    workspace: &str,
    scope: MemoryScope,
    actor: Option<&str>,
) -> AskCorpus {
    load_corpus_with_scope_boundary(
        db,
        workspace,
        Utc::now(),
        || {
            let mut context = scope_context(db, workspace, scope)?;
            context.current_agent = actor.map(str::to_owned);
            Ok(context)
        },
        || Ok(()),
    )
    .unwrap()
}

#[test]
fn native_rule_is_answered_and_audited_without_creating_a_memory_alias() {
    let (root, db, workspace) = fixture();
    let id = rule(&db, &workspace, 1, BODY, &[]);
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    let projection = crate::search::RuleIndexProjection::new(
        db.get_procedural_rule(&id).unwrap().unwrap(),
        root.path(),
        vec![],
        vec![],
    );
    let report = answer(&corpus);
    assert!(!report.abstained, "{report:?}");
    let data = ask_data_json(&report);
    let citation = &data["citations"][0];
    assert_eq!(citation["ruleId"], id);
    assert_eq!(citation["entityId"], id);
    assert_eq!(citation["entityKind"], "rule");
    assert_eq!(citation["entityRevision"], projection.entity_revision());
    assert_eq!(citation["provenanceUri"], format!("ee://rule/{id}"));
    assert!(citation.get("memoryId").is_none());
    assert_eq!(citation["text"], BODY);
    let markdown = render_ask_markdown(&report);
    assert!(markdown.contains(&id) && markdown.contains(projection.entity_revision()));
    assert!(db.list_memories(&workspace, None, true).unwrap().is_empty());
    record_ask_retrieval_best_effort(&db, &workspace, &report);
    let audits = db.list_audit_by_target("rule", &id, None).unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(
        audits[0].action,
        crate::db::audit_actions::SEARCH_RETURNED_MEM
    );
    let details: serde_json::Value =
        serde_json::from_str(audits[0].details.as_ref().unwrap()).unwrap();
    assert_eq!(details["entityRevision"], projection.entity_revision());
    assert!(!details.to_string().contains(QUESTION));
    assert!(
        db.list_audit_by_target("memory", &id, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_lifecycle_scope_and_privacy_are_applied_before_answering_or_hints() {
    let (_root, db, workspace) = fixture();
    let good = rule(&db, &workspace, 1, BODY, &[]);
    for (n, maturity) in [(2, "draft"), (3, "deprecated"), (4, "superseded")] {
        let id = rule(&db, &workspace, n, BODY, &[]);
        db.execute_raw(&format!(
            "UPDATE procedural_rules SET maturity = '{maturity}' WHERE id = '{id}'"
        ))
        .unwrap();
    }
    let replaced = rule(&db, &workspace, 5, BODY, &[]);
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET superseded_by = '{good}' WHERE id = '{replaced}'"
    ))
    .unwrap();
    let dead = rule(&db, &workspace, 6, BODY, &[]);
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET tombstoned_at = '2026-01-01T00:00:00Z' WHERE id = '{dead}'"
    ))
    .unwrap();
    let narrow = rule(&db, &workspace, 7, BODY, &[]);
    db.execute_raw(&format!("UPDATE procedural_rules SET scope = 'directory', scope_pattern = 'src' WHERE id = '{narrow}'")).unwrap();
    rule(
        &db,
        &workspace,
        8,
        "Run cargo fmt before release with password=native-secret-canary.",
        &[],
    );
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(corpus.candidates.len(), 1);
    assert_eq!(corpus.candidates[0].memory_id, good);
    let mut weak = request(&corpus);
    weak.min_confidence = 1.0;
    let data = ask_data_json(&evaluate_ask(&weak, &corpus.candidates));
    assert_eq!(data["abstained"], true);
    assert_eq!(data["nearestEvidence"][0]["ruleId"], good);
    assert!(data["nearestEvidence"][0].get("memoryId").is_none());
    assert!(!data.to_string().contains("native-secret-canary"));
}

#[test]
fn native_abstention_assistance_never_mislabels_a_rule_as_a_memory() {
    let (_root, db, workspace) = fixture();
    let id = rule(&db, &workspace, 1, BODY, &[]);
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    let mut weak = request(&corpus);
    weak.question = "release".to_owned();
    weak.min_confidence = 1.0;
    let report = evaluate_ask(&weak, &corpus.candidates);
    let data = ask_data_json(&report);
    assert_eq!(data["queryAssist"]["didYouMean"][0]["ruleId"], id);
    assert_eq!(
        data["queryAssist"]["reformulations"][0]["matchedRuleId"],
        id
    );
    assert!(!data.to_string().contains("memoryId"));
    assert!(!data.to_string().contains("matchedMemoryId"));
    record_ask_retrieval_best_effort(&db, &workspace, &report);
    assert!(
        db.list_audit_by_target("rule", &id, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn shared_derivation_inputs_do_not_manufacture_independent_corroboration() {
    let (_root, db, workspace) = fixture();
    let parent = memory(&db, &workspace, 1, "Alice", BODY);
    let baseline = answer(&load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap());
    for n in 1..=4 {
        rule(&db, &workspace, n, BODY, std::slice::from_ref(&parent));
    }
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    let report = answer(&corpus);
    assert_eq!(report.confidence.to_bits(), baseline.confidence.to_bits());
    assert_eq!(report.confidence_components.corroboration, 1.0);
    memory(&db, &workspace, 2, "Bob", BODY);
    let independent = answer(&load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap());
    assert!(independent.confidence > report.confidence);
    let mut reversed = corpus.clone();
    reversed.candidates.reverse();
    assert_eq!(ask_data_json(&answer(&reversed)), ask_data_json(&report));
}

#[test]
fn rules_sharing_an_unselected_parent_are_still_correlated() {
    let (_root, db, workspace) = fixture();
    let parent = memory(&db, &workspace, 1, "Alice", "Historical source material.");
    for n in 1..=3 {
        rule(&db, &workspace, n, BODY, std::slice::from_ref(&parent));
    }
    db.execute_raw(&format!(
        "UPDATE memories SET valid_to = '2001-01-01T00:00:00Z' WHERE id = '{parent}'"
    ))
    .unwrap();
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(corpus.candidates.len(), 3);
    let report = answer(&corpus);
    assert!(!report.abstained);
    assert_eq!(report.confidence_components.corroboration, 1.0);
    assert_eq!(
        report.native_sources.len(),
        1,
        "only exposed sources belong in the report"
    );
}

#[test]
fn rules_require_complete_producer_attribution_for_self_and_team_scope() {
    let (_root, db, workspace) = fixture();
    let alice = memory(&db, &workspace, 1, "Alice", "An observation by Alice.");
    let bob = memory(&db, &workspace, 2, "Bob", "An observation by Bob.");
    let own = rule(&db, &workspace, 1, BODY, std::slice::from_ref(&alice));
    let shared = rule(&db, &workspace, 2, BODY, &[alice, bob]);
    rule(&db, &workspace, 3, BODY, &[]);
    let mine = scoped(&db, &workspace, MemoryScope::SelfOnly, Some("Alice"));
    assert_eq!(mine.native_sources.keys().collect::<Vec<_>>(), [&own]);
    assert!(
        scoped(&db, &workspace, MemoryScope::SelfOnly, None)
            .native_sources
            .is_empty()
    );
    db.insert_team_member(&InsertTeamMemberInput {
        member_id: format!("mbr_{:032x}", 1),
        team_id: "team_native".to_owned(),
        workspace_id: workspace.clone(),
        display_name: "Bob".to_owned(),
        state: "active".to_owned(),
        is_self: false,
        origin_node_id: "node_Bob".to_owned(),
        bound_via: "invite_ceremony".to_owned(),
        joined_at: "2020-01-01T00:00:00Z".to_owned(),
    })
    .unwrap();
    let team = scoped(&db, &workspace, MemoryScope::Team, Some("Alice"));
    assert_eq!(
        team.native_sources.keys().collect::<Vec<_>>(),
        [&own, &shared]
    );
    db.execute_raw("UPDATE team_members SET state = 'removed'")
        .unwrap();
    assert_eq!(
        scoped(&db, &workspace, MemoryScope::Team, Some("Alice"))
            .native_sources
            .keys()
            .collect::<Vec<_>>(),
        [&own]
    );
}

#[test]
fn global_and_verified_native_rules_do_not_widen_the_workspace() {
    let (_root, db, workspace) = fixture();
    let global = rule(&db, &workspace, 1, BODY, &[]);
    let local = rule(&db, &workspace, 2, BODY, &[]);
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET scope = 'global' WHERE id = '{global}'"
    ))
    .unwrap();
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET trust_class = 'agent_assertion' WHERE id = '{local}'"
    ))
    .unwrap();
    assert_eq!(
        scoped(&db, &workspace, MemoryScope::Global, None)
            .native_sources
            .keys()
            .collect::<Vec<_>>(),
        [&global]
    );
    assert_eq!(
        scoped(&db, &workspace, MemoryScope::Verified, None)
            .native_sources
            .keys()
            .collect::<Vec<_>>(),
        [&global]
    );
    let foreign = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([58; 16])).to_string();
    db.insert_workspace(
        &foreign,
        &CreateWorkspaceInput {
            path: "/other-native-workspace".to_owned(),
            name: None,
        },
    )
    .unwrap();
    let foreign_rule = rule(&db, &foreign, 3, BODY, &[]);
    let data = ask_data_json(&answer(
        &load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap(),
    ));
    assert!(!data.to_string().contains(&foreign_rule));
}

#[test]
fn concurrent_rule_revision_remains_in_the_same_snapshot_as_its_body() {
    let (root, db, workspace) = fixture();
    let id = rule(&db, &workspace, 1, BODY, &[]);
    let old = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    let writer = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
    let pinned = load_corpus_with_boundary(&db, &workspace, Utc::now(), || {
        writer.with_transaction(|| {
            writer.execute_raw(&format!("UPDATE procedural_rules SET content = 'Do not run cargo fmt before every release tag.' WHERE id = '{id}'"))?;
            Ok(())
        }).unwrap();
        Ok(())
    }).unwrap();
    assert_eq!(pinned.native_sources[&id], old.native_sources[&id]);
    assert_eq!(pinned.candidates[0].content, BODY);
    let next = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_ne!(
        next.native_sources[&id].entity_revision,
        old.native_sources[&id].entity_revision
    );
    assert!(next.candidates[0].content.starts_with("Do not"));
}

#[test]
fn unavailable_rule_provenance_fails_closed_and_releases_the_snapshot() {
    let (_root, db, workspace) = fixture();
    rule(&db, &workspace, 1, BODY, &[]);
    db.execute_raw("ALTER TABLE rule_source_memories RENAME TO unavailable_rule_sources")
        .unwrap();
    let error = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap_err();
    assert!(!error.message().contains("unavailable_rule_sources"));
    db.begin_read_snapshot().unwrap();
    db.commit_read_snapshot().unwrap();
}

#[test]
fn native_identity_metadata_cannot_be_missing_or_substituted() {
    let (_root, db, workspace) = fixture();
    let id = rule(&db, &workspace, 1, BODY, &[]);
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    for fault in ["missing", "identity", "revision"] {
        let mut bad = request(&corpus);
        match fault {
            "missing" => bad.native_sources.clear(),
            "identity" => {
                bad.native_sources.get_mut(&id).unwrap().entity =
                    PackEntityRef::Rule(RuleId::from_str(&format!("rule_{:026}", 2)).unwrap())
            }
            _ => {
                bad.native_sources.get_mut(&id).unwrap().entity_revision = "unversioned".to_owned()
            }
        }
        let report = evaluate_ask(&bad, &corpus.candidates);
        assert!(report.extractiveness_violated && report.abstained);
        assert!(report.native_sources.is_empty());
        assert!(ask_data_json(&report).get("queryAssist").is_none());
    }
}

#[test]
fn opposing_native_rules_are_cited_as_separate_typed_sides() {
    let (_root, db, workspace) = fixture();
    rule(&db, &workspace, 1, BODY, &[]);
    rule(
        &db,
        &workspace,
        2,
        "Do not run cargo fmt before every release tag.",
        &[],
    );
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    let report = answer(&corpus);
    assert!(report.conflict_detected && !report.abstained);
    let data = ask_data_json(&report);
    assert_eq!(data["sides"].as_array().unwrap().len(), 2);
    for side in data["sides"].as_array().unwrap() {
        let citation = &side["citations"][0];
        assert_eq!(citation["entityKind"], "rule");
        assert!(citation.get("memoryId").is_none());
    }
}
