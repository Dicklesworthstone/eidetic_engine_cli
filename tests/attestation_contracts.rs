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

use std::collections::BTreeSet;

use ee::core::attest::{
    ATTESTATION_SURFACE_MANIFEST_SCHEMA_V1, attestation_surface_manifest, build_query_attestation,
    public_attestation_bundle,
};
use ee::models::attestation::{ATTESTATION_BUNDLE_SCHEMA_V2, AttestationSeal};

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

#[test]
fn attest_schema_pins_optional_v2_seal_block() {
    let schema: serde_json::Value =
        serde_json::from_str(ATTEST_RESPONSE_SCHEMA).expect("ee.attest schema JSON");
    let bundle_schema = schema
        .pointer("/properties/data/properties/bundle")
        .expect("attestation bundle schema");
    assert_eq!(
        bundle_schema["additionalProperties"],
        serde_json::json!(true),
        "the v2 seal contract must not tighten unrelated bundle extensions"
    );
    assert!(
        !bundle_schema["required"]
            .as_array()
            .expect("bundle required fields")
            .contains(&serde_json::json!("seal")),
        "seal must remain optional for unsealed attestation bundles"
    );

    let seal_schema = bundle_schema
        .pointer("/properties/seal")
        .expect("v2 seal schema");
    assert_eq!(seal_schema["type"], "object");
    assert_eq!(seal_schema["additionalProperties"], false);
    assert_eq!(
        seal_schema["required"],
        serde_json::json!([
            "contentCommitment",
            "sealedAt",
            "revealedAt",
            "revealVerified"
        ])
    );
    assert_eq!(
        object_keys(&seal_schema["properties"]),
        BTreeSet::from([
            "contentCommitment",
            "revealVerified",
            "revealedAt",
            "sealedAt",
        ])
    );
    assert_eq!(
        seal_schema["properties"]["contentCommitment"],
        serde_json::json!({
            "type": "string",
            "pattern": "^blake3:[0-9a-f]{64}$",
            "description": "Domain-separated blake3 commitment over the exact sealed content bytes; never the raw content."
        })
    );
    assert_eq!(seal_schema["properties"]["sealedAt"]["type"], "string");
    assert_eq!(seal_schema["properties"]["sealedAt"]["format"], "date-time");
    assert_eq!(
        seal_schema["properties"]["revealedAt"]["type"],
        serde_json::json!(["string", "null"])
    );
    assert_eq!(
        seal_schema["properties"]["revealedAt"]["format"],
        "date-time"
    );
    assert_eq!(
        seal_schema["properties"]["revealVerified"]["type"],
        serde_json::json!(["boolean", "null"])
    );
}

#[test]
fn v2_seal_instances_are_strict_and_redaction_safe() {
    let schema: serde_json::Value =
        serde_json::from_str(ATTEST_RESPONSE_SCHEMA).expect("ee.attest schema JSON");
    let seal_schema = schema
        .pointer("/properties/data/properties/bundle/properties/seal")
        .expect("v2 seal schema");

    let raw_content = "SEALED_RAW_CONTENT_MUST_NOT_LEAK_4f2d";
    let raw_path = "/Users/private/project/secret-plan.txt";
    let subject = format!("{raw_content} from {raw_path}");
    let unsealed = serde_json::to_value(build_query_attestation(&subject))
        .expect("serialize unsealed attestation");
    validate_optional_seal_instance(&unsealed, seal_schema)
        .expect("unsealed v2 bundle must validate without a seal key");
    assert!(unsealed.get("seal").is_none());

    let sealed =
        serde_json::to_value(
            build_query_attestation(&subject).with_seal(Some(AttestationSeal {
                content_commitment:
                    "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .to_owned(),
                sealed_at: "2026-08-10T00:00:00Z".to_owned(),
                revealed_at: Some("2026-08-11T12:34:56Z".to_owned()),
                reveal_verified: Some(true),
            })),
        )
        .expect("serialize sealed attestation");
    validate_optional_seal_instance(&sealed, seal_schema)
        .expect("sealed v2 bundle must satisfy the strict seal contract");
    assert_eq!(
        sealed["seal"],
        serde_json::json!({
            "contentCommitment": "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sealedAt": "2026-08-10T00:00:00Z",
            "revealedAt": "2026-08-11T12:34:56Z",
            "revealVerified": true
        })
    );

    let rendered = serde_json::to_string(&sealed).expect("render sealed attestation");
    assert!(!rendered.contains(raw_content), "raw content leaked");
    assert!(!rendered.contains(raw_path), "filesystem path leaked");

    for forbidden_key in ["content", "path"] {
        let mut invalid = sealed.clone();
        invalid["seal"][forbidden_key] = serde_json::json!("must not be accepted");
        assert!(
            validate_optional_seal_instance(&invalid, seal_schema).is_err(),
            "strict seal validation must reject `{forbidden_key}`"
        );
    }
}

fn object_keys(value: &serde_json::Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("schema properties object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn validate_optional_seal_instance(
    bundle: &serde_json::Value,
    seal_schema: &serde_json::Value,
) -> Result<(), String> {
    let Some(seal) = bundle.get("seal") else {
        return Ok(());
    };
    let seal = seal
        .as_object()
        .ok_or_else(|| "seal must be an object".to_owned())?;
    let properties = seal_schema["properties"]
        .as_object()
        .ok_or_else(|| "seal schema properties must be an object".to_owned())?;
    let required = seal_schema["required"]
        .as_array()
        .ok_or_else(|| "seal schema required must be an array".to_owned())?;

    for field in required {
        let field = field
            .as_str()
            .ok_or_else(|| "seal required field must be a string".to_owned())?;
        if !seal.contains_key(field) {
            return Err(format!("seal is missing required field `{field}`"));
        }
    }
    if seal_schema["additionalProperties"] == serde_json::json!(false) {
        for field in seal.keys() {
            if !properties.contains_key(field) {
                return Err(format!("seal contains forbidden field `{field}`"));
            }
        }
    }

    let commitment = seal["contentCommitment"]
        .as_str()
        .ok_or_else(|| "contentCommitment must be a string".to_owned())?;
    if !is_canonical_blake3(commitment) {
        return Err("contentCommitment must be a canonical blake3 hash".to_owned());
    }
    validate_rfc3339(&seal["sealedAt"], false, "sealedAt")?;
    validate_rfc3339(&seal["revealedAt"], true, "revealedAt")?;
    if !seal["revealVerified"].is_null() && !seal["revealVerified"].is_boolean() {
        return Err("revealVerified must be a boolean or null".to_owned());
    }
    Ok(())
}

fn is_canonical_blake3(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn validate_rfc3339(value: &serde_json::Value, nullable: bool, field: &str) -> Result<(), String> {
    if nullable && value.is_null() {
        return Ok(());
    }
    let timestamp = value
        .as_str()
        .ok_or_else(|| format!("{field} must be an RFC 3339 string or allowed null"))?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|_| ())
        .map_err(|error| format!("{field} must be RFC 3339: {error}"))
}
