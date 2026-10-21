//! Select the existing average-linkage winner without allocating candidates.
//!
//! Every cluster is sorted by point position. Compare tied merged memberships
//! as borrowed streams instead of cloning and sorting one Vec for every pair.
//! Similarity arithmetic, epsilon admission and deterministic ties are unchanged;
//! the parent materializes only the winning merge. No linkage cache or rounded
//! score changes the comparisons that decide a curation cluster.

use std::cmp::Ordering;

use super::{FLOAT_EPSILON, average_linkage_similarity};

pub(super) fn best_merge(
    clusters: &[Vec<usize>],
    similarities: &[Vec<f64>],
    threshold: f64,
) -> Option<(usize, usize, f64)> {
    let mut best: Option<(usize, usize, f64)> = None;
    for left in 0..clusters.len() {
        for right in (left + 1)..clusters.len() {
            let similarity =
                average_linkage_similarity(&clusters[left], &clusters[right], similarities);
            if similarity + FLOAT_EPSILON < threshold {
                continue;
            }
            let replace = match &best {
                None => true,
                Some((best_left, best_right, best_similarity)) => {
                    similarity
                        .total_cmp(best_similarity)
                        .then_with(|| {
                            merged_members(&clusters[left], &clusters[right])
                                .cmp(merged_members(&clusters[*best_left], &clusters[*best_right]))
                                .reverse()
                        })
                        .then_with(|| (left, right).cmp(&(*best_left, *best_right)).reverse())
                        == Ordering::Greater
                }
            };
            if replace {
                best = Some((left, right, similarity));
            }
        }
    }
    best
}

/// Merge sorted slices lazily, retaining multiplicity just like the original
/// concatenate-and-sort operation. Real cluster partitions are disjoint, but
/// comparison correctness must not depend on silently deduplicating members.
fn merged_members<'a>(
    mut left: &'a [usize],
    mut right: &'a [usize],
) -> impl Iterator<Item = usize> + 'a {
    std::iter::from_fn(move || {
        let take_left = match (left.first(), right.first()) {
            (Some(left), Some(right)) => left <= right,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return None,
        };
        let next = if take_left {
            let value = left[0];
            left = &left[1..];
            value
        } else {
            let value = right[0];
            right = &right[1..];
            value
        };
        Some(next)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Literal pre-change selector, including candidate ownership and all tie
    /// terms. Keep this independent from the streaming comparison above.
    fn original_best_merge(
        clusters: &[Vec<usize>],
        similarities: &[Vec<f64>],
        threshold: f64,
    ) -> Option<(usize, usize, f64)> {
        let mut best: Option<(usize, usize, f64, Vec<usize>)> = None;
        for left in 0..clusters.len() {
            for right in (left + 1)..clusters.len() {
                let similarity =
                    average_linkage_similarity(&clusters[left], &clusters[right], similarities);
                if similarity + FLOAT_EPSILON < threshold {
                    continue;
                }
                let mut merged = clusters[left].clone();
                merged.extend(clusters[right].iter().copied());
                merged.sort_unstable();
                let replace = match &best {
                    None => true,
                    Some((best_left, best_right, best_similarity, best_members)) => {
                        similarity
                            .total_cmp(best_similarity)
                            .then_with(|| merged.cmp(best_members).reverse())
                            .then_with(|| (left, right).cmp(&(*best_left, *best_right)).reverse())
                            == Ordering::Greater
                    }
                };
                if replace {
                    best = Some((left, right, similarity, merged));
                }
            }
        }
        best.map(|(left, right, similarity, _members)| (left, right, similarity))
    }

    fn same_winner(
        actual: Option<(usize, usize, f64)>,
        expected: Option<(usize, usize, f64)>,
    ) {
        let bits = |winner: Option<(usize, usize, f64)>| {
            winner.map(|(left, right, similarity)| (left, right, similarity.to_bits()))
        };
        assert_eq!(bits(actual), bits(expected));
    }

    #[test]
    fn streamed_members_equal_concatenation_and_sort_including_empty_and_repeated_values() {
        let samples = [
            vec![],
            vec![0],
            vec![0, 0, 2],
            vec![1, 3, 5],
            vec![0, 2, 4, 6],
            vec![usize::MAX],
        ];
        for left in &samples {
            for right in &samples {
                let mut expected = left.clone();
                expected.extend(right);
                expected.sort_unstable();
                let mut actual = merged_members(left, right);
                assert_eq!(actual.by_ref().collect::<Vec<_>>(), expected);
                assert_eq!(actual.next(), None);
                assert_eq!(actual.next(), None);
            }
        }
    }

    #[test]
    fn equal_scores_compare_merged_members_before_cluster_positions() {
        let clusters = vec![vec![4, 5], vec![0, 3], vec![1, 2]];
        let matrix = vec![vec![0.75; 6]; 6];
        same_winner(best_merge(&clusters, &matrix, 0.5), Some((1, 2, 0.75)));
        // Identical memberships retain the final position tie-break as well.
        let repeated = vec![vec![0], vec![0], vec![0]];
        same_winner(best_merge(&repeated, &matrix, 0.5), Some((0, 1, 0.75)));
    }

    #[test]
    fn a_strictly_better_score_wins_before_lexicographic_membership() {
        let clusters = vec![vec![0], vec![1], vec![2]];
        let matrix = vec![
            vec![1.0, 0.75, 0.80],
            vec![0.75, 1.0, 0.95],
            vec![0.80, 0.95, 1.0],
        ];
        same_winner(best_merge(&clusters, &matrix, 0.5), Some((1, 2, 0.95)));
        same_winner(best_merge(&clusters, &matrix, 0.96), None);
    }

    #[test]
    fn empty_singleton_and_epsilon_threshold_boundaries_keep_the_old_decision() {
        same_winner(best_merge(&[], &[], 0.5), None);
        same_winner(best_merge(&[vec![0]], &[vec![1.0]], 0.5), None);
        let clusters = vec![vec![0], vec![1]];
        for score in [-1.0, -0.0, 0.0, 0.5 - FLOAT_EPSILON, 0.5, 1.0] {
            let matrix = vec![vec![1.0, score], vec![score, 1.0]];
            for threshold in [0.0, 0.5, 0.5 + FLOAT_EPSILON, 1.0] {
                same_winner(
                    best_merge(&clusters, &matrix, threshold),
                    original_best_merge(&clusters, &matrix, threshold),
                );
            }
        }
    }

    fn next(random: &mut u64) -> u64 {
        *random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        *random
    }

    #[test]
    fn every_merge_matches_the_original_selector_across_ties_and_source_orders() {
        let mut random = 37_u64;
        let levels = [-0.5, 0.0, 0.5 - FLOAT_EPSILON, 0.5, 0.75, 1.0];
        for case in 0..2048 {
            let count = 2 + (next(&mut random) % 15) as usize;
            let mut matrix = vec![vec![1.0; count]; count];
            for left in 0..count {
                for right in left + 1..count {
                    let score = levels[(next(&mut random) as usize) % levels.len()];
                    matrix[left][right] = score;
                    matrix[right][left] = score;
                }
            }
            let threshold = [0.0, 0.5, 0.75, 1.0][case % 4];
            let mut clusters = (0..count).map(|point| vec![point]).collect::<Vec<_>>();
            if case % 2 == 0 {
                clusters.reverse();
            }
            clusters.rotate_left(case % count);
            loop {
                let expected = original_best_merge(&clusters, &matrix, threshold);
                let actual = best_merge(&clusters, &matrix, threshold);
                same_winner(actual, expected);
                let Some((left, right, _)) = actual else {
                    break;
                };
                let mut members = clusters[left].clone();
                members.extend(clusters[right].iter().copied());
                members.sort_unstable();
                clusters[left] = members;
                clusters.remove(right);
            }
        }
    }

    #[test]
    fn dense_equal_similarity_runs_keep_exact_membership_and_merge_order() {
        let count = 128;
        let matrix = vec![vec![1.0; count]; count];
        let mut clusters = (0..count).rev().map(|point| vec![point]).collect::<Vec<_>>();
        let mut merges = 0;
        while let Some(actual) = best_merge(&clusters, &matrix, 0.55) {
            same_winner(Some(actual), original_best_merge(&clusters, &matrix, 0.55));
            let (left, right, _) = actual;
            let mut members = clusters[left].clone();
            members.extend(clusters[right].iter().copied());
            members.sort_unstable();
            clusters[left] = members;
            clusters.remove(right);
            merges += 1;
        }
        assert_eq!(merges, count - 1);
        assert_eq!(clusters, vec![(0..count).collect::<Vec<_>>()]);
    }
}
