use super::*;
use std::collections::BTreeSet;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture()
-> Result<(tempfile::TempDir, PathBuf, DaemonForegroundOptions), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let mut options = DaemonForegroundOptions::new(root.to_string_lossy().into_owned());
    options.interval_ms = 0;
    options.job_types = vec![JobType::HealthCheck];
    Ok((temp, root, options))
}

fn seed(root: &Path, options: &DaemonForegroundOptions) -> Result<DaemonJobRow, String> {
    let plan = record_daemon_foreground_start(root, options)?;
    let row = plan.rows[0].clone();
    drop(plan); // Simulate a stopped owner, not an owner that is still executing.
    Ok(row)
}

#[test]
fn invalid_limits_do_not_create_any_workspace_state() -> TestResult {
    for dry_run in [false, true] {
        for ticks in [0, 1_001, 1_000_000, u32::MAX] {
            let (_temp, root, mut options) = fixture()?;
            options.tick_limit = ticks;
            options.dry_run = dry_run;
            assert!(record_daemon_foreground_start(&root, &options).is_err());
            assert!(!root.join(".ee").exists());
        }
        let (_temp, root, mut options) = fixture()?;
        options.tick_limit = 1_000;
        options.interval_ms = 60_001;
        options.dry_run = dry_run;
        assert!(record_daemon_foreground_start(&root, &options).is_err());
        assert!(!root.join(".ee").exists());
    }
    Ok(())
}

#[test]
fn plan_is_one_tick_and_its_last_clone_owns_the_execution_lease() -> TestResult {
    let (_temp, root, mut options) = fixture()?;
    options.tick_limit = 1_000;
    options.job_types.push(JobType::DecaySweep);
    let plan = record_daemon_foreground_start(&root, &options)?;
    assert_eq!(plan.rows.len(), 2);
    assert!(fs::metadata(&plan.table_path)?.len() < 8_192);
    let clone = plan.clone();
    drop(plan);
    let before = fs::read(daemon_job_table_path(&root))?;
    assert!(record_daemon_foreground_start(&root, &options).is_err());
    assert!(recover_orphaned_daemon_jobs(&root, "not an orphan").is_err());
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 5)?.open_job_count,
        2
    );
    assert_eq!(fs::read(daemon_job_table_path(&root))?, before);
    drop(clone);
    assert_eq!(
        recover_orphaned_daemon_jobs(&root, "owner stopped")?.open_jobs_cancelled,
        2
    );
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 5)?.open_job_count,
        0
    );
    assert!(root.join(".ee/daemon-run.lock").is_file());
    let restarted = record_daemon_foreground_start(&root, &options)?;
    assert_eq!(restarted.rows.len(), 2);
    Ok(())
}

#[test]
fn status_of_missing_workspace_storage_does_not_create_it() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let status = daemon_status_report(&root, &options.job_types, 5)?;
    assert_eq!(status.row_count, 0);
    assert_eq!(status.open_job_count, 0);
    assert!(!root.join(".ee").exists());
    assert_eq!(
        recover_orphaned_daemon_jobs(&root, "missing")?.scanned_rows,
        0
    );
    assert!(!root.join(".ee").exists());
    Ok(())
}

#[test]
fn every_truncation_of_a_valid_unicode_row_preserves_the_complete_prefix() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let mut row = seed(&root, &options)?;
    row.reason = "escaped \"quote\", newline\n, backslash \\, and multibyte: λ 🦀".to_owned();
    let mut valid = serde_json::to_vec(&row)?;
    valid.push(b'\n');
    let path = daemon_job_table_path(&root);
    for cut in 0..=valid.len() {
        let mut bytes = valid.clone();
        bytes.extend_from_slice(&valid[..cut]);
        fs::write(&path, &bytes)?;
        let rows = load_daemon_job_rows(&root)?;
        let expected = if cut >= valid.len() - 1 { 2 } else { 1 };
        assert_eq!(rows.len(), expected, "cut {cut}");
        assert_eq!(rows[0], row);
        assert_eq!(fs::read(&path)?, bytes, "read-only at cut {cut}");
    }
    Ok(())
}

#[test]
fn recovery_preserves_tail_evidence_and_valid_history_and_is_idempotent() -> TestResult {
    let (_temp, root, options) = fixture()?;
    seed(&root, &options)?;
    let path = daemon_job_table_path(&root);
    let prefix = fs::read(&path)?;
    let tail = b"{\"reason\":\"stopped while writing \xf0\x9f";
    OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(tail)?;
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 5)?.open_job_count,
        1
    );
    assert_eq!(fs::read(&path)?.len(), prefix.len() + tail.len());
    let recovery = recover_orphaned_daemon_jobs(&root, "interrupted process")?;
    assert_eq!(recovery.scanned_rows, 1);
    assert_eq!(recovery.open_jobs_cancelled, 1);
    assert!(!recovery.recovered_rows_truncated);
    assert!(fs::read(&path)?.starts_with(&prefix));
    let evidence = root.join(".ee").join(format!(
        "daemon-jobs.torn-{}.partial",
        blake3::hash(tail).to_hex()
    ));
    assert_eq!(fs::read(evidence)?, tail);
    let after = fs::read(&path)?;
    assert_eq!(
        recover_orphaned_daemon_jobs(&root, "again")?.open_jobs_cancelled,
        0
    );
    assert_eq!(fs::read(&path)?, after);
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 5)?.open_job_count,
        0
    );
    Ok(())
}

#[test]
fn a_torn_tail_is_repaired_even_when_there_are_no_open_jobs() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let path = daemon_job_table_path(&root);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, b"{\"schema\":\"interrupted")?;
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 5)?.row_count,
        0
    );
    assert_eq!(
        recover_orphaned_daemon_jobs(&root, "empty prefix")?.open_jobs_cancelled,
        0
    );
    assert!(fs::read(&path)?.is_empty());
    let plan = record_daemon_foreground_start(&root, &options)?;
    assert_eq!(load_daemon_job_rows(&root)?.len(), 1);
    drop(plan);
    Ok(())
}

#[test]
fn a_complete_object_without_newline_is_preserved_not_truncated() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let row = seed(&root, &options)?;
    let path = daemon_job_table_path(&root);
    let bytes = serde_json::to_vec(&row)?;
    fs::write(&path, &bytes)?;
    let plan = record_daemon_foreground_start(&root, &options)?;
    let all = load_daemon_job_rows(&root)?;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0], row);
    let actual = fs::read(&path)?;
    assert_eq!(&actual[..bytes.len()], bytes.as_slice());
    assert_eq!(actual[bytes.len()], b'\n');
    drop(plan);
    Ok(())
}

#[test]
fn middle_and_terminated_corruption_fail_closed_without_repair() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let row = seed(&root, &options)?;
    let path = daemon_job_table_path(&root);
    let mut valid = serde_json::to_vec(&row)?;
    valid.push(b'\n');
    for damage in [b"{\n".as_slice(), b"not-json", b"{\"reason\":\"bad\\q"] {
        let mut bytes = valid.clone();
        bytes.extend_from_slice(damage);
        fs::write(&path, &bytes)?;
        assert!(daemon_status_report(&root, &options.job_types, 5).is_err());
        assert!(recover_orphaned_daemon_jobs(&root, "must refuse").is_err());
        assert_eq!(fs::read(&path)?, bytes);
    }
    let mut middle = valid.clone();
    middle.extend_from_slice(b"{\n");
    middle.extend_from_slice(&valid);
    fs::write(&path, &middle)?;
    assert!(recover_orphaned_daemon_jobs(&root, "middle corruption").is_err());
    assert_eq!(fs::read(&path)?, middle);
    Ok(())
}

#[test]
fn status_and_recovery_read_past_the_old_sixteen_mib_cutoff() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let mut row = seed(&root, &options)?;
    let path = daemon_job_table_path(&root);
    row.status = "success".to_owned();
    row.outcome = Some("success".to_owned());
    row.reason = "x".repeat(1_024);
    let mut count = 0;
    {
        let mut writer = io::BufWriter::new(File::create(&path)?);
        for index in 0..14_000 {
            row.daemon_job_key = format!("completed-{}", index % 8);
            serde_json::to_writer(&mut writer, &row)?;
            writer.write_all(b"\n")?;
            count += 1;
        }
        row.daemon_job_key = "only-open-row-after-old-read-cap".to_owned();
        row.status = "running".to_owned();
        row.outcome = None;
        serde_json::to_writer(&mut writer, &row)?;
        writer.write_all(b"\n")?;
        count += 1;
        writer.flush()?;
    }
    assert!(fs::metadata(&path)?.len() > MAX_HISTORY_BYTES);
    let status = daemon_status_report(&root, &options.job_types, 20)?;
    assert_eq!(status.row_count, count);
    assert_eq!(status.open_job_count, 1);
    assert!(
        load_daemon_job_rows(&root)
            .unwrap_err()
            .contains("full-history")
    );
    let recovery = recover_orphaned_daemon_jobs(&root, "after old cap")?;
    assert_eq!(recovery.scanned_rows, count);
    assert_eq!(recovery.open_jobs_cancelled, 1);
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 20)?.open_job_count,
        0
    );
    Ok(())
}

#[test]
fn recovery_detail_output_is_bounded_without_skipping_cancellations() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let mut row = seed(&root, &options)?;
    let path = daemon_job_table_path(&root);
    let mut rows = Vec::new();
    for index in 0..MAX_RECOVERY_DETAILS + 17 {
        row.daemon_job_key = format!("orphan-{index}");
        rows.push(row.clone());
    }
    append_rows(&path, &rows)?;
    let recovery = recover_orphaned_daemon_jobs(&root, "many old pending jobs")?;
    assert_eq!(recovery.open_jobs_cancelled, rows.len() + 1);
    assert_eq!(recovery.recovered_rows.len(), MAX_RECOVERY_DETAILS);
    assert_eq!(recovery.data_json()["recoveredRowsTruncated"], true);
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 5)?.open_job_count,
        0
    );
    Ok(())
}

#[test]
fn concurrent_appenders_cannot_interleave_or_repair_each_others_rows() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let row = seed(&root, &options)?;
    let path = daemon_job_table_path(&root);
    OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(b"{\"status\":\"torn")?;
    let mut workers = Vec::new();
    for worker in 0..4 {
        let path = path.clone();
        let mut row = row.clone();
        workers.push(std::thread::spawn(move || -> Result<(), String> {
            for index in 0..32 {
                row.daemon_job_key = format!("worker-{worker}-{index}");
                append_rows(&path, &[row.clone()])?;
            }
            Ok(())
        }));
    }
    for worker in workers {
        worker.join().expect("writer thread")?;
    }
    let rows = load_daemon_job_rows(&root)?;
    assert_eq!(rows.len(), 129);
    let keys = rows
        .iter()
        .map(|row| row.daemon_job_key.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(keys.len(), 129);
    Ok(())
}

#[test]
fn oversize_records_are_not_silently_treated_as_a_torn_tail() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let path = daemon_job_table_path(&root);
    fs::create_dir_all(path.parent().unwrap())?;
    let mut bytes = b"{\"reason\":\"".to_vec();
    bytes.resize(MAX_ROW_BYTES + 1, b'x');
    fs::write(&path, &bytes)?;
    assert!(
        daemon_status_report(&root, &options.job_types, 5)
            .unwrap_err()
            .contains("limit")
    );
    assert!(recover_orphaned_daemon_jobs(&root, "too large").is_err());
    assert_eq!(fs::metadata(path)?.len(), bytes.len() as u64);
    Ok(())
}

#[cfg(unix)]
#[test]
fn journal_and_execution_lease_symlinks_are_refused() -> TestResult {
    use std::os::unix::fs::symlink;
    let (_temp, root, options) = fixture()?;
    let outside = root.join("outside");
    fs::write(&outside, "do not alter")?;
    fs::create_dir(root.join(".ee"))?;
    let lease = root.join(".ee/daemon-run.lock");
    symlink(&outside, &lease)?;
    assert!(record_daemon_foreground_start(&root, &options).is_err());
    assert_eq!(fs::read_to_string(&outside)?, "do not alter");
    // Only remove the symlink created by this test, never the target.
    fs::remove_file(lease)?;
    symlink(&outside, daemon_job_table_path(&root))?;
    assert!(daemon_status_report(&root, &options.job_types, 5).is_err());
    assert!(recover_orphaned_daemon_jobs(&root, "symlink").is_err());
    assert_eq!(fs::read_to_string(&outside)?, "do not alter");
    Ok(())
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn killed_owner_releases_lease_and_next_process_can_recover() -> TestResult {
    let (_temp, root, options) = fixture()?;
    let child = Command::new(std::env::current_exe()?)
        .arg("daemon_lease_child_process")
        .env("EE_TEST_DAEMON_LEASE_ROOT", &root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut child = ChildGuard(child);
    let ready = root.join("child-ready");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < deadline {
        if let Some(status) = child.0.try_wait()? {
            panic!("child exited before lease proof: {status}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "child did not report durable running row");
    assert!(recover_orphaned_daemon_jobs(&root, "live child").is_err());
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 5)?.open_job_count,
        1
    );
    child.0.kill()?;
    child.0.wait()?;
    assert_eq!(
        recover_orphaned_daemon_jobs(&root, "killed child")?.open_jobs_cancelled,
        1
    );
    assert_eq!(
        daemon_status_report(&root, &options.job_types, 5)?.open_job_count,
        0
    );
    Ok(())
}

#[test]
fn daemon_lease_child_process() -> TestResult {
    let Some(root) = std::env::var_os("EE_TEST_DAEMON_LEASE_ROOT") else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    let mut options = DaemonForegroundOptions::new(root.to_string_lossy().into_owned());
    options.job_types = vec![JobType::HealthCheck];
    let _plan = record_daemon_foreground_start(&root, &options)?;
    fs::write(root.join("child-ready"), b"ready")?;
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn mismatched_workspace_cannot_steer_jobs_away_from_their_journal() -> TestResult {
    let (_temp, root, mut options) = fixture()?;
    let other = tempfile::tempdir()?;
    options.workspace = other.path().to_string_lossy().into_owned();
    assert!(
        record_daemon_foreground_start(&root, &options)
            .unwrap_err()
            .contains("does not match")
    );
    assert!(!root.join(".ee").exists());
    assert!(!other.path().join(".ee").exists());
    Ok(())
}
