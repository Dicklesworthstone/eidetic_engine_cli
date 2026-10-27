#![forbid(unsafe_code)]

use serde::Serialize;

use crate::core::read_fence::{
    ConsistencyBlock, ConsistencySeverity, ConsistencyVerdict, ReadFence, evaluate_consistency,
};
use crate::core::write_owner::{
    SourceWriteStats, WriteImmuneQuarantineConfig, WriteImmuneQuarantineDecision,
    WriteStreamObservation, WriteStreamStatsConfig, compute_source_write_stats,
    evaluate_write_immune_quarantine,
};

pub const STORE_INTEGRITY_REPORT_SCHEMA_V1: &str = "ee.store_integrity.report.v1";
pub const STORE_INTEGRITY_READ_FENCE_SCHEMA_V1: &str = "ee.store_integrity.read_fence.v1";
pub const STORE_INTEGRITY_WRITE_IMMUNE_SCHEMA_V1: &str = "ee.store_integrity.write_immune.v1";
pub const READ_FENCE_GENERATION_UNAVAILABLE_REPAIR: &str = "Inspect `ee index status` for the same workspace, database and index directory; repair the store or rebuild the index before retrying.";

#[derive(Clone, Debug)]
pub struct StoreIntegrityOptions {
    pub read_fence: ReadFence,
    pub db_generation: u64,
    pub asset_generations: Vec<(String, u64)>,
    pub strict_read_fence: bool,
    /// Where `db_generation` and `asset_generations` came from. Carried
    /// through to the report so a verdict is never read as a claim about a
    /// store that was never opened (bd-1n0np.8.13).
    pub generation_source: GenerationSource,
    pub write_stream_config: WriteStreamStatsConfig,
    pub write_observations: Vec<WriteStreamObservation>,
    pub quarantine_config: WriteImmuneQuarantineConfig,
}

/// Provenance of the generations the read fence compared (bd-1n0np.8.13).
///
/// A read-fence verdict means something fundamentally different in each case:
/// `Observed` is a claim about a live store, while `Supplied` is arithmetic on
/// numbers the caller already held. Reporting the verdict without its source
/// would make a calculation indistinguishable from a measurement, and
/// `workspace_generation: 0` alone cannot say whether the store is empty,
/// absent, or simply never consulted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationSource {
    /// Every generation was read from the live store.
    Observed,
    /// Every generation came from an explicit caller argument. This is the
    /// default because the pure-function entry point makes no store access.
    #[default]
    Supplied,
    /// Some generations were observed and some were supplied explicitly.
    Mixed,
    /// Observation was attempted and failed — no store, or one that could not
    /// be opened — so the supplied values were used. Deliberately distinct
    /// from `Supplied`: there the caller chose, here the store was missing.
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreIntegrityWriteObservationInput {
    pub source_id: String,
    pub content: String,
    pub trust_class: String,
    pub provenance_uri: Option<String>,
    pub observed_at_ms: u64,
}

impl StoreIntegrityWriteObservationInput {
    #[must_use]
    pub fn to_observation(&self) -> WriteStreamObservation {
        WriteStreamObservation::memory_create(
            self.source_id.clone(),
            &self.content,
            &self.trust_class,
            self.provenance_uri.as_deref(),
            self.observed_at_ms,
        )
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIntegrityReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub status: StoreIntegrityStatus,
    pub read_fence: StoreIntegrityReadFenceReport,
    pub write_immune: StoreIntegrityWriteImmuneReport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreIntegrityStatus {
    Ok,
    Degraded,
    Blocked,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIntegrityReadFenceReport {
    pub schema: &'static str,
    pub mode: String,
    pub verdict: String,
    pub severity: String,
    pub strict_failed: bool,
    /// Provenance of the generations below. Emitted beside the verdict, not
    /// instead of it, so a consumer can tell a measurement from a
    /// calculation (bd-1n0np.8.13).
    pub generation_source: GenerationSource,
    pub workspace_generation: u64,
    pub asset_generations: Vec<StoreIntegrityAssetGeneration>,
    pub stale_assets: Vec<StoreIntegrityStaleAsset>,
    pub snapshot_generation: Option<u64>,
    pub repair: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIntegrityAssetGeneration {
    pub name: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIntegrityStaleAsset {
    pub name: String,
    pub generation: u64,
    pub lag: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIntegrityWriteImmuneReport {
    pub schema: &'static str,
    pub window_start_ms: u64,
    pub window_end_ms: u64,
    pub observation_count: usize,
    pub source_count: usize,
    pub quarantined_source_count: usize,
    pub advisory_only: bool,
    pub global_write_stall: bool,
    pub stats: Vec<SourceWriteStats>,
    pub decisions: Vec<WriteImmuneQuarantineDecision>,
}

#[must_use]
pub fn run_store_integrity_report(options: StoreIntegrityOptions) -> StoreIntegrityReport {
    let consistency = evaluate_consistency(
        options.read_fence,
        options.db_generation,
        options.asset_generations,
        options.strict_read_fence,
    );
    let mut read_fence =
        StoreIntegrityReadFenceReport::from_consistency(&consistency, options.generation_source);

    let stats =
        compute_source_write_stats(&options.write_observations, options.write_stream_config);
    let decisions = stats
        .iter()
        .map(|source_stats| {
            evaluate_write_immune_quarantine(source_stats, &options.quarantine_config)
        })
        .collect::<Vec<_>>();
    let quarantined_source_count = decisions
        .iter()
        .filter(|decision| decision.action == "quarantine")
        .count();
    let write_immune = StoreIntegrityWriteImmuneReport {
        schema: STORE_INTEGRITY_WRITE_IMMUNE_SCHEMA_V1,
        window_start_ms: options.write_stream_config.window_start_ms,
        window_end_ms: options.write_stream_config.window_end_ms,
        observation_count: options.write_observations.len(),
        source_count: stats.len(),
        quarantined_source_count,
        advisory_only: true,
        global_write_stall: false,
        stats,
        decisions,
    };

    // bd-1n0np.8.13: an unobservable store must never read as a clean one.
    //
    // When observation fails, `asset_generations` is empty, so the fence finds
    // nothing behind and would otherwise return `Ok` — reporting health it
    // never measured. That is a silent false negative, and the one failure
    // this surface must not have: the whole point of a read fence is to be
    // trusted when it says the derived assets are current.
    //
    // Under `--strict-read-fence` the caller has asked to fail closed, and
    // "I could not look" is a closed-fail. The cause travels in the same
    // block as `generation_source`, so a reader is never left guessing which
    // condition produced the status.
    let generations_unobservable = options.generation_source == GenerationSource::Unavailable
        || (options.generation_source == GenerationSource::Mixed
            && consistency.asset_generations.is_empty());
    if generations_unobservable {
        // An incomplete observation cannot prove coherence or a pinned replay.
        // Keep the observed values and their provenance, but make the nested
        // verdict agree with the admission decision used by every read surface.
        read_fence.verdict = "unavailable".to_owned();
        read_fence.severity = if options.strict_read_fence {
            "high"
        } else {
            "warning"
        }
        .to_owned();
        read_fence.strict_failed = options.strict_read_fence;
        read_fence.snapshot_generation = None;
        read_fence.repair = Some(READ_FENCE_GENERATION_UNAVAILABLE_REPAIR.to_owned());
    }
    let status = if read_fence.strict_failed {
        StoreIntegrityStatus::Blocked
    } else if quarantined_source_count > 0
        || generations_unobservable
        || !matches!(consistency.severity, ConsistencySeverity::Info)
    {
        StoreIntegrityStatus::Degraded
    } else {
        StoreIntegrityStatus::Ok
    };

    StoreIntegrityReport {
        schema: STORE_INTEGRITY_REPORT_SCHEMA_V1,
        command: "diag store-integrity",
        status,
        read_fence,
        write_immune,
    }
}

impl StoreIntegrityReadFenceReport {
    fn from_consistency(
        consistency: &ConsistencyBlock,
        generation_source: GenerationSource,
    ) -> Self {
        let (stale_assets, snapshot_generation) = match &consistency.verdict {
            ConsistencyVerdict::Coherent => (Vec::new(), None),
            ConsistencyVerdict::AssetsBehind { .. } => (
                consistency
                    .asset_generations
                    .iter()
                    .filter(|(_, generation)| *generation < consistency.db_generation)
                    .map(|(name, generation)| StoreIntegrityStaleAsset {
                        name: name.clone(),
                        generation: *generation,
                        lag: consistency.db_generation.saturating_sub(*generation),
                    })
                    .collect(),
                None,
            ),
            ConsistencyVerdict::PinnedSnapshot { generation } => (Vec::new(), Some(*generation)),
        };

        Self {
            schema: STORE_INTEGRITY_READ_FENCE_SCHEMA_V1,
            mode: consistency.mode.to_string(),
            verdict: consistency.verdict.as_str().to_string(),
            severity: consistency.severity.as_str().to_string(),
            strict_failed: consistency.strict_failed,
            generation_source,
            workspace_generation: consistency.db_generation,
            asset_generations: consistency
                .asset_generations
                .iter()
                .map(|(name, generation)| StoreIntegrityAssetGeneration {
                    name: name.clone(),
                    generation: *generation,
                })
                .collect(),
            stale_assets,
            snapshot_generation,
            repair: consistency.repair.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_generations_never_claim_coherence_or_a_pinned_snapshot() {
        for read_fence in [
            ReadFence::Eventual,
            ReadFence::Latest,
            ReadFence::Snapshot(7),
        ] {
            for strict in [false, true] {
                for asset_generations in [Vec::new(), vec![("search_index".to_owned(), 7)]] {
                    let report = run_store_integrity_report(StoreIntegrityOptions {
                        read_fence,
                        db_generation: 0,
                        asset_generations,
                        strict_read_fence: strict,
                        generation_source: GenerationSource::Unavailable,
                        write_stream_config: WriteStreamStatsConfig::new(0, 1_000, 3),
                        write_observations: Vec::new(),
                        quarantine_config: WriteImmuneQuarantineConfig::default(),
                    });
                    assert_eq!(report.read_fence.verdict, "unavailable");
                    assert_eq!(report.read_fence.strict_failed, strict);
                    assert_eq!(report.read_fence.snapshot_generation, None);
                    assert_eq!(
                        report.read_fence.generation_source,
                        GenerationSource::Unavailable
                    );
                    assert_eq!(
                        report.read_fence.repair.as_deref(),
                        Some(READ_FENCE_GENERATION_UNAVAILABLE_REPAIR)
                    );
                    assert_eq!(
                        report.status,
                        if strict {
                            StoreIntegrityStatus::Blocked
                        } else {
                            StoreIntegrityStatus::Degraded
                        }
                    );
                    assert_eq!(
                        report.read_fence.severity,
                        if strict { "high" } else { "warning" }
                    );
                }
            }
        }
    }
}
