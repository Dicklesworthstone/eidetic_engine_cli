//! N7.1 Phase 7b (bd-17c65.14.7.2) — orchestrator for batch
//! Bayesian-posterior backfill across a workspace.
//!
//! The pure-math layer ([`crate::core::bayes`]) computes a posterior
//! from either a legacy scalar `confidence` value or a replay of the
//! workspace's feedback events. This module wraps that math in a DB
//! iteration loop that:
//!
//! 1. Reads every non-tombstoned memory in a workspace via
//!    [`DbConnection::list_memories`].
//! 2. Computes a posterior per memory per the chosen [`BackfillMode`].
//! 3. Rechecks and persists it under the database's write-owner transaction.
//! 4. Commits one `audit_actions::MEMORY_BAYES_POSTERIOR_UPDATED` audit
//!    entry and a single-document index job with each changed posterior.
//!    A `backfillSource` field distinguishes replay from inverse-fit.
//!
//! The CLI wires this into `ee migrate run --bayes-backfill-from-utility`
//! and `ee migrate run --bayes-backfill-from-feedback-events`.

use crate::core::bayes::{BetaPosterior, FeedbackSignal};
use crate::db::{
    CreateAuditInput, CreateSearchIndexJobInput, DbConnection, SearchIndexJobType, audit_actions,
    generate_audit_id,
};
use crate::models::{DomainError, MemoryId};

/// Which derivation strategy the orchestrator should apply to each
/// memory. Picked per-invocation; not mixed within a single run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillMode {
    /// Inverse-fit the legacy scalar `confidence` field to a Beta
    /// posterior with the given total pseudo-evidence weight.
    /// `weight = 2.0` matches the default in the N7.1 spec.
    FromUtility { weight_hundredths: u32 },
    /// Replay the workspace's stored feedback events from the
    /// Jeffreys prior, using `harmful_weight` for each harmful event.
    FromFeedbackEvents,
}

impl BackfillMode {
    /// Canonical short string used in audit `backfillSource` field
    /// and CLI surface tests. Kept stable so audit hash chains stay
    /// reproducible across versions.
    #[must_use]
    pub const fn audit_source(&self) -> &'static str {
        match self {
            Self::FromUtility { .. } => "backfill_from_utility",
            Self::FromFeedbackEvents => "backfill_from_feedback_events",
        }
    }
}

/// Per-run summary. The CLI prints this as a human-readable line
/// and the migrate-command surface test pins its shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Memories considered (i.e. returned by `list_memories`).
    pub scanned: usize,
    /// Memories whose posterior was rewritten (i.e. the update
    /// affected a row). Memories whose persisted (alpha, beta) was
    /// already exactly equal to the freshly-computed value are
    /// skipped (no memory write, audit row, or index job).
    pub updated: usize,
    /// Memories the orchestrator could not derive a posterior for
    /// (e.g. non-finite legacy confidence, invalid weight). These
    /// are left unchanged.
    pub skipped: usize,
}

/// Orchestrate a workspace-wide Bayes-posterior backfill.
///
/// `harmful_weight` is used by the [`BackfillMode::FromFeedbackEvents`]
/// path; pass [`crate::core::bayes::DEFAULT_HARMFUL_WEIGHT`] for the spec default.
pub fn backfill_workspace(
    conn: &DbConnection,
    workspace_id: &str,
    mode: BackfillMode,
    harmful_weight: f64,
    actor: Option<&str>,
) -> Result<BackfillReport, DomainError> {
    let memories = conn
        .list_memories(workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories for Bayes backfill: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let mut report = BackfillReport {
        scanned: memories.len(),
        ..BackfillReport::default()
    };

    tracing::info!(
        target: "ee::trust::bayes",
        mode = mode.audit_source(),
        n_memories = report.scanned,
        harmful_weight,
        "starting Bayes posterior backfill"
    );

    for memory in &memories {
        let change = conn
            .with_transaction(|| {
                backfill_memory_in_txn(
                    conn,
                    workspace_id,
                    &memory.id,
                    mode,
                    harmful_weight,
                    actor,
                )
            })
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to commit Bayes backfill for memory {}: {error}",
                    memory.id
                ),
                repair: Some("ee doctor".to_string()),
            })?;
        match change {
            BackfillChange::Updated => report.updated += 1,
            BackfillChange::Skipped => report.skipped += 1,
            BackfillChange::Unchanged => {}
        }
    }

    tracing::info!(
        target: "ee::trust::bayes",
        mode = mode.audit_source(),
        n_memories = report.scanned,
        n_migrated = report.updated,
        n_skipped = report.skipped,
        "completed Bayes posterior backfill"
    );

    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackfillChange {
    Updated,
    Unchanged,
    Skipped,
}

fn backfill_memory_in_txn(
    conn: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    mode: BackfillMode,
    harmful_weight: f64,
    actor: Option<&str>,
) -> crate::db::Result<BackfillChange> {
    // Listing chooses the work set, not the scoring snapshot. Live feedback,
    // revisions, and retirement may have committed since that scan. Derive and
    // compare under the same writer fence that publishes the update.
    let Some(memory) = conn.get_memory(memory_id)?.filter(|memory| {
        memory.workspace_id == workspace_id && memory.tombstoned_at.is_none()
    }) else {
        return Ok(BackfillChange::Skipped);
    };
    if conn.get_memory_superseded_at(memory_id)?.is_some() {
        return Ok(BackfillChange::Skipped);
    }
    let derived = match mode {
        BackfillMode::FromUtility { weight_hundredths } => {
            let weight = f64::from(weight_hundredths) / 100.0;
            BetaPosterior::from_utility_inverse(f64::from(memory.confidence), weight)
        }
        BackfillMode::FromFeedbackEvents => {
            let events = conn.list_feedback_events_for_target("memory", memory_id)?;
            let replay = events.into_iter().map(|event| {
                let signal = FeedbackSignal::from_signal_str(&event.signal);
                let weight = backfill_event_weight_or_default(
                    signal,
                    f64::from(event.weight),
                    harmful_weight,
                );
                (signal, weight)
            });
            Some(BetaPosterior::from_feedback_events(replay))
        }
    };
    let Some(posterior) = derived else {
        return Ok(BackfillChange::Skipped);
    };
    if conn
        .get_memory_bayes_posterior(memory_id)?
        .is_some_and(|(alpha, beta)| {
            approx_eq(alpha, posterior.alpha()) && approx_eq(beta, posterior.beta())
        })
    {
        return Ok(BackfillChange::Unchanged);
    }
    if !conn.update_memory_bayes_posterior(memory_id, posterior.alpha(), posterior.beta())? {
        return Ok(BackfillChange::Skipped);
    }

    let index_job_id = format!(
        "sidx_{}",
        MemoryId::now().to_string().trim_start_matches("mem_")
    );
    let details = serde_json::json!({
        "schema": "ee.audit.bayes_posterior_updated.v1",
        "backfillSource": mode.audit_source(),
        "posteriorAlpha": posterior.alpha(),
        "posteriorBeta": posterior.beta(),
        "posteriorMean": posterior.mean(),
        "effectiveSampleSize": posterior.effective_sample_size(),
        "indexJobId": index_job_id,
    })
    .to_string();
    conn.insert_audit(
        &generate_audit_id(),
        &CreateAuditInput {
            workspace_id: Some(workspace_id.to_owned()),
            actor: actor.map(str::to_owned),
            action: audit_actions::MEMORY_BAYES_POSTERIOR_UPDATED.to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(memory_id.to_owned()),
            details: Some(details),
        },
    )?;
    // Backfill runs after the CLI's schema-migration rebuild. Even a raw
    // posterior update advances source generation, so publication must remain
    // durable when no schema migration ran or a later backfill item fails.
    conn.insert_search_index_job(
        &index_job_id,
        &CreateSearchIndexJobInput {
            workspace_id: workspace_id.to_owned(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(memory_id.to_owned()),
            documents_total: 1,
        },
    )?;
    Ok(BackfillChange::Updated)
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

fn backfill_event_weight_or_default(
    signal: FeedbackSignal,
    event_weight: f64,
    harmful_weight: f64,
) -> f64 {
    if event_weight.is_finite() && event_weight >= 0.0 {
        return event_weight;
    }
    match signal {
        FeedbackSignal::Helpful => 1.0,
        FeedbackSignal::Harmful => {
            if harmful_weight.is_finite() && harmful_weight > 0.0 {
                harmful_weight
            } else {
                crate::core::bayes::DEFAULT_HARMFUL_WEIGHT
            }
        }
        FeedbackSignal::Neutral => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bayes::DEFAULT_HARMFUL_WEIGHT;
    use crate::db::{CreateFeedbackEventInput, CreateMemoryInput, CreateWorkspaceInput};
    use std::path::PathBuf;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const WORKSPACE: &str = "wsp_00000000000000000000000871";
    const MEMORY: &str = "mem_00000000000000000000000871";

    struct Fixture {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        database: PathBuf,
    }

    #[derive(Debug, PartialEq)]
    struct BackfillState {
        memory: crate::db::StoredMemory,
        posterior: (f64, f64),
        generation: Option<u64>,
        audits: Vec<crate::db::StoredAuditEntry>,
        index_jobs: Vec<crate::db::StoredSearchIndexJob>,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let root = tempfile::tempdir()?;
            let workspace = root.path().canonicalize()?;
            std::fs::create_dir(workspace.join(".ee"))?;
            let database = workspace.join(".ee/ee.db");
            let connection = DbConnection::open_file(&database)?;
            connection.migrate()?;
            connection.insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("Bayes backfill regression".to_owned()),
                },
            )?;
            connection.insert_memory(
                MEMORY,
                &CreateMemoryInput {
                    workspace_id: WORKSPACE.to_owned(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "Release verification requires reproducible source evidence.".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )?;
            connection.close()?;
            Ok(Self {
                _root: root,
                workspace,
                database,
            })
        }

        fn feedback(&self, ordinal: u32, signal: &str, weight: f32) -> TestResult {
            let connection = DbConnection::open_file(&self.database)?;
            connection.insert_feedback_event(
                &format!("fb_{ordinal:026}"),
                &CreateFeedbackEventInput {
                    workspace_id: WORKSPACE.to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: MEMORY.to_owned(),
                    signal: signal.to_owned(),
                    weight,
                    source_type: "human_explicit".to_owned(),
                    source_id: None,
                    reason: None,
                    evidence_json: None,
                    session_id: None,
                },
            )?;
            connection.close()?;
            Ok(())
        }

        fn run(&self, mode: BackfillMode) -> TestResult<BackfillReport> {
            let connection = DbConnection::open_file(&self.database)?;
            let result = backfill_workspace(
                &connection,
                WORKSPACE,
                mode,
                DEFAULT_HARMFUL_WEIGHT,
                Some("backfill-regression"),
            );
            connection.close()?;
            Ok(result?)
        }

        fn state(&self) -> TestResult<BackfillState> {
            let connection = DbConnection::open_file_read_only(&self.database)?;
            let state = BackfillState {
                memory: connection.get_memory(MEMORY)?.ok_or("fixture memory")?,
                posterior: connection
                    .get_memory_bayes_posterior(MEMORY)?
                    .ok_or("fixture posterior")?,
                generation: connection.get_workspace_generation(WORKSPACE)?,
                audits: connection
                    .list_audit_by_action(audit_actions::MEMORY_BAYES_POSTERIOR_UPDATED, None)?,
                index_jobs: connection.list_search_index_jobs(WORKSPACE, None)?,
            };
            connection.close()?;
            Ok(state)
        }

        fn block_commit_stage(&self, stage: &str) -> TestResult {
            let connection = DbConnection::open_file(&self.database)?;
            connection.execute_raw(
                "CREATE TABLE backfill_test_failpoint (enabled INTEGER NOT NULL)",
            )?;
            connection.execute_raw("INSERT INTO backfill_test_failpoint VALUES (1)")?;
            let trigger = match stage {
                "audit" => {
                    "CREATE TRIGGER reject_backfill_audit BEFORE INSERT ON audit_log \
                     WHEN NEW.action = 'memory.bayes_posterior_updated' \
                     AND (SELECT enabled FROM backfill_test_failpoint) = 1 \
                     BEGIN SELECT RAISE(ABORT, 'backfill audit blocked'); END"
                }
                "index" => {
                    "CREATE TRIGGER reject_backfill_index BEFORE INSERT ON search_index_jobs \
                     WHEN NEW.document_source = 'memory' \
                     AND (SELECT enabled FROM backfill_test_failpoint) = 1 \
                     BEGIN SELECT RAISE(ABORT, 'backfill index blocked'); END"
                }
                _ => return Err("unknown fixture failure stage".into()),
            };
            connection.execute_raw(trigger)?;
            connection.close()?;
            Ok(())
        }

        fn allow_commit(&self) -> TestResult {
            let connection = DbConnection::open_file(&self.database)?;
            connection.execute_raw("UPDATE backfill_test_failpoint SET enabled = 0")?;
            connection.close()?;
            Ok(())
        }
    }

    #[test]
    fn audit_source_strings_are_canonical() {
        assert_eq!(
            BackfillMode::FromUtility {
                weight_hundredths: 200
            }
            .audit_source(),
            "backfill_from_utility"
        );
        assert_eq!(
            BackfillMode::FromFeedbackEvents.audit_source(),
            "backfill_from_feedback_events"
        );
    }

    #[test]
    fn default_harmful_weight_re_exported_for_callers() {
        // The CLI imports DEFAULT_HARMFUL_WEIGHT via this module so
        // wiring stays self-contained. This test pins the value.
        assert!((DEFAULT_HARMFUL_WEIGHT - 2.5).abs() < 1e-12);
    }

    #[test]
    fn approx_eq_helper_matches_within_tolerance() {
        assert!(approx_eq(1.0, 1.0));
        assert!(approx_eq(0.5, 0.5 + 1e-13));
        assert!(!approx_eq(0.5, 0.5 + 1e-6));
    }

    #[test]
    fn invalid_feedback_event_weight_defaults_by_signal() {
        assert!(approx_eq(
            backfill_event_weight_or_default(FeedbackSignal::Helpful, -1.0, DEFAULT_HARMFUL_WEIGHT),
            1.0
        ));
        assert!(approx_eq(
            backfill_event_weight_or_default(
                FeedbackSignal::Harmful,
                f64::NAN,
                DEFAULT_HARMFUL_WEIGHT,
            ),
            DEFAULT_HARMFUL_WEIGHT
        ));
        assert!(approx_eq(
            backfill_event_weight_or_default(FeedbackSignal::Neutral, 0.0, DEFAULT_HARMFUL_WEIGHT),
            0.0
        ));
    }

    #[test]
    fn invalid_helpful_backfill_weight_replays_as_one_helpful_event() {
        let weight =
            backfill_event_weight_or_default(FeedbackSignal::Helpful, f64::NAN, DEFAULT_HARMFUL_WEIGHT);
        let posterior = BetaPosterior::from_feedback_events([(FeedbackSignal::Helpful, weight)]);

        assert!(approx_eq(posterior.alpha(), 1.5));
        assert!(approx_eq(posterior.beta(), 0.5));
    }

    #[test]
    fn invalid_configured_harmful_weight_still_replays_as_default_harmful_event() {
        for harmful_weight in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY] {
            let weight =
                backfill_event_weight_or_default(FeedbackSignal::Harmful, -1.0, harmful_weight);
            let posterior = BetaPosterior::from_feedback_events([(FeedbackSignal::Harmful, weight)]);

            assert!(approx_eq(posterior.alpha(), 0.5));
            assert!(approx_eq(posterior.beta(), 0.5 + DEFAULT_HARMFUL_WEIGHT));
        }
    }

    #[test]
    fn zero_weight_feedback_replay_keeps_the_prior_and_does_not_publish() -> TestResult {
        let fixture = Fixture::new()?;
        for (ordinal, signal) in ["helpful", "harmful", "confirmation", "stale"]
            .into_iter()
            .enumerate()
        {
            fixture.feedback(u32::try_from(ordinal)?, signal, 0.0)?;
        }
        let before = fixture.state()?;
        assert_eq!(before.posterior, (0.5, 0.5));
        assert_eq!(
            fixture.run(BackfillMode::FromFeedbackEvents)?,
            BackfillReport {
                scanned: 1,
                updated: 0,
                skipped: 0,
            }
        );
        assert_eq!(fixture.state()?, before);
        Ok(())
    }

    #[test]
    fn feedback_replay_preserves_positive_weights_and_ignores_zero_weight_rows() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.feedback(1, "helpful", 2.0)?;
        fixture.feedback(2, "harmful", 3.0)?;
        fixture.feedback(3, "helpful", 0.0)?;
        fixture.feedback(4, "harmful", 0.0)?;
        fixture.feedback(5, "neutral", 10.0)?;
        let before = fixture.state()?;
        assert_eq!(fixture.run(BackfillMode::FromFeedbackEvents)?.updated, 1);
        let after = fixture.state()?;
        assert_eq!(after.posterior, (2.5, 3.5));
        assert_eq!(after.memory.confidence, before.memory.confidence);
        assert_eq!(after.generation, before.generation.map(|value| value + 1));
        assert_eq!(after.audits.len(), 1);
        assert_eq!(after.index_jobs.len(), 1);
        let job = &after.index_jobs[0];
        assert_eq!(job.job_type_enum(), Some(SearchIndexJobType::SingleDocument));
        assert_eq!(job.document_source.as_deref(), Some("memory"));
        assert_eq!(job.document_id.as_deref(), Some(MEMORY));
        assert_eq!(job.status, "pending");
        let details: serde_json::Value =
            serde_json::from_str(after.audits[0].details.as_deref().ok_or("audit details")?)?;
        assert_eq!(details["indexJobId"].as_str(), Some(job.id.as_str()));
        assert_eq!(fixture.run(BackfillMode::FromFeedbackEvents)?.updated, 0);
        assert_eq!(fixture.state()?, after, "a replay cannot duplicate publication");
        Ok(())
    }

    #[test]
    fn backfill_audit_and_index_failures_roll_back_and_remain_retryable() -> TestResult {
        for stage in ["audit", "index"] {
            let fixture = Fixture::new()?;
            fixture.block_commit_stage(stage)?;
            let before = fixture.state()?;
            let mode = BackfillMode::FromUtility {
                weight_hundredths: 200,
            };
            let error = fixture.run(mode).err().ok_or("failpoint must execute")?;
            assert!(error.to_string().contains(&format!("backfill {stage} blocked")));
            assert_eq!(
                fixture.state()?,
                before,
                "{stage} failure must roll back posterior, source generation, audit and index work"
            );
            fixture.allow_commit()?;
            assert_eq!(fixture.run(mode)?.updated, 1);
            let committed = fixture.state()?;
            assert!(approx_eq(
                committed.posterior.0,
                f64::from(before.memory.confidence) * 2.0
            ));
            assert_eq!(committed.audits.len(), 1);
            assert_eq!(committed.index_jobs.len(), 1);
            assert_eq!(fixture.run(mode)?.updated, 0);
            assert_eq!(fixture.state()?, committed);
        }
        Ok(())
    }

    #[test]
    fn a_failed_later_memory_keeps_earlier_backfill_publication_recoverable() -> TestResult {
        const SECOND: &str = "mem_00000000000000000000000872";
        let fixture = Fixture::new()?;
        let connection = DbConnection::open_file(&fixture.database)?;
        let first = connection.get_memory(MEMORY)?.ok_or("fixture memory")?;
        connection.insert_memory(
            SECOND,
            &CreateMemoryInput {
                workspace_id: WORKSPACE.to_owned(),
                level: first.level,
                kind: first.kind,
                content: "A second memory also needs durable migration history.".to_owned(),
                workflow_id: None,
                confidence: 0.6,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )?;
        // Select the last memory in the actual scan order, without assuming
        // timestamps, ID order, or insertion order decide list_memories.
        let work_set = connection.list_memories(WORKSPACE, None, false)?;
        let last = work_set.last().ok_or("backfill work set")?.id.clone();
        let earlier = work_set.first().ok_or("first backfill memory")?.id.clone();
        assert_ne!(earlier, last);
        connection.execute_raw("CREATE TABLE backfill_test_failpoint (enabled INTEGER NOT NULL)")?;
        connection.execute_raw("INSERT INTO backfill_test_failpoint VALUES (1)")?;
        connection.execute_raw(&format!(
            "CREATE TRIGGER reject_later_backfill BEFORE INSERT ON audit_log \
             WHEN NEW.action = 'memory.bayes_posterior_updated' AND NEW.target_id = '{last}' \
             AND (SELECT enabled FROM backfill_test_failpoint) = 1 \
             BEGIN SELECT RAISE(ABORT, 'later backfill blocked'); END"
        ))?;
        connection.close()?;
        let mode = BackfillMode::FromUtility {
            weight_hundredths: 200,
        };
        let error = fixture.run(mode).err().ok_or("second memory must fail")?;
        assert!(error.to_string().contains("later backfill blocked"));
        let connection = DbConnection::open_file(&fixture.database)?;
        let jobs = connection.list_search_index_jobs(WORKSPACE, None)?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].document_id.as_deref(), Some(earlier.as_str()));
        assert_eq!(connection.get_memory_bayes_posterior(&last)?, Some((0.5, 0.5)));
        connection.close()?;
        fixture.allow_commit()?;
        assert_eq!(fixture.run(mode)?.updated, 1);
        let connection = DbConnection::open_file_read_only(&fixture.database)?;
        let repaired_jobs = connection.list_search_index_jobs(WORKSPACE, None)?;
        assert_eq!(repaired_jobs.len(), 2);
        assert!(repaired_jobs.contains(&jobs[0]), "the first committed job is retained");
        assert_eq!(
            connection.list_audit_by_action(audit_actions::MEMORY_BAYES_POSTERIOR_UPDATED, None)?.len(),
            2
        );
        connection.close()?;
        Ok(())
    }

    #[test]
    fn a_memory_superseded_after_the_work_scan_is_not_backfilled() -> TestResult {
        let fixture = Fixture::new()?;
        let connection = DbConnection::open_file(&fixture.database)?;
        let work_set = connection.list_memories(WORKSPACE, None, false)?;
        let selected = &work_set.first().ok_or("backfill work set")?.id;
        assert!(connection.mark_memory_superseded(selected, &chrono::Utc::now().to_rfc3339())?);
        connection.close()?;
        let before = fixture.state()?;
        let connection = DbConnection::open_file(&fixture.database)?;
        let change = connection.with_transaction(|| {
            backfill_memory_in_txn(
                &connection,
                WORKSPACE,
                selected,
                BackfillMode::FromUtility {
                    weight_hundredths: 200,
                },
                DEFAULT_HARMFUL_WEIGHT,
                None,
            )
        })?;
        connection.close()?;
        assert_eq!(change, BackfillChange::Skipped);
        assert_eq!(fixture.state()?, before, "historical revision stays immutable");
        Ok(())
    }

    #[test]
    fn backfill_index_job_recovers_search_after_the_prior_generation_was_published() -> TestResult {
        use crate::core::index::{
            IndexHealth, IndexProcessingOptions, IndexProcessingStatus, IndexRebuildOptions,
            IndexRebuildStatus, IndexStatusOptions, get_index_status,
            install_test_hash_workspace_embedder, process_index_jobs_coalesced, rebuild_index,
        };

        let fixture = Fixture::new()?;
        let _embedder = install_test_hash_workspace_embedder(WORKSPACE);
        let index_dir = fixture.workspace.join(".ee/index");
        let initial = rebuild_index(&IndexRebuildOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: Some(fixture.database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
        })?;
        assert_eq!(initial.status, IndexRebuildStatus::Success, "{initial:?}");
        let status_options = IndexStatusOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: Some(fixture.database.clone()),
            index_dir: Some(index_dir.clone()),
        };
        let before = get_index_status(&status_options)?;
        assert_eq!(before.health, IndexHealth::Ready);
        fixture.feedback(1, "helpful", 1.0)?;
        assert_eq!(fixture.run(BackfillMode::FromFeedbackEvents)?.updated, 1);
        let stale = get_index_status(&status_options)?;
        assert_eq!(stale.health, IndexHealth::Stale);
        assert_eq!(stale.index_generation, before.index_generation);
        let queued = fixture.state()?;
        assert_eq!(queued.index_jobs.len(), 1);
        let repaired = process_index_jobs_coalesced(&IndexProcessingOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: Some(fixture.database.clone()),
            index_dir: Some(index_dir),
            dry_run: false,
            job_limit: None,
        })?;
        assert_eq!(repaired.status, IndexProcessingStatus::Success, "{repaired:?}");
        assert_eq!(repaired.completed_jobs, 1);
        assert_eq!(repaired.jobs[0].job_id, queued.index_jobs[0].id);
        let ready = get_index_status(&status_options)?;
        assert_eq!(ready.health, IndexHealth::Ready);
        assert_eq!(ready.index_generation, ready.db_generation);
        assert_eq!(ready.db_generation, stale.db_generation);
        let published = fixture.state()?;
        assert_eq!(published.index_jobs[0].status, "completed");
        assert_eq!(fixture.run(BackfillMode::FromFeedbackEvents)?.updated, 0);
        assert_eq!(fixture.state()?, published);
        Ok(())
    }
}
