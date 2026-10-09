//! Bounded admission accounting for a generation-bound index snapshot.
//!
//! These counts partition cheap stored columns, not full security verdicts.
//! Accepted candidate counts still come from the current index generation.
//! The source-write triggers and backfill transaction establish consistency;
//! readiness and totals detect ordinary missing, stale and partial derived
//! state. They do not authenticate against an arbitrary database writer.
//! Insert conflicts conservatively invalidate affected workspaces because
//! SQLite REPLACE can bypass victim-delete triggers. Ordinary append inserts
//! stay current; successful IGNORE/UPSERT/REPLACE requires explicit rebuild.

use super::{
    BTreeSet, DbConnection, DbError, DbOperation, EVIDENCE_SEARCH_CANDIDATE_PREDICATE,
    EvidenceAdmissionReport, EvidenceProducerKind, Result, Value,
};

const POLICY_VERSION: i64 = 1;
// V131 freezes this expression in its triggers. A future predicate change
// must install matching triggers in a forward migration; a rebuild alone
// cannot silently bless an older write-maintenance policy.
const V131_CANDIDATE_PREDICATE: &str =
    "e.producer_kind = 'cass_import' AND e.search_eligibility = 'admitted'";
const MAX_BUCKETS_PER_WORKSPACE: usize = 18;

impl DbConnection {
    fn evidence_admission_counts_available(&self) -> Result<bool> {
        // Do not latch this probe: a removed readiness table or trigger is a
        // cache miss even on a connection that previously used the counts.
        let rows = self.query_for(
            DbOperation::Query,
            "SELECT name FROM sqlite_master WHERE
                (type = 'table' AND name IN ('evidence_admission_counts', 'evidence_admission_count_state'))
                OR (type = 'trigger' AND name IN (
                    'trg_evidence_admission_counts_workspace_insert',
                    'trg_evidence_admission_counts_insert_conflict',
                    'trg_evidence_admission_counts_span_insert',
                    'trg_evidence_admission_counts_span_delete',
                    'trg_evidence_admission_counts_span_update'
                ))",
            &[],
        )?;
        Ok(rows.len() == 7 && EVIDENCE_SEARCH_CANDIDATE_PREDICATE == V131_CANDIDATE_PREDICATE)
    }

    pub(super) fn materialized_evidence_admission_report(
        &self,
        workspace_id: &str,
        indexed_admitted: u32,
    ) -> Result<Option<EvidenceAdmissionReport>> {
        if !self.evidence_admission_counts_available()? {
            return Ok(None);
        }
        let params = [Value::Text(workspace_id.to_owned())];
        let state = self.query_for(
            DbOperation::Query,
            "SELECT policy_version, candidate_predicate, ready, source_rows
             FROM evidence_admission_count_state WHERE workspace_id = ?1",
            &params,
        )?;
        let Some(state) = state.first() else {
            return Ok(None);
        };
        if state.get(0).and_then(Value::as_i64) != Some(POLICY_VERSION)
            || state.get(1).and_then(Value::as_str) != Some(EVIDENCE_SEARCH_CANDIDATE_PREDICATE)
            || state.get(2).and_then(Value::as_i64) != Some(1)
        {
            return Ok(None);
        }
        let Some(source_rows) = state
            .get(3)
            .and_then(Value::as_i64)
            .and_then(|count| u64::try_from(count).ok())
        else {
            return Ok(None);
        };
        // The extra row detects a violated vocabulary/cardinality contract
        // without letting damaged derived state turn this into a corpus read.
        let buckets = self.query_for(
            DbOperation::Query,
            "SELECT producer_kind, search_eligibility, is_candidate, row_count
             FROM evidence_admission_counts WHERE workspace_id = ?1
             ORDER BY producer_kind, search_eligibility, is_candidate LIMIT 19",
            &params,
        )?;
        if buckets.len() > MAX_BUCKETS_PER_WORKSPACE {
            return Ok(None);
        }
        let mut report = EvidenceAdmissionReport::default();
        let mut observed_rows = 0_u64;
        let mut candidates = 0_u32;
        let mut seen = BTreeSet::new();
        for bucket in &buckets {
            let Some(producer) = bucket.get(0).and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(eligibility) = bucket.get(1).and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(candidate) = bucket.get(2).and_then(Value::as_i64) else {
                return Ok(None);
            };
            let Some(count) = bucket
                .get(3)
                .and_then(Value::as_i64)
                .and_then(|count| u32::try_from(count).ok())
            else {
                return Ok(None);
            };
            let expected_candidate =
                producer == EvidenceProducerKind::CassImport.as_str() && eligibility == "admitted";
            if EvidenceProducerKind::parse(producer).is_none()
                || !matches!(eligibility, "admitted" | "quarantined" | "denied")
                || candidate != i64::from(expected_candidate)
                || !seen.insert((producer, eligibility))
            {
                return Ok(None);
            }
            let Some(total) = observed_rows.checked_add(u64::from(count)) else {
                return Ok(None);
            };
            observed_rows = total;
            if expected_candidate {
                let Some(total) = candidates.checked_add(count) else {
                    return Ok(None);
                };
                candidates = total;
            } else {
                report.record_many(producer, eligibility, false, count);
            }
        }
        if observed_rows != source_rows || indexed_admitted > candidates {
            return Ok(None);
        }
        report.record_many(
            EvidenceProducerKind::CassImport.as_str(),
            "admitted",
            true,
            indexed_admitted,
        );
        report.record_many(
            EvidenceProducerKind::CassImport.as_str(),
            "admitted",
            false,
            candidates - indexed_admitted,
        );
        Ok(Some(report))
    }

    /// Rebuild derived candidate-count partitions under one source-write
    /// transaction. Only migration, explicit index repair, or restore should
    /// call this; normal reads never repair materialization. The return value
    /// counts the source rows represented by the rebuilt workspace set.
    ///
    /// Call outside any caller-held transaction. Source generations, evidence
    /// content, security verdicts and provenance remain unchanged.
    pub fn rebuild_evidence_admission_counts(&self, workspace_id: Option<&str>) -> Result<u64> {
        if !self.evidence_admission_counts_available()? {
            return Ok(0);
        }
        let params = [workspace_id.map_or(Value::Null, |id| Value::Text(id.to_owned()))];
        self.with_transaction(|| {
            self.execute_for(
                DbOperation::Execute,
                "DELETE FROM evidence_admission_counts WHERE (?1 IS NULL OR workspace_id = ?1)",
                &params,
            )?;
            self.execute_for(
                DbOperation::Execute,
                "DELETE FROM evidence_admission_count_state WHERE (?1 IS NULL OR workspace_id = ?1)",
                &params,
            )?;
            self.execute_for(
                DbOperation::Execute,
                "INSERT INTO evidence_admission_counts
                    (workspace_id, producer_kind, search_eligibility, is_candidate, row_count)
                 SELECT workspace_id, producer_kind, search_eligibility,
                        producer_kind = 'cass_import' AND search_eligibility = 'admitted', COUNT(*)
                 FROM evidence_spans WHERE (?1 IS NULL OR workspace_id = ?1)
                 GROUP BY workspace_id, producer_kind, search_eligibility",
                &params,
            )?;
            self.execute_for(
                DbOperation::Execute,
                "INSERT INTO evidence_admission_count_state
                    (workspace_id, policy_version, candidate_predicate, ready, source_rows)
                 SELECT workspace_id, 1,
                        'e.producer_kind = ''cass_import'' AND e.search_eligibility = ''admitted''',
                        1, SUM(row_count)
                 FROM evidence_admission_counts WHERE (?1 IS NULL OR workspace_id = ?1)
                 GROUP BY workspace_id",
                &params,
            )?;
            self.execute_for(
                DbOperation::Execute,
                "INSERT OR IGNORE INTO evidence_admission_count_state
                    (workspace_id, policy_version, candidate_predicate, ready, source_rows)
                 SELECT id, 1,
                        'e.producer_kind = ''cass_import'' AND e.search_eligibility = ''admitted''', 1, 0
                 FROM workspaces WHERE (?1 IS NULL OR id = ?1)",
                &params,
            )?;
            let rows = self.query_for(
                DbOperation::Query,
                "SELECT COALESCE(SUM(source_rows), 0) FROM evidence_admission_count_state
                 WHERE (?1 IS NULL OR workspace_id = ?1)",
                &params,
            )?;
            rows.first()
                .and_then(|row| row.get(0))
                .and_then(Value::as_i64)
                .and_then(|count| u64::try_from(count).ok())
                .ok_or_else(|| DbError::MalformedRow {
                    operation: DbOperation::Query,
                    message: "rebuilt evidence admission total must fit a nonnegative integer"
                        .to_owned(),
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, MIGRATIONS,
        V131_EVIDENCE_ADMISSION_COUNTS, canonical_evidence_hash,
    };

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
    const WORKSPACE: &str = "wsp_01234567890123456789012345";
    const OTHER_WORKSPACE: &str = "wsp_11234567890123456789012345";
    const SESSION: &str = "sess_01234567890123456789012345";
    const OTHER_SESSION: &str = "sess_11234567890123456789012345";

    fn setup_workspace(db: &DbConnection, workspace: &str, session: &str) -> TestResult {
        db.insert_workspace(
            workspace,
            &CreateWorkspaceInput {
                path: format!("/tmp/evidence-counts/{workspace}"),
                name: None,
            },
        )?;
        db.insert_session(
            session,
            &CreateSessionInput {
                workspace_id: workspace.to_owned(),
                cass_session_id: session.to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: canonical_evidence_hash(session),
                metadata_json: None,
            },
        )?;
        Ok(())
    }

    fn insert_span(db: &DbConnection, sequence: u128) -> TestResult<String> {
        let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(sequence)).to_string();
        let excerpt = "The copper kestrel release preserves complete source records.";
        db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WORKSPACE.to_owned(),
                session_id: SESSION.to_owned(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("counter-span-{sequence}"),
                span_kind: "message".to_owned(),
                start_line: u32::try_from(sequence)?,
                end_line: u32::try_from(sequence)?,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                excerpt: excerpt.to_owned(),
                content_hash: canonical_evidence_hash(excerpt),
                metadata_json: Some(
                    r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.to_owned(),
                ),
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        Ok(id)
    }

    fn legacy_report(
        db: &DbConnection,
        workspace: &str,
        indexed_admitted: u32,
    ) -> TestResult<EvidenceAdmissionReport> {
        let mut report = EvidenceAdmissionReport::default();
        for (producer, eligibility, count) in db.count_non_candidate_evidence(workspace, None)? {
            report.record_many(&producer, &eligibility, false, count);
        }
        let sql = format!(
            "SELECT COUNT(*) FROM evidence_spans e WHERE e.workspace_id = ?1 AND {EVIDENCE_SEARCH_CANDIDATE_PREDICATE}"
        );
        let rows = db.query(&sql, &[Value::Text(workspace.to_owned())])?;
        let candidates = rows
            .first()
            .and_then(|row| row.get(0))
            .and_then(Value::as_i64)
            .ok_or("legacy candidate count missing")?;
        let candidates = u32::try_from(candidates)?;
        report.record_many("cass_import", "admitted", true, indexed_admitted);
        report.record_many(
            "cass_import",
            "admitted",
            false,
            candidates - indexed_admitted,
        );
        Ok(report)
    }

    fn assert_matches_legacy(db: &DbConnection, workspace: &str, admitted: u32) -> TestResult {
        assert_eq!(
            db.evidence_admission_report_for_indexed_count(workspace, admitted)?,
            Some(legacy_report(db, workspace, admitted)?),
            "current materialization must match the old grouped source counts"
        );
        Ok(())
    }

    fn replace_span(
        db: &DbConnection,
        template_id: &str,
        replacement_id: &str,
        workspace: &str,
        session: &str,
        cass_span: &str,
    ) -> TestResult {
        // Preserve only fields required by source storage; readiness counts
        // candidates, while the published generation decides actual admission.
        let template = db
            .get_evidence_span(template_id)?
            .ok_or("replacement template missing")?;
        db.execute_for(
            DbOperation::Execute,
            "INSERT OR REPLACE INTO evidence_spans
                (id, workspace_id, session_id, cass_span_id, span_kind, start_line,
                 end_line, role, excerpt, content_hash, producer_kind,
                 search_eligibility, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     'cass_import', 'admitted', ?11, ?12)",
            &[
                Value::Text(replacement_id.to_owned()),
                Value::Text(workspace.to_owned()),
                Value::Text(session.to_owned()),
                Value::Text(cass_span.to_owned()),
                Value::Text(template.span_kind),
                Value::BigInt(i64::from(template.start_line)),
                Value::BigInt(i64::from(template.end_line)),
                template.role.map_or(Value::Null, Value::Text),
                Value::Text(template.excerpt),
                Value::Text(template.content_hash),
                Value::Text(template.created_at),
                Value::Text(template.updated_at),
            ],
        )?;
        assert!(db.get_evidence_span(replacement_id)?.is_some());
        Ok(())
    }

    #[test]
    fn evidence_admission_counts_cover_every_partition_and_reclassification() -> TestResult {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        setup_workspace(&db, WORKSPACE, SESSION)?;
        setup_workspace(&db, OTHER_WORKSPACE, OTHER_SESSION)?;
        assert_matches_legacy(&db, WORKSPACE, 0)?;
        let mut sequence = 1_u128;
        let mut ids = Vec::new();
        for producer in [
            "cass_import",
            "agentsmd_import",
            "docs_bootstrap",
            "journal_distill",
            "remember_reinforcement",
            "legacy_unknown",
        ] {
            for eligibility in ["admitted", "quarantined", "denied"] {
                let id = insert_span(&db, sequence)?;
                sequence += 1;
                db.execute_for(
                    DbOperation::Execute,
                    "UPDATE evidence_spans SET producer_kind = ?1, search_eligibility = ?2 WHERE id = ?3",
                    &[Value::Text(producer.to_owned()), Value::Text(eligibility.to_owned()), Value::Text(id.clone())],
                )?;
                ids.push(id);
                assert_matches_legacy(&db, WORKSPACE, 1)?;
                assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;
            }
        }
        let buckets = db.query(
            "SELECT row_count FROM evidence_admission_counts WHERE workspace_id = ?1",
            &[Value::Text(WORKSPACE.to_owned())],
        )?;
        assert_eq!(buckets.len(), MAX_BUCKETS_PER_WORKSPACE);
        assert!(
            buckets
                .iter()
                .all(|row| row.get(0).and_then(Value::as_i64) == Some(1))
        );

        db.execute_for(
            DbOperation::Execute,
            "UPDATE evidence_spans SET workspace_id = ?1, session_id = ?2 WHERE id = ?3",
            &[
                Value::Text(OTHER_WORKSPACE.to_owned()),
                Value::Text(OTHER_SESSION.to_owned()),
                Value::Text(ids[0].clone()),
            ],
        )?;
        assert_matches_legacy(&db, WORKSPACE, 0)?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 1)?;
        db.execute_for(
            DbOperation::Execute,
            "DELETE FROM evidence_spans WHERE id = ?1",
            &[Value::Text(ids[0].clone())],
        )?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;
        assert!(
            db.evidence_admission_report_for_indexed_count(OTHER_WORKSPACE, 1)?
                .is_none()
        );

        // A source edit that cannot affect this cheap partition leaves it
        // alone; generation-bound metadata still handles actual admission.
        db.execute_for(
            DbOperation::Execute,
            "UPDATE evidence_spans SET excerpt = 'Different screened source body' WHERE id = ?1",
            &[Value::Text(ids[1].clone())],
        )?;
        assert_matches_legacy(&db, WORKSPACE, 0)?;
        Ok(())
    }

    #[test]
    fn evidence_admission_counts_rollback_and_reopen_preserve_the_same_partition() -> TestResult {
        let root = tempfile::tempdir()?;
        let path = root.path().join("counts.db");
        let db = DbConnection::open_file(&path)?;
        db.migrate()?;
        setup_workspace(&db, WORKSPACE, SESSION)?;
        setup_workspace(&db, OTHER_WORKSPACE, OTHER_SESSION)?;
        let id = insert_span(&db, 40)?;
        let generation = db.get_workspace_generation(WORKSPACE)?;
        let result: Result<()> = db.with_transaction(|| {
            db.execute_for(
                DbOperation::Execute,
                "UPDATE evidence_spans SET workspace_id = ?1, session_id = ?2, search_eligibility = 'quarantined' WHERE id = ?3",
                &[Value::Text(OTHER_WORKSPACE.to_owned()), Value::Text(OTHER_SESSION.to_owned()), Value::Text(id.clone())],
            )?;
            assert_eq!(db.evidence_admission_report_for_indexed_count(WORKSPACE, 0)?, Some(EvidenceAdmissionReport::default()));
            db.execute_for(DbOperation::Execute, "DELETE FROM evidence_spans WHERE id = ?1", &[Value::Text(id.clone())])?;
            Err(DbError::MalformedRow { operation: DbOperation::Execute, message: "deliberate source rollback".to_owned() })
        });
        assert!(result.is_err());
        assert_matches_legacy(&db, WORKSPACE, 1)?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;
        assert_eq!(db.get_workspace_generation(WORKSPACE)?, generation);
        assert_eq!(db.rebuild_evidence_admission_counts(None)?, 1);
        assert_eq!(db.get_workspace_generation(WORKSPACE)?, generation);
        db.close()?;

        let reopened = DbConnection::open_file_read_only(&path)?;
        let before = crate::cass::transcript::projection_calls_for_test();
        assert_matches_legacy(&reopened, WORKSPACE, 1)?;
        assert_matches_legacy(&reopened, OTHER_WORKSPACE, 0)?;
        assert_eq!(crate::cass::transcript::projection_calls_for_test(), before);
        assert!(reopened.rebuild_evidence_admission_counts(None).is_err());
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn evidence_admission_counts_missing_stale_or_damaged_state_requires_explicit_rebuild()
    -> TestResult {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        setup_workspace(&db, WORKSPACE, SESSION)?;
        let first = insert_span(&db, 50)?;
        assert_matches_legacy(&db, WORKSPACE, 1)?;
        for mutation in [
            "UPDATE evidence_admission_count_state SET policy_version = 0",
            "UPDATE evidence_admission_count_state SET candidate_predicate = 'unknown future predicate'",
            "UPDATE evidence_admission_count_state SET ready = 0",
            "UPDATE evidence_admission_counts SET row_count = 0",
            "DELETE FROM evidence_admission_counts",
            "DELETE FROM evidence_admission_count_state",
        ] {
            db.execute_raw(mutation)?;
            assert!(
                db.evidence_admission_report_for_indexed_count(WORKSPACE, 0)?
                    .is_none(),
                "{mutation}"
            );
            assert_eq!(db.rebuild_evidence_admission_counts(Some(WORKSPACE))?, 1);
            assert_matches_legacy(&db, WORKSPACE, 1)?;
        }

        // A missing old bucket must neither abort source deletion nor become
        // a ready empty workspace after the delete has completed.
        db.execute_raw("DELETE FROM evidence_admission_counts")?;
        db.execute_for(
            DbOperation::Execute,
            "DELETE FROM evidence_spans WHERE id = ?1",
            &[Value::Text(first)],
        )?;
        assert_eq!(db.count_evidence_spans_for_workspace(WORKSPACE)?, 0);
        assert!(
            db.evidence_admission_report_for_indexed_count(WORKSPACE, 0)?
                .is_none()
        );
        assert_eq!(db.rebuild_evidence_admission_counts(Some(WORKSPACE))?, 0);
        assert_matches_legacy(&db, WORKSPACE, 0)?;

        // Missing readiness cannot be reconstructed from just the next new
        // row: an arbitrary older corpus may still exist in this workspace.
        db.execute_raw("DELETE FROM evidence_admission_count_state")?;
        let _ = insert_span(&db, 51)?;
        assert!(
            db.evidence_admission_report_for_indexed_count(WORKSPACE, 1)?
                .is_none()
        );
        assert_eq!(db.rebuild_evidence_admission_counts(None)?, 1);
        assert_matches_legacy(&db, WORKSPACE, 1)?;

        // Overflow invalidates derived readiness, while the authoritative
        // source insert still commits successfully.
        db.execute_raw("UPDATE evidence_admission_counts SET row_count = 9223372036854775807; UPDATE evidence_admission_count_state SET source_rows = 9223372036854775807;")?;
        let _ = insert_span(&db, 52)?;
        assert_eq!(db.count_evidence_spans_for_workspace(WORKSPACE)?, 2);
        assert!(
            db.evidence_admission_report_for_indexed_count(WORKSPACE, 2)?
                .is_none()
        );
        assert_eq!(db.rebuild_evidence_admission_counts(None)?, 2);
        assert_matches_legacy(&db, WORKSPACE, 2)?;
        Ok(())
    }

    #[test]
    fn evidence_admission_counts_insert_conflicts_invalidate_every_victim_workspace() -> TestResult
    {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        setup_workspace(&db, WORKSPACE, SESSION)?;
        setup_workspace(&db, OTHER_WORKSPACE, OTHER_SESSION)?;
        let first = insert_span(&db, 70)?;
        let second = insert_span(&db, 71)?;
        assert_matches_legacy(&db, WORKSPACE, 2)?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;

        // A duplicate plain INSERT aborts the entire statement, including
        // provisional invalidation by the BEFORE trigger.
        assert!(insert_span(&db, 70).is_err());
        assert_matches_legacy(&db, WORKSPACE, 2)?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;

        // A primary-key replacement can move the victim into a different
        // workspace without invoking its DELETE trigger. Both scopes miss.
        let shared_cass_span = canonical_evidence_hash("replacement-secondary-key");
        replace_span(
            &db,
            &second,
            &first,
            OTHER_WORKSPACE,
            OTHER_SESSION,
            &shared_cass_span,
        )?;
        for workspace in [WORKSPACE, OTHER_WORKSPACE] {
            assert!(
                db.evidence_admission_report_for_indexed_count(workspace, 0)?
                    .is_none()
            );
            assert_eq!(db.count_evidence_spans_for_workspace(workspace)?, 1);
        }
        assert_eq!(db.rebuild_evidence_admission_counts(None)?, 2);
        assert_matches_legacy(&db, WORKSPACE, 0)?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;

        // The secondary unique key is (session_id, cass_span_id). A fresh id
        // can still replace its victim; the unaffected workspace stays ready.
        let third = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(72)).to_string();
        replace_span(
            &db,
            &first,
            &third,
            OTHER_WORKSPACE,
            OTHER_SESSION,
            &shared_cass_span,
        )?;
        assert!(db.get_evidence_span(&first)?.is_none());
        assert!(
            db.evidence_admission_report_for_indexed_count(OTHER_WORKSPACE, 0)?
                .is_none()
        );
        assert_matches_legacy(&db, WORKSPACE, 0)?;
        assert_eq!(
            db.rebuild_evidence_admission_counts(Some(OTHER_WORKSPACE))?,
            1
        );
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;

        // A single REPLACE may conflict with two different rows via those
        // two keys. Invalidate both victim workspaces, not just NEW's scope.
        replace_span(
            &db,
            &third,
            &second,
            OTHER_WORKSPACE,
            OTHER_SESSION,
            &shared_cass_span,
        )?;
        assert!(db.get_evidence_span(&third)?.is_none());
        assert_eq!(db.count_evidence_spans_for_workspace(WORKSPACE)?, 0);
        assert_eq!(db.count_evidence_spans_for_workspace(OTHER_WORKSPACE)?, 1);
        assert!(
            db.evidence_admission_report_for_indexed_count(WORKSPACE, 0)?
                .is_none()
        );
        assert!(
            db.evidence_admission_report_for_indexed_count(OTHER_WORKSPACE, 0)?
                .is_none()
        );
        assert_eq!(db.rebuild_evidence_admission_counts(None)?, 1);
        assert_matches_legacy(&db, WORKSPACE, 0)?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;

        // SQL UPSERT is allowed, but conservative invalidation must not be
        // mistaken for incrementally maintained replacement accounting.
        db.execute_for(
            DbOperation::Execute,
            "INSERT INTO evidence_spans SELECT * FROM evidence_spans WHERE id = ?1
             ON CONFLICT(id) DO UPDATE SET excerpt = excluded.excerpt",
            &[Value::Text(second)],
        )?;
        assert_eq!(db.count_evidence_spans_for_workspace(OTHER_WORKSPACE)?, 1);
        assert!(
            db.evidence_admission_report_for_indexed_count(OTHER_WORKSPACE, 0)?
                .is_none()
        );
        assert_eq!(
            db.rebuild_evidence_admission_counts(Some(OTHER_WORKSPACE))?,
            1
        );
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;
        Ok(())
    }

    #[test]
    fn evidence_admission_counts_migration_materializes_existing_and_empty_workspaces() -> TestResult
    {
        let db = DbConnection::open_memory()?;
        db.ensure_migration_table()?;
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version() < 131)
        {
            db.apply_migration(migration, "2026-10-08T23:00:00Z")?;
        }
        setup_workspace(&db, WORKSPACE, SESSION)?;
        setup_workspace(&db, OTHER_WORKSPACE, OTHER_SESSION)?;
        let _ = insert_span(&db, 60)?;
        assert!(
            db.evidence_admission_report_for_indexed_count(WORKSPACE, 1)?
                .is_none()
        );
        assert!(
            db.evidence_admission_report_for_indexed_count(OTHER_WORKSPACE, 0)?
                .is_none()
        );
        let generation = db.get_workspace_generation(WORKSPACE)?;
        db.apply_migration(&V131_EVIDENCE_ADMISSION_COUNTS, "2026-10-08T23:01:00Z")?;
        assert_matches_legacy(&db, WORKSPACE, 1)?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;
        assert_eq!(db.get_workspace_generation(WORKSPACE)?, generation);
        assert!(
            db.evidence_admission_report_for_indexed_count("wsp_missing", 0)?
                .is_none()
        );
        assert_eq!(
            EVIDENCE_SEARCH_CANDIDATE_PREDICATE,
            V131_CANDIDATE_PREDICATE
        );
        Ok(())
    }
}
