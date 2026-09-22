//! Source-backed revision admission before ranking and duplicate suppression.
//!
//! Historical versions remain indexed for `--as-of`. V123 made supersession
//! independent of author expiry, so inclusion flags cannot revive an obsolete
//! revision. Closed seals withhold candidates at every reference time, even
//! when retained index generations still contain their old body. Indexed
//! metadata is not authority for either decision.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use super::{DbConnection, SearchDegradation, SearchHit, SearchOptions};
use crate::db::{DbError, DbOperation};
use crate::models::MemoryId;

const PAGE_SIZE: usize = 256;
const FILTERED: &str = "superseded_revision_filtered";
const SEALED_FILTERED: &str = "sealed_memory_filtered";
pub(in crate::core::search) const UNAVAILABLE: &str = "revision_visibility_unavailable";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RevisionState {
    Current,
    Superseded(DateTime<Utc>),
    Sealed,
    Malformed,
}

impl RevisionState {
    fn visible_at(self, reference: DateTime<Utc>) -> bool {
        match self {
            Self::Current => true,
            Self::Superseded(at) => reference < at,
            Self::Sealed | Self::Malformed => false,
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
        message: "Memory candidates whose revision or seal authority could not be verified were withheld; unrelated entity types remain available.".to_owned(),
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
    hits
}

/// Source truth for a similarity seed; missing or closed evidence cannot
/// drive either lexical query construction or semantic embedding. The caller
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

    pub(in crate::core::search) fn finish(mut self) -> Result<(), DbError> {
        self.connection.rollback_read_snapshot()?;
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
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
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
