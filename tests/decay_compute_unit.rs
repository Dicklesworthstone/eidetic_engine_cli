#![allow(clippy::expect_used)]

use ee::db::StoredMemory;
use ee::policy::{
    MemoryDecayHalfLives, MemoryDecaySettings, MemoryDecayThresholds,
    evaluate_memory_decay_with_settings, memory_decay_freshness_score,
};

fn memory_fixture(level: &str, kind: &str) -> StoredMemory {
    StoredMemory {
        id: "mem_decaycompute000000000001".to_owned(),
        workspace_id: "wsp_decaycompute000000000001".to_owned(),
        level: level.to_owned(),
        kind: kind.to_owned(),
        content: "decay compute fixture".to_owned(),
        workflow_id: None,
        confidence: 0.9,
        utility: 0.9,
        importance: 0.8,
        provenance_uri: None,
        trust_class: "human_explicit".to_owned(),
        trust_subclass: None,
        provenance_chain_hash: None,
        provenance_chain_hash_version: "ee.memory.provenance_chain.v1".to_owned(),
        provenance_verification_status: "verified".to_owned(),
        provenance_verified_at: None,
        provenance_verification_note: None,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        tombstoned_at: None,
        valid_from: None,
        valid_to: None,
    }
}

#[test]
fn freshness_function_pins_half_life_boundaries() {
    assert_eq!(memory_decay_freshness_score(0.0, 30.0), 1.0);
    assert_eq!(memory_decay_freshness_score(30.0, 30.0), 0.5);
    assert_eq!(memory_decay_freshness_score(60.0, 30.0), 0.25);
    assert_eq!(memory_decay_freshness_score(f64::INFINITY, 30.0), 0.0);
}

#[test]
fn configured_half_life_is_deterministic() {
    let as_of = "2026-01-11T00:00:00Z".parse().expect("as_of should parse");
    let reference = "2026-01-01T00:00:00Z"
        .parse()
        .expect("reference should parse");
    let settings = MemoryDecaySettings {
        thresholds: MemoryDecayThresholds {
            demote: 0.3,
            forget: 0.001,
        },
        half_lives: MemoryDecayHalfLives {
            procedural_rule: 1.0,
            ..MemoryDecayHalfLives::default()
        },
    };

    let first = evaluate_memory_decay_with_settings(
        &memory_fixture("procedural", "rule"),
        reference,
        as_of,
        settings,
    );
    let second = evaluate_memory_decay_with_settings(
        &memory_fixture("procedural", "rule"),
        reference,
        as_of,
        settings,
    );

    assert_eq!(first, second);
    assert_eq!(first.half_life_days, 1.0);
}
