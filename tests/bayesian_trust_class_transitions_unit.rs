//! N7.1 / ADR 0032 — trust-class transition table unit tests.

use ee::{
    core::bayes::{BetaPosterior, TrustClassTransitionDirection, trust_class_transition},
    models::TrustClass,
};

fn posterior(alpha: f64, beta: f64) -> Result<BetaPosterior, String> {
    BetaPosterior::new(alpha, beta)
        .ok_or_else(|| format!("valid posterior fixture for alpha={alpha}, beta={beta}"))
}

fn assert_transition(
    current: TrustClass,
    posterior: BetaPosterior,
    validation_events: u64,
    explicit_human_promotion: bool,
    expected_next: TrustClass,
    expected_direction: TrustClassTransitionDirection,
    expected_reason: &str,
) {
    let transition = trust_class_transition(
        current,
        &posterior,
        validation_events,
        explicit_human_promotion,
    );

    assert_eq!(transition.previous_class, current);
    assert_eq!(transition.next_class, expected_next);
    assert_eq!(transition.direction, expected_direction);
    assert_eq!(transition.reason, expected_reason);
    assert_eq!(
        transition.audit_required,
        expected_direction != TrustClassTransitionDirection::Stable
    );
    assert_eq!(transition.validation_events, validation_events);
    assert_eq!(
        transition.explicit_human_promotion,
        explicit_human_promotion
    );
    assert!(transition.ci90_lower.is_some());
    assert!(transition.ci90_upper.is_some());
}

#[test]
fn trust_class_transition_table_promotes_on_ci_lower_crossings() -> Result<(), String> {
    let strong_positive = posterior(30.0, 1.0)?;

    assert_transition(
        TrustClass::LegacyImport,
        strong_positive,
        1,
        false,
        TrustClass::CassEvidence,
        TrustClassTransitionDirection::Promote,
        "legacy_import_promote_ci90_lower_gt_0_50_with_validation",
    );
    assert_transition(
        TrustClass::CassEvidence,
        strong_positive,
        0,
        false,
        TrustClass::AgentAssertion,
        TrustClassTransitionDirection::Promote,
        "cass_evidence_promote_ci90_lower_gt_0_60",
    );
    assert_transition(
        TrustClass::AgentAssertion,
        strong_positive,
        0,
        false,
        TrustClass::AgentValidated,
        TrustClassTransitionDirection::Promote,
        "agent_assertion_promote_ci90_lower_gt_0_70_sample_size_ge_6",
    );

    Ok(())
}

#[test]
fn trust_class_transition_table_demotes_on_ci_upper_crossings() -> Result<(), String> {
    let strong_negative = posterior(0.5, 100.0)?;

    assert_transition(
        TrustClass::CassEvidence,
        strong_negative,
        0,
        false,
        TrustClass::LegacyImport,
        TrustClassTransitionDirection::Demote,
        "cass_evidence_demote_ci90_upper_lt_0_30",
    );
    assert_transition(
        TrustClass::AgentAssertion,
        strong_negative,
        0,
        false,
        TrustClass::CassEvidence,
        TrustClassTransitionDirection::Demote,
        "agent_assertion_demote_ci90_upper_lt_0_35",
    );
    assert_transition(
        TrustClass::AgentValidated,
        strong_negative,
        0,
        false,
        TrustClass::AgentAssertion,
        TrustClassTransitionDirection::Demote,
        "agent_validated_demote_ci90_upper_lt_0_40",
    );
    assert_transition(
        TrustClass::HumanExplicit,
        strong_negative,
        0,
        false,
        TrustClass::AgentValidated,
        TrustClassTransitionDirection::Demote,
        "human_explicit_demote_ci90_upper_lt_0_45",
    );
    assert_transition(
        TrustClass::PeerHumanAttested,
        strong_negative,
        0,
        false,
        TrustClass::AgentValidated,
        TrustClassTransitionDirection::Demote,
        "peer_human_attested_demote_ci90_upper_lt_0_45",
    );

    Ok(())
}

#[test]
fn legacy_import_promotion_requires_validation_event() -> Result<(), String> {
    let strong_positive = posterior(30.0, 1.0)?;

    assert_transition(
        TrustClass::LegacyImport,
        strong_positive,
        0,
        false,
        TrustClass::LegacyImport,
        TrustClassTransitionDirection::Stable,
        "legacy_import_validation_required",
    );

    Ok(())
}

#[test]
fn agent_assertion_promotion_requires_sample_size_gate() -> Result<(), String> {
    let thin_positive = posterior(5.8, 0.05)?;

    let transition = trust_class_transition(TrustClass::AgentAssertion, &thin_positive, 0, false);

    assert_eq!(transition.previous_class, TrustClass::AgentAssertion);
    assert_eq!(transition.next_class, TrustClass::AgentAssertion);
    assert_eq!(transition.direction, TrustClassTransitionDirection::Stable);
    assert_eq!(transition.reason, "agent_assertion_sample_size_gate_unmet");
    assert!(transition.effective_sample_size < 6.0);
    assert!(!transition.audit_required);

    Ok(())
}

#[test]
fn human_explicit_promotion_requires_operator_intent() -> Result<(), String> {
    let strong_positive = posterior(30.0, 1.0)?;

    assert_transition(
        TrustClass::AgentValidated,
        strong_positive,
        0,
        false,
        TrustClass::AgentValidated,
        TrustClassTransitionDirection::Stable,
        "human_explicit_promotion_requires_operator",
    );
    assert_transition(
        TrustClass::AgentValidated,
        strong_positive,
        0,
        true,
        TrustClass::HumanExplicit,
        TrustClassTransitionDirection::Promote,
        "agent_validated_promote_explicit_human_operator",
    );
    assert_transition(
        TrustClass::PeerHumanAttested,
        strong_positive,
        0,
        false,
        TrustClass::PeerHumanAttested,
        TrustClassTransitionDirection::Stable,
        "local_human_promotion_requires_operator",
    );
    assert_transition(
        TrustClass::PeerHumanAttested,
        strong_positive,
        0,
        true,
        TrustClass::HumanExplicit,
        TrustClassTransitionDirection::Promote,
        "peer_human_attested_promote_explicit_local_operator",
    );

    Ok(())
}

#[test]
fn neutral_posterior_does_not_emit_transition_audit() -> Result<(), String> {
    let neutral = posterior(2.0, 2.0)?;
    let transition = trust_class_transition(TrustClass::AgentAssertion, &neutral, 0, false);

    assert_eq!(transition.next_class, TrustClass::AgentAssertion);
    assert_eq!(transition.direction, TrustClassTransitionDirection::Stable);
    assert_eq!(transition.reason, "no_transition_threshold_crossed");
    assert!(!transition.audit_required);

    Ok(())
}
