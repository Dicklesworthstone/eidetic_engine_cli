//! bd-1n0np.7.5 — pack-time contradiction guard: decision core.
//!
//! The pack guard must never include both sides of an *unresolved hard
//! contradiction* in a context pack: it keeps the higher-trust / fresher side
//! and flags the other `contradiction_suppressed`. An opt-in `forced` mode
//! instead surfaces both sides under a `## Contradictions` header, ranked + capped.
//!
//! This module is the pure decision core (the proven decision-core-vs-I/O split,
//! mirroring `models::memory_sentinel::SentinelObservation`): it answers "which
//! contradiction pairs are still unresolved?" and "given two contradicting
//! memories, which one survives and why?" without touching the DB or the pack
//! pipeline. The caller resolves the unresolved set from the 7.2 detector
//! (`core::contradiction_detect::detect_explicit_contradictions_from_connection`)
//! minus recorded resolutions (7.4), then applies these decisions during pack
//! assembly. Deterministic and panic-free.

use std::cmp::Ordering;
use std::collections::BTreeSet;

/// Default cap on how many contradiction sides `forced` mode surfaces under the
/// `## Contradictions` header (the rest are summarized as a count — never a
/// silent drop).
pub const DEFAULT_FORCED_CONTRADICTION_CAP: usize = 8;

/// A memory's standing used to choose which side of a contradiction survives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardedMemory {
    pub memory_id: String,
    /// Higher = more trusted (milli-units, so callers can pass fixed-point trust).
    pub trust_milli: i64,
    /// Higher = fresher (e.g. updated-at epoch seconds).
    pub freshness_epoch: i64,
}

/// Why one side of a contradiction was kept over the other.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuppressionBasis {
    /// The kept side had strictly higher trust.
    HigherTrust,
    /// Trust tied; the kept side was fresher.
    Fresher,
    /// Trust and freshness tied; broken deterministically by memory id.
    DeterministicTieBreak,
}

impl SuppressionBasis {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HigherTrust => "higher_trust",
            Self::Fresher => "fresher",
            Self::DeterministicTieBreak => "deterministic_tie_break",
        }
    }
}

/// The pack-guard decision for one unresolved hard-contradiction pair: keep one
/// side, suppress the other. Never drops both.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContradictionSuppression {
    pub kept_memory_id: String,
    pub suppressed_memory_id: String,
    pub basis: SuppressionBasis,
}

/// Decide which side of a contradiction to keep: higher trust, then fresher,
/// then the lexically-smaller memory id (a deterministic, never-both-dropped
/// tie-break). Pure.
#[must_use]
pub fn decide_contradiction_survivor(
    left: &GuardedMemory,
    right: &GuardedMemory,
) -> ContradictionSuppression {
    let (keep, suppress, basis) = match left.trust_milli.cmp(&right.trust_milli) {
        Ordering::Greater => (left, right, SuppressionBasis::HigherTrust),
        Ordering::Less => (right, left, SuppressionBasis::HigherTrust),
        Ordering::Equal => match left.freshness_epoch.cmp(&right.freshness_epoch) {
            Ordering::Greater => (left, right, SuppressionBasis::Fresher),
            Ordering::Less => (right, left, SuppressionBasis::Fresher),
            Ordering::Equal => {
                if left.memory_id <= right.memory_id {
                    (left, right, SuppressionBasis::DeterministicTieBreak)
                } else {
                    (right, left, SuppressionBasis::DeterministicTieBreak)
                }
            }
        },
    };
    ContradictionSuppression {
        kept_memory_id: keep.memory_id.clone(),
        suppressed_memory_id: suppress.memory_id.clone(),
        basis,
    }
}

/// Canonicalize a pair to an unordered, trimmed `(low, high)`, dropping blanks
/// and self-loops.
fn canonical_pair(a: &str, b: &str) -> Option<(String, String)> {
    let a = a.trim();
    let b = b.trim();
    if a.is_empty() || b.is_empty() || a == b {
        return None;
    }
    if a <= b {
        Some((a.to_string(), b.to_string()))
    } else {
        Some((b.to_string(), a.to_string()))
    }
}

/// The unresolved hard-contradiction set: detected contradiction pairs (from the
/// 7.2 detector) minus pairs that already carry a recorded resolution (7.4).
/// Deterministic: canonicalized, deduplicated, sorted.
#[must_use]
pub fn unresolved_contradiction_pairs(
    detected: &[(String, String)],
    resolved: &[(String, String)],
) -> Vec<(String, String)> {
    let resolved_set: BTreeSet<(String, String)> = resolved
        .iter()
        .filter_map(|(a, b)| canonical_pair(a, b))
        .collect();
    let mut unresolved: BTreeSet<(String, String)> = BTreeSet::new();
    for (a, b) in detected {
        if let Some(pair) = canonical_pair(a, b)
            && !resolved_set.contains(&pair)
        {
            unresolved.insert(pair);
        }
    }
    unresolved.into_iter().collect()
}

/// Whether a memory id participates in any unresolved contradiction.
#[must_use]
pub fn is_in_unresolved_contradiction(memory_id: &str, unresolved: &[(String, String)]) -> bool {
    let id = memory_id.trim();
    !id.is_empty() && unresolved.iter().any(|(a, b)| a == id || b == id)
}

/// `forced`-mode view of one contradiction's members: ranked by trust, then
/// freshness, then id, capped to `cap`. `total` is always the full count so the
/// cap is never a silent drop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForcedContradictionView {
    /// Ranked memory ids, capped to `cap`.
    pub shown: Vec<String>,
    /// Total members before the cap.
    pub total: usize,
}

/// Rank + cap contradiction members for `forced` mode. Deterministic.
#[must_use]
pub fn forced_contradiction_view(members: &[GuardedMemory], cap: usize) -> ForcedContradictionView {
    let mut ranked: Vec<&GuardedMemory> = members.iter().collect();
    ranked.sort_by(|a, b| {
        b.trust_milli
            .cmp(&a.trust_milli)
            .then(b.freshness_epoch.cmp(&a.freshness_epoch))
            .then(a.memory_id.cmp(&b.memory_id))
    });
    let total = ranked.len();
    let shown = ranked
        .into_iter()
        .take(cap)
        .map(|memory| memory.memory_id.clone())
        .collect();
    ForcedContradictionView { shown, total }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_FORCED_CONTRADICTION_CAP, GuardedMemory, SuppressionBasis,
        decide_contradiction_survivor, forced_contradiction_view, is_in_unresolved_contradiction,
        unresolved_contradiction_pairs,
    };

    fn mem(id: &str, trust_milli: i64, freshness_epoch: i64) -> GuardedMemory {
        GuardedMemory {
            memory_id: id.to_string(),
            trust_milli,
            freshness_epoch,
        }
    }

    #[test]
    fn survivor_prefers_higher_trust_then_fresher_then_id() {
        // Higher trust wins regardless of freshness.
        let d = decide_contradiction_survivor(&mem("a", 900, 1), &mem("b", 100, 999));
        assert_eq!(d.kept_memory_id, "a");
        assert_eq!(d.suppressed_memory_id, "b");
        assert_eq!(d.basis, SuppressionBasis::HigherTrust);

        // Trust tie -> fresher wins.
        let d = decide_contradiction_survivor(&mem("a", 500, 10), &mem("b", 500, 20));
        assert_eq!(d.kept_memory_id, "b");
        assert_eq!(d.basis, SuppressionBasis::Fresher);

        // Full tie -> deterministic by id (lexically smaller kept).
        let d = decide_contradiction_survivor(&mem("z", 500, 10), &mem("a", 500, 10));
        assert_eq!(d.kept_memory_id, "a");
        assert_eq!(d.basis, SuppressionBasis::DeterministicTieBreak);
    }

    #[test]
    fn survivor_decision_is_symmetric_in_argument_order() {
        let forward = decide_contradiction_survivor(&mem("a", 500, 20), &mem("b", 700, 10));
        let reversed = decide_contradiction_survivor(&mem("b", 700, 10), &mem("a", 500, 20));
        assert_eq!(
            forward, reversed,
            "the survivor must not depend on arg order"
        );
        assert_eq!(forward.kept_memory_id, "b");
    }

    #[test]
    fn unresolved_set_is_detected_minus_resolved() {
        let detected = vec![
            ("mem_a".to_string(), "mem_b".to_string()),
            ("mem_c".to_string(), "mem_d".to_string()),
            // duplicate in the other order — must collapse.
            ("mem_b".to_string(), "mem_a".to_string()),
        ];
        // Resolved pair given in the opposite order — canonicalization must match.
        let resolved = vec![("mem_b".to_string(), "mem_a".to_string())];
        let unresolved = unresolved_contradiction_pairs(&detected, &resolved);
        assert_eq!(
            unresolved,
            vec![("mem_c".to_string(), "mem_d".to_string())],
            "a->b is resolved and dedups; only c-d remains unresolved"
        );
        assert!(is_in_unresolved_contradiction("mem_c", &unresolved));
        assert!(!is_in_unresolved_contradiction("mem_a", &unresolved));
        assert!(!is_in_unresolved_contradiction("", &unresolved));
    }

    #[test]
    fn forced_view_ranks_caps_and_reports_total_no_silent_drop() {
        let members = vec![mem("low", 100, 1), mem("high", 900, 1), mem("mid", 500, 1)];
        let view = forced_contradiction_view(&members, 2);
        assert_eq!(
            view.total, 3,
            "total must reflect all members despite the cap"
        );
        assert_eq!(view.shown, vec!["high".to_string(), "mid".to_string()]);
        // A generous cap shows everyone.
        let full = forced_contradiction_view(&members, DEFAULT_FORCED_CONTRADICTION_CAP);
        assert_eq!(full.shown.len(), 3);
    }
}
