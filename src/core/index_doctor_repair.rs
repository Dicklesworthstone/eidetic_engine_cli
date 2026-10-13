//! Build a complete repair generation without writing to the source store.
//!
//! Doctor owns publication and its inverse journal. This adapter deliberately
//! omits index-job, advisory-SQL-lock, model-registry and anchor backfill writes;
//! it reuses the canonical source projector, builders and admission checks.

use std::path::{Path, PathBuf};

use super::{
    DbConnection, IndexGenerationLease, IndexRebuildError, build_index_generation,
    cached_local_selection, collect_workspace_index_source_snapshot, default_embedder_settings,
    default_workspace_database_path, embedder_fingerprint_for_index_metadata,
    ensure_index_path_has_no_symlinks, hash_fallback_embedder_stack, index_checkpoint,
    resolve_index_workspace_id, sync_index_directory, sync_index_generation,
    validate_built_generation, write_index_metadata,
};
use crate::core::remote_embed::{EmbedBackendSelection, configured_embed_backend};

pub(crate) struct PreparedRepair {
    database_path: PathBuf,
    workspace_id: String,
    generation: u64,
    document_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RepairSourceGeneration {
    pub(crate) published: u64,
    pub(crate) current: u64,
}

impl PreparedRepair {
    pub(crate) fn published_generation(&self) -> u64 {
        self.generation
    }

    /// Observe freshness without treating a completed publication as failed.
    /// Callers before publication must still require equality; after publication
    /// a mismatch describes a valid replacement that already needs refreshing.
    pub(crate) fn source_generation(&self) -> Result<RepairSourceGeneration, IndexRebuildError> {
        let db = DbConnection::open_file_read_only(&self.database_path)?;
        let current = db
            .get_workspace_generation(&self.workspace_id)?
            .unwrap_or_else(|| u64::from(self.document_count));
        Ok(RepairSourceGeneration {
            published: self.generation,
            current,
        })
    }

    /// A concurrent source change must not be overwritten by an older repair.
    /// Called under the generation publication lease, immediately before the
    /// first live-file mutation. Later source writes naturally make this
    /// captured generation stale, just as they do for an ordinary rebuild.
    pub(crate) fn check_source_generation(&self) -> Result<(), IndexRebuildError> {
        let observed = self.source_generation()?;
        if observed.current != observed.published {
            return Err(IndexRebuildError::Index(
                "source changed while doctor rebuilt the index; retry after active writes finish"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) async fn stage(
    cx: &asupersync::Cx,
    workspace: &Path,
    staging: &Path,
) -> Result<PreparedRepair, IndexRebuildError> {
    stage_with_selection(cx, workspace, staging, |db, workspace_id| {
        // A doctor repair never fetches (bd-65jem). Reuse a verified cached
        // model or the deterministic hash tier without registering a model.
        Ok(cached_local_selection(
            Some((db, workspace_id)),
            &default_embedder_settings(),
            configured_embed_backend() == EmbedBackendSelection::Remote,
        )?
        .stack)
    })
    .await
}

async fn stage_with_selection(
    cx: &asupersync::Cx,
    workspace: &Path,
    staging: &Path,
    select: impl FnOnce(&DbConnection, &str) -> Result<super::EmbedderStack, IndexRebuildError>,
) -> Result<PreparedRepair, IndexRebuildError> {
    index_checkpoint(cx)?;
    ensure_index_path_has_no_symlinks(staging, "stage doctor index repair")?;
    if staging
        .try_exists()
        .map_err(|error| IndexRebuildError::Index(error.to_string()))?
    {
        return Err(IndexRebuildError::Index(
            "doctor index staging directory already exists".to_owned(),
        ));
    }
    let database_path = default_workspace_database_path(workspace);
    let db = DbConnection::open_file_read_only(&database_path)?;
    let workspace_id = resolve_index_workspace_id(&db, workspace)?;
    let captured = collect_workspace_index_source_snapshot(&db, &workspace_id);
    // A source read may report cancellation as a database error. Check the
    // request after capture has released its snapshot, before loading a model.
    index_checkpoint(cx)?;
    let source = captured?;
    // An empty repair must not load or download a model to embed nothing, and
    // must not REGISTER one either.
    //
    // bd-71a77: this branch used to fall back to the hash tier only when
    // DEFAULT_SEARCH_EMBEDDER was unset, and otherwise reuse whatever model the
    // PROCESS had already loaded. That satisfied the no-fetch rule above --
    // reusing a loaded model downloads nothing -- while still writing an
    // `Available` model-registry row for a workspace that never embedded
    // anything with it. In a long-lived process (the daemon, the lib test
    // binary) that leaks one workspace's model into another's registry, which
    // is order-dependent contamination rather than a stable wrong answer.
    //
    // Ruling R1 on bd-2q8hn: the zero-document path ALWAYS takes the hash tier,
    // as rebuild_index_with_cx already does (src/core/index.rs:1776). Nothing is
    // embedded here, so there is nothing to prove and no model to record.
    let stack = if source.documents_total == 0 {
        hash_fallback_embedder_stack()
    } else {
        let selected = select(&db, &workspace_id);
        index_checkpoint(cx)?;
        selected?
    };
    // Doctor must emit the same reconciliation state as an ordinary full
    // build. Without it, the next write cannot plan a delta and re-embeds the
    // entire repaired corpus. Bind only this captured source and its actual
    // producer, never digests copied from the damaged generation.
    let digests = super::delta::DocDigests::new(&stack, &source.documents);
    index_checkpoint(cx)?;
    let fingerprint = embedder_fingerprint_for_index_metadata(&stack);
    let stats = build_index_generation(cx, staging, stack, source.documents).await?;
    validate_built_generation(staging, stats, source.document_counts)
        .map_err(IndexRebuildError::Index)?;
    index_checkpoint(cx)?;
    // The admission marker is written only after digests exist, and the
    // existing durability barrier flushes both before doctor can publish them.
    digests.write(staging)?;
    write_index_metadata(
        staging,
        source.generation,
        source.document_counts,
        fingerprint.as_ref(),
    )?;
    sync_index_generation(staging, || index_checkpoint(cx))?;
    Ok(PreparedRepair {
        database_path,
        workspace_id,
        generation: source.generation,
        document_count: source.documents_total,
    })
}

#[cfg(test)]
#[path = "index_doctor_repair_tests.rs"]
mod tests;

/// The same parent-directory lease used by ordinary search and publication;
/// no new lock file, database write, or independent coordination protocol.
pub(crate) async fn publication_lease(
    cx: &asupersync::Cx,
    index: &Path,
) -> Result<IndexGenerationLease, IndexRebuildError> {
    IndexGenerationLease::publish(cx, index).await
}

/// Publication is a non-cancellable commit tail, like ordinary index
/// publication. Reuse its no-follow durability barriers rather than treating
/// a buffered file flush as a disk persistence guarantee.
pub(crate) fn flush_tree(path: &Path) -> Result<(), IndexRebuildError> {
    sync_index_generation(path, || Ok(()))?;
    if let Some(parent) = path.parent() {
        sync_index_directory(parent)?;
    }
    Ok(())
}

pub(crate) fn flush_directory(path: &Path) -> Result<(), IndexRebuildError> {
    sync_index_directory(path)
}

/// Staging admission alone is insufficient for doctor: publication copies the
/// staged generation and retains before-image backups instead of swapping two
/// directory names. Project that extra allocation through the shared reserve
/// policy before retiring the live admission marker. This is a preflight, not
/// a reservation against unrelated filesystem writers.
pub(crate) fn admit_repair_copy(
    cx: &asupersync::Cx,
    destination: &Path,
    bytes: u64,
    entries: u64,
) -> Result<(), IndexRebuildError> {
    super::storage::confirm_reserve(cx, destination, |ancestor| {
        let capacity = super::storage::filesystem_capacity(ancestor)?;
        Ok(super::storage::Capacity {
            available_bytes: capacity.available_bytes.saturating_sub(bytes),
            available_inodes: capacity
                .available_inodes
                .map(|value| value.saturating_sub(entries)),
        })
    })
}
