//! Build a complete repair generation without writing to the source store.
//!
//! Doctor owns publication and its inverse journal. This adapter deliberately
//! omits index-job, advisory-SQL-lock, model-registry and anchor backfill writes;
//! it reuses the canonical source projector, builders and admission checks.

use std::path::{Path, PathBuf};

use super::{
    DEFAULT_SEARCH_EMBEDDER, DbConnection, IndexGenerationLease, IndexRebuildError,
    build_index_generation, collect_workspace_index_source_snapshot,
    default_workspace_database_path, embedder_fingerprint_for_index_metadata,
    ensure_index_path_has_no_symlinks, hash_fallback_embedder_stack, index_checkpoint,
    resolve_index_workspace_id, sync_index_directory, sync_index_generation, validate_built_generation,
    workspace_embedder_stack, write_index_metadata,
};

pub(crate) struct PreparedRepair {
    database_path: PathBuf,
    workspace_id: String,
    generation: u64,
    document_count: u32,
}

impl PreparedRepair {
    /// A concurrent source change must not be overwritten by an older repair.
    /// Called under the generation publication lease, immediately before the
    /// first live-file mutation. Later source writes naturally make this
    /// captured generation stale, just as they do for an ordinary rebuild.
    pub(crate) fn check_source_generation(&self) -> Result<(), IndexRebuildError> {
        let db = DbConnection::open_file_read_only(&self.database_path)?;
        let current = db
            .get_workspace_generation(&self.workspace_id)?
            .unwrap_or_else(|| u64::from(self.document_count));
        if current != self.generation {
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
    let source = collect_workspace_index_source_snapshot(&db, &workspace_id)?;
    // An empty repair must not load or download a model to embed nothing.
    let stack = if source.documents_total == 0 {
        DEFAULT_SEARCH_EMBEDDER
            .get()
            .map_or_else(hash_fallback_embedder_stack, |selection| {
                selection.stack.clone()
            })
    } else {
        workspace_embedder_stack(&db, &workspace_id)?.0
    };
    let fingerprint = embedder_fingerprint_for_index_metadata(&stack);
    let stats = build_index_generation(cx, staging, stack, source.documents).await?;
    validate_built_generation(staging, stats, source.document_counts)
        .map_err(IndexRebuildError::Index)?;
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
