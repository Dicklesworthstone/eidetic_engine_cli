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
    BTreeMap, BTreeSet, DbConnection, DbError, DbOperation, EVIDENCE_SEARCH_CANDIDATE_PREDICATE,
    EvidenceAdmissionReport, EvidenceProducerKind, Result, Value,
};

const POLICY_VERSION: i64 = 1;
// V131 freezes this expression in its triggers. A future predicate change
// must install matching triggers in a forward migration; a rebuild alone
// cannot silently bless an older write-maintenance policy.
const V131_CANDIDATE_PREDICATE: &str =
    "e.producer_kind = 'cass_import' AND e.search_eligibility = 'admitted'";
const MAX_BUCKETS_PER_WORKSPACE: usize = 18;
const COUNT_REBUILD_PAGE_ROWS: u32 = 256;

// Preserve request cancellation even when the database operation reports an
// interrupted-query error. No new context or deadline is created here.
fn count_rebuild_step<T>(
    checkpoint: &mut impl FnMut() -> Result<()>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    checkpoint()?;
    let result = operation();
    checkpoint()?;
    result
}

fn invalid_count_page() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "evidence admission count page has invalid scope, ordering or fields".to_owned(),
    }
}

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
        let cx = asupersync::Cx::current();
        self.rebuild_evidence_counts_with_checkpoint(workspace_id, || {
            super::checkpoint_retry_cx(DbOperation::Query, cx.as_ref())
        })
    }

    fn rebuild_evidence_counts_with_checkpoint(
        &self,
        workspace_id: Option<&str>,
        mut checkpoint: impl FnMut() -> Result<()>,
    ) -> Result<u64> {
        if !count_rebuild_step(&mut checkpoint, || self.evidence_admission_counts_available())? {
            return Ok(0);
        }
        let params = [workspace_id.map_or(Value::Null, |id| Value::Text(id.to_owned()))];
        // The page boundary is NOT a transaction boundary. Concurrent source
        // writes cannot invalidate an earlier page, and any failed scan or
        // cancelled finalization rolls back both buckets and readiness.
        self.with_transaction(|| {
            count_rebuild_step(&mut checkpoint, || {
                self.execute_for(
                    DbOperation::Execute,
                    "DELETE FROM evidence_admission_counts WHERE (?1 IS NULL OR workspace_id = ?1)",
                    &params,
                )
            })?;
            count_rebuild_step(&mut checkpoint, || {
                self.execute_for(
                    DbOperation::Execute,
                    "DELETE FROM evidence_admission_count_state WHERE (?1 IS NULL OR workspace_id = ?1)",
                    &params,
                )
            })?;
            self.rebuild_evidence_count_pages(workspace_id, &mut checkpoint)?;
            // Group only the small derived bucket table, never evidence text
            // or the whole source corpus. Readiness is published last.
            count_rebuild_step(&mut checkpoint, || {
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
                )
            })?;
            count_rebuild_step(&mut checkpoint, || {
                self.execute_for(
                    DbOperation::Execute,
                    "INSERT OR IGNORE INTO evidence_admission_count_state
                    (workspace_id, policy_version, candidate_predicate, ready, source_rows)
                 SELECT id, 1,
                        'e.producer_kind = ''cass_import'' AND e.search_eligibility = ''admitted''', 1, 0
                 FROM workspaces WHERE (?1 IS NULL OR id = ?1)",
                    &params,
                )
            })?;
            let rows = count_rebuild_step(&mut checkpoint, || {
                self.query_for(
                    DbOperation::Query,
                    "SELECT COALESCE(SUM(source_rows), 0) FROM evidence_admission_count_state
                 WHERE (?1 IS NULL OR workspace_id = ?1)",
                    &params,
                )
            })?;
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

    /// One rowid-seeking page of narrow fields at a time. Rebuild previously
    /// made a whole-source GROUP BY a prerequisite for recovery (GH#78).
    /// No OFFSET, transcript ordering, projection hydration or unbounded Rust
    /// bucket map is needed. The same writer transaction owns every page.
    fn rebuild_evidence_count_pages(
        &self,
        workspace_id: Option<&str>,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let scope = if workspace_id.is_some() {
            " AND workspace_id = ?3"
        } else {
            ""
        };
        let sql = format!(
            "SELECT rowid, workspace_id, producer_kind, search_eligibility
             FROM evidence_spans WHERE rowid >= ?1{scope} ORDER BY rowid LIMIT ?2"
        );
        // SQLite rowids can be negative, zero, or i64::MAX. An inclusive
        // lower bound admits MIN; checked advancement cannot wrap past MAX.
        let mut next_rowid = i64::MIN;
        loop {
            let mut params = vec![
                Value::BigInt(next_rowid),
                Value::BigInt(i64::from(COUNT_REBUILD_PAGE_ROWS)),
            ];
            if let Some(workspace) = workspace_id {
                params.push(Value::Text(workspace.to_owned()));
            }
            let rows = count_rebuild_step(checkpoint, || {
                self.query_for(DbOperation::Query, &sql, &params)
            })?;
            if rows.is_empty() {
                return Ok(());
            }
            if rows.len() > COUNT_REBUILD_PAGE_ROWS as usize {
                return Err(invalid_count_page());
            }
            let mut previous = None;
            // Keys borrow this one page; even an all-workspace rebuild keeps
            // at most PAGE_ROWS partitions, not every workspace seen so far.
            let mut buckets = BTreeMap::<(&str, &str, &str), i64>::new();
            for row in &rows {
                checkpoint()?;
                let rowid = row
                    .get(0)
                    .and_then(Value::as_i64)
                    .ok_or_else(invalid_count_page)?;
                let workspace = row
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_count_page)?;
                let producer = row
                    .get(2)
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_count_page)?;
                let eligibility = row
                    .get(3)
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_count_page)?;
                if rowid < next_rowid
                    || previous.is_some_and(|last| rowid <= last)
                    || workspace_id.is_some_and(|expected| workspace != expected)
                    || EvidenceProducerKind::parse(producer).is_none()
                    || !matches!(eligibility, "admitted" | "quarantined" | "denied")
                {
                    return Err(invalid_count_page());
                }
                previous = Some(rowid);
                *buckets.entry((workspace, producer, eligibility)).or_default() += 1;
            }
            for ((workspace, producer, eligibility), count) in buckets {
                count_rebuild_step(checkpoint, || {
                    self.execute_for(
                        DbOperation::Execute,
                        "INSERT INTO evidence_admission_counts
                            (workspace_id, producer_kind, search_eligibility, is_candidate, row_count)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(workspace_id, producer_kind, search_eligibility, is_candidate)
                         DO UPDATE SET row_count = evidence_admission_counts.row_count + excluded.row_count",
                        &[
                            Value::Text(workspace.to_owned()),
                            Value::Text(producer.to_owned()),
                            Value::Text(eligibility.to_owned()),
                            Value::BigInt(i64::from(
                                producer == "cass_import" && eligibility == "admitted",
                            )),
                            Value::BigInt(count),
                        ],
                    )
                })?;
            }
            let last = previous.ok_or_else(invalid_count_page)?;
            let Some(next) = last.checked_add(1) else {
                return Ok(());
            };
            if rows.len() < COUNT_REBUILD_PAGE_ROWS as usize {
                return Ok(());
            }
            next_rowid = next;
        }
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

    fn count_page_fixture(count: u32) -> TestResult<DbConnection> {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        setup_workspace(&db, WORKSPACE, SESSION)?;
        setup_workspace(&db, OTHER_WORKSPACE, OTHER_SESSION)?;
        let producers = [
            "cass_import",
            "agentsmd_import",
            "docs_bootstrap",
            "journal_distill",
            "remember_reinforcement",
            "legacy_unknown",
        ];
        for sequence in 1..=count {
            let id = insert_span(&db, u128::from(sequence))?;
            // Interleave both workspaces and all eighteen partition keys.
            // This exercises repeated buckets split over several source pages.
            let (workspace, session) = if (sequence / 18) % 2 == 0 {
                (WORKSPACE, SESSION)
            } else {
                (OTHER_WORKSPACE, OTHER_SESSION)
            };
            db.execute_for(
                DbOperation::Execute,
                "UPDATE evidence_spans SET workspace_id = ?1, session_id = ?2,
                 producer_kind = ?3, search_eligibility = ?4 WHERE id = ?5",
                &[
                    Value::Text(workspace.to_owned()),
                    Value::Text(session.to_owned()),
                    Value::Text(producers[(sequence % 6) as usize].to_owned()),
                    Value::Text(
                        ["admitted", "quarantined", "denied"][((sequence / 6) % 3) as usize]
                            .to_owned(),
                    ),
                    Value::Text(id),
                ],
            )?;
        }
        Ok(db)
    }

    fn count_scalar(db: &DbConnection, sql: &str) -> Result<i64> {
        db.query(sql, &[])?
            .first()
            .and_then(|row| row.get(0))
            .and_then(Value::as_i64)
            .ok_or_else(invalid_count_page)
    }

    fn saved_count_state(db: &DbConnection, workspace: Option<&str>) -> TestResult<String> {
        let params = [workspace.map_or(Value::Null, |id| Value::Text(id.to_owned()))];
        let buckets = db.query(
            "SELECT * FROM evidence_admission_counts WHERE (?1 IS NULL OR workspace_id = ?1)
             ORDER BY workspace_id, producer_kind, search_eligibility, is_candidate",
            &params,
        )?;
        let state = db.query(
            "SELECT * FROM evidence_admission_count_state WHERE (?1 IS NULL OR workspace_id = ?1)
             ORDER BY workspace_id",
            &params,
        )?;
        Ok(format!("{buckets:?}\n{state:?}"))
    }

    #[test]
    fn paged_count_rebuild_matches_source_partitions_without_projecting_or_changing_evidence()
    -> TestResult {
        let count = 2 * COUNT_REBUILD_PAGE_ROWS + 17;
        let db = count_page_fixture(count)?;
        let before = db.list_evidence_spans_for_session(SESSION)?;
        let other_before = db.list_evidence_spans_for_session(OTHER_SESSION)?;
        let generation = db.get_workspace_generation(WORKSPACE)?;
        let other_generation = db.get_workspace_generation(OTHER_WORKSPACE)?;
        let projections = crate::cass::transcript::projection_calls_for_test();
        assert_eq!(db.rebuild_evidence_admission_counts(None)?, u64::from(count));
        assert_eq!(
            crate::cass::transcript::projection_calls_for_test(),
            projections
        );
        assert_eq!(db.get_workspace_generation(WORKSPACE)?, generation);
        assert_eq!(
            db.get_workspace_generation(OTHER_WORKSPACE)?,
            other_generation
        );
        assert_eq!(db.list_evidence_spans_for_session(SESSION)?, before);
        assert_eq!(db.list_evidence_spans_for_session(OTHER_SESSION)?, other_before);
        for workspace in [WORKSPACE, OTHER_WORKSPACE] {
            assert_matches_legacy(&db, workspace, 0)?;
        }
        Ok(())
    }

    #[test]
    fn paged_count_rebuild_includes_negative_zero_and_maximum_rowids() -> TestResult {
        let count = COUNT_REBUILD_PAGE_ROWS + 7;
        let db = count_page_fixture(count)?;
        for (sequence, rowid) in [(1, i64::MIN), (2, -1), (3, 0), (count, i64::MAX)] {
            let id =
                crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(u128::from(sequence)))
                    .to_string();
            db.execute_for(
                DbOperation::Execute,
                "UPDATE evidence_spans SET rowid = ?1 WHERE id = ?2",
                &[Value::BigInt(rowid), Value::Text(id)],
            )?;
        }
        assert_eq!(db.rebuild_evidence_admission_counts(None)?, u64::from(count));
        for workspace in [WORKSPACE, OTHER_WORKSPACE] {
            assert_eq!(
                db.rebuild_evidence_admission_counts(Some(workspace))?,
                u64::try_from(db.count_evidence_spans_for_workspace(workspace)?)?
            );
            assert_matches_legacy(&db, workspace, 0)?;
        }
        Ok(())
    }

    #[test]
    fn count_rebuild_rolls_back_partial_pages_and_cancelled_readiness_then_retries() -> TestResult {
        let count = COUNT_REBUILD_PAGE_ROWS + 7;
        let db = count_page_fixture(count)?;
        let generation = db.get_workspace_generation(WORKSPACE)?;
        for fail_during_readiness in [false, true] {
            db.execute_raw(
                "UPDATE evidence_admission_counts SET row_count = 0;
                 UPDATE evidence_admission_count_state SET ready = 0, source_rows = 0;",
            )?;
            let before = saved_count_state(&db, None)?;
            let mut injected = false;
            let result = db.rebuild_evidence_counts_with_checkpoint(None, || {
                let rebuilt = count_scalar(
                    &db,
                    "SELECT COALESCE(SUM(row_count), 0) FROM evidence_admission_counts",
                )?;
                let ready = count_scalar(
                    &db,
                    "SELECT COUNT(*) FROM evidence_admission_count_state WHERE ready = 1",
                )?;
                if (fail_during_readiness && ready > 0)
                    || (!fail_during_readiness && rebuilt > 0 && rebuilt < i64::from(count))
                {
                    injected = true;
                    return Err(DbError::sqlmodel(
                        DbOperation::Query,
                        sqlmodel_core::Error::Cancelled,
                    ));
                }
                Ok(())
            });
            assert!(injected, "the failure must occur after real derived writes");
            assert!(result.is_err());
            assert_eq!(saved_count_state(&db, None)?, before);
            assert_eq!(db.get_workspace_generation(WORKSPACE)?, generation);
            assert_eq!(db.count_table_rows("evidence_spans")?, i64::from(count));
            assert_eq!(db.rebuild_evidence_admission_counts(None)?, u64::from(count));
            assert_matches_legacy(&db, WORKSPACE, 0)?;
            assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;
        }
        Ok(())
    }

    #[test]
    fn scoped_paged_repair_keeps_another_workspaces_unready_counts_untouched() -> TestResult {
        let db = count_page_fixture(2 * COUNT_REBUILD_PAGE_ROWS + 17)?;
        db.execute_for(
            DbOperation::Execute,
            "UPDATE evidence_admission_count_state SET ready = 0 WHERE workspace_id = ?1",
            &[Value::Text(OTHER_WORKSPACE.to_owned())],
        )?;
        let other = saved_count_state(&db, Some(OTHER_WORKSPACE))?;
        let expected = u64::try_from(db.count_evidence_spans_for_workspace(WORKSPACE)?)?;
        assert!(expected > u64::from(COUNT_REBUILD_PAGE_ROWS));
        assert_eq!(
            db.rebuild_evidence_admission_counts(Some(WORKSPACE))?,
            expected
        );
        assert_eq!(saved_count_state(&db, Some(OTHER_WORKSPACE))?, other);
        assert_matches_legacy(&db, WORKSPACE, 0)?;
        assert!(
            db.evidence_admission_report_for_indexed_count(OTHER_WORKSPACE, 0)?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn count_rebuild_checkpoint_prevents_sql_and_preserves_cancellation_over_query_error()
    -> TestResult {
        let cancelled = || DbError::sqlmodel(DbOperation::Query, sqlmodel_core::Error::Cancelled);
        let early = count_rebuild_step(&mut || Err(cancelled()), || -> Result<()> {
            panic!("a cancelled count rebuild must not start a database operation")
        });
        assert!(early.is_err());
        let mut calls = 0;
        let result: Result<()> = count_rebuild_step(
            &mut || {
                calls += 1;
                if calls == 2 {
                    Err(cancelled())
                } else {
                    Ok(())
                }
            },
            || Err(invalid_count_page()),
        );
        let Err(error) = result else {
            return Err("cancelled operation unexpectedly succeeded".into());
        };
        assert_eq!(calls, 2);
        assert_eq!(error.to_string(), cancelled().to_string());
        Ok(())
    }

    #[test]
    fn count_rebuild_preserves_empty_and_missing_scope_semantics() -> TestResult {
        let db = count_page_fixture(0)?;
        assert_eq!(db.rebuild_evidence_admission_counts(None)?, 0);
        assert_matches_legacy(&db, WORKSPACE, 0)?;
        assert_matches_legacy(&db, OTHER_WORKSPACE, 0)?;
        let before = saved_count_state(&db, None)?;
        assert_eq!(
            db.rebuild_evidence_admission_counts(Some("wsp_missing"))?,
            0
        );
        assert_eq!(saved_count_state(&db, None)?, before);
        assert!(
            db.evidence_admission_report_for_indexed_count("wsp_missing", 0)?
                .is_none()
        );
        Ok(())
    }
}
