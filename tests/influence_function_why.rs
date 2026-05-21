use ee::core::influence::{
    WhyInfluenceCandidate, WhyInfluenceDirection, exact_leave_one_out_delta,
    relative_total_influence_error, sum_absolute_influence, why_counterfactual_influence,
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn candidate(
    memory_id: &str,
    relation: &str,
    score: f32,
    direction: WhyInfluenceDirection,
) -> WhyInfluenceCandidate {
    WhyInfluenceCandidate {
        memory_id: memory_id.to_owned(),
        relation: relation.to_owned(),
        score,
        direction,
    }
}

#[test]
fn why_influence_surfaces_top_three_positive_and_negative() -> TestResult {
    let report = why_counterfactual_influence(
        "mem_target",
        0.70,
        [
            candidate(
                "mem_pos_1",
                "supports",
                0.30,
                WhyInfluenceDirection::Positive,
            ),
            candidate(
                "mem_pos_2",
                "derived_from",
                0.20,
                WhyInfluenceDirection::Positive,
            ),
            candidate(
                "mem_pos_3",
                "supports",
                0.10,
                WhyInfluenceDirection::Positive,
            ),
            candidate(
                "mem_pos_4",
                "supports",
                0.05,
                WhyInfluenceDirection::Positive,
            ),
            candidate(
                "mem_neg_1",
                "contradicts",
                0.25,
                WhyInfluenceDirection::Negative,
            ),
            candidate(
                "mem_neg_2",
                "invalidates",
                0.15,
                WhyInfluenceDirection::Negative,
            ),
            candidate(
                "mem_neg_3",
                "refutes",
                0.10,
                WhyInfluenceDirection::Negative,
            ),
            candidate(
                "mem_neg_4",
                "conflicts_with",
                0.02,
                WhyInfluenceDirection::Negative,
            ),
        ],
    );

    let positive_ids = report
        .top_positive
        .iter()
        .map(|entry| entry.memory_id.as_str())
        .collect::<Vec<_>>();
    let negative_ids = report
        .top_negative
        .iter()
        .map(|entry| entry.memory_id.as_str())
        .collect::<Vec<_>>();

    ensure(
        positive_ids == vec!["mem_pos_1", "mem_pos_2", "mem_pos_3"],
        format!("unexpected positive influencers: {positive_ids:?}"),
    )?;
    ensure(
        negative_ids == vec!["mem_neg_1", "mem_neg_2", "mem_neg_3"],
        format!("unexpected negative influencers: {negative_ids:?}"),
    )
}

#[test]
fn why_influence_ordering_is_deterministic_for_ties() -> TestResult {
    let first = why_counterfactual_influence(
        "mem_target",
        0.50,
        [
            candidate("mem_b", "supports", 0.20, WhyInfluenceDirection::Positive),
            candidate("mem_a", "supports", 0.20, WhyInfluenceDirection::Positive),
            candidate("mem_c", "supports", 0.10, WhyInfluenceDirection::Positive),
        ],
    );
    let second = why_counterfactual_influence(
        "mem_target",
        0.50,
        [
            candidate("mem_c", "supports", 0.10, WhyInfluenceDirection::Positive),
            candidate("mem_a", "supports", 0.20, WhyInfluenceDirection::Positive),
            candidate("mem_b", "supports", 0.20, WhyInfluenceDirection::Positive),
        ],
    );
    let first_ids = first
        .entries
        .iter()
        .map(|entry| entry.memory_id.as_str())
        .collect::<Vec<_>>();
    let second_ids = second
        .entries
        .iter()
        .map(|entry| entry.memory_id.as_str())
        .collect::<Vec<_>>();

    ensure(first_ids == second_ids, "entry ordering must be stable")?;
    ensure(
        first_ids == vec!["mem_a", "mem_b", "mem_c"],
        format!("unexpected tie order: {first_ids:?}"),
    )
}

#[test]
fn why_influence_total_matches_exact_leave_one_out_delta_within_five_percent() -> TestResult {
    let report = why_counterfactual_influence(
        "mem_target",
        0.80,
        [
            candidate(
                "mem_support",
                "supports",
                0.30,
                WhyInfluenceDirection::Positive,
            ),
            candidate(
                "mem_contradiction",
                "contradicts",
                0.10,
                WhyInfluenceDirection::Negative,
            ),
            candidate(
                "mem_duplicate",
                "supports",
                0.05,
                WhyInfluenceDirection::Positive,
            ),
        ],
    );
    let exact_total = report
        .entries
        .iter()
        .map(|entry| exact_leave_one_out_delta(entry).abs())
        .sum::<f32>();
    let absolute_total = sum_absolute_influence(&report.entries);
    let relative_error = if exact_total == 0.0 {
        absolute_total
    } else {
        ((absolute_total - exact_total) / exact_total).abs()
    };

    ensure(
        relative_error <= 0.05,
        format!("relative influence error {relative_error} exceeded 5%"),
    )?;
    ensure(
        relative_total_influence_error(&report) <= 0.05,
        "reported total should match exact leave-one-out deltas",
    )
}

#[test]
fn why_influence_aggregates_duplicate_pack_mates() -> TestResult {
    let report = why_counterfactual_influence(
        "mem_target",
        0.60,
        [
            candidate(
                "mem_peer",
                "supports",
                0.10,
                WhyInfluenceDirection::Positive,
            ),
            candidate(
                "mem_peer",
                "supports",
                0.15,
                WhyInfluenceDirection::Positive,
            ),
        ],
    );
    let peer = report
        .entries
        .iter()
        .find(|entry| entry.memory_id == "mem_peer")
        .ok_or_else(|| "aggregated peer missing".to_owned())?;

    ensure(report.entries.len() == 1, "duplicates should aggregate")?;
    ensure(
        (peer.influence_delta - 0.25).abs() < 0.0001,
        format!("unexpected aggregate influence: {}", peer.influence_delta),
    )
}

#[test]
fn why_influence_empty_candidates_emit_empty_lists() -> TestResult {
    let report = why_counterfactual_influence(
        "mem_target",
        0.42,
        std::iter::empty::<WhyInfluenceCandidate>(),
    );

    ensure(report.entries.is_empty(), "entries should be empty")?;
    ensure(
        report.top_positive.is_empty(),
        "top_positive should be empty",
    )?;
    ensure(
        report.top_negative.is_empty(),
        "top_negative should be empty",
    )
}
