use std::fs;
use std::path::Path;
use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;

use asupersync::Outcome;
use asupersync::channel::oneshot;
use asupersync::cx::Cx;
use ee::core::journal::{
    JournalAppendOptions, JournalEntryDraft, JournalSource, append_journal_entries_stdin,
    append_journal_entry,
};
use ee::core::memory::{
    RememberBatchOptions, RememberMemoryOptions, RememberMemoryReport, remember_memory,
    remember_memory_batch_stdin,
};
use ee::core::outcome::{
    DEFAULT_HARMFUL_BURST_WINDOW_SECONDS, DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
    OutcomeBatchOptions, OutcomeRecordOptions, record_outcome, record_outcome_batch_stdin,
};
use ee::core::status::StatusReport;
use ee::core::write_owner::{
    WriteGroupCommitRunReport, WriteHotPathConfig, WriteOperation, WriteOwner, WriteResult,
    WriteSpool, WriteSpoolConfig, WriteSpoolIntent, WriteSpoolIntentKind, WriteSpoolRecordStatus,
    reset_write_group_commit_telemetry_for_test, write_group_commit_telemetry,
};
use ee::db::{CreateWorkspaceInput, DbConnection};

type TestResult = Result<(), String>;

#[test]
fn file_backed_two_writers_are_serialized() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let database_path = tempdir.path().join("write-owner.db");
    let setup = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    setup.migrate().map_err(|error| error.to_string())?;
    setup.close().map_err(|error| error.to_string())?;

    let barrier = Arc::new(Barrier::new(2));
    let active_writers = Arc::new(AtomicUsize::new(0));
    let max_active_writers = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    for index in 0..2 {
        let database_path = database_path.clone();
        let barrier = Arc::clone(&barrier);
        let active_writers = Arc::clone(&active_writers);
        let max_active_writers = Arc::clone(&max_active_writers);

        handles.push(thread::spawn(move || -> TestResult {
            let connection =
                DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
            let workspace_id = format!("wsp_writeowner{index:016}");
            let input = CreateWorkspaceInput {
                path: format!("/tmp/write-owner-{index}"),
                name: Some(format!("Write Owner {index}")),
            };

            barrier.wait();
            connection
                .with_transaction(|| {
                    let active = active_writers.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active_writers.fetch_max(active, Ordering::SeqCst);

                    let result = connection.insert_workspace(&workspace_id, &input);
                    thread::sleep(Duration::from_millis(25));
                    active_writers.fetch_sub(1, Ordering::SeqCst);
                    result
                })
                .map_err(|error| error.to_string())?;
            connection.close().map_err(|error| error.to_string())
        }));
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| "writer thread panicked".to_string())??;
    }

    if max_active_writers.load(Ordering::SeqCst) != 1 {
        return Err("write owner allowed overlapping durable writers".to_string());
    }

    let check = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    for index in 0..2 {
        let workspace_id = format!("wsp_writeowner{index:016}");
        let stored = check
            .get_workspace(&workspace_id)
            .map_err(|error| error.to_string())?;
        if stored.is_none() {
            return Err(format!("missing serialized workspace {workspace_id}"));
        }
    }
    check.close().map_err(|error| error.to_string())
}

#[test]
fn writer_spool_simulated_swarm_load_batches_all_agent_writes() -> TestResult {
    const AGENTS: usize = 8;
    const WRITES_PER_AGENT: usize = 16;
    const TOTAL_WRITES: usize = AGENTS * WRITES_PER_AGENT;

    let spool = Arc::new(Mutex::new(WriteSpool::new(
        WriteSpoolConfig::new(TOTAL_WRITES + 1, 8, 1024 * 1024, 30_000),
        0,
    )));
    let barrier = Arc::new(Barrier::new(AGENTS));
    let mut handles = Vec::new();

    for agent in 0..AGENTS {
        let spool = Arc::clone(&spool);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> TestResult {
            barrier.wait();
            for write in 0..WRITES_PER_AGENT {
                let idempotency_key = format!("agent-{agent}-write-{write}");
                let mut guard = spool
                    .lock()
                    .map_err(|_| "write spool mutex poisoned".to_string())?;
                guard
                    .enqueue(
                        WriteSpoolIntent::new(
                            WriteSpoolIntentKind::Remember,
                            "workspace",
                            idempotency_key,
                            128,
                        ),
                        (agent * WRITES_PER_AGENT + write) as u64,
                    )
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        }));
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| "writer thread panicked".to_string())??;
    }

    let mut guard = spool
        .lock()
        .map_err(|_| "write spool mutex poisoned".to_string())?;
    assert_eq!(guard.status(1_000).queue_depth, TOTAL_WRITES);

    let mut batches = 0usize;
    loop {
        let Some(batch) = guard.next_batch().map_err(|error| error.to_string())? else {
            break;
        };
        if batch.row_count() > 8 {
            return Err(format!(
                "batch exceeded configured size: {}",
                batch.row_count()
            ));
        }
        guard.mark_batch_committed(batch.batch_id, 2_000);
        batches += 1;
    }

    let status = guard.status(2_000);
    assert_eq!(status.queue_depth, 0);
    assert_eq!(status.total_enqueued, TOTAL_WRITES as u64);
    assert_eq!(status.total_committed, TOTAL_WRITES as u64);
    assert!(
        batches < TOTAL_WRITES,
        "writes should coalesce into batches"
    );
    assert_eq!(status.max_batch_size_observed, 8);
    assert!(
        guard
            .recovery_records()
            .iter()
            .all(|record| record.status == WriteSpoolRecordStatus::Committed),
        "all simulated swarm writes must recover as committed"
    );

    Ok(())
}

#[test]
fn write_group_commit_one_shot_intake_covers_durable_command_paths_and_status() -> TestResult {
    reset_write_group_commit_telemetry_for_test();
    let disabled = exercise_one_shot_durable_paths(false, "disabled")?;
    assert!(!disabled.enabled);
    assert_eq!(disabled.fsync_count, 6);
    assert_eq!(disabled.batches, 0);
    assert_eq!(disabled.writes_coalesced, 0);
    assert_eq!(disabled.fallback_reasons.disabled, 6);

    reset_write_group_commit_telemetry_for_test();
    let enabled = exercise_one_shot_durable_paths(true, "enabled")?;
    assert!(enabled.enabled);
    assert_eq!(enabled.fsync_count, 6);
    assert_eq!(enabled.batches, 6);
    assert_eq!(enabled.writes_coalesced, 0);
    assert_eq!(enabled.fallback_reasons.single_writer, 6);

    Ok(())
}

#[test]
fn write_owner_actor_group_commit_matches_per_write_results_with_fewer_fsyncs() -> TestResult {
    let disabled = run_write_owner_actor_fixture(WriteHotPathConfig::default())?;
    let enabled = run_write_owner_actor_fixture(WriteHotPathConfig::enabled(8, 8, 0, 1))?;

    assert_eq!(disabled.entity_ids, enabled.entity_ids);
    assert_eq!(disabled.callback_count, 3);
    assert_eq!(disabled.report.telemetry.fsync_count, 3);
    assert_eq!(disabled.report.telemetry.fallback_reasons.disabled, 3);
    assert_eq!(disabled.report.generation_advances, 3);

    assert_eq!(enabled.callback_count, 1);
    assert_eq!(enabled.report.telemetry.fsync_count, 1);
    assert_eq!(enabled.report.telemetry.batches, 1);
    assert_eq!(enabled.report.telemetry.writes_coalesced, 3);
    assert_eq!(enabled.report.telemetry.fsync_saved, 2);
    assert_eq!(enabled.report.telemetry.fallback_count, 0);
    assert_eq!(enabled.report.generation_advances, 1);

    Ok(())
}

fn exercise_one_shot_durable_paths(
    group_commit_enabled: bool,
    suffix: &str,
) -> Result<ee::core::write_owner::WriteGroupCommitTelemetry, String> {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_path = tempdir
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    write_group_commit_config(&workspace_path, group_commit_enabled)?;

    let remember = remember_fixture(
        &workspace_path,
        &format!("Group-commit integration memory {suffix}."),
    )?;
    record_outcome(&OutcomeRecordOptions {
        database_path: &remember.database_path,
        target_type: "memory".to_owned(),
        target_id: remember.memory_id.to_string(),
        workspace_id: Some(remember.workspace_id.clone()),
        signal: "helpful".to_owned(),
        weight: Some(1.0),
        source_type: "outcome_observed".to_owned(),
        source_id: Some(format!("write-group-commit-{suffix}")),
        reason: Some("Integration test feedback event.".to_owned()),
        evidence_json: Some(r#"{"result":"ok"}"#.to_owned()),
        session_id: None,
        event_id: Some(match suffix {
            "disabled" => "fb_21234567890123456789012345".to_owned(),
            _ => "fb_31234567890123456789012345".to_owned(),
        }),
        actor: Some("write-group-commit-test".to_owned()),
        agent_name: None,
        dry_run: false,
        harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
        harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        prompt_injection_guard: true,
    })
    .map_err(|error| error.message())?;

    let journal = append_journal_entry(
        &JournalAppendOptions {
            workspace_path: &workspace_path,
            database_path: Some(&remember.database_path),
            agent_name: Some("cod1".to_owned()),
            source: JournalSource::Manual,
        },
        &JournalEntryDraft {
            body: format!("Group-commit integration journal {suffix}."),
            kind: Some("note".to_owned()),
            ..JournalEntryDraft::default()
        },
    )
    .map_err(|error| error.message())?;
    assert_eq!(journal.status, "stored");

    let remember_batch = remember_memory_batch_stdin(
        &RememberBatchOptions {
            workspace_path: &workspace_path,
            database_path: Some(&remember.database_path),
            reinforce: false,
            dry_run: false,
            auto_link: false,
            propose_candidates: false,
        },
        &format!(
            "{{\"content\":\"Group-commit batch memory {suffix}.\",\"level\":\"procedural\",\"kind\":\"rule\",\"tags\":\"group-commit\",\"source\":\"manual://write-group-commit-test\"}}\n"
        ),
    )
    .map_err(|error| error.message())?;
    assert_eq!(remember_batch.stored_count, 1);

    let outcome_batch = record_outcome_batch_stdin(
        &OutcomeBatchOptions {
            database_path: &remember.database_path,
            actor: Some("write-group-commit-test".to_owned()),
            agent_name: None,
            dry_run: false,
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
            prompt_injection_guard: true,
        },
        &format!(
            "{{\"target\":\"{}\",\"targetType\":\"memory\",\"workspaceId\":\"{}\",\"signal\":\"helpful\",\"sourceType\":\"outcome_observed\",\"sourceId\":\"write-group-commit-batch-{suffix}\",\"reason\":\"Batch integration test feedback event.\",\"evidenceJson\":{{\"result\":\"ok\"}},\"eventId\":\"{}\"}}\n",
            remember.memory_id,
            remember.workspace_id,
            match suffix {
                "disabled" => "fb_41234567890123456789012345",
                _ => "fb_51234567890123456789012345",
            }
        ),
    )
    .map_err(|error| error.message())?;
    assert_eq!(outcome_batch.recorded_count, 1);

    let journal_batch = append_journal_entries_stdin(
        &JournalAppendOptions {
            workspace_path: &workspace_path,
            database_path: Some(&remember.database_path),
            agent_name: Some("cod1".to_owned()),
            source: JournalSource::Stdin,
        },
        &format!("{{\"body\":\"Group-commit batch journal {suffix}.\",\"kind\":\"note\"}}\n"),
    )
    .map_err(|error| error.message())?;
    assert_eq!(journal_batch.stored_count, 1);

    let direct = write_group_commit_telemetry(Some(&workspace_path));
    let status = StatusReport::gather_for_workspace(&workspace_path);
    assert_eq!(status.write_group_commit.enabled, direct.enabled);
    assert_eq!(status.write_group_commit.fsync_count, direct.fsync_count);
    assert_eq!(status.write_group_commit.batches, direct.batches);
    assert_eq!(
        status.write_group_commit.fallback_count,
        direct.fallback_count
    );

    Ok(status.write_group_commit)
}

fn write_group_commit_config(workspace_path: &Path, enabled: bool) -> TestResult {
    let config_dir = workspace_path.join(".ee");
    fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[write]\ngroup_commit_enabled = {enabled}\nbatch_window_ms = 2\nmax_batch_size = 8\nmax_inflight_bytes = 1048576\n"
        ),
    )
    .map_err(|error| error.to_string())
}

fn remember_fixture(workspace_path: &Path, content: &str) -> Result<RememberMemoryReport, String> {
    remember_memory(&RememberMemoryOptions {
        workspace_path,
        database_path: None,
        content,
        workflow_id: None,
        level: "procedural",
        kind: "rule",
        tags: Some("group-commit"),
        confidence: 0.8,
        source: Some("manual://write-group-commit-test"),
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: false,
        propose_candidates: false,
    })
    .map_err(|error| error.message())
}

struct ActorRun {
    entity_ids: Vec<Option<String>>,
    callback_count: u64,
    report: WriteGroupCommitRunReport,
}

fn run_write_owner_actor_fixture(config: WriteHotPathConfig) -> Result<ActorRun, String> {
    let (owner, handle) = WriteOwner::new(8);
    let mut receivers = Vec::new();
    for index in 0..3 {
        receivers.push(
            handle
                .try_submit(group_commit_test_operation(index))
                .ok_or_else(|| format!("request {index} should enqueue"))?,
        );
    }
    drop(handle);

    let mut callback_count = 0_u64;
    let outcome = ee::core::run_cli_future(async {
        let cx = Cx::for_testing();
        owner
            .run_group_commit(&cx, config, |operations| {
                callback_count = callback_count.saturating_add(1);
                Ok(operations
                    .iter()
                    .enumerate()
                    .map(|(index, operation)| WriteResult::Success {
                        entity_id: Some(format!("{}-{index}", operation.operation_type())),
                    })
                    .collect())
            })
            .await
    })
    .map_err(|error| error.to_string())?;
    let Outcome::Ok(report) = outcome else {
        return Err(format!("write-owner actor run should succeed: {outcome:?}"));
    };

    let entity_ids = receivers
        .iter_mut()
        .map(expect_success_id)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ActorRun {
        entity_ids,
        callback_count,
        report,
    })
}

fn group_commit_test_operation(index: usize) -> WriteOperation {
    WriteOperation::Custom {
        operation_type: "group_commit_integration".to_owned(),
        payload: serde_json::json!({ "index": index }),
    }
}

fn expect_success_id(
    receiver: &mut oneshot::Receiver<WriteResult>,
) -> Result<Option<String>, String> {
    match receiver.try_recv().map_err(|error| error.to_string())? {
        WriteResult::Success { entity_id } => Ok(entity_id),
        other => Err(format!("expected success result, got {other:?}")),
    }
}
