//! bd-1nxz4.4: optional explanation latency circuit breaker for context and why.
//!
//! Pure budget-classification policy that lets `ee context --explain` and
//! `ee why` decide which optional explanation components to omit when the
//! per-response budget is tight. The primary surface (the pack body / the
//! core why findings) is never gated by this module; only the optional
//! decorative explanation blocks are.
//!
//! Acceptance shape pinned by the bead body:
//! - Deterministic optional-work budget policy classifying components as
//!   required, useful, or deferrable.
//! - When budget is tight, return the primary answer with structured
//!   degraded entries and a replayable follow-up command for omitted
//!   explanation detail.
//! - No daemon requirement and no silent mutation.
//! - Identical inputs plus budget produce byte-identical omission
//!   decisions.
//!
//! This module is intentionally side-effect free, holds no clock state, and
//! takes the elapsed-millisecond reading as an input so callers can plug in
//! synthetic timings under test. Subsystem wiring into context.rs / why.rs
//! lands in follow-up slices (bd-1nxz4.4.b / .c per the proposed
//! decomposition); this slice owns the classifier policy + repair strings.

use serde::{Serialize, Serializer};

/// Public schema identifier for the budget verdict surface that callers
/// embed in their response envelope.
pub const EXPLANATION_LATENCY_BUDGET_SCHEMA_V1: &str = "ee.explanation_latency_budget.v1";

/// Stable degraded codes the consumer is expected to emit alongside the
/// budget verdict. Listed in a child module so contract tests can pin the
/// closed set without depending on string literals scattered through the
/// implementation.
pub mod degraded_code {
    /// At least one optional component was omitted because the elapsed
    /// budget exceeded the configured target.
    pub const EXPLANATION_LATENCY_BUDGET_EXCEEDED: &str = "explanation_latency_budget_exceeded";
    /// A useful (non-required, non-deferrable) component was omitted; the
    /// caller should surface the follow-up command in degraded.repair.
    pub const EXPLANATION_USEFUL_OMITTED: &str = "explanation_useful_omitted";
    /// A deferrable component was omitted; the follow-up command typically
    /// re-runs with a wider budget or with explicit opt-in flags.
    pub const EXPLANATION_DEFERRABLE_OMITTED: &str = "explanation_deferrable_omitted";
    /// The component was skipped because the budget was exhausted before
    /// the classifier reached it; emitted instead of either of the per-tier
    /// codes when no further work is allowed.
    pub const EXPLANATION_BUDGET_EXHAUSTED: &str = "explanation_budget_exhausted";
}

/// Classifier tier for each explanation component.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ComponentTier {
    /// Required for the primary surface; never omitted by the budget.
    Required,
    /// Useful decoration but acceptably omitted under tight budgets.
    Useful,
    /// Deferrable decoration; omitted first when the budget tightens.
    Deferrable,
}

impl ComponentTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Useful => "useful",
            Self::Deferrable => "deferrable",
        }
    }
}

impl Serialize for ComponentTier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// The closed set of optional explanation components the classifier knows
/// about. Adding a new component requires both an entry here and a tier in
/// [`component_tier`].
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExplanationComponent {
    PrimaryPack,
    PrimaryWhy,
    PackDna,
    GraphRetrieval,
    BipartiteLoadBearing,
    HitsScores,
    ProvenanceEnrichment,
    Contradictions,
    RevisionLineage,
    History,
    RationaleTraces,
    VerificationEvidence,
    CoordinationFallbackEvidence,
    CausalExplanation,
    BayesPosterior,
}

impl ExplanationComponent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryPack => "primary_pack",
            Self::PrimaryWhy => "primary_why",
            Self::PackDna => "pack_dna",
            Self::GraphRetrieval => "graph_retrieval",
            Self::BipartiteLoadBearing => "bipartite_load_bearing",
            Self::HitsScores => "hits_scores",
            Self::ProvenanceEnrichment => "provenance_enrichment",
            Self::Contradictions => "contradictions",
            Self::RevisionLineage => "revision_lineage",
            Self::History => "history",
            Self::RationaleTraces => "rationale_traces",
            Self::VerificationEvidence => "verification_evidence",
            Self::CoordinationFallbackEvidence => "coordination_fallback_evidence",
            Self::CausalExplanation => "causal_explanation",
            Self::BayesPosterior => "bayes_posterior",
        }
    }
}

impl Serialize for ExplanationComponent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Stable classifier mapping. Every component is required, useful, or
/// deferrable. The primary surfaces are Required; the load-bearing
/// explanation blocks an agent typically reads first are Useful; the deeper
/// decoration is Deferrable.
#[must_use]
pub const fn component_tier(component: ExplanationComponent) -> ComponentTier {
    match component {
        ExplanationComponent::PrimaryPack | ExplanationComponent::PrimaryWhy => {
            ComponentTier::Required
        }
        ExplanationComponent::PackDna
        | ExplanationComponent::GraphRetrieval
        | ExplanationComponent::BipartiteLoadBearing
        | ExplanationComponent::HitsScores
        | ExplanationComponent::ProvenanceEnrichment
        | ExplanationComponent::Contradictions
        | ExplanationComponent::RevisionLineage => ComponentTier::Useful,
        ExplanationComponent::History
        | ExplanationComponent::RationaleTraces
        | ExplanationComponent::VerificationEvidence
        | ExplanationComponent::CoordinationFallbackEvidence
        | ExplanationComponent::CausalExplanation
        | ExplanationComponent::BayesPosterior => ComponentTier::Deferrable,
    }
}

/// Caller-facing budget input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplanationLatencyBudget {
    /// Elapsed milliseconds since the primary surface was assembled.
    pub elapsed_ms: u64,
    /// Hard p95 target. Once elapsed >= target_p95_ms the classifier stops
    /// including new optional components and omits any remaining work.
    pub target_p95_ms: u64,
    /// Soft useful-tier cap. Once elapsed >= useful_cap_ms the classifier
    /// omits Useful components even if the hard target has not been hit
    /// yet, leaving room for Deferrable work only when the operator has
    /// explicitly asked for it.
    pub useful_cap_ms: u64,
    /// Soft deferrable-tier cap. Once elapsed >= deferrable_cap_ms the
    /// classifier omits Deferrable components. Lower than useful_cap_ms so
    /// the tightest budget drops deferrable work first.
    pub deferrable_cap_ms: u64,
}

impl ExplanationLatencyBudget {
    /// Default budget anchored to the swarm-scale p95 acceptance the
    /// parent epic targets. Callers should override as needed via
    /// configuration; this default keeps tests deterministic.
    pub const DEFAULT: Self = Self {
        elapsed_ms: 0,
        target_p95_ms: 50,
        useful_cap_ms: 30,
        deferrable_cap_ms: 15,
    };
}

/// Decision the classifier made for one component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetVerdict {
    /// Component should be included in the response.
    Include,
    /// Component should be omitted; emit the matching degraded code plus
    /// the follow-up command stored in [`omitted_repair_command`].
    Omit,
}

impl BudgetVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Omit => "omit",
        }
    }
}

impl Serialize for BudgetVerdict {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// One classifier output entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentDecision {
    pub component: ExplanationComponent,
    pub tier: ComponentTier,
    pub verdict: BudgetVerdict,
    pub reason_code: Option<&'static str>,
    pub follow_up_command: Option<&'static str>,
}

/// Top-level classifier response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplanationBudgetVerdict {
    pub schema: &'static str,
    pub elapsed_ms: u64,
    pub target_p95_ms: u64,
    pub budget_exceeded: bool,
    pub decisions: Vec<ComponentDecision>,
    pub degraded_codes: Vec<&'static str>,
}

impl ExplanationBudgetVerdict {
    /// Convenience: true iff at least one Useful or Deferrable component
    /// was omitted by this verdict.
    #[must_use]
    pub fn any_omissions(&self) -> bool {
        self.decisions
            .iter()
            .any(|decision| decision.verdict == BudgetVerdict::Omit)
    }
}

/// Stable, replayable follow-up command for each omitted component. The
/// caller embeds this in the degraded.repair string so the agent can
/// re-request the missing detail with a single shell line.
#[must_use]
pub const fn omitted_repair_command(component: ExplanationComponent) -> &'static str {
    match component {
        ExplanationComponent::PrimaryPack | ExplanationComponent::PrimaryWhy => {
            // Required components are never omitted; the repair command is
            // a no-op pointer so the table stays exhaustive.
            "ee context --explain"
        }
        ExplanationComponent::PackDna => "ee context --explain --include pack_dna",
        ExplanationComponent::GraphRetrieval => "ee context --explain --include graph_retrieval",
        ExplanationComponent::BipartiteLoadBearing => {
            "ee insights --section loadBearingMemories --json"
        }
        ExplanationComponent::HitsScores => {
            "ee insights --section hubs --json && ee insights --section authorities --json"
        }
        ExplanationComponent::ProvenanceEnrichment => "ee why <memory_id> --include-provenance",
        ExplanationComponent::Contradictions => "ee why <memory_id> --include-contradictions",
        ExplanationComponent::RevisionLineage => "ee why <memory_id> --include-revision-lineage",
        ExplanationComponent::History => "ee memory history <memory_id> --json",
        ExplanationComponent::RationaleTraces => "ee why <memory_id> --include-rationale-traces",
        ExplanationComponent::VerificationEvidence => {
            "ee why <memory_id> --include-verification-evidence"
        }
        ExplanationComponent::CoordinationFallbackEvidence => {
            "ee why <memory_id> --include-coordination-fallback"
        }
        ExplanationComponent::CausalExplanation => "ee why <memory_id> --include-causal",
        ExplanationComponent::BayesPosterior => "ee why <memory_id> --include-bayes-posterior",
    }
}

/// Pure entrypoint: classify each requested component as Include or Omit
/// against the budget. The order of `components` is preserved in the
/// output so callers see decisions in the same order they asked.
#[must_use]
pub fn classify_components(
    budget: ExplanationLatencyBudget,
    components: &[ExplanationComponent],
) -> ExplanationBudgetVerdict {
    let budget_exceeded = budget.elapsed_ms >= budget.target_p95_ms;
    let mut degraded_codes: Vec<&'static str> = Vec::new();
    let mut decisions: Vec<ComponentDecision> = Vec::with_capacity(components.len());

    for component in components.iter().copied() {
        let tier = component_tier(component);
        let (verdict, reason_code, repair) = match tier {
            ComponentTier::Required => (BudgetVerdict::Include, None, None),
            ComponentTier::Useful => {
                if budget.elapsed_ms >= budget.useful_cap_ms {
                    let code = if budget_exceeded {
                        degraded_code::EXPLANATION_BUDGET_EXHAUSTED
                    } else {
                        degraded_code::EXPLANATION_USEFUL_OMITTED
                    };
                    (
                        BudgetVerdict::Omit,
                        Some(code),
                        Some(omitted_repair_command(component)),
                    )
                } else {
                    (BudgetVerdict::Include, None, None)
                }
            }
            ComponentTier::Deferrable => {
                if budget.elapsed_ms >= budget.deferrable_cap_ms {
                    let code = if budget_exceeded {
                        degraded_code::EXPLANATION_BUDGET_EXHAUSTED
                    } else {
                        degraded_code::EXPLANATION_DEFERRABLE_OMITTED
                    };
                    (
                        BudgetVerdict::Omit,
                        Some(code),
                        Some(omitted_repair_command(component)),
                    )
                } else {
                    (BudgetVerdict::Include, None, None)
                }
            }
        };
        if let Some(code) = reason_code {
            if !degraded_codes.contains(&code) {
                degraded_codes.push(code);
            }
        }
        decisions.push(ComponentDecision {
            component,
            tier,
            verdict,
            reason_code,
            follow_up_command: repair,
        });
    }

    if budget_exceeded
        && !degraded_codes.contains(&degraded_code::EXPLANATION_LATENCY_BUDGET_EXCEEDED)
    {
        degraded_codes.insert(0, degraded_code::EXPLANATION_LATENCY_BUDGET_EXCEEDED);
    }

    ExplanationBudgetVerdict {
        schema: EXPLANATION_LATENCY_BUDGET_SCHEMA_V1,
        elapsed_ms: budget.elapsed_ms,
        target_p95_ms: budget.target_p95_ms,
        budget_exceeded,
        decisions,
        degraded_codes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[ExplanationComponent] = &[
        ExplanationComponent::PrimaryPack,
        ExplanationComponent::PrimaryWhy,
        ExplanationComponent::PackDna,
        ExplanationComponent::GraphRetrieval,
        ExplanationComponent::BipartiteLoadBearing,
        ExplanationComponent::HitsScores,
        ExplanationComponent::ProvenanceEnrichment,
        ExplanationComponent::Contradictions,
        ExplanationComponent::RevisionLineage,
        ExplanationComponent::History,
        ExplanationComponent::RationaleTraces,
        ExplanationComponent::VerificationEvidence,
        ExplanationComponent::CoordinationFallbackEvidence,
        ExplanationComponent::CausalExplanation,
        ExplanationComponent::BayesPosterior,
    ];

    #[test]
    fn under_budget_includes_every_component() {
        let budget = ExplanationLatencyBudget {
            elapsed_ms: 0,
            ..ExplanationLatencyBudget::DEFAULT
        };
        let verdict = classify_components(budget, ALL);
        assert!(!verdict.budget_exceeded);
        assert!(!verdict.any_omissions());
        for decision in &verdict.decisions {
            assert_eq!(decision.verdict, BudgetVerdict::Include);
            assert!(decision.reason_code.is_none());
            assert!(decision.follow_up_command.is_none());
        }
        assert!(verdict.degraded_codes.is_empty());
    }

    #[test]
    fn deferrable_cap_drops_deferrable_components_first() {
        let budget = ExplanationLatencyBudget {
            elapsed_ms: 20,
            ..ExplanationLatencyBudget::DEFAULT
        };
        let verdict = classify_components(budget, ALL);
        assert!(!verdict.budget_exceeded);
        for decision in &verdict.decisions {
            match decision.tier {
                ComponentTier::Required => {
                    assert_eq!(decision.verdict, BudgetVerdict::Include)
                }
                ComponentTier::Useful => {
                    assert_eq!(decision.verdict, BudgetVerdict::Include)
                }
                ComponentTier::Deferrable => {
                    assert_eq!(decision.verdict, BudgetVerdict::Omit);
                    assert_eq!(
                        decision.reason_code,
                        Some(degraded_code::EXPLANATION_DEFERRABLE_OMITTED)
                    );
                    assert!(decision.follow_up_command.is_some());
                }
            }
        }
        assert!(
            verdict
                .degraded_codes
                .contains(&degraded_code::EXPLANATION_DEFERRABLE_OMITTED)
        );
        assert!(
            !verdict
                .degraded_codes
                .contains(&degraded_code::EXPLANATION_LATENCY_BUDGET_EXCEEDED)
        );
    }

    #[test]
    fn useful_cap_drops_useful_components_when_exceeded() {
        let budget = ExplanationLatencyBudget {
            elapsed_ms: 35,
            ..ExplanationLatencyBudget::DEFAULT
        };
        let verdict = classify_components(budget, ALL);
        for decision in &verdict.decisions {
            match decision.tier {
                ComponentTier::Required => {
                    assert_eq!(decision.verdict, BudgetVerdict::Include)
                }
                ComponentTier::Useful | ComponentTier::Deferrable => {
                    assert_eq!(decision.verdict, BudgetVerdict::Omit);
                }
            }
        }
        assert!(
            verdict
                .degraded_codes
                .contains(&degraded_code::EXPLANATION_USEFUL_OMITTED)
        );
    }

    #[test]
    fn target_p95_marks_budget_exceeded_and_swaps_reason_to_exhausted() {
        let budget = ExplanationLatencyBudget {
            elapsed_ms: 60,
            ..ExplanationLatencyBudget::DEFAULT
        };
        let verdict = classify_components(budget, ALL);
        assert!(verdict.budget_exceeded);
        assert_eq!(
            verdict.degraded_codes.first().copied(),
            Some(degraded_code::EXPLANATION_LATENCY_BUDGET_EXCEEDED)
        );
        let exhausted_decisions: Vec<_> = verdict
            .decisions
            .iter()
            .filter(|decision| decision.verdict == BudgetVerdict::Omit)
            .collect();
        assert!(!exhausted_decisions.is_empty());
        for decision in exhausted_decisions {
            assert_eq!(
                decision.reason_code,
                Some(degraded_code::EXPLANATION_BUDGET_EXHAUSTED)
            );
        }
        for decision in &verdict.decisions {
            if decision.tier == ComponentTier::Required {
                assert_eq!(decision.verdict, BudgetVerdict::Include);
                assert!(decision.reason_code.is_none());
            }
        }
    }

    #[test]
    fn classifier_preserves_input_order() {
        let order = [
            ExplanationComponent::RationaleTraces,
            ExplanationComponent::PrimaryPack,
            ExplanationComponent::PackDna,
            ExplanationComponent::History,
        ];
        let verdict = classify_components(ExplanationLatencyBudget::DEFAULT, &order);
        let observed: Vec<ExplanationComponent> = verdict
            .decisions
            .iter()
            .map(|decision| decision.component)
            .collect();
        assert_eq!(observed.as_slice(), order.as_slice());
    }

    #[test]
    fn identical_inputs_produce_byte_identical_decisions() {
        let budget = ExplanationLatencyBudget {
            elapsed_ms: 40,
            ..ExplanationLatencyBudget::DEFAULT
        };
        let a = classify_components(budget, ALL);
        let b = classify_components(budget, ALL);
        assert_eq!(a, b);
        let a_json = serde_json::to_string(&a).expect("serialize a");
        let b_json = serde_json::to_string(&b).expect("serialize b");
        assert_eq!(a_json, b_json);
    }

    #[test]
    fn every_component_has_a_follow_up_command() {
        for component in ALL {
            let cmd = omitted_repair_command(*component);
            assert!(!cmd.is_empty());
            // Replayable commands all start with ee (we never recommend a
            // forbidden / destructive shell).
            assert!(cmd.starts_with("ee "), "unsafe command: {cmd}");
        }
    }

    #[test]
    fn required_tier_is_never_omitted_regardless_of_budget() {
        for elapsed in [0_u64, 50, 1_000, 10_000] {
            let budget = ExplanationLatencyBudget {
                elapsed_ms: elapsed,
                ..ExplanationLatencyBudget::DEFAULT
            };
            let verdict = classify_components(
                budget,
                &[
                    ExplanationComponent::PrimaryPack,
                    ExplanationComponent::PrimaryWhy,
                ],
            );
            for decision in &verdict.decisions {
                assert_eq!(decision.verdict, BudgetVerdict::Include);
                assert_eq!(decision.tier, ComponentTier::Required);
            }
        }
    }
}
