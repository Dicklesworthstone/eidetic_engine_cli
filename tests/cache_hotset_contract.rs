//! bd-1zb7k.10.2: contract snapshot for `ee.cache.hotset.v1`.
//!
//! Builds an in-process [`HotsetManifest`] from seeded
//! `SearchHotsetEntry` and `PackHotsetEntry` records and pins its JSON
//! shape via insta. This is a structural contract: the snapshot guards
//! against schema drift in the public artifact emitted by the
//! redaction-safe hotset recorder, independent of the inline unit tests
//! that live alongside the module.
//!
//! The test does NOT spawn the `ee` binary and does NOT touch disk; it
//! exercises only the deterministic builder path so the snapshot stays
//! reproducible across hosts and toolchains.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::cache::hotset::{GenerationGate, HotsetBudget, HotsetManifestBuilder};
use ee::pack::{PackHotsetEntry, PackHotsetEntryKind};
use ee::search::SearchHotsetEntry;
use insta::assert_json_snapshot;

type TestResult = Result<(), String>;

#[test]
fn cache_hotset_manifest_json_shape_is_stable() -> TestResult {
    let pack_audit = PackHotsetEntry {
        key: "pack:audit:fixture".to_owned(),
        kind: PackHotsetEntryKind::SelectionAudit,
        section: None,
        generation: 5,
        estimated_bytes: 256,
        hit_count: 4,
        redaction_status: "content_not_stored",
    };

    let manifest =
        HotsetManifestBuilder::new("ws_01HQTSNAPSHOT00000000000", GenerationGate::new(5, 5))
            .with_profile_tier("balanced")
            .with_captured_at("2026-05-19T20:00:00Z")
            .with_budget(HotsetBudget::new(1024, 1_048_576).with_current(3, 768))
            .search_entries([
                SearchHotsetEntry::memory("mem_alpha_______________________", 5, 3),
                SearchHotsetEntry::memory("mem_beta________________________", 5, 1),
                SearchHotsetEntry::query_shape("ee context release", 5, 2)
                    .ok_or_else(|| "query shape should normalize".to_owned())?,
            ])
            .pack_entries([pack_audit])
            .build();

    assert_json_snapshot!("cache_hotset_v1_manifest", manifest.to_json());
    Ok(())
}

#[test]
fn cache_hotset_manifest_emits_degraded_when_stale_entries_rejected() -> TestResult {
    let manifest =
        HotsetManifestBuilder::new("ws_01HQTSNAPSHOT00000000000", GenerationGate::new(10, 10))
            .with_profile_tier("balanced")
            .with_captured_at("2026-05-19T20:00:00Z")
            .with_budget(HotsetBudget::new(1024, 1_048_576))
            .search_entries([
                SearchHotsetEntry::memory("mem_fresh_______________________", 10, 1),
                SearchHotsetEntry::memory("mem_stale_______________________", 4, 1),
            ])
            .build();

    assert_json_snapshot!(
        "cache_hotset_v1_manifest_stale_rejected",
        manifest.to_json()
    );
    Ok(())
}
