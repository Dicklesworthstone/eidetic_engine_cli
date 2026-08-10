//! bd-1n0np.22.5 — Provenance Attestation Bundle contract tests.
//!
//! Pins the public contracts of the 22.1 `AttestationBundle` from a consumer's
//! view, exercised via the pure `build_query_attestation` builder (no DB):
//! deterministic + subject-sensitive bundle hash, redaction applied (no raw
//! query text in the bundle), and manifest completeness. These are the
//! cross-surface invariants 22.3 relies on when it refactors support-bundle /
//! handoff / pack-replay / why to consume the bundle.
//!
//! Cross-surface bundle-hash parity is exercised by `scripts/e2e_attestation.sh`
//! across direct pack attestation, support bundles, and handoff output; pack
//! replay goldens pin the corresponding public attestation manifest.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use ee::core::attest::{
    ATTESTATION_SURFACE_MANIFEST_SCHEMA_V1, attestation_surface_manifest, build_query_attestation,
    public_attestation_bundle,
};
use ee::models::attestation::ATTESTATION_BUNDLE_SCHEMA_V2;

const ATTEST_RESPONSE_SCHEMA: &str = include_str!("../docs/schemas/ee.attest.v1.json");

#[test]
fn bundle_hash_is_deterministic_and_subject_sensitive() {
    let first = build_query_attestation("how do I configure network retries");
    let again = build_query_attestation("how do I configure network retries");
    assert_eq!(
        first.bundle_hash(),
        again.bundle_hash(),
        "the same subject must always produce the same bundle hash"
    );
    assert_eq!(first.schema, ATTESTATION_BUNDLE_SCHEMA_V2);
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
    assert_eq!(value["schema"], ATTESTATION_BUNDLE_SCHEMA_V2);
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

#[test]
fn surface_manifest_is_hash_only_and_reuses_bundle_hash() {
    let secret = "SURFACE_MANIFEST_SECRET_QUERY_DO_NOT_LEAK";
    let bundle = build_query_attestation(secret);
    let manifest = attestation_surface_manifest(&bundle);

    assert_eq!(manifest["schema"], ATTESTATION_SURFACE_MANIFEST_SCHEMA_V1);
    assert_eq!(manifest["status"], "available");
    assert_eq!(manifest["sourceSchema"], ATTESTATION_BUNDLE_SCHEMA_V2);
    assert_eq!(
        manifest["bundleHash"],
        public_attestation_bundle(&bundle).bundle_hash()
    );
    assert_eq!(manifest["subject"]["kind"], "query");
    assert!(
        manifest
            .pointer("/evidenceManifest/entryCount")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| count >= 1),
        "surface manifest must preserve evidence counts for consumers"
    );

    let rendered = manifest.to_string();
    assert!(
        !rendered.contains(secret),
        "surface manifest must not rehydrate raw subject text"
    );
}

#[test]
fn attest_schema_matches_public_bundle_trust_shape_and_identity_posture() {
    let schema: serde_json::Value =
        serde_json::from_str(ATTEST_RESPONSE_SCHEMA).expect("ee.attest schema JSON");
    assert_eq!(
        schema.pointer("/properties/data/properties/bundle/properties/trustStatement/type"),
        Some(&serde_json::json!("object"))
    );
    let subject_description = schema
        .pointer("/properties/data/properties/subjectId/description")
        .and_then(serde_json::Value::as_str)
        .expect("subjectId description");
    assert!(subject_description.contains("Raw query text is never emitted"));
}
