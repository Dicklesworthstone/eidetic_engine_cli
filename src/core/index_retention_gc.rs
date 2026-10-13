//! Bounded retention of displaced index generations
//! (bd-reality-core-convergence-1azkt.42).
//!
//! Every publication moves the displaced live generation to a retained
//! sibling (`index.previous`, `index.previous.001`, ...). Two features read
//! retained generations: snapshot-bounded search
//! (`IndexGenerationLease::index_for_snapshot`) and crash recovery
//! (`recover_interrupted_publish_for_snapshot`). Both use only a generation
//! that validates completely, and both prefer the newest one. Keeping the
//! newest [`RETAINED_GENERATION_LIMIT`] *distinct, valid* generations keeps both
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
//!
//! Automatic reclamation is optional work in the masked publication tail.
//! Its discovery, validation and deletion admission share a small budget;
//! excess work belongs to explicit vacuum, not to a reader's lock wait.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    INDEX_RETAINED_SUFFIX, IndexRebuildError, ensure_index_path_has_no_symlinks, index_base_name,
    index_parent, monotonicish_stamp, parse_index_metadata, path_exists_no_follow,
    recoverable_index_generation, retained_generation_sequence,
};

/// Distinct valid source generations kept beside the active one. Two covers a
/// reader whose snapshot predates the newest publication plus one older
/// fallback for recovery when the newest retained copy is itself damaged.
pub(super) const RETAINED_GENERATION_LIMIT: usize = 2;

/// Name prefix (after the dot and index base name) of a retained generation
/// that was renamed out of the retained namespace and is being deleted.
const RECLAIM_PREFIX: &str = ".reclaim-";

const AUTO_WALL_BUDGET: Duration = Duration::from_millis(250);
const AUTO_WORK_LIMIT: usize = 4096;
const AUTO_VALIDATION_LIMIT: usize = 8;
const AUTO_RECLAIM_LIMIT: usize = 2;
const AUTO_TREE_ENTRY_LIMIT: usize = 1024;

/// Independent maintenance allowance: the caller masks cancellation to finish
/// an already-committed publication. That mask must not also admit unbounded
/// optional cleanup. No caller budget is renewed or publication undone here.
struct AutomaticRetentionBudget {
    request: crate::core::budget::RequestBudget,
    work_remaining: usize,
    validations_remaining: usize,
}

impl AutomaticRetentionBudget {
    fn new() -> Self {
        Self {
            request: crate::core::budget::RequestBudget::unbounded()
                .with_wall_clock(AUTO_WALL_BUDGET),
            work_remaining: AUTO_WORK_LIMIT,
            validations_remaining: AUTO_VALIDATION_LIMIT,
        }
    }

    fn checkpoint(&mut self) -> Result<(), IndexRebuildError> {
        self.request.check().map_err(|_| automatic_deferred())?;
        self.work_remaining = self
            .work_remaining
            .checked_sub(1)
            .ok_or_else(automatic_deferred)?;
        Ok(())
    }

    fn validation(&mut self) -> Result<(), IndexRebuildError> {
        self.checkpoint()?;
        self.validations_remaining = self
            .validations_remaining
            .checked_sub(1)
            .ok_or_else(automatic_deferred)?;
        Ok(())
    }
}

fn automatic_deferred() -> IndexRebuildError {
    IndexRebuildError::Index(
        "Automatic index retention budget exhausted; publication is preserved; use explicit ee index vacuum for remaining cleanup".to_owned(),
    )
}

/// Why one retained directory is kept or reclaimed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionReason {
    /// One of the newest valid generations within the retention limit.
    NewestValid,
    /// A valid generation older than the newest `limit` valid ones.
    BeyondRetentionLimit,
    /// A newer validated copy already covers this source generation.
    DuplicateGeneration,
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
            Self::DuplicateGeneration => "duplicate_generation",
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
/// anything. Keep one validated copy of each of the newest `limit` distinct
/// source generations. Covered duplicates need no further tier opens. Invalid
/// candidates may require extra opens before enough recoverable snapshots are
/// found; a readable watermark alone is never sufficient to retain a copy.
pub(crate) fn plan(index_dir: &Path, limit: usize) -> Result<RetentionPlan, IndexRebuildError> {
    let (candidates, mut leftovers) = collect_candidates(index_dir, || Ok(()))?;
    let mut retention = RetentionPlan {
        limit,
        entries: Vec::new(),
    };
    for (candidate, reason) in classify_candidates(candidates, limit, recoverable_index_generation)
    {
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

/// A partial inventory cannot authorize any retention decision. Count even
/// unrelated parent entries, and check again after metadata reads before
/// returning the complete candidate set. Explicit vacuum has no automatic cap.
fn collect_candidates(
    index_dir: &Path,
    mut checkpoint: impl FnMut() -> Result<(), IndexRebuildError>,
) -> Result<(Vec<Candidate>, Vec<PathBuf>), IndexRebuildError> {
    checkpoint()?;
    let parent = index_parent(index_dir);
    if !path_exists_no_follow(parent) {
        return Ok((Vec::new(), Vec::new()));
    }
    ensure_index_path_has_no_symlinks(parent, "plan retained index reclamation")?;
    let base = index_base_name(index_dir)?;
    let retained_prefix = format!("{base}{INDEX_RETAINED_SUFFIX}");
    let entries = std::fs::read_dir(parent).map_err(|error| {
        IndexRebuildError::Index(format!(
            "Failed to inspect retained index generations in '{}': {error}",
            parent.display()
        ))
    })?;

    let mut candidates = Vec::new();
    let mut leftovers = Vec::new();
    for entry in entries {
        checkpoint()?;
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
        if is_reclaim_name(name, &base) {
            leftovers.push(path);
            continue;
        }
        let Some(sequence) = retained_generation_sequence(name, &retained_prefix) else {
            continue;
        };
        checkpoint()?;
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
        checkpoint()?;
        candidates.push(Candidate {
            path,
            sequence,
            generation,
            modified_nanos,
        });
    }

    checkpoint()?;
    Ok((candidates, leftovers))
}

/// Rebuilding without a source write can retain several copies of the same
/// watermark. Counting directories would let those copies evict the only
/// generation usable by an older database snapshot. Count validated source
/// watermarks instead, and never let a corrupt copy suppress a healthy one.
fn classify_candidates(
    mut candidates: Vec<Candidate>,
    limit: usize,
    mut recover_generation: impl FnMut(&Path) -> Option<u64>,
) -> impl Iterator<Item = (Candidate, RetentionReason)> {
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

    let mut kept_generations = BTreeSet::new();
    candidates.into_iter().map(move |candidate| {
        let reason = match candidate.generation {
            Some(generation) if kept_generations.contains(&generation) => {
                RetentionReason::DuplicateGeneration
            }
            Some(_) if kept_generations.len() >= limit => RetentionReason::BeyondRetentionLimit,
            Some(generation) if recover_generation(&candidate.path) == Some(generation) => {
                kept_generations.insert(generation);
                RetentionReason::NewestValid
            }
            _ => RetentionReason::UnusableGeneration,
        };
        (candidate, reason)
    })
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

/// Do bounded optional work under the caller's publication fence. Full tier
/// validation still decides which distinct snapshots may be discarded. Never
/// size kept trees or drain an arbitrarily large backlog in the masked tail.
/// Synchronous backend/filesystem calls are not preemptible; the wall budget
/// stops subsequent work, and deletion additionally requires a bounded tree.
pub(crate) fn reclaim_after_publish(index_dir: &Path) -> Option<RetentionOutcome> {
    reclaim_after_publish_with_budget(index_dir, &mut AutomaticRetentionBudget::new())
}

fn reclaim_after_publish_with_budget(
    index_dir: &Path,
    budget: &mut AutomaticRetentionBudget,
) -> Option<RetentionOutcome> {
    let selected = (|| {
        let (candidates, mut leftovers) = collect_candidates(index_dir, || budget.checkpoint())?;
        let (kept, mut reclaimable) =
            automatic_candidates(candidates, budget, recoverable_index_generation)?;
        // Finish already-retired leftovers first, then the oldest excess
        // copies. A subsequent publication can resume without a new sidecar.
        leftovers.sort();
        reclaimable.reverse();
        let mut entries: Vec<_> = leftovers
            .into_iter()
            .map(|path| RetainedGenerationEntry {
                path,
                generation: None,
                reason: RetentionReason::InterruptedReclaim,
                size_bytes: 0,
            })
            .collect();
        entries.extend(reclaimable);
        budget.checkpoint()?;
        Ok::<_, IndexRebuildError>((kept, entries))
    })();
    let (kept, entries) = match selected {
        Ok(selected) => selected,
        Err(error) => {
            tracing::warn!(
                target: "ee::index",
                %error,
                index_dir = %index_dir.display(),
                "automatic index retention deferred; publication is preserved; use explicit ee index vacuum for remaining cleanup"
            );
            return None;
        }
    };
    let mut outcome = RetentionOutcome {
        kept,
        ..RetentionOutcome::default()
    };
    if entries.is_empty() {
        return Some(outcome);
    }
    let parent = index_parent(index_dir);
    let base = index_base_name(index_dir).ok()?;
    let total = entries.len();
    for mut entry in entries.into_iter().take(AUTO_RECLAIM_LIMIT) {
        // Measure only an entry we will attempt to reclaim, charging every
        // nested entry to the same budget as discovery and tier validation.
        // Refusal happens before rename, so an oversized copy stays intact.
        match automatic_directory_bytes(&entry.path, budget) {
            Ok(bytes) => entry.size_bytes = bytes,
            Err(error) => {
                tracing::warn!(
                    target: "ee::index",
                    %error,
                    path = %entry.path.display(),
                    "automatic index deletion deferred before rename; use explicit ee index vacuum"
                );
                break;
            }
        }
        match reclaim_one(parent, &base, &entry) {
            Ok(()) => outcome.reclaimed.push(entry),
            Err(error) => outcome.failures.push((entry.path, error)),
        }
    }
    tracing::info!(
        target: "ee::index",
        index_dir = %index_dir.display(),
        reclaimed = outcome.reclaimed.len(),
        reclaimed_bytes = outcome.reclaimed_bytes(),
        kept = outcome.kept,
        failures = outcome.failures.len(),
        deferred = total.saturating_sub(outcome.reclaimed.len() + outcome.failures.len()),
        "finished bounded post-publication retention; explicit ee index vacuum can finish deferred cleanup"
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

fn automatic_candidates(
    candidates: Vec<Candidate>,
    budget: &mut AutomaticRetentionBudget,
    mut recover_generation: impl FnMut(&Path) -> Option<u64>,
) -> Result<(usize, Vec<RetainedGenerationEntry>), IndexRebuildError> {
    budget.checkpoint()?;
    if candidates.len() <= RETAINED_GENERATION_LIMIT {
        // Keeping everything needs no proof of validity. Do not open tiers
        // merely to discover there is no excess to delete. These are counts
        // of preserved directories, not claims that their generations validate.
        return Ok((candidates.len(), Vec::new()));
    }
    let mut interrupted = false;
    let classified: Vec<_> = classify_candidates(candidates, RETAINED_GENERATION_LIMIT, |path| {
        if interrupted || budget.validation().is_err() {
            interrupted = true;
            return None;
        }
        let generation = recover_generation(path);
        if budget.checkpoint().is_err() {
            interrupted = true;
            return None;
        }
        generation
    })
    .collect();
    // None from an interrupted validator is NOT an unusable generation. Drop
    // every provisional decision; in particular, never delete a valid copy
    // because optional maintenance lacked time to finish checking it.
    if interrupted {
        return Err(automatic_deferred());
    }
    budget.checkpoint()?;
    let mut kept = 0;
    let mut reclaimable = Vec::new();
    for (candidate, reason) in classified {
        if reason.keeps() {
            kept += 1;
        } else {
            reclaimable.push(RetainedGenerationEntry {
                path: candidate.path,
                generation: candidate.generation,
                reason,
                // Filled from the complete bounded tree immediately before
                // deletion. No unmeasured entry is reported as reclaimed.
                size_bytes: 0,
            });
        }
    }
    Ok((kept, reclaimable))
}

/// Bound the tree before invoking the existing no-follow deletion primitive.
/// Under the exclusive generation fence cooperating publishers cannot add
/// entries between inspection and deletion. This is not a hard I/O timeout or
/// a fence against uncooperative filesystem writers. Explicit vacuum retains
/// its complete-tree behavior for oversized or unreadable automatic candidates.
fn automatic_directory_bytes(
    path: &Path,
    budget: &mut AutomaticRetentionBudget,
) -> Result<u64, IndexRebuildError> {
    let mut total = 0_u64;
    let mut count = 1_usize;
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        budget.checkpoint()?;
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            IndexRebuildError::Index(format!(
                "Could not inspect automatic retention tree: {error}"
            ))
        })?;
        if metadata.file_type().is_dir() {
            let entries = std::fs::read_dir(&current).map_err(|error| {
                IndexRebuildError::Index(format!(
                    "Could not read automatic retention tree: {error}"
                ))
            })?;
            for entry in entries {
                budget.checkpoint()?;
                count += 1;
                if count > AUTO_TREE_ENTRY_LIMIT {
                    return Err(automatic_deferred());
                }
                pending.push(
                    entry
                        .map_err(|error| IndexRebuildError::Index(error.to_string()))?
                        .path(),
                );
            }
        } else if metadata.file_type().is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    budget.checkpoint()?;
    Ok(total)
}

fn reclaim_one(parent: &Path, base: &str, entry: &RetainedGenerationEntry) -> Result<(), String> {
    // A plan is data, not deletion authority. Recheck its namespace at the
    // destructive boundary even though the ordinary planner emits only owned
    // siblings. Never allow an active/staging/quarantine path, another index,
    // an escaped parent, or a kept entry to be renamed or recursively removed.
    if entry.path.parent() != Some(parent) {
        return Err("refusing to reclaim a path outside the index parent".to_owned());
    }
    let owned = entry
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| match entry.reason {
            RetentionReason::InterruptedReclaim => is_reclaim_name(name, base),
            RetentionReason::BeyondRetentionLimit
            | RetentionReason::DuplicateGeneration
            | RetentionReason::UnusableGeneration => {
                retained_generation_sequence(name, &format!("{base}{INDEX_RETAINED_SUFFIX}"))
                    .is_some()
            }
            RetentionReason::NewestValid => false,
        });
    if !owned {
        return Err("refusing to reclaim a path outside the owned retention namespace".to_owned());
    }
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

/// Recognize exactly the names emitted by `allocate_reclaim_name`. A prefix
/// match alone would adopt unrelated directories such as `.index.reclaim-notes`
/// as interrupted deletions. The timestamp is canonical decimal `u128` and the
/// collision sequence is exactly three ASCII digits (000 through 999).
fn is_reclaim_name(name: &str, base: &str) -> bool {
    let prefix = format!(".{base}{RECLAIM_PREFIX}");
    let Some(suffix) = name.strip_prefix(&prefix) else {
        return false;
    };
    let Some((stamp, sequence)) = suffix.split_once('-') else {
        return false;
    };
    if sequence.len() != 3 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    stamp
        .parse::<u128>()
        .is_ok_and(|value| stamp == value.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn candidate(sequence: u32, generation: Option<u64>) -> Candidate {
        Candidate {
            path: PathBuf::from(format!("index.previous.{sequence:03}")),
            sequence,
            generation,
            modified_nanos: u128::from(sequence),
        }
    }

    fn classify_valid(candidates: Vec<Candidate>, limit: usize) -> RetentionPlan {
        let watermarks: BTreeMap<_, _> = candidates
            .iter()
            .map(|entry| (entry.path.clone(), entry.generation))
            .collect();
        RetentionPlan {
            limit,
            entries: classify_candidates(candidates, limit, |path| {
                watermarks.get(path).copied().flatten()
            })
            .map(|(entry, reason)| RetainedGenerationEntry {
                path: entry.path,
                generation: entry.generation,
                reason,
                size_bytes: 1,
            })
            .collect(),
        }
    }

    #[test]
    fn repeated_rebuilds_do_not_evict_the_older_source_snapshot() {
        let retention = classify_valid(
            vec![
                candidate(1, Some(28)),
                candidate(2, Some(29)),
                candidate(3, Some(30)),
                candidate(4, Some(30)),
            ],
            2,
        );
        assert_eq!(
            retention
                .kept()
                .map(|entry| entry.generation)
                .collect::<Vec<_>>(),
            vec![Some(30), Some(29)]
        );
        assert_eq!(
            retention.entries[1].reason,
            RetentionReason::DuplicateGeneration
        );
        assert_eq!(retention.reclaimable_bytes(), 2);
        assert_eq!(
            RetentionReason::DuplicateGeneration.as_str(),
            "duplicate_generation"
        );
        assert!(!RetentionReason::DuplicateGeneration.keeps());
    }

    #[test]
    fn corrupt_newer_copy_does_not_suppress_a_valid_duplicate() {
        let mut opened = Vec::new();
        let classified: Vec<_> = classify_candidates(
            vec![
                candidate(1, Some(7)),
                candidate(2, Some(8)),
                candidate(3, Some(8)),
            ],
            2,
            |path| {
                opened.push(path.to_path_buf());
                match path.to_str() {
                    Some("index.previous.003") => None,
                    Some("index.previous.002") => Some(8),
                    _ => Some(7),
                }
            },
        )
        .map(|(entry, reason)| (entry.generation, reason))
        .collect();
        assert_eq!(
            classified,
            vec![
                (Some(8), RetentionReason::UnusableGeneration),
                (Some(8), RetentionReason::NewestValid),
                (Some(7), RetentionReason::NewestValid),
            ]
        );
        assert_eq!(opened.len(), 3);
    }

    #[test]
    fn validation_must_agree_with_the_captured_watermark() {
        let classified: Vec<_> = classify_candidates(
            vec![candidate(1, Some(8)), candidate(2, Some(8))],
            1,
            |path| {
                if path == Path::new("index.previous.002") {
                    Some(9)
                } else {
                    Some(8)
                }
            },
        )
        .map(|(_, reason)| reason)
        .collect();
        assert_eq!(
            classified,
            vec![
                RetentionReason::UnusableGeneration,
                RetentionReason::NewestValid,
            ]
        );
    }

    #[test]
    fn duplicate_storm_opens_only_the_distinct_retained_generations() {
        let mut candidates: Vec<_> = (3..=1102)
            .map(|sequence| candidate(sequence, Some(30)))
            .collect();
        candidates.extend([candidate(2, Some(29)), candidate(1, Some(28))]);
        let mut opens = 0;
        let reasons: Vec<_> = classify_candidates(candidates, 2, |path| {
            opens += 1;
            Some(if path == Path::new("index.previous.002") {
                29
            } else {
                30
            })
        })
        .map(|(_, reason)| reason)
        .collect();
        assert_eq!(opens, 2);
        assert_eq!(reasons.iter().filter(|reason| reason.keeps()).count(), 2);
        assert_eq!(
            reasons
                .iter()
                .filter(|reason| **reason == RetentionReason::DuplicateGeneration)
                .count(),
            1099
        );
        assert_eq!(reasons.last(), Some(&RetentionReason::BeyondRetentionLimit));
    }

    #[test]
    fn zero_limit_and_missing_watermarks_never_open_tiers() {
        for (limit, generation) in [(0, Some(0)), (2, None)] {
            let reasons: Vec<_> =
                classify_candidates(vec![candidate(1, generation)], limit, |_| {
                    panic!("a zero budget or missing watermark must not open tiers")
                })
                .map(|(_, reason)| reason)
                .collect();
            assert_eq!(reasons.len(), 1);
            assert!(!reasons[0].keeps());
        }
        let retention = classify_valid(vec![candidate(1, Some(0)), candidate(2, Some(0))], 2);
        assert_eq!(retention.kept().count(), 1);
        assert_eq!(retention.entries[0].generation, Some(0));
        assert_eq!(
            retention.entries[1].reason,
            RetentionReason::DuplicateGeneration
        );
    }

    #[test]
    fn retention_is_independent_of_directory_enumeration_order() {
        let build = || {
            let mut candidates = vec![
                candidate(1, Some(7)),
                candidate(2, Some(8)),
                candidate(3, Some(8)),
                candidate(4, None),
            ];
            // Tie the newest copies' timestamps: canonical sequence breaks it.
            candidates[1].modified_nanos = 10;
            candidates[2].modified_nanos = 10;
            candidates
        };
        let expected = classify_valid(build(), 2);
        for offset in 0..4 {
            let mut candidates = build();
            candidates.rotate_left(offset);
            assert_eq!(classify_valid(candidates, 2), expected);
            let mut candidates = build();
            candidates.reverse();
            candidates.rotate_left(offset);
            assert_eq!(classify_valid(candidates, 2), expected);
        }
        assert_eq!(expected.entries[0].path, Path::new("index.previous.003"));
    }

    #[test]
    fn reclaim_name_recognizes_only_the_allocators_canonical_format() {
        for base in ["index", "tenant.index", "индекс"] {
            for stamp in [0, 1, monotonicish_stamp(), u128::MAX] {
                for sequence in [0, 1, 42, 999] {
                    let name = format!(".{base}{RECLAIM_PREFIX}{stamp}-{sequence:03}");
                    assert!(is_reclaim_name(&name, base), "{name}");
                    assert!(!is_reclaim_name(&name, "another-index"));
                }
            }
        }
        for suffix in [
            "",
            "notes",
            "1",
            "-000",
            "1-",
            "1-00",
            "1-0000",
            "1-1000",
            "01-000",
            "+1-000",
            "-1-000",
            "1-+00",
            "1-00x",
            "1-000-old",
            "1-０００",
            "340282366920938463463374607431768211456-000",
        ] {
            let name = format!(".index{RECLAIM_PREFIX}{suffix}");
            assert!(!is_reclaim_name(&name, "index"), "{name}");
        }
    }

    fn canary_directory(path: &Path) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
        std::fs::write(path.join("canary"), b"must survive unless explicitly owned")
            .map_err(|error| error.to_string())
    }

    #[test]
    fn planning_and_reclamation_preserve_unowned_siblings_and_resume_owned_leftovers()
    -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let index = parent.join("index");
        let protected = [
            "index",
            ".index.publish-1-000",
            ".index.rejected-1-000",
            "other-index.previous",
            ".other-index.reclaim-1-000",
            ".index.reclaim-notes",
            ".index.reclaim-01-000",
            ".index.reclaim-1-000-old",
            ".index.reclaim-1-1000",
        ];
        for name in protected {
            canary_directory(&parent.join(name))?;
        }
        let retained = parent.join("index.previous");
        canary_directory(&retained)?;
        let interrupted = allocate_reclaim_name(&parent, "index")?;
        canary_directory(&interrupted)?;
        let regular_file = parent.join("index.previous.001");
        std::fs::write(&regular_file, b"not a directory").map_err(|error| error.to_string())?;

        let retention = plan(&index, 2).map_err(|error| error.to_string())?;
        assert_eq!(retention.entries.len(), 2);
        assert_eq!(
            retention
                .reclaimable()
                .map(|entry| entry.path.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([retained.clone(), interrupted.clone()])
        );
        let expected_bytes = retention.reclaimable_bytes();
        assert!(expected_bytes > 0);
        let outcome = apply(&index, &retention);
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(outcome.reclaimed.len(), 2);
        assert_eq!(outcome.reclaimed_bytes(), expected_bytes);
        assert!(!retained.exists());
        assert!(!interrupted.exists());
        for name in protected {
            assert_eq!(
                std::fs::read(parent.join(name).join("canary"))
                    .map_err(|error| error.to_string())?,
                b"must survive unless explicitly owned"
            );
        }
        assert_eq!(
            std::fs::read(&regular_file).map_err(|error| error.to_string())?,
            b"not a directory"
        );
        let next = plan(&index, 2).map_err(|error| error.to_string())?;
        assert!(next.entries.is_empty());
        assert!(apply(&index, &next).reclaimed.is_empty());
        Ok(())
    }

    #[test]
    fn destructive_boundary_rejects_forged_paths_and_dispositions() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let paths = [
            ("index", RetentionReason::UnusableGeneration),
            ("index", RetentionReason::InterruptedReclaim),
            (".index.publish-1-000", RetentionReason::UnusableGeneration),
            (".index.rejected-1-000", RetentionReason::UnusableGeneration),
            ("other-index.previous", RetentionReason::DuplicateGeneration),
            (
                "nested/index.previous",
                RetentionReason::BeyondRetentionLimit,
            ),
            (".index.reclaim-notes", RetentionReason::InterruptedReclaim),
            ("index.previous", RetentionReason::NewestValid),
            ("index.previous", RetentionReason::InterruptedReclaim),
            (".index.reclaim-1-000", RetentionReason::UnusableGeneration),
        ];
        for (relative, reason) in paths {
            let path = parent.join(relative);
            canary_directory(&path)?;
            let entry = RetainedGenerationEntry {
                path: path.clone(),
                generation: Some(1),
                reason,
                size_bytes: 1,
            };
            assert!(reclaim_one(&parent, "index", &entry).is_err(), "{relative}");
            assert!(path.join("canary").is_file(), "{relative}");
        }
        // Even a syntactically owned basename from a different parent is not
        // transferable reclamation authority for this index.
        let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_path = outside.path().join("index.previous");
        canary_directory(&outside_path)?;
        let escaped = RetainedGenerationEntry {
            path: outside_path.clone(),
            generation: Some(1),
            reason: RetentionReason::BeyondRetentionLimit,
            size_bytes: 1,
        };
        assert!(reclaim_one(&parent, "index", &escaped).is_err());
        assert!(outside_path.join("canary").is_file());
        Ok(())
    }

    #[test]
    fn one_rejected_entry_does_not_prevent_legitimate_reclamation() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let index = parent.join("index");
        let retained = parent.join("index.previous");
        canary_directory(&index)?;
        canary_directory(&retained)?;
        let retention = RetentionPlan {
            limit: 2,
            entries: [&index, &retained]
                .into_iter()
                .map(|path| RetainedGenerationEntry {
                    path: path.to_path_buf(),
                    generation: None,
                    reason: RetentionReason::UnusableGeneration,
                    size_bytes: 1,
                })
                .collect(),
        };
        let outcome = apply(&index, &retention);
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].0, index);
        assert_eq!(outcome.reclaimed.len(), 1);
        assert_eq!(outcome.reclaimed[0].path, retained);
        assert!(index.join("canary").is_file());
        assert!(!retained.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn canonical_reclaim_name_does_not_authorize_following_a_symlink() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let target = parent.join("unrelated-data");
        canary_directory(&target)?;
        let link = parent.join(".index.reclaim-1-000");
        std::os::unix::fs::symlink(&target, &link).map_err(|error| error.to_string())?;
        let entry = RetainedGenerationEntry {
            path: link.clone(),
            generation: None,
            reason: RetentionReason::InterruptedReclaim,
            size_bytes: 1,
        };
        assert!(reclaim_one(&parent, "index", &entry).is_err());
        let retention = plan(&parent.join("index"), 2).map_err(|error| error.to_string())?;
        assert!(retention.entries.is_empty());
        assert!(target.join("canary").is_file());
        assert!(
            std::fs::symlink_metadata(&link)
                .map_err(|error| error.to_string())?
                .is_symlink()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn real_tiers_keep_an_older_snapshot_readable_after_duplicate_reclamation() -> Result<(), String>
    {
        use super::super::{
            IndexBuilder, IndexDocumentCounts, IndexGenerationLease, hash_fallback_embedder_stack,
            write_index_metadata,
        };
        use std::time::Duration;

        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let index = parent.join("index");
        crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            let older = parent.join("index.previous");
            for (path, generation) in [
                (older.clone(), 7),
                (parent.join("index.previous.001"), 8),
                (parent.join("index.previous.002"), 8),
            ] {
                let documents = vec![crate::search::IndexableDocument::new(
                    "mem_retention_snapshot",
                    "Retained source snapshots must survive repeated index rebuilds.",
                )];
                IndexBuilder::new(&path)
                    .with_embedder_stack(hash_fallback_embedder_stack())
                    .add_documents(documents.clone())
                    .build(&cx)
                    .await
                    .map_err(|error| error.to_string())?;
                #[cfg(feature = "lexical-bm25")]
                super::super::build_lexical_tier(&cx, &path, &documents)
                    .await
                    .map_err(|error| error.to_string())?;
                write_index_metadata(&path, generation, IndexDocumentCounts::memory_only(1), None)
                    .map_err(|error| error.to_string())?;
                assert_eq!(recoverable_index_generation(&path), Some(generation));
            }
            let publisher = IndexGenerationLease::publish(&cx, &index)
                .await
                .map_err(|error| error.to_string())?;
            let retention = plan(&index, 2).map_err(|error| error.to_string())?;
            assert_eq!(retention.kept().count(), 2);
            assert_eq!(retention.reclaimable().count(), 1);
            let outcome = apply(&index, &retention);
            assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
            assert_eq!(outcome.reclaimed.len(), 1);
            assert_eq!(
                outcome.reclaimed[0].reason,
                RetentionReason::DuplicateGeneration
            );
            assert_eq!(recoverable_index_generation(&older), Some(7));
            drop(publisher);
            let reader = IndexGenerationLease::read(&cx, &index)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(
                reader
                    .index_for_snapshot(&cx, &index, 7)
                    .map_err(|error| error.to_string())?,
                older
            );
            assert!(reader.index_for_snapshot(&cx, &index, 6).is_err());
            assert!(
                !index.exists(),
                "retention must not invent a live generation"
            );
            drop(reader);
            let publisher = IndexGenerationLease::publish(&cx, &index)
                .await
                .map_err(|error| error.to_string())?;
            let second = plan(&index, 2).map_err(|error| error.to_string())?;
            assert_eq!(second.kept().count(), 2);
            assert_eq!(second.reclaimable().count(), 0);
            assert!(apply(&index, &second).reclaimed.is_empty());
            drop(publisher);
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())?
    }
}

#[cfg(test)]
mod automatic_tests {
    use super::*;
    use std::collections::BTreeMap;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn work_budget() -> AutomaticRetentionBudget {
        // Work-count tests must not depend on the speed of a debug filesystem.
        // The independent production wall allowance has its own refusal test.
        let mut budget = AutomaticRetentionBudget::new();
        budget.request = crate::core::budget::RequestBudget::unbounded();
        budget
    }

    fn canary(path: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(path)?;
        std::fs::write(
            path.join("canary"),
            b"retained source and publication sentinel",
        )
    }

    fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, Box<dyn std::error::Error>> {
        let mut files = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    pending.push(path);
                } else {
                    files.insert(path.strip_prefix(root)?.to_path_buf(), std::fs::read(path)?);
                }
            }
        }
        Ok(files)
    }

    #[test]
    fn automatic_budget_is_shared_monotone_and_independent_of_request_masking() -> TestResult {
        let mut budget = work_budget();
        budget.work_remaining = 2;
        budget.checkpoint()?;
        budget.checkpoint()?;
        assert!(budget.checkpoint().is_err());
        assert!(budget.checkpoint().is_err());
        assert_eq!(budget.work_remaining, 0);
        let mut budget = work_budget();
        budget.validations_remaining = 1;
        budget.validation()?;
        assert!(budget.validation().is_err());
        assert!(budget.validation().is_err());
        assert_eq!(budget.validations_remaining, 0);

        let root = tempfile::tempdir()?;
        let index = root.path().canonicalize()?.join("index");
        canary(&index)?;
        canary(&index_parent(&index).join(".index.reclaim-1-000"))?;
        let before = inventory(root.path())?;
        crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
            cx.masked(|| {
                let mut expired = AutomaticRetentionBudget::new();
                expired.request = crate::core::budget::RequestBudget::unbounded_at(
                    std::time::Instant::now() - Duration::from_secs(1),
                )
                .with_wall_clock(Duration::ZERO);
                assert!(reclaim_after_publish_with_budget(&index, &mut expired).is_none());
            });
        })?;
        assert_eq!(inventory(root.path())?, before);
        Ok(())
    }

    #[test]
    fn incomplete_parent_inventory_cannot_authorize_deletion() -> TestResult {
        let root = tempfile::tempdir()?;
        let parent = root.path().canonicalize()?;
        let index = parent.join("index");
        for name in ["index", "index.previous", ".index.reclaim-1-000"] {
            canary(&parent.join(name))?;
        }
        for number in 0..32 {
            std::fs::write(parent.join(format!("unrelated-{number}")), b"not an index")?;
        }
        let before = inventory(&parent)?;
        let mut budget = work_budget();
        budget.work_remaining = 8;
        assert!(reclaim_after_publish_with_budget(&index, &mut budget).is_none());
        assert_eq!(budget.work_remaining, 0);
        assert_eq!(inventory(&parent)?, before);
        // The explicit planner still inspects the complete parent and trees.
        let full = plan(&index, RETAINED_GENERATION_LIMIT)?;
        assert_eq!(full.reclaimable().count(), 2);
        assert!(full.reclaimable_bytes() > 0);
        Ok(())
    }

    #[test]
    fn an_already_bounded_retained_set_needs_no_tier_opens_or_tree_sizing() -> TestResult {
        let root = tempfile::tempdir()?;
        let parent = root.path().canonicalize()?;
        for name in ["index.previous", "index.previous.001"] {
            canary(&parent.join(name))?;
        }
        let before = inventory(&parent)?;
        let mut budget = work_budget();
        budget.validations_remaining = 0;
        // No manifest/tier is valid here. Keeping both must not pretend to
        // validate them, classify them unusable, size them, or remove them.
        let outcome = reclaim_after_publish_with_budget(&parent.join("index"), &mut budget)
            .ok_or("bounded set should require no validation")?;
        assert_eq!(outcome.kept, 2);
        assert!(outcome.reclaimed.is_empty());
        assert!(outcome.failures.is_empty());
        assert_eq!(inventory(&parent)?, before);
        assert_eq!(plan(&parent.join("index"), 2)?.reclaimable().count(), 2);
        Ok(())
    }

    fn candidate(sequence: u32, generation: Option<u64>) -> Candidate {
        Candidate {
            path: PathBuf::from(format!("index.previous.{sequence:03}")),
            sequence,
            generation,
            modified_nanos: u128::from(sequence),
        }
    }

    #[test]
    fn exhausted_validation_drops_every_provisional_disposition() {
        for first_valid in [false, true] {
            let mut budget = work_budget();
            budget.validations_remaining = 1;
            let mut opens = 0;
            let result = automatic_candidates(
                vec![
                    candidate(1, Some(7)),
                    candidate(2, Some(8)),
                    candidate(3, Some(9)),
                ],
                &mut budget,
                |_| {
                    opens += 1;
                    first_valid.then_some(9)
                },
            );
            assert!(result.is_err(), "no partial plan may reach the deleter");
            assert_eq!(opens, 1);
        }
    }

    #[test]
    fn automatic_selection_matches_full_distinct_generation_policy_when_it_finishes() -> TestResult
    {
        for count in 3..40_u32 {
            for reverse in [false, true] {
                let build = || {
                    let mut candidates: Vec<_> = (0..count)
                        .map(|number| {
                            candidate(number, (number % 7 != 0).then_some(u64::from(number / 3)))
                        })
                        .collect();
                    if reverse {
                        candidates.reverse();
                    }
                    candidates
                };
                let watermarks: BTreeMap<_, _> = build()
                    .into_iter()
                    .map(|entry| {
                        let valid = if entry.sequence % 5 == 0 {
                            None
                        } else {
                            entry.generation
                        };
                        (entry.path, valid)
                    })
                    .collect();
                let expected: Vec<_> =
                    classify_candidates(build(), 2, |path| watermarks.get(path).copied().flatten())
                        .collect();
                let mut budget = work_budget();
                budget.validations_remaining = usize::MAX;
                let (kept, reclaimed) = automatic_candidates(build(), &mut budget, |path| {
                    watermarks.get(path).copied().flatten()
                })?;
                assert_eq!(
                    kept,
                    expected.iter().filter(|(_, reason)| reason.keeps()).count()
                );
                assert_eq!(
                    reclaimed
                        .into_iter()
                        .map(|entry| (entry.path, entry.generation, entry.reason))
                        .collect::<Vec<_>>(),
                    expected
                        .into_iter()
                        .filter(|(_, reason)| !reason.keeps())
                        .map(|(entry, reason)| (entry.path, entry.generation, reason))
                        .collect::<Vec<_>>()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn automatic_cleanup_drains_backlog_in_bounded_batches_without_touching_sources() -> TestResult
    {
        let root = tempfile::tempdir()?;
        let parent = root.path().canonicalize()?;
        let index = parent.join("index");
        for name in [
            "index",
            "index.previous",
            "index.previous.001",
            ".index.publish-1-000",
        ] {
            canary(&parent.join(name))?;
        }
        std::fs::write(
            parent.join("ee.db"),
            b"source database is never retention work",
        )?;
        let protected = inventory(&parent)?;
        for number in 0..31 {
            canary(&parent.join(format!(".index.reclaim-1-{number:03}")))?;
        }
        let mut remaining = 31;
        let mut reclaimed_bytes = 0;
        while remaining > 0 {
            let outcome = reclaim_after_publish_with_budget(&index, &mut work_budget())
                .ok_or("bounded cleanup")?;
            assert!(outcome.failures.is_empty());
            assert_eq!(outcome.kept, 2);
            assert_eq!(outcome.reclaimed.len(), remaining.min(AUTO_RECLAIM_LIMIT));
            assert!(outcome.reclaimed.iter().all(|entry| {
                entry.reason == RetentionReason::InterruptedReclaim && !entry.path.exists()
            }));
            reclaimed_bytes += outcome.reclaimed_bytes();
            remaining -= outcome.reclaimed.len();
        }
        assert_eq!(
            reclaimed_bytes,
            31 * b"retained source and publication sentinel".len() as u64
        );
        assert_eq!(inventory(&parent)?, protected);
        assert!(
            reclaim_after_publish(&index)
                .ok_or("idempotent bounded cleanup")?
                .reclaimed
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn oversized_tree_is_deferred_intact_and_explicit_vacuum_can_finish_it() -> TestResult {
        let root = tempfile::tempdir()?;
        let parent = root.path().canonicalize()?;
        let index = parent.join("index");
        canary(&index)?;
        let oversized = parent.join(".index.reclaim-1-000");
        std::fs::create_dir(&oversized)?;
        // Root plus these files is one entry beyond the deletion allowance.
        for number in 0..AUTO_TREE_ENTRY_LIMIT {
            std::fs::write(oversized.join(format!("entry-{number}")), b"retained")?;
        }
        let before = inventory(&parent)?;
        let outcome = reclaim_after_publish_with_budget(&index, &mut work_budget())
            .ok_or("deletion admission is separate from candidate planning")?;
        assert!(outcome.reclaimed.is_empty());
        assert!(outcome.failures.is_empty());
        assert_eq!(inventory(&parent)?, before);
        let full = plan(&index, 2)?;
        assert_eq!(full.reclaimable().count(), 1);
        assert_eq!(full.reclaimable_bytes(), (AUTO_TREE_ENTRY_LIMIT * 8) as u64);
        let outcome = apply(&index, &full);
        assert_eq!(outcome.reclaimed.len(), 1);
        assert!(outcome.failures.is_empty());
        assert!(!oversized.exists());
        assert!(index.join("canary").is_file());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn bounded_tree_measurement_and_deletion_do_not_follow_internal_symlinks() -> TestResult {
        let root = tempfile::tempdir()?;
        let parent = root.path().canonicalize()?;
        let outside = parent.join("unrelated-data");
        canary(&outside)?;
        let leftover = parent.join(".index.reclaim-1-000");
        std::fs::create_dir(&leftover)?;
        std::os::unix::fs::symlink(&outside, leftover.join("outside"))?;
        assert_eq!(automatic_directory_bytes(&leftover, &mut work_budget())?, 0);
        let outcome = reclaim_after_publish_with_budget(&parent.join("index"), &mut work_budget())
            .ok_or("owned leftover containing a symlink")?;
        assert_eq!(outcome.reclaimed.len(), 1);
        assert_eq!(outcome.reclaimed_bytes(), 0);
        assert!(!leftover.exists());
        assert_eq!(
            std::fs::read(outside.join("canary"))?,
            b"retained source and publication sentinel"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retention_preserves_real_generations_and_releases_reader_authority() -> TestResult
    {
        use super::super::{
            IndexBuilder, IndexDocumentCounts, IndexGenerationLease, hash_fallback_embedder_stack,
            write_index_metadata,
        };

        let root = tempfile::tempdir()?;
        let parent = root.path().canonicalize()?;
        let index = parent.join("index");
        crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            let older = parent.join("index.previous");
            for (path, generation) in [
                (index.clone(), 9),
                (older.clone(), 7),
                (parent.join("index.previous.001"), 8),
                (parent.join("index.previous.002"), 8),
            ] {
                let documents = vec![crate::search::IndexableDocument::new(
                    "mem_bounded_retention",
                    "Index recovery preserves the complete captured source generation.",
                )];
                IndexBuilder::new(&path)
                    .with_embedder_stack(hash_fallback_embedder_stack())
                    .add_documents(documents.clone())
                    .build(&cx)
                    .await?;
                #[cfg(feature = "lexical-bm25")]
                super::super::build_lexical_tier(&cx, &path, &documents).await?;
                write_index_metadata(&path, generation, IndexDocumentCounts::memory_only(1), None)?;
            }
            let publisher = IndexGenerationLease::publish(&cx, &index).await?;
            let before = inventory(&parent)?;
            let active = inventory(&index)?;
            let mut refused = work_budget();
            refused.validations_remaining = 1;
            assert!(
                cx.masked(|| reclaim_after_publish_with_budget(&index, &mut refused))
                    .is_none()
            );
            assert_eq!(inventory(&parent)?, before);
            let outcome = cx
                .masked(|| reclaim_after_publish_with_budget(&index, &mut work_budget()))
                .ok_or("bounded retention with complete validation")?;
            assert_eq!(outcome.kept, 2);
            assert_eq!(outcome.reclaimed.len(), 1);
            assert_eq!(
                outcome.reclaimed[0].reason,
                RetentionReason::DuplicateGeneration
            );
            assert!(outcome.failures.is_empty());
            assert_eq!(inventory(&index)?, active);
            assert_eq!(recoverable_index_generation(&older), Some(7));
            drop(publisher);
            let reader = IndexGenerationLease::read(&cx, &index).await?;
            assert_eq!(reader.index_for_snapshot(&cx, &index, 7)?, older);
            assert_eq!(reader.index_for_snapshot(&cx, &index, 9)?, index);
            Ok::<(), Box<dyn std::error::Error>>(())
        })??;
        Ok(())
    }
}
