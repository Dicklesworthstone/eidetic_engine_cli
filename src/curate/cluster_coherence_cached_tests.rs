use super::*;

type Trace = Vec<(Vec<usize>, Vec<usize>, u64)>;

// Literal original candidate materialization and comparison. Do not share the
// cached scheduler's representative ordering, linkage table or row maxima.
fn reference(matrix: &[Vec<f64>], threshold: f64) -> (Vec<Vec<usize>>, Trace, usize) {
    let mut clusters = (0..matrix.len()).map(|i| vec![i]).collect::<Vec<_>>();
    let mut trace = Vec::new();
    let mut calls = 0;
    loop {
        let mut best: Option<(usize, usize, f64, Vec<usize>)> = None;
        for left in 0..clusters.len() {
            for right in left + 1..clusters.len() {
                calls += 1;
                let similarity =
                    average_linkage_similarity(&clusters[left], &clusters[right], matrix);
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
        let Some((left, right, score, merged)) = best else {
            return (clusters, trace, calls);
        };
        trace.push((clusters[left].clone(), clusters[right].clone(), score.to_bits()));
        clusters[left] = merged;
        clusters.remove(right);
    }
}

fn observed(matrix: &[Vec<f64>], threshold: f64, limit: usize) -> (Vec<Vec<usize>>, Trace) {
    let mut trace = Vec::new();
    let clusters = cluster_observed(matrix, threshold, limit, |left, right, score| {
        trace.push((left.to_vec(), right.to_vec(), score.to_bits()));
    });
    (clusters, trace)
}

fn next(random: &mut u64) -> u64 {
    *random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
    *random
}

#[test]
fn every_cached_and_streamed_merge_matches_the_original_bits() {
    let mut random = 91_u64;
    let levels = [-1.0, -0.0, 0.0, 0.5 - FLOAT_EPSILON, 0.5, 0.75, 1.0];
    for case in 0..2048 {
        let count = 2 + (next(&mut random) % 17) as usize;
        let mut matrix = vec![vec![1.0; count]; count];
        for left in 0..count {
            for right in left + 1..count {
                let score = if case % 3 == 0 {
                    ((next(&mut random) >> 11) as f64 / ((1_u64 << 53) as f64)) * 2.0 - 1.0
                } else {
                    levels[(next(&mut random) as usize) % levels.len()]
                };
                matrix[left][right] = score;
                matrix[right][left] = score;
            }
        }
        let threshold = [0.0, 0.5, 0.75, 1.0][case % 4];
        let (clusters, trace, _) = reference(&matrix, threshold);
        for limit in [0, MAX_CACHED_LINKAGES] {
            assert_eq!(observed(&matrix, threshold, limit), (clusters.clone(), trace.clone()));
        }
    }
}

#[test]
fn a_changed_row_winner_cannot_hide_an_unchanged_runner_up() {
    let matrix = vec![
        vec![1.0, 0.9, -0.9, 0.8, 0.1],
        vec![0.9, 1.0, 0.99, 0.1, 0.1],
        vec![-0.9, 0.99, 1.0, 0.1, 0.1],
        vec![0.8, 0.1, 0.1, 1.0, 0.7],
        vec![0.1, 0.1, 0.1, 0.7, 1.0],
    ];
    let (clusters, trace) = observed(&matrix, 0.5, MAX_CACHED_LINKAGES);
    assert_eq!(trace.len(), 2);
    assert_eq!((&trace[0].0, &trace[0].1), (&vec![1], &vec![2]));
    assert_eq!((&trace[1].0, &trace[1].1), (&vec![0], &vec![3]));
    assert_eq!(clusters, vec![vec![0, 3], vec![1, 2], vec![4]]);
    let (expected, decisions, _) = reference(&matrix, 0.5);
    assert_eq!((clusters, trace), (expected, decisions));
}

#[test]
fn empty_singleton_signed_zero_and_threshold_edges_keep_the_same_trace() {
    for matrix in [vec![], vec![vec![1.0]]] {
        let (expected, trace, _) = reference(&matrix, 0.5);
        assert_eq!(observed(&matrix, 0.5, 0), (expected.clone(), trace.clone()));
        assert_eq!(observed(&matrix, 0.5, MAX_CACHED_LINKAGES), (expected, trace));
    }
    for value in [-1.0, -0.0, 0.0, 0.5 - FLOAT_EPSILON, 0.5, 1.0] {
        let matrix = vec![
            vec![1.0, value, value],
            vec![value, 1.0, value],
            vec![value, value, 1.0],
        ];
        for threshold in [0.0, 0.5, 0.5 + FLOAT_EPSILON, 1.0] {
            let (expected, trace, _) = reference(&matrix, threshold);
            assert_eq!(observed(&matrix, threshold, MAX_CACHED_LINKAGES), (expected, trace));
        }
    }
}

#[test]
fn triangular_cache_admission_is_bounded_and_refusal_keeps_complete_results() {
    assert!(Scores::new(usize::MAX, usize::MAX).is_none());
    assert!(Scores::new(2000, usize::MAX).is_none());
    for width in 0_usize..32 {
        let pairs = width * width.saturating_sub(1) / 2;
        let mut scores = Scores::new(width, pairs).expect("exact bound");
        assert_eq!(scores.values.len(), pairs);
        let mut indices = BTreeSet::new();
        for left in 0..width {
            for right in left + 1..width {
                indices.insert(scores.index(left, right));
                scores.set(left, right, (left * width + right) as f64);
            }
        }
        assert_eq!(indices, (0..pairs).collect());
        for left in 0..width {
            for right in left + 1..width {
                assert_eq!(
                    scores.candidate(left, right, 0.0).unwrap().score,
                    (left * width + right) as f64
                );
            }
        }
        if pairs > 0 {
            assert!(Scores::new(width, pairs - 1).is_none());
            let matrix = vec![vec![1.0; width]; width];
            assert_eq!(observed(&matrix, 0.5, pairs - 1), observed(&matrix, 0.5, pairs));
        }
    }
}

#[test]
fn dense_history_computes_each_exact_pair_once_instead_of_each_scan() {
    let count = 128;
    let matrix = vec![vec![1.0; count]; count];
    let mut calls = 0;
    let mut trace = Vec::new();
    let actual = cached(
        (0..count).map(|i| vec![i]).collect(),
        Scores::new(count, MAX_CACHED_LINKAGES).unwrap(),
        0.55,
        |left, right| {
            calls += 1;
            average_linkage_similarity(left, right, &matrix)
        },
        |left, right, score| trace.push((left.to_vec(), right.to_vec(), score.to_bits())),
    );
    let (expected, decisions, original_calls) = reference(&matrix, 0.55);
    assert_eq!((actual, trace), (expected, decisions));
    assert_eq!(calls, (count - 1) * (count - 1));
    assert_eq!(original_calls, count * (count - 1) * (count + 1) / 6);
    assert!(calls * 20 < original_calls);
}

#[test]
fn unchanged_member_pairs_are_never_reduced_twice_and_cache_is_not_shared() {
    let count = 16;
    let mut matrix = vec![vec![1.0; count]; count];
    for left in 0..count {
        for right in left + 1..count {
            let value = if left % 4 == right % 4 { 0.9 } else { 0.1 };
            matrix[left][right] = value;
            matrix[right][left] = value;
        }
    }
    let before = matrix.clone();
    let mut pairs = BTreeSet::new();
    let first = cached(
        (0..count).map(|i| vec![i]).collect(),
        Scores::new(count, MAX_CACHED_LINKAGES).unwrap(),
        0.5,
        |left, right| {
            assert!(pairs.insert((left.to_vec(), right.to_vec())));
            average_linkage_similarity(left, right, &matrix)
        },
        |_, _, _| {},
    );
    assert_eq!(matrix, before);
    assert_eq!(first, reference(&matrix, 0.5).0);
    assert_eq!(first.len(), 4);
    matrix.iter_mut().for_each(|row| row.fill(1.0));
    assert_eq!(cluster(&matrix, 0.5), vec![(0..count).collect::<Vec<_>>()]);
    assert_eq!(cluster(&before, 0.5), first);
}

#[test]
fn representative_ties_preserve_interleaved_members_not_only_singletons() {
    // First build interleaved clusters, then tie their means exactly.
    let count = 12;
    let matrix = (0..count)
        .map(|left| {
            (0..count)
                .map(|right| if left % 3 == right % 3 { 1.0 } else { 0.5 })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let (expected, trace, _) = reference(&matrix, 0.5);
    assert!(trace.iter().any(|(left, right, _)| left.len() > 1 && right.len() > 1));
    assert_eq!(observed(&matrix, 0.5, MAX_CACHED_LINKAGES), (expected, trace));
}

#[test]
fn public_reports_keep_membership_scores_admission_and_centroid_identity() {
    use super::super::{
        ClusterCoherenceConfig, EmbeddingPoint, agglomerate, coherent_cluster,
        normalized_points, pairwise_cosine_matrix,
    };

    let points = [0.0_f64, 5.0, 10.0, 45.0, 50.0, 55.0, 170.0]
        .into_iter()
        .enumerate()
        .map(|(index, angle)| {
            let angle = angle.to_radians();
            EmbeddingPoint::new(format!("mem_{index:02}"), vec![angle.cos(), angle.sin()])
        })
        .collect::<Vec<_>>();
    let points = normalized_points(&points).unwrap();
    let matrix = pairwise_cosine_matrix(&points).unwrap();
    for threshold in [0.0, 0.55, 0.95, 1.0] {
        let config = ClusterCoherenceConfig { merge_threshold: threshold, ..Default::default() };
        let expected_members = reference(&matrix, threshold).0;
        let expected = expected_members.iter().map(|members| {
            coherent_cluster(members, &expected_members, &points, &matrix, config)
        }).collect::<Vec<_>>();
        let actual = agglomerate(&points, config).unwrap();
        assert_eq!(actual.clusters, expected);
        let mut reversed = points.clone();
        reversed.reverse();
        assert_eq!(agglomerate(&reversed, config).unwrap(), actual);
    }
}
