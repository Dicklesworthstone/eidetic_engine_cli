/// The public importer must reconcile an existing transcript even when
/// CASS discovery metadata is unchanged and the initial index job finished.
#[cfg(unix)]
#[test]
fn reimport_backfills_existing_transcripts_and_reconciles_snapshot_jobs() -> TestResult {
    for initially_include_spans in [false, true] {
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
        let old_session = db.get_session(&id).map_err(|e| e.to_string())?;
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
        ensure_equal(
            &db.get_session(&id).map_err(|e| e.to_string())?,
            &old_session,
            "initial provenance snapshot remains exact",
        )?;
        for span in old_spans {
            ensure_equal(
                &db.get_evidence_span(&span.id).map_err(|e| e.to_string())?,
                &Some(span),
                "retained evidence remains exact",
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
        let retry = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
        ensure_equal(
            &retry.sessions_imported,
            &0,
            "unchanged retry does not import",
        )?;
        ensure_equal(&retry.sessions_skipped, &1, "unchanged retry skips")?;
        ensure_equal(&retry.spans_imported, &0, "no repeated span accounting")?;
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
