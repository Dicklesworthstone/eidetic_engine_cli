//! Shared builder for a `ee.agent_mail.snapshot.v1` fixture that PASSES
//! `validate_declared_agent_mail_snapshot_v1` (src/core/swarm_brief.rs:8152).
//!
//! Why this exists as a helper rather than a literal in each test: the schema
//! carries sixteen required root fields plus cross-field invariants that
//! contradict each other if hand-maintained in two places. Hand-copying it is
//! exactly how the next drift happens.
//!
//! Why a declaration is REQUIRED rather than optional: an undeclared snapshot
//! does not error, but `agent_mail_snapshot_freshness_assessment`
//! (swarm_brief.rs:8034) returns `freshness::unknown()` plus an
//! `AGENT_MAIL_UNAVAILABLE_CODE` warning for it, which surfaces as
//! `agentMailAvailable: false` and leaves `blockedByCoordination` empty. Only a
//! declared v1 snapshot with a fresh `generated_at` reaches the blocking path.
//!
//! Every invariant encoded below is cited to the validator line that enforces
//! it, so a future reader can re-derive the shape instead of guessing.

use std::path::Path;

use chrono::Utc;
use serde_json::{Value, json};

/// One file reservation row. `exclusive` is what makes the path blocking.
pub struct ReservationFixture<'a> {
    pub path_pattern: &'a str,
    pub holder: &'a str,
    pub exclusive: bool,
}

/// The `project_key` binding: `sha256:` + SHA-256 of the CANONICALIZED
/// workspace path (swarm_brief.rs:7993-8007). The validator canonicalizes the
/// workspace it was given before comparing, so the fixture must canonicalize
/// too or the digests differ and every other field is wasted.
pub fn project_key_for_workspace(workspace: &Path) -> Result<String, String> {
    let canonical = std::fs::canonicalize(workspace)
        .map_err(|error| format!("canonicalize fixture workspace: {error}"))?;
    let identity = canonical
        .to_str()
        .ok_or_else(|| "fixture workspace path is not UTF-8".to_owned())?;
    Ok(format!(
        "sha256:{}",
        ee::models::release::sha256_hex(identity.as_bytes())
    ))
}

/// Build the snapshot JSON.
///
/// Posture is all-green: six successful source commands, so `degraded` is
/// empty, `fallback_active` is false and `producer_status` is `"ok"`
/// (swarm_brief.rs:8378-8384 ties those three together).
pub fn declared_snapshot_v1(
    workspace: &Path,
    agent_name: &str,
    reservations: &[ReservationFixture<'_>],
) -> Result<String, String> {
    // `agent_mail_snapshot_v1_shell_quote` leaves shell-safe names bare, so an
    // alphanumeric agent name appears UNQUOTED in commands 2 and 3.
    assert!(
        agent_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "fixture agent name must be shell-safe so the command spelling stays unquoted"
    );

    // The first four commands must share one prefix and match the exact
    // per-index spelling in `agent_mail_snapshot_v1_cli_command_prefix`
    // (swarm_brief.rs:8126-8150); commands 4 and 5 are compared literally
    // (swarm_brief.rs:8308-8310).
    let prefix = "am";
    let source_commands = vec![
        format!("{prefix} agents list --project '<workspace>' --json"),
        format!("{prefix} robot reservations --project '<workspace>' --all --format json"),
        format!(
            "{prefix} mail inbox --project '<workspace>' --agent {agent_name} --limit 50 --json"
        ),
        format!("{prefix} status --project '<workspace>' --agent {agent_name} --json"),
        "agent-mail-health http://127.0.0.1:8765/health".to_owned(),
        "agent-mail-health http://127.0.0.1:8765/health/durability".to_owned(),
    ];

    // Index-matched statuses. A successful CLI command exits 0; a successful
    // health probe exits 200 (swarm_brief.rs:8336-8341).
    let command_statuses = source_commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            json!({
                "command": command,
                "ok": true,
                "exit_code": if index < 4 { 0 } else { 200 },
                "timed_out": false,
                "error_class": Value::Null,
            })
        })
        .collect::<Vec<_>>();

    let reservation_rows = reservations
        .iter()
        .map(|reservation| {
            json!({
                "path_pattern": reservation.path_pattern,
                "holder": reservation.holder,
                "exclusive": reservation.exclusive,
            })
        })
        .collect::<Vec<_>>();

    let agent_rows = vec![json!({ "name": agent_name })];

    // A successful status probe (command 3) must carry EXACTLY its own agent
    // mailbox — an empty inbox is rejected (swarm_brief.rs:8399-8405).
    let inbox_rows = vec![json!({
        "mailbox": agent_name,
        "unread_count": 0,
        "ack_required_count": 0,
    })];
    let thread_rows: Vec<Value> = Vec::new();

    // `health_level`, `semantic_readiness`, `durability_state` and `recovery`
    // are omitted rather than set to null: they are optional, but an EXPLICIT
    // null is rejected (swarm_brief.rs:8195-8204). Any of them being present
    // and non-green would also force `fallback_active`.
    let snapshot = json!({
        "schema": "ee.agent_mail.snapshot.v1",
        "generated_at": Utc::now().to_rfc3339(),
        "project_key": project_key_for_workspace(workspace)?,
        "agent_name": agent_name,
        "redaction_status": "paths_counts_subjects_only_no_content",
        "producer_status": "ok",
        "source_commands": source_commands,
        "command_statuses": command_statuses,
        "fallback_active": false,
        "am_agents_list_ok": true,
        "summary": {
            "agent_count": agent_rows.len(),
            "file_reservation_count": reservation_rows.len(),
            "inbox_mailbox_count": inbox_rows.len(),
            "thread_count": thread_rows.len(),
            "source_command_count": 6,
            "degraded_count": 0,
        },
        "degraded": Vec::<Value>::new(),
        "file_reservations": reservation_rows,
        "agents": agent_rows,
        "inbox": inbox_rows,
        "threads": thread_rows,
    });

    serde_json::to_string_pretty(&snapshot)
        .map_err(|error| format!("serialize agent mail snapshot fixture: {error}"))
}
