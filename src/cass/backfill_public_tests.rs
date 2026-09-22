/// The public importer must reconcile an existing transcript even when
/// CASS discovery metadata is unchanged and the initial index job finished.
#[cfg(unix)]
#[test]
fn reimport_backfills_existing_transcripts_and_reconciles_snapshot_jobs() -> TestResult {
    for (initially_include_spans, migrated_history) in [(false, false), (true, false), (true, true)]
    {
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
        ensure_equal(
            &checkpoint["indexJobId"],
            &json!(new_job),
            "current snapshot job",
        )?;
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
                        && hash
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                }),
            "checkpoint must attest a canonical transcript revision",
        )?;
        let original_metadata: JsonValue =
            serde_json::from_str(old_session.metadata_json.as_deref().unwrap_or("{}"))
                .map_err(|e| e.to_string())?;
        ensure_equal(
            &metadata,
            &original_metadata,
            "original metadata remains exact",
        )?;
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
            ensure_equal(
                &retry.sessions_imported,
                &0,
                "unchanged retry does not import",
            )?;
            ensure_equal(&retry.sessions_skipped, &1, "unchanged retry skips")?;
            ensure_equal(&retry.spans_imported, &0, "no repeated span accounting")?;
            ensure_equal(
                &retry.index_jobs_queued,
                &1,
                "unfinished publication remains visible",
            )?;
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

/// Capture and publication recovery are independent: declining another CASS
/// view cannot erase work already recorded by a newer transcript checkpoint.
#[cfg(unix)]
#[test]
fn metadata_only_reimport_recovers_latest_publication_without_reading_transcript() -> TestResult {
    let root = unique_test_dir("cass-metadata-publication")?;
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
        include_spans: true,
    };
    let first = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
    let id = first.sessions[0]
        .session_id
        .clone()
        .ok_or_else(|| "initial session missing".to_owned())?;
    let original = first.sessions[0]
        .index_job_id
        .clone()
        .ok_or_else(|| "initial publication missing".to_owned())?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    db.execute_raw("UPDATE search_index_jobs SET status = 'completed'")
        .map_err(|e| e.to_string())?;
    write_fake_cass_binary_with_view_lines(&binary, &workspace, &source, 4)?;
    let grown = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
    let latest = grown.sessions[0]
        .index_job_id
        .clone()
        .ok_or_else(|| "refreshed publication missing".to_owned())?;
    ensure(
        latest != original,
        "growth requires a new publication identity",
    )?;
    let stored = db.get_session(&id).map_err(|e| e.to_string())?;
    let evidence = db
        .list_evidence_spans_for_session(&id)
        .map_err(|e| e.to_string())?;
    ensure_equal(&evidence.len(), &4, "growth was committed")?;
    let counts = || {
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
    let before = counts()?;
    // This output cannot be imported. A successful metadata-only retry thus
    // proves that publication recovery did not secretly fetch the transcript.
    write_fake_cass_binary_with_verbatim_view(
        &binary,
        &workspace,
        &source,
        "not JSON PRIVATE_RAW_SENTINEL\n",
    )?;
    options.include_spans = false;
    for status in ["pending", "failed", "cancelled", "running"] {
        db.execute_raw(&format!(
            "UPDATE search_index_jobs SET status = '{status}' WHERE id = '{latest}'"
        ))
        .map_err(|e| e.to_string())?;
        let retry = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
        ensure_equal(
            &retry.sessions_imported,
            &0,
            "recovery must not recapture evidence",
        )?;
        ensure_equal(
            &retry.spans_imported,
            &0,
            "metadata-only means no new spans",
        )?;
        ensure_equal(
            &retry.index_jobs_queued,
            &1,
            "latest unfinished work stays visible",
        )?;
        ensure_equal(
            &retry.sessions[0].index_job_id.as_ref(),
            &Some(&latest),
            "recover current checkpoint, not original job",
        )?;
        ensure_equal(&counts()?, &before, "retry does not duplicate durable work")?;
        ensure_equal(
            &db.get_session(&id).map_err(|e| e.to_string())?,
            &stored,
            "checkpoint stays exact",
        )?;
        ensure_equal(
            &db.list_evidence_spans_for_session(&id)
                .map_err(|e| e.to_string())?,
            &evidence,
            "history stays exact",
        )?;
        let job = db
            .get_search_index_job(&latest)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "latest job disappeared".to_owned())?;
        ensure_equal(
            &job.status.as_str(),
            &status,
            "recovery never steals a live publisher",
        )?;
    }
    db.execute_raw(&format!(
        "UPDATE search_index_jobs SET status = 'completed' WHERE id = '{latest}'"
    ))
    .map_err(|e| e.to_string())?;
    let published = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
    ensure_equal(
        &published.index_jobs_queued,
        &0,
        "completed latest snapshot needs no work",
    )?;
    // Loss of rebuildable queue state must not force another upstream read.
    db.execute_raw(&format!(
        "DELETE FROM search_index_jobs WHERE id = '{latest}'"
    ))
    .map_err(|e| e.to_string())?;
    let recovered = import_cass_sessions(&client, &options).map_err(|e| e.to_string())?;
    ensure_equal(
        &recovered.sessions[0].index_job_id.as_ref(),
        &Some(&latest),
        "recreate the exact checkpoint job",
    )?;
    ensure_equal(
        &counts()?,
        &before,
        "only the missing queue row is recreated",
    )?;
    ensure_equal(
        &db.get_session(&id).map_err(|e| e.to_string())?,
        &stored,
        "recovery preserves the checkpoint",
    )?;
    ensure_equal(
        &db.list_evidence_spans_for_session(&id)
            .map_err(|e| e.to_string())?,
        &evidence,
        "recovery preserves evidence",
    )?;
    // Negative control: actually asking to capture the invalid view must fail.
    options.include_spans = true;
    ensure(
        import_cass_sessions(&client, &options).is_err(),
        "the transcript fixture is genuinely invalid",
    )?;
    ensure_equal(
        &counts()?,
        &before,
        "failed capture cannot undo publication recovery",
    )?;
    db.close().map_err(|e| e.to_string())?;
    Ok(())
}

/// A recent source modification must reach the live refresh path even when
/// both the original start and the retained end predate the --since window.
#[cfg(unix)]
#[test]
fn since_reimport_captures_resumed_session_growth_and_retains_end_provenance() -> TestResult {
    let root = unique_test_dir("cass-since-resumed")?;
    let bin_dir = root.join("bin");
    let workspace = root.join("workspace");
    let source = root.join("session.jsonl");
    fs::create_dir_all(&bin_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::write(&source, "{}\n").map_err(|error| error.to_string())?;
    let binary = bin_dir.join("cass");
    let database = root.join("ee.db");
    let write_snapshot = |modified: &str, count: u32| -> TestResult {
        let sessions = json!({"sessions": [{
            "path": source.to_string_lossy(),
            "workspace": workspace.to_string_lossy(),
            "agent": "codex",
            "started_at": "2026-09-01T00:00:00Z",
            "ended_at": "2026-09-02T00:00:00Z",
            "modified": modified,
            "message_count": count
        }]});
        let lines: Vec<_> = (1..=count)
            .map(|line| json!({"line": line, "content": format!("Resumed build observation {line}.")}))
            .collect();
        let view = json!({
            "path": source.to_string_lossy(),
            "target_line": 1,
            "context": DEFAULT_VIEW_CONTEXT,
            "lines": lines,
            "total_lines": count
        });
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  sessions) cat <<'EE_CASS_SESSIONS'\n{sessions}\nEE_CASS_SESSIONS\n;;\n  view) cat <<'EE_CASS_VIEW'\n{view}\nEE_CASS_VIEW\n;;\n  *) exit 2;;\nesac\n"
        );
        fs::write(&binary, script).map_err(|error| error.to_string())?;
        let mut permissions = fs::metadata(&binary)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).map_err(|error| error.to_string())
    };
    write_snapshot("2026-09-02T00:00:00Z", 1)?;
    let cutoff = DateTime::parse_from_rfc3339("2026-09-20T00:00:00Z")
        .map_err(|error| error.to_string())?
        .with_timezone(&Utc);
    let client = CassClient::with_binary(binary.clone()).with_timeout(Duration::from_secs(5));
    let mut options = CassImportOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        limit: 1,
        since: Some(cutoff),
        dry_run: true,
        include_spans: true,
    };
    let old = import_cass_sessions(&client, &options).map_err(|error| error.to_string())?;
    ensure_equal(&old.sessions_discovered, &0, "old activity is excluded")?;
    ensure(!database.exists(), "dry-run selection creates no database")?;

    options.since = None;
    options.dry_run = false;
    let first = import_cass_sessions(&client, &options).map_err(|error| error.to_string())?;
    let id = first.sessions[0]
        .session_id
        .clone()
        .ok_or_else(|| "initial session missing".to_owned())?;
    let original_job = first.sessions[0]
        .index_job_id
        .clone()
        .ok_or_else(|| "initial job missing".to_owned())?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let retained = db
        .list_evidence_spans_for_session(&id)
        .map_err(|error| error.to_string())?;
    db.execute_raw("UPDATE search_index_jobs SET status = 'completed'")
        .map_err(|error| error.to_string())?;

    // Exactly the cutoff instant in another timezone. Start/end stay old, so
    // neither can substitute for the independent modification timestamp.
    write_snapshot("2026-09-19T20:00:00-04:00", 3)?;
    options.since = Some(cutoff);
    let grown = import_cass_sessions(&client, &options).map_err(|error| error.to_string())?;
    ensure_equal(&grown.sessions_imported, &1, "resumed session refreshed")?;
    ensure_equal(
        &grown.spans_imported,
        &2,
        "only newly observed turns imported",
    )?;
    ensure_equal(
        &grown.index_jobs_queued,
        &1,
        "new transcript queues publication",
    )?;
    ensure_equal(
        &grown.sessions[0].session_id.as_ref(),
        &Some(&id),
        "stable session identity",
    )?;
    let latest_job = grown.sessions[0]
        .index_job_id
        .clone()
        .ok_or_else(|| "resumed snapshot job missing".to_owned())?;
    ensure(
        latest_job != original_job,
        "completed original job cannot publish resumed activity",
    )?;
    let stored = db
        .get_session(&id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "refreshed session missing".to_owned())?;
    ensure_equal(
        &stored.started_at.as_deref(),
        &Some("2026-09-01T00:00:00Z"),
        "retain start provenance",
    )?;
    ensure_equal(
        &stored.ended_at.as_deref(),
        &Some("2026-09-02T00:00:00Z"),
        "retain explicit end provenance",
    )?;
    for span in retained {
        ensure_equal(
            &db.get_evidence_span(&span.id)
                .map_err(|error| error.to_string())?,
            &Some(span),
            "earlier evidence is unchanged",
        )?;
    }
    for line in 2..=3 {
        let evidence_id = stable_evidence_id(&id, &format!("{}:{line}", source.to_string_lossy()));
        ensure(
            db.get_search_admitted_evidence_span(&evidence_id, &stored.workspace_id)
                .map_err(|error| error.to_string())?
                .is_some(),
            "new activity is searchable under its native evidence identity",
        )?;
    }
    let retry = import_cass_sessions(&client, &options).map_err(|error| error.to_string())?;
    ensure_equal(
        &retry.sessions_imported,
        &0,
        "unchanged recent activity is idempotent",
    )?;
    ensure_equal(
        &retry.spans_imported,
        &0,
        "retry does not duplicate evidence",
    )?;
    ensure_equal(
        &retry.sessions[0].index_job_id.as_ref(),
        &Some(&latest_job),
        "retry retains pending snapshot work",
    )?;
    ensure_equal(
        &db.count_table_rows("sessions")
            .map_err(|error| error.to_string())?,
        &1,
        "one durable session",
    )?;
    ensure_equal(
        &db.list_evidence_spans_for_session(&id)
            .map_err(|error| error.to_string())?
            .len(),
        &3,
        "complete durable transcript",
    )?;
    db.close().map_err(|error| error.to_string())?;
    Ok(())
}
