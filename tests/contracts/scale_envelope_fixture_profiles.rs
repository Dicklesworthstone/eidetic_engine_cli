//! bd-ssoco.2: deterministic scale-envelope fixture profiles.
//!
//! This pins the generator that later scale-envelope probes and RCH-only SLO
//! harnesses consume. Large corpora are described by manifests and generated on
//! demand; only a tiny canonical small-profile sample is committed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use ee::core::lab::{
    SCALE_ENVELOPE_FIXTURE_MANIFEST_SCHEMA_V1, SCALE_ENVELOPE_FIXTURE_RECORD_SCHEMA_V1,
    ScaleEnvelopeFixtureManifest, ScaleEnvelopeFixtureOptions, ScaleEnvelopeFixtureProfile,
    ScaleEnvelopeFixtureRecord, generate_scale_envelope_fixture_manifest,
    generate_scale_envelope_fixture_records,
};
use serde::Serialize;
use serde_json::Value;

type TestResult = Result<(), String>;

const SMALL_MANIFEST_PATH: &str = "tests/fixtures/scale_envelope/small_manifest.json";
const SMALL_RECORDS_PATH: &str = "tests/fixtures/scale_envelope/small_records_sample.json";
const SMALL_SEED: &str = "scale_small_seed_001";
const FULL_SEED: &str = "scale_full_seed_001";

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_path(relative);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn pretty<T: Serialize>(value: &T) -> Result<String, String> {
    let mut rendered = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    rendered.push('\n');
    Ok(rendered)
}

fn assert_hash(value: &str, context: &str) -> TestResult {
    ensure(
        value.starts_with("blake3:") && value.len() >= "blake3:".len() + 32,
        format!("{context} must be a redaction-safe blake3 hash, got {value}"),
    )
}

#[test]
fn small_profile_generates_stable_ci_safe_records() -> TestResult {
    let options = ScaleEnvelopeFixtureOptions::small(SMALL_SEED);
    let first_manifest = generate_scale_envelope_fixture_manifest(&options);
    let second_manifest = generate_scale_envelope_fixture_manifest(&options);
    ensure(
        first_manifest == second_manifest,
        "same profile and seed must generate equal manifests",
    )?;
    ensure(
        pretty(&first_manifest)? == pretty(&second_manifest)?,
        "same profile and seed must generate byte-identical pretty JSON",
    )?;
    ensure(
        first_manifest.schema == SCALE_ENVELOPE_FIXTURE_MANIFEST_SCHEMA_V1,
        "manifest schema mismatch",
    )?;
    ensure(
        first_manifest
            .fixture_profile_id
            .starts_with("scale_small_"),
        "small profile id should be namespaced for scale-envelope fixtures",
    )?;
    ensure(
        first_manifest.corpus_shape.memory_count == 512
            && first_manifest.corpus_shape.link_count == 1_536
            && first_manifest.corpus_shape.pack_count == 32
            && first_manifest.corpus_shape.search_document_count == 544,
        "small corpus shape must stay cheap enough for normal contract tests",
    )?;
    ensure(
        first_manifest.output_policy.materialized_in_ci
            && !first_manifest
                .output_policy
                .rch_required_for_full_materialization,
        "small profile should be CI-safe and not require RCH materialization",
    )?;
    let total_weight: u32 = first_manifest
        .query_distribution
        .iter()
        .map(|bucket| bucket.weight_per_million)
        .sum();
    ensure(
        total_weight == 1_000_000,
        format!("Zipfian query weights must sum to one million, got {total_weight}"),
    )?;
    ensure(
        first_manifest
            .query_distribution
            .windows(2)
            .all(|pair| pair[0].weight_per_million >= pair[1].weight_per_million),
        "Zipfian query weights must be non-increasing by rank",
    )?;
    assert_hash(&first_manifest.hash_summary.shape_hash, "shape_hash")?;
    assert_hash(
        &first_manifest.hash_summary.query_distribution_hash,
        "query_distribution_hash",
    )?;
    assert_hash(
        &first_manifest.hash_summary.sample_records_hash,
        "sample_records_hash",
    )?;
    assert_hash(&first_manifest.hash_summary.manifest_hash, "manifest_hash")?;

    let records = generate_scale_envelope_fixture_records(&options, 20);
    ensure(
        records.len() == 20,
        "small record sample limit should be honored",
    )?;
    ensure(
        records[0].schema == SCALE_ENVELOPE_FIXTURE_RECORD_SCHEMA_V1
            && records[0].memory_id == "mem_scale_small_000001"
            && records[0].search_document_id == "doc_scale_small_000001"
            && records[0].query_rank == 1
            && records[0].query_key == "release_verification"
            && records[0].topic == "release",
        "first generated record should pin deterministic IDs and query rank",
    )?;
    ensure(
        records[0].link_targets == vec!["mem_scale_small_000018".to_owned()],
        "first record should link to the deterministic forward neighbor",
    )?;
    ensure(
        records[0].contradiction_cluster_id.as_deref() == Some("scale_contradiction_000001")
            && records[1].contradiction_cluster_id == records[0].contradiction_cluster_id,
        "first two records should seed the first contradiction cluster",
    )?;
    ensure(
        records[16].duplicate_of.as_deref() == Some("mem_scale_small_000016"),
        "duplicate stride should mark the seventeenth small record as a controlled duplicate",
    )?;
    ensure(
        !pretty(&records)?.contains("Verification status:")
            && !pretty(&records)?.contains("memory from"),
        "fixture records must not commit raw generated memory bodies",
    )
}

#[test]
fn medium_and_full_profiles_describe_heavy_corpora_without_committing_them() -> TestResult {
    let medium = generate_scale_envelope_fixture_manifest(&ScaleEnvelopeFixtureOptions::medium(
        "scale_medium_seed_001",
    ));
    let full =
        generate_scale_envelope_fixture_manifest(&ScaleEnvelopeFixtureOptions::full(FULL_SEED));

    ensure(
        !medium.output_policy.materialized_in_ci
            && medium.output_policy.rch_required_for_full_materialization,
        "medium profile should be manifest-described and RCH-oriented",
    )?;
    ensure(
        full.profile == ScaleEnvelopeFixtureProfile::Full
            && full.envelope_profile_name == "large"
            && full.corpus_shape.memory_count == 1_000_000
            && full.corpus_shape.search_document_count == 1_025_000
            && full.corpus_shape.expected_last_memory_id == "mem_scale_full_1000000",
        "full profile must target million-memory scale through the scale envelope large profile",
    )?;
    ensure(
        !full.output_policy.materialized_in_ci
            && full.output_policy.rch_required_for_full_materialization
            && full
                .output_policy
                .generated_records_path_tail
                .contains(".ee/lab/scale-envelope/"),
        "full generated records should stay outside tracked source and require RCH materialization",
    )?;
    ensure(
        full.pipeline_alignment.components
            == vec![
                "frankensqlite_store".to_owned(),
                "frankensearch_document".to_owned(),
                "context_pack".to_owned(),
                "graph_projection".to_owned(),
            ],
        "fixture design should align with store, search, pack, and graph projections",
    )?;
    ensure(
        full.hash_summary.manifest_hash != medium.hash_summary.manifest_hash,
        "profile changes must alter the manifest hash",
    )?;
    let full_sample =
        generate_scale_envelope_fixture_records(&ScaleEnvelopeFixtureOptions::full(FULL_SEED), 3);
    ensure(
        full_sample.len() == 3
            && full_sample[0].memory_id == "mem_scale_full_0000001"
            && full_sample[2].memory_id == "mem_scale_full_0000003",
        "full profile should generate bounded samples without materializing the full corpus",
    )
}

#[test]
fn committed_canonical_fixtures_pin_only_the_small_profile_sample() -> TestResult {
    let fixture_manifest: ScaleEnvelopeFixtureManifest =
        serde_json::from_value(read_json(SMALL_MANIFEST_PATH)?)
            .map_err(|error| format!("small manifest fixture shape drifted: {error}"))?;
    let generated_manifest =
        generate_scale_envelope_fixture_manifest(&ScaleEnvelopeFixtureOptions::small(SMALL_SEED));
    ensure(
        fixture_manifest.schema == generated_manifest.schema
            && fixture_manifest.fixture_profile_id == generated_manifest.fixture_profile_id
            && fixture_manifest.fixture_seed == generated_manifest.fixture_seed
            && fixture_manifest.profile == generated_manifest.profile
            && fixture_manifest.corpus_shape == generated_manifest.corpus_shape
            && fixture_manifest.output_policy == generated_manifest.output_policy,
        "committed small manifest should match generated small-profile shape and policy",
    )?;
    assert_hash(
        &fixture_manifest.hash_summary.manifest_hash,
        "fixture manifest_hash",
    )?;

    let fixture_records: Vec<ScaleEnvelopeFixtureRecord> =
        serde_json::from_value(read_json(SMALL_RECORDS_PATH)?)
            .map_err(|error| format!("small records fixture shape drifted: {error}"))?;
    let generated_records =
        generate_scale_envelope_fixture_records(&ScaleEnvelopeFixtureOptions::small(SMALL_SEED), 2);
    ensure(
        fixture_records == generated_records,
        "committed record sample should equal the first two generated small records",
    )?;
    ensure(
        !repo_path("tests/fixtures/scale_envelope/full_records.jsonl").exists()
            && !repo_path("tests/fixtures/scale_envelope/medium_records.jsonl").exists(),
        "medium/full generated corpora must not be committed",
    )
}
