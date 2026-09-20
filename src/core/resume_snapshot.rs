//! A resume bundle is one source snapshot, not a sequence of current reads.
//!
//! Session tags, open-loop labels and canonical decision fields must agree with
//! the admitted memory bodies. A concurrent writer may commit while we read,
//! but its changes belong to the next resume, never half of this one.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};

use super::{
    RESUME_STORAGE_PAGE_SIZE, ResumeAdmissionBoundary, ResumeOptions, load_decision_typed_fields,
};
use crate::db::{DbConnection, StoredMemory};
use crate::models::DomainError;

pub(super) struct ResumeState {
    pub(super) workspace_id: String,
    pub(super) all_live: Vec<StoredMemory>,
    pub(super) tags: BTreeMap<String, Vec<String>>,
    pub(super) typed_decision_fields: BTreeMap<String, String>,
}

pub(super) fn load(
    connection: &DbConnection,
    options: &ResumeOptions<'_>,
    canonical_workspace: &Path,
    now: DateTime<Utc>,
) -> Result<ResumeState, DomainError> {
    load_with_boundary(connection, options, canonical_workspace, now, || Ok(()))
}

// The boundary permits deterministic real-writer interleavings in tests,
// following ask's corpus loader. Production supplies a no-op, not a mock store.
fn load_with_boundary(
    connection: &DbConnection,
    options: &ResumeOptions<'_>,
    canonical_workspace: &Path,
    now: DateTime<Utc>,
    after_memories: impl FnOnce() -> Result<(), DomainError>,
) -> Result<ResumeState, DomainError> {
    let snapshot = ResumeReadSnapshot::begin(connection)?;
    if connection
        .needs_migration()
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to inspect addressed workspace schema: {error}"),
            repair: Some("ee doctor --workspace . --json".to_owned()),
        })?
    {
        return Err(DomainError::MigrationRequired {
            message: "The addressed workspace database requires migration before resume."
                .to_owned(),
            repair: Some("ee migrate run --workspace . --json".to_owned()),
        });
    }
    let workspace_id = crate::core::workspace::addressed_workspace_row(
        connection,
        options.workspace_path,
        options.database_path,
    )?
    .map_or_else(
        || crate::core::workspace::stable_workspace_id(canonical_workspace),
        |row| row.id,
    );
    let current_memories = connection
        .list_recent_current_memories_for_retrieval(
            &workspace_id,
            // The storage query derives its own canonical validity bound.
            // Keep fractional precision for created_at/updated_at: a fresh
            // row must not disappear for the rest of the current second.
            &crate::core::memory::normalize_row_timestamp(now),
            u32::MAX,
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list current resume memories: {error}"),
            repair: Some("ee doctor --workspace . --json".to_owned()),
        })?;
    after_memories()?;
    let ids: Vec<&str> = current_memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect();
    let mut tags = BTreeMap::new();
    for page in ids.chunks(RESUME_STORAGE_PAGE_SIZE) {
        let page_tags = connection
            .get_memory_tags_batch(page)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to load memory tags required for resume session grouping, open-loop detection, and staleness: {error}"
                ),
                repair: Some(
                    "Run `ee doctor --workspace . --json`, repair the reported storage failure, then retry `ee resume`."
                        .to_owned(),
                ),
            })?;
        tags.extend(page_tags);
    }

    // Apply the ordinary workspace-scope and public-content admission rules
    // only after the one batched tag read. The exact sealed placeholder and
    // secret-bearing bodies fail closed; tags and provenance remain eligible
    // for field-level public redaction during projection. This deliberately
    // performs no per-memory storage lookup.
    let admission =
        ResumeAdmissionBoundary::for_bound_workspace(canonical_workspace, workspace_id.clone());
    let all_live: Vec<StoredMemory> = current_memories
        .into_iter()
        .filter_map(|memory| {
            let memory_tags = tags.get(&memory.id).map(Vec::as_slice).unwrap_or_default();
            admission.admit(memory, memory_tags)
        })
        .collect();

    let typed_decision_fields = load_decision_typed_fields(connection, &all_live)?;
    snapshot.finish()?;
    Ok(ResumeState {
        workspace_id,
        all_live,
        tags,
        typed_decision_fields,
    })
}

/// Own only a transaction whose BEGIN succeeded. A nested begin must not
/// release someone else's transaction; errors and unwinding release ours.
struct ResumeReadSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl<'a> ResumeReadSnapshot<'a> {
    fn begin(connection: &'a DbConnection) -> Result<Self, DomainError> {
        connection
            .begin_read_snapshot()
            .map_err(|_| snapshot_error("begin"))?;
        Ok(Self {
            connection,
            active: true,
        })
    }

    fn finish(mut self) -> Result<(), DomainError> {
        self.connection
            .commit_read_snapshot()
            .map_err(|_| snapshot_error("finish"))?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ResumeReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            tracing::error!(
                target: "ee::core::resume::snapshot",
                "failed to release resume read snapshot"
            );
        }
    }
}

fn snapshot_error(stage: &str) -> DomainError {
    DomainError::Storage {
        message: format!("Could not {stage} a coherent resume snapshot; bundle withheld"),
        repair: Some(
            "Retry ee resume; use ee doctor --workspace . --json if the failure persists."
                .to_owned(),
        ),
    }
}

#[cfg(test)]
#[path = "resume_snapshot_tests.rs"]
mod tests;
