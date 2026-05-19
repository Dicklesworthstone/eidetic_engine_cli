//! bd-1eq3l.8 - Workspace hygiene classifier and coordination matrix coverage.
//!
//! These tests pin the high-risk matrix that is easy to regress without
//! noticing from the public e2e goldens alone: local config precedence,
//! secret-risk safety overrides, deterministic classifier ordering, and
//! Agent Mail reservation overlay edge cases.

use chrono::{TimeZone, Utc};
use ee::core::hygiene_classifier::{
    Bucket, ClassificationRow, HygieneClassifierConfig, HygienePathPattern, Kind,
    SecretEvidenceLookup, classify_workspace_with_config, reason,
};
use ee::core::hygiene_coordination::{
    ActiveAgent, AgentMailCoordinationInput, AgentMailReservation, overlay_coordination_state,
    reason as coordination_reason,
};
use ee::core::swarm_brief::{
    WorkspaceGitOperationState, WorkspaceGitSnapshot, WorkspaceGitStatusEntry,
};

type TestResult = Result<(), String>;

fn entry(path: &str, staged: &str, unstaged: &str, entry_kind: &str) -> WorkspaceGitStatusEntry {
    WorkspaceGitStatusEntry {
        path: path.to_owned(),
        original_path: None,
        staged: staged.to_owned(),
        unstaged: unstaged.to_owned(),
        entry_kind: entry_kind.to_owned(),
        submodule_state: None,
        metadata: None,
    }
}

fn untracked(path: &str) -> WorkspaceGitStatusEntry {
    entry(path, "?", "?", "untracked")
}

fn modified(path: &str) -> WorkspaceGitStatusEntry {
    entry(path, ".", "M", "ordinary")
}

fn snapshot(entries: Vec<WorkspaceGitStatusEntry>) -> WorkspaceGitSnapshot {
    WorkspaceGitSnapshot {
        repository_root: "/tmp/workspace-hygiene-contract".to_owned(),
        entries,
        operation_state: WorkspaceGitOperationState::default(),
    }
}

fn no_secret_evidence() -> SecretEvidenceLookup {
    SecretEvidenceLookup::default()
}

fn configured_matrix() -> HygieneClassifierConfig {
    HygieneClassifierConfig {
        generated_patterns: vec![
            HygienePathPattern::suffix(".env"),
            HygienePathPattern::suffix(".pb.rs"),
        ],
        scratch_patterns: vec![
            HygienePathPattern::exact(".env"),
            HygienePathPattern::suffix(".scratch.json"),
        ],
        local_machine_patterns: vec![HygienePathPattern::exact(".local-agent-state")],
        always_review_patterns: vec![HygienePathPattern::prefix("src/generated/")],
    }
}

fn row_for<'a>(rows: &'a [ClassificationRow], path: &str) -> Result<&'a ClassificationRow, String> {
    rows.iter()
        .find(|row| row.path == path)
        .ok_or_else(|| format!("missing row for {path}; rows: {rows:#?}"))
}

fn assert_row(
    rows: &[ClassificationRow],
    path: &str,
    bucket: Bucket,
    kind: Kind,
    primary_reason: &'static str,
    suggested_group: Option<&str>,
) -> TestResult {
    let row = row_for(rows, path)?;
    assert_eq!(row.bucket, bucket, "{path} bucket");
    assert_eq!(row.kind, kind, "{path} kind");
    assert_eq!(
        row.reasons.first().copied(),
        Some(primary_reason),
        "{path} primary reason"
    );
    assert_eq!(
        row.suggested_group.as_deref(),
        suggested_group,
        "{path} suggested group"
    );
    Ok(())
}

fn classifier_signature(rows: &[ClassificationRow]) -> Vec<(String, &'static str, &'static str)> {
    rows.iter()
        .map(|row| (row.path.clone(), row.bucket.as_str(), row.kind.as_str()))
        .collect()
}

fn reservation(
    path_pattern: &str,
    holder_agent: &str,
    exclusive: bool,
    expires_at: Option<&str>,
    reservation_id: &str,
) -> AgentMailReservation {
    AgentMailReservation {
        path_pattern: path_pattern.to_owned(),
        holder_agent: holder_agent.to_owned(),
        exclusive,
        expires_at: expires_at.map(str::to_owned),
        reservation_id: Some(reservation_id.to_owned()),
        bead_id: Some("bd-1eq3l.8".to_owned()),
        thread_id: Some("bd-1eq3l.8".to_owned()),
    }
}

#[test]
fn config_precedence_keeps_secret_and_review_safety_overrides() -> TestResult {
    let rows = classify_workspace_with_config(
        &snapshot(vec![
            untracked(".env"),
            modified("src/generated/client.pb.rs"),
            modified(".local-agent-state"),
            modified("reports/build.scratch.json"),
        ]),
        &no_secret_evidence(),
        &configured_matrix(),
    );

    assert_row(
        &rows,
        ".env",
        Bucket::DoNotCommit,
        Kind::SecretRisk,
        reason::SECRET_PATH_PATTERN,
        Some("secret_risk"),
    )?;
    assert_row(
        &rows,
        "src/generated/client.pb.rs",
        Bucket::NeedsHumanReview,
        Kind::Unknown,
        reason::CONFIG_ALWAYS_REVIEW_PATTERN,
        Some("human_review"),
    )?;
    assert_row(
        &rows,
        ".local-agent-state",
        Bucket::DoNotCommit,
        Kind::LocalMachine,
        reason::CONFIG_LOCAL_MACHINE_PATTERN,
        Some("local_machine"),
    )?;
    assert_row(
        &rows,
        "reports/build.scratch.json",
        Bucket::DoNotCommit,
        Kind::Scratch,
        reason::CONFIG_SCRATCH_PATTERN,
        Some("scratch"),
    )?;

    let env_row = row_for(&rows, ".env")?;
    assert!(
        !env_row
            .reasons
            .iter()
            .any(|code| code.starts_with("config_")),
        "secret-risk path must not be weakened by config pattern matches: {env_row:#?}"
    );
    Ok(())
}

#[test]
fn classifier_order_is_stable_for_shuffled_dirty_paths() {
    let entries = vec![
        modified("src/generated/client.pb.rs"),
        untracked(".env"),
        modified("reports/build.scratch.json"),
        modified(".local-agent-state"),
        modified("tests/workspace_hygiene.rs"),
    ];
    let mut shuffled = entries.clone();
    shuffled.reverse();

    let forward = classify_workspace_with_config(
        &snapshot(entries),
        &no_secret_evidence(),
        &configured_matrix(),
    );
    let reversed = classify_workspace_with_config(
        &snapshot(shuffled),
        &no_secret_evidence(),
        &configured_matrix(),
    );

    assert_eq!(
        classifier_signature(&forward),
        classifier_signature(&reversed),
        "classifier output ordering must not depend on git porcelain input order"
    );
}

#[test]
fn coordination_overlay_sorts_and_partitions_reservation_edge_cases() -> TestResult {
    let rows = classify_workspace_with_config(
        &snapshot(vec![
            modified("tests/a.rs"),
            modified("src/b.rs"),
            modified("src/a.rs"),
        ]),
        &no_secret_evidence(),
        &HygieneClassifierConfig::default(),
    );
    let now = Utc
        .with_ymd_and_hms(2026, 5, 19, 22, 0, 0)
        .single()
        .ok_or_else(|| "invalid fixed UTC timestamp".to_owned())?;
    let input = AgentMailCoordinationInput::Available {
        reservations: vec![
            reservation(
                "src/a.rs",
                "LavenderHollow",
                true,
                Some("2099-01-01T00:00:00Z"),
                "self-1",
            ),
            reservation(
                "src/a.rs",
                "ExpiredAgent",
                true,
                Some("2026-05-19T21:59:59Z"),
                "expired-1",
            ),
            reservation(
                "tests/*.rs",
                "BetaAgent",
                true,
                Some("2099-01-01T00:00:00Z"),
                "block-tests",
            ),
            reservation(
                "src/*.rs",
                "SharedAgent",
                false,
                Some("2099-01-01T00:00:00Z"),
                "shared-src",
            ),
            reservation(
                "src/b.rs",
                "OtherAgent",
                true,
                Some("2099-01-01T00:00:00Z"),
                "block-src-b",
            ),
        ],
        active_agents: vec![
            ActiveAgent {
                name: "ZuluAgent".to_owned(),
                last_active_at: Some("2026-05-19T21:00:00Z".to_owned()),
            },
            ActiveAgent {
                name: "AlphaAgent".to_owned(),
                last_active_at: None,
            },
        ],
    };

    let overlay = overlay_coordination_state(&rows, &input, now, Some("LavenderHollow"));

    assert!(overlay.agent_mail_available);
    assert_eq!(overlay.reservation_count, 5);
    assert!(overlay.degraded_codes.is_empty());
    assert_eq!(
        overlay
            .active_agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect::<Vec<_>>(),
        ["AlphaAgent", "ZuluAgent"],
        "active agents must be sorted deterministically"
    );
    assert_eq!(
        overlay
            .blocked_by_coordination
            .iter()
            .map(|blocked| (blocked.path.as_str(), blocked.holder_agent.as_str()))
            .collect::<Vec<_>>(),
        [("src/b.rs", "OtherAgent"), ("tests/a.rs", "BetaAgent")],
        "only active exclusive reservations held by other agents should block paths"
    );
    assert_eq!(
        overlay.blocked_by_coordination[0].reasons,
        vec![coordination_reason::ACTIVE_EXCLUSIVE_RESERVATION]
    );
    assert_eq!(
        overlay
            .observed_shared_reservations
            .iter()
            .map(|observed| (observed.path.as_str(), observed.holder_agent.as_str()))
            .collect::<Vec<_>>(),
        [("src/a.rs", "SharedAgent"), ("src/b.rs", "SharedAgent")],
        "shared reservations should be reported but not blocking"
    );
    assert_eq!(
        overlay
            .ignored_reservations
            .iter()
            .map(|ignored| {
                (
                    ignored.path.as_str(),
                    ignored.holder_agent.as_str(),
                    ignored.reasons[0],
                )
            })
            .collect::<Vec<_>>(),
        [
            (
                "src/a.rs",
                "ExpiredAgent",
                coordination_reason::EXPIRED_RESERVATION_IGNORED,
            ),
            (
                "src/a.rs",
                "LavenderHollow",
                coordination_reason::SELF_RESERVATION_IGNORED,
            ),
        ],
        "expired and self-held reservations should be ignored with auditable reasons"
    );
    Ok(())
}
