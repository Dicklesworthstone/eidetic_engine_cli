#![allow(clippy::expect_used)]

use chrono::{DateTime, Duration, Utc};
use ee::db::StoredMemory;
use ee::policy::{MemoryDecayAction, MemoryDecayThresholds, evaluate_memory_decay};

fn memory_fixture(level: &str, kind: &str) -> StoredMemory {
    StoredMemory {
        id: format!("mem_decaylevel{level}{kind}")
            .chars()
            .take(30)
            .collect(),
        workspace_id: "wsp_decaylevel000000000000001".to_owned(),
        level: level.to_owned(),
        kind: kind.to_owned(),
        content: "decay level fixture".to_owned(),
        workflow_id: None,
        confidence: 0.4,
        utility: 0.4,
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
fn demotion_ladder_is_reversible_and_non_destructive() {
    let as_of: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().expect("as_of should parse");
    let thresholds = MemoryDecayThresholds {
        demote: 0.05,
        forget: 0.0,
    };

    let procedural = evaluate_memory_decay(
        &memory_fixture("procedural", "rule"),
        as_of - Duration::days(800),
        as_of,
        thresholds,
    );
    assert_eq!(procedural.action, MemoryDecayAction::Demote);
    assert_eq!(procedural.previous_level, "procedural");
    assert_eq!(procedural.new_level, "semantic");
    assert_eq!(procedural.new_importance, 0.4);

    let semantic = evaluate_memory_decay(
        &memory_fixture("semantic", "fact"),
        as_of - Duration::days(400),
        as_of,
        thresholds,
    );
    assert_eq!(semantic.action, MemoryDecayAction::Demote);
    assert_eq!(semantic.previous_level, "semantic");
    assert_eq!(semantic.new_level, "episodic");

    let episodic = evaluate_memory_decay(
        &memory_fixture("episodic", "event"),
        as_of - Duration::days(90),
        as_of,
        thresholds,
    );
    assert_eq!(episodic.action, MemoryDecayAction::Demote);
    assert_eq!(episodic.previous_level, "episodic");
    assert_eq!(episodic.new_level, "episodic");
}
