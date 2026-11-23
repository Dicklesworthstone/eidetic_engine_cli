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
        return Err(work_history_error(
            "append-only control requires an existing row",
        ));
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

#[test]
fn authenticated_incomplete_primary_records_fail_before_restore_side_effects() -> Result<(), String>
{
    use super::*;

    let (root, workspace, database) = tests::fixture().map_err(|e| e.message())?;
    let created = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database),
        output_dir: None,
        label: None,
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.message())?;
    let side_path = root
        .path()
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join("primary-preflight-refusal");
    let mut options = BackupRestoreOptions {
        workspace_path: workspace.clone(),
        backup_path: PathBuf::from(&created.backup_path),
        side_path,
        restore_graph_cache: false,
        dry_run: true,
    };
    let positive = restore_backup_to_side_path(&options).map_err(|e| e.message())?;
    assert_eq!(positive.status, "dry_run");
    assert!(!options.side_path.exists());

    // Preserve valid authentication while declaring a producer-side incomplete
    // stream. Integrity verification alone must not make recovery preview say
    // success or let real recovery create a destination before admission.
    let original = fs::read_to_string(&created.records_path).map_err(|e| e.to_string())?;
    let mut lines = original.lines().map(str::to_owned).collect::<Vec<_>>();
    let footer_line = lines.last_mut().ok_or("backup footer")?;
    let mut footer: JsonValue = serde_json::from_str(footer_line).map_err(|e| e.to_string())?;
    assert_eq!(footer["schema"], crate::models::EXPORT_FOOTER_SCHEMA_V1);
    assert_eq!(footer["success"], true);
    assert!(footer["authentication"].is_object());
    footer["success"] = json!(false);
    *footer_line = footer.to_string();
    let changed = lines.join("\n") + "\n";
    fs::write(&created.records_path, &changed).map_err(|e| e.to_string())?;

    let (_, mut manifest) =
        read_backup_manifest(Path::new(&created.backup_path)).map_err(|e| e.message())?;
    let artifact = manifest["artifacts"]
        .as_array_mut()
        .ok_or("manifest artifacts")?
        .iter_mut()
        .find(|artifact| artifact["path"] == RECORDS_FILE)
        .ok_or("records artifact")?;
    artifact["hash"] = json!(hash_bytes(changed.as_bytes()));
    artifact["sizeBytes"] = json!(changed.len());
    let auth = StoreAuthRoot::open(workspace_keys_dir(&workspace)).map_err(|e| e.message())?;
    authenticate_backup_manifest(&mut manifest, &auth).map_err(|e| e.message())?;
    verify_backup_manifest_authentication(&workspace, &manifest)
        .map_err(|issue| format!("preflight fixture must authenticate: {issue:?}"))?;
    fs::write(
        &created.manifest_path,
        serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: workspace,
        backup_path: options.backup_path.clone(),
    })
    .map_err(|e| e.message())?;
    assert_eq!(verified.status, "failed", "{:?}", verified.issues);
    assert_eq!(
        verified
            .issues
            .iter()
            .filter(|issue| backup_verification_issue_is_blocking(issue))
            .map(|issue| issue.code.as_str())
            .collect::<Vec<_>>(),
        vec!["trust_history_invalid"],
        "v2 provenance admission must reject the incomplete primary stream"
    );

    for dry_run in [true, false] {
        options.dry_run = dry_run;
        let error = restore_backup_to_side_path(&options)
            .err()
            .ok_or("incomplete authenticated records were accepted")?;
        assert!(
            error.message().contains("failed integrity verification"),
            "{}",
            error.message()
        );
        assert!(!options.side_path.exists(), "dry_run={dry_run}");
        assert_eq!(
            fs::read_to_string(&created.records_path).map_err(|e| e.to_string())?,
            changed
        );
    }
    Ok(())
}
