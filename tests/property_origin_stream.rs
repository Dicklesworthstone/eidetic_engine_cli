//! bd-tc-epic-qzk7o.3.9 (T2.7, origin slice) — property/fuzz coverage for
//! the T2.0 origin stream shipped in bd-tc-epic-qzk7o.3.1.
//!
//! Two falsifiable families (canonicalization key-order invariance is pinned
//! by the in-module unit test + the independent KAT vectors, since
//! serde_json's BTree-backed maps make order unrepresentable at this layer):
//! 1. COMMITMENTS — shape invariants (prefix + 71 length), nonce and body
//!    sensitivity, and reproducibility with the exact same inputs.
//! 2. INGEST FUZZ — arbitrary garbage in every field of an inbound event
//!    NEVER panics the classifier and NEVER yields `Applied` unless the
//!    integrity chain actually verifies (which random garbage cannot);
//!    non-v1 outer schemas classify Unsupported whenever the disposition
//!    write itself is representable.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use ee::db::DbConnection;
use ee::mesh::origin_stream::{
    InboundOriginEvent, IngestDisposition, ORIGIN_EVENT_SCHEMA_V1, OriginSignatureVerifier,
    body_commitment, ingest_origin_event,
};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

struct RejectAllVerifier;

impl OriginSignatureVerifier for RejectAllVerifier {
    fn verify(&self, _: &str, _: u64, _: &str, _: &[u8], _: &str) -> bool {
        false
    }
}

/// Accepts everything: isolates the structural checks in fuzzing — even with
/// signatures waved through, garbage must still fail hash/id derivation.
struct AcceptAllVerifier;

impl OriginSignatureVerifier for AcceptAllVerifier {
    fn verify(&self, _: &str, _: u64, _: &str, _: &[u8], _: &str) -> bool {
        true
    }
}

fn open_db() -> DbConnection {
    let connection = DbConnection::open_memory().expect("open in-memory db");
    connection.migrate().expect("migrate");
    connection
}

fn arb_text() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[ -~]{0,48}").unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn commitment_shape_and_sensitivity(
        nonce_a in proptest::array::uniform32(any::<u8>()),
        nonce_b in proptest::array::uniform32(any::<u8>()),
        body_a in proptest::collection::vec(any::<u8>(), 0..256),
        body_b in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let commitment = body_commitment(&nonce_a, &body_a);
        prop_assert!(commitment.starts_with("blake3:"));
        prop_assert_eq!(commitment.len(), 71);
        // Reproducible with identical inputs (fetch-side verification).
        prop_assert_eq!(&commitment, &body_commitment(&nonce_a, &body_a));
        // Nonce sensitivity: distinct nonces unlink identical bodies.
        if nonce_a != nonce_b {
            prop_assert_ne!(&commitment, &body_commitment(&nonce_b, &body_a));
        }
        // Body sensitivity under one nonce.
        if body_a != body_b {
            prop_assert_ne!(&commitment, &body_commitment(&nonce_a, &body_b));
        }
    }

    #[test]
    fn ingest_never_panics_and_never_applies_garbage(
        schema in arb_text(),
        event_id in arb_text(),
        team_id in arb_text(),
        origin_node_id in arb_text(),
        generation in any::<u64>(),
        seq in 0_u64..1_000_000,
        prev in proptest::option::of(arb_text()),
        event_hash in arb_text(),
        signature in arb_text(),
        payload_schema in arb_text(),
        payload_key in arb_text(),
        payload_value in arb_text(),
        feature in arb_text(),
        produced_at in arb_text(),
    ) {
        let connection = open_db();
        let event = InboundOriginEvent {
            schema: schema.clone(),
            event_id,
            team_id: team_id.clone(),
            origin_node_id,
            signing_key_generation: generation,
            seq,
            prev_event_hash: prev,
            event_hash,
            signature,
            payload_schema,
            payload: serde_json::json!({ payload_key: payload_value }),
            required_features: vec![feature],
            produced_at,
        };
        let supported: BTreeSet<String> = BTreeSet::new();
        // Even with an accept-all signature verifier, random garbage must
        // fail the hash/id integrity chain before ever reaching Applied.
        let result = ingest_origin_event(
            &connection,
            &AcceptAllVerifier,
            "node_00000000000000000000000self",
            &supported,
            &event,
            "2026-08-11T00:00:00Z",
        );
        match result {
            Ok(disposition) => prop_assert!(
                !matches!(disposition, IngestDisposition::Applied),
                "garbage event must never be Applied: {disposition:?}"
            ),
            // Storage refusals on malformed ids are legal outcomes; the
            // property is only that nothing panics and nothing applies.
            Err(_) => {}
        }
        // The reject-all verifier can only make things stricter.
        let result = ingest_origin_event(
            &connection,
            &RejectAllVerifier,
            "node_00000000000000000000000self",
            &supported,
            &event,
            "2026-08-11T00:00:01Z",
        );
        if let Ok(disposition) = result {
            prop_assert!(!matches!(disposition, IngestDisposition::Applied));
        }
        // Outer-schema gate: anything that is not the v1 id is Unsupported
        // (checked first, so this holds regardless of the other garbage).
        if schema != ORIGIN_EVENT_SCHEMA_V1 {
            let connection = open_db();
            let outcome = ingest_origin_event(
                &connection,
                &AcceptAllVerifier,
                "node_00000000000000000000000self",
                &supported,
                &InboundOriginEvent { schema, team_id, ..event },
                "2026-08-11T00:00:02Z",
            );
            if let Ok(disposition) = outcome {
                prop_assert!(matches!(disposition, IngestDisposition::Unsupported { .. }));
            }
        }
    }
}
