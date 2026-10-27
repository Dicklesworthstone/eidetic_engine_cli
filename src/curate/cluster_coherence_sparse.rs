//! Exact row winners when the triangular linkage cache is not affordable.
//!
//! Retain one best pair per row and the newly merged cluster's linkages, not
//! every pair. An unchanged winner remains authoritative unless it touches a
//! changed/retired cluster; only then rescan that row. Reuse each new linkage
//! during those rescans so it is reduced once per merge. This trades possible
//! rescoring of unchanged runner-ups for O(n) extra scheduling storage.
//!
//! Scores still use the original ordered member reduction. There is no weighted
//! mean recurrence, approximation, truncated result or cross-invocation state.

use std::collections::BTreeSet;

use super::{Candidate, FLOAT_EPSILON};

pub(super) struct Workspace {
    rows: Vec<Option<Candidate>>,
    changed: Vec<f64>,
}

impl Workspace {
    /// Reserve the two linear buffers before beginning work. Refusal leaves
    /// the caller free to run its complete original streaming implementation.
    pub(super) fn new(width: usize) -> Option<Self> {
        let mut rows = Vec::new();
        rows.try_reserve_exact(width).ok()?;
        rows.resize(width, None);
        let mut changed = Vec::new();
        changed.try_reserve_exact(width).ok()?;
        changed.resize(width, 0.0);
        Some(Self { rows, changed })
    }
}

fn candidate(left: usize, right: usize, score: f64, threshold: f64) -> Option<Candidate> {
    if score + FLOAT_EPSILON < threshold {
        None
    } else {
        Some(Candidate { left, right, score })
    }
}

fn row_best(
    left: usize,
    members: &[Vec<usize>],
    threshold: f64,
    changed: Option<(usize, &[f64])>,
    score: &mut impl FnMut(&[usize], &[usize]) -> f64,
) -> Option<Candidate> {
    let mut best = None;
    for right in left + 1..members.len() {
        if members[right].is_empty() {
            continue;
        }
        let value = match changed {
            Some((merged, scores)) if left == merged => scores[right],
            Some((merged, scores)) if right == merged => scores[left],
            _ => score(&members[left], &members[right]),
        };
        best = best.max(candidate(left, right, value, threshold));
    }
    best
}

/// Starts from singleton slots in original point order, just like the dense
/// scheduler. A live slot remains its cluster's smallest member. Candidate's
/// ordering therefore retains the full sorted-union tie-break for partitions.
pub(super) fn cluster(
    mut members: Vec<Vec<usize>>,
    workspace: Workspace,
    threshold: f64,
    mut score: impl FnMut(&[usize], &[usize]) -> f64,
    mut on_merge: impl FnMut(&[usize], &[usize], f64),
) -> Vec<Vec<usize>> {
    let count = members.len();
    let Workspace {
        mut rows,
        mut changed,
    } = workspace;
    debug_assert_eq!(rows.len(), count);
    debug_assert_eq!(changed.len(), count);
    let mut frontier = BTreeSet::new();
    for left in 0..count {
        rows[left] = row_best(left, &members, threshold, None, &mut score);
        if let Some(winner) = rows[left] {
            frontier.insert(winner);
        }
    }

    while let Some(winner) = frontier.last().copied() {
        let Candidate { left, right, .. } = winner;
        on_merge(&members[left], &members[right], winner.score);
        let retired = std::mem::take(&mut members[right]);
        members[left].extend(retired);
        members[left].sort_unstable();

        // Compute new pairs once, retaining their original operand order.
        // Empty/retired slots are never read from this reusable scratch row.
        for other in 0..count {
            if other != left && !members[other].is_empty() {
                let (a, b) = (left.min(other), left.max(other));
                changed[other] = score(&members[a], &members[b]);
            }
        }
        for row in 0..count {
            let previous = rows[row];
            let next = if members[row].is_empty() {
                None
            } else if row == left
                || previous.is_some_and(|entry| entry.right == left || entry.right == right)
            {
                // A removed/decreased winner can expose any unchanged pair,
                // not just the merged pair. No stale row winner may survive.
                row_best(row, &members, threshold, Some((left, &changed)), &mut score)
            } else if row < left {
                // All older pairs in this row are unchanged; the new pair is
                // the only candidate that can beat its retained exact winner.
                previous.max(candidate(row, left, changed[row], threshold))
            } else {
                previous
            };
            if next != previous {
                if let Some(previous) = previous {
                    frontier.remove(&previous);
                }
                if let Some(next) = next {
                    frontier.insert(next);
                }
                rows[row] = next;
            }
        }
        debug_assert!(frontier.len() <= count);
    }
    members.into_iter().filter(|row| !row.is_empty()).collect()
}

#[cfg(test)]
#[path = "cluster_coherence_sparse_tests.rs"]
mod tests;
