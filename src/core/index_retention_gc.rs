//! Bounded retention of displaced index generations
//! (bd-reality-core-convergence-1azkt.42).
//!
//! Every publication moves the displaced live generation to a retained
//! sibling (`index.previous`, `index.previous.001`, ...). Two features read
//! retained generations: snapshot-bounded search
//! (`IndexGenerationLease::index_for_snapshot`) and crash recovery
//! (`recover_interrupted_publish_for_snapshot`). Both use only a generation
//! that validates completely, and both prefer the newest one. Keeping the
//! newest [`RETAINED_GENERATION_LIMIT`] *valid* generations therefore keeps both
//! features while bounding disk to a constant number of index copies instead
//! of one full copy per write.
//!
//! Reclamation runs only while the caller holds the exclusive publication
//! fence. On Unix that fence drains every shared reader lease first, so no
//! reader can be holding a generation while it is reclaimed. Search indexes
//! are derived, rebuildable assets; reclaiming one never touches FrankenSQLite
//! source truth. This module never removes the active generation, staging
//! output (`.publish-*`, including an attested displacement) or rejected
//! quarantine (`.rejected-*`): it recognizes only canonical retained names and
//! its own interrupted reclamation leftovers.

use std::path::{Path, PathBuf};

use super::{
    INDEX_RETAINED_SUFFIX, IndexRebuildError, ensure_index_path_has_no_symlinks, index_base_name,
    index_parent, monotonicish_stamp, parse_index_metadata, path_exists_no_follow,
    recoverable_index_generation, retained_generation_sequence,
};

/// Valid retained generations kept beside the active one. Two covers a reader
/// whose snapshot predates the newest publication plus one older fallback for
/// recovery when the newest retained copy is itself damaged.
pub(super) const RETAINED_GENERATION_LIMIT: usize = 2;

/// Name prefix (after the dot and index base name) of a retained generation
/// that was renamed out of the retained namespace and is being deleted.
const RECLAIM_PREFIX: &str = ".reclaim-";

/// Why one retained directory is kept or reclaimed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionReason {
    /// One of the newest valid generations within the retention limit.
    NewestValid,
    /// A valid generation older than the newest `limit` valid ones.
    BeyondRetentionLimit,
    /// No readable generation watermark, or the generation does not validate,
    /// so neither search nor recovery could ever use it.
    UnusableGeneration,
    /// A previous reclamation renamed it aside but did not finish deleting it.
    InterruptedReclaim,
}

impl RetentionReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewestValid => "newest_valid",
            Self::BeyondRetentionLimit => "beyond_retention_limit",
            Self::UnusableGeneration => "unusable_generation",
            Self::InterruptedReclaim => "interrupted_reclaim",
        }
    }

    #[must_use]
    pub const fn keeps(self) -> bool {
        matches!(self, Self::NewestValid)
    }
}

/// One retained (or leftover) directory and its disposition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedGenerationEntry {
    pub path: PathBuf,
    pub generation: Option<u64>,
    pub reason: RetentionReason,
    pub size_bytes: u64,
}

/// Deterministic retention decision for every retained directory.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetentionPlan {
    pub limit: usize,
    pub entries: Vec<RetainedGenerationEntry>,
}

impl RetentionPlan {
    pub fn kept(&self) -> impl Iterator<Item = &RetainedGenerationEntry> {
        self.entries.iter().filter(|entry| entry.reason.keeps())
    }

    pub fn reclaimable(&self) -> impl Iterator<Item = &RetainedGenerationEntry> {
        self.entries.iter().filter(|entry| !entry.reason.keeps())
    }

    #[must_use]
    pub fn reclaimable_bytes(&self) -> u64 {
        self.reclaimable()
            .fold(0_u64, |total, entry| total.saturating_add(entry.size_bytes))
    }
}

/// Result of applying a plan.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetentionOutcome {
    pub reclaimed: Vec<RetainedGenerationEntry>,
    pub failures: Vec<(PathBuf, String)>,
    pub kept: usize,
}

impl RetentionOutcome {
    #[must_use]
    pub fn reclaimed_bytes(&self) -> u64 {
        self.reclaimed
            .iter()
            .fold(0_u64, |total, entry| total.saturating_add(entry.size_bytes))
    }
}

struct Candidate {
    path: PathBuf,
    sequence: u32,
    generation: Option<u64>,
    modified_nanos: u128,
}

/// Classify every retained generation of `index_dir` without mutating
/// anything. Only the newest `limit` generations that fully validate are
/// kept; validation stops once `limit` are found, so the cost is bounded by
/// `limit` tier opens regardless of how many copies have accumulated.
pub(crate) fn plan(index_dir: &Path, limit: usize) -> Result<RetentionPlan, IndexRebuildError> {
    let parent = index_parent(index_dir);
    let mut retention = RetentionPlan {
        limit,
        entries: Vec::new(),
    };
    if !path_exists_no_follow(parent) {
        return Ok(retention);
    }
    ensure_index_path_has_no_symlinks(parent, "plan retained index reclamation")?;
    let base = index_base_name(index_dir)?;
    let retained_prefix = format!("{base}{INDEX_RETAINED_SUFFIX}");
    let reclaim_prefix = format!(".{base}{RECLAIM_PREFIX}");
    let entries = std::fs::read_dir(parent).map_err(|error| {
        IndexRebuildError::Index(format!(
            "Failed to inspect retained index generations in '{}': {error}",
            parent.display()
        ))
    })?;

    let mut candidates = Vec::new();
    let mut leftovers = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            IndexRebuildError::Index(format!(
                "Failed to inspect a retained index generation in '{}': {error}",
                parent.display()
            ))
        })?;
        // `DirEntry::file_type` does not follow links: a symlink or file that
        // happens to carry a retained name is never planned for deletion.
        let is_directory = entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false);
        if !is_directory {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        if name.starts_with(&reclaim_prefix) {
            leftovers.push(path);
            continue;
        }
        let Some(sequence) = retained_generation_sequence(name, &retained_prefix) else {
            continue;
        };
        let generation = parse_index_metadata(&path)
            .ok()
            .flatten()
            .and_then(|metadata| metadata.generation);
        let modified_nanos = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        candidates.push(Candidate {
            path,
            sequence,
            generation,
            modified_nanos,
        });
    }

    // Newest first: a readable watermark beats none, then the higher source
    // generation, then the later displacement, then the larger name.
    candidates.sort_by(|left, right| {
        (
            right.generation.is_some(),
            right.generation,
            right.modified_nanos,
            right.sequence,
        )
            .cmp(&(
                left.generation.is_some(),
                left.generation,
                left.modified_nanos,
                left.sequence,
            ))
    });

    let mut kept = 0_usize;
    for candidate in candidates {
        let reason = if kept >= limit {
            if candidate.generation.is_some() {
                RetentionReason::BeyondRetentionLimit
            } else {
                RetentionReason::UnusableGeneration
            }
        } else if candidate.generation.is_some()
            && recoverable_index_generation(&candidate.path) == candidate.generation
        {
            kept = kept.saturating_add(1);
            RetentionReason::NewestValid
        } else {
            RetentionReason::UnusableGeneration
        };
        retention.entries.push(RetainedGenerationEntry {
            size_bytes: directory_bytes(&candidate.path),
            path: candidate.path,
            generation: candidate.generation,
            reason,
        });
    }
    leftovers.sort();
    for path in leftovers {
        retention.entries.push(RetainedGenerationEntry {
            size_bytes: directory_bytes(&path),
            path,
            generation: None,
            reason: RetentionReason::InterruptedReclaim,
        });
    }
    Ok(retention)
}

/// Delete every reclaimable entry of `retention`. The caller must hold the
/// exclusive publication fence. Each generation is first renamed out of the
/// retained namespace, so a crash mid-delete can never leave a partial copy
/// that looks like a retained generation; the leftover is finished by the next
/// reclamation. Failures are collected rather than raised: reclamation is
/// maintenance and must never fail the publication that triggered it.
pub(crate) fn apply(index_dir: &Path, retention: &RetentionPlan) -> RetentionOutcome {
    let mut outcome = RetentionOutcome {
        kept: retention.kept().count(),
        ..RetentionOutcome::default()
    };
    let parent = index_parent(index_dir);
    let Ok(base) = index_base_name(index_dir) else {
        return outcome;
    };
    for entry in retention.reclaimable() {
        match reclaim_one(parent, &base, entry) {
            Ok(()) => outcome.reclaimed.push(entry.clone()),
            Err(error) => outcome.failures.push((entry.path.clone(), error)),
        }
    }
    outcome
}

/// Plan with the default limit and apply under the caller's fence, logging
/// the result. Used after every successful publication.
pub(crate) fn reclaim_after_publish(index_dir: &Path) -> Option<RetentionOutcome> {
    let retention = match plan(index_dir, RETAINED_GENERATION_LIMIT) {
        Ok(retention) => retention,
        Err(error) => {
            tracing::warn!(
                target: "ee::index",
                %error,
                index_dir = %index_dir.display(),
                "could not plan retained index generation reclamation; retained copies are kept"
            );
            return None;
        }
    };
    if retention.reclaimable().next().is_none() {
        return Some(RetentionOutcome {
            kept: retention.kept().count(),
            ..RetentionOutcome::default()
        });
    }
    let outcome = apply(index_dir, &retention);
    tracing::info!(
        target: "ee::index",
        index_dir = %index_dir.display(),
        reclaimed = outcome.reclaimed.len(),
        reclaimed_bytes = outcome.reclaimed_bytes(),
        kept = outcome.kept,
        failures = outcome.failures.len(),
        "reclaimed retained index generations beyond the retention bound"
    );
    for (path, error) in &outcome.failures {
        tracing::warn!(
            target: "ee::index",
            path = %path.display(),
            %error,
            "retained index generation could not be reclaimed; it will be retried on the next publication"
        );
    }
    Some(outcome)
}

fn reclaim_one(parent: &Path, base: &str, entry: &RetainedGenerationEntry) -> Result<(), String> {
    ensure_index_path_has_no_symlinks(&entry.path, "reclaim retained index generation")
        .map_err(|error| error.to_string())?;
    let metadata = std::fs::symlink_metadata(&entry.path).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_dir() {
        return Err("refusing to reclaim a retained entry that is not a directory".to_owned());
    }
    let doomed = if entry.reason == RetentionReason::InterruptedReclaim {
        entry.path.clone()
    } else {
        let doomed = allocate_reclaim_name(parent, base)?;
        super::rename_index_dir(&entry.path, &doomed, "retire retained index generation")
            .map_err(|error| error.to_string())?;
        doomed
    };
    // `remove_dir_all` removes symlinks inside the tree without following them.
    std::fs::remove_dir_all(&doomed).map_err(|error| {
        format!(
            "failed to delete reclaimed index generation '{}': {error}",
            doomed.display()
        )
    })
}

fn allocate_reclaim_name(parent: &Path, base: &str) -> Result<PathBuf, String> {
    let stamp = monotonicish_stamp();
    for sequence in 0_u32..1000 {
        let candidate = parent.join(format!(".{base}{RECLAIM_PREFIX}{stamp}-{sequence:03}"));
        if !path_exists_no_follow(&candidate) {
            return Ok(candidate);
        }
    }
    Err("could not allocate a reclamation name for a retained index generation".to_owned())
}

/// Apparent bytes under `path`, never following symlinks. Best effort: an
/// unreadable subtree contributes what could be read.
fn directory_bytes(path: &Path) -> u64 {
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_dir() {
            if let Ok(entries) = std::fs::read_dir(&current) {
                pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
            }
        } else if metadata.file_type().is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    total
}
