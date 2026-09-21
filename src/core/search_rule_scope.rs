//! Scope native rules by their own identity, never by a fabricated MemoryId.
//!
//! The ordinary memory scope gate used to discard every rule in verified,
//! global, self and team scopes because no rule lives in the memories table.
//! Reuse native source admission and the caller's scope context/snapshot. A
//! derived rule has its own lifecycle; its parents establish attribution only.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use crate::core::memory_scope::MemoryScopeContext;
use crate::db::DbConnection;
use crate::models::{MemoryScope, RuleScope, TrustClass};

use super::{SearchHit, SearchOptions, canonical_metadata, is_rule_hit, load_projections};

/// The caller owns the source snapshot and the final scope statistics/strict
/// decision. Returning no metadata means exclusion, not indexed fallback.
/// Workspace/swarm use their existing passthrough path and do no extra reads.
pub(in crate::core::search) fn scoped_metadata(
    options: &SearchOptions,
    hits: &[SearchHit],
    context: &MemoryScopeContext,
    connection: &DbConnection,
) -> crate::db::Result<BTreeMap<String, serde_json::Value>> {
    let ids: BTreeSet<_> = hits
        .iter()
        .filter(|hit| is_rule_hit(hit))
        .map(|hit| hit.doc_id.as_str())
        .collect();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let projections = load_projections(options, &ids, Some(connection), || {});
    // Producer parsing belongs to MemoryScopeContext. Only attribution scopes
    // need parent records; ordinary verified/global scope does not hydrate any
    // parent body. Bind-safe pages also handle large multi-source derivations.
    let mut parents = BTreeMap::new();
    if matches!(context.scope, MemoryScope::SelfOnly | MemoryScope::Team) {
        let source_ids: BTreeSet<_> = projections
            .values()
            .flat_map(|projection| projection.source_memory_ids().iter().map(String::as_str))
            .collect();
        let source_ids: Vec<_> = source_ids.into_iter().collect();
        for page in source_ids.chunks(super::RULE_RELATION_PAGE_SIZE) {
            parents.extend(connection.get_memories_batch(page)?);
        }
    }
    let mut admitted = BTreeMap::new();
    for hit in hits.iter().filter(|hit| is_rule_hit(hit)) {
        let Some(projection) = projections.get(&hit.doc_id) else {
            continue;
        };
        // Do not revive a stale supplied revision or launder peer metadata as
        // locally owned. Semantic candidates may have no revision at all.
        let revision_matches = hit
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("entity_revision"))
            .is_none_or(|revision| revision.as_str() == Some(projection.entity_revision()));
        if !revision_matches
            || !matches!(
                super::super::mesh_query_visibility(hit.metadata.as_ref()),
                super::MeshQueryVisibility::Local
            )
        {
            continue;
        }
        let rule = projection.rule();
        let visible = match context.scope {
            MemoryScope::Workspace | MemoryScope::Swarm => true,
            MemoryScope::Verified => matches!(
                TrustClass::from_str(&rule.trust_class),
                Ok(TrustClass::HumanExplicit
                    | TrustClass::PeerHumanAttested
                    | TrustClass::AgentValidated)
            ),
            MemoryScope::Global => {
                matches!(RuleScope::from_str(&rule.scope), Ok(RuleScope::Global))
                    || crate::models::memory_tags_include_global_scope(projection.tags())
            }
            MemoryScope::SelfOnly | MemoryScope::Team => {
                let sources = projection.source_memory_ids();
                !sources.is_empty()
                    && sources.iter().all(|id| {
                        parents.get(id).is_some_and(|memory| {
                            memory.workspace_id == rule.workspace_id
                                && context.memory_in_scope(memory)
                        })
                    })
            }
        };
        if visible {
            let mut metadata = canonical_metadata(projection);
            if let Some(object) = metadata.as_object_mut() {
                object.insert("memory_scope".to_owned(), context.scope.as_str().into());
            }
            admitted.insert(hit.doc_id.clone(), metadata);
        }
    }
    Ok(admitted)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::core::search::{
        ScoreSource, SearchDedupMode, SearchSourceMode, SpeedMode,
        apply_memory_scope_visibility_with_connection,
    };
    use crate::db::{CreateMemoryInput, CreateProceduralRuleInput, CreateWorkspaceInput};
    use crate::models::MemoryScopeStats;
    use serde_json::json;

    const WORKSPACE: &str = "wsp_00000000000000000000000071";
    const BODY: &str = "Use an atomic generation pointer for every index publication.";

    struct Fixture {
        _root: tempfile::TempDir,
        db: DbConnection,
        options: SearchOptions,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().canonicalize().unwrap();
            std::fs::create_dir(path.join(".ee")).unwrap();
            std::fs::write(
                path.join(".ee/config.toml"),
                "[memory]\ninclude_global = false\n",
            )
            .unwrap();
            let database = path.join(".ee/ee.db");
            let db = DbConnection::open_file(&database).unwrap();
            db.migrate().unwrap();
            db.insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
            Self {
                _root: root,
                db,
                options: SearchOptions {
                    workspace_path: path.clone(),
                    database_path: Some(database),
                    index_dir: Some(path.join("index")),
                    query: "atomic generation pointer".to_owned(),
                    limit: 20,
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
                    strict_source_mode: true,
                    memory_scope: MemoryScope::Workspace,
                    strict_scope: false,
                },
            }
        }

        fn rule(&self, n: usize, trust: &str, scope: &str, sources: &[String]) -> String {
            let id = format!("rule_{n:026}");
            self.db
                .insert_procedural_rule(
                    &id,
                    &CreateProceduralRuleInput {
                        workspace_id: WORKSPACE.to_owned(),
                        content: BODY.to_owned(),
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        trust_class: trust.to_owned(),
                        scope: scope.to_owned(),
                        scope_pattern: None,
                        maturity: "validated".to_owned(),
                        protected: false,
                        source_memory_ids: sources.to_vec(),
                        tags: Vec::new(),
                    },
                )
                .unwrap();
            id
        }

        fn memory(&self, n: usize, agent: &str) -> String {
            let id = format!("mem_{n:026}");
            self.db
                .insert_memory(
                    &id,
                    &CreateMemoryInput {
                        workspace_id: WORKSPACE.to_owned(),
                        level: "episodic".to_owned(),
                        kind: "note".to_owned(),
                        content: "PRIVATE-PARENT-BODY must not be substituted for a rule."
                            .to_owned(),
                        workflow_id: None,
                        confidence: 0.8,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some(format!("agent://{agent}/scope-test")),
                        trust_class: "agent_assertion".to_owned(),
                        trust_subclass: Some(format!("agent:{agent}")),
                        tags: Vec::new(),
                        valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
                        valid_to: None,
                    },
                )
                .unwrap();
            id
        }

        fn scope(
            &self,
            hits: Vec<SearchHit>,
            context: &MemoryScopeContext,
        ) -> (
            Vec<SearchHit>,
            MemoryScopeStats,
            Vec<super::super::SearchDegradation>,
        ) {
            let mut options = self.options.clone();
            options.memory_scope = context.scope;
            options.strict_scope = context.strict_scope;
            let mut degraded = Vec::new();
            let (hits, stats) = apply_memory_scope_visibility_with_connection(
                &options,
                hits,
                &mut degraded,
                context,
                context.stats(),
                matches!(context.scope, MemoryScope::Workspace | MemoryScope::Swarm),
                &self.db,
                None,
                None,
            );
            (hits, stats, degraded)
        }
    }

    fn context(scope: MemoryScope) -> MemoryScopeContext {
        MemoryScopeContext {
            scope,
            strict_scope: false,
            current_agent: Some("Alice".to_owned()),
            team_members: BTreeSet::from(["Bob".to_owned()]),
        }
    }

    fn hit(id: &str) -> SearchHit {
        SearchHit {
            doc_id: id.to_owned(),
            score: 0.75,
            source: ScoreSource::SemanticFast,
            fast_score: Some(0.75),
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: None,
            explanation: None,
        }
    }

    fn ids(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.doc_id.as_str()).collect()
    }

    #[test]
    fn verified_sourceless_rules_reach_the_ordinary_scope_gate_with_native_metadata() {
        let f = Fixture::new();
        let strong = f.rule(1, "human_explicit", "workspace", &[]);
        let weak = f.rule(2, "agent_assertion", "workspace", &[]);
        let (hits, stats, _) = f.scope(
            vec![hit(&weak), hit(&strong)],
            &context(MemoryScope::Verified),
        );
        assert_eq!(ids(&hits), vec![strong.as_str()]);
        assert_eq!(stats.candidates_excluded_by_scope, 1);
        assert_eq!(hits[0].score.to_bits(), 0.75_f32.to_bits());
        let metadata = hits[0].metadata.as_ref().unwrap();
        assert_eq!(metadata["source"], "rule");
        assert_eq!(metadata["content"], BODY);
        assert_eq!(metadata["memory_scope"], "verified");
        assert_eq!(metadata["provenance_uri"], format!("ee://rule/{strong}"));
        assert!(metadata.get("memory_id").is_none());
    }

    #[test]
    fn global_rule_scope_and_live_house_rule_tags_are_both_usable() {
        let f = Fixture::new();
        let global = f.rule(1, "human_explicit", "global", &[]);
        let tagged = f.rule(2, "human_explicit", "workspace", &[]);
        let local = f.rule(3, "human_explicit", "workspace", &[]);
        f.db.execute_raw(&format!(
            "INSERT INTO rule_tags (rule_id, tag) VALUES ('{tagged}', 'house_rule')"
        ))
        .unwrap();
        let candidates = vec![hit(&tagged), hit(&local), hit(&global)];
        let (hits, stats, _) = f.scope(candidates, &context(MemoryScope::Global));
        assert_eq!(ids(&hits), vec![tagged.as_str(), global.as_str()]);
        assert_eq!(stats.candidates_excluded_by_scope, 1);
        f.db.execute_raw(&format!(
            "UPDATE rule_tags SET tag = 'local-only' WHERE rule_id = '{tagged}'"
        ))
        .unwrap();
        let mut stale = hit(&tagged);
        stale.metadata = Some(json!({"tags": "house_rule", "scope": "global"}));
        assert!(
            f.scope(vec![stale], &context(MemoryScope::Global))
                .0
                .is_empty()
        );
    }

    #[test]
    fn self_scope_requires_every_parent_to_be_attributed_to_the_current_agent() {
        let f = Fixture::new();
        let alice = f.memory(1, "Alice");
        let bob = f.memory(2, "Bob");
        let own = f.rule(
            1,
            "agent_validated",
            "workspace",
            std::slice::from_ref(&alice),
        );
        let mixed = f.rule(2, "agent_validated", "workspace", &[alice, bob]);
        let unowned = f.rule(3, "human_explicit", "workspace", &[]);
        let candidates = vec![hit(&own), hit(&mixed), hit(&unowned)];
        let (hits, stats, _) = f.scope(candidates.clone(), &context(MemoryScope::SelfOnly));
        assert_eq!(ids(&hits), vec![own.as_str()]);
        assert_eq!(stats.candidates_excluded_by_scope, 2);
        assert!(
            !serde_json::to_string(&hits[0].metadata)
                .unwrap()
                .contains("PRIVATE-PARENT-BODY")
        );
        let mut unknown = context(MemoryScope::SelfOnly);
        unknown.current_agent = None;
        assert!(f.scope(candidates, &unknown).0.is_empty());
    }

    #[test]
    fn team_scope_accepts_member_lineage_but_never_a_single_authorized_parent_of_a_mixed_rule() {
        let f = Fixture::new();
        let alice = f.memory(1, "Alice");
        let bob = f.memory(2, "Bob");
        let eve = f.memory(3, "Eve");
        let team = f.rule(1, "agent_validated", "workspace", &[alice, bob.clone()]);
        let mixed = f.rule(2, "agent_validated", "workspace", &[bob, eve]);
        let (hits, _, _) = f.scope(vec![hit(&mixed), hit(&team)], &context(MemoryScope::Team));
        assert_eq!(ids(&hits), vec![team.as_str()]);
        let mut revoked = context(MemoryScope::Team);
        revoked.team_members.clear();
        assert!(f.scope(vec![hit(&team)], &revoked).0.is_empty());
    }

    #[test]
    fn strict_scope_counts_native_rules_and_clears_the_whole_mixed_result() {
        let f = Fixture::new();
        let rule = f.rule(1, "human_explicit", "workspace", &[]);
        let weak_memory = f.memory(1, "Alice");
        let mut strict = context(MemoryScope::Verified);
        strict.strict_scope = true;
        let (accepted, stats, _) = f.scope(vec![hit(&rule)], &strict);
        assert_eq!(ids(&accepted), vec![rule.as_str()]);
        assert_eq!(stats.strict_violations, 0);
        let (empty, stats, degraded) = f.scope(vec![hit(&rule), hit(&weak_memory)], &strict);
        assert!(empty.is_empty());
        assert_eq!(stats.strict_violations, 1);
        assert!(!degraded.is_empty());
    }

    #[test]
    fn inspection_maturity_is_preserved_but_retired_rules_never_gain_scope_authority() {
        let f = Fixture::new();
        let rule = f.rule(1, "human_explicit", "workspace", &[]);
        for maturity in ["draft", "candidate", "validated", "deprecated"] {
            f.db.execute_raw(&format!(
                "UPDATE procedural_rules SET maturity = '{maturity}' WHERE id = '{rule}'"
            ))
            .unwrap();
            assert_eq!(
                f.scope(vec![hit(&rule)], &context(MemoryScope::Verified))
                    .0
                    .len(),
                1
            );
        }
        f.db.execute_raw(&format!(
            "UPDATE procedural_rules SET tombstoned_at = '2020-01-01T00:00:00Z' WHERE id = '{rule}'"
        ))
        .unwrap();
        assert!(
            f.scope(vec![hit(&rule)], &context(MemoryScope::Verified))
                .0
                .is_empty()
        );
    }

    #[test]
    fn stale_revisions_and_forged_indexed_trust_do_not_pass_native_scope() {
        let f = Fixture::new();
        let rule = f.rule(1, "human_explicit", "workspace", &[]);
        let (hits, _, _) = f.scope(vec![hit(&rule)], &context(MemoryScope::Verified));
        let mut stale = hit(&rule);
        stale.metadata = hits[0].metadata.clone();
        f.db.execute_raw(&format!(
            "INSERT INTO rule_tags (rule_id, tag) VALUES ('{rule}', 'new-revision')"
        ))
        .unwrap();
        assert!(
            f.scope(vec![stale], &context(MemoryScope::Verified))
                .0
                .is_empty()
        );
        f.db.execute_raw(&format!(
            "UPDATE procedural_rules SET trust_class = 'agent_assertion' WHERE id = '{rule}'"
        ))
        .unwrap();
        let mut forged = hit(&rule);
        forged.metadata = Some(json!({"trust_class": "human_explicit", "content": "forged"}));
        assert!(
            f.scope(vec![forged], &context(MemoryScope::Verified))
                .0
                .is_empty()
        );
    }

    #[test]
    fn source_lifecycle_does_not_replace_the_independent_rule_lifecycle() {
        let f = Fixture::new();
        let memory = f.memory(1, "Alice");
        let rule = f.rule(
            1,
            "agent_validated",
            "workspace",
            std::slice::from_ref(&memory),
        );
        f.db.execute_raw(&format!(
            "UPDATE memories SET tombstoned_at = '2020-02-01T00:00:00Z', valid_to = '2020-02-01T00:00:00Z', superseded_at = '2020-02-01T00:00:00Z' WHERE id = '{memory}'"
        )).unwrap();
        f.db.insert_memory_seal(
            &memory,
            &crate::models::memory_seal_commitment(b"private parent"),
            "2020-02-01T00:00:00Z",
        )
        .unwrap();
        let (hits, _, _) = f.scope(vec![hit(&rule)], &context(MemoryScope::SelfOnly));
        assert_eq!(ids(&hits), vec![rule.as_str()]);
        assert_eq!(hits[0].metadata.as_ref().unwrap()["content"], BODY);
    }

    #[test]
    fn pinned_snapshot_keeps_scope_and_body_coherent_and_remains_caller_owned() {
        let f = Fixture::new();
        let rule = f.rule(1, "human_explicit", "workspace", &[]);
        f.db.begin_read_snapshot().unwrap();
        f.db.get_procedural_rule(&rule).unwrap().unwrap();
        let writer = DbConnection::open_file(&f.options.resolve_database_path()).unwrap();
        writer
            .execute_raw(&format!(
                "UPDATE procedural_rules SET trust_class = 'agent_assertion' WHERE id = '{rule}'"
            ))
            .unwrap();
        let (hits, _, _) = f.scope(vec![hit(&rule)], &context(MemoryScope::Verified));
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].metadata.as_ref().unwrap()["trust_class"],
            "human_explicit"
        );
        f.db.rollback_read_snapshot()
            .expect("scope must not release the caller snapshot");
        assert!(
            f.scope(vec![hit(&rule)], &context(MemoryScope::Verified))
                .0
                .is_empty()
        );
    }

    #[test]
    fn unavailable_rule_relations_do_not_discard_unrelated_verified_memories() {
        let f = Fixture::new();
        let rule = f.rule(1, "human_explicit", "workspace", &[]);
        let memory = f.memory(1, "Alice");
        f.db.execute_raw(&format!(
            "UPDATE memories SET trust_class = 'human_explicit' WHERE id = '{memory}'"
        ))
        .unwrap();
        f.db.execute_raw("ALTER TABLE rule_tags RENAME TO unavailable_scope_rule_tags")
            .unwrap();
        let (hits, stats, _) = f.scope(
            vec![hit(&rule), hit(&memory)],
            &context(MemoryScope::Verified),
        );
        assert_eq!(ids(&hits), vec![memory.as_str()]);
        assert_eq!(stats.candidates_excluded_by_scope, 1);
    }

    #[test]
    fn multi_page_lineage_requires_all_513_parents_without_mutating_the_store() {
        let f = Fixture::new();
        let mut sources = Vec::new();
        f.db.with_transaction(|| {
            for n in 1..=513 {
                sources.push(f.memory(n, "Alice"));
            }
            Ok(())
        })
        .unwrap();
        let rule = f.rule(1, "agent_validated", "workspace", &sources);
        let audits = f.db.count_table_rows("audit_log").unwrap();
        assert_eq!(
            f.scope(vec![hit(&rule)], &context(MemoryScope::SelfOnly))
                .0
                .len(),
            1
        );
        assert_eq!(f.db.count_table_rows("audit_log").unwrap(), audits);
        f.db.execute_raw(&format!(
            "UPDATE memories SET trust_subclass = 'agent:Eve' WHERE id = '{}'",
            sources[512]
        ))
        .unwrap();
        assert!(
            f.scope(vec![hit(&rule)], &context(MemoryScope::SelfOnly))
                .0
                .is_empty()
        );
        assert!(!f.options.resolve_index_dir().exists());
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn public_lexical_search_returns_native_rules_in_verified_and_global_scopes() {
        let mut f = Fixture::new();
        let local = f.rule(1, "human_explicit", "workspace", &[]);
        let global = f.rule(2, "human_explicit", "global", &[]);
        crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
            workspace_path: f.options.workspace_path.clone(),
            database_path: f.options.database_path.clone(),
            index_dir: f.options.index_dir.clone(),
            dry_run: false,
        })
        .unwrap();
        f.options.memory_scope = MemoryScope::Verified;
        f.options.strict_scope = true;
        let verified = crate::core::search::run_search_unaudited(&f.options).unwrap();
        let actual: BTreeSet<_> = ids(&verified.results).into_iter().collect();
        assert_eq!(actual, BTreeSet::from([local.as_str(), global.as_str()]));
        assert_eq!(verified.scope_stats.strict_violations, 0);
        f.options.memory_scope = MemoryScope::Global;
        f.options.strict_scope = false;
        let global_report = crate::core::search::run_search_unaudited(&f.options).unwrap();
        assert_eq!(ids(&global_report.results), vec![global.as_str()]);
    }
}
