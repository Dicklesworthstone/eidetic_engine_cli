use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn row(key: &str, timestamp: &str, padding: usize) -> DaemonJobRow {
    DaemonJobRow {
        schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(), row_id: format!("row-{key}"),
        run_id: "run-budget".to_owned(), daemon_job_key: key.to_owned(),
        runner_job_id: "j".repeat(padding), tick: 1, job_type: "health_check".to_owned(),
        status: "running".to_owned(), outcome: None, workspace: "/fixture".to_owned(),
        write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(), reason: "fixture".to_owned(),
        started_at: Some(timestamp.to_owned()), completed_at: None,
        recorded_at: timestamp.to_owned(), duration_ms: None, items_processed: None,
        error: None, dry_run: false, durable_mutation: false,
        recovered_from_orphan: false, recovery_reason: None,
    }
}

fn fixture() -> Result<(tempfile::TempDir, PathBuf, DaemonForegroundOptions), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let mut options = DaemonForegroundOptions::new(root.to_string_lossy().into_owned());
    options.interval_ms = 0;
    options.job_types = vec![JobType::HealthCheck];
    Ok((temp, root, options))
}

fn keys(rows: &[DaemonJobRow]) -> Vec<&str> {
    rows.iter().map(|row| row.daemon_job_key.as_str()).collect()
}

#[test]
fn payload_budget_counts_allocated_capacity_and_optional_strings() {
    let mut value = row("key", "2026-10-17T00:00:00Z", 0);
    let before = retained_bytes(&value);
    let mut error = String::with_capacity(65_536);
    error.push('x');
    let capacity = error.capacity();
    value.error = Some(error);
    assert_eq!(retained_bytes(&value) - before, capacity);
    let before = retained_bytes(&value);
    value.recovery_reason = Some("λ🦀".repeat(500));
    assert_eq!(retained_bytes(&value) - before, value.recovery_reason.as_ref().unwrap().capacity());
    assert!(retained_bytes(&value) > serde_json::to_vec(&value).unwrap().len());
}

#[test]
fn excluded_large_newer_row_blocks_gap_filling_by_small_older_rows() {
    let newer = row("new", "003", 0);
    let barrier = row("large", "002", 65_536);
    let older = row("old", "001", 0);
    let budget = retained_bytes(&newer) + retained_bytes(&older);
    let mut recent = RecentRows::new(10, budget);
    recent.push(barrier);
    recent.push(newer);
    recent.push(older);
    assert_eq!(keys(&recent.into_rows()), vec!["new"]);
}

fn compare_rows(left: &DaemonJobRow, right: &DaemonJobRow) -> std::cmp::Ordering {
    right.recorded_at.cmp(&left.recorded_at)
        .then_with(|| left.daemon_job_key.cmp(&right.daemon_job_key))
}

fn check_permutations(values: &mut [DaemonJobRow], at: usize, checks: &mut usize) {
    if at != values.len() {
        for index in at..values.len() {
            values.swap(at, index);
            check_permutations(values, at + 1, checks);
            values.swap(at, index);
        }
        return;
    }
    for limit in [0, 1, 3, 6] {
        for budget in [0_usize, 2_048, 10_000, 100_000] {
            let mut oracle = values.to_vec();
            oracle.sort_by(compare_rows);
            let mut used = 0;
            let expected: Vec<_> = oracle.iter().take(limit).take_while(|row| {
                let bytes = retained_bytes(row);
                if bytes > budget.saturating_sub(used) { return false; }
                used += bytes;
                true
            }).map(|row| row.daemon_job_key.as_str()).collect();
            let mut recent = RecentRows::new(limit, budget);
            for row in values.iter().cloned() { recent.push(row); }
            let actual = recent.into_rows();
            assert_eq!(keys(&actual), expected);
            assert!(actual.iter().map(retained_bytes).sum::<usize>() <= budget);
            *checks += 1;
        }
    }
}

#[test]
fn newest_prefix_matches_full_sort_for_every_scan_order() {
    let mut values = [
        row("a", "004", 0), row("b", "004", 7_000),
        row("c", "003", 30), row("d", "002", 12_000),
        row("e", "002", 0), row("f", "001", 100),
    ];
    let mut checks = 0;
    check_permutations(&mut values, 0, &mut checks);
    assert_eq!(checks, 720 * 16);
}

#[test]
fn exact_payload_budget_is_inclusive() {
    let value = row("exact", "2026-10-17T00:00:00Z", 256);
    let bytes = retained_bytes(&value);
    let mut recent = RecentRows::new(1, bytes);
    recent.push(value.clone());
    assert_eq!(keys(&recent.into_rows()), vec!["exact"]);
    let mut recent = RecentRows::new(1, bytes - 1);
    recent.push(value);
    assert!(recent.into_rows().is_empty());
}

#[test]
fn large_valid_rows_do_not_exhaust_status_or_skip_recovery() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let path = daemon_job_table_path(&root);
    // Each row is legal, but the combined details exceed the payload budget
    // long before the 256-row count ceiling. Keep fixture creation streaming.
    for index in 0..12 {
        let value = row(&format!("large-{index:03}"), &format!("2026-10-17T00:00:{index:02}Z"),
            MAX_ROW_BYTES * 3 / 4);
        append_rows(&path, &[value])?;
    }
    let before = fs::read(&path)?;
    let status = daemon_status_report(&root, &options.job_types, MAX_RECENT_ROWS)?;
    assert_eq!(status.row_count, 12);
    assert_eq!(status.open_job_count, 12);
    assert!(!status.recent_outcomes.is_empty());
    assert!(status.recent_outcomes.len() < 12);
    assert_eq!(status.recent_outcomes[0].daemon_job_key, "large-011");
    assert!(status.recent_outcomes.iter().map(retained_bytes).sum::<usize>() <= MAX_DETAIL_BYTES);
    assert_eq!(fs::read(&path)?, before, "status must remain read-only");
    let empty = daemon_status_report(&root, &options.job_types, 0)?;
    assert_eq!(empty.open_job_count, 12);
    assert!(empty.recent_outcomes.is_empty());

    let recovered = recover_orphaned_daemon_jobs(&root, "restart")?;
    assert_eq!(recovered.open_jobs_cancelled, 12);
    assert_eq!(recovered.scanned_rows, 12);
    assert!(recovered.recovered_rows_truncated);
    assert!(recovered.recovered_rows.len() < 12);
    assert!(recovered.recovered_rows.iter().map(retained_bytes).sum::<usize>() <= MAX_DETAIL_BYTES);
    assert_eq!(recovered.data_json()["recoveredRowsTruncated"], true);
    let after = daemon_status_report(&root, &options.job_types, 5)?;
    assert_eq!(after.row_count, 24);
    assert_eq!(after.open_job_count, 0);
    assert_eq!(recover_orphaned_daemon_jobs(&root, "again")?.open_jobs_cancelled, 0);
    Ok(())
}

#[test]
fn excessive_job_lists_are_rejected_before_any_workspace_mutation() -> TestResult {
    for dry_run in [false, true] {
        let (_temp, root, mut options) = fixture()?;
        options.dry_run = dry_run;
        options.job_types = vec![JobType::HealthCheck; MAX_JOBS_PER_TICK + 1];
        let error = record_daemon_foreground_start(&root, &options).unwrap_err();
        assert!(error.contains("jobs per tick"));
        assert!(!root.join(".ee").exists());
    }
    let (_temp, root, mut options) = fixture()?;
    options.dry_run = true;
    options.job_types = vec![JobType::HealthCheck; MAX_JOBS_PER_TICK];
    assert!(record_daemon_foreground_start(&root, &options)?.rows.is_empty());
    assert!(!root.join(".ee").exists());
    Ok(())
}

#[test]
fn invalid_later_row_cannot_append_a_valid_prefix_or_repair_a_tail() -> TestResult {
    let (_temp, root, _) = fixture()?;
    let path = daemon_job_table_path(&root);
    let valid = row("valid", "001", 0);
    append_rows(&path, &[valid.clone()])?;
    let tail = b"{\"reason\":\"interrupted";
    OpenOptions::new().append(true).open(&path)?.write_all(tail)?;
    let before = fs::read(&path)?;
    let entries = fs::read_dir(path.parent().unwrap())?.count();
    let mut invalid = valid.clone();
    // Escapes, not raw input length, exceed the wire bound.
    invalid.error = Some("\0".repeat(MAX_ROW_BYTES / 6));
    assert!(serde_json::to_vec(&invalid)?.len() >= MAX_ROW_BYTES);
    assert!(append_rows(&path, &[valid, invalid]).is_err());
    assert_eq!(fs::read(&path)?, before);
    assert_eq!(fs::read_dir(path.parent().unwrap())?.count(), entries);
    Ok(())
}

#[test]
fn invalid_batch_does_not_create_journal_directories() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("absent/.ee/daemon-jobs.jsonl");
    let valid = row("valid", "001", 0);
    let mut invalid = valid.clone();
    invalid.error = Some("x".repeat(MAX_ROW_BYTES));
    assert!(append_rows(&path, &[valid, invalid]).is_err());
    assert!(!temp.path().join("absent").exists());
    Ok(())
}

#[test]
fn row_size_preflight_reserves_the_final_newline() -> TestResult {
    let mut value = row("boundary", "001", 0);
    value.reason.clear();
    let overhead = serde_json::to_vec(&value)?.len();
    value.reason = "x".repeat(MAX_ROW_BYTES - 1 - overhead);
    let mut size = RowSize(0);
    serde_json::to_writer(&mut size, &value)?;
    assert_eq!(size.0, MAX_ROW_BYTES - 1);
    let mut buffer = RowBuffer(Vec::new());
    serde_json::to_writer(&mut buffer, &value)?;
    assert_eq!(buffer.0.len(), size.0);
    value.reason.push('x');
    assert!(serde_json::to_writer(RowSize(0), &value).is_err());
    assert!(serde_json::to_writer(RowBuffer(Vec::new()), &value).is_err());
    Ok(())
}
