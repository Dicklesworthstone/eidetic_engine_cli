//! Read durable publication posture without hydrating completed job history.
//!
//! The caller pins the source snapshot used for both jobs and index freshness.
//! Historical diagnostics, document identities and timestamps cannot establish
//! readiness, so this projection reads only required identities and two flags.

use super::{
    BTreeSet, DbConnection, DbError, DbOperation, Result, Value, required_bool, required_text,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchIndexJobPosture {
    Complete,
    Incomplete,
    Failed,
}

impl DbConnection {
    /// Verify required job membership and the entire workspace's completion
    /// posture within the caller's existing read snapshot.
    ///
    /// Missing required jobs take precedence over failed peers. Once every
    /// required job is present, any failed peer takes precedence over other
    /// incomplete work. Unknown or null legacy statuses are incomplete.
    /// This method neither starts nor releases the caller's transaction.
    pub(crate) fn search_index_job_posture_in_current_snapshot(
        &self,
        workspace_id: &str,
        required_job_ids: &[&str],
    ) -> Result<SearchIndexJobPosture> {
        let required = required_job_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for page in required.chunks(256) {
            let slots = (2..page.len() + 2)
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT id FROM search_index_jobs WHERE workspace_id = ?1 AND id IN ({slots})"
            );
            let mut values = vec![Value::Text(workspace_id.to_owned())];
            values.extend(page.iter().map(|id| Value::Text((*id).to_owned())));
            let rows = self.query_for(DbOperation::Query, &sql, &values)?;
            let found = rows
                .iter()
                .map(|row| required_text(row, 0, DbOperation::Query, "id"))
                .collect::<Result<BTreeSet<_>>>()?;
            if found.len() != page.len() || page.iter().any(|id| !found.contains(*id)) {
                return Ok(SearchIndexJobPosture::Incomplete);
            }
        }

        let rows = self.query_for(
            DbOperation::Query,
            "SELECT EXISTS(SELECT 1 FROM search_index_jobs WHERE workspace_id = ?1 AND status = 'failed') AS has_failed_job, EXISTS(SELECT 1 FROM search_index_jobs WHERE workspace_id = ?1 AND (status IS NULL OR status != 'completed')) AS has_incomplete_job",
            &[Value::Text(workspace_id.to_owned())],
        )?;
        let row = rows.first().ok_or_else(|| DbError::MalformedRow {
            operation: DbOperation::Query,
            message: "Missing search index job completion posture".to_owned(),
        })?;
        let failed = required_bool(row, 0, DbOperation::Query, "has_failed_job")?;
        let incomplete = required_bool(row, 1, DbOperation::Query, "has_incomplete_job")?;
        Ok(if failed {
            SearchIndexJobPosture::Failed
        } else if incomplete {
            SearchIndexJobPosture::Incomplete
        } else {
            SearchIndexJobPosture::Complete
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn schema(db: &DbConnection) -> Result<()> {
        // Permit unknown/null statuses to exercise damaged or legacy storage.
        // The production table's other columns must never be needed here.
        db.execute_raw(
            "CREATE TABLE search_index_jobs (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, status TEXT, error_message TEXT)",
        )
    }

    fn insert(db: &DbConnection, id: &str, workspace: &str, status: &str) -> Result<()> {
        db.execute(
            "INSERT INTO search_index_jobs (id, workspace_id, status, error_message) VALUES (?1, ?2, ?3, ?4)",
            &[
                Value::Text(id.to_owned()),
                Value::Text(workspace.to_owned()),
                Value::Text(status.to_owned()),
                Value::Text("Unused historical diagnostic. ".repeat(512)),
            ],
        )?;
        Ok(())
    }

    #[test]
    fn completion_posture_preserves_membership_status_and_failure_priority() -> TestResult {
        let db = DbConnection::open_memory()?;
        schema(&db)?;
        insert(&db, "own", "local", "completed")?;
        insert(&db, "peer", "local", "completed")?;
        insert(&db, "foreign", "other", "failed")?;
        let posture =
            |required: &[&str]| db.search_index_job_posture_in_current_snapshot("local", required);
        assert_eq!(posture(&["own", "own"])?, SearchIndexJobPosture::Complete);
        assert_eq!(posture(&[])?, SearchIndexJobPosture::Complete);
        for status in ["pending", "running", "cancelled", "future_status"] {
            db.execute(
                "UPDATE search_index_jobs SET status = ?1 WHERE id = 'peer'",
                &[Value::Text(status.to_owned())],
            )?;
            assert_eq!(posture(&["own"])?, SearchIndexJobPosture::Incomplete);
        }
        db.execute_raw("UPDATE search_index_jobs SET status = NULL WHERE id = 'peer'")?;
        assert_eq!(posture(&["own"])?, SearchIndexJobPosture::Incomplete);
        db.execute_raw("UPDATE search_index_jobs SET status = 'failed' WHERE id = 'peer'")?;
        assert_eq!(posture(&["own"])?, SearchIndexJobPosture::Failed);
        insert(&db, "pending", "local", "pending")?;
        assert_eq!(posture(&["own"])?, SearchIndexJobPosture::Failed);
        assert_eq!(posture(&["missing"])?, SearchIndexJobPosture::Incomplete);
        assert_eq!(posture(&["foreign"])?, SearchIndexJobPosture::Incomplete);
        assert_eq!(
            posture(&["own", "foreign"])?,
            SearchIndexJobPosture::Incomplete
        );
        assert_eq!(
            db.search_index_job_posture_in_current_snapshot("' OR 1 = 1 --", &["own"])?,
            SearchIndexJobPosture::Incomplete
        );
        Ok(())
    }

    #[test]
    fn completion_posture_batches_membership_without_reading_or_sorting_history() -> TestResult {
        let db = DbConnection::open_memory()?;
        schema(&db)?;
        let ids = (0..257)
            .map(|ordinal| format!("sidx_{ordinal:026}"))
            .collect::<Vec<_>>();
        db.with_transaction(|| {
            for id in &ids {
                insert(&db, id, "local", "completed")?;
            }
            Ok(())
        })?;
        let required = ids.iter().map(String::as_str).collect::<Vec<_>>();
        let before = db.logical_table_digests()?;
        let (posture, queries) = super::super::capture_query_sql_for_test(|| {
            db.search_index_job_posture_in_current_snapshot("local", &required)
        });
        assert_eq!(posture?, SearchIndexJobPosture::Complete);
        assert_eq!(
            queries.len(),
            3,
            "two bounded ID pages and one posture query"
        );
        assert!(queries[0].starts_with("SELECT id FROM search_index_jobs"));
        assert!(queries[0].contains("?257"));
        assert!(!queries[0].contains("?258"));
        assert!(queries[1].contains("id IN (?2)"));
        assert!(queries[2].starts_with("SELECT EXISTS("));
        assert!(queries.iter().all(|sql| {
            !sql.contains("error_message")
                && !sql.contains("document_id")
                && !sql.contains("ORDER BY")
        }));
        assert_eq!(db.logical_table_digests()?, before);
        Ok(())
    }

    #[test]
    fn completion_posture_borrows_a_snapshot_and_does_not_hide_storage_errors() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("posture.db");
        let writer = DbConnection::open_file(&path)?;
        schema(&writer)?;
        insert(&writer, "own", "local", "completed")?;
        let reader = DbConnection::open_file_read_only(&path)?;
        reader.begin_read_snapshot()?;
        assert_eq!(
            reader.search_index_job_posture_in_current_snapshot("local", &["own"])?,
            SearchIndexJobPosture::Complete
        );
        insert(&writer, "peer", "local", "pending")?;
        assert_eq!(
            reader.search_index_job_posture_in_current_snapshot("local", &["own"])?,
            SearchIndexJobPosture::Complete,
            "a concurrent peer remains outside the caller's pinned snapshot"
        );
        reader.commit_read_snapshot()?;
        assert_eq!(
            reader.search_index_job_posture_in_current_snapshot("local", &["own"])?,
            SearchIndexJobPosture::Incomplete,
            "a new snapshot observes the durable peer job"
        );
        assert!(
            DbConnection::open_memory()?
                .search_index_job_posture_in_current_snapshot("local", &[])
                .is_err()
        );
        Ok(())
    }
}
