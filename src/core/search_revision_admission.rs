//! Source-backed revision admission before ranking and duplicate suppression.
//!
//! Historical versions remain indexed for `--as-of`. V123 made supersession
//! independent of author expiry, so inclusion flags cannot revive an obsolete
//! revision. Closed seals withhold candidates at every reference time, even
//! when retained index generations still contain their old body. Indexed
//! metadata is not authority for either decision. Pending feedback quarantine
//! holds also come from this snapshot, not from an old index or a trust label.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use super::{DbConnection, SearchDegradation, SearchHit, SearchOptions};
use crate::db::{DbError, DbOperation};
use crate::models::MemoryId;

const PAGE_SIZE: usize = 256;
const FILTERED: &str = "superseded_revision_filtered";
const SEALED_FILTERED: &str = "sealed_memory_filtered";
const QUARANTINED_FILTERED: &str = "quarantined_memory_filtered";
pub(in crate::core::search) const UNAVAILABLE: &str = "revision_visibility_unavailable";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RevisionState {
    Current,
    Superseded(DateTime<Utc>),
    Sealed,
    Quarantined,
    Malformed,
}

impl RevisionState {
    fn visible_at(self, reference: DateTime<Utc>) -> bool {
        match self {
            Self::Current => true,
            Self::Superseded(at) => reference < at,
            Self::Sealed | Self::Quarantined | Self::Malformed => false,
        }
    }
}

fn is_memory(hit: &SearchHit) -> bool {
    hit.doc_id.parse::<MemoryId>().is_ok()
}

fn states(
    connection: &DbConnection,
    ids: &BTreeSet<&str>,
) -> Result<BTreeMap<String, RevisionState>, DbError> {
    let ids: Vec<_> = ids.iter().copied().collect();
    let mut result = BTreeMap::new();
    for page in ids.chunks(PAGE_SIZE) {
        let placeholders = (1..=page.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT m.id, m.superseded_at, s.memory_id, s.revealed_at FROM memories AS m LEFT JOIN memory_seals AS s ON s.memory_id = m.id WHERE m.id IN ({placeholders}) ORDER BY m.id ASC"
        );
        let parameters = page
            .iter()
            .map(|id| Value::Text((*id).to_owned()))
            .collect::<Vec<_>>();
        for row in connection.query(&sql, &parameters)? {
            let Some(Value::Text(id)) = row.get(0) else {
                return Err(DbError::MalformedRow {
                    operation: DbOperation::Query,
                    message: "Could not read memory revision identity".to_owned(),
                });
            };
            let state = match row.get(1) {
                Some(Value::Null) => RevisionState::Current,
                Some(Value::Text(raw)) => DateTime::parse_from_rfc3339(raw)
                    .map(|at| RevisionState::Superseded(at.with_timezone(&Utc)))
                    .unwrap_or(RevisionState::Malformed),
                _ => RevisionState::Malformed,
            };
            // Match MemorySeal::is_sealed: a present seal without revealed_at
            // is closed. The clock, body spelling, indexed reveal flags and
            // reveal_verified do not override this source authority. Retain
            // malformed revision failures even while the body is sealed.
            let state = match (row.get(2), row.get(3)) {
                (Some(Value::Null), Some(Value::Null)) => state,
                (Some(Value::Text(seal_id)), Some(Value::Null)) if seal_id == id => {
                    if state == RevisionState::Malformed {
                        state
                    } else {
                        RevisionState::Sealed
                    }
                }
                (Some(Value::Text(seal_id)), Some(Value::Text(_))) if seal_id == id => state,
                _ => RevisionState::Malformed,
            };
            result.insert(id.clone(), state);
        }
        // Review holds are current authority even for historical reads. Match
        // both the native target type and its owning workspace: a foreign hold
        // or feedback about a derived rule must not suppress this memory.
        // DISTINCT bounds the result to one row per candidate even when many
        // sources have pending events against the same target. Never read the
        // quarantined feedback body, reason, source identity or event payload.
        let quarantine_sql = format!(
            "SELECT DISTINCT m.id FROM memories AS m JOIN feedback_quarantine AS q ON q.target_id = m.id AND q.workspace_id = m.workspace_id WHERE q.target_type = 'memory' AND q.status = 'pending' AND m.id IN ({placeholders}) ORDER BY m.id ASC"
        );
        for row in connection.query(&quarantine_sql, &parameters)? {
            let Some(Value::Text(id)) = row.get(0) else {
                return Err(DbError::MalformedRow {
                    operation: DbOperation::Query,
                    message: "Could not read memory quarantine identity".to_owned(),
                });
            };
            let Some(state) = result.get_mut(id) else {
                return Err(DbError::MalformedRow {
                    operation: DbOperation::Query,
                    message: "Could not verify memory quarantine authority".to_owned(),
                });
            };
            // Do not mask malformed source authority or disclose a quarantine
            // decision instead of the stronger closed-seal explanation.
            if !matches!(state, RevisionState::Malformed | RevisionState::Sealed) {
                *state = RevisionState::Quarantined;
            }
        }
    }
    Ok(result)
}

fn load_states(
    options: &SearchOptions,
    ids: &BTreeSet<&str>,
    read_connection: Option<&DbConnection>,
) -> Result<BTreeMap<String, RevisionState>, DbError> {
    if let Some(connection) = read_connection {
        // The caller owns the snapshot. Never begin or release its transaction.
        return states(connection, ids);
    }
    let connection = DbConnection::open_file_read_only(&options.resolve_database_path())?;
    let snapshot = RevisionReadSnapshot::begin(&connection)?;
    let result = states(&connection, ids)?;
    snapshot.finish()?;
    Ok(result)
}

fn unavailable() -> SearchDegradation {
    SearchDegradation {
        code: UNAVAILABLE.to_owned(),
        severity: "medium".to_owned(),
        message: "Memory candidates whose revision, seal or quarantine authority could not be verified were withheld; unrelated entity types remain available.".to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

/// Filter identities belonging to the addressed source snapshot. Missing rows
/// are left to the existing orphan/scope gates: the merged candidate pool can
/// include independently admitted global-store rows. This does not authenticate
/// those rows or replace the separate global-store admission boundary.
pub(in crate::core::search) fn admit_hits(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
) -> Vec<SearchHit> {
    let ids = hits
        .iter()
        .filter(|hit| is_memory(hit))
        .map(|hit| hit.doc_id.as_str())
        .collect::<BTreeSet<_>>();
    if ids.is_empty() {
        return hits;
    }
    let states = match load_states(options, &ids, read_connection) {
        Ok(states) => states,
        Err(_) => {
            // Do not echo SQL, host paths, raw IDs or malformed timestamps.
            degraded.push(unavailable());
            return hits.into_iter().filter(|hit| !is_memory(hit)).collect();
        }
    };
    let reference = options.as_of.unwrap_or_else(Utc::now);
    let mut superseded = 0usize;
    let mut sealed = 0usize;
    let mut quarantined = 0usize;
    let mut malformed = 0usize;
    let hits = hits
        .into_iter()
        .filter(|hit| match states.get(&hit.doc_id).copied() {
            Some(RevisionState::Malformed) => {
                malformed += 1;
                false
            }
            Some(RevisionState::Sealed) => {
                sealed += 1;
                false
            }
            Some(RevisionState::Quarantined) => {
                quarantined += 1;
                false
            }
            Some(state) if !state.visible_at(reference) => {
                superseded += 1;
                false
            }
            _ => true,
        })
        .collect();
    if superseded > 0 {
        degraded.push(SearchDegradation {
            code: FILTERED.to_owned(),
            severity: "low".to_owned(),
            message: format!("Excluded {superseded} superseded memory revisions at the requested reference time. Author expiry and inclusion flags do not change revision identity."),
            repair: Some("Use --as-of <RFC3339> before supersession to inspect historical revisions.".to_owned()),
        });
    }
    if sealed > 0 {
        degraded.push(SearchDegradation {
            code: SEALED_FILTERED.to_owned(),
            severity: "low".to_owned(),
            message: format!("Withheld {sealed} sealed memory candidates before ranking. Historical reference times and inclusion flags do not reveal committed content."),
            repair: None,
        });
    }
    if malformed > 0 {
        degraded.push(unavailable());
    }
    if quarantined > 0 {
        degraded.push(SearchDegradation {
            code: QUARANTINED_FILTERED.to_owned(),
            severity: "low".to_owned(),
            message: format!("Withheld {quarantined} memory candidates with pending feedback quarantine review. Historical reference times, indexed trust labels and inclusion flags do not release a review hold."),
            repair: Some("Review pending feedback with ee outcome quarantine list in the addressed workspace.".to_owned()),
        });
    }
    hits
}

/// Source truth for a similarity seed: missing, closed or quarantined evidence
/// cannot drive lexical query construction or semantic embedding. The caller
/// owns a snapshot covering the body and all of these admission reads.
pub(in crate::core::search) fn seed_is_visible(
    connection: &DbConnection,
    id: &str,
    reference: DateTime<Utc>,
) -> Result<bool, DbError> {
    let states = states(connection, &BTreeSet::from([id]))?;
    match states.get(id).copied() {
        Some(RevisionState::Malformed) => Err(DbError::MalformedRow {
            operation: DbOperation::Query,
            message: "Could not verify similarity seed revision state".to_owned(),
        }),
        Some(state) if state.visible_at(reference) => Ok(true),
        _ => Ok(false),
    }
}

/// Own only snapshots begun here. A nested begin must not release a caller's
/// existing transaction, and early returns must never leave a snapshot pinned.
pub(in crate::core::search) struct RevisionReadSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl<'a> RevisionReadSnapshot<'a> {
    pub(in crate::core::search) fn begin(connection: &'a DbConnection) -> Result<Self, DbError> {
        connection.begin_read_snapshot()?;
        Ok(Self {
            connection,
            active: true,
        })
    }

    /// Canonical retrieval also accepts caller-provided connections. Borrow
    /// only the recognized already-open transaction; other BEGIN failures do
    /// not establish a snapshot and must propagate.
    pub(in crate::core::search) fn begin_or_borrow(
        connection: &'a DbConnection,
    ) -> Result<Self, DbError> {
        let active = match connection.begin_read_snapshot() {
            Ok(()) => true,
            Err(error) if crate::db::db_error_is_nested_transaction(&error) => false,
            Err(error) => return Err(error),
        };
        Ok(Self { connection, active })
    }

    pub(in crate::core::search) fn finish(mut self) -> Result<(), DbError> {
        if self.active {
            self.connection.rollback_read_snapshot()?;
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for RevisionReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            // Do not expose database paths or source evidence on cleanup.
            tracing::error!(target: "ee::search::revision", "could not release revision read snapshot");
        }
    }
}

#[cfg(test)]
#[path = "search_seed_admission_tests.rs"]
mod seed_tests;

#[cfg(test)]
#[path = "search_revision_admission_tests.rs"]
mod tests;

#[cfg(test)]
mod seal_tests {
    use super::super::super::{ScoreSource, SearchDedupMode, SearchSourceMode, SpeedMode};
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, StoredFeedbackQuarantine};
    use crate::models::MemoryScope;
    use serde_json::json;

    type TestResult = Result<(), String>;
    const WORKSPACE: &str = "wsp_00000000000000000000000081";
    const HIDDEN: &str = "mem_00000000000000000000000081";
    const PUBLIC: &str = "mem_00000000000000000000000082";
    const TIME: &str = "2026-01-01T00:00:00Z";
    const BODY: &str = "Run release validation before publishing.";

    fn instant(raw: &str) -> Result<DateTime<Utc>, String> {
        DateTime::parse_from_rfc3339(raw)
            .map(|at| at.with_timezone(&Utc))
            .map_err(|error| error.to_string())
    }

    fn input() -> CreateMemoryInput {
        CreateMemoryInput {
            workspace_id: WORKSPACE.to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: BODY.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.6,
            provenance_uri: Some("manual://seal-admission".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        }
    }

    fn fixture() -> Result<(tempfile::TempDir, SearchOptions, DbConnection), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        std::fs::create_dir(root.join(".ee")).map_err(|error| error.to_string())?;
        std::fs::write(
            root.join(".ee/config.toml"),
            "[memory]\ninclude_global = false\n",
        )
        .map_err(|error| error.to_string())?;
        let database = root.join("seals.db");
        let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        db.migrate().map_err(|error| error.to_string())?;
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: root.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .map_err(|error| error.to_string())?;
        for id in [HIDDEN, PUBLIC] {
            db.insert_memory(id, &input())
                .map_err(|error| error.to_string())?;
        }
        let options = SearchOptions {
            workspace_path: root.clone(),
            database_path: Some(database),
            index_dir: Some(root.join("index")),
            query: "release validation".to_owned(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
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
        };
        Ok((temp, options, db))
    }

    fn seal(db: &DbConnection, id: &str) -> TestResult {
        db.insert_memory_seal(id, &format!("blake3:{}", "a".repeat(64)), TIME)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn quarantine(
        db: &DbConnection,
        ordinal: usize,
        workspace: &str,
        target_type: &str,
        target_id: &str,
        status: &str,
    ) -> Result<(), DbError> {
        db.insert_feedback_quarantine_for_recovery(&StoredFeedbackQuarantine {
            id: format!("fq_{ordinal:026}"),
            workspace_id: workspace.to_owned(),
            source_id: "private-quarantine-source-canary".to_owned(),
            target_type: target_type.to_owned(),
            target_id: target_id.to_owned(),
            signal: "harmful".to_owned(),
            weight: 0.5,
            source_type: "outcome_observed".to_owned(),
            proposed_event_id: Some(format!("fb_{ordinal:026}")),
            recorded_at: TIME.to_owned(),
            reason: "private-quarantine-reason-canary".to_owned(),
            event_reason: None,
            evidence_json: None,
            session_id: None,
            raw_event_hash: format!("blake3:{ordinal:064x}"),
            status: status.to_owned(),
            reviewed_at: (status != "pending").then(|| TIME.to_owned()),
            reviewed_by: (status != "pending").then(|| "test-reviewer".to_owned()),
            released_feedback_event_id: None,
        })
    }

    fn reject_hold(db: &DbConnection, ordinal: usize) -> TestResult {
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
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.9),
            rerank_score: None,
            metadata: Some(json!({"content": BODY, "sealed": false, "revealed_at": TIME})),
            explanation: None,
        }
    }

    fn ids(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.doc_id.as_str()).collect()
    }

    #[test]
    fn pending_quarantine_withholds_candidates_and_seeds_at_every_reference_time() -> TestResult {
        let (_temp, mut options, db) = fixture()?;
        quarantine(&db, 1, WORKSPACE, "memory", HIDDEN, "pending")
            .map_err(|error| error.to_string())?;
        options.include_tombstoned = true;
        options.include_expired = true;
        options.include_future = true;
        options.include_stale = true;
        for reference in ["2020-01-01T00:00:00Z", "2030-01-01T00:00:00Z"] {
            options.as_of = Some(instant(reference)?);
            let mut candidate = hit(HIDDEN);
            candidate.metadata = Some(json!({
                "content": BODY,
                "trust_class": "human_explicit",
                "quarantined": false,
            }));
            let mut degraded = Vec::new();
            let visible = admit_hits(
                &options,
                vec![candidate, hit(PUBLIC), hit("evd_other"), hit("rule_other")],
                &mut degraded,
                None,
            );
            assert_eq!(ids(&visible), vec![PUBLIC, "evd_other", "rule_other"]);
            assert_eq!(visible[0].score.to_bits(), 0.9_f32.to_bits());
            assert_eq!(degraded.len(), 1);
            assert_eq!(degraded[0].code, QUARANTINED_FILTERED);
            for private in [HIDDEN, BODY, "private-quarantine"] {
                assert!(!degraded[0].message.contains(private));
                assert!(
                    !degraded[0]
                        .repair
                        .as_deref()
                        .unwrap_or("")
                        .contains(private)
                );
            }
            assert!(
                !seed_is_visible(&db, HIDDEN, instant(reference)?)
                    .map_err(|error| error.to_string())?
            );
            assert!(
                seed_is_visible(&db, PUBLIC, instant(reference)?)
                    .map_err(|error| error.to_string())?
            );
        }
        Ok(())
    }

    #[test]
    fn quarantine_requires_native_target_and_owning_workspace() -> TestResult {
        let (_temp, options, db) = fixture()?;
        let foreign = "wsp_00000000000000000000000083";
        db.insert_workspace(
            foreign,
            &CreateWorkspaceInput {
                path: options
                    .workspace_path
                    .join("foreign")
                    .to_string_lossy()
                    .into_owned(),
                name: None,
            },
        )
        .map_err(|error| error.to_string())?;
        quarantine(&db, 1, foreign, "memory", HIDDEN, "pending")
            .map_err(|error| error.to_string())?;
        quarantine(&db, 2, WORKSPACE, "rule", HIDDEN, "pending")
            .map_err(|error| error.to_string())?;
        quarantine(&db, 3, WORKSPACE, "memory", HIDDEN, "rejected")
            .map_err(|error| error.to_string())?;
        let mut degraded = Vec::new();
        let visible = admit_hits(&options, vec![hit(HIDDEN), hit(PUBLIC)], &mut degraded, None);
        assert_eq!(ids(&visible), vec![HIDDEN, PUBLIC]);
        assert!(degraded.is_empty());
        quarantine(&db, 4, WORKSPACE, "memory", HIDDEN, "pending")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            ids(&admit_hits(
                &options,
                vec![hit(HIDDEN), hit(PUBLIC)],
                &mut Vec::new(),
                None
            )),
            vec![PUBLIC]
        );
        Ok(())
    }

    #[test]
    fn every_pending_hold_must_be_reviewed_without_changing_source_or_index() -> TestResult {
        let (_temp, options, db) = fixture()?;
        for ordinal in [1, 2] {
            quarantine(&db, ordinal, WORKSPACE, "memory", HIDDEN, "pending")
                .map_err(|error| error.to_string())?;
        }
        let before = db.get_memory(HIDDEN).map_err(|error| error.to_string())?;
        let audits = db
            .count_table_rows("audit_log")
            .map_err(|error| error.to_string())?;
        let holds = db
            .list_feedback_quarantine(WORKSPACE, None)
            .map_err(|error| error.to_string())?;
        let mut degraded = Vec::new();
        assert!(admit_hits(&options, vec![hit(HIDDEN)], &mut degraded, None).is_empty());
        assert!(degraded[0].message.starts_with("Withheld 1 memory"));
        assert_eq!(
            db.list_feedback_quarantine(WORKSPACE, None)
                .map_err(|error| error.to_string())?,
            holds
        );
        assert_eq!(
            db.get_memory(HIDDEN).map_err(|error| error.to_string())?,
            before
        );
        assert_eq!(
            db.count_table_rows("audit_log")
                .map_err(|error| error.to_string())?,
            audits
        );
        assert!(!options.workspace_path.join("index").exists());
        reject_hold(&db, 1)?;
        assert!(admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), None).is_empty());
        reject_hold(&db, 2)?;
        assert_eq!(
            ids(&admit_hits(
                &options,
                vec![hit(HIDDEN)],
                &mut Vec::new(),
                None
            )),
            vec![HIDDEN]
        );
        assert_eq!(
            db.get_memory(HIDDEN).map_err(|error| error.to_string())?,
            before
        );
        Ok(())
    }

    #[test]
    fn quarantine_review_does_not_override_seals_or_supersession() -> TestResult {
        let (_temp, options, db) = fixture()?;
        quarantine(&db, 1, WORKSPACE, "memory", HIDDEN, "pending")
            .map_err(|error| error.to_string())?;
        seal(&db, HIDDEN)?;
        let mut degraded = Vec::new();
        assert!(admit_hits(&options, vec![hit(HIDDEN)], &mut degraded, None).is_empty());
        assert_eq!(degraded[0].code, SEALED_FILTERED);
        assert!(
            db.mark_memory_seal_revealed(HIDDEN, TIME)
                .map_err(|error| error.to_string())?
        );
        let mut degraded = Vec::new();
        assert!(admit_hits(&options, vec![hit(HIDDEN)], &mut degraded, None).is_empty());
        assert_eq!(degraded[0].code, QUARANTINED_FILTERED);
        assert!(
            db.restore_imported_memory_supersession(HIDDEN, TIME)
                .map_err(|error| error.to_string())?
        );
        reject_hold(&db, 1)?;
        let mut degraded = Vec::new();
        assert!(admit_hits(&options, vec![hit(HIDDEN)], &mut degraded, None).is_empty());
        assert_eq!(degraded[0].code, FILTERED);
        Ok(())
    }

    #[test]
    fn quarantine_transitions_obey_the_callers_snapshot_in_both_directions() -> TestResult {
        for initially_held in [false, true] {
            let (_temp, options, writer) = fixture()?;
            if initially_held {
                quarantine(&writer, 1, WORKSPACE, "memory", HIDDEN, "pending")
                    .map_err(|error| error.to_string())?;
            }
            let reader = DbConnection::open_file_read_only(&options.resolve_database_path())
                .map_err(|error| error.to_string())?;
            reader
                .begin_read_snapshot()
                .map_err(|error| error.to_string())?;
            let before = admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), Some(&reader));
            assert_eq!(before.is_empty(), initially_held);
            if initially_held {
                reject_hold(&writer, 1)?;
            } else {
                quarantine(&writer, 1, WORKSPACE, "memory", HIDDEN, "pending")
                    .map_err(|error| error.to_string())?;
            }
            let captured = admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), Some(&reader));
            assert_eq!(ids(&captured), ids(&before));
            assert_eq!(
                seed_is_visible(&reader, HIDDEN, instant(TIME)?)
                    .map_err(|error| error.to_string())?,
                !initially_held
            );
            assert!(
                reader.begin_read_snapshot().is_err(),
                "caller still owns the snapshot"
            );
            reader
                .rollback_read_snapshot()
                .map_err(|error| error.to_string())?;
            let next = admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), Some(&reader));
            assert_eq!(next.is_empty(), !initially_held);
        }
        Ok(())
    }

    #[test]
    fn unavailable_quarantine_authority_withholds_memories_and_similarity_seeds() -> TestResult {
        let (_temp, options, db) = fixture()?;
        db.execute_raw("ALTER TABLE feedback_quarantine RENAME TO private_unavailable_quarantine")
            .map_err(|error| error.to_string())?;
        let mut degraded = Vec::new();
        let visible = admit_hits(
            &options,
            vec![hit(HIDDEN), hit("evd_other"), hit("rule_other")],
            &mut degraded,
            None,
        );
        assert_eq!(ids(&visible), vec!["evd_other", "rule_other"]);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, UNAVAILABLE);
        assert!(!degraded[0].message.contains("private_unavailable_quarantine"));
        assert!(seed_is_visible(&db, HIDDEN, instant(TIME)?).is_err());
        db.execute_raw("ALTER TABLE private_unavailable_quarantine RENAME TO feedback_quarantine")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            ids(&admit_hits(
                &options,
                vec![hit(HIDDEN)],
                &mut Vec::new(),
                None
            )),
            vec![HIDDEN]
        );
        Ok(())
    }

    #[test]
    fn quarantine_candidate_pages_preserve_order_and_unknown_store_ids() -> TestResult {
        let (_temp, options, db) = fixture()?;
        let mut candidates = Vec::new();
        let mut expected = Vec::new();
        db.with_transaction(|| {
            for ordinal in 1000..1000 + PAGE_SIZE * 2 + 1 {
                let id = MemoryId::from_uuid(uuid::Uuid::from_u128(ordinal as u128)).to_string();
                db.insert_memory(&id, &input())?;
                if ordinal % 2 == 0 {
                    quarantine(&db, ordinal, WORKSPACE, "memory", &id, "pending")?;
                } else {
                    expected.push(id.clone());
                }
                candidates.push(hit(&id));
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
        let unknown = MemoryId::from_uuid(uuid::Uuid::from_u128(9000)).to_string();
        candidates.push(hit(&unknown));
        expected.push(unknown);
        candidates.reverse();
        expected.reverse();
        let visible = admit_hits(&options, candidates, &mut Vec::new(), None);
        assert_eq!(
            ids(&visible),
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn public_search_rechecks_quarantine_without_rebuilding_the_index() -> TestResult {
        let (_temp, options, db) = fixture()?;
        db.close().map_err(|error| error.to_string())?;
        crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
            workspace_path: options.workspace_path.clone(),
            database_path: options.database_path.clone(),
            index_dir: options.index_dir.clone(),
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;
        let before = crate::core::search::run_search_unaudited(&options)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            ids(&before.results).into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([HIDDEN, PUBLIC])
        );
        let writer = DbConnection::open_file(&options.resolve_database_path())
            .map_err(|error| error.to_string())?;
        quarantine(&writer, 1, WORKSPACE, "memory", HIDDEN, "pending")
            .map_err(|error| error.to_string())?;
        writer.close().map_err(|error| error.to_string())?;
        let after = crate::core::search::run_search_unaudited(&options)
            .map_err(|error| error.to_string())?;
        assert_eq!(ids(&after.results), vec![PUBLIC]);
        assert!(
            after
                .degraded
                .iter()
                .any(|entry| entry.code == QUARANTINED_FILTERED)
        );
        let writer = DbConnection::open_file(&options.resolve_database_path())
            .map_err(|error| error.to_string())?;
        reject_hold(&writer, 1)?;
        writer.close().map_err(|error| error.to_string())?;
        let reviewed = crate::core::search::run_search_unaudited(&options)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            ids(&reviewed.results).into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([HIDDEN, PUBLIC])
        );
        Ok(())
    }

    #[test]
    fn closed_seals_override_indexed_bodies_reveal_claims_and_inclusion_flags() -> TestResult {
        let (_temp, mut options, db) = fixture()?;
        seal(&db, HIDDEN)?;
        options.include_tombstoned = true;
        options.include_expired = true;
        options.include_future = true;
        options.include_stale = true;
        for reference in ["2020-01-01T00:00:00Z", "2030-01-01T00:00:00Z"] {
            options.as_of = Some(instant(reference)?);
            let mut degraded = Vec::new();
            let visible = admit_hits(
                &options,
                vec![hit(HIDDEN), hit(PUBLIC), hit("evd_other")],
                &mut degraded,
                None,
            );
            assert_eq!(ids(&visible), vec![PUBLIC, "evd_other"]);
            assert_eq!(visible[0].score.to_bits(), 0.9_f32.to_bits());
            assert_eq!(degraded.len(), 1);
            assert_eq!(degraded[0].code, SEALED_FILTERED);
            assert!(!degraded[0].message.contains(HIDDEN));
            assert!(!degraded[0].message.contains(BODY));
            assert!(degraded[0].repair.is_none());
            assert!(
                !seed_is_visible(&db, HIDDEN, instant(reference)?)
                    .map_err(|error| error.to_string())?
            );
        }
        Ok(())
    }

    #[test]
    fn reveal_readmits_content_but_cannot_resurrect_a_superseded_revision() -> TestResult {
        let (_temp, options, db) = fixture()?;
        seal(&db, HIDDEN)?;
        let before = db.get_memory(HIDDEN).map_err(|error| error.to_string())?;
        let audits = db
            .count_table_rows("audit_log")
            .map_err(|error| error.to_string())?;
        assert!(admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), None).is_empty());
        assert_eq!(
            db.get_memory(HIDDEN).map_err(|error| error.to_string())?,
            before
        );
        assert_eq!(
            db.count_table_rows("audit_log")
                .map_err(|error| error.to_string())?,
            audits
        );
        assert!(
            db.mark_memory_seal_revealed(HIDDEN, TIME)
                .map_err(|error| error.to_string())?
        );
        assert_eq!(
            ids(&admit_hits(
                &options,
                vec![hit(HIDDEN)],
                &mut Vec::new(),
                None
            )),
            vec![HIDDEN]
        );
        assert!(
            seed_is_visible(&db, HIDDEN, instant("2030-01-01T00:00:00Z")?)
                .map_err(|error| error.to_string())?
        );
        assert!(
            db.restore_imported_memory_supersession(HIDDEN, TIME)
                .map_err(|error| error.to_string())?
        );
        let mut degraded = Vec::new();
        assert!(admit_hits(&options, vec![hit(HIDDEN)], &mut degraded, None).is_empty());
        assert_eq!(degraded[0].code, FILTERED);
        Ok(())
    }

    #[test]
    fn seal_transitions_obey_the_callers_snapshot_in_both_directions() -> TestResult {
        for initially_closed in [false, true] {
            let (_temp, options, writer) = fixture()?;
            if initially_closed {
                seal(&writer, HIDDEN)?;
            }
            let reader = DbConnection::open_file_read_only(&options.resolve_database_path())
                .map_err(|error| error.to_string())?;
            reader
                .begin_read_snapshot()
                .map_err(|error| error.to_string())?;
            let before = admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), Some(&reader));
            assert_eq!(before.is_empty(), initially_closed);
            if initially_closed {
                assert!(
                    writer
                        .mark_memory_seal_revealed(HIDDEN, TIME)
                        .map_err(|error| error.to_string())?
                );
            } else {
                seal(&writer, HIDDEN)?;
            }
            let captured = admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), Some(&reader));
            assert_eq!(ids(&captured), ids(&before));
            assert!(
                reader.begin_read_snapshot().is_err(),
                "caller still owns the snapshot"
            );
            reader
                .rollback_read_snapshot()
                .map_err(|error| error.to_string())?;
            let next = admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), Some(&reader));
            assert_eq!(next.is_empty(), !initially_closed);
        }
        Ok(())
    }

    #[test]
    fn candidate_pages_keep_order_and_defer_unknown_store_identities() -> TestResult {
        let (_temp, options, db) = fixture()?;
        let mut candidates = Vec::new();
        let mut expected = Vec::new();
        db.with_transaction(|| {
            for ordinal in 1000..1000 + PAGE_SIZE * 2 + 1 {
                let id = MemoryId::from_uuid(uuid::Uuid::from_u128(ordinal as u128)).to_string();
                db.insert_memory(&id, &input())?;
                if ordinal % 2 == 0 {
                    db.insert_memory_seal(&id, &format!("blake3:{}", "a".repeat(64)), TIME)?;
                } else {
                    expected.push(id.clone());
                }
                candidates.push(hit(&id));
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
        let unknown = MemoryId::from_uuid(uuid::Uuid::from_u128(9000)).to_string();
        candidates.push(hit(&unknown));
        expected.push(unknown);
        candidates.reverse();
        expected.reverse();
        let actual = admit_hits(&options, candidates, &mut Vec::new(), None);
        assert_eq!(
            ids(&actual),
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn unavailable_seal_authority_withholds_memories_not_other_entity_types() -> TestResult {
        let (_temp, options, db) = fixture()?;
        db.execute_raw("ALTER TABLE memory_seals RENAME TO private_unavailable_seals")
            .map_err(|error| error.to_string())?;
        let mut degraded = Vec::new();
        let visible = admit_hits(
            &options,
            vec![hit(HIDDEN), hit("evd_other")],
            &mut degraded,
            None,
        );
        assert_eq!(ids(&visible), vec!["evd_other"]);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, UNAVAILABLE);
        assert!(!degraded[0].message.contains("private_unavailable_seals"));
        assert!(!degraded[0].message.contains(HIDDEN));
        db.execute_raw("ALTER TABLE private_unavailable_seals RENAME TO memory_seals")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            admit_hits(&options, vec![hit(HIDDEN)], &mut Vec::new(), None).len(),
            1
        );
        assert!(!options.workspace_path.join("index").exists());
        Ok(())
    }

    #[test]
    fn malformed_reveal_metadata_fails_closed_without_echoing_values() -> TestResult {
        let (_temp, options, db) = fixture()?;
        seal(&db, HIDDEN)?;
        // Preserve the schema's null-pair invariant so the malformed value
        // reaches admission, rather than failing the fixture's own write.
        db.execute_raw(
            "UPDATE memory_seals SET revealed_at = X'50524956415445', reveal_verified = 1",
        )
        .map_err(|error| error.to_string())?;
        let mut degraded = Vec::new();
        assert!(admit_hits(&options, vec![hit(HIDDEN)], &mut degraded, None).is_empty());
        assert_eq!(degraded[0].code, UNAVAILABLE);
        assert!(!degraded[0].message.contains("PRIVATE"));
        Ok(())
    }

    #[test]
    fn unrelated_candidates_need_no_memory_or_seal_schema() -> TestResult {
        let (_temp, mut options, _db) = fixture()?;
        let absent = options.workspace_path.join("absent.db");
        options.database_path = Some(absent.clone());
        let mut degraded = Vec::new();
        assert_eq!(
            ids(&admit_hits(
                &options,
                vec![hit("evd_other")],
                &mut degraded,
                None
            )),
            vec!["evd_other"]
        );
        assert!(degraded.is_empty());
        assert!(!absent.exists());
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn public_search_rechecks_seals_when_an_index_still_has_the_old_body() -> TestResult {
        let (_temp, options, db) = fixture()?;
        db.close().map_err(|error| error.to_string())?;
        crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
            workspace_path: options.workspace_path.clone(),
            database_path: options.database_path.clone(),
            index_dir: options.index_dir.clone(),
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;
        let before = crate::core::search::run_search_unaudited(&options)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            ids(&before.results).into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([HIDDEN, PUBLIC])
        );
        let writer = DbConnection::open_file(&options.resolve_database_path())
            .map_err(|error| error.to_string())?;
        seal(&writer, HIDDEN)?;
        writer.close().map_err(|error| error.to_string())?;
        let after = crate::core::search::run_search_unaudited(&options)
            .map_err(|error| error.to_string())?;
        assert_eq!(ids(&after.results), vec![PUBLIC]);
        assert!(
            after
                .degraded
                .iter()
                .any(|entry| entry.code == SEALED_FILTERED)
        );
        Ok(())
    }
}
