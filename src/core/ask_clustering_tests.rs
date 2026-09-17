use super::*;
use crate::core::ask::jaccard_similarity;

// Executable specification of the previous algorithm. This is an independent
// all-pairs oracle, not a mock of the new postings path.
fn all_pairs_reference(spans: &[AskSpan]) -> Vec<AskSpan> {
    let term_sets: Vec<_> = spans.iter().map(|span| tokenize_for_ask(&span.text)).collect();
    let mut order: Vec<_> = (0..spans.len()).collect();
    order.sort_by(|&left, &right| {
        spans[right].score.total_cmp(&spans[left].score)
            .then_with(|| spans[left].memory_id.cmp(&spans[right].memory_id))
            .then_with(|| spans[left].byte_start.cmp(&spans[right].byte_start))
    });
    let mut assigned = vec![false; spans.len()];
    let mut representatives = Vec::new();
    for &seed in &order {
        if assigned[seed] { continue; }
        assigned[seed] = true;
        let mut supporting = BTreeSet::from([spans[seed].memory_id.as_str()]);
        for &other in &order {
            if assigned[other] || has_negation(&spans[seed].text) != has_negation(&spans[other].text) {
                continue;
            }
            if jaccard_similarity(&term_sets[seed], &term_sets[other]) >= CLUSTER_SIMILARITY_THRESHOLD {
                assigned[other] = true;
                supporting.insert(spans[other].memory_id.as_str());
            }
        }
        let multiplier = (1.0 + 0.1 * (supporting.len() as f32).ln()).min(CORROBORATION_CAP);
        let mut representative = spans[seed].clone();
        representative.score = (representative.score * multiplier).clamp(0.0, 1.0);
        representatives.push(representative);
    }
    representatives.sort_by(|left, right| {
        right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    representatives
}

fn span(index: usize, memory: usize, text: &str, score: f32) -> AskSpan {
    AskSpan {
        memory_id: format!("memory-{memory:05}"),
        byte_start: index * 100,
        byte_end: index * 100 + text.len(),
        text: text.to_owned(),
        score,
        trust_class: "human_explicit".to_owned(),
        memory_confidence: 0.9,
        provenance_uri: Some(format!("manual://sparse-clustering/{memory}")),
        team_provenance: None,
    }
}

fn signature(spans: &[AskSpan]) -> serde_json::Value {
    serde_json::json!(spans.iter().map(|span| serde_json::json!({
        "memory": span.memory_id,
        "start": span.byte_start,
        "end": span.byte_end,
        "text": span.text,
        "scoreBits": span.score.to_bits(),
        "confidenceBits": span.memory_confidence.to_bits(),
        "trust": span.trust_class,
        "provenance": span.provenance_uri,
        "team": span.team_provenance.as_ref().map(|value| value.to_json()),
    })).collect::<Vec<_>>())
}

#[test]
fn agrees_bit_for_bit_with_all_pairs_on_seeded_corpora() {
    let vocabulary = ["cargo", "fmt", "release", "cache", "delta", "port", "database", "tls", "migration", "audit", "café", "the", "not", "alpha", "beta"];
    let mut state = 0x7289_d115_0831_abc7_u64;
    for round in 0..64 {
        let mut input = Vec::new();
        for index in 0..48 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let length = (state as usize % 9) + 1;
            let mut words = Vec::new();
            for _ in 0..length {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                words.push(vocabulary[state as usize % vocabulary.len()]);
            }
            let score = ((state >> 32) % 101) as f32 / 100.0;
            input.push(span(index, index / 2, &words.join(" "), score));
        }
        assert_eq!(signature(&cluster_spans(&input)), signature(&all_pairs_reference(&input)), "corpus {round}");
        input.reverse();
        assert_eq!(signature(&cluster_spans(&input)), signature(&all_pairs_reference(&input)), "reversed corpus {round}");
    }
}

#[test]
fn opposition_and_same_memory_repetition_do_not_inflate_corroboration() {
    let input = vec![
        span(0, 0, "Run cargo fmt before release.", 0.7),
        span(1, 0, "Run cargo fmt before release.", 0.7),
        span(2, 1, "Run cargo fmt before release.", 0.7),
        span(3, 2, "Do not run cargo fmt before release.", 0.7),
    ];
    let actual = cluster_spans(&input);
    assert_eq!(signature(&actual), signature(&all_pairs_reference(&input)));
    assert_eq!(actual.len(), 2);
    assert!(actual.iter().any(|span| has_negation(&span.text)));
    assert_eq!(actual[0].score.to_bits(), (0.7_f32 * (1.0 + 0.1 * 2.0_f32.ln())).to_bits());
}

#[test]
fn shared_vocabulary_does_not_turn_seed_clustering_into_transitive_closure() {
    let input = vec![
        span(0, 0, "alpha beta gamma delta epsilon", 0.9),
        span(1, 1, "alpha beta gamma delta epsilon zeta", 0.8),
        span(2, 2, "alpha beta gamma delta epsilon zeta theta", 0.7),
    ];
    let actual = cluster_spans(&input);
    assert_eq!(signature(&actual), signature(&all_pairs_reference(&input)));
    assert_eq!(actual.len(), 2); // A matches B; B matches C; A does not match C.
}

#[test]
fn ten_thousand_disjoint_spans_need_zero_pairwise_similarity_checks() {
    let input: Vec<_> = (0..10_000).map(|index| span(index, index, &format!("unique_term_{index}"), 0.5)).collect();
    let mut comparisons = 0;
    let actual = cluster_with_observer(&input, || comparisons += 1);
    assert_eq!(comparisons, 0);
    assert_eq!(actual.len(), input.len(), "no evidence was truncated to get the work reduction");
    assert_eq!(actual[0].memory_id, "memory-00000");
    assert_eq!(actual[9_999].memory_id, "memory-09999");
    assert!(actual.iter().all(|span| span.score.to_bits() == 0.5_f32.to_bits()));
}

#[test]
fn repeated_words_count_once_and_empty_term_sets_stay_independent() {
    let input = vec![
        span(0, 0, "cargo cargo fmt release", 0.6),
        span(1, 1, "cargo fmt fmt release release", 0.6),
        span(2, 2, "the and of", 0.6),
        span(3, 3, "a an the", 0.6),
        span(4, 4, "🦀", 0.6),
    ];
    assert_eq!(signature(&cluster_spans(&input)), signature(&all_pairs_reference(&input)));
    assert_eq!(cluster_spans(&input).len(), 4);
    assert!(cluster_spans(&[]).is_empty());
}

#[test]
fn input_permutations_preserve_citations_and_exact_score_bits() {
    let mut input: Vec<_> = (0..40).map(|index| span(index, index, if index % 3 == 0 { "Do not run cargo fmt before release." } else { "Run cargo fmt before release." }, 0.6)).collect();
    let expected = signature(&cluster_spans(&input));
    input.reverse();
    assert_eq!(signature(&cluster_spans(&input)), expected);
    input.rotate_left(17);
    assert_eq!(signature(&cluster_spans(&input)), expected);
}

#[test]
fn opposite_polarities_are_never_pairwise_compared() {
    let input: Vec<_> = (0..100).map(|index| span(index, index, if index % 2 == 0 { "Run cargo fmt before release." } else { "Do not run cargo fmt before release." }, 0.6)).collect();
    let mut comparisons = 0;
    let actual = cluster_with_observer(&input, || comparisons += 1);
    assert_eq!(comparisons, 98);
    assert_eq!(signature(&actual), signature(&all_pairs_reference(&input)));
}
