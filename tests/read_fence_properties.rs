//! Property / invariant tests for the read-fence consistency model
//! (bd-1n0np.8.4), over `ee::core::read_fence::evaluate_consistency`.
//!
//! These exercise the model across a deterministic grid of generations, fences,
//! and strictness rather than single points, locking the load-bearing
//! invariants: Eventual never fails closed, Latest fails iff an asset lags under
//! strict, Coherent iff no asset lags, max_lag is exact, Snapshot is always a
//! pinned info, output is deterministic, and asset rows are sorted.

use ee::core::read_fence::{
    ConsistencySeverity, ConsistencyVerdict, ReadFence, evaluate_consistency,
};

/// A small but representative grid of `(db_generation, asset_generations)` cases.
fn cases() -> Vec<(u64, Vec<(String, u64)>)> {
    vec![
        (0, vec![]),
        (5, vec![("search_index".to_string(), 5)]),
        (5, vec![("search_index".to_string(), 4)]),
        (
            10,
            vec![
                ("search_index".to_string(), 10),
                ("graph_snapshot".to_string(), 10),
            ],
        ),
        (
            10,
            vec![
                ("search_index".to_string(), 10),
                ("graph_snapshot".to_string(), 2),
            ],
        ),
        (
            12,
            vec![
                ("a".to_string(), 11),
                ("b".to_string(), 12),
                ("c".to_string(), 13),
            ],
        ),
    ]
}

fn any_behind(db_generation: u64, assets: &[(String, u64)]) -> bool {
    assets
        .iter()
        .any(|(_, generation)| *generation < db_generation)
}

#[test]
fn eventual_never_fails_closed() {
    for (db_generation, assets) in cases() {
        for strict in [false, true] {
            let block =
                evaluate_consistency(ReadFence::Eventual, db_generation, assets.clone(), strict);
            assert!(
                !block.strict_failed,
                "Eventual must never fail closed (db={db_generation}, strict={strict})"
            );
            assert_eq!(block.mode, "eventual");
        }
    }
}

#[test]
fn latest_strict_fails_iff_an_asset_lags() {
    for (db_generation, assets) in cases() {
        let behind = any_behind(db_generation, &assets);
        let strict = evaluate_consistency(ReadFence::Latest, db_generation, assets.clone(), true);
        assert_eq!(
            strict.strict_failed, behind,
            "Latest+strict strict_failed must equal 'an asset lags' (db={db_generation})"
        );
        // Non-strict Latest never fails closed, even when lagging.
        let lenient = evaluate_consistency(ReadFence::Latest, db_generation, assets, false);
        assert!(!lenient.strict_failed);
    }
}

#[test]
fn coherent_iff_no_asset_lags_for_non_snapshot() {
    for fence in [ReadFence::Eventual, ReadFence::Latest] {
        for (db_generation, assets) in cases() {
            let behind = any_behind(db_generation, &assets);
            let block = evaluate_consistency(fence, db_generation, assets, false);
            let is_coherent = matches!(block.verdict, ConsistencyVerdict::Coherent);
            assert_eq!(
                is_coherent, !behind,
                "coherent iff no asset lags (fence={:?}, db={db_generation})",
                fence
            );
        }
    }
}

#[test]
fn assets_behind_reports_exact_max_lag_and_sorted_names() {
    let assets = vec![
        ("z".to_string(), 2),
        ("a".to_string(), 9),
        ("m".to_string(), 12),
    ];
    let block = evaluate_consistency(ReadFence::Latest, 12, assets, false);
    match block.verdict {
        ConsistencyVerdict::AssetsBehind {
            max_lag,
            behind_assets,
        } => {
            assert_eq!(max_lag, 10, "max lag is 12 - 2");
            // Behind assets are the ones < 12, surfaced in sorted order.
            assert_eq!(behind_assets, vec!["a".to_string(), "z".to_string()]);
        }
        other => panic!("expected AssetsBehind, got {other:?}"),
    }
    assert_eq!(block.severity, ConsistencySeverity::High);
    assert!(block.repair.is_some());
}

#[test]
fn snapshot_is_always_pinned_info_regardless_of_lag() {
    for (db_generation, assets) in cases() {
        for strict in [false, true] {
            let block = evaluate_consistency(
                ReadFence::Snapshot(3),
                db_generation,
                assets.clone(),
                strict,
            );
            assert_eq!(
                block.verdict,
                ConsistencyVerdict::PinnedSnapshot { generation: 3 }
            );
            assert_eq!(block.severity, ConsistencySeverity::Info);
            assert!(!block.strict_failed);
            assert!(block.repair.is_none());
            assert_eq!(block.mode, "snapshot");
        }
    }
}

#[test]
fn evaluation_is_deterministic() {
    for fence in [
        ReadFence::Eventual,
        ReadFence::Latest,
        ReadFence::Snapshot(4),
    ] {
        for (db_generation, assets) in cases() {
            for strict in [false, true] {
                let first = evaluate_consistency(fence, db_generation, assets.clone(), strict);
                let second = evaluate_consistency(fence, db_generation, assets.clone(), strict);
                assert_eq!(first, second, "same inputs must yield the same block");
            }
        }
    }
}

#[test]
fn asset_generations_are_always_sorted_in_output() {
    let unsorted = vec![
        ("zeta".to_string(), 12),
        ("alpha".to_string(), 12),
        ("mu".to_string(), 12),
    ];
    for fence in [
        ReadFence::Eventual,
        ReadFence::Latest,
        ReadFence::Snapshot(1),
    ] {
        let block = evaluate_consistency(fence, 12, unsorted.clone(), false);
        let names: Vec<&str> = block
            .asset_generations
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"], "fence={fence:?}");
    }
}
