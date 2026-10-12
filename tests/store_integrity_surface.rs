#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use ee::core::read_fence::{READ_FENCE_GRAPH_REPAIR, ReadFence};
use ee::core::store_integrity::{
    GenerationSource, StoreIntegrityOptions, StoreIntegrityReport, StoreIntegrityStatus,
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
        // These generations are typed into the fixture, not read from a
        // store, and the report must say so (bd-1n0np.8.13).
        generation_source: GenerationSource::Supplied,
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
    assert_eq!(
        report.read_fence.repair.as_deref(),
        Some(READ_FENCE_GRAPH_REPAIR)
    );

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
fn latest_non_strict_stale_assets_degrade_store_integrity_status() {
    let report = run_store_integrity_report(StoreIntegrityOptions {
        read_fence: ReadFence::Latest,
        db_generation: 12,
        asset_generations: vec![("search".to_owned(), 12), ("graph".to_owned(), 11)],
        strict_read_fence: false,
        generation_source: GenerationSource::Supplied,
        write_stream_config: WriteStreamStatsConfig::new(0, 1_000, 0),
        write_observations: Vec::new(),
        quarantine_config: quarantine_config(),
    });

    assert_eq!(report.status, StoreIntegrityStatus::Degraded);
    assert_eq!(report.read_fence.severity, "high");
    assert!(!report.read_fence.strict_failed);
    assert_eq!(report.read_fence.stale_assets.len(), 1);
    assert_eq!(report.read_fence.stale_assets[0].name, "graph");
    assert_eq!(
        report.read_fence.repair.as_deref(),
        Some(READ_FENCE_GRAPH_REPAIR)
    );
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

    // bd-1n0np.8.13: the verdict must travel with its provenance. A reader
    // that cannot tell an observed generation from a typed one cannot tell
    // this report's `assets_behind` from a stale graph in a live workspace,
    // and the fixture's generations were typed.
    assert_eq!(
        envelope["data"]["readFence"]["generationSource"],
        "supplied"
    );
}

/// bd-1n0np.8.13: a store that could not be observed must not report health.
///
/// With no observable asset generations the fence finds nothing behind and
/// would return `Ok` on its own. That is the one false negative this surface
/// cannot afford, so an unobservable store degrades, and under
/// `--strict-read-fence` it fails closed.
#[test]
fn unobservable_generations_never_report_a_clean_store() {
    let report = |strict| {
        run_store_integrity_report(StoreIntegrityOptions {
            read_fence: ReadFence::Latest,
            db_generation: 0,
            asset_generations: Vec::new(),
            strict_read_fence: strict,
            generation_source: GenerationSource::Unavailable,
            write_stream_config: WriteStreamStatsConfig::new(0, 1_000, 0),
            write_observations: Vec::new(),
            quarantine_config: quarantine_config(),
        })
    };

    assert_eq!(
        report(false).status,
        StoreIntegrityStatus::Degraded,
        "an unobservable store is not a clean store"
    );
    assert_eq!(
        report(true).status,
        StoreIntegrityStatus::Blocked,
        "--strict-read-fence asked to fail closed, and 'I could not look' is a closed fail"
    );

    // The cause must travel beside the label, or an operator cannot tell this
    // Blocked from a genuinely stale asset.
    assert_eq!(
        report(true).read_fence.generation_source,
        GenerationSource::Unavailable
    );
}

/// NEGATIVE CONTROL for the rule above: the degrade is keyed on PROVENANCE,
/// not on an empty asset list.
///
/// A caller who supplies no assets on purpose -- the pure-function mode, and
/// every pre-existing caller of this API -- is making no claim about derived
/// assets and must still be able to reach `Ok`. If this test ever fails
/// alongside the one above, the predicate has widened from "could not
/// observe" to "has no assets", which would fire on every library caller.
#[test]
fn deliberately_supplied_empty_assets_still_reach_ok() {
    let report = run_store_integrity_report(StoreIntegrityOptions {
        read_fence: ReadFence::Latest,
        db_generation: 7,
        asset_generations: Vec::new(),
        strict_read_fence: true,
        generation_source: GenerationSource::Supplied,
        write_stream_config: WriteStreamStatsConfig::new(0, 1_000, 0),
        write_observations: Vec::new(),
        quarantine_config: quarantine_config(),
    });

    assert_eq!(report.status, StoreIntegrityStatus::Ok);
    assert!(!report.read_fence.strict_failed);
    assert_eq!(
        report.read_fence.generation_source,
        GenerationSource::Supplied
    );
}

/// bd-1n0np.8.13: `GenerationSource` must serialize to the snake_case tokens
/// the CLI contract names, and the four states must stay distinguishable.
///
/// `Unavailable` exists precisely so that "the store had nothing to offer" is
/// not reported as `Supplied`; collapsing them would make a missing store
/// read as a caller's deliberate choice.
#[test]
fn generation_source_variants_are_distinct_snake_case_tokens() {
    let tokens = [
        GenerationSource::Observed,
        GenerationSource::Supplied,
        GenerationSource::Mixed,
        GenerationSource::Unavailable,
    ]
    .iter()
    .map(|source| serde_json::to_string(source).expect("generation source serializes"))
    .collect::<Vec<_>>();

    assert_eq!(
        tokens,
        vec![
            "\"observed\"".to_owned(),
            "\"supplied\"".to_owned(),
            "\"mixed\"".to_owned(),
            "\"unavailable\"".to_owned(),
        ]
    );
    assert_eq!(
        tokens.iter().collect::<BTreeSet<_>>().len(),
        4,
        "four states must not collapse onto one token"
    );
    assert_eq!(
        GenerationSource::default(),
        GenerationSource::Supplied,
        "the pure-function entry point opens no store, so Supplied is the \
         only safe default"
    );
}
