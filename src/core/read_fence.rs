//! bd-1n0np.8.3 — Read-fence consistency model for Multi-Agent Store Integrity.
//!
//! In a crowded checkout, a context-producing command (`search`/`pack`/`why`)
//! may serve results from a derived asset (search index, graph snapshot) that
//! lags the FrankenSQLite DB generation. This module is the pure model + verdict
//! logic for stating, on every such response, "coherent as of generation N" or
//! "used an index K writes behind the DB; here is the repair."
//!
//! Design (per bead notes): `Eventual` is the fast default — lag is reported,
//! never enforced — so the common path is not slowed. `Latest` is the opt-in
//! high-stakes mode that fails (in strict) when any derived asset trails the DB.
//! `Snapshot(n)` replays a pinned generation. The emitted [`ConsistencyBlock`]
//! is intentionally cleanly-additive and stable-ordered so wiring it onto
//! responses is a single coordinated golden update (the threading + emission is
//! the follow-on; this module is pure and golden-free).

/// Stable schema id for the consistency block emitted onto responses.
pub const READ_FENCE_CONSISTENCY_SCHEMA_V1: &str = "ee.read_fence.consistency.v1";

/// Default repair when a search/index asset trails the DB generation.
pub const READ_FENCE_INDEX_REPAIR: &str = "ee index rebuild --workspace .";
/// Repair when a graph snapshot trails the DB generation.
pub const READ_FENCE_GRAPH_REPAIR: &str = "ee graph centrality-refresh --workspace .";
/// Repair when the pack cache trails the DB generation.
pub const READ_FENCE_CACHE_REPAIR: &str = "ee pack \"<task>\" --workspace . --json";
/// Fallback repair when the stale asset name is not recognized by this model.
pub const READ_FENCE_GENERIC_REPAIR: &str =
    "Inspect stale derived asset generations before retrying.";
/// Backwards-compatible alias for the historical search-index repair.
pub const READ_FENCE_REPAIR: &str = READ_FENCE_INDEX_REPAIR;

/// Requested read coherence for a context-producing command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReadFence {
    /// Fast default: derived assets may lag the DB; lag is reported, not enforced.
    #[default]
    Eventual,
    /// High-stakes opt-in: require every derived asset to be >= the DB generation.
    Latest,
    /// Replay a pinned workspace generation.
    Snapshot(u64),
}

impl ReadFence {
    /// Stable lowercase mode label for serialization and logging.
    #[must_use]
    pub const fn mode_str(self) -> &'static str {
        match self {
            Self::Eventual => "eventual",
            Self::Latest => "latest",
            Self::Snapshot(_) => "snapshot",
        }
    }
}

/// Severity of a consistency finding (subset of the response severity ladder).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsistencySeverity {
    Info,
    Warning,
    High,
}

impl ConsistencySeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::High => "high",
        }
    }
}

/// The coherence verdict for a read against the current generations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConsistencyVerdict {
    /// Every derived asset is at or ahead of the DB generation.
    Coherent,
    /// One or more derived assets trail the DB generation. `max_lag` is the
    /// largest gap; `behind_assets` names the lagging assets (sorted).
    AssetsBehind {
        max_lag: u64,
        behind_assets: Vec<String>,
    },
    /// A pinned snapshot generation was replayed.
    PinnedSnapshot { generation: u64 },
}

impl ConsistencyVerdict {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Coherent => "coherent",
            Self::AssetsBehind { .. } => "assets_behind",
            Self::PinnedSnapshot { .. } => "pinned_snapshot",
        }
    }
}

/// A cleanly-additive, stable-ordered consistency block for response emission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsistencyBlock {
    pub schema: &'static str,
    pub mode: &'static str,
    pub db_generation: u64,
    /// `(asset_name, asset_generation)` pairs, sorted by asset name for
    /// deterministic output.
    pub asset_generations: Vec<(String, u64)>,
    pub verdict: ConsistencyVerdict,
    pub severity: ConsistencySeverity,
    pub repair: Option<String>,
    /// `true` only in `Latest` + strict mode when an asset trails the DB — the
    /// caller should fail closed (exit code 6 / `degraded_required`).
    pub strict_failed: bool,
}

/// Evaluate read consistency for `fence` against the DB generation and the
/// derived-asset generations. Pure and deterministic: the same inputs always
/// yield the same block, and `asset_generations` is sorted by name.
///
/// `Eventual` reports lag as `warning` (advisory, never failing). `Latest`
/// escalates lag to `high`, and sets `strict_failed` when `strict` is on.
/// `Snapshot` is always an informational pinned replay.
#[must_use]
pub fn evaluate_consistency(
    fence: ReadFence,
    db_generation: u64,
    asset_generations: Vec<(String, u64)>,
    strict: bool,
) -> ConsistencyBlock {
    let mut asset_generations = asset_generations;
    asset_generations.sort_by(|left, right| left.0.cmp(&right.0));

    if let ReadFence::Snapshot(generation) = fence {
        return ConsistencyBlock {
            schema: READ_FENCE_CONSISTENCY_SCHEMA_V1,
            mode: fence.mode_str(),
            db_generation,
            asset_generations,
            verdict: ConsistencyVerdict::PinnedSnapshot { generation },
            severity: ConsistencySeverity::Info,
            repair: None,
            strict_failed: false,
        };
    }

    let behind_assets: Vec<String> = asset_generations
        .iter()
        .filter(|(_, generation)| *generation < db_generation)
        .map(|(name, _)| name.clone())
        .collect();

    if behind_assets.is_empty() {
        return ConsistencyBlock {
            schema: READ_FENCE_CONSISTENCY_SCHEMA_V1,
            mode: fence.mode_str(),
            db_generation,
            asset_generations,
            verdict: ConsistencyVerdict::Coherent,
            severity: ConsistencySeverity::Info,
            repair: None,
            strict_failed: false,
        };
    }

    let max_lag = asset_generations
        .iter()
        .filter(|(_, generation)| *generation < db_generation)
        .map(|(_, generation)| db_generation.saturating_sub(*generation))
        .max()
        .unwrap_or(0);
    let latest = matches!(fence, ReadFence::Latest);

    let repair = repair_for_behind_assets(&behind_assets);

    ConsistencyBlock {
        schema: READ_FENCE_CONSISTENCY_SCHEMA_V1,
        mode: fence.mode_str(),
        db_generation,
        asset_generations,
        verdict: ConsistencyVerdict::AssetsBehind {
            max_lag,
            behind_assets,
        },
        severity: if latest {
            ConsistencySeverity::High
        } else {
            ConsistencySeverity::Warning
        },
        repair: Some(repair),
        strict_failed: latest && strict,
    }
}

fn repair_for_behind_assets(behind_assets: &[String]) -> String {
    let mut repairs = Vec::new();
    let mut saw_unknown = false;

    for asset in behind_assets {
        if let Some(repair) = repair_for_asset(asset) {
            push_unique_repair(&mut repairs, repair);
        } else {
            saw_unknown = true;
        }
    }

    if saw_unknown {
        push_unique_repair(&mut repairs, READ_FENCE_GENERIC_REPAIR);
    }

    if repairs.is_empty() {
        READ_FENCE_GENERIC_REPAIR.to_owned()
    } else {
        repairs.join(" && ")
    }
}

fn repair_for_asset(asset: &str) -> Option<&'static str> {
    let normalized = asset.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "search" | "search_index" | "index" => Some(READ_FENCE_INDEX_REPAIR),
        "graph" | "graph_snapshot" | "graph_snapshot_artifact" => Some(READ_FENCE_GRAPH_REPAIR),
        "cache" | "pack_cache" | "pack_l2_cache" | "l2_pack_cache" => Some(READ_FENCE_CACHE_REPAIR),
        _ => None,
    }
}

fn push_unique_repair(repairs: &mut Vec<&'static str>, repair: &'static str) {
    if !repairs.contains(&repair) {
        repairs.push(repair);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConsistencySeverity, ConsistencyVerdict, READ_FENCE_CACHE_REPAIR,
        READ_FENCE_CONSISTENCY_SCHEMA_V1, READ_FENCE_GRAPH_REPAIR, READ_FENCE_INDEX_REPAIR,
        ReadFence, evaluate_consistency,
    };

    fn assets() -> Vec<(String, u64)> {
        vec![
            ("search_index".to_string(), 12),
            ("graph_snapshot".to_string(), 9),
        ]
    }

    #[test]
    fn eventual_with_current_assets_is_coherent_info() {
        let current = vec![("search_index".to_string(), 12), ("graph".to_string(), 12)];
        let block = evaluate_consistency(ReadFence::Eventual, 12, current, false);
        assert_eq!(block.verdict, ConsistencyVerdict::Coherent);
        assert_eq!(block.severity, ConsistencySeverity::Info);
        assert!(block.repair.is_none());
        assert!(!block.strict_failed);
        assert_eq!(block.schema, READ_FENCE_CONSISTENCY_SCHEMA_V1);
    }

    #[test]
    fn eventual_with_lag_is_warning_not_failing() {
        let block = evaluate_consistency(ReadFence::Eventual, 12, assets(), true);
        match &block.verdict {
            ConsistencyVerdict::AssetsBehind {
                max_lag,
                behind_assets,
            } => {
                assert_eq!(*max_lag, 3); // 12 - 9
                assert_eq!(behind_assets, &vec!["graph_snapshot".to_string()]);
            }
            other => panic!("expected AssetsBehind, got {other:?}"),
        }
        assert_eq!(block.severity, ConsistencySeverity::Warning);
        assert!(block.repair.is_some());
        // Eventual never fails closed, even under strict.
        assert!(!block.strict_failed);
        assert_eq!(block.mode, "eventual");
    }

    #[test]
    fn latest_strict_with_lag_fails_high() {
        let block = evaluate_consistency(ReadFence::Latest, 12, assets(), true);
        assert_eq!(block.severity, ConsistencySeverity::High);
        assert!(block.strict_failed);
        assert_eq!(block.mode, "latest");
    }

    #[test]
    fn latest_non_strict_with_lag_is_high_but_does_not_fail() {
        let block = evaluate_consistency(ReadFence::Latest, 12, assets(), false);
        assert_eq!(block.severity, ConsistencySeverity::High);
        assert!(!block.strict_failed);
    }

    #[test]
    fn snapshot_is_pinned_info() {
        let block = evaluate_consistency(ReadFence::Snapshot(7), 12, assets(), true);
        assert_eq!(
            block.verdict,
            ConsistencyVerdict::PinnedSnapshot { generation: 7 }
        );
        assert_eq!(block.severity, ConsistencySeverity::Info);
        assert!(!block.strict_failed);
        assert_eq!(block.mode, "snapshot");
    }

    #[test]
    fn asset_generations_are_sorted_for_determinism() {
        let unsorted = vec![
            ("z_asset".to_string(), 12),
            ("a_asset".to_string(), 12),
            ("m_asset".to_string(), 12),
        ];
        let block = evaluate_consistency(ReadFence::Eventual, 12, unsorted, false);
        let names: Vec<&str> = block
            .asset_generations
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, vec!["a_asset", "m_asset", "z_asset"]);
    }

    #[test]
    fn graph_only_lag_uses_graph_refresh_repair() {
        let block = evaluate_consistency(
            ReadFence::Latest,
            12,
            vec![("search".to_string(), 12), ("graph".to_string(), 11)],
            false,
        );

        assert_eq!(block.repair.as_deref(), Some(READ_FENCE_GRAPH_REPAIR));
    }

    #[test]
    fn pack_cache_lag_uses_pack_repair_with_required_task_placeholder() {
        let block = evaluate_consistency(
            ReadFence::Latest,
            12,
            vec![("l2_pack_cache".to_string(), 11)],
            false,
        );

        assert_eq!(block.repair.as_deref(), Some(READ_FENCE_CACHE_REPAIR));
        assert!(READ_FENCE_CACHE_REPAIR.contains("\"<task>\""));
    }

    #[test]
    fn mixed_lag_reports_all_required_repairs_in_stable_order() {
        let block = evaluate_consistency(
            ReadFence::Latest,
            12,
            vec![
                ("search_index".to_string(), 10),
                ("graph_snapshot".to_string(), 11),
            ],
            false,
        );

        let expected = format!("{READ_FENCE_GRAPH_REPAIR} && {READ_FENCE_INDEX_REPAIR}");
        assert_eq!(block.repair.as_deref(), Some(expected.as_str()));
    }
}
