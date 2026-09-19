//! Test-only corruption below the append-only SQL guard.
//!
//! The publication fence must detect a broken recovery writer, not merely a
//! rejected UPDATE. Exercise that independent boundary with the real store and
//! restore the exact compiled trigger before returning to product verification.

use super::{DbConnection, DomainError, work_history_error};

pub(super) fn inject_history_corruption(
    db: &DbConnection,
    table: &str,
    sql: &str,
) -> Result<(), DomainError> {
    let (trigger, control) = match table {
        "recorder_events" => (
            "recorder_events_no_update",
            "UPDATE recorder_events SET event_id = event_id",
        ),
        "audit_log" => ("audit_log_no_update", "UPDATE audit_log SET id = id"),
        _ => return db.execute_raw(sql).map_err(work_history_error),
    };
    if db.count_table_rows(table).map_err(work_history_error)? == 0 {
        return Err(work_history_error("append-only control requires an existing row"));
    }
    let create = format!("CREATE TRIGGER {trigger}\n");
    let (_, definition) = crate::db::V036_APPEND_ONLY_TRIGGERS
        .sql()
        .split_once(&create)
        .ok_or_else(|| work_history_error("missing compiled append-only trigger"))?;
    let (body, _) = definition
        .split_once("\nEND;")
        .ok_or_else(|| work_history_error("incomplete compiled append-only trigger"))?;
    let definition = format!("{create}{body}\nEND;");
    // The injected mutation can invalidate its own WHERE predicate. Use an
    // independent, guaranteed-matching no-op to check trigger enforcement on
    // both sides, rather than mistaking a zero-row UPDATE for a broken guard.
    require_append_only_refusal(db, control)?;
    db.with_transaction(|| {
        db.execute_raw(&format!("DROP TRIGGER {trigger}"))?;
        db.execute_raw(sql)?;
        db.execute_raw(&definition)?;
        Ok(())
    })
    .map_err(work_history_error)?;
    require_append_only_refusal(db, control)
}

fn require_append_only_refusal(db: &DbConnection, sql: &str) -> Result<(), DomainError> {
    match db.execute_raw(sql) {
        Err(error) if error.to_string().contains("append-only") => Ok(()),
        Err(error) => Err(work_history_error(error)),
        Ok(()) => Err(work_history_error(
            "test append-only control did not refuse corruption",
        )),
    }
}

#[test]
fn audit_corruption_reinstates_the_original_append_only_guard() -> Result<(), String> {
    let (_root, _workspace, database) = super::tests::fixture().map_err(|e| e.message())?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let id = "audit_00000000000000000000000001";
    let original = db
        .get_audit(id)
        .map_err(|e| e.to_string())?
        .ok_or("missing fixture audit")?;
    assert_eq!(original.action, "memory.create");
    inject_history_corruption(
        &db,
        "audit_log",
        "UPDATE audit_log SET action = 'changed-history' WHERE action = 'memory.create'",
    )
    .map_err(|e| e.message())?;
    let changed = db
        .get_audit(id)
        .map_err(|e| e.to_string())?
        .ok_or("lost fixture audit")?;
    assert_ne!(original, changed);
    assert_eq!(changed.action, "changed-history");
    db.close().map_err(|e| e.to_string())?;
    Ok(())
}
