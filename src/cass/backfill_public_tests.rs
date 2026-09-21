/// The public importer must reconcile an existing transcript even when
/// CASS discovery metadata is unchanged and the initial index job finished.
#[cfg(unix)]
#[test]
fn reimport_backfills_existing_transcripts_and_reconciles_snapshot_jobs() -> TestResult {
    for (initially_include_spans, migrated_history) in [(false, false), (true, false), (true, true)] {
        let root = unique_test_dir("cass-public-backfill")?;
        let bin_dir = root.join("bin");
        let workspace = root.join("workspace");
        let source = root.join("session.jsonl");
        fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
        fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
        fs::write(&source, "{}\n").map_err(|e| e.to_string())?;
        let binary = bin_dir.join("cass");
        write_fake_cass_binary_with_view_lines(&binary, &workspace, &source, 1)?;
        let database = root.join("ee.db");
        let client = CassClient::with_binary(binary.clone()).with_timeout(Duration::from_secs(5));
        let mut options = CassImportOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            limit: 1,
            since: None,
            dry_run: false,
            include_spans: initially_include_spans,
        };
        let first = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
        ensure_equal(&first.sessions_imported, &1, "one initial session")?;
        let first_spans = if initially_include_spans { 1_u32 } else { 0 };
        ensure_equal(
            &first.spans_imported,
            &first_spans,
            "initial transcript count",
        )?;
        let id = first.sessions[0]
            .session_id
            .clone()
            .ok_or_else(|| "initial session ID missing".to_owned())?;
        let old_job = first.sessions[0]
            .index_job_id
            .clone()
            .ok_or_else(|| "initial index job missing".to_owned())?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let old_session = db
            .get_session(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "initial session not durable".to_owned())?;
        if migrated_history {
            // Reproduce an evolved store, not just a pristine current import.
            // Retaining a raw locator must never re-admit this denied evidence.
            let reference = format!("{}:1", source.to_string_lossy()).replace('\'', "''");
            db.execute_raw(&format!(
                "UPDATE evidence_spans SET producer_kind = 'legacy_unknown', cass_span_id = '{reference}', search_eligibility = 'denied', pack_eligibility = 'denied'"
            ))
            .map_err(|e| e.to_string())?;
        }
        let old_spans = db
            .list_evidence_spans_for_session(&id)
            .map_err(|e| e.to_string())?;
        db.execute_raw("UPDATE search_index_jobs SET status = 'completed'")
            .map_err(|e| e.to_string())?;

        // The fixture deliberately keeps discovery's message_count and
        // modified values unchanged. Only the complete view reveals growth.
        write_fake_cass_binary_with_view_lines(&binary, &workspace, &source, 4)?;
        options.include_spans = true;
        let grown = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
        ensure_equal(&grown.sessions_imported, &1, "one session backfilled")?;
        ensure_equal(&grown.sessions_skipped, &0, "backfill is not a skip")?;
        ensure_equal(
            &grown.spans_imported,
            &(4 - first_spans),
            "only missing spans counted",
        )?;
        ensure_equal(&grown.index_jobs_queued, &1, "new snapshot must publish")?;
        ensure_equal(
            &grown.sessions[0].session_id.as_ref(),
            &Some(&id),
            "stable session ID",
        )?;
        let new_job = grown.sessions[0]
            .index_job_id
            .clone()
            .ok_or_else(|| "backfill publication job missing".to_owned())?;
        ensure(
            new_job != old_job,
            "completed job cannot attest a newer transcript",
        )?;
        ensure_equal(
            &db.count_table_rows("sessions").map_err(|e| e.to_string())?,
            &1,
            "no duplicate session",
        )?;
        ensure_equal(
            &db.list_evidence_spans_for_session(&id)
                .map_err(|e| e.to_string())?
                .len(),
            &4,
            "complete transcript",
        )?;
        let refreshed_session = db
            .get_session(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "refreshed session not durable".to_owned())?;
        let mut metadata: JsonValue = serde_json::from_str(
            refreshed_session
                .metadata_json
                .as_deref()
                .ok_or_else(|| "refresh checkpoint metadata missing".to_owned())?,
        )
        .map_err(|e| e.to_string())?;
        let checkpoint = metadata
            .as_object_mut()
            .and_then(|object| object.remove("eeCassImportCheckpoint"))
            .ok_or_else(|| "refresh must persist its publication checkpoint".to_owned())?;
        ensure_equal(
            &checkpoint["schema"],
            &json!("ee.cass.session_checkpoint.v1"),
            "checkpoint schema",
        )?;
        ensure_equal(&checkpoint["indexJobId"], &json!(new_job), "current snapshot job")?;
        ensure_equal(
            &checkpoint["previousIndexJobId"],
            &json!(old_job),
            "checkpoint predecessor",
        )?;
        ensure(
            checkpoint["snapshotRevision"]
                .as_str()
                .and_then(|revision| revision.strip_prefix("blake3:"))
                .is_some_and(|hash| {
                    hash.len() == 64
                        && hash.bytes().all(|byte| {
                            byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                        })
                }),
            "checkpoint must attest a canonical transcript revision",
        )?;
        let original_metadata: JsonValue =
            serde_json::from_str(old_session.metadata_json.as_deref().unwrap_or("{}"))
                .map_err(|e| e.to_string())?;
        ensure_equal(&metadata, &original_metadata, "original metadata remains exact")?;
        let mut expected_session = old_session;
        expected_session.metadata_json = refreshed_session.metadata_json.clone();
        ensure_equal(
            &refreshed_session,
            &expected_session,
            "only the validated publication checkpoint may change",
        )?;
        for span in old_spans {
            if migrated_history {
                ensure(
                    db.get_search_admitted_evidence_span(&span.id, &span.workspace_id)
                        .map_err(|e| e.to_string())?
                        .is_none(),
                    "recognizing legacy history cannot restore its search authority",
                )?;
            }
            ensure_equal(
                &db.get_evidence_span(&span.id).map_err(|e| e.to_string())?,
                &Some(span),
                "retained evidence remains exact",
            )?;
        }
        for line in (first_spans + 1)..=4 {
            let reference = format!("{}:{line}", source.to_string_lossy());
            let evidence_id = stable_evidence_id(&id, &reference);
            ensure(
                db.get_search_admitted_evidence_span(&evidence_id, &refreshed_session.workspace_id)
                    .map_err(|e| e.to_string())?
                    .is_some(),
                "newly imported evidence must be search-admitted through the normal boundary",
            )?;
        }

        let durable_counts = || {
            [
                "sessions",
                "evidence_spans",
                "search_index_jobs",
                "audit_log",
            ]
            .into_iter()
            .map(|table| db.count_table_rows(table).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, String>>()
        };
        let before_retry = durable_counts()?;
        for status in ["pending", "failed", "cancelled", "running"] {
            db.execute_raw(&format!(
                "UPDATE search_index_jobs SET status = '{status}' WHERE id = '{new_job}'"
            ))
            .map_err(|e| e.to_string())?;
            let retry = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
            ensure_equal(&retry.sessions_imported, &0, "unchanged retry does not import")?;
            ensure_equal(&retry.sessions_skipped, &1, "unchanged retry skips")?;
            ensure_equal(&retry.spans_imported, &0, "no repeated span accounting")?;
            ensure_equal(&retry.index_jobs_queued, &1, "unfinished publication remains visible")?;
            ensure_equal(
                &retry.sessions[0].index_job_id.as_ref(),
                &Some(&new_job),
                "pending snapshot job recovered",
            )?;
            ensure_equal(
                &durable_counts()?,
                &before_retry,
                "retry cannot duplicate durable work",
            )?;
            ensure_equal(
                &db.get_session(&id).map_err(|e| e.to_string())?,
                &Some(refreshed_session.clone()),
                "retry preserves the exact committed checkpoint",
            )?;
        }

        db.execute_raw("UPDATE search_index_jobs SET status = 'completed'")
            .map_err(|e| e.to_string())?;
        let published = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
        ensure_equal(
            &published.index_jobs_queued,
            &0,
            "completed snapshot needs no new job",
        )?;
        ensure_equal(
            &published.spans_imported,
            &0,
            "completed snapshot is idempotent",
        )?;

        write_fake_cass_binary_with_view_lines(&binary, &workspace, &source, 2)?;
        let before_refusal = durable_counts()?;
        ensure(
            import_cass_sessions(&client, &options).is_err(),
            "shortened history must fail closed",
        )?;
        ensure_equal(
            &durable_counts()?,
            &before_refusal,
            "refused refresh cannot partially mutate evidence",
        )?;
        options.include_spans = false;
        let metadata_only = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
        ensure_equal(
            &metadata_only.spans_imported,
            &0,
            "metadata-only import never reads a changed view",
        )?;
        options.include_spans = true;
        options.dry_run = true;
        let dry_run = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
        ensure(dry_run.dry_run, "dry run stays non-mutating")?;
        ensure_equal(
            &durable_counts()?,
            &before_refusal,
            "dry run and metadata-only preserve evidence",
        )?;
        db.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}
