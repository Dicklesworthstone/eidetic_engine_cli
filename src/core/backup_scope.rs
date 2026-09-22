//! Workspace-local recovery from a shared database.
//!
//! Count the source obligations, not the rows a writer happened to emit. Only
//! durable required tables are scoped; cache/credential/unknown-table inventory
//! remains database-wide diagnostic information. Restore still reconciles the
//! entire isolated destination, so scope can never hide foreign restored rows.

use sqlmodel_core::Value;

use super::super::{BackupExportData, BackupRecoveryInventory};
use super::{REQUIRED_TABLES, recovery_error, storage_error};
use crate::db::DbConnection;
use crate::models::DomainError;

#[derive(Clone, Copy)]
enum Scope {
    Column(&'static str),
    WithUnscoped,
    Child {
        key: &'static str,
        parent: &'static str,
        parent_key: &'static str,
        unscoped_parent: bool,
    },
    MemoryLinks,
    Shared,
}

fn child(key: &'static str, parent: &'static str, parent_key: &'static str) -> Scope {
    Scope::Child {
        key,
        parent,
        parent_key,
        unscoped_parent: false,
    }
}

/// Binary-owned ownership rules, never archive-supplied SQL or a guessed column.
/// A newly required family without a rule fails closed even when it is empty.
fn scope(table: &str) -> Option<Scope> {
    Some(match table {
        "workspaces" => Scope::Column("id"),
        "situation_records" => Scope::Column("workspace_scope"),
        "audit_log" | "recorder_runs" | "task_episodes" => Scope::WithUnscoped,
        "curation_ttl_policies" => Scope::Shared,
        "memory_tags" | "memory_seals" | "memory_sentinel_specs" => {
            child("memory_id", "memories", "id")
        }
        "memory_links" => Scope::MemoryLinks,
        "rule_source_memories" | "rule_tags" => child("rule_id", "procedural_rules", "id"),
        "artifact_links" => child("artifact_id", "artifacts", "id"),
        "rationale_trace_links" => child("trace_id", "rationale_traces", "trace_id"),
        "pack_items" | "pack_evidence_items" | "pack_omissions" => {
            child("pack_id", "pack_records", "id")
        }
        "recorder_events" => Scope::Child {
            key: "run_id",
            parent: "recorder_runs",
            parent_key: "run_id",
            unscoped_parent: true,
        },
        "memories"
        | "attempt_families"
        | "attempt_family_members"
        | "journal_entries"
        | "search_index_jobs"
        | "rch_verify_runs"
        | "error_fingerprints"
        | "error_repair_links"
        | "artifacts"
        | "rationale_traces"
        | "causal_evidence"
        | "agents"
        | "certificates"
        | "trust_quarantine"
        | "procedural_rules"
        | "feedback_events"
        | "agent_context_profiles"
        | "debt_snapshots"
        | "reflection_request_ledger"
        | "tripwire_check_events"
        | "tripwires"
        | "evidence_spans"
        | "sessions"
        | "import_ledger"
        | "pack_baselines"
        | "pack_candidate_impressions"
        | "pack_records"
        | "curation_candidates"
        | "procedures"
        | "procedure_events"
        | "feedback_quarantine"
        | "learning_observations"
        | "outcome_evidence_rows"
        | "plan_recipes" => Scope::Column("workspace_id"),
        _ => return None,
    })
}

pub(in crate::core::backup) fn count_rows(
    db: &DbConnection,
    table: &str,
    workspace: &str,
) -> Result<u64, DomainError> {
    if !REQUIRED_TABLES.iter().any(|(name, _, _)| *name == table) {
        return u64::try_from(db.count_table_rows(table).map_err(storage_error)?)
            .map_err(storage_error);
    }
    let ownership = scope(table)
        .ok_or_else(|| recovery_error("Required backup table has no source ownership rule"))?;
    let predicate = match ownership {
        Scope::Shared => {
            return u64::try_from(db.count_table_rows(table).map_err(storage_error)?)
                .map_err(storage_error);
        }
        Scope::Column(column) => format!("\"{column}\" = ?1"),
        // Global audit and recorder history is intentionally carried by every
        // scoped backup. Unowned task episodes are counted too: until there is
        // a writer for them they must remain an explicit coverage gap, not be
        // relabelled another workspace's state and silently disappear.
        Scope::WithUnscoped => "workspace_id = ?1 OR workspace_id IS NULL".to_owned(),
        Scope::Child {
            key,
            parent,
            parent_key,
            unscoped_parent,
        } => {
            let extra = if unscoped_parent {
                " OR workspace_id IS NULL"
            } else {
                ""
            };
            format!(
                "\"{key}\" IN (SELECT \"{parent_key}\" FROM \"{parent}\" WHERE workspace_id = ?1{extra})"
            )
        }
        // A cross-workspace edge is still an obligation of either endpoint.
        // The portable writer cannot include the other workspace's memory;
        // reconcile_primary therefore marks it partial rather than declaring
        // success after silently discarding the edge or leaking its endpoint.
        Scope::MemoryLinks => {
            "src_memory_id IN (SELECT id FROM memories WHERE workspace_id = ?1) OR dst_memory_id IN (SELECT id FROM memories WHERE workspace_id = ?1)".to_owned()
        }
    };
    let rows = db
        .query(
            &format!("SELECT COUNT(*) FROM \"{table}\" WHERE {predicate}"),
            &[Value::Text(workspace.to_owned())],
        )
        .map_err(storage_error)?;
    if rows.len() != 1 {
        return Err(recovery_error(
            "Backup source count returned an invalid result",
        ));
    }
    rows[0]
        .get(0)
        .and_then(Value::as_i64)
        .and_then(|count| u64::try_from(count).ok())
        .ok_or_else(|| recovery_error("Backup source count is not a nonnegative integer"))
}

/// Primary JSONL carriers need the same source-versus-capture reconciliation
/// as history assets. In particular, policy-filtered and cross-workspace links
/// and unexported attempt-family slots cannot turn into a complete recovery.
pub(in crate::core::backup) fn reconcile_primary(
    inventory: &mut BackupRecoveryInventory,
    data: &BackupExportData,
) {
    let families: std::collections::BTreeSet<_> = data
        .attempt_families_by_memory
        .values()
        .map(|family| &family.family_id)
        .collect();
    for (table, captured) in [
        ("workspaces", 1),
        ("memories", data.memories.len()),
        (
            "memory_tags",
            data.tags_by_memory.values().map(Vec::len).sum(),
        ),
        ("memory_links", data.links.len()),
        ("attempt_families", families.len()),
        (
            "attempt_family_members",
            data.attempt_families_by_memory.len(),
        ),
    ] {
        if let Some(entry) = inventory.entries.iter_mut().find(|row| row.table == table) {
            entry.snapshot_covered = u64::try_from(captured).ok() == Some(entry.row_count);
        }
    }
}

#[cfg(test)]
#[path = "backup_scope_tests.rs"]
mod tests;
