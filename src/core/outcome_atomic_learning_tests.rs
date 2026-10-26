//! Real-store rollback and retry tests for outcome evidence and memory learning.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use std::path::PathBuf;

const WORKSPACE: &str = "wsp_00000000000000000000000071";
const MEMORY: &str = "mem_00000000000000000000000071";
const EVENT: &str = "fb_00000000000000000000000071";
const HELD_EVENT: &str = "fb_00000000000000000000000072";
const SEED: u64 = 0x4154_4f4d_4943;

struct Fixture {
    _root: tempfile::TempDir,
    database: PathBuf,
    db: DbConnection,
    /// The trust class the memory was seeded with; a rolled-back outcome must
    /// leave exactly this class in place.
    seeded_trust: std::cell::Cell<&'static str>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        std::fs::create_dir_all(workspace.join(".ee")).unwrap();
        let database = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&database).unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        db.insert_memory(
            MEMORY,
            &CreateMemoryInput {
                workspace_id: WORKSPACE.to_owned(),
                level: "semantic".to_owned(),
                kind: "note".to_owned(),
                content: "Release validation requires the integration test suite.".to_owned(),
                workflow_id: None,
                confidence: 0.5,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .unwrap();
        Self {
            _root: root,
            database,
            db,
            seeded_trust: std::cell::Cell::new("agent_assertion"),
        }
    }

    /// ADR 0032: `human_explicit` is reachable only from `agent_validated`, by
    /// explicit operator promotion; one human outcome cannot lift an
    /// `agent_assertion` memory past the sample-size gate. A test that needs a
    /// real human promotion therefore seeds that predecessor (bd-g66ja).
    fn seed_agent_validated(&self) {
        assert!(
            self.db
                .update_memory_trust_class_if(MEMORY, "agent_assertion", "agent_validated")
                .unwrap(),
            "the fixture memory must start as agent_assertion"
        );
        self.seeded_trust.set("agent_validated");
    }

    fn options(&self) -> OutcomeRecordOptions<'_> {
        OutcomeRecordOptions {
            database_path: &self.database,
            target_type: "memory".to_owned(),
            target_id: MEMORY.to_owned(),
            workspace_id: Some(WORKSPACE.to_owned()),
            signal: "helpful".to_owned(),
            weight: None,
            source_type: "outcome_observed".to_owned(),
            // No source => no SPRT audit. With an explicit event ID, the
            // seeded stream begins at the primary feedback audit.
            source_id: None,
            reason: Some("Integration checks passed.".to_owned()),
            evidence_json: None,
            session_id: None,
            event_id: Some(EVENT.to_owned()),
            actor: Some("atomic-learning-test".to_owned()),
            agent_name: None,
            dry_run: false,
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
            prompt_injection_guard: true,
        }
    }

    fn posterior(&self) -> (f64, f64) {
        self.db.get_memory_bayes_posterior(MEMORY).unwrap().unwrap()
    }

    fn harmful_options(&self) -> OutcomeRecordOptions<'_> {
        let mut options = self.options();
        options.signal = "harmful".to_owned();
        options.weight = Some(0.25);
        options.source_id = Some("atomic-learning-release".to_owned());
        options.reason = Some("Release validation found a regression.".to_owned());
        options
    }

    fn queue_harmful_feedback(&self, source_type: &str) -> String {
        let mut options = self.harmful_options();
        options.harmful_per_source_per_hour = 1;
        assert_eq!(
            record_outcome(&options).unwrap().status,
            OutcomeRecordStatus::Recorded
        );
        let prior = self.posterior();
        options.event_id = Some(HELD_EVENT.to_owned());
        options.source_type = source_type.to_owned();
        let queued = record_outcome(&options).unwrap();
        assert_eq!(queued.status, OutcomeRecordStatus::Quarantined);
        assert_eq!(self.posterior(), prior);
        assert!(self.db.get_feedback_event(HELD_EVENT).unwrap().is_none());
        queued.quarantine.and_then(|summary| summary.id).unwrap()
    }

    fn release_options<'a>(&'a self, quarantine_id: &'a str) -> OutcomeQuarantineReviewOptions<'a> {
        OutcomeQuarantineReviewOptions {
            workspace_path: self._root.path(),
            database_path: Some(&self.database),
            quarantine_id,
            reject: false,
            actor: Some("atomic-learning-reviewer"),
            dry_run: false,
        }
    }

    fn audit_count(&self) -> u64 {
        self.db.count_table_rows("audit_log").unwrap() as u64
    }

    fn collide_audit(&self, ordinal: usize) {
        let mut ids = Deterministic::from_seed(SEED);
        let id = (0..ordinal)
            .map(|_| generate_audit_id_seeded(&mut ids))
            .last()
            .unwrap();
        self.db
            .insert_audit(
                &id,
                &CreateAuditInput {
                    workspace_id: Some(WORKSPACE.to_owned()),
                    actor: Some("atomic-learning-test".to_owned()),
                    action: "atomic_learning_failure_control".to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(MEMORY.to_owned()),
                    details: None,
                },
            )
            .unwrap();
    }

    fn assert_unrecorded(&self, posterior: (f64, f64), audits: u64) {
        assert!(get_existing_event(&self.db, EVENT).unwrap().is_none());
        assert_eq!(self.posterior(), posterior);
        assert_eq!(
            self.db.get_memory_trust_class(MEMORY).unwrap().as_deref(),
            Some(self.seeded_trust.get())
        );
        assert_eq!(
            self.db
                .count_feedback_by_signal("memory", MEMORY)
                .unwrap()
                .total_count(),
            0
        );
        assert_eq!(self.audit_count(), audits);
    }
}

#[test]
fn posterior_audit_failure_rolls_back_the_feedback_idempotency_key() {
    let fixture = Fixture::new();
    fixture.collide_audit(2);
    let prior = fixture.posterior();
    let audits = fixture.audit_count();
    let result = record_outcome_seeded(&fixture.options(), &mut Deterministic::from_seed(SEED));
    assert!(matches!(result, Err(DomainError::Storage { .. })));
    fixture.assert_unrecorded(prior, audits);
}

#[test]
fn trust_audit_failure_rolls_back_posterior_event_and_all_new_audits() {
    let fixture = Fixture::new();
    fixture.seed_agent_validated();
    fixture.collide_audit(3);
    let prior = fixture.posterior();
    let audits = fixture.audit_count();
    let mut options = fixture.options();
    options.source_type = "human_explicit".to_owned();
    let result = record_outcome_seeded(&options, &mut Deterministic::from_seed(SEED));
    assert!(matches!(result, Err(DomainError::Storage { .. })));
    fixture.assert_unrecorded(prior, audits);
}

#[test]
fn retry_after_learning_failure_records_and_learns_exactly_once() {
    let fixture = Fixture::new();
    fixture.collide_audit(2);
    let prior = fixture.posterior();
    assert!(
        record_outcome_seeded(&fixture.options(), &mut Deterministic::from_seed(SEED)).is_err()
    );
    // A fresh ID stream removes the injected audit collision, but the caller
    // deliberately retries the SAME feedback event ID and payload.
    let report =
        record_outcome_seeded(&fixture.options(), &mut Deterministic::from_seed(SEED + 1)).unwrap();
    assert_eq!(report.status, OutcomeRecordStatus::Recorded);
    assert_eq!(fixture.posterior(), (prior.0 + 1.0, prior.1));
    assert_eq!(
        report.confidence_before,
        Some((prior.0 / (prior.0 + prior.1)) as f32)
    );
    let learned = fixture.posterior();
    let audits = fixture.audit_count();
    let replay =
        record_outcome_seeded(&fixture.options(), &mut Deterministic::from_seed(SEED + 2)).unwrap();
    assert_eq!(replay.status, OutcomeRecordStatus::AlreadyRecorded);
    assert_eq!(fixture.posterior(), learned);
    assert_eq!(fixture.audit_count(), audits);
    assert_eq!(
        fixture
            .db
            .count_feedback_by_signal("memory", MEMORY)
            .unwrap()
            .total_count(),
        1
    );
}

#[test]
fn primary_audit_failure_never_applies_learning() {
    let fixture = Fixture::new();
    fixture.collide_audit(1);
    let prior = fixture.posterior();
    let audits = fixture.audit_count();
    assert!(
        record_outcome_seeded(&fixture.options(), &mut Deterministic::from_seed(SEED)).is_err()
    );
    fixture.assert_unrecorded(prior, audits);
}

#[test]
fn ordinary_signals_keep_the_existing_bayesian_update_contract() {
    for signal in [
        "helpful",
        "positive",
        "confirmation",
        "harmful",
        "negative",
        "inaccurate",
        "neutral",
        "stale",
    ] {
        let fixture = Fixture::new();
        let prior = fixture.posterior();
        let mut options = fixture.options();
        options.signal = signal.to_owned();
        let report = record_outcome(&options).unwrap();
        assert_eq!(report.status, OutcomeRecordStatus::Recorded, "{signal}");
        let expected = match FeedbackSignal::from_signal_str(signal) {
            FeedbackSignal::Helpful => (prior.0 + 1.0, prior.1),
            FeedbackSignal::Harmful => (prior.0, prior.1 + DEFAULT_HARMFUL_WEIGHT),
            FeedbackSignal::Neutral => prior,
        };
        assert_eq!(fixture.posterior(), expected, "{signal}");
        assert_eq!(
            report.confidence_after,
            Some((expected.0 / (expected.0 + expected.1)) as f32)
        );
        assert!(get_existing_event(&fixture.db, EVENT).unwrap().is_some());
    }
}

#[test]
fn dry_run_does_not_reserve_the_event_or_change_learning() {
    let fixture = Fixture::new();
    let prior = fixture.posterior();
    let audits = fixture.audit_count();
    let mut options = fixture.options();
    options.dry_run = true;
    let report = record_outcome(&options).unwrap();
    assert_eq!(report.status, OutcomeRecordStatus::DryRun);
    fixture.assert_unrecorded(prior, audits);
    options.dry_run = false;
    assert_eq!(
        record_outcome(&options).unwrap().status,
        OutcomeRecordStatus::Recorded
    );
}

#[test]
fn quarantine_does_not_learn_or_install_a_live_feedback_event() {
    let fixture = Fixture::new();
    let mut options = fixture.options();
    options.signal = "harmful".to_owned();
    options.source_id = Some("atomic-learning-burst".to_owned());
    options.harmful_per_source_per_hour = 1;
    assert_eq!(
        record_outcome(&options).unwrap().status,
        OutcomeRecordStatus::Recorded
    );
    let prior = fixture.posterior();
    let second = "fb_00000000000000000000000072";
    options.event_id = Some(second.to_owned());
    let report = record_outcome(&options).unwrap();
    assert_eq!(report.status, OutcomeRecordStatus::Quarantined);
    assert_eq!(fixture.posterior(), prior);
    assert!(get_existing_event(&fixture.db, second).unwrap().is_none());
    assert_eq!(
        fixture
            .db
            .count_feedback_by_signal("memory", MEMORY)
            .unwrap()
            .total_count(),
        1
    );
    assert_eq!(report.confidence_before, None);
    assert_eq!(report.confidence_after, None);
}

#[test]
fn successful_human_promotion_and_its_feedback_commit_together() {
    let fixture = Fixture::new();
    fixture.seed_agent_validated();
    let mut options = fixture.options();
    options.source_type = "human_explicit".to_owned();
    let report = record_outcome(&options).unwrap();
    assert_eq!(report.status, OutcomeRecordStatus::Recorded);
    assert_eq!(
        fixture
            .db
            .get_memory_trust_class(MEMORY)
            .unwrap()
            .as_deref(),
        Some("human_explicit")
    );
    assert!(get_existing_event(&fixture.db, EVENT).unwrap().is_some());
    assert!(report.confidence_after.unwrap() > report.confidence_before.unwrap());
}

#[test]
fn quarantine_release_matches_direct_memory_learning_and_applies_once() {
    let direct = Fixture::new();
    let mut direct_options = direct.harmful_options();
    assert_eq!(
        record_outcome(&direct_options).unwrap().status,
        OutcomeRecordStatus::Recorded
    );
    direct_options.event_id = Some(HELD_EVENT.to_owned());
    assert_eq!(
        record_outcome(&direct_options).unwrap().status,
        OutcomeRecordStatus::Recorded
    );

    let fixture = Fixture::new();
    let quarantine_id = fixture.queue_harmful_feedback("outcome_observed");
    let pending = fixture.db.get_feedback_quarantine(&quarantine_id).unwrap();
    let prior = fixture.posterior();
    let audits = fixture.audit_count();
    let mut release = fixture.release_options(&quarantine_id);
    release.dry_run = true;
    assert_eq!(
        review_feedback_quarantine(&release).unwrap().status,
        "dry_run"
    );
    assert_eq!(fixture.posterior(), prior);
    assert_eq!(fixture.audit_count(), audits);
    assert_eq!(
        fixture.db.get_feedback_quarantine(&quarantine_id).unwrap(),
        pending
    );
    assert!(fixture.db.get_feedback_event(HELD_EVENT).unwrap().is_none());

    release.dry_run = false;
    let report = review_feedback_quarantine(&release).unwrap();
    assert_eq!(report.status, "released");
    assert_eq!(report.feedback_event_id.as_deref(), Some(HELD_EVENT));
    assert_eq!(
        fixture.posterior(),
        (prior.0, prior.1 + DEFAULT_HARMFUL_WEIGHT)
    );
    assert_eq!(fixture.posterior(), direct.posterior());
    assert_eq!(
        fixture.db.get_memory_trust_class(MEMORY).unwrap(),
        direct.db.get_memory_trust_class(MEMORY).unwrap()
    );
    let event = fixture.db.get_feedback_event(HELD_EVENT).unwrap().unwrap();
    assert_eq!(event.signal, "harmful");
    assert_eq!(event.weight, 0.25);
    assert_eq!(event.source_type, direct_options.source_type);
    assert_eq!(event.source_id, direct_options.source_id);
    assert_eq!(event.reason, direct_options.reason);
    let bayes_audits = fixture
        .db
        .list_audit_by_action(audit_actions::OUTCOME_BAYES_UPDATE, None)
        .unwrap();
    assert_eq!(bayes_audits.len(), 2);
    let release_audit = bayes_audits
        .iter()
        .find(|audit| {
            let details: serde_json::Value =
                serde_json::from_str(audit.details.as_deref().unwrap()).unwrap();
            details["feedbackEventId"] == HELD_EVENT
        })
        .expect("released feedback must carry its posterior audit");
    assert_eq!(release_audit.actor.as_deref(), release.actor);

    let learned = fixture.posterior();
    let memory = fixture.db.get_memory(MEMORY).unwrap();
    let audits = fixture.audit_count();
    let replay = review_feedback_quarantine(&release).unwrap();
    assert_eq!(replay.status, "already_reviewed");
    assert!(!replay.changed);
    assert_eq!(fixture.posterior(), learned);
    assert_eq!(fixture.db.get_memory(MEMORY).unwrap(), memory);
    assert_eq!(fixture.audit_count(), audits);
    assert_eq!(fixture.db.count_table_rows("feedback_events").unwrap(), 2);
}

#[test]
fn failed_memory_release_rolls_back_learning_and_retry_cannot_grant_human_promotion() {
    let fixture = Fixture::new();
    fixture.seed_agent_validated();
    assert!(
        fixture
            .db
            .update_memory_bayes_posterior(MEMORY, 100.0, 0.5)
            .unwrap()
    );
    let quarantine_id = fixture.queue_harmful_feedback("human_explicit");
    let pending = fixture.db.get_feedback_quarantine(&quarantine_id).unwrap();
    let memory = fixture.db.get_memory(MEMORY).unwrap();
    let prior = fixture.posterior();
    let audits = fixture.audit_count();
    fixture
        .db
        .execute_raw(&format!(
            "CREATE TRIGGER reject_memory_release_audit BEFORE INSERT ON audit_log \
             WHEN NEW.action = '{}' BEGIN SELECT RAISE(ABORT, 'release audit failure'); END;",
            audit_actions::FEEDBACK_QUARANTINE_RELEASE
        ))
        .unwrap();
    let release = fixture.release_options(&quarantine_id);
    assert!(matches!(
        review_feedback_quarantine(&release),
        Err(DomainError::Storage { .. })
    ));
    assert_eq!(fixture.posterior(), prior);
    assert_eq!(fixture.db.get_memory(MEMORY).unwrap(), memory);
    assert_eq!(
        fixture.db.get_feedback_quarantine(&quarantine_id).unwrap(),
        pending
    );
    assert!(fixture.db.get_feedback_event(HELD_EVENT).unwrap().is_none());
    assert_eq!(fixture.audit_count(), audits);
    assert_eq!(fixture.db.count_table_rows("feedback_events").unwrap(), 1);

    fixture
        .db
        .execute_raw("DROP TRIGGER reject_memory_release_audit")
        .unwrap();
    assert_eq!(
        review_feedback_quarantine(&release).unwrap().status,
        "released"
    );
    assert_eq!(
        fixture.posterior(),
        (prior.0, prior.1 + DEFAULT_HARMFUL_WEIGHT)
    );
    // The queued human-explicit payload has an actor and reason, but reviewing
    // its admission is not the separate explicit memory-promotion operation.
    assert_eq!(
        fixture
            .db
            .get_memory_trust_class(MEMORY)
            .unwrap()
            .as_deref(),
        Some("agent_validated")
    );
    assert_eq!(
        fixture
            .db
            .get_feedback_event(HELD_EVENT)
            .unwrap()
            .unwrap()
            .source_type,
        "human_explicit"
    );
}

#[test]
fn quarantine_release_rechecks_memory_workspace_and_liveness_before_learning() {
    for tombstone in [false, true] {
        let fixture = Fixture::new();
        let quarantine_id = fixture.queue_harmful_feedback("outcome_observed");
        if tombstone {
            fixture.db.execute_raw(&format!(
                "UPDATE memories SET tombstoned_at = '2026-09-20T00:00:00Z' WHERE id = '{MEMORY}'"
            )).unwrap();
        } else {
            let foreign = "wsp_00000000000000000000000073";
            fixture
                .db
                .insert_workspace(
                    foreign,
                    &CreateWorkspaceInput {
                        path: "/atomic-memory-other-workspace".to_owned(),
                        name: None,
                    },
                )
                .unwrap();
            fixture
                .db
                .execute_raw(&format!(
                    "UPDATE memories SET workspace_id = '{foreign}' WHERE id = '{MEMORY}'"
                ))
                .unwrap();
        }
        let pending = fixture.db.get_feedback_quarantine(&quarantine_id).unwrap();
        let memory = fixture.db.get_memory(MEMORY).unwrap();
        let prior = fixture.posterior();
        let audits = fixture.audit_count();
        assert!(matches!(
            review_feedback_quarantine(&fixture.release_options(&quarantine_id)),
            Err(DomainError::Storage { .. })
        ));
        assert_eq!(fixture.posterior(), prior);
        assert_eq!(fixture.db.get_memory(MEMORY).unwrap(), memory);
        assert_eq!(
            fixture.db.get_feedback_quarantine(&quarantine_id).unwrap(),
            pending
        );
        assert!(fixture.db.get_feedback_event(HELD_EVENT).unwrap().is_none());
        assert_eq!(fixture.audit_count(), audits);
        assert_eq!(fixture.db.count_table_rows("feedback_events").unwrap(), 1);
    }
}
