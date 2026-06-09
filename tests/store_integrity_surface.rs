#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use ee::core::read_fence::ReadFence;
use ee::core::store_integrity::{
    StoreIntegrityOptions, StoreIntegrityReport, StoreIntegrityStatus,
    StoreIntegrityWriteObservationInput, run_store_integrity_report,
};
use ee::core::write_owner::{WriteImmuneQuarantineConfig, WriteStreamStatsConfig};
use ee::output::render_store_integrity_json;
use serde_json::Value;

fn observation(
    source_id: &str,
    content: &str,
    trust_class: &str,
    provenance_uri: Option<&str>,
    observed_at_ms: u64,
) -> StoreIntegrityWriteObservationInput {
    StoreIntegrityWriteObservationInput {
        source_id: source_id.to_owned(),
        content: content.to_owned(),
        trust_class: trust_class.to_owned(),
        provenance_uri: provenance_uri.map(str::to_owned),
        observed_at_ms,
    }
}

fn quarantine_config() -> WriteImmuneQuarantineConfig {
    WriteImmuneQuarantineConfig {
        max_writes_per_window: 2,
        max_near_duplicate_ratio: 1.0,
        max_missing_evidence_ratio: 0.75,
        max_high_trust_missing_evidence_ratio: 0.20,
        high_trust_classes: BTreeSet::from(["agent_validated".to_owned()]),
        source_whitelist: BTreeSet::new(),
    }
}

fn fixture_report() -> StoreIntegrityReport {
    let observations = [
        observation("noisy-agent", "same content", "agent_validated", None, 10),
        observation("noisy-agent", "same content", "agent_validated", None, 20),
        observation("noisy-agent", "same content", "agent_validated", None, 30),
        observation(
            "normal-agent",
            "different content",
            "agent_validated",
            Some("cass://session/1"),
            40,
        ),
    ]
    .iter()
    .map(StoreIntegrityWriteObservationInput::to_observation)
    .collect();

    run_store_integrity_report(StoreIntegrityOptions {
        read_fence: ReadFence::Latest,
        db_generation: 12,
        asset_generations: vec![("search".to_owned(), 12), ("graph".to_owned(), 11)],
        strict_read_fence: true,
        write_stream_config: WriteStreamStatsConfig::new(0, 1_000, 0),
        write_observations: observations,
        quarantine_config: quarantine_config(),
    })
}

#[test]
fn store_integrity_report_surfaces_read_fence_and_per_source_write_immune() {
    let report = fixture_report();

    assert_eq!(report.status, StoreIntegrityStatus::Blocked);
    assert_eq!(report.read_fence.mode, "latest");
    assert_eq!(report.read_fence.verdict, "assets_behind");
    assert_eq!(report.read_fence.severity, "high");
    assert!(report.read_fence.strict_failed);
    assert_eq!(report.read_fence.workspace_generation, 12);
    assert_eq!(report.read_fence.stale_assets.len(), 1);
    assert_eq!(report.read_fence.stale_assets[0].name, "graph");
    assert_eq!(report.read_fence.stale_assets[0].generation, 11);
    assert_eq!(report.read_fence.stale_assets[0].lag, 1);

    assert!(report.write_immune.advisory_only);
    assert!(!report.write_immune.global_write_stall);
    assert_eq!(report.write_immune.quarantined_source_count, 1);

    let noisy = report
        .write_immune
        .decisions
        .iter()
        .find(|decision| decision.source_id == "noisy-agent")
        .expect("noisy source decision");
    assert_eq!(noisy.action, "quarantine");
    assert!(
        noisy
            .reasons
            .iter()
            .any(|reason| reason.code == "writes_per_window_exceeded")
    );
    assert!(
        noisy
            .reasons
            .iter()
            .any(|reason| reason.code == "high_trust_missing_evidence_ratio_exceeded")
    );

    let normal = report
        .write_immune
        .decisions
        .iter()
        .find(|decision| decision.source_id == "normal-agent")
        .expect("normal source decision");
    assert_eq!(normal.action, "allow");
}

#[test]
fn store_integrity_json_surface_is_enveloped_and_byte_stable() {
    let report = fixture_report();
    let first = render_store_integrity_json(&report);
    let second = render_store_integrity_json(&fixture_report());
    assert_eq!(first, second);

    let envelope: Value = serde_json::from_str(&first).expect("store integrity JSON envelope");
    assert_eq!(envelope["schema"], "ee.response.v2");
    assert_eq!(envelope["success"], true);
    assert_eq!(envelope["data"]["schema"], "ee.store_integrity.report.v1");
    assert_eq!(
        envelope["data"]["readFence"]["schema"],
        "ee.store_integrity.read_fence.v1"
    );
    assert_eq!(
        envelope["data"]["writeImmune"]["schema"],
        "ee.store_integrity.write_immune.v1"
    );
    assert_eq!(envelope["data"]["writeImmune"]["globalWriteStall"], false);
}
