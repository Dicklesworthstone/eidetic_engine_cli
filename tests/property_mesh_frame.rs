//! bd-tc-epic-qzk7o.3.9 (T2.7, frame/session/bootstrap slice).
//!
//! Origin-stream properties stay in `property_origin_stream.rs`. This file
//! covers the remainder: frame-v2 decode, exact-next counters, MAC
//! authentication-before-counter, v1 reject, and bootstrap envelope triage.
//!
//! Families:
//! 1. DECODE FUZZ — arbitrary bytes never panic `decode_frame` /
//!    `decode_envelope`. `Ok` requires a well-formed schema the parsers own.
//! 2. COUNTERS — exact-next accepts; duplicate / skip / regress close.
//! 3. AUTH — a signed frame verifies; a flipped MAC fails without consuming
//!    the exact-next counter; a wrong target fails closed as binding mismatch.
//! 4. BOOTSTRAP — hello/join envelopes round-trip; oversize and unknown
//!    capabilities refuse; loopback hello targets are never selected.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::mesh::bootstrap_envelope::{
    BOOTSTRAP_ENVELOPE_SCHEMA_V1, BOOTSTRAP_MAX_ENVELOPE_BYTES, BootstrapCapability,
    BootstrapEnvelopeError, bootstrap_hello_target, decode_envelope, encode_envelope,
};
use ee::mesh::key_store::SecretBytes;
use ee::mesh::transport_session::{
    CounterViolation, DirectionalSessionKeys, FrameCapability, FrameDraft, FrameKind,
    MAX_FRAME_BYTES, NegotiatedExtensions, SessionBinding, SessionCounters, SessionDirection,
    TRANSPORT_FRAME_SCHEMA_V1, TRANSPORT_FRAME_SCHEMA_V2, TransportSessionError, decode_frame,
    derive_session_keys, sign_frame, verify_frame,
};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use serde_json::json;

fn fixture_binding() -> SessionBinding {
    SessionBinding {
        team_id: "team-prop".to_owned(),
        tailnet_id: "tailnet-prop.ts.net".to_owned(),
        initiator_node_id: "node-init".to_owned(),
        responder_node_id: "node-resp".to_owned(),
        initiator_workspace_id: "ws-init".to_owned(),
        responder_workspace_id: "ws-resp".to_owned(),
        initiator_stable_id: "stable-init".to_owned(),
        responder_stable_id: "stable-resp".to_owned(),
        session_id: "sess-prop".to_owned(),
    }
}

fn fixture_keys() -> DirectionalSessionKeys {
    derive_session_keys(
        &SecretBytes::new([0x42; 32]),
        &fixture_binding(),
        &[0x11; 32],
        &[0x22; 32],
    )
}

fn signed_hello(counter: u64) -> ee::mesh::transport_session::FrameV2 {
    sign_frame(
        &fixture_binding(),
        &fixture_keys(),
        FrameDraft {
            direction: SessionDirection::InitiatorToResponder,
            counter,
            correlation_id: "corr-prop".to_owned(),
            kind: FrameKind::Request,
            capability: FrameCapability::Hello,
            requested_budget_ms: 1_000,
            payload: json!({}),
        },
    )
    .expect("sign hello")
}

fn replay_violation(error: &TransportSessionError) -> Option<CounterViolation> {
    match error {
        TransportSessionError::ReplayRejected { violation, .. } => Some(*violation),
        _ => None,
    }
}

fn is_schema_mismatch(error: &TransportSessionError) -> bool {
    matches!(error, TransportSessionError::SchemaMismatch { .. })
}

fn is_frame_too_large(error: &TransportSessionError) -> bool {
    matches!(error, TransportSessionError::FrameTooLarge { .. })
}

fn is_binding_mismatch(error: &TransportSessionError) -> bool {
    matches!(error, TransportSessionError::BindingMismatch { .. })
}

fn is_unsupported_capability(error: &BootstrapEnvelopeError) -> bool {
    matches!(error, BootstrapEnvelopeError::UnsupportedCapability { .. })
}

fn is_envelope_over_budget(error: &BootstrapEnvelopeError) -> bool {
    matches!(error, BootstrapEnvelopeError::OverBudget { .. })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn decode_frame_never_panics_on_garbage(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let result = std::panic::catch_unwind(|| decode_frame(&bytes));
        prop_assert!(result.is_ok(), "decode_frame panicked");
        if let Ok(Ok(frame)) = result {
            prop_assert_eq!(frame.schema, TRANSPORT_FRAME_SCHEMA_V2);
        }
    }

    #[test]
    fn decode_frame_rejects_v1_and_unknown_schema(
        extra in "[a-z0-9]{0,16}",
        unknown in "[a-z.]{1,24}"
    ) {
        let v1 = serde_json::to_vec(&json!({
            "schema": TRANSPORT_FRAME_SCHEMA_V1,
            "extra": extra,
        }))
        .expect("encode v1");
        prop_assert_eq!(
            decode_frame(&v1).expect_err("v1"),
            TransportSessionError::V1Rejected
        );

        let other = serde_json::to_vec(&json!({
            "schema": format!("ee.mesh.{unknown}"),
        }))
        .expect("encode unknown");
        prop_assert!(is_schema_mismatch(
            &decode_frame(&other).expect_err("unknown")
        ));
    }

    #[test]
    fn truncated_and_oversize_frames_fail_closed(keep in 0_usize..64) {
        let encoded = serde_json::to_vec(&signed_hello(1)).expect("encode");
        let cut = encoded.len().min(keep);
        prop_assert!(decode_frame(&encoded[..cut]).is_err());

        let oversize = vec![b'{'; MAX_FRAME_BYTES + 1];
        prop_assert!(is_frame_too_large(
            &decode_frame(&oversize).expect_err("oversize")
        ));
    }

    #[test]
    fn exact_next_counter_accepts_and_violations_close(
        start in 1_u64..1_000,
        skip_by in 2_u64..16
    ) {
        let mut counters = SessionCounters::expecting(
            std::num::NonZeroU64::new(start).expect("start >= 1"),
        );
        counters.accept(start).expect("exact next");
        prop_assert_eq!(counters.expected_next(), start + 1);
        prop_assert!(!counters.is_closed());

        let dup = counters.accept(start).expect_err("duplicate");
        prop_assert_eq!(replay_violation(&dup), Some(CounterViolation::Duplicate));
        prop_assert!(counters.is_closed());
        prop_assert_eq!(
            counters.accept(start + 1).expect_err("closed"),
            TransportSessionError::SessionClosed
        );

        let mut skipped = SessionCounters::expecting(
            std::num::NonZeroU64::new(start).expect("start >= 1"),
        );
        let skip = skipped.accept(start.saturating_add(skip_by)).expect_err("skip");
        prop_assert_eq!(replay_violation(&skip), Some(CounterViolation::Skipped));

        if start > 1 {
            let mut advanced = SessionCounters::expecting(
                std::num::NonZeroU64::new(start).expect("start >= 1"),
            );
            advanced.accept(start).expect("first");
            let regress = advanced.accept(start - 1).expect_err("regress");
            prop_assert_eq!(
                replay_violation(&regress),
                Some(CounterViolation::Regressed)
            );
        }
    }

    #[test]
    fn signed_frame_verifies_and_flipped_mac_does_not_consume_counter(flip_at in 0_usize..63) {
        let frame = signed_hello(1);
        let encoded = serde_json::to_vec(&frame).expect("encode");
        let decoded = decode_frame(&encoded).expect("decode");
        let mut counters = SessionCounters::new();
        verify_frame(
            &decoded,
            &fixture_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &fixture_keys(),
            &NegotiatedExtensions::none(),
        )
        .expect("verify");
        prop_assert_eq!(counters.expected_next(), 2);

        let mut forged = signed_hello(1);
        let mut mac: Vec<u8> = forged.mac.bytes().collect();
        mac[flip_at] = if mac[flip_at] == b'0' { b'1' } else { b'0' };
        forged.mac = String::from_utf8(mac).expect("hex");
        let mut unconsumed = SessionCounters::new();
        let error = verify_frame(
            &forged,
            &fixture_binding(),
            SessionDirection::InitiatorToResponder,
            &mut unconsumed,
            &fixture_keys(),
            &NegotiatedExtensions::none(),
        )
        .expect_err("forged MAC");
        prop_assert_eq!(error, TransportSessionError::MacMismatch);
        prop_assert_eq!(unconsumed.expected_next(), 1);
        prop_assert!(!unconsumed.is_closed());
    }

    #[test]
    fn wrong_target_is_binding_mismatch_not_replay(suffix in "[a-z0-9]{1,8}") {
        let mut frame = signed_hello(1);
        frame.target_node_id = format!("node-{suffix}");
        let mut counters = SessionCounters::new();
        let error = verify_frame(
            &frame,
            &fixture_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &fixture_keys(),
            &NegotiatedExtensions::none(),
        )
        .expect_err("wrong target");
        prop_assert!(is_binding_mismatch(&error));
        prop_assert_eq!(error.degraded_code(), "mesh_frame_target_mismatch");
        prop_assert_eq!(counters.expected_next(), 1);
        prop_assert!(!counters.is_closed());
    }

    #[test]
    fn decode_envelope_never_panics_on_garbage(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let result = std::panic::catch_unwind(|| decode_envelope(&bytes));
        prop_assert!(result.is_ok(), "decode_envelope panicked");
    }

    #[test]
    fn bootstrap_hello_and_join_round_trip(token in "[a-z0-9]{0,24}") {
        let payload = json!({ "token": token });
        for capability in [BootstrapCapability::Hello, BootstrapCapability::Join] {
            let bytes = encode_envelope(capability, payload.clone()).expect("encode");
            let decoded = decode_envelope(&bytes).expect("decode");
            prop_assert_eq!(decoded.schema, BOOTSTRAP_ENVELOPE_SCHEMA_V1);
            prop_assert_eq!(decoded.capability, capability);
            prop_assert_eq!(decoded.payload, payload.clone());
        }
    }

    #[test]
    fn bootstrap_rejects_unknown_capability_and_oversize(name in "[A-Z][a-z]{1,12}") {
        let unknown = serde_json::to_vec(&json!({
            "schema": BOOTSTRAP_ENVELOPE_SCHEMA_V1,
            "capability": name,
            "payload": {},
        }))
        .expect("encode");
        prop_assert!(is_unsupported_capability(
            &decode_envelope(&unknown).expect_err("unknown")
        ));

        let oversize = vec![b'{'; BOOTSTRAP_MAX_ENVELOPE_BYTES + 1];
        prop_assert!(is_envelope_over_budget(
            &decode_envelope(&oversize).expect_err("oversize")
        ));
    }

    #[test]
    fn bootstrap_hello_target_never_selects_loopback_or_low_port(
        port in 1_u16..1024,
        high in 1024_u16..40000
    ) {
        prop_assert_eq!(
            bootstrap_hello_target(&["127.0.0.1".to_owned()], high),
            None
        );
        prop_assert_eq!(
            bootstrap_hello_target(&["::1".to_owned()], high),
            None
        );
        prop_assert_eq!(
            bootstrap_hello_target(&["100.64.1.2".to_owned()], port),
            None
        );
    }
}
