//! Admit the complete vector image a published generation actually exposes.
//!
//! Native read-only opens deliberately tolerate crash-recovery WAL prefixes.
//! Publication has a stronger contract: every vector delta is compacted before
//! its generation becomes eligible. A pending replacement can leave the main
//! header's count unchanged, and a corrupt or stale WAL can be silently ignored
//! by the native recovery reader. Neither state is a complete published tier.

use std::path::Path;

use frankensearch::index::{StrictWalInspection, inspect_wal_strict, wal_path_for};
use frankensearch::{SearchError, VectorIndex};

// Admitted sidecars contain no records, only an optional native header. Refuse
// larger files before native inspection or recovery loading allocates their
// whole image. This is a preflight bound; the caller still owns the pinned
// generation. It does not limit private delta writes.
const MAX_PUBLISHED_WAL_BYTES: u64 = 4096;

fn refused(path: &Path, detail: impl Into<String>) -> SearchError {
    SearchError::IndexCorrupted {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn inspect_published_wal(path: &Path) -> Result<StrictWalInspection, SearchError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.len() > MAX_PUBLISHED_WAL_BYTES => {
            return Err(refused(
                path,
                "published vector WAL exceeds the empty-sidecar bound",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(SearchError::Io(error)),
    }
    let inspection = inspect_wal_strict(path)
        .map_err(|error| refused(path, format!("published vector WAL is invalid: {error}")))?;
    match &inspection {
        StrictWalInspection::Absent | StrictWalInspection::Empty { .. } => {}
        StrictWalInspection::LegacyV1(image) if image.entry_count == 0 => {}
        StrictWalInspection::LegacyV1(_) => {
            return Err(refused(
                path,
                "published vector tier contains uncompacted WAL records",
            ));
        }
        StrictWalInspection::IdentityBoundV2 { .. } => {
            return Err(refused(
                path,
                "published legacy vector tier has an incompatible WAL identity",
            ));
        }
    }
    Ok(inspection)
}

pub(super) fn open_read_only(path: &Path) -> Result<VectorIndex, SearchError> {
    open_with(path, VectorIndex::open_read_only)
}

fn open_with(
    path: &Path,
    open: impl FnOnce(&Path) -> Result<VectorIndex, SearchError>,
) -> Result<VectorIndex, SearchError> {
    let wal_path = wal_path_for(path);
    let before = inspect_published_wal(&wal_path)?;
    let index = open(path)?;
    // The caller pins the generation. Also bind the native reader to the
    // sidecar just inspected, rather than admitting a different WAL image
    // installed between the strict inspection and the native open.
    let after = inspect_published_wal(&wal_path)?;
    if before != after || index.wal_record_count() != 0 {
        return Err(refused(
            &wal_path,
            "published vector WAL changed while the tier was opened",
        ));
    }
    Ok(index)
}

#[cfg(test)]
#[path = "index_vector_admission_tests.rs"]
mod tests;
