//! bd-2tehh.4 — property tests for the ADR 0070 retrieval-weight evaluator.
//!
//! The ADR's verification section asks for "evaluator monotonicity — adding
//! a helpful label for a memory never lowers the score of vectors ranking
//! it higher". The banked derivation on the bead shows that statement is
//! FALSIFIABLE in two of three readings because of per-query normalization:
//!
//! - Absolute monotonicity fails: a helpful label at a deep rank grows the
//!   normalizing denominator faster than the numerator when the query's
//!   score is already high.
//! - Cross-query vector ordering can flip: the added label dilutes only its
//!   own query's normalized contribution, so a lead built on that query can
//!   shrink below a deficit elsewhere (documented by the regression test
//!   below so nobody "fixes" the evaluator to satisfy the false reading).
//! - What HOLDS, and what the property test pins: within a single query,
//!   for two vectors that both rank the target memory, adding a helpful
//!   label for it never flips their pairwise order against the vector that
//!   ranks it higher — numerators gain `w/log2(1+rank)` (more for the
//!   better ranker) over an identical denominator.
#![allow(clippy::unwrap_used)]

use ee::core::search::{ScoreSource, SearchHit};
use ee::core::shadow_tuning::{
    LabelSource, LabeledTriple, QueryReplay, TuningWeights, evaluate_fusion_candidates,
    score_fusion_candidate,
};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

fn hybrid_hit(doc_id: &str, raw_score: f64, lexical: bool) -> SearchHit {
    #[allow(clippy::cast_possible_truncation)]
    let raw = raw_score as f32;
    SearchHit {
        doc_id: doc_id.to_owned(),
        score: raw,
        source: ScoreSource::Hybrid,
        fast_score: None,
        quality_score: (!lexical).then_some(1.0),
        lexical_score: lexical.then_some(1.0),
        rerank_score: None,
        metadata: None,
        explanation: None,
    }
}

fn helpful(query: &str, memory_id: &str, weight: f64) -> LabeledTriple {
    LabeledTriple {
        query: query.to_owned(),
        memory_id: memory_id.to_owned(),
        signal: "helpful".to_owned(),
        base_weight: weight,
        weight,
        age_days: 0.0,
        source: LabelSource::PackItemOutcome,
        feedback_event_id: format!("fev-{query}-{memory_id}"),
        pack_record_id: Some("pack-prop".to_owned()),
        audit_row_id: None,
    }
}

fn replay(query: &str, hits: Vec<SearchHit>) -> QueryReplay {
    QueryReplay {
        query: query.to_owned(),
        hits,
    }
}

/// Rank probe through the public API: with a single unit helpful label the
/// normalized score is exactly `1/log2(1+rank)` (0 when unranked).
fn rank_probe(replays: &[QueryReplay], query: &str, memory_id: &str, v: TuningWeights) -> f64 {
    score_fusion_candidate(replays, &[helpful(query, memory_id, 1.0)], v)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Single-query pairwise invariant (the reading of the ADR property
    /// that actually holds).
    #[test]
    fn adding_helpful_label_never_flips_single_query_pairwise_order(
        raw_scores in prop::collection::vec(0.006f64..0.05, 3..7),
        lexical_flags in prop::collection::vec(any::<bool>(), 3..7),
        target_index in 0usize..3,
        added_weight in 0.05f64..1.0,
        label_weight in 0.05f64..1.0,
        v1_lex in 0.2f64..0.7, v1_sem in 0.2f64..0.7, v1_graph in 0.0f64..0.3,
        v2_lex in 0.2f64..0.7, v2_sem in 0.2f64..0.7, v2_graph in 0.0f64..0.3,
    ) {
        #[allow(clippy::cast_possible_truncation)]
        let (v1, v2) = (
            TuningWeights { lexical: v1_lex as f32, semantic: v1_sem as f32, graph: v1_graph as f32 },
            TuningWeights { lexical: v2_lex as f32, semantic: v2_sem as f32, graph: v2_graph as f32 },
        );
        let count = raw_scores.len().min(lexical_flags.len());
        let hits: Vec<SearchHit> = (0..count)
            .map(|index| hybrid_hit(&format!("mem-{index}"), raw_scores[index], lexical_flags[index]))
            .collect();
        let replays = [replay("q", hits)];
        let target = format!("mem-{}", target_index.min(count - 1));

        // Precondition: both vectors rank the target.
        let probe1 = rank_probe(&replays, "q", &target, v1);
        let probe2 = rank_probe(&replays, "q", &target, v2);
        prop_assume!(probe1 > 0.0 && probe2 > 0.0);
        // Orient the pair so v1 ranks the target at least as well
        // (larger probe = better rank).
        let (better, worse) = if probe1 >= probe2 { (v1, v2) } else { (v2, v1) };

        // Arbitrary base label set on the same query (kept mapped/simple:
        // one helpful label on the first pool memory).
        let base = vec![helpful("q", "mem-0", label_weight)];
        let pre_better = score_fusion_candidate(&replays, &base, better);
        let pre_worse = score_fusion_candidate(&replays, &base, worse);
        prop_assume!(pre_better >= pre_worse);

        let mut extended = base;
        extended.push(helpful("q", &target, added_weight));
        let post_better = score_fusion_candidate(&replays, &extended, better);
        let post_worse = score_fusion_candidate(&replays, &extended, worse);
        prop_assert!(
            post_better >= post_worse - 1e-9,
            "single-query pairwise order flipped against the better ranker: \
             pre ({pre_better}, {pre_worse}) post ({post_better}, {post_worse})"
        );
    }

    /// Determinism: identical inputs produce byte-identical evaluations.
    #[test]
    fn evaluation_is_deterministic(
        raw_scores in prop::collection::vec(0.006f64..0.05, 2..6),
        lexical_flags in prop::collection::vec(any::<bool>(), 2..6),
        label_weight in 0.05f64..1.0,
    ) {
        let count = raw_scores.len().min(lexical_flags.len());
        let hits: Vec<SearchHit> = (0..count)
            .map(|index| hybrid_hit(&format!("mem-{index}"), raw_scores[index], lexical_flags[index]))
            .collect();
        let replays = [replay("q", hits)];
        let labels = [helpful("q", "mem-0", label_weight)];
        let incumbent = TuningWeights { lexical: 0.45, semantic: 0.45, graph: 0.10 };
        let cx = asupersync::Cx::for_testing();
        let first = evaluate_fusion_candidates(&cx, &replays, &labels, incumbent).unwrap();
        let second = evaluate_fusion_candidates(&cx, &replays, &labels, incumbent).unwrap();
        prop_assert_eq!(first, second);
    }
}

/// Regression documenting the FALSE reading: across queries, adding a
/// helpful label can flip the total ordering of two vectors even though the
/// better ranker gains more inside the labeled query — per-query
/// normalization dilutes the lead that query was providing. A deterministic
/// grid search finds a concrete instance; if the evaluator is ever "fixed"
/// to make this impossible, this test fails and the normalization contract
/// (chatty workspaces cannot dominate) has silently changed.
#[test]
fn cross_query_dilution_can_flip_vector_totals() {
    let lex_heavy = TuningWeights {
        lexical: 0.7,
        semantic: 0.2,
        graph: 0.0,
    };
    let sem_heavy = TuningWeights {
        lexical: 0.2,
        semantic: 0.7,
        graph: 0.0,
    };
    let mut found = false;
    'outer: for &a in &[0.010f64, 0.020, 0.030] {
        for &b in &[0.012f64, 0.022, 0.032] {
            for &c in &[0.014f64, 0.024, 0.034] {
                let replays = [
                    replay(
                        "q1",
                        vec![
                            hybrid_hit("mem-a", a, true),
                            hybrid_hit("mem-b", b, false),
                            hybrid_hit("mem-e", c, false),
                        ],
                    ),
                    replay(
                        "q2",
                        vec![hybrid_hit("mem-c", a, false), hybrid_hit("mem-d", b, true)],
                    ),
                ];
                let base = vec![helpful("q1", "mem-a", 1.0), helpful("q2", "mem-c", 1.0)];
                let pre_lex = score_fusion_candidate(&replays, &base, lex_heavy);
                let pre_sem = score_fusion_candidate(&replays, &base, sem_heavy);
                let mut extended = base.clone();
                extended.push(helpful("q1", "mem-b", 1.0));
                extended.push(helpful("q1", "mem-e", 1.0));
                let post_lex = score_fusion_candidate(&replays, &extended, lex_heavy);
                let post_sem = score_fusion_candidate(&replays, &extended, sem_heavy);
                let flipped = (pre_lex > pre_sem + 1e-9 && post_lex + 1e-9 < post_sem)
                    || (pre_sem > pre_lex + 1e-9 && post_sem + 1e-9 < post_lex);
                if flipped {
                    found = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(
        found,
        "no cross-query dilution flip found in the deterministic grid — the \
         per-query normalization contract may have silently changed"
    );
}
