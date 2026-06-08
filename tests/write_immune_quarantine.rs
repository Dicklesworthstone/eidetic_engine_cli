//! Write-immune advisory-quarantine tests (bd-1n0np.8.8) over the landed pure
//! logic in `ee::core::write_owner`: `compute_source_write_stats` +
//! `evaluate_write_immune_quarantine`.
//!
//! `write_owner.rs` already unit-tests stats determinism, threshold trips, and
//! whitelist bypass. These cover the remaining 8.8 invariants:
//! - the GENEROUS-THRESHOLD FALSE-POSITIVE GUARD (a legit, evidenced source is
//!   never falsely held);
//! - NEVER A GLOBAL WRITE STALL: decisions are strictly per-source, so an
//!   abusive source's quarantine does not affect a clean source;
//! - high-trust-without-evidence abuse trips the dedicated reason.

use ee::core::write_owner::{
    WriteImmuneQuarantineConfig, WriteStreamObservation, WriteStreamStatsConfig,
    compute_source_write_stats, evaluate_write_immune_quarantine,
};

fn obs(
    source: &str,
    content: &str,
    trust_class: &str,
    evidence: bool,
    at_ms: u64,
) -> WriteStreamObservation {
    WriteStreamObservation::memory_create(
        source.to_string(),
        content,
        trust_class,
        if evidence {
            Some("file://AGENTS.md#L1")
        } else {
            None
        },
        at_ms,
    )
}

fn window() -> WriteStreamStatsConfig {
    WriteStreamStatsConfig::new(0, 1_000_000, 3)
}

#[test]
fn clean_source_under_defaults_is_not_falsely_quarantined() {
    // Distinct content + full evidence + low count -> every ratio is 0 and the
    // count is well under any generous default. Must NOT be held.
    let config = WriteImmuneQuarantineConfig::default();
    let observations = vec![
        obs(
            "legit",
            "alpha release checklist item one",
            "agent_assertion",
            true,
            10,
        ),
        obs(
            "legit",
            "beta rollout verification step two",
            "agent_assertion",
            true,
            20,
        ),
        obs(
            "legit",
            "gamma migration sequencing note three",
            "agent_assertion",
            true,
            30,
        ),
    ];
    let stats = compute_source_write_stats(&observations, window());
    let row = stats
        .iter()
        .find(|stats| stats.source_id == "legit")
        .expect("legit stats present");
    let decision = evaluate_write_immune_quarantine(row, &config);
    assert_eq!(
        decision.action, "allow",
        "a clean evidenced source must not be falsely quarantined; reasons={:?}",
        decision.reasons
    );
    assert!(decision.reasons.is_empty());
    assert!(!decision.whitelisted);
}

#[test]
fn per_source_decisions_are_isolated_never_a_global_stall() {
    // One abusive source (count over the generous write limit) alongside a clean
    // source. The abusive source is held; the clean source is unaffected — the
    // hold is per-source advisory, never a global write lock.
    let config = WriteImmuneQuarantineConfig::default();
    let mut observations = Vec::new();
    for i in 0..(config.max_writes_per_window + 5) {
        observations.push(obs(
            "spammer",
            "identical duplicated burst content",
            "agent_assertion",
            false,
            u64::from(i),
        ));
    }
    observations.push(obs(
        "legit",
        "distinct evidenced write one",
        "agent_assertion",
        true,
        5,
    ));
    observations.push(obs(
        "legit",
        "distinct evidenced write two",
        "agent_assertion",
        true,
        6,
    ));

    let stats = compute_source_write_stats(&observations, window());
    let spammer = stats
        .iter()
        .find(|stats| stats.source_id == "spammer")
        .expect("spammer stats present");
    let legit = stats
        .iter()
        .find(|stats| stats.source_id == "legit")
        .expect("legit stats present");

    assert_eq!(
        evaluate_write_immune_quarantine(spammer, &config).action,
        "quarantine",
        "the abusive burst source must trip advisory quarantine"
    );
    assert_eq!(
        evaluate_write_immune_quarantine(legit, &config).action,
        "allow",
        "a clean source must be unaffected by another source's quarantine (no global write stall)"
    );
}

#[test]
fn high_trust_writes_without_evidence_trip_the_dedicated_reason() {
    // Few distinct writes (under the write-count limit) claiming a high-trust
    // class with no evidence. With an explicit low high-trust threshold this must
    // trip the dedicated high-trust-missing-evidence reason.
    let config = WriteImmuneQuarantineConfig {
        max_high_trust_missing_evidence_ratio: 0.5,
        max_missing_evidence_ratio: 0.5,
        ..WriteImmuneQuarantineConfig::default()
    };
    let observations = vec![
        obs(
            "forger",
            "asserted fact one distinct",
            "human_explicit",
            false,
            10,
        ),
        obs(
            "forger",
            "asserted fact two distinct",
            "human_explicit",
            false,
            20,
        ),
        obs(
            "forger",
            "asserted fact three distinct",
            "human_explicit",
            false,
            30,
        ),
    ];
    let stats = compute_source_write_stats(&observations, window());
    let row = stats
        .iter()
        .find(|stats| stats.source_id == "forger")
        .expect("forger stats present");
    let decision = evaluate_write_immune_quarantine(row, &config);
    assert_eq!(decision.action, "quarantine");
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| reason.code == "high_trust_missing_evidence_ratio_exceeded"),
        "high-trust writes without evidence must trip the dedicated reason; reasons={:?}",
        decision.reasons
    );
}
