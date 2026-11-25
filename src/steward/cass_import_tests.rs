//! Regression coverage for the opt-in CASS maintenance job.

use super::*;
use crate::steward::{JobType, ResourceBudget, default_budgets_for_job_type};

type TestResult = Result<(), String>;

#[test]
fn cass_import_job_has_explicit_name_and_bounded_defaults() -> TestResult {
    assert_eq!(
        "cass_import"
            .parse::<JobType>()
            .map_err(|error| error.to_string())?,
        JobType::CassImport,
    );
    assert!(JobType::all().contains(&JobType::CassImport));
    let limits = default_budgets_for_job_type(JobType::CassImport);
    assert!(limits.iter().any(|limit| {
        limit.resource == ResourceType::Items
            && limit.limit == 10
            && limit.on_exceed == BudgetExceedAction::Cancel
    }));
    assert!(limits.iter().any(|limit| {
        limit.resource == ResourceType::TimeMs
            && limit.limit == 120_000
            && limit.on_exceed == BudgetExceedAction::Cancel
    }));
    assert!(
        !crate::steward::implementation::BACKGROUND_SCHEDULER_JOB_TYPES
            .contains(&JobType::CassImport),
        "external session import must remain opt-in",
    );
    Ok(())
}

#[test]
fn cass_import_preserves_parent_cancel_reason_before_store_admission() -> TestResult {
    let parent = Cx::for_testing();
    let reason = CancelReason::user("stop explicitly selected session refresh");
    parent.set_cancel_reason(reason.clone());
    let mut runner = ManualRunner::new(RunnerOptions::new().with_cancellation_cx(&parent));
    let result = runner.run_job_type(JobType::CassImport, None);
    assert_eq!(result.outcome, RunOutcome::Cancelled);
    let details = result
        .details
        .ok_or_else(|| "missing cancellation details".to_owned())?;
    assert_eq!(
        details["cancelReason"],
        serde_json::to_value(reason).map_err(|error| error.to_string())?,
    );
    assert_eq!(details["durableMutation"], false);
    assert_eq!(details["indexPublished"], false);
    Ok(())
}

#[test]
fn cass_import_zero_remaining_budget_prevents_discovery() {
    for resource in [ResourceType::Items, ResourceType::TimeMs] {
        let runner = ManualRunner::new(RunnerOptions::new());
        let mut budget = JobBudgetState::new("exhausted-cass", "2026-11-24T00:00:00Z");
        budget.add_budget(ResourceBudget::new(resource, 4, BudgetExceedAction::Cancel));
        budget.record(resource, 4);
        let (outcome, items, _, details) = runner
            .execute_cass_import_with_client(&mut budget, |_| {
                panic!("an exhausted import budget must not discover a source")
            });
        assert_eq!(
            outcome,
            if resource == ResourceType::TimeMs {
                RunOutcome::TimedOut
            } else {
                RunOutcome::Cancelled
            },
        );
        assert_eq!(items, Some(0));
        assert_eq!(
            details.expect("budget refusal details")["durableMutation"],
            false
        );
    }
}

#[test]
fn cass_import_publication_repair_keeps_custom_store_and_quotes() {
    let mut options = CassImportOptions::new("/work/team's notes");
    options.database_path = Some(PathBuf::from("/stores/alternate team's database.db"));
    assert_eq!(
        index_repair(&options),
        r#"ee job run index_coalesce --workspace '/work/team'\''s notes' --database '/stores/alternate team'\''s database.db' --json"#,
    );
}

#[test]
fn cass_import_unknown_progress_is_not_reported_as_no_mutation() -> TestResult {
    let options = CassImportOptions::new("/addressed/workspace");
    let (_, items, _, details) = finish_import(
        &options,
        "imp_00000000000000000000000000",
        ImportProgress::default(),
        Err("retained progress store is unavailable".to_owned()),
        true,
        Err(CassImportError::InvalidJson {
            source: "view",
            message: "fixture subprocess did not finish".to_owned(),
        }),
        None,
        false,
    );
    let details = details.ok_or_else(|| "missing unknown-outcome details".to_owned())?;
    assert!(details["durableMutation"].is_null());
    assert_eq!(details["mutationStatus"], "unknown");
    assert_eq!(details["mutationMayHaveOccurred"], true);
    assert_eq!(details["indexPublished"], false);
    assert_eq!(items, None);
    Ok(())
}

#[cfg(unix)]
mod subprocess {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: PathBuf,
        workspace: PathBuf,
        database: PathBuf,
        workspace_id: String,
        binary: PathBuf,
    }

    impl Fixture {
        fn new(label: &str, initialize_store: bool) -> Result<Self, String> {
            // Executable-authority checks reject writable temporary ancestors.
            // Use the same trusted target-root convention as CASS importer tests.
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
                .canonicalize()
                .map_err(|error| error.to_string())?;
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos();
            let root = target
                .join("ee-steward-cass-tests")
                .join(format!("{label}-{}-{nonce}", std::process::id()));
            let workspace = root.join("workspace");
            let binary_dir = root.join("bin");
            fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
            fs::create_dir_all(&binary_dir).map_err(|error| error.to_string())?;
            let database = workspace.join(".ee").join("ee.db");
            let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
            if initialize_store {
                fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
                let connection =
                    DbConnection::open_file(&database).map_err(|error| error.to_string())?;
                connection.migrate().map_err(|error| error.to_string())?;
                crate::core::workspace::ensure_bound_workspace(
                    &connection,
                    &workspace_id,
                    &[workspace.as_path()],
                )
                .map_err(|error| error.message().to_owned())?;
            }
            let binary = binary_dir.join("cass");
            fs::write(
                &binary,
                r#"#!/bin/sh
case "$1" in
  sessions)
    if [ -f "$EE_STEWARD_CASS_FIXTURE/source-unavailable" ]; then
      printf 'CASS fixture store is unavailable\n' >&2
      exit 1
    fi
    cat "$EE_STEWARD_CASS_FIXTURE/sessions.json";;
  view)
    for argument in "$@"; do selected="$argument"; done
    case "$selected" in
      */session-1.jsonl) cat "$EE_STEWARD_CASS_FIXTURE/view-1.json";;
      */session-2.jsonl)
        if [ -f "$EE_STEWARD_CASS_FIXTURE/block-view-2" ]; then
          printf '%s\n' "$$" > "$EE_STEWARD_CASS_FIXTURE/view-2.pid"
          : > "$EE_STEWARD_CASS_FIXTURE/view-2.started"
          exec sleep 30
        fi
        cat "$EE_STEWARD_CASS_FIXTURE/view-2.json";;
      *) exit 3;;
    esac;;
  *) exit 2;;
esac
"#,
            )
            .map_err(|error| error.to_string())?;
            let mut permissions = fs::metadata(&binary)
                .map_err(|error| error.to_string())?
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary, permissions).map_err(|error| error.to_string())?;
            Ok(Self {
                root,
                workspace,
                database,
                workspace_id,
                binary,
            })
        }

        fn options(&self) -> RunnerOptions {
            RunnerOptions::new()
                .with_workspace_path(&self.workspace)
                .with_database_path(&self.database)
        }

        fn client(&self) -> CassClient {
            CassClient::with_binary(&self.binary)
                .with_extra_env("EE_STEWARD_CASS_FIXTURE", self.root.as_os_str())
                .with_timeout(Duration::from_secs(10))
        }

        fn write_session(&self, ordinal: u32, line_count: u32) -> TestResult {
            let path = self.root.join(format!("session-{ordinal}.jsonl"));
            let raw_lines = (1..=line_count)
                .map(|line| {
                    json!({
                        "type": "user",
                        "message": {
                            "role": "user",
                            "content": format!("Refresh memory note {ordinal} line {line}"),
                        },
                    })
                    .to_string()
                })
                .collect::<Vec<_>>();
            fs::write(&path, format!("{}\n", raw_lines.join("\n")))
                .map_err(|error| error.to_string())?;
            let lines = raw_lines
                .iter()
                .enumerate()
                .map(|(index, content)| {
                    json!({"line": index + 1, "content": content, "highlighted": index == 0})
                })
                .collect::<Vec<_>>();
            let view = json!({
                "path": path,
                "target_line": 1,
                "context": 4,
                "lines": lines,
                "total_lines": line_count,
            });
            fs::write(
                self.root.join(format!("view-{ordinal}.json")),
                serde_json::to_string_pretty(&view).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())
        }

        fn select(&self, ordinals: &[u32]) -> TestResult {
            let sessions = ordinals
                .iter()
                .map(|ordinal| {
                    json!({
                        "path": self.root.join(format!("session-{ordinal}.jsonl")),
                        "workspace": self.workspace,
                        "agent": "codex",
                        "modified": "2026-11-24T00:00:00Z",
                        "message_count": 1,
                        "token_count": 8,
                    })
                })
                .collect::<Vec<_>>();
            fs::write(
                self.root.join("sessions.json"),
                json!({"sessions": sessions}).to_string(),
            )
            .map_err(|error| error.to_string())
        }

        fn run(&self, options: RunnerOptions) -> JobWorkResult {
            let runner = ManualRunner::new(options);
            let mut budget = runner.create_run_budget(
                "cass-fixture",
                JobType::CassImport,
                "2026-11-24T00:00:00Z",
            );
            runner.execute_cass_import_with_client(&mut budget, |_| Ok(self.client()))
        }

        fn connection(&self) -> Result<DbConnection, String> {
            DbConnection::open_file_read_only(&self.database).map_err(|error| error.to_string())
        }

        fn count(&self, table: &str) -> Result<u64, String> {
            let rows = self
                .connection()?
                .query(&format!("SELECT COUNT(*) FROM {table}"), &[])
                .map_err(|error| error.to_string())?;
            rows.first()
                .and_then(|row| row.get(0))
                .and_then(|value| value.as_i64())
                .and_then(|count| u64::try_from(count).ok())
                .ok_or_else(|| format!("missing count for {table}"))
        }

        fn source_counts(&self) -> Result<(u64, u64, u64, u64), String> {
            Ok((
                self.count("sessions")?,
                self.count("evidence_spans")?,
                self.count("search_index_jobs")?,
                self.count("import_ledger")?,
            ))
        }
    }

    fn success(result: JobWorkResult) -> Result<JsonValue, String> {
        let (outcome, _, error, details) = result;
        if outcome != RunOutcome::Success {
            return Err(format!(
                "CASS fixture failed: {outcome:?}, {error:?}, {details:?}"
            ));
        }
        details.ok_or_else(|| "missing CASS import details".to_owned())
    }

    #[test]
    fn cass_import_job_refreshes_growth_and_keeps_publication_queued() -> TestResult {
        let fixture = Fixture::new("growth", true)?;
        fixture.write_session(1, 1)?;
        fixture.select(&[1])?;
        let first = success(fixture.run(fixture.options()))?;
        assert_eq!(first["import"]["sessionsImported"], 1);
        assert_eq!(first["import"]["spansImported"], 1);
        assert_eq!(first["durableMutation"], true);
        assert_eq!(first["indexPublished"], false);
        let session_id = first["import"]["sessions"][0]["sessionId"]
            .as_str()
            .ok_or_else(|| "missing session ID".to_owned())?
            .to_owned();
        let first_job_id = first["import"]["sessions"][0]["indexJobId"]
            .as_str()
            .ok_or_else(|| "missing index job ID".to_owned())?
            .to_owned();
        let second = success(fixture.run(fixture.options()))?;
        assert_eq!(second["import"]["sessionsImported"], 0);
        assert_eq!(second["import"]["sessionsSkipped"], 1);
        assert_eq!(second["import"]["sessions"][0]["sessionId"], session_id);
        assert_eq!(second["import"]["sessions"][0]["indexJobId"], first_job_id);
        assert_eq!(fixture.source_counts()?, (1, 1, 1, 1));

        fixture.write_session(1, 2)?;
        let grown = success(fixture.run(fixture.options()))?;
        assert_eq!(grown["import"]["sessionsImported"], 1);
        assert_eq!(grown["import"]["spansImported"], 1);
        assert_eq!(grown["import"]["sessions"][0]["sessionId"], session_id);
        assert_eq!(fixture.count("sessions")?, 1);
        assert_eq!(fixture.count("evidence_spans")?, 2);
        let jobs = fixture
            .connection()?
            .list_search_index_jobs(&fixture.workspace_id, None)
            .map_err(|error| error.to_string())?;
        assert!(!jobs.is_empty());
        assert!(jobs.iter().all(|job| job.status == "pending"));
        assert_eq!(grown["indexPublished"], false);
        let mut import_options = CassImportOptions::new(&fixture.workspace);
        import_options.database_path = Some(fixture.database.clone());
        assert_eq!(grown["indexRequiredAction"], index_repair(&import_options));
        assert!(!fixture.workspace.join(".ee").join("index").exists());
        Ok(())
    }

    #[test]
    fn cass_import_job_preview_has_no_durable_source_or_queue_changes() -> TestResult {
        let fixture = Fixture::new("preview", true)?;
        fixture.write_session(1, 1)?;
        fixture.select(&[1])?;
        let before = fixture.source_counts()?;
        let mut options = fixture.options();
        options.dry_run = true;
        let details = success(fixture.run(options))?;
        assert_eq!(details["import"]["sessionsDiscovered"], 1);
        assert_eq!(details["durableMutation"], false);
        assert_eq!(details["indexPublished"], false);
        assert_eq!(fixture.source_counts()?, before);
        Ok(())
    }

    #[test]
    fn cass_import_job_refuses_missing_store_and_conflicting_workspace() -> TestResult {
        let missing = Fixture::new("missing", false)?;
        assert!(validate_cass_import_store(&missing.workspace, None).is_err());
        let runner = ManualRunner::new(missing.options());
        let mut budget =
            runner.create_run_budget("missing-store", JobType::CassImport, "2026-11-24T00:00:00Z");
        let result = runner.execute_cass_import_with_client(&mut budget, |_| {
            panic!("missing store must fail before executable discovery")
        });
        assert_eq!(result.0, RunOutcome::Failed);
        assert_eq!(
            result.3.expect("missing-store details")["durableMutation"],
            false
        );
        assert!(!missing.workspace.join(".ee").exists());

        let bound = Fixture::new("binding", true)?;
        let before = bound.source_counts()?;
        let runner = ManualRunner::new(
            bound
                .options()
                .with_workspace_id("wsp_00000000000000000000009999"),
        );
        let mut budget =
            runner.create_run_budget("wrong-binding", JobType::CassImport, "2026-11-24T00:00:00Z");
        let result = runner.execute_cass_import_with_client(&mut budget, |_| {
            panic!("conflicting workspace must fail before executable discovery")
        });
        assert_eq!(result.0, RunOutcome::Failed);
        assert_eq!(
            result.3.expect("binding details")["code"],
            "cass_import_workspace_mismatch"
        );
        assert_eq!(bound.source_counts()?, before);
        Ok(())
    }

    #[test]
    fn cass_import_job_missing_executable_or_source_fails_without_importing() -> TestResult {
        let fixture = Fixture::new("missing-executable", true)?;
        let before = fixture.source_counts()?;
        let runner = ManualRunner::new(fixture.options());
        let mut budget = runner.create_run_budget(
            "missing-executable",
            JobType::CassImport,
            "2026-11-24T00:00:00Z",
        );
        let result = runner.execute_cass_import_with_client(&mut budget, |_| {
            Ok(CassClient::with_binary(
                fixture.root.join("missing").join("cass"),
            ))
        });
        assert_eq!(result.0, RunOutcome::Failed);
        assert_eq!(
            result.3.expect("executable failure details")["durableMutation"],
            false
        );
        assert_eq!(fixture.source_counts()?, before);

        fs::write(fixture.root.join("source-unavailable"), "")
            .map_err(|error| error.to_string())?;
        let result = fixture.run(fixture.options());
        assert_eq!(result.0, RunOutcome::Failed);
        assert_eq!(
            result.3.expect("source failure details")["durableMutation"],
            false
        );
        assert_eq!(fixture.source_counts()?, before);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cass_import_job_shutdown_reaps_child_and_retains_prefix_for_retry() -> TestResult {
        let fixture = Fixture::new("cancel-resume", true)?;
        fixture.write_session(1, 1)?;
        fixture.write_session(2, 1)?;
        fixture.select(&[1, 2])?;
        let block = fixture.root.join("block-view-2");
        let started_marker = fixture.root.join("view-2.started");
        fs::write(&block, "").map_err(|error| error.to_string())?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&shutdown);
        let result = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !started_marker.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                signal.store(true, Ordering::SeqCst);
            });
            let result = fixture.run(fixture.options().with_cancellation_flag(shutdown));
            waiter
                .join()
                .map_err(|_| "cancellation fixture thread panicked".to_owned())?;
            Ok::<_, String>(result)
        })?;
        assert!(
            started_marker.exists(),
            "first session must commit before cancellation"
        );
        assert_eq!(result.0, RunOutcome::Cancelled, "{:?}", result.2);
        let details = result
            .3
            .ok_or_else(|| "missing partial-progress details".to_owned())?;
        assert_eq!(details["durableMutation"], true);
        assert_eq!(details["retainedProgress"]["cumulativeImportedSessions"], 1);
        assert_eq!(details["indexPublished"], false);
        assert_eq!(fixture.source_counts()?, (1, 1, 1, 1));
        let pid = fs::read_to_string(fixture.root.join("view-2.pid"))
            .map_err(|error| error.to_string())?;
        assert!(
            !Path::new("/proc").join(pid.trim()).exists(),
            "CASS child must be reaped"
        );

        // Keep the fixture marker as evidence; rename it to unblock the retry.
        fs::rename(&block, fixture.root.join("view-2.unblocked"))
            .map_err(|error| error.to_string())?;
        let retried = success(fixture.run(fixture.options()))?;
        assert_eq!(retried["import"]["sessionsImported"], 1);
        assert_eq!(retried["import"]["sessionsSkipped"], 1);
        assert_eq!(fixture.source_counts()?, (2, 2, 2, 1));
        assert_eq!(retried["retainedProgress"]["attempts"], 2);
        assert_eq!(retried["retainedProgress"]["cumulativeImportedSessions"], 2);
        Ok(())
    }
}
