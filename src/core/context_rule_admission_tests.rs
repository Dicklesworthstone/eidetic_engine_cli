//! Real-store coverage for rule admission while packs still hydrate source memories.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::profile::OperatingProfile;
use crate::db::{CreateMemoryInput, CreateProceduralRuleInput, CreateWorkspaceInput};
use crate::models::{
    QueryFilters, QueryTemporalFilters, QueryTemporalValidity, QueryTemporalValidityPosture,
    RedactionFilters, TagFilters, TrustFilters,
};

const OLD: &str = "2020-01-01T00:00:00Z";
const NEW: &str = "2025-01-01T00:00:00Z";
const BOUND: &str = "2023-01-01T00:00:00Z";

struct Fixture {
    _directory: tempfile::TempDir,
    workspace: PathBuf,
    workspace_id: String,
    db: DbConnection,
}

struct RulePair {
    memory: String,
    rule: String,
}

struct Resolution {
    candidates: Vec<PackCandidate>,
    metrics: CandidateResolutionMetrics,
    degraded: Vec<ContextResponseDegradation>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        Self {
            _directory: directory,
            workspace,
            workspace_id,
            db,
        }
    }

    fn add(
        &self,
        seed: u128,
        customize_memory: impl FnOnce(&mut CreateMemoryInput),
        customize_rule: impl FnOnce(&mut CreateProceduralRuleInput),
    ) -> RulePair {
        let memory = MemoryId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        let rule = RuleId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        let mut memory_input = CreateMemoryInput {
            workspace_id: self.workspace_id.clone(),
            content: format!("Historical release incident {seed}."),
            level: "semantic".to_owned(),
            kind: "note".to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.5,
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            provenance_uri: Some(format!("manual://rule-admission-{seed}")),
            tags: Vec::new(),
            // insert_memory otherwise defaults this to the current clock,
            // outside the historical validity windows exercised below.
            valid_from: Some(OLD.to_owned()),
            valid_to: None,
        };
        customize_memory(&mut memory_input);
        self.db.insert_memory(&memory, &memory_input).unwrap();
        let mut rule_input = CreateProceduralRuleInput {
            workspace_id: self.workspace_id.clone(),
            content: format!("Run formatting before release {seed}."),
            confidence: 0.8,
            utility: 0.7,
            importance: 0.5,
            trust_class: "agent_assertion".to_owned(),
            scope: "workspace".to_owned(),
            scope_pattern: None,
            maturity: "candidate".to_owned(),
            protected: false,
            source_memory_ids: vec![memory.clone()],
            tags: Vec::new(),
        };
        customize_rule(&mut rule_input);
        self.db.insert_procedural_rule(&rule, &rule_input).unwrap();
        self.times("memories", &memory, OLD, OLD);
        self.times("procedural_rules", &rule, OLD, OLD);
        RulePair { memory, rule }
    }

    fn times(&self, table: &str, id: &str, created_at: &str, updated_at: &str) {
        self.db
            .execute_raw(&format!(
                "UPDATE {table} SET created_at = '{created_at}', updated_at = '{updated_at}' WHERE id = '{id}'"
            ))
            .unwrap();
    }

    fn resolve(&self, ids: &[&str], filters: &QueryFilters) -> Resolution {
        let mut degraded = Vec::new();
        let (candidates, metrics) = candidates_from_search_with_metrics(
            &self.db,
            &self.workspace,
            &report(ids),
            filters,
            false,
            &mut degraded,
            None,
        );
        Resolution {
            candidates,
            metrics,
            degraded,
        }
    }

    fn projection(&self, pair: &RulePair) -> RuleIndexProjection {
        RuleIndexProjection::new(
            self.db.get_procedural_rule(&pair.rule).unwrap().unwrap(),
            &self.workspace,
            self.db.get_rule_tags(&pair.rule).unwrap(),
            self.db.get_rule_source_memory_ids(&pair.rule).unwrap(),
        )
    }
}

fn report(ids: &[&str]) -> SearchReport {
    SearchReport {
        index_freshness: None,
        status: SearchStatus::Success,
        embed_backend: EmbedBackend::HashFallback,
        query: "prepare release".to_owned(),
        requested_limit: ids.len().try_into().unwrap(),
        results: ids
            .iter()
            .map(|id| SearchHit {
                doc_id: (*id).to_owned(),
                score: 0.91,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.91),
                rerank_score: None,
                metadata: None,
                explanation: None,
            })
            .collect(),
        elapsed_ms: 0.0,
        errors: Vec::new(),
        degraded: Vec::new(),
        runtime_profile: RuntimeProfileReport::for_profile(
            OperatingProfile::Workstation,
            "rule_admission_test",
        ),
        rerank_configured_mode: crate::config::SearchRerankMode::Auto,
        rerank_configured_top_k: 50,
        rerank_runtime_available: false,
        relevance_floor_applied: None,
        candidates_below_floor: 0,
        query_assist: None,
        source_mode_requested: SearchSourceMode::LexicalOnly,
        source_mode_applied: SearchSourceMode::LexicalOnly,
        source_mode_fallback: false,
        strict_source_mode: false,
        memory_scope: MemoryScope::Workspace,
        strict_scope: false,
        scope_stats: MemoryScopeStats::new(MemoryScope::Workspace, false, None, 0),
    }
}

fn sources(candidates: &[PackCandidate]) -> BTreeSet<String> {
    candidates
        .iter()
        .map(|candidate| candidate.memory_id.to_string())
        .collect()
}

fn timestamp(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn minimum_trust_uses_the_live_rule_in_both_parent_trust_directions() {
    let f = Fixture::new();
    let weak_rule = f.add(
        1,
        |memory| memory.trust_class = "human_explicit".to_owned(),
        |_| {},
    );
    let strong_rule = f.add(
        2,
        |_| {},
        |rule| rule.trust_class = "human_explicit".to_owned(),
    );
    let ids = [&*weak_rule.rule, &*strong_rule.rule];
    let mut filters = QueryFilters {
        trust: TrustFilters {
            min_class: Some("agent_validated".to_owned()),
            ..TrustFilters::default()
        },
        ..QueryFilters::default()
    };
    let resolved = f.resolve(&ids, &filters);
    assert_eq!(
        sources(&resolved.candidates),
        BTreeSet::from([strong_rule.memory.clone()])
    );
    assert_eq!(resolved.metrics.trust_filtered_candidates, 1);
    assert_eq!(
        resolved.candidates[0].trust.class,
        TrustClass::HumanExplicit
    );
    assert_eq!(
        resolved.candidates[0].content,
        f.projection(&strong_rule).rule().content
    );

    filters.trust.require_posture = Some("advisory".to_owned());
    assert_eq!(
        sources(&f.resolve(&ids, &filters).candidates),
        BTreeSet::from([strong_rule.memory])
    );
    filters
        .trust
        .exclude_classes
        .push("human_explicit".to_owned());
    assert!(f.resolve(&ids, &filters).candidates.is_empty());
}

#[test]
fn unsigned_candidate_and_validated_rules_keep_declared_class_but_remain_advisory() {
    let f = Fixture::new();
    let mut pairs = Vec::new();
    let mut seed = 1;
    for maturity in ["candidate", "validated"] {
        for class in ["human_explicit", "peer_human_attested", "agent_validated"] {
            let pair = f.add(
                seed,
                |memory| memory.trust_class = "human_explicit".to_owned(),
                |rule| {
                    rule.trust_class = class.to_owned();
                    rule.maturity = maturity.to_owned();
                    rule.protected = maturity == "validated";
                },
            );
            pairs.push((pair, class));
            seed += 1;
        }
    }
    let ids: Vec<&str> = pairs.iter().map(|(pair, _)| pair.rule.as_str()).collect();
    let mut filters = QueryFilters {
        trust: TrustFilters {
            require_posture: Some("advisory".to_owned()),
            ..TrustFilters::default()
        },
        ..QueryFilters::default()
    };
    let resolved = f.resolve(&ids, &filters);
    assert_eq!(
        resolved.candidates.len(),
        pairs.len(),
        "{:?}",
        resolved.degraded
    );
    for candidate in &resolved.candidates {
        let (_, class) = pairs
            .iter()
            .find(|(pair, _)| pair.memory == candidate.memory_id.to_string())
            .unwrap();
        assert_eq!(candidate.trust.class.as_str(), *class);
        assert_eq!(candidate.trust.subclass.as_deref(), Some("procedural_rule"));
        assert_eq!(candidate.trust.posture(), PackTrustPosture::Advisory);
        assert_eq!(candidate.section, PackSection::ProceduralRules);
    }

    filters.trust.require_posture = Some("authoritative".to_owned());
    assert!(f.resolve(&ids, &filters).candidates.is_empty());
    let direct_memory = f.resolve(&[&pairs[0].0.memory], &filters);
    assert_eq!(direct_memory.candidates.len(), 1);
    assert_eq!(
        direct_memory.candidates[0].trust.posture(),
        PackTrustPosture::Authoritative
    );
}

#[test]
fn verified_scope_uses_rule_class_without_inheriting_parent_authority() {
    let f = Fixture::new();
    let weak_rule = f.add(
        1,
        |memory| memory.trust_class = "human_explicit".to_owned(),
        |_| {},
    );
    let human_rule = f.add(
        2,
        |_| {},
        |rule| rule.trust_class = "human_explicit".to_owned(),
    );
    let validated_rule = f.add(
        3,
        |_| {},
        |rule| {
            rule.trust_class = "agent_validated".to_owned();
            rule.maturity = "validated".to_owned();
        },
    );
    let mut resolved = f.resolve(
        &[&weak_rule.rule, &human_rule.rule, &validated_rule.rule],
        &QueryFilters::default(),
    );
    assert_eq!(resolved.candidates.len(), 3);
    let stats = filter_candidates_by_memory_scope(
        &f.db,
        &mut resolved.candidates,
        &MemoryScopeContext {
            scope: MemoryScope::Verified,
            strict_scope: false,
            current_agent: None,
            team_members: BTreeSet::new(),
        },
        &mut resolved.degraded,
        None,
        &BTreeSet::new(),
    );
    assert_eq!(
        sources(&resolved.candidates),
        BTreeSet::from([human_rule.memory, validated_rule.memory])
    );
    assert_eq!(stats.candidates_in_scope, 2);
    assert_eq!(stats.candidates_excluded_by_scope, 1);
    assert!(
        resolved
            .candidates
            .iter()
            .all(|candidate| candidate.trust.posture() == PackTrustPosture::Advisory)
    );
}

#[test]
fn rule_tags_control_required_and_excluded_tags_in_both_directions() {
    let f = Fixture::new();
    let parent_only = f.add(
        1,
        |memory| memory.tags = vec!["release".to_owned()],
        |rule| rule.tags = vec!["internal".to_owned()],
    );
    let rule_only = f.add(
        2,
        |memory| memory.tags = vec!["internal".to_owned()],
        |rule| rule.tags = vec!["release".to_owned()],
    );
    let ids = [&*parent_only.rule, &*rule_only.rule];
    for tags in [
        TagFilters {
            require: vec!["release".to_owned()],
            ..TagFilters::default()
        },
        TagFilters {
            require_any: vec!["release".to_owned()],
            ..TagFilters::default()
        },
        TagFilters {
            require: vec!["release".to_owned()],
            exclude: vec!["internal".to_owned()],
            ..TagFilters::default()
        },
    ] {
        let resolved = f.resolve(
            &ids,
            &QueryFilters {
                tags,
                ..QueryFilters::default()
            },
        );
        assert_eq!(
            sources(&resolved.candidates),
            BTreeSet::from([rule_only.memory.clone()])
        );
        assert_eq!(resolved.metrics.tag_filtered_candidates, 1);
    }
    let resolved = f.resolve(
        &ids,
        &QueryFilters {
            tags: TagFilters {
                exclude: vec!["release".to_owned()],
                ..TagFilters::default()
            },
            ..QueryFilters::default()
        },
    );
    assert_eq!(
        sources(&resolved.candidates),
        BTreeSet::from([parent_only.memory])
    );
}

#[test]
fn rule_creation_and_update_times_control_temporal_record_filters() {
    let f = Fixture::new();
    let old_rule = f.add(1, |_| {}, |_| {});
    let new_rule = f.add(2, |_| {}, |_| {});
    f.times("memories", &old_rule.memory, NEW, NEW);
    f.times("procedural_rules", &new_rule.rule, NEW, NEW);
    let ids = [&*old_rule.rule, &*new_rule.rule];
    for (temporal, expected) in [
        (
            QueryTemporalFilters {
                after: Some(timestamp(BOUND)),
                ..QueryTemporalFilters::default()
            },
            &new_rule.memory,
        ),
        (
            QueryTemporalFilters {
                before: Some(timestamp(BOUND)),
                ..QueryTemporalFilters::default()
            },
            &old_rule.memory,
        ),
    ] {
        let resolved = f.resolve(
            &ids,
            &QueryFilters {
                temporal,
                ..QueryFilters::default()
            },
        );
        assert_eq!(
            sources(&resolved.candidates),
            BTreeSet::from([expected.clone()])
        );
        assert_eq!(resolved.metrics.temporal_filtered_candidates, 1);
    }

    // Both rules existed before the bound; only the second rule's own update
    // crosses it. Parent bookkeeping must neither admit nor hide either body.
    f.times("procedural_rules", &new_rule.rule, OLD, NEW);
    let resolved = f.resolve(
        &ids,
        &QueryFilters {
            temporal: QueryTemporalFilters {
                as_of: Some(timestamp(BOUND)),
                ..QueryTemporalFilters::default()
            },
            ..QueryFilters::default()
        },
    );
    assert_eq!(
        sources(&resolved.candidates),
        BTreeSet::from([old_rule.memory])
    );
    assert_eq!(resolved.metrics.temporal_filtered_candidates, 1);
}

#[test]
fn redaction_categories_describe_the_selected_rule_body() {
    let f = Fixture::new();
    let secret_body = concat!("Use api", "_key=sk_test_123 only locally.");
    assert!(
        crate::policy::redact_secret_like_content(secret_body)
            .redacted_reasons
            .contains(&"api_key")
    );
    let private_parent = f.add(1, |memory| memory.content = secret_body.to_owned(), |_| {});
    let private_rule = f.add(2, |_| {}, |rule| rule.content = secret_body.to_owned());
    let ids = [&*private_parent.rule, &*private_rule.rule];
    let mut filters = QueryFilters {
        redaction: RedactionFilters {
            allow_categories: vec!["email_address".to_owned()],
            ..RedactionFilters::default()
        },
        ..QueryFilters::default()
    };
    let resolved = f.resolve(&ids, &filters);
    assert_eq!(
        sources(&resolved.candidates),
        BTreeSet::from([private_parent.memory])
    );
    assert_eq!(resolved.metrics.redaction_filtered_candidates, 1);
    assert!(!resolved.candidates[0].content.contains(secret_body));
    filters.redaction.allow_categories = vec!["api_key".to_owned()];
    assert_eq!(f.resolve(&ids, &filters).candidates.len(), 2);
}

#[test]
fn path_scoped_and_retired_rules_cannot_hydrate_an_unscoped_pack() {
    let f = Fixture::new();
    std::fs::create_dir(f.workspace.join("src")).unwrap();
    let workspace_rule = f.add(1, |_| {}, |_| {});
    let mut denied = Vec::new();
    for (seed, scope, pattern) in [(2, "directory", "src"), (3, "file_pattern", "src/**/*.rs")] {
        let pair = f.add(
            seed,
            |_| {},
            |rule| {
                rule.scope = scope.to_owned();
                rule.scope_pattern = Some(pattern.to_owned());
            },
        );
        assert!(
            f.projection(&pair).is_pack_admissible(),
            "valid path scope fixture"
        );
        denied.push(pair);
    }
    denied.push(f.add(
        4,
        |_| {},
        |rule| {
            rule.scope = "directory".to_owned();
            rule.scope_pattern = Some("../outside".to_owned());
        },
    ));
    for (seed, maturity) in [(5, "draft"), (6, "deprecated"), (7, "superseded")] {
        denied.push(f.add(seed, |_| {}, |rule| rule.maturity = maturity.to_owned()));
    }
    let tombstoned = f.add(8, |_| {}, |_| {});
    f.db.execute_raw(&format!(
        "UPDATE procedural_rules SET tombstoned_at = '{NEW}' WHERE id = '{}'",
        tombstoned.rule
    ))
    .unwrap();
    denied.push(tombstoned);
    let replaced = f.add(9, |_| {}, |_| {});
    f.db.execute_raw(&format!(
        "UPDATE procedural_rules SET superseded_by = '{}' WHERE id = '{}'",
        workspace_rule.rule, replaced.rule
    ))
    .unwrap();
    denied.push(replaced);
    let ids: Vec<&str> = std::iter::once(workspace_rule.rule.as_str())
        .chain(denied.iter().map(|pair| pair.rule.as_str()))
        .collect();
    let resolved = f.resolve(&ids, &QueryFilters::default());
    assert_eq!(
        sources(&resolved.candidates),
        BTreeSet::from([workspace_rule.memory])
    );
    assert_eq!(resolved.metrics.skipped_candidates, denied.len());
    assert!(
        resolved
            .degraded
            .iter()
            .any(|entry| entry.code == "context_rule_hit_unhydrated")
    );
}

#[test]
fn source_lifecycle_and_seal_requirements_still_guard_rule_hydration() {
    let f = Fixture::new();
    let live = f.add(1, |_| {}, |_| {});
    let retired = f.add(2, |_| {}, |_| {});
    f.db.execute_raw(&format!(
        "UPDATE memories SET tombstoned_at = '{NEW}' WHERE id = '{}'",
        retired.memory
    ))
    .unwrap();
    let sealed = f.add(
        3,
        |memory| memory.content = crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT.to_owned(),
        |_| {},
    );
    f.db.insert_memory_seal(
        &sealed.memory,
        &crate::models::memory_seal_commitment(b"private source incident"),
        OLD,
    )
    .unwrap();
    let sourceless = f.add(4, |_| {}, |rule| rule.source_memory_ids.clear());
    let ids = [
        &*live.rule,
        &*retired.rule,
        &*sealed.rule,
        &*sourceless.rule,
    ];
    let resolved = f.resolve(&ids, &QueryFilters::default());
    assert_eq!(
        sources(&resolved.candidates),
        BTreeSet::from([live.memory.clone()])
    );
    assert!(
        resolved
            .degraded
            .iter()
            .any(|entry| entry.code == "context_candidate_sealed")
    );

    let expired = f.add(
        5,
        |memory| {
            memory.valid_from = Some("2019-01-01T00:00:00Z".to_owned());
            memory.valid_to = Some(OLD.to_owned());
        },
        |_| {},
    );
    let future = f.add(6, |memory| memory.valid_from = Some(NEW.to_owned()), |_| {});
    let resolved = f.resolve(
        &[&live.rule, &expired.rule, &future.rule],
        &QueryFilters {
            temporal: QueryTemporalFilters {
                validity: Some(QueryTemporalValidity {
                    posture: QueryTemporalValidityPosture::Strict,
                    reference_time: Some(timestamp(BOUND)),
                }),
                ..QueryTemporalFilters::default()
            },
            ..QueryFilters::default()
        },
    );
    assert_eq!(sources(&resolved.candidates), BTreeSet::from([live.memory]));
    assert_eq!(resolved.metrics.temporal_filtered_candidates, 2);
}

#[test]
fn candidate_construction_rejects_malformed_rule_trust_and_missing_source() {
    let f = Fixture::new();
    let pair = f.add(
        1,
        |_| {},
        |rule| rule.trust_class = "human_explicit".to_owned(),
    );
    // The storage CHECK is the first guard. Feed the second boundary a corrupt
    // projection explicitly because production storage rejects this mutation.
    assert!(
        f.db.execute_raw(&format!(
            "UPDATE procedural_rules SET trust_class = 'corrupt_native_trust' WHERE id = '{}'",
            pair.rule
        ))
        .is_err()
    );
    let live_projection = f.projection(&pair);
    let mut corrupt_rule = live_projection.rule().clone();
    corrupt_rule.trust_class = "corrupt_native_trust".to_owned();
    let corrupt_projection = RuleIndexProjection::new(
        corrupt_rule,
        &f.workspace,
        live_projection.tags().to_vec(),
        live_projection.source_memory_ids().to_vec(),
    );
    let source_memory = f.db.get_memory(&pair.memory).unwrap().unwrap();
    let tags = f.db.get_memory_tags_batch(&[&pair.memory]).unwrap();
    let search = report(&[&pair.rule]);
    for (projection, memory_present, admitted) in [
        (live_projection.clone(), true, true),
        (corrupt_projection, true, false),
        (live_projection, false, false),
    ] {
        let memories = CandidateMemoryBatch::Owned(if memory_present {
            BTreeMap::from([(pair.memory.clone(), source_memory.clone())])
        } else {
            BTreeMap::new()
        });
        let rules = BTreeMap::from([(pair.rule.clone(), projection)]);
        let mut cache = crate::core::memory::EvidenceFreshnessFileCache::default();
        let candidate = candidate_from_hit_preloaded(
            PreloadedCandidateSource {
                memories: &memories,
                tags_map: &tags,
                workspace_path: &f.workspace,
                bound_workspace_id: Some(&f.workspace_id),
                query: &search.query,
                validity_reference_time: Some(timestamp(BOUND)),
                include_tombstoned: false,
                freshness_file_cache: &mut cache,
                rules: &rules,
            },
            &search.results[0],
            &pair.memory,
            MemoryId::from_str(&pair.memory).unwrap(),
            Some(pair.rule.clone()),
            &mut Vec::new(),
            &mut CandidateResolutionSubspans::default(),
        );
        assert_eq!(candidate.is_some(), admitted);
        if let Some(candidate) = candidate {
            assert_eq!(candidate.trust.class, TrustClass::HumanExplicit);
            assert_eq!(candidate.trust.posture(), PackTrustPosture::Advisory);
        }
    }
}
