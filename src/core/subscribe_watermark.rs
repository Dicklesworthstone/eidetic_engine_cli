//! Seek the subscription head without joining every modern audit to memories.
//!
//! Modern rows bind their workspace explicitly. Only unbound legacy rows newer
//! than that head can change the answer. Both reads borrow the poll's snapshot;
//! this helper never opens, acknowledges or releases a transaction.

use sqlmodel_core::Value;

use crate::db::DbConnection;
use crate::models::DomainError;

use super::{checkpoint, malformed_row, storage_error};

pub(super) fn read(db: &DbConnection, workspace: &str) -> Result<u64, DomainError> {
    read_observed(db, workspace, || Ok(()))
}

fn read_observed(
    db: &DbConnection,
    workspace: &str,
    after_bound_head: impl FnOnce() -> Result<(), DomainError>,
) -> Result<u64, DomainError> {
    checkpoint()?;
    // idx_audit_log_workspace ends in rowid. MAX on one exact workspace is
    // an indexed endpoint lookup, rather than the old OR/join over its history.
    let bound = db
        .query(
            "SELECT MAX(rowid) FROM audit_log WHERE workspace_id = ?1",
            &[Value::Text(workspace.to_owned())],
        )
        .map_err(storage_error)?;
    let bound = match bound.first().and_then(|row| row.get(0)) {
        Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .ok_or_else(|| malformed_row("workspace audit high watermark"))?,
        ),
        None => return Err(malformed_row("workspace audit high watermark")),
    };
    after_bound_head()?;
    checkpoint()?;
    // Do not join bound rows to memory state: their original workspace owns
    // them even when a target was removed or is not a memory. Conversely a
    // foreign bound audit cannot be adopted merely because its target is local.
    // The inclusive signed lower bound also preserves the minimum rowid case.
    let legacy = db
        .query(
            "SELECT a.rowid FROM audit_log a \
             WHERE a.workspace_id IS NULL AND a.rowid >= ?2 \
               AND EXISTS (SELECT 1 FROM memories m \
                           WHERE m.id = a.target_id AND m.workspace_id = ?1) \
             ORDER BY a.rowid DESC LIMIT 1",
            &[
                Value::Text(workspace.to_owned()),
                Value::BigInt(bound.unwrap_or(i64::MIN)),
            ],
        )
        .map_err(storage_error)?;
    checkpoint()?;
    let legacy = legacy
        .first()
        .map(|row| {
            row.get(0)
                .and_then(Value::as_i64)
                .ok_or_else(|| malformed_row("legacy audit high watermark"))
        })
        .transpose()?;
    let maximum = bound.into_iter().chain(legacy).max().unwrap_or(0);
    u64::try_from(maximum).map_err(|_| malformed_row("workspace audit high watermark"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::MemoryId;

    const OWN: &str = "wsp_00000000000000000000000081";
    const OTHER: &str = "wsp_00000000000000000000000082";
    const ORIGINAL: &str = "SELECT COALESCE(MAX(a.rowid), 0) FROM audit_log a \
        LEFT JOIN memories m ON m.id = a.target_id \
        WHERE a.workspace_id = ?1 OR (a.workspace_id IS NULL AND m.workspace_id = ?1)";

    fn fixture() -> DbConnection {
        initialize(DbConnection::open_memory().expect("real store"))
    }

    fn initialize(db: DbConnection) -> DbConnection {
        db.migrate().expect("migrations");
        for (workspace, path) in [(OWN, "/subscribe/own"), (OTHER, "/subscribe/other")] {
            db.insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: path.to_owned(),
                    name: None,
                },
            )
            .expect("workspace");
        }
        db
    }

    fn memory(db: &DbConnection, n: u128, workspace: &str) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(n)).to_string();
        db.insert_memory(
            &id,
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Run the checks.".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .expect("memory");
        id
    }

    fn audit(db: &DbConnection, workspace: Option<&str>, target: Option<&str>) -> String {
        let id = crate::db::generate_audit_id();
        db.insert_audit(
            &id,
            &CreateAuditInput {
                workspace_id: workspace.map(str::to_owned),
                actor: None,
                action: "memory.update".to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: target.map(str::to_owned),
                details: None,
            },
        )
        .expect("audit");
        id
    }

    fn original(db: &DbConnection, workspace: &str) -> Result<u64, DomainError> {
        let rows = db
            .query(ORIGINAL, &[Value::Text(workspace.to_owned())])
            .map_err(storage_error)?;
        rows.first()
            .and_then(|row| row.get(0))
            .and_then(Value::as_i64)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| malformed_row("reference watermark"))
    }

    fn assert_parity(db: &DbConnection) {
        for workspace in [OWN, OTHER, "unbound"] {
            let old = original(db, workspace);
            let new = read(db, workspace);
            assert_eq!(new.is_ok(), old.is_ok(), "{workspace}");
            if let (Ok(new), Ok(old)) = (new, old) {
                assert_eq!(new, old, "{workspace}");
            }
        }
    }

    #[test]
    fn watermark_matches_complete_history_for_mixed_explicit_and_legacy_scope() {
        let db = fixture();
        let own = memory(&db, 1, OWN);
        let foreign = memory(&db, 2, OTHER);
        assert_parity(&db);
        for (workspace, target) in [
            (Some(OWN), None),
            (None, Some(own.as_str())),
            (Some(OTHER), Some(own.as_str())),
            (Some(OWN), Some(foreign.as_str())),
            (None, Some(foreign.as_str())),
            (None, Some("missing")),
            (Some(OWN), Some("missing")),
            (None, None),
        ] {
            audit(&db, workspace, target);
            assert_parity(&db);
        }
        // Compare the ID COLUMN, not the Rows. sqlmodel's `Row` implements no
        // PartialEq, so `assert_eq!` on two query results does not compile
        // (E0369) -- and because every build in this repo targets `--bin ee`,
        // which never compiles cfg(test) code, that error took the entire lib
        // TEST target down with nothing reporting it. Projecting the ids keeps
        // the claim this test makes -- repeated parity checks do not mutate the
        // audit log -- and makes it checkable.
        let audit_ids = || -> Vec<String> {
            db.query("SELECT id FROM audit_log ORDER BY rowid", &[])
                .unwrap()
                .iter()
                .filter_map(|row| row.get(0).and_then(Value::as_str).map(str::to_owned))
                .collect()
        };
        let before = audit_ids();
        for _ in 0..3 {
            assert_parity(&db);
        }
        assert_eq!(audit_ids(), before);
    }

    #[test]
    fn both_head_queries_share_the_callers_snapshot_during_a_concurrent_legacy_write() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("memory.db");
        let writer = initialize(DbConnection::open_file(&path).expect("writer"));
        let own = memory(&writer, 1, OWN);
        audit(&writer, Some(OWN), Some(&own));
        let initial = read(&writer, OWN).unwrap();
        let reader =
            DbConnection::open(crate::db::DatabaseConfig::read_only_file(&path)).expect("reader");
        reader.begin_read_snapshot().expect("caller snapshot");
        let observed = read_observed(&reader, OWN, || {
            audit(&writer, None, Some(&own));
            Ok(())
        })
        .expect("consistent bound and legacy heads");
        assert_eq!(observed, initial);
        assert!(
            reader.begin_read_snapshot().is_err(),
            "helper must not release caller"
        );
        reader
            .commit_read_snapshot()
            .expect("caller releases snapshot");
        let fresh = read(&reader, OWN).expect("fresh head");
        assert!(fresh > observed);
        assert_eq!(fresh, original(&reader, OWN).unwrap());
    }

    #[test]
    fn watermark_preserves_signed_extremes_without_clamping_or_overflow() {
        let db = fixture();
        let own = memory(&db, 1, OWN);
        let first = audit(&db, None, Some(&own));
        db.execute_raw(&format!(
            "UPDATE audit_log SET rowid = {} WHERE id = '{first}'",
            i64::MIN
        ))
        .unwrap();
        assert_parity(&db);
        assert!(read(&db, OWN).is_err());
        db.execute_raw(&format!(
            "UPDATE audit_log SET rowid = 0 WHERE id = '{first}'"
        ))
        .unwrap();
        assert_eq!(read(&db, OWN).unwrap(), 0);
        assert_parity(&db);
        let second = audit(&db, Some(OWN), None);
        db.execute_raw(&format!(
            "UPDATE audit_log SET rowid = {} WHERE id = '{second}'",
            i64::MAX
        ))
        .unwrap();
        assert_parity(&db);
        assert_eq!(read(&db, OWN).unwrap(), i64::MAX as u64);
    }

    #[test]
    fn cancellation_between_head_reads_cannot_publish_a_bound_only_watermark() {
        let db = fixture();
        let own = memory(&db, 1, OWN);
        audit(&db, Some(OWN), Some(&own));
        audit(&db, None, Some(&own));
        let expected = read(&db, OWN).unwrap();
        db.begin_read_snapshot().expect("caller snapshot");
        let borrowed = &db;
        let result =
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
                read_observed(borrowed, OWN, || {
                    cx.cancel_with(
                        asupersync::CancelKind::User,
                        Some("stop before legacy head"),
                    );
                    Ok(())
                })
            })
            .expect("runtime");
        assert!(result.is_err());
        assert!(db.begin_read_snapshot().is_err());
        db.commit_read_snapshot()
            .expect("caller still owns the snapshot");
        assert_eq!(read(&db, OWN).unwrap(), expected);
    }

    #[test]
    fn a_later_legacy_storage_failure_does_not_acknowledge_the_bound_head() {
        let db = fixture();
        let own = memory(&db, 1, OWN);
        audit(&db, Some(OWN), Some(&own));
        audit(&db, None, Some(&own));
        let before = read(&db, OWN).unwrap();
        let transaction: crate::db::Result<()> = db.with_transaction(|| {
            let result = read_observed(&db, OWN, || {
                db.execute_raw("ALTER TABLE memories RENAME TO private_watermark_source")
                    .map_err(storage_error)?;
                Ok(())
            });
            let error = result
                .expect_err("legacy authority is unavailable")
                .to_string();
            assert!(!error.contains("private_watermark_source"));
            assert!(!error.contains(&own));
            assert!(
                db.query("SELECT id FROM private_watermark_source", &[])
                    .is_ok()
            );
            db.execute_raw("INSERT INTO watermark_test_rollback VALUES (1)")?;
            Ok(())
        });
        assert!(transaction.is_err());
        assert_eq!(read(&db, OWN).unwrap(), before);
    }
}
