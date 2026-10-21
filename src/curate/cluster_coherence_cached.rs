//! Exact average-linkage scheduling with bounded, invocation-local reuse.
//!
//! Only pairs touching the newly merged cluster need new source arithmetic.
//! Each row keeps its best eligible pair; an ordered frontier contains at most
//! one entry per row, with no stale heap entries. Recompute changed linkages
//! using the original member traversal, NOT a weighted update of old means.
//! That preserves floating-point bits, threshold boundaries and merge order.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use super::{FLOAT_EPSILON, average_linkage_similarity, merge::best_merge};

// At most 8 MiB of f64 linkage payload, plus O(n) row/frontier state. This is
// extra workspace, not a bound on the original cosine matrix or total RSS.
// Larger inputs (or a refused allocation) keep the original streaming path.
const MAX_CACHED_LINKAGES: usize = 1_048_576;

pub(super) fn cluster(similarities: &[Vec<f64>], threshold: f64) -> Vec<Vec<usize>> {
    cluster_observed(similarities, threshold, MAX_CACHED_LINKAGES, |_, _, _| {})
}

fn cluster_observed(
    similarities: &[Vec<f64>],
    threshold: f64,
    limit: usize,
    mut on_merge: impl FnMut(&[usize], &[usize], f64),
) -> Vec<Vec<usize>> {
    let mut members = (0..similarities.len())
        .map(|point| vec![point])
        .collect::<Vec<_>>();
    let Some(scores) = Scores::new(members.len(), limit) else {
        // No truncation, changed threshold or partial clustering on refusal.
        while let Some((left, right, score)) = best_merge(&members, similarities, threshold) {
            on_merge(&members[left], &members[right], score);
            let retired = members.remove(right);
            members[left].extend(retired);
            members[left].sort_unstable();
        }
        return members;
    };
    cached(
        members,
        scores,
        threshold,
        |left, right| average_linkage_similarity(left, right, similarities),
        on_merge,
    )
}

struct Scores {
    width: usize,
    values: Vec<f64>,
}

impl Scores {
    fn new(width: usize, limit: usize) -> Option<Self> {
        let pairs = width.checked_mul(width.saturating_sub(1))? / 2;
        if pairs > limit.min(MAX_CACHED_LINKAGES) {
            return None;
        }
        let mut values = Vec::new();
        values.try_reserve_exact(pairs).ok()?;
        values.resize(pairs, 0.0);
        Some(Self { width, values })
    }

    fn index(&self, left: usize, right: usize) -> usize {
        debug_assert!(left < right && right < self.width);
        // Width is bounded by the admitted triangular payload above.
        left * self.width - left * (left + 1) / 2 + right - left - 1
    }

    fn set(&mut self, left: usize, right: usize, score: f64) {
        let index = self.index(left, right);
        self.values[index] = score;
    }

    fn candidate(&self, left: usize, right: usize, threshold: f64) -> Option<Candidate> {
        let score = self.values[self.index(left, right)];
        if score + FLOAT_EPSILON < threshold {
            None
        } else {
            Some(Candidate { left, right, score })
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    left: usize,
    right: usize,
    score: f64,
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| (other.left, other.right).cmp(&(self.left, self.right)))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Candidate {}

fn row_best(
    left: usize,
    members: &[Vec<usize>],
    scores: &Scores,
    threshold: f64,
) -> Option<Candidate> {
    (left + 1..members.len())
        .filter(|right| !members[*right].is_empty())
        .filter_map(|right| scores.candidate(left, right, threshold))
        .max()
}

fn cached(
    mut members: Vec<Vec<usize>>,
    mut scores: Scores,
    threshold: f64,
    mut score: impl FnMut(&[usize], &[usize]) -> f64,
    mut on_merge: impl FnMut(&[usize], &[usize], f64),
) -> Vec<Vec<usize>> {
    let count = members.len();
    let mut rows = vec![None; count];
    let mut frontier = BTreeSet::new();
    for left in 0..count {
        for right in left + 1..count {
            // Even singleton arithmetic goes through the original reducer;
            // copying the matrix cell would treat signed zero differently.
            scores.set(left, right, score(&members[left], &members[right]));
        }
        rows[left] = row_best(left, &members, &scores, threshold);
        if let Some(candidate) = rows[left] {
            frontier.insert(candidate);
        }
    }

    // Slots never move. A live slot is its cluster's smallest original point;
    // retired slots are empty. For disjoint nonempty clusters, lexicographic
    // union order is exactly order by the sorted pair of smallest points:
    // different first points decide immediately; a shared first cluster leaves
    // the two disjoint other clusters' first points as the first difference.
    // This invariant preserves the old full-membership tie-break. It is not a
    // shortcut for arbitrary overlapping or unsorted cluster inputs.
    while let Some(winner) = frontier.last().copied() {
        let Candidate { left, right, .. } = winner;
        on_merge(&members[left], &members[right], winner.score);
        let retired = std::mem::take(&mut members[right]);
        members[left].extend(retired);
        members[left].sort_unstable();

        // Keep operand/member iteration order identical to a fresh old scan.
        // A symmetric matrix alone does not justify reversing summation order.
        for other in 0..count {
            if other == left || members[other].is_empty() {
                continue;
            }
            let (a, b) = (left.min(other), left.max(other));
            scores.set(a, b, score(&members[a], &members[b]));
        }
        for row in 0..count {
            let previous = rows[row];
            let next = if members[row].is_empty() {
                None
            } else if row == left
                || previous.is_some_and(|entry| entry.right == left || entry.right == right)
            {
                // The old winner changed or disappeared: another unchanged
                // pair can now win this row, including one below the old max.
                row_best(row, &members, &scores, threshold)
            } else if row < left {
                // The old winner remains exact; only the new pair can beat it.
                previous.max(scores.candidate(row, left, threshold))
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
#[path = "cluster_coherence_cached_tests.rs"]
mod tests;
