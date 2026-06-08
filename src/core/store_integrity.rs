#![forbid(unsafe_code)]

use serde::Serialize;

use crate::core::read_fence::{
    ConsistencyBlock, ConsistencySeverity, ConsistencyVerdict, ReadFence, evaluate_consistency,
};
use crate::core::write_owner::{
    SourceWriteStats, WriteImmuneQuarantineConfig, WriteImmuneQuarantineDecision, WriteOperation,
    WriteStreamObservation, WriteStreamStatsConfig, compute_source_write_stats,
    evaluate_write_immune_quarantine,
};

pub const STORE_INTEGRITY_REPORT_SCHEMA_V1: &str = "ee.store_integrity.report.v1";
pub const STORE_INTEGRITY_READ_FENCE_SCHEMA_V1: &str = "ee.store_integrity.read_fence.v1";
pub const STORE_INTEGRITY_WRITE_IMMUNE_SCHEMA_V1: &str = "ee.store_integrity.write_immune.v1";

#[derive(Clone, Debug)]
pub struct StoreIntegrityOptions {
    pub read_fence: ReadFence,
    pub db_generation: u64,
    pub asset_generations: Vec<(String, u64)>,
    pub strict_read_fence: bool,
    pub write_stream_config: WriteStreamStatsConfig,
    pub write_observations: Vec<WriteStreamObservation>,
    pub quarantine_config: WriteImmuneQuarantineConfig,
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
        WriteStreamObservation {
            operation: WriteOperation::MemoryCreate {
                source_id: self.source_id.clone(),
                content: self.content.clone(),
                trust_class: self.trust_class.clone(),
                provenance_uri: self.provenance_uri.clone(),
                observed_at_ms: self.observed_at_ms,
            },
        }
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
    let read_fence = StoreIntegrityReadFenceReport::from_consistency(&consistency);

    let stats =
        compute_source_write_stats(&options.write_observations, &options.write_stream_config);
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

    let status = if read_fence.strict_failed {
        StoreIntegrityStatus::Blocked
    } else if quarantined_source_count > 0
        || matches!(consistency.severity, ConsistencySeverity::Warning)
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
    fn from_consistency(consistency: &ConsistencyBlock) -> Self {
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
