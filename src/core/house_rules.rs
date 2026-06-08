//! House Rules — cross-workspace global memory tier decision cores
//! (bd-1n0np.10.2 / 10.3).
//!
//! 10.1 landed the [`crate::core::memory_scope::MemoryScope::Global`] lane and
//! the candidate-load union. This module is the pure, deterministic decision
//! logic on top of that substrate, kept free of CLI / DB / golden surfaces so it
//! is verifiable without RCH:
//!
//! - the **audited promotion gate** (10.2): a memory promotes to the global tier
//!   only on explicit human marking or evidence from N distinct workspaces
//!   (ADR-0006, "procedural memory requires evidence");
//! - the **capped house-rules quota** (10.3): global rules get a bounded share of
//!   the pack budget so they never crowd out project context, with a
//!   per-workspace opt-out.
//!
//! The CLI surfaces (`ee rule promote-global`, `ee remember --scope global`,
//! `ee insights --section houseRules`) and the audited curate transition wire
//! these decisions in; that wiring is the golden-gated follow-on.

use std::collections::BTreeSet;

use serde::Serialize;

/// Default number of distinct workspaces whose evidence justifies promoting a
/// memory to the global tier without explicit human marking (ADR-0006).
pub const DEFAULT_GLOBAL_PROMOTION_WORKSPACE_THRESHOLD: usize = 3;

/// Default share (basis points) of the pack budget reserved as the *cap* for the
/// house-rules section, so global rules never crowd out project context.
pub const DEFAULT_HOUSE_RULES_QUOTA_BASIS_POINTS: u32 = 2_000;

/// Why a memory is eligible for global-tier promotion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "basis")]
pub enum GlobalPromotionBasis {
    /// A human explicitly marked the memory for the global tier — always allowed.
    ExplicitHumanMarking,
    /// Evidence from `distinct_workspaces` (>= threshold) distinct workspaces.
    CrossWorkspaceEvidence { distinct_workspaces: usize },
}

/// Audited promotion-gate decision for a candidate global-tier memory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum GlobalPromotionDecision {
    /// Promote to the global tier; `basis` records the justification for audit.
    Promote { basis: GlobalPromotionBasis },
    /// Deny promotion; the memory stays workspace-scoped.
    Deny {
        reason: &'static str,
        distinct_workspaces: usize,
        threshold: usize,
    },
}

impl GlobalPromotionDecision {
    /// Whether this decision promotes the memory to the global tier.
    #[must_use]
    pub const fn is_promote(&self) -> bool {
        matches!(self, Self::Promote { .. })
    }
}

/// Inputs to the promotion gate. `evidence_workspace_ids` is the set of distinct
/// workspaces that carry supporting evidence for the memory.
#[derive(Clone, Debug)]
pub struct GlobalPromotionGateInput<'a> {
    pub evidence_workspace_ids: &'a BTreeSet<String>,
    pub explicit_human_marking: bool,
    pub threshold: usize,
}

/// Evaluate the audited global-promotion gate (bd-1n0np.10.2).
///
/// Explicit human marking always promotes (human authority). Otherwise the
/// memory must carry evidence from at least `threshold` distinct workspaces
/// (and the threshold must be non-zero). Pure and deterministic.
#[must_use]
pub fn evaluate_global_promotion_gate(
    input: &GlobalPromotionGateInput<'_>,
) -> GlobalPromotionDecision {
    if input.explicit_human_marking {
        return GlobalPromotionDecision::Promote {
            basis: GlobalPromotionBasis::ExplicitHumanMarking,
        };
    }
    let distinct_workspaces = input.evidence_workspace_ids.len();
    if input.threshold > 0 && distinct_workspaces >= input.threshold {
        GlobalPromotionDecision::Promote {
            basis: GlobalPromotionBasis::CrossWorkspaceEvidence {
                distinct_workspaces,
            },
        }
    } else {
        GlobalPromotionDecision::Deny {
            reason: "insufficient_cross_workspace_evidence",
            distinct_workspaces,
            threshold: input.threshold,
        }
    }
}

/// Inputs to the house-rules pack quota (bd-1n0np.10.3).
#[derive(Clone, Copy, Debug)]
pub struct HouseRulesQuotaInput {
    pub total_budget_tokens: u64,
    pub quota_basis_points: u32,
    pub workspace_opted_out: bool,
}

/// The resolved house-rules section quota for one pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HouseRulesQuota {
    /// Hard token cap for the house-rules section.
    pub cap_tokens: u64,
    /// `false` when the workspace opted out (cap is then zero).
    pub enabled: bool,
}

/// Resolve the capped house-rules quota (bd-1n0np.10.3).
///
/// A per-workspace opt-out disables the section entirely (cap 0). Otherwise the
/// cap is `quota_basis_points` (clamped to 100%) of the total budget — a bounded
/// share so global rules never crowd out project context. Pure and deterministic.
#[must_use]
pub fn house_rules_quota(input: &HouseRulesQuotaInput) -> HouseRulesQuota {
    if input.workspace_opted_out {
        return HouseRulesQuota {
            cap_tokens: 0,
            enabled: false,
        };
    }
    let basis_points = u64::from(input.quota_basis_points.min(10_000));
    let cap_tokens = input.total_budget_tokens.saturating_mul(basis_points) / 10_000;
    HouseRulesQuota {
        cap_tokens,
        enabled: true,
    }
}

/// Greedily select house-rule items (in caller-supplied priority order) whose
/// cumulative token cost stays within `cap_tokens`. Deterministic: ties are
/// resolved by input order, and a single oversize item never blocks later
/// smaller items from filling the remaining cap (bd-1n0np.10.3 — global rules
/// never crowd out, and the section never overflows its quota).
#[must_use]
pub fn select_within_house_rules_quota(item_token_costs: &[u64], cap_tokens: u64) -> Vec<usize> {
    let mut selected = Vec::new();
    let mut used = 0_u64;
    for (index, &cost) in item_token_costs.iter().enumerate() {
        if used.saturating_add(cost) <= cap_tokens {
            used = used.saturating_add(cost);
            selected.push(index);
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspaces(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn explicit_human_marking_always_promotes() {
        let evidence = workspaces(&[]);
        let decision = evaluate_global_promotion_gate(&GlobalPromotionGateInput {
            evidence_workspace_ids: &evidence,
            explicit_human_marking: true,
            threshold: DEFAULT_GLOBAL_PROMOTION_WORKSPACE_THRESHOLD,
        });
        assert_eq!(
            decision,
            GlobalPromotionDecision::Promote {
                basis: GlobalPromotionBasis::ExplicitHumanMarking
            }
        );
        assert!(decision.is_promote());
    }

    #[test]
    fn cross_workspace_evidence_promotes_at_or_above_threshold() {
        let evidence = workspaces(&["ws_a", "ws_b", "ws_c"]);
        let decision = evaluate_global_promotion_gate(&GlobalPromotionGateInput {
            evidence_workspace_ids: &evidence,
            explicit_human_marking: false,
            threshold: 3,
        });
        assert_eq!(
            decision,
            GlobalPromotionDecision::Promote {
                basis: GlobalPromotionBasis::CrossWorkspaceEvidence {
                    distinct_workspaces: 3
                }
            }
        );
    }

    #[test]
    fn insufficient_distinct_workspaces_is_denied() {
        let evidence = workspaces(&["ws_a", "ws_b"]);
        let decision = evaluate_global_promotion_gate(&GlobalPromotionGateInput {
            evidence_workspace_ids: &evidence,
            explicit_human_marking: false,
            threshold: 3,
        });
        assert_eq!(
            decision,
            GlobalPromotionDecision::Deny {
                reason: "insufficient_cross_workspace_evidence",
                distinct_workspaces: 2,
                threshold: 3,
            }
        );
        assert!(!decision.is_promote());
    }

    #[test]
    fn zero_threshold_without_marking_never_promotes() {
        // A misconfigured zero threshold must not silently auto-promote everything.
        let evidence = workspaces(&["ws_a"]);
        let decision = evaluate_global_promotion_gate(&GlobalPromotionGateInput {
            evidence_workspace_ids: &evidence,
            explicit_human_marking: false,
            threshold: 0,
        });
        assert!(!decision.is_promote());
    }

    #[test]
    fn opt_out_disables_house_rules_section() {
        let quota = house_rules_quota(&HouseRulesQuotaInput {
            total_budget_tokens: 10_000,
            quota_basis_points: DEFAULT_HOUSE_RULES_QUOTA_BASIS_POINTS,
            workspace_opted_out: true,
        });
        assert_eq!(
            quota,
            HouseRulesQuota {
                cap_tokens: 0,
                enabled: false
            }
        );
    }

    #[test]
    fn quota_is_a_bounded_share_of_the_budget() {
        let quota = house_rules_quota(&HouseRulesQuotaInput {
            total_budget_tokens: 10_000,
            quota_basis_points: 2_000, // 20%
            workspace_opted_out: false,
        });
        assert_eq!(
            quota,
            HouseRulesQuota {
                cap_tokens: 2_000,
                enabled: true
            }
        );
        // Over-100% basis points clamp to the full budget, never above.
        let clamped = house_rules_quota(&HouseRulesQuotaInput {
            total_budget_tokens: 10_000,
            quota_basis_points: 25_000,
            workspace_opted_out: false,
        });
        assert_eq!(clamped.cap_tokens, 10_000);
    }

    #[test]
    fn selection_stays_within_cap_and_does_not_let_one_item_block_others() {
        // Costs: 30, 100 (too big), 40, 50. Cap 100 -> take 30, skip 100, take 40,
        // skip 50 (30+40+50 > 100). A single oversize item never blocks later fits.
        let selected = select_within_house_rules_quota(&[30, 100, 40, 50], 100);
        assert_eq!(selected, vec![0, 2]);
        let total: u64 = [30_u64, 100, 40, 50]
            .iter()
            .enumerate()
            .filter(|(index, _)| selected.contains(index))
            .map(|(_, cost)| *cost)
            .sum();
        assert!(total <= 100, "selection never exceeds the cap");
    }

    #[test]
    fn empty_selection_when_cap_is_zero() {
        assert!(select_within_house_rules_quota(&[10, 20], 0).is_empty());
    }
}
