//! bd-7lvbg.6 — per-agent pack-baseline ledger unit contracts.
//!
//! The ledger backs `ee pack --since last`: every persisted pack records a
//! (agent, task-key) baseline row, and resolution picks the most recent
//! baseline for the exact task key, falling back to the agent's most
//! recent baseline of any key. These tests pin the storage-layer contract:
//!
//! 1. **Resolution determinism** — empty ledger resolves to None;
//!    per-agent isolation; exact task-key rows win over any-key rows;
//!    the any-key fallback fires only when no exact match exists.
//! 2. **Eviction bounds** — rows past the per-agent cap are evicted
//!    oldest-first inside the insert transaction, with one
//!    `pack.baseline_evicted` audit row carrying the evicted pack ids
//!    (the ledger never shrinks silently).
//! 3. **Idempotence** — re-recording the same (agent, task key, pack)
//!    replaces rather than duplicates.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ee::db::{CreatePackBaselineInput, CreatePackRecordInput, DbConnection, audit_actions};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition { Ok(()) } else { Err(message.into()) }
}

/// `ee init` a temp workspace (runs migrations incl. V078) and return the
/// opened connection plus the registered workspace id.
fn initialized_connection(workspace: &Path) -> Result<(DbConnection, String), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("init")
        .arg("--json")
        .output()
        .map_err(|error| format!("failed to run ee init: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ee init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let database = workspace.join(".ee").join("ee.db");
    let connection =
        DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    let workspaces = connection
        .list_workspaces()
        .map_err(|error| format!("list workspaces: {error}"))?;
    let workspace_id = workspaces
        .first()
        .map(|workspace| workspace.id.clone())
        .ok_or("init must register a workspace")?;
    Ok((connection, workspace_id))
}

fn temp_workspace(label: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!(
        "ee-baseline-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).map_err(|error| format!("create workspace: {error}"))?;
    Ok(dir)
}

/// Insert a minimal persisted pack record so baseline FK targets exist.
fn seed_pack_record(
    connection: &DbConnection,
    workspace_id: &str,
    pack_id: &str,
    pack_hash: &str,
) -> TestResult {
    let input = CreatePackRecordInput {
        workspace_id: workspace_id.to_string(),
        query: "baseline ledger seed".to_string(),
        profile: "balanced".to_string(),
        max_tokens: 1000,
        used_tokens: 10,
        item_count: 0,
        omitted_count: 0,
        pack_hash: pack_hash.to_string(),
        degraded_json: None,
        created_by: Some("baseline-test".to_string()),
    };
    connection
        .insert_pack_record_with_timings_and_task_lens(pack_id, &input, &[], &[], None)
        .map_err(|error| format!("seed pack record {pack_id}: {error}"))?;
    Ok(())
}

fn record_baseline(
    connection: &DbConnection,
    workspace_id: &str,
    agent: &str,
    task_key: Option<&str>,
    pack_id: &str,
    cap: u32,
) -> Result<u32, String> {
    connection
        .insert_pack_baseline(
            &CreatePackBaselineInput {
                workspace_id: workspace_id.to_string(),
                agent_name: agent.to_string(),
                task_key: task_key.map(str::to_owned),
                pack_id: pack_id.to_string(),
                pack_hash: format!("hash-{pack_id}"),
            },
            cap,
            Some("baseline-test"),
        )
        .map_err(|error| format!("insert baseline {pack_id}: {error}"))
}

#[test]
fn empty_ledger_resolves_to_none() -> TestResult {
    let workspace = temp_workspace("empty")?;
    let (connection, workspace_id) = initialized_connection(&workspace)?;
    let resolved = connection
        .resolve_pack_baseline(&workspace_id, "AgentA", None)
        .map_err(|error| error.to_string())?;
    ensure(resolved.is_none(), "empty ledger must resolve to None")
}

#[test]
fn resolution_is_per_agent_and_prefers_exact_task_key() -> TestResult {
    let workspace = temp_workspace("resolve")?;
    let (connection, workspace_id) = initialized_connection(&workspace)?;
    for (pack_id, hash) in [
        ("pack_a1", "h1"),
        ("pack_a2", "h2"),
        ("pack_b1", "h3"),
        ("pack_a3", "h4"),
    ] {
        seed_pack_record(&connection, &workspace_id, pack_id, hash)?;
    }

    // AgentA: an any-task baseline, then a task-scoped one, then a newer
    // any-task one. AgentB gets its own row.
    record_baseline(&connection, &workspace_id, "AgentA", None, "pack_a1", 32)?;
    record_baseline(
        &connection,
        &workspace_id,
        "AgentA",
        Some("release"),
        "pack_a2",
        32,
    )?;
    record_baseline(&connection, &workspace_id, "AgentB", None, "pack_b1", 32)?;
    record_baseline(&connection, &workspace_id, "AgentA", None, "pack_a3", 32)?;

    let exact = connection
        .resolve_pack_baseline(&workspace_id, "AgentA", Some("release"))
        .map_err(|error| error.to_string())?
        .ok_or("exact task-key baseline must resolve")?;
    ensure(
        exact.pack_id == "pack_a2",
        format!("exact task key must win, got {}", exact.pack_id),
    )?;

    let any = connection
        .resolve_pack_baseline(&workspace_id, "AgentA", Some("unrelated-task"))
        .map_err(|error| error.to_string())?
        .ok_or("any-key fallback must resolve")?;
    ensure(
        any.pack_id == "pack_a3",
        format!(
            "unknown task key must fall back to the newest any-key row, got {}",
            any.pack_id
        ),
    )?;

    let no_key = connection
        .resolve_pack_baseline(&workspace_id, "AgentA", None)
        .map_err(|error| error.to_string())?
        .ok_or("agent baseline must resolve")?;
    ensure(
        no_key.pack_id == "pack_a3",
        format!("no task key resolves the newest row, got {}", no_key.pack_id),
    )?;

    let other = connection
        .resolve_pack_baseline(&workspace_id, "AgentB", None)
        .map_err(|error| error.to_string())?
        .ok_or("AgentB baseline must resolve")?;
    ensure(
        other.pack_id == "pack_b1",
        format!("agents must not share baselines, got {}", other.pack_id),
    )?;

    let stranger = connection
        .resolve_pack_baseline(&workspace_id, "AgentC", None)
        .map_err(|error| error.to_string())?;
    ensure(
        stranger.is_none(),
        "an agent with no rows must resolve to None",
    )
}

#[test]
fn eviction_caps_rows_oldest_first_with_audit() -> TestResult {
    let workspace = temp_workspace("evict")?;
    let (connection, workspace_id) = initialized_connection(&workspace)?;
    const CAP: u32 = 3;
    let pack_ids = ["pack_e1", "pack_e2", "pack_e3", "pack_e4", "pack_e5"];
    for pack_id in pack_ids {
        seed_pack_record(&connection, &workspace_id, pack_id, pack_id)?;
    }

    let mut total_evicted = 0u32;
    for pack_id in pack_ids {
        total_evicted +=
            record_baseline(&connection, &workspace_id, "AgentE", None, pack_id, CAP)?;
    }
    ensure(
        total_evicted == 2,
        format!("5 inserts at cap 3 must evict 2, got {total_evicted}"),
    )?;

    let rows = connection
        .list_pack_baselines(&workspace_id, "AgentE")
        .map_err(|error| error.to_string())?;
    ensure(
        rows.len() == CAP as usize,
        format!("ledger must hold exactly {CAP} rows, got {}", rows.len()),
    )?;
    let kept: Vec<&str> = rows.iter().map(|row| row.pack_id.as_str()).collect();
    ensure(
        !kept.contains(&"pack_e1") && !kept.contains(&"pack_e2"),
        format!("oldest rows must be evicted first, kept {kept:?}"),
    )?;

    let resolved = connection
        .resolve_pack_baseline(&workspace_id, "AgentE", None)
        .map_err(|error| error.to_string())?
        .ok_or("capped ledger must still resolve")?;
    ensure(
        resolved.pack_id == "pack_e5",
        format!("newest row must resolve, got {}", resolved.pack_id),
    )?;

    let audits = connection
        .list_audit_by_action(audit_actions::PACK_BASELINE_EVICTED, None)
        .map_err(|error| error.to_string())?;
    ensure(
        audits.len() == 2,
        format!("each eviction batch must audit once, got {}", audits.len()),
    )?;
    let details = audits
        .first()
        .and_then(|row| row.details.clone())
        .ok_or("eviction audit must carry details")?;
    ensure(
        details.contains("evictedPackIds") && details.contains("pack_e"),
        format!("eviction audit must name the evicted packs, got {details}"),
    )
}

#[test]
fn rerecording_the_same_pack_is_idempotent() -> TestResult {
    let workspace = temp_workspace("idem")?;
    let (connection, workspace_id) = initialized_connection(&workspace)?;
    seed_pack_record(&connection, &workspace_id, "pack_i1", "h1")?;

    record_baseline(&connection, &workspace_id, "AgentI", None, "pack_i1", 32)?;
    record_baseline(&connection, &workspace_id, "AgentI", None, "pack_i1", 32)?;

    let rows = connection
        .list_pack_baselines(&workspace_id, "AgentI")
        .map_err(|error| error.to_string())?;
    ensure(
        rows.len() == 1,
        format!("re-recording the same pack must replace, got {} rows", rows.len()),
    )
}
