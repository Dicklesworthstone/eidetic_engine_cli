#![allow(clippy::expect_used)]

use chrono::{DateTime, Duration, Utc};
use ee::db::StoredMemory;
use ee::policy::{MemoryDecayAction, MemoryDecayThresholds, evaluate_memory_decay};

fn memory_fixture(confidence: f32, utility: f32) -> StoredMemory {
    StoredMemory {
        id: "mem_decaythreshold0000000001".to_owned(),
        workspace_id: "wsp_decaythreshold0000000001".to_owned(),
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        content: "decay threshold fixture".to_owned(),
        workflow_id: None,
        confidence,
        utility,
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
fn lifecycle_score_classifies_preserve_demote_and_tombstone() {
    let as_of: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().expect("as_of should parse");
    let thresholds = MemoryDecayThresholds {
        demote: 0.05,
        forget: 0.01,
    };

    let cases = [
        (memory_fixture(0.9, 0.9), as_of, MemoryDecayAction::Preserve),
        (
            memory_fixture(0.3, 0.2),
            as_of - Duration::days(400),
            MemoryDecayAction::Demote,
        ),
        (
            memory_fixture(0.2, 0.2),
            as_of - Duration::days(1500),
            MemoryDecayAction::Tombstone,
        ),
        (
            memory_fixture(f32::NAN, f32::INFINITY),
            as_of,
            MemoryDecayAction::Tombstone,
        ),
    ];

    for (memory, reference, expected) in cases {
        let actual = evaluate_memory_decay(&memory, reference, as_of, thresholds);
        assert_eq!(actual.action, expected);
    }
}
