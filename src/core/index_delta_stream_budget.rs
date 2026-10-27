//! Streaming counterpart of the ordinary, growth and maintenance admission policies.
//!
//! Final source size is unknown until reconciliation finishes. Never infer
//! net growth from a prefix, nor renew inference allowance after a fallback.
//! Differential tests compare these decisions with the complete V3 planner.

use super::{IncrementalFallback, invalid, over_limit};

// These are the existing V3 planner ceilings, not increased inference limits.
// Keep the streaming collector independent of retained document bodies; its
// differential tests must change if metadata/append policy changes.
pub(super) const MAX_MUTATIONS: usize = 65_536;
const ORDINARY_MUTATIONS: usize = 8192;

fn inference_limit(live: usize, current: usize) -> usize {
    (live.max(current).saturating_mul(super::MAX_DELTA_PERCENT) / 100).clamp(1, ORDINARY_MUTATIONS)
}

/// All counts describe the same complete source snapshot. New inputs are
/// distinct identities, not distinct strings. Rewrites are inference inputs
/// belonging to old identities. Metadata-only upserts consume mutation/byte
/// capacity but no inference capacity. Removal bytes are charged by the caller.
pub(super) fn validate(
    live: usize,
    current: usize,
    upserts: usize,
    removals: usize,
    inference: usize,
    additions: usize,
) -> Result<(), IncrementalFallback> {
    if additions > inference
        || inference > upserts
        || upserts > current
        || removals > live
        || current.checked_sub(additions) != live.checked_sub(removals)
    {
        return Err(invalid(
            "streaming source mutation accounting is inconsistent",
        ));
    }
    let mutations = upserts
        .checked_add(removals)
        .ok_or_else(|| over_limit("streaming source delta exceeds the mutation budget"))?;
    if mutations > MAX_MUTATIONS {
        return Err(over_limit(
            "streaming source delta exceeds the mutation budget",
        ));
    }
    // Mirror the ordinary planner first, before considering its bulk fallback.
    // All inference is counted here; the growing-source fallback below has a
    // distinct allowance for previously absent identities.
    if mutations <= ORDINARY_MUTATIONS && inference <= inference_limit(live, current) {
        return Ok(());
    }
    if current <= live {
        // Rolling maintenance shares one allowance across new and rewritten
        // inputs. A deletion or metadata refresh cannot buy more inference.
        if inference <= inference_limit(live, live) {
            return Ok(());
        }
    } else {
        // Bulk imports may add many NEW inputs. Mutations and rewrites of the
        // existing corpus keep their own old-corpus ceilings, so growing the
        // archive cannot disguise a wholesale rewrite of existing history.
        let existing_mutations = mutations - additions;
        let rewrites = inference - additions;
        if existing_mutations <= ORDINARY_MUTATIONS && rewrites <= inference_limit(live, live) {
            return Ok(());
        }
    }
    Err(over_limit(
        "streaming source delta exceeds the applicable inference or existing-mutation budget",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_preserves_separate_new_input_and_old_rewrite_allowances() {
        // Eight added identities do not buy a fifth rewrite of old inputs.
        // A bulk import may exceed the ordinary combined inference bound.
        assert!(validate(16, 24, 13, 0, 13, 8).is_err());
        assert!(validate(16, 20, 6, 0, 5, 4).is_ok());
        assert!(validate(16, 24, 6, 0, 5, 8).is_err());
        assert!(validate(16, 17, 5, 0, 5, 1).is_ok());
    }

    #[test]
    fn growing_and_non_growing_sources_cannot_borrow_each_others_allowances() {
        assert!(validate(32_768, 32_768, 12_288, 8192, 8192, 8192).is_ok());
        assert!(validate(32_768, 32_768, 12_289, 8192, 8193, 8192).is_err());
        assert!(validate(1024, 66_304, 65_536, 0, 65_536, 65_280).is_ok());
        assert!(validate(1024, 66_304, 65_537, 0, 65_537, 65_280).is_err());
        assert!(validate(20_000, 21_000, 10_000, 0, 1000, 1000).is_err());
        assert!(validate(20_000, 20_000, 10_000, 0, 16, 0).is_ok());
    }

    #[test]
    fn accounting_and_checked_arithmetic_refuse_impossible_snapshots() {
        for counts in [
            (10, 10, 1, 0, 2, 0),
            (10, 10, 2, 0, 1, 2),
            (10, 10, 2, 11, 1, 0),
            (10, 10, 11, 0, 1, 0),
            (10, 10, 1, 1, 0, 0),
            (
                usize::MAX,
                usize::MAX,
                usize::MAX,
                usize::MAX,
                0,
                usize::MAX,
            ),
        ] {
            let (live, current, upserts, removals, inference, additions) = counts;
            assert!(validate(live, current, upserts, removals, inference, additions).is_err());
        }
        assert!(validate(0, 0, 0, 0, 0, 0).is_ok());
        assert!(
            validate(
                0,
                MAX_MUTATIONS,
                MAX_MUTATIONS,
                0,
                MAX_MUTATIONS,
                MAX_MUTATIONS
            )
            .is_ok()
        );
        assert!(validate(MAX_MUTATIONS, 0, 0, MAX_MUTATIONS, 0, 0).is_ok());
        assert!(validate(MAX_MUTATIONS + 1, 0, 0, MAX_MUTATIONS + 1, 0, 0).is_err());
    }
}
