//! Source-of-truth corpus admission for extractive question answering.
//!
//! Current lifecycle, scope and public-evidence eligibility are resolved before
//! scoring, nearest-evidence hints, and incident-link lookup. Memory bodies,
//! scope metadata and links must describe one coherent database snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Utc};

use crate::core::memory_scope::MemoryScopeContext;
use crate::db::DbConnection;
use crate::models::{DomainError, MemoryScope, RuleScope, TrustClass};

use super::{AskCandidate, AskContradiction, AskNativeSource, load_scoped_contradictions};

#[path = "ask_admission.rs"]
mod admission;

#[path = "ask_memory_admission.rs"]
mod memory_admission;

#[path = "ask_quarantine.rs"]
mod quarantine;

#[cfg(test)]
#[path = "ask_quarantine_tests.rs"]
mod quarantine_tests;

use memory_admission::load_memory_revisions;

#[derive(Clone, Debug)]
pub struct AskCorpus {
    pub candidates: Vec<AskCandidate>,
    pub contradictions: Vec<AskContradiction>,
    pub native_sources: BTreeMap<String, AskNativeSource>,
}

/// Load live, public command advice without searching transcripts or indexes.
///
/// Preflight uses the same source authority as answers: current revisions,
/// author validity, seals, public-body screening, and workspace-owned native
/// rule lineage. It must not acquire evidence from an index or turn imported
/// transcript prose into an instruction. File/directory rules require explicit
/// task targets and are therefore withheld on this command-only surface.
pub(crate) fn load_command_advice_corpus(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
) -> Result<AskCorpus, DomainError> {
    let snapshot = AskReadSnapshot::begin(connection)?;
    let stored =
        memory_admission::load_command_advice_revisions(connection, workspace_id, reference_time)?;
    let mut candidates = stored
        .into_iter()
        .filter(|memory| {
            matches!(
                memory.kind.as_str(),
                "risk" | "anti-pattern" | "failure" | "rule"
            )
        })
        .filter_map(admission::into_candidate)
        .collect();
    let scope = MemoryScopeContext {
        scope: MemoryScope::Workspace,
        strict_scope: false,
        current_agent: None,
        team_members: BTreeSet::new(),
    };
    let native_sources = load_rules(connection, workspace_id, &scope, &[], &mut candidates)?;
    snapshot.finish()?;
    Ok(AskCorpus {
        candidates,
        contradictions: Vec::new(),
        native_sources,
    })
}

/// Load current evidence for one already-resolved workspace.
///
/// `reference_time` is captured once by the caller, not separately per row.
/// Author validity bounds are inclusive; supersession closes a revision at its
/// exclusive cutoff. A closed seal withholds the body regardless of the clock
/// or placeholder spelling. Invalid timestamps or inverted windows fail the
/// entire read without exposing the offending body, identifier, or timestamp.
///
/// Memory bodies, validity metadata, and every batch of incident links come
/// from one database read snapshot. The snapshot is released before returning
/// owned data for scoring or best-effort audit writes. No writer fence,
/// migrations, index/model loading, or cross-workspace expansion occur here.
pub fn load_current_ask_corpus(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
) -> Result<AskCorpus, DomainError> {
    load_corpus_with_boundary(connection, workspace_id, reference_time, || Ok(()))
}

/// Apply the ordinary memory scope before scoring or contradiction lookup.
/// Global scope selects tagged memories in this workspace; it never opens a
/// global store or widens the already-resolved workspace boundary.
pub fn load_scoped_ask_corpus(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    scope: MemoryScope,
) -> Result<AskCorpus, DomainError> {
    load_ask_corpus_for_paths(connection, workspace_id, reference_time, scope, &[])
}

/// Add literal workspace-relative task targets without widening memory scope.
/// Paths select directory/file rules; ordinary workspace evidence is retained.
/// No target contents are opened and targets may describe not-yet-created files.
pub fn load_ask_corpus_for_paths(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    scope: MemoryScope,
    paths: &[String],
) -> Result<AskCorpus, DomainError> {
    load_corpus_with_path_boundary(
        connection,
        workspace_id,
        reference_time,
        paths,
        || scope_context(connection, workspace_id, scope),
        || Ok(()),
    )
}

fn scope_context(
    connection: &DbConnection,
    workspace_id: &str,
    scope: MemoryScope,
) -> Result<MemoryScopeContext, DomainError> {
    let mut context = MemoryScopeContext {
        scope,
        strict_scope: false,
        current_agent: crate::core::memory_scope::current_agent_name(),
        team_members: BTreeSet::new(),
    };
    if scope == MemoryScope::Team {
        admission::require_workspace_roster(connection, workspace_id)?;
        // The addressed store's authenticated roster is authority, not a
        // config-file list or a roster from a different workspace/database.
        for member in connection
            .list_all_team_members()
            .map_err(|_| corpus_storage_error())?
        {
            if member.workspace_id == workspace_id && member.state == "active" {
                for name in [member.display_name, member.origin_node_id] {
                    let name = name.trim();
                    if !name.is_empty() {
                        context.team_members.insert(name.to_owned());
                    }
                }
            }
        }
    }
    Ok(context)
}

// The private boundary lets real-store tests commit through a second connection
// at the exact memory/link boundary. Production passes a no-op, not a timing
// sleep or a mock database. The owned snapshot encloses both reads regardless.
fn load_corpus_with_boundary(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    after_memory_read: impl FnOnce() -> Result<(), DomainError>,
) -> Result<AskCorpus, DomainError> {
    load_corpus_with_scope_boundary(
        connection,
        workspace_id,
        reference_time,
        || {
            Ok(MemoryScopeContext {
                scope: MemoryScope::Workspace,
                strict_scope: false,
                current_agent: None,
                team_members: BTreeSet::new(),
            })
        },
        after_memory_read,
    )
}

fn load_corpus_with_scope_boundary(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    scope_context: impl FnOnce() -> Result<MemoryScopeContext, DomainError>,
    after_memory_read: impl FnOnce() -> Result<(), DomainError>,
) -> Result<AskCorpus, DomainError> {
    load_corpus_with_path_boundary(
        connection,
        workspace_id,
        reference_time,
        &[],
        scope_context,
        after_memory_read,
    )
}

fn load_corpus_with_path_boundary(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    paths: &[String],
    scope_context: impl FnOnce() -> Result<MemoryScopeContext, DomainError>,
    after_memory_read: impl FnOnce() -> Result<(), DomainError>,
) -> Result<AskCorpus, DomainError> {
    let snapshot = AskReadSnapshot::begin(connection)?;
    let paths = normalize_ask_targets(connection, workspace_id, paths)?;
    let stored = load_memory_revisions(connection, workspace_id, reference_time)?;
    let scope = scope_context()?;
    after_memory_read()?;
    let mut tags = std::collections::BTreeMap::new();
    if scope.scope == MemoryScope::Global {
        let ids: Vec<_> = stored.iter().map(|memory| memory.id.as_str()).collect();
        for batch in ids.chunks(256) {
            tags.extend(
                connection
                    .get_memory_tags_batch(batch)
                    .map_err(|_| corpus_storage_error())?,
            );
        }
    }
    let mut candidates = Vec::with_capacity(stored.len());
    for memory in stored {
        if validity_contains(
            memory.valid_from.as_deref(),
            memory.valid_to.as_deref(),
            reference_time,
        )? && memory.workspace_id == workspace_id
            && scope.memory_in_scope_with_tags(
                &memory,
                tags.get(&memory.id).map(Vec::as_slice).unwrap_or(&[]),
            )
            && let Some(candidate) = admission::into_candidate(memory)
        {
            candidates.push(candidate);
        }
    }
    let ids: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.memory_id.as_str())
        .collect();
    let contradictions = load_scoped_contradictions(connection, &ids)?;
    let mut native_sources = load_rules(connection, workspace_id, &scope, &paths, &mut candidates)?;
    admission::append_evidence(
        connection,
        workspace_id,
        scope.scope,
        &mut candidates,
        &mut native_sources,
    )?;
    snapshot.finish()?;
    Ok(AskCorpus {
        candidates,
        contradictions,
        native_sources,
    })
}

const ASK_MEMORY_REVISION_PAGE_SIZE: usize = 256;

/// Current source authority is separate from validity, trust, and relevance.
/// This deliberately uses the repository's seal classifier, not a second
/// interpretation of reveal flags. Historical reference times never unseal a
/// body. All timestamps are validated before any candidates can be returned.
fn withheld_memory_ids(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
) -> Result<BTreeSet<String>, DomainError> {
    let revisions = connection
        .list_memory_supersession_markers(workspace_id)
        .map_err(|_| corpus_storage_error())?;
    let mut withheld: BTreeSet<_> =
        crate::core::memory_lifecycle::load_memory_seals_for_admission(connection, workspace_id)
            .map_err(|_| corpus_storage_error())?
            .into_iter()
            .filter(|seal| seal.is_sealed())
            .map(|seal| seal.memory_id)
            .collect();
    for (id, raw) in revisions {
        let cutoff = DateTime::parse_from_rfc3339(&raw)
            .map_err(|_| DomainError::Storage {
                message: "Ask evidence contains invalid revision metadata; answer withheld"
                    .to_owned(),
                repair: Some("ee doctor --json".to_owned()),
            })?
            .with_timezone(&Utc);
        if reference_time >= cutoff {
            withheld.insert(id);
        }
    }
    Ok(withheld)
}

// Rule lineage is authority about authorship and ownership, not admission of
// parent bodies. A superseded incident must not hide a separately active rule;
// a missing or foreign parent must not masquerade as a source-less rule either.
const ASK_RULE_LINEAGE_PAGE_SIZE: usize = 256;

#[derive(Default)]
struct AskRuleLineage {
    owned: BTreeSet<String>,
    attributed: BTreeSet<String>,
}

fn load_rule_lineage(
    connection: &DbConnection,
    workspace_id: &str,
    scope: &MemoryScopeContext,
    source_ids: &BTreeSet<&str>,
) -> Result<AskRuleLineage, DomainError> {
    use sqlmodel_core::Value;

    let ids: Vec<_> = source_ids
        .iter()
        .copied()
        .filter(|id| {
            crate::models::MemoryId::from_str(id).is_ok_and(|parsed| parsed.to_string() == *id)
        })
        .collect();
    let mut lineage = AskRuleLineage::default();
    for page in ids.chunks(ASK_RULE_LINEAGE_PAGE_SIZE) {
        if matches!(scope.scope, MemoryScope::SelfOnly | MemoryScope::Team) {
            // Reuse the established producer parser on actual source rows.
            // ID lookup intentionally includes retired versions: no source
            // body is made answerable, and no parent lifecycle is inherited.
            let memories = connection
                .get_memories_batch(page)
                .map_err(|_| corpus_storage_error())?;
            for id in page {
                let Some(memory) = memories.get(*id) else {
                    continue;
                };
                if memory.id != *id || memory.workspace_id != workspace_id {
                    continue;
                }
                lineage.owned.insert(memory.id.clone());
                if scope.memory_in_scope(memory) {
                    lineage.attributed.insert(memory.id.clone());
                }
            }
        } else {
            // Workspace/global/verified need identity and ownership only.
            // Avoid loading private parent bodies simply to prove provenance.
            let placeholders = (1..=page.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT id, workspace_id FROM memories WHERE id IN ({placeholders}) ORDER BY id ASC"
            );
            let parameters: Vec<_> = page
                .iter()
                .map(|id| Value::Text((*id).to_owned()))
                .collect();
            for row in connection
                .query(&sql, &parameters)
                .map_err(|_| corpus_storage_error())?
            {
                let (Some(Value::Text(id)), Some(Value::Text(owner))) = (row.get(0), row.get(1))
                else {
                    return Err(corpus_storage_error());
                };
                if owner == workspace_id && source_ids.contains(id.as_str()) {
                    lineage.owned.insert(id.clone());
                }
            }
        }
    }
    Ok(lineage)
}

fn load_rules(
    connection: &DbConnection,
    workspace_id: &str,
    scope: &MemoryScopeContext,
    paths: &[String],
    candidates: &mut Vec<AskCandidate>,
) -> Result<BTreeMap<String, AskNativeSource>, DomainError> {
    let mut rules = connection
        .list_procedural_rules(workspace_id, None, None, false)
        .map_err(|_| corpus_storage_error())?;
    // A rule's native review status is independent of its source memories.
    // Resolve holds before lineage, target matching, scoring or hint selection.
    let held = quarantine::held_ids(
        connection,
        workspace_id,
        quarantine::Target::Rule,
        &rules.iter().map(|rule| rule.id.as_str()).collect::<Vec<_>>(),
    )?;
    rules.retain(|rule| !held.contains(&rule.id));
    let mut native_sources = BTreeMap::new();
    if rules.is_empty() {
        return Ok(native_sources);
    }
    let workspace = connection
        .get_workspace(workspace_id)
        .map_err(|_| corpus_storage_error())?
        .ok_or_else(corpus_storage_error)?;
    let mut tags = connection
        .list_rule_tags_for_workspace(workspace_id)
        .map_err(|_| corpus_storage_error())?;
    let mut sources = connection
        .list_rule_source_memory_ids_for_workspace(workspace_id)
        .map_err(|_| corpus_storage_error())?;
    let mut projections = Vec::with_capacity(rules.len());
    for rule in rules {
        if rule.workspace_id != workspace_id {
            continue;
        }
        let rule_tags = tags.remove(&rule.id).unwrap_or_default();
        let source_ids = sources.remove(&rule.id).unwrap_or_default();
        let projection =
            crate::search::RuleIndexProjection::new(rule, &workspace.path, rule_tags, source_ids);
        if projection.is_pack_admissible() && rule_matches_targets(&projection, paths) {
            projections.push(projection);
        }
    }
    // Every read remains inside the caller's existing body/link snapshot.
    // Work is bounded by eligible rules' lineage, not all workspace history.
    let source_ids = projections
        .iter()
        .flat_map(|projection| projection.source_memory_ids().iter().map(String::as_str))
        .collect();
    let lineage = load_rule_lineage(connection, workspace_id, scope, &source_ids)?;
    for projection in projections {
        let rule = projection.rule();
        let source_ids = projection.source_memory_ids();
        if !source_ids.iter().all(|id| lineage.owned.contains(id)) {
            continue;
        }
        let visible = match scope.scope {
            MemoryScope::Workspace | MemoryScope::Swarm => true,
            MemoryScope::Global => {
                rule.scope == RuleScope::Global.as_str()
                    || crate::models::memory_tags_include_global_scope(projection.tags())
            }
            MemoryScope::Verified => matches!(
                TrustClass::from_str(&rule.trust_class),
                Ok(TrustClass::HumanExplicit
                    | TrustClass::PeerHumanAttested
                    | TrustClass::AgentValidated)
            ),
            // There is no durable producer field on a rule. Require a nonempty
            // fully-attributed lineage; one authorized parent cannot launder
            // another producer's contribution into self/team scope.
            MemoryScope::SelfOnly | MemoryScope::Team => {
                !source_ids.is_empty()
                    && source_ids.iter().all(|id| lineage.attributed.contains(id))
            }
        };
        if visible && let Some((candidate, source)) = admission::rule_candidate(&projection) {
            native_sources.insert(candidate.memory_id.clone(), source);
            candidates.push(candidate);
        }
    }
    Ok(native_sources)
}

fn target_usage_error(code: &str) -> DomainError {
    DomainError::Usage {
        // Never echo the submitted path or a filesystem diagnostic; either may
        // disclose a private absolute path through an otherwise safe answer.
        message: format!(
            "Invalid ee ask --path ({code}); use a literal workspace-relative target without glob characters or parent traversal"
        ),
        repair: Some(
            "ee ask \"What must I check?\" --path src/lib.rs --read-only --json".to_owned(),
        ),
    }
}

fn normalize_ask_targets(
    connection: &DbConnection,
    workspace_id: &str,
    paths: &[String],
) -> Result<Vec<String>, DomainError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let workspace = connection
        .get_workspace(workspace_id)
        .map_err(|_| corpus_storage_error())?
        .ok_or_else(corpus_storage_error)?;
    let mut normalized = BTreeSet::new();
    for path in paths {
        if path.trim().starts_with('~')
            || path
                .chars()
                .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
        {
            return Err(target_usage_error("non_literal_target"));
        }
        // Reuse the rule writer's portable path and symlink-escape contract.
        // Glob characters are rejected above so every existing target prefix
        // is inspected, rather than stopping at the first pattern component.
        let path = crate::search::normalize_rule_scope_pattern(
            std::path::Path::new(&workspace.path),
            RuleScope::FilePattern,
            Some(path),
        )
        .map_err(|error| target_usage_error(error.code()))?
        .ok_or_else(|| target_usage_error("missing_target"))?;
        normalized.insert(path);
    }
    Ok(normalized.into_iter().collect())
}

fn rule_matches_targets(projection: &crate::search::RuleIndexProjection, paths: &[String]) -> bool {
    match RuleScope::from_str(&projection.rule().scope) {
        Ok(RuleScope::Global | RuleScope::Workspace | RuleScope::Project) => true,
        Ok(scope @ (RuleScope::Directory | RuleScope::FilePattern)) => {
            let Some(pattern) = projection.normalized_scope_pattern() else {
                return false;
            };
            paths.iter().any(|path| {
                // Use recall's established case-sensitive fnmatch language.
                // Directory rules match whole ancestor components, never a
                // lexical prefix such as `src` matching `src-other`.
                std::iter::successors(Some(path.as_str()), |&parent| {
                    if scope == RuleScope::Directory {
                        parent.rsplit_once('/').map(|(prefix, _)| prefix)
                    } else {
                        None
                    }
                })
                .any(|target| crate::core::recall::recall_glob_match(pattern, target))
            })
        }
        Err(_) => false,
    }
}

/// Own only the read transaction that this operation successfully began.
/// A failed nested begin must never roll back a caller's existing transaction.
/// Errors and unwinding release our snapshot; a failed commit is rolled back
/// rather than leaving the connection pinned for later audit writes.
struct AskReadSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl<'a> AskReadSnapshot<'a> {
    fn begin(connection: &'a DbConnection) -> Result<Self, DomainError> {
        connection
            .begin_read_snapshot()
            .map_err(|_| snapshot_error("begin"))?;
        Ok(Self {
            connection,
            active: true,
        })
    }

    fn finish(mut self) -> Result<(), DomainError> {
        self.connection
            .commit_read_snapshot()
            .map_err(|_| snapshot_error("finish"))?;
        self.active = false;
        Ok(())
    }
}

impl Drop for AskReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            // Do not echo backend errors that may contain SQL or private paths.
            tracing::error!(
                target: "ee::core::ask::snapshot",
                "failed to release ask evidence read snapshot"
            );
        }
    }
}

fn snapshot_error(stage: &str) -> DomainError {
    DomainError::Storage {
        message: format!("Could not {stage} a coherent ask evidence snapshot; answer withheld"),
        repair: Some("retry ee ask; use ee doctor --json if the failure persists".to_owned()),
    }
}

fn corpus_storage_error() -> DomainError {
    DomainError::Storage {
        message: "Failed to read the ask evidence corpus".to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn invalid_validity_error() -> DomainError {
    DomainError::Storage {
        message: "Ask evidence contains invalid validity metadata; answer withheld".to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn validity_contains(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
    reference_time: DateTime<Utc>,
) -> Result<bool, DomainError> {
    let parse = |raw: &str| {
        DateTime::parse_from_rfc3339(raw)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| invalid_validity_error())
    };
    // Parse both bounds before testing visibility. An already-expired or
    // not-yet-active bound cannot hide malformed metadata in the other bound.
    let from = valid_from.map(parse).transpose()?;
    let to = valid_to.map(parse).transpose()?;
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(invalid_validity_error());
    }
    Ok(from.is_none_or(|from| from <= reference_time) && to.is_none_or(|to| reference_time <= to))
}

#[cfg(test)]
#[path = "ask_corpus_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ask_snapshot_tests.rs"]
mod snapshot_tests;

#[cfg(test)]
#[path = "ask_privacy_tests.rs"]
mod privacy_tests;

#[cfg(test)]
#[path = "ask_scope_tests.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "ask_native_tests.rs"]
mod native_tests;

#[cfg(test)]
#[path = "ask_evidence_tests.rs"]
mod evidence_tests;

#[cfg(test)]
mod source_authority_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::core::ask::{AskRequest, ask_data_json, evaluate_ask};
    use crate::db::{
        CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, MemoryLinkRelation,
        MemoryLinkSource,
    };

    const WORKSPACE: &str = "wsp_00000000000000000000000051";
    const PRIOR: &str = "mem_00000000000000000000000051";
    const CURRENT: &str = "mem_00000000000000000000000052";
    const CUTOFF: &str = "2026-09-17T12:00:00Z";
    const OLD_BODY: &str = "Never run cargo fmt before release.";
    const NEW_BODY: &str = "Run cargo fmt before release.";

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("fixture time")
            .with_timezone(&Utc)
    }

    fn fixture() -> (tempfile::TempDir, DbConnection) {
        let root = tempfile::tempdir().expect("temporary real store");
        let path = root.path().canonicalize().expect("physical root");
        let db = DbConnection::open_file(&path.join("ask.db")).expect("open store");
        db.migrate().expect("migrate real schema");
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .expect("workspace");
        (root, db)
    }

    fn seed(db: &DbConnection, id: &str, workspace: &str, body: &str) {
        db.insert_memory(
            id,
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: body.to_owned(),
                workflow_id: None,
                confidence: 1.0,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some("manual://source-authority".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
                valid_to: Some("2099-01-01T00:00:00Z".to_owned()),
            },
        )
        .expect("memory");
    }

    fn answer(corpus: &AskCorpus) -> crate::core::ask::AskReport {
        evaluate_ask(
            &AskRequest {
                question: "Run cargo fmt before release".to_owned(),
                contradictions: corpus.contradictions.clone(),
                native_sources: corpus.native_sources.clone(),
                ..AskRequest::default()
            },
            &corpus.candidates,
        )
    }

    fn withhold(db: &DbConnection, sealed: bool) {
        if sealed {
            db.insert_memory_seal(PRIOR, &format!("blake3:{}", "a".repeat(64)), CUTOFF)
                .expect("closed seal with a populated body");
        } else {
            assert!(
                db.restore_imported_memory_supersession(PRIOR, CUTOFF)
                    .expect("supersession independent of expiry")
            );
        }
    }

    #[test]
    fn corrected_advice_does_not_compete_with_its_superseded_revision() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, OLD_BODY);
        seed(&db, CURRENT, WORKSPACE, NEW_BODY);
        db.insert_memory_link(
            "link_00000000000000000000000051",
            &CreateMemoryLinkInput {
                src_memory_id: PRIOR.to_owned(),
                dst_memory_id: CURRENT.to_owned(),
                relation: MemoryLinkRelation::Contradicts,
                weight: 1.0,
                confidence: 1.0,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Human,
                created_by: None,
                metadata_json: None,
            },
        )
        .expect("explicit old/new opposition");
        withhold(&db, false);
        assert_eq!(
            db.get_memory(PRIOR).unwrap().unwrap().valid_to.as_deref(),
            Some("2099-01-01T00:00:00Z")
        );
        let corpus = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap();
        assert_eq!(corpus.candidates.len(), 1);
        assert_eq!(corpus.candidates[0].memory_id, CURRENT);
        assert!(corpus.contradictions.is_empty());
        let report = answer(&corpus);
        assert!(!report.abstained && !report.conflict_detected);
        assert_eq!(report.citations.len(), 1);
        assert_eq!(report.citations[0].memory_id, CURRENT);
        assert_eq!(report.citations[0].text, NEW_BODY);
        let output = ask_data_json(&report).to_string();
        assert!(!output.contains(PRIOR) && !output.contains(OLD_BODY));
    }

    #[test]
    fn closed_sources_do_not_escape_through_nearest_evidence_or_capture_hints() {
        for sealed in [false, true] {
            let (_root, db) = fixture();
            seed(&db, PRIOR, WORKSPACE, OLD_BODY);
            withhold(&db, sealed);
            let corpus = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap();
            assert!(corpus.candidates.is_empty());
            let report = answer(&corpus);
            assert!(report.abstained);
            assert!(report.nearest_evidence.as_ref().unwrap().is_empty());
            let output = ask_data_json(&report).to_string();
            assert!(!output.contains(PRIOR) && !output.contains(OLD_BODY));
            assert!(report.citations.is_empty() && report.sides.is_none());
        }
    }

    #[test]
    fn supersession_is_exclusive_and_compares_actual_rfc3339_instants() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, OLD_BODY);
        db.execute_raw("UPDATE memories SET superseded_at = '2026-09-17T08:00:00-04:00'")
            .unwrap();
        for (reference, expected) in [
            ("2026-09-17T11:59:59.999999999Z", 1),
            (CUTOFF, 0),
            ("2026-09-17T14:00:00+02:00", 0),
            ("2026-09-17T12:00:00.000000001Z", 0),
        ] {
            let corpus = load_current_ask_corpus(&db, WORKSPACE, at(reference)).unwrap();
            assert_eq!(corpus.candidates.len(), expected, "{reference}");
        }
    }

    #[test]
    fn a_real_seal_not_the_body_spelling_controls_readmission() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, NEW_BODY);
        withhold(&db, true);
        let before = db.get_memory(PRIOR).unwrap();
        let audits = db.count_table_rows("audit_log").unwrap();
        for reference in ["2026-01-01T00:00:00Z", CUTOFF] {
            assert!(
                load_current_ask_corpus(&db, WORKSPACE, at(reference))
                    .unwrap()
                    .candidates
                    .is_empty()
            );
        }
        assert_eq!(db.get_memory(PRIOR).unwrap(), before);
        assert_eq!(db.count_table_rows("audit_log").unwrap(), audits);
        assert!(db.mark_memory_seal_revealed(PRIOR, CUTOFF).unwrap());
        let revealed = db.get_memory(PRIOR).unwrap();
        let revealed_audits = db.count_table_rows("audit_log").unwrap();
        let corpus = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap();
        assert_eq!(corpus.candidates.len(), 1);
        assert_eq!(answer(&corpus).citations[0].text, NEW_BODY);
        assert_eq!(db.get_memory(PRIOR).unwrap(), revealed);
        assert_eq!(db.count_table_rows("audit_log").unwrap(), revealed_audits);
    }

    #[test]
    fn revealing_a_seal_cannot_resurrect_a_superseded_revision() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, OLD_BODY);
        withhold(&db, false);
        withhold(&db, true);
        assert!(db.mark_memory_seal_revealed(PRIOR, CUTOFF).unwrap());
        assert!(
            load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF))
                .unwrap()
                .candidates
                .is_empty()
        );
    }

    #[test]
    fn invalid_revision_fails_closed_without_leaking_or_pinning_the_snapshot() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, OLD_BODY);
        db.execute_raw("UPDATE memories SET superseded_at = 'PRIVATE-REVISION-CANARY'")
            .unwrap();
        let error = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap_err();
        assert!(matches!(error, DomainError::Storage { .. }));
        assert!(!format!("{error:?}").contains("PRIVATE-REVISION-CANARY"));
        assert!(!format!("{error:?}").contains(PRIOR));
        db.begin_read_snapshot().expect("owned snapshot released");
        db.rollback_read_snapshot().unwrap();
    }

    #[test]
    fn authority_and_bodies_observe_one_snapshot_during_a_concurrent_write() {
        for sealed in [false, true] {
            let (root, writer) = fixture();
            seed(&writer, PRIOR, WORKSPACE, NEW_BODY);
            let reader = DbConnection::open_file_read_only(
                &root.path().canonicalize().unwrap().join("ask.db"),
            )
            .unwrap();
            let corpus = load_corpus_with_boundary(&reader, WORKSPACE, at(CUTOFF), || {
                withhold(&writer, sealed);
                Ok(())
            })
            .unwrap();
            assert_eq!(corpus.candidates.len(), 1, "the captured body was public");
            assert_eq!(answer(&corpus).citations[0].text, NEW_BODY);
            assert!(
                load_current_ask_corpus(&reader, WORKSPACE, at(CUTOFF))
                    .unwrap()
                    .candidates
                    .is_empty(),
                "a later snapshot must observe the closure"
            );
        }
    }

    #[test]
    fn unrelated_workspace_authority_cannot_poison_the_selected_corpus() {
        let (root, db) = fixture();
        seed(&db, CURRENT, WORKSPACE, NEW_BODY);
        let other = "wsp_00000000000000000000000052";
        db.insert_workspace(
            other,
            &CreateWorkspaceInput {
                path: root.path().join("other").to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        seed(&db, PRIOR, other, OLD_BODY);
        db.execute_raw(&format!(
            "UPDATE memories SET superseded_at = 'PRIVATE-OTHER-CANARY' WHERE id = '{PRIOR}'"
        ))
        .unwrap();
        let corpus = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap();
        assert_eq!(corpus.candidates.len(), 1);
        assert_eq!(answer(&corpus).citations[0].memory_id, CURRENT);
    }

    #[test]
    fn populated_closed_seal_withholds_only_its_body_without_weakening_backup_checks() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, OLD_BODY);
        seed(&db, CURRENT, WORKSPACE, NEW_BODY);
        withhold(&db, true);
        assert!(db.list_memory_seals_for_recovery(WORKSPACE).is_err());
        let before = db.get_memory(PRIOR).unwrap();
        let audits = db.count_table_rows("audit_log").unwrap();
        let corpus = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap();
        assert_eq!(corpus.candidates.len(), 1);
        assert_eq!(answer(&corpus).citations[0].memory_id, CURRENT);
        let output = ask_data_json(&answer(&corpus)).to_string();
        assert!(!output.contains(PRIOR) && !output.contains(OLD_BODY));
        assert_eq!(db.get_memory(PRIOR).unwrap(), before);
        assert_eq!(db.count_table_rows("audit_log").unwrap(), audits);
        assert!(db.list_memory_seals_for_recovery(WORKSPACE).is_err());
    }

    #[test]
    fn invalid_live_seal_metadata_withholds_answers_and_releases_the_snapshot() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, OLD_BODY);
        seed(&db, CURRENT, WORKSPACE, NEW_BODY);
        withhold(&db, true);
        db.execute_raw(
            "UPDATE memory_seals SET revealed_at = 'PRIVATE-REVEAL-CANARY', reveal_verified = 1",
        )
        .unwrap();
        let error = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap_err();
        assert!(matches!(error, DomainError::Storage { .. }));
        assert!(!format!("{error:?}").contains("PRIVATE-REVEAL-CANARY"));
        db.begin_read_snapshot().expect("owned snapshot released");
        db.rollback_read_snapshot().unwrap();
    }

    #[test]
    fn revision_cutoff_selects_the_right_body_and_exact_citation() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, OLD_BODY);
        seed(&db, CURRENT, WORKSPACE, NEW_BODY);
        withhold(&db, false);
        db.execute_raw(&format!(
            "UPDATE memories SET valid_from = '{CUTOFF}' WHERE id = '{CURRENT}'"
        ))
        .unwrap();
        for (reference, expected_id, expected_body) in [
            ("2026-09-17T11:59:59.999999999Z", PRIOR, OLD_BODY),
            (CUTOFF, CURRENT, NEW_BODY),
            ("2026-09-17T14:00:00+02:00", CURRENT, NEW_BODY),
        ] {
            let corpus = load_current_ask_corpus(&db, WORKSPACE, at(reference)).unwrap();
            assert_eq!(corpus.candidates.len(), 1);
            assert_eq!(corpus.candidates[0].memory_id, expected_id);
            let report = answer(&corpus);
            assert!(!report.abstained);
            assert_eq!(report.citations.len(), 1);
            assert_eq!(report.citations[0].memory_id, expected_id);
            assert_eq!(report.citations[0].text, expected_body);
        }
    }

    #[test]
    fn historical_reference_never_reads_or_revives_a_tombstoned_body() {
        let (_root, db) = fixture();
        seed(&db, PRIOR, WORKSPACE, OLD_BODY);
        seed(&db, CURRENT, WORKSPACE, NEW_BODY);
        assert!(db.tombstone_memory(PRIOR).unwrap());
        // A tombstoned row is not part of live answer admission, even at an
        // earlier clock. Its malformed validity must never be decoded as a
        // candidate or stop an otherwise valid public answer.
        db.execute_raw(&format!(
            "UPDATE memories SET valid_from = 'PRIVATE-DEAD-CANARY' WHERE id = '{PRIOR}'"
        ))
        .unwrap();
        let audits = db.count_table_rows("audit_log").unwrap();
        for reference in ["2021-01-01T00:00:00Z", CUTOFF] {
            let loaded = load_memory_revisions(&db, WORKSPACE, at(reference)).unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].id, CURRENT);
            let corpus = load_current_ask_corpus(&db, WORKSPACE, at(reference)).unwrap();
            assert_eq!(answer(&corpus).citations[0].memory_id, CURRENT);
            let output = ask_data_json(&answer(&corpus)).to_string();
            assert!(!output.contains(PRIOR) && !output.contains("PRIVATE-DEAD-CANARY"));
        }
        assert_eq!(db.count_table_rows("audit_log").unwrap(), audits);
        assert!(
            load_memory_revisions(&db, "' OR 1 = 1 --", at(CUTOFF))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn revision_paging_preserves_history_and_validates_the_last_superseded_row() {
        let (_root, db) = fixture();
        let mut expected = Vec::new();
        db.with_transaction(|| {
            for ordinal in 1000..1000 + ASK_MEMORY_REVISION_PAGE_SIZE * 2 + 1 {
                let id = format!("mem_{ordinal:026}");
                seed(&db, &id, WORKSPACE, NEW_BODY);
                assert!(db.restore_imported_memory_supersession(&id, CUTOFF)?);
                expected.push(id);
            }
            Ok(())
        })
        .unwrap();
        let before =
            load_current_ask_corpus(&db, WORKSPACE, at("2026-09-17T11:59:59.999999999Z")).unwrap();
        assert_eq!(
            before
                .candidates
                .iter()
                .map(|candidate| candidate.memory_id.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF))
                .unwrap()
                .candidates
                .is_empty()
        );
        db.execute_raw(&format!(
            "UPDATE memories SET superseded_at = 'PRIVATE-TAIL-REVISION' WHERE id = '{}'",
            expected.last().unwrap()
        ))
        .unwrap();
        let error = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap_err();
        assert!(matches!(error, DomainError::Storage { .. }));
        assert!(!format!("{error:?}").contains("PRIVATE-TAIL-REVISION"));
        db.begin_read_snapshot()
            .expect("failed read releases its snapshot");
        db.rollback_read_snapshot().unwrap();
    }

    #[test]
    fn concurrent_replacement_belongs_entirely_to_the_next_answer_snapshot() {
        let (root, writer) = fixture();
        seed(&writer, PRIOR, WORKSPACE, OLD_BODY);
        let reader =
            DbConnection::open_file_read_only(&root.path().canonicalize().unwrap().join("ask.db"))
                .unwrap();
        let captured = load_corpus_with_boundary(&reader, WORKSPACE, at(CUTOFF), || {
            writer
                .with_transaction(|| {
                    seed(&writer, CURRENT, WORKSPACE, NEW_BODY);
                    assert!(writer.restore_imported_memory_supersession(PRIOR, CUTOFF)?);
                    Ok(())
                })
                .unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(captured.candidates.len(), 1);
        assert_eq!(answer(&captured).citations[0].memory_id, PRIOR);
        let next = load_current_ask_corpus(&reader, WORKSPACE, at(CUTOFF)).unwrap();
        assert_eq!(next.candidates.len(), 1);
        assert_eq!(answer(&next).citations[0].memory_id, CURRENT);
        assert!(!ask_data_json(&answer(&next)).to_string().contains(OLD_BODY));
    }
}

#[cfg(test)]
#[path = "ask_rule_lineage_tests.rs"]
mod rule_lineage_tests;
