use super::*;
use std::cmp::Ordering;

use super::super::{MAX_CACHED_LINKAGES, Scores, average_linkage_similarity, cluster_observed};

type Trace = Vec<(Vec<usize>, Vec<usize>, u64)>;

// Independent literal full scan: materialize every candidate's union and use
// its complete membership for ties, not sparse slots or retained row winners.
fn reference(matrix: &[Vec<f64>], threshold: f64) -> (Vec<Vec<usize>>, Trace, usize) {
    let mut members = (0..matrix.len()).map(|i| vec![i]).collect::<Vec<_>>();
    let mut trace = Vec::new();
    let mut calls = 0;
    loop {
        let mut best: Option<(usize, usize, f64, Vec<usize>)> = None;
        for left in 0..members.len() {
            for right in left + 1..members.len() {
                calls += 1;
                let score = average_linkage_similarity(&members[left], &members[right], matrix);
                if score + FLOAT_EPSILON < threshold {
                    continue;
                }
                let mut union = members[left].clone();
                union.extend(members[right].iter().copied());
                union.sort_unstable();
                let replace = match &best {
                    None => true,
                    Some((a, b, previous, previous_union)) => {
                        score
                            .total_cmp(previous)
                            .then_with(|| union.cmp(previous_union).reverse())
                            .then_with(|| (left, right).cmp(&(*a, *b)).reverse())
                            == Ordering::Greater
                    }
                };
                if replace {
                    best = Some((left, right, score, union));
                }
            }
        }
        let Some((left, right, score, union)) = best else {
            return (members, trace, calls);
        };
        trace.push((
            members[left].clone(),
            members[right].clone(),
            score.to_bits(),
        ));
        members[left] = union;
        members.remove(right);
    }
}

fn observed(matrix: &[Vec<f64>], threshold: f64) -> (Vec<Vec<usize>>, Trace, usize) {
    let count = matrix.len();
    let mut trace = Vec::new();
    let mut calls = 0;
    let members = cluster(
        (0..count).map(|i| vec![i]).collect(),
        Workspace::new(count).expect("linear state"),
        threshold,
        |left, right| {
            calls += 1;
            average_linkage_similarity(left, right, matrix)
        },
        |left, right, score| trace.push((left.to_vec(), right.to_vec(), score.to_bits())),
    );
    (members, trace, calls)
}

fn next(random: &mut u64) -> u64 {
    *random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
    *random
}

#[test]
fn sparse_traces_match_full_member_ties_and_exact_operand_order() {
    let mut random = 613_u64;
    let levels = [-1.0, -0.0, 0.0, 0.5 - FLOAT_EPSILON, 0.5, 0.75, 1.0];
    for case in 0..2048 {
        let count = 2 + (next(&mut random) % 17) as usize;
        let mut matrix = vec![vec![1.0; count]; count];
        for left in 0..count {
            for right in left + 1..count {
                let value = if case % 3 == 0 {
                    ((next(&mut random) >> 11) as f64 / ((1_u64 << 53) as f64)) * 2.0 - 1.0
                } else {
                    levels[(next(&mut random) as usize) % levels.len()]
                };
                matrix[left][right] = value;
                // Deliberately asymmetric fixtures expose accidental operand
                // reversal; the production cosine matrix is symmetric.
                matrix[right][left] = if case % 5 == 0 {
                    levels[(next(&mut random) as usize) % levels.len()]
                } else {
                    value
                };
            }
        }
        let threshold = [0.0, 0.5, 0.5 + FLOAT_EPSILON, 0.75, 1.0][case % 5];
        let (expected, expected_trace, _) = reference(&matrix, threshold);
        let (actual, trace, _) = observed(&matrix, threshold);
        assert_eq!((actual, trace), (expected.clone(), expected_trace.clone()));
        for limit in [0, MAX_CACHED_LINKAGES] {
            let mut trace = Vec::new();
            let members = cluster_observed(&matrix, threshold, limit, |a, b, score| {
                trace.push((a.to_vec(), b.to_vec(), score.to_bits()));
            });
            assert_eq!((members, trace), (expected.clone(), expected_trace.clone()));
        }
    }
}

#[test]
fn invalidated_sparse_winners_recover_unchanged_runner_up_pairs() {
    let matrix = vec![
        vec![1.0, 0.9, -0.9, 0.8, 0.1],
        vec![0.9, 1.0, 0.99, 0.1, 0.1],
        vec![-0.9, 0.99, 1.0, 0.1, 0.1],
        vec![0.8, 0.1, 0.1, 1.0, 0.7],
        vec![0.1, 0.1, 0.1, 0.7, 1.0],
    ];
    let (members, trace, _) = observed(&matrix, 0.5);
    assert_eq!(trace.len(), 2);
    assert_eq!((&trace[0].0, &trace[0].1), (&vec![1], &vec![2]));
    assert_eq!((&trace[1].0, &trace[1].1), (&vec![0], &vec![3]));
    assert_eq!(members, vec![vec![0, 3], vec![1, 2], vec![4]]);
    let (expected, expected_trace, _) = reference(&matrix, 0.5);
    assert_eq!((members, trace), (expected, expected_trace));
}

#[test]
fn sparse_scratch_never_reduces_a_pair_twice_in_one_merge_phase() {
    use std::cell::RefCell;

    let count = 24;
    let matrix = (0..count)
        .map(|a| {
            (0..count)
                .map(|b| if a % 4 == b % 4 { 1.0 } else { 0.5 })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let before = matrix.clone();
    let phase_pairs = RefCell::new(BTreeSet::new());
    let mut trace = Vec::new();
    let members = cluster(
        (0..count).map(|i| vec![i]).collect(),
        Workspace::new(count).unwrap(),
        0.5,
        |a, b| {
            assert!(a[0] < b[0]);
            assert!(phase_pairs.borrow_mut().insert((a.to_vec(), b.to_vec())));
            average_linkage_similarity(a, b, &matrix)
        },
        |a, b, score| {
            trace.push((a.to_vec(), b.to_vec(), score.to_bits()));
            phase_pairs.borrow_mut().clear();
        },
    );
    let (expected, expected_trace, _) = reference(&matrix, 0.5);
    assert_eq!((members, trace), (expected, expected_trace));
    assert_eq!(
        matrix, before,
        "scheduling must not modify its source matrix"
    );
}

#[test]
fn dense_sparse_history_avoids_repeated_full_scans_without_a_pair_table() {
    let count = 128;
    let matrix = vec![vec![1.0; count]; count];
    let (members, trace, calls) = observed(&matrix, 0.55);
    let (expected, expected_trace, original_calls) = reference(&matrix, 0.55);
    assert_eq!((members, trace), (expected, expected_trace));
    assert_eq!(calls, (count - 1) * (count - 1));
    assert_eq!(original_calls, count * (count - 1) * (count + 1) / 6);
    assert!(calls * 20 < original_calls);
}

#[test]
fn first_input_above_dense_cap_retains_every_subject_through_production_dispatch() {
    let mut count = 2_usize;
    while count * (count - 1) / 2 <= MAX_CACHED_LINKAGES {
        count += 1;
    }
    assert_eq!(count, 1449);
    assert!(Scores::new(count, MAX_CACHED_LINKAGES).is_none());
    assert!(Scores::new(count - 1, MAX_CACHED_LINKAGES).is_some());
    let state = Workspace::new(count).expect("only linear scheduling buffers");
    assert_eq!(state.rows.len(), count);
    assert_eq!(state.changed.len(), count);
    let mut matrix = vec![vec![0.0; count]; count];
    for (index, row) in matrix.iter_mut().enumerate() {
        row[index] = 1.0;
    }
    let members = super::super::cluster(&matrix, 0.55);
    assert_eq!(members, (0..count).map(|i| vec![i]).collect::<Vec<_>>());
}

#[test]
fn linear_reservation_overflow_and_empty_inputs_do_not_begin_partial_clustering() {
    assert!(Workspace::new(usize::MAX).is_none());
    for count in [0, 1] {
        let members = (0..count).map(|i| vec![i]).collect::<Vec<_>>();
        assert_eq!(
            cluster(
                members.clone(),
                Workspace::new(count).unwrap(),
                0.55,
                |_, _| panic!("no pair may be scored"),
                |_, _, _| panic!("no merge may be emitted"),
            ),
            members
        );
    }
}

#[test]
fn sparse_threshold_and_signed_zero_boundaries_retain_the_original_decisions() {
    for value in [-1.0, -0.0, 0.0, 0.5 - FLOAT_EPSILON, 0.5, 1.0] {
        let matrix = vec![
            vec![1.0, value, value],
            vec![value, 1.0, value],
            vec![value, value, 1.0],
        ];
        for threshold in [0.0, 0.5, 0.5 + FLOAT_EPSILON, 1.0] {
            let (expected, expected_trace, _) = reference(&matrix, threshold);
            let (members, trace, _) = observed(&matrix, threshold);
            assert_eq!((members, trace), (expected, expected_trace));
        }
    }
}

#[test]
fn sparse_invocations_do_not_share_membership_or_linkage_state() {
    let count = 12;
    let separated = (0..count)
        .map(|a| {
            (0..count)
                .map(|b| if a % 3 == b % 3 { 1.0 } else { 0.1 })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let first = observed(&separated, 0.5);
    assert_eq!(first.0.len(), 3);
    let together = vec![vec![1.0; count]; count];
    assert_eq!(
        observed(&together, 0.5).0,
        vec![(0..count).collect::<Vec<_>>()]
    );
    assert_eq!(observed(&separated, 0.5), first);
}

#[test]
fn sparse_members_preserve_public_scores_admission_and_centroid_commitments() {
    use super::super::super::{
        ClusterCoherenceConfig, EmbeddingPoint, agglomerate, coherent_cluster, normalized_points,
        pairwise_cosine_matrix,
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
        let config = ClusterCoherenceConfig {
            merge_threshold: threshold,
            ..Default::default()
        };
        let members = observed(&matrix, threshold).0;
        let sparse = members
            .iter()
            .map(|group| coherent_cluster(group, &members, &points, &matrix, config))
            .collect::<Vec<_>>();
        let report = agglomerate(&points, config).unwrap();
        assert_eq!(sparse, report.clusters);
    }
}
