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

#[derive(Clone, Debug)]
pub struct AskCorpus {
    pub candidates: Vec<AskCandidate>,
    pub contradictions: Vec<AskContradiction>,
    pub native_sources: BTreeMap<String, AskNativeSource>,
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
    let stored = connection
        .list_memories(workspace_id, None, false)
        .map_err(|_| corpus_storage_error())?;
    let scope = scope_context()?;
    // Producer membership can scope a derived rule, but the source memory's
    // body is not substituted for that rule. Rule lifecycle is independent of
    // the source memory's validity window.
    let attributed_memories: BTreeSet<_> = stored
        .iter()
        .filter(|memory| memory.workspace_id == workspace_id && scope.memory_in_scope(memory))
        .map(|memory| memory.id.clone())
        .collect();
    after_memory_read()?;
    // V123 separates revision identity from author expiry. Neither a future
    // valid_to nor an unexpectedly populated sealed body grants admission.
    // Read both authority tables in bulk inside this same body/link snapshot;
    // never reopen the store or perform one authority query per memory.
    let withheld = if stored.is_empty() {
        BTreeSet::new()
    } else {
        withheld_memory_ids(connection, workspace_id, reference_time)?
    };
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
            && !withheld.contains(&memory.id)
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
    let mut native_sources = load_rules(
        connection,
        workspace_id,
        &scope,
        &attributed_memories,
        &paths,
        &mut candidates,
    )?;
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
    let mut withheld: BTreeSet<_> = connection
        .list_memory_seals_for_recovery(workspace_id)
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

fn load_rules(
    connection: &DbConnection,
    workspace_id: &str,
    scope: &MemoryScopeContext,
    attributed_memories: &BTreeSet<String>,
    paths: &[String],
    candidates: &mut Vec<AskCandidate>,
) -> Result<BTreeMap<String, AskNativeSource>, DomainError> {
    let rules = connection
        .list_procedural_rules(workspace_id, None, None, false)
        .map_err(|_| corpus_storage_error())?;
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
    for rule in rules {
        if rule.workspace_id != workspace_id {
            continue;
        }
        let rule_tags = tags.remove(&rule.id).unwrap_or_default();
        let source_ids = sources.remove(&rule.id).unwrap_or_default();
        let visible = match scope.scope {
            MemoryScope::Workspace | MemoryScope::Swarm => true,
            MemoryScope::Global => {
                rule.scope == RuleScope::Global.as_str()
                    || crate::models::memory_tags_include_global_scope(&rule_tags)
            }
            MemoryScope::Verified => matches!(
                TrustClass::from_str(&rule.trust_class),
                Ok(TrustClass::HumanExplicit
                    | TrustClass::PeerHumanAttested
                    | TrustClass::AgentValidated)
            ),
            // There is no durable producer field on a rule. Require a nonempty
            // fully-attributed lineage; a single authorized parent cannot
            // launder another producer's contribution into self/team scope.
            MemoryScope::SelfOnly | MemoryScope::Team => {
                !source_ids.is_empty()
                    && source_ids.iter().all(|id| attributed_memories.contains(id))
            }
        };
        if !visible {
            continue;
        }
        let projection =
            crate::search::RuleIndexProjection::new(rule, &workspace.path, rule_tags, source_ids);
        if !rule_matches_targets(&projection, paths) {
            continue;
        }
        if let Some((candidate, source)) = admission::rule_candidate(&projection) {
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
}
