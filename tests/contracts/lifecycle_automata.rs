//! Gate 13 lifecycle automaton certificate contract coverage.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::models::certificate::{
    AutomatonState, AutomatonTransition, BackupAutomatonCertificate, ImportAutomatonCertificate,
    IndexPublishAutomatonCertificate, ShutdownAutomatonCertificate,
};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("certificates")
        .join(format!("{name}.json.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        actual == expected,
        format!(
            "golden mismatch {}\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ),
    )
}

fn transition(from: AutomatonState, to: AutomatonState, trigger: &str) -> AutomatonTransition {
    AutomatonTransition {
        from,
        to,
        trigger: trigger.to_string(),
        timestamp: "2026-05-01T00:00:00Z".to_string(),
        metadata: None,
    }
}

fn transition_json(transition: &AutomatonTransition) -> Value {
    json!({
        "from": transition.from.as_str(),
        "to": transition.to.as_str(),
        "trigger": transition.trigger,
        "timestamp": transition.timestamp,
        "metadata": transition.metadata,
    })
}

#[test]
fn lifecycle_automata_cover_gate13_terminal_scenarios() -> TestResult {
    let mut cancelled = ShutdownAutomatonCertificate::new("immediate", "operator_cancelled");
    cancelled.state = AutomatonState::Cancelled;
    cancelled.pending_operations = 2;
    cancelled.operations_cancelled = 2;
    cancelled.state_persisted = true;
    cancelled.transitions = vec![
        transition(AutomatonState::Idle, AutomatonState::Running, "start"),
        transition(AutomatonState::Running, AutomatonState::Cancelled, "cancel"),
    ];
    ensure(cancelled.state.is_terminal(), "cancellation is terminal")?;

    let mut failed_validation = ImportAutomatonCertificate::new("cass", "session.jsonl");
    failed_validation.state = AutomatonState::Failed;
    failed_validation.validation_passed = false;
    failed_validation.sessions_imported = 1;
    failed_validation.transitions = vec![
        transition(AutomatonState::Idle, AutomatonState::Running, "start"),
        transition(
            AutomatonState::Running,
            AutomatonState::Failed,
            "validation_failed",
        ),
    ];
    ensure(
        !failed_validation.is_successful(),
        "failed validation is not successful",
    )?;

    let mut interrupted_publish = IndexPublishAutomatonCertificate::new("hybrid");
    interrupted_publish.state = AutomatonState::Failed;
    interrupted_publish.db_generation_before = 9;
    interrupted_publish.db_generation_after = 10;
    interrupted_publish.consistency_check = false;
    interrupted_publish.transitions = vec![
        transition(AutomatonState::Idle, AutomatonState::Running, "publish"),
        transition(
            AutomatonState::Running,
            AutomatonState::Rollback,
            "interrupted",
        ),
        transition(
            AutomatonState::Rollback,
            AutomatonState::Failed,
            "rollback_done",
        ),
    ];
    ensure(
        interrupted_publish.state == AutomatonState::Failed,
        "interrupted publish failed closed",
    )?;

    let mut duplicate_apply = ImportAutomatonCertificate::new("manual", "note-001");
    duplicate_apply.state = AutomatonState::Failed;
    duplicate_apply.idempotency_fingerprint = Some("idem:note-001".to_string());
    duplicate_apply.transitions = vec![
        transition(AutomatonState::Idle, AutomatonState::Running, "apply"),
        transition(
            AutomatonState::Running,
            AutomatonState::Failed,
            "duplicate_apply",
        ),
    ];
    ensure(
        duplicate_apply.idempotency_fingerprint.is_some(),
        "duplicate apply carries idempotency fingerprint",
    )?;

    let mut normal_completion = BackupAutomatonCertificate::new("snapshot", ".ee/backup.tar");
    normal_completion.state = AutomatonState::Completed;
    normal_completion.files_count = 4;
    normal_completion.total_bytes = 2048;
    normal_completion.checksum = Some("blake3:abc123".to_string());
    normal_completion.verified = true;
    normal_completion.transitions = vec![
        transition(AutomatonState::Idle, AutomatonState::Running, "start"),
        transition(
            AutomatonState::Running,
            AutomatonState::Completed,
            "verified",
        ),
    ];
    ensure(
        normal_completion.is_verified(),
        "normal backup completion verified",
    )?;

    let value = json!({
        "schema": "ee.lifecycle.automaton.gate13.v1",
        "cases": [
            {
                "name": "cancellation",
                "state": cancelled.state.as_str(),
                "terminal": cancelled.state.is_terminal(),
                "operationsCancelled": cancelled.operations_cancelled,
                "transitions": cancelled.transitions.iter().map(transition_json).collect::<Vec<_>>(),
            },
            {
                "name": "failed_validation",
                "state": failed_validation.state.as_str(),
                "validationPassed": failed_validation.validation_passed,
                "successful": failed_validation.is_successful(),
                "transitions": failed_validation.transitions.iter().map(transition_json).collect::<Vec<_>>(),
            },
            {
                "name": "interrupted_publish",
                "state": interrupted_publish.state.as_str(),
                "consistencyCheck": interrupted_publish.consistency_check,
                "generationsMatch": interrupted_publish.generations_match(),
                "transitions": interrupted_publish.transitions.iter().map(transition_json).collect::<Vec<_>>(),
            },
            {
                "name": "duplicate_apply",
                "state": duplicate_apply.state.as_str(),
                "idempotencyFingerprint": duplicate_apply.idempotency_fingerprint,
                "transitions": duplicate_apply.transitions.iter().map(transition_json).collect::<Vec<_>>(),
            },
            {
                "name": "normal_completion",
                "state": normal_completion.state.as_str(),
                "verified": normal_completion.is_verified(),
                "checksum": normal_completion.checksum,
                "transitions": normal_completion.transitions.iter().map(transition_json).collect::<Vec<_>>(),
            }
        ],
    });
    let mut rendered = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    rendered.push('\n');
    assert_golden("lifecycle_automaton", &rendered)
}
