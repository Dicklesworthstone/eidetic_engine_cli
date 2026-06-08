//! bd-1n0np.22.5 — Provenance Attestation Bundle contract tests.
//!
//! Pins the public contracts of the 22.1 `AttestationBundle` from a consumer's
//! view, exercised via the pure `build_query_attestation` builder (no DB):
//! deterministic + subject-sensitive bundle hash, redaction applied (no raw
//! query text in the bundle), and manifest completeness. These are the
//! cross-surface invariants 22.3 relies on when it refactors support-bundle /
//! handoff / pack-replay / why to consume the bundle.
//!
//! Goldens for `ee attest memory|pack` and the "consuming surfaces produce
//! identical bundle hash" cross-check (which needs the 22.3 refactor) are owed:
//! the goldens RCH-remote-regen (bd-17c65.10.17), the cross-surface check once
//! 22.3 lands.

use ee::core::attest::build_query_attestation;
use ee::models::attestation::ATTESTATION_BUNDLE_SCHEMA_V1;

#[test]
fn bundle_hash_is_deterministic_and_subject_sensitive() {
    let first = build_query_attestation("how do I configure network retries");
    let again = build_query_attestation("how do I configure network retries");
    assert_eq!(
        first.bundle_hash(),
        again.bundle_hash(),
        "the same subject must always produce the same bundle hash"
    );
    assert_eq!(first.schema, ATTESTATION_BUNDLE_SCHEMA_V1);
    assert!(
        !first.bundle_hash().is_empty(),
        "bundle hash must be present"
    );

    let other = build_query_attestation("a completely unrelated query about disks");
    assert_ne!(
        first.bundle_hash(),
        other.bundle_hash(),
        "a different subject must yield a different bundle hash"
    );
}

#[test]
fn bundle_redacts_raw_subject_text() {
    let secret = "SUPERSECRET_QUERY_TOKEN_DO_NOT_LEAK_9z9z";
    let bundle = build_query_attestation(secret);
    let json = serde_json::to_string(&bundle).expect("serialize attestation bundle");
    assert!(
        !json.contains(secret),
        "raw subject text must be redacted (hashed), never stored verbatim in the bundle"
    );
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid bundle json");
    assert!(
        value.get("redactionManifest").is_some(),
        "the bundle must carry a redaction manifest recording the policy"
    );
}

#[test]
fn manifest_is_complete_and_schema_tagged() {
    let bundle = build_query_attestation("query for manifest completeness");
    let value = serde_json::to_value(&bundle).expect("attestation bundle to_value");
    assert_eq!(value["schema"], ATTESTATION_BUNDLE_SCHEMA_V1);
    for key in [
        "subject",
        "evidenceManifest",
        "redactionManifest",
        "hashManifest",
    ] {
        assert!(
            value.get(key).is_some(),
            "a complete attestation bundle must include `{key}`"
        );
    }
}
