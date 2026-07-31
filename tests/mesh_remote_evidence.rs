//! SRR6.36 remote evidence materialization contract tests.

use std::io::Cursor;

use ee::mesh::cache::blake3_content_hash;
use ee::mesh::remote_evidence::{
    MESH_REMOTE_EVIDENCE_MATERIALIZATION_SCHEMA_V1, MESH_REMOTE_EVIDENCE_SCHEMA_V1,
    MeshRemoteEvidenceFetchPolicy, MeshRemoteEvidenceFetchStatus, MeshRemoteEvidenceKind,
    MeshRemoteEvidenceMaterializationInput, MeshRemoteEvidencePolicyClass,
    MeshRemoteEvidenceRedaction, MeshRemoteEvidenceRef, MeshRemoteEvidenceStreamError,
    REMOTE_EVIDENCE_BODY_QUARANTINED_EVENT, REMOTE_EVIDENCE_FETCH_ALLOWED_EVENT,
    REMOTE_EVIDENCE_FETCH_DENIED_EVENT, REMOTE_EVIDENCE_HASH_VERIFIED_EVENT,
    REMOTE_EVIDENCE_REF_INDEXED_EVENT, copy_remote_evidence_body_bounded, degraded_codes,
    normalize_remote_evidence_uri, plan_remote_evidence_materialization,
};

#[test]
fn cass_session_reference_normalizes_without_fetching_session_body() {
    let uri = match normalize_remote_evidence_uri(
        MeshRemoteEvidenceKind::CassSession,
        " cass-session://sess_A1-B2#L7-11 ",
    ) {
        Ok(uri) => uri,
        Err(err) => panic!("expected valid CASS session evidence URI, got {err:?}"),
    };

    assert_eq!(uri, "cass-session://sess_A1-B2#L7-11");

    let reference = MeshRemoteEvidenceRef::new(
        "ref_cass_span",
        MeshRemoteEvidenceKind::CassSession,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        uri,
    );

    assert_eq!(reference.schema, MESH_REMOTE_EVIDENCE_SCHEMA_V1);
    assert_eq!(
        reference.policy_class,
        MeshRemoteEvidencePolicyClass::SessionReference
    );
    assert_eq!(reference.redaction, MeshRemoteEvidenceRedaction::Redacted);
}

#[test]
fn denied_remote_artifact_keeps_redacted_placeholder_even_if_body_arrives() {
    let reference = MeshRemoteEvidenceRef::new(
        "ref_secret_artifact",
        MeshRemoteEvidenceKind::Artifact,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        "artifact://support_bundle_001",
    )
    .with_redaction(MeshRemoteEvidenceRedaction::Denied);

    let plan = plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
        reference: &reference,
        policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(10_000),
        fetch_consent: true,
        fetched_body: Some(b"remote secret body"),
    });

    assert_eq!(plan.schema, MESH_REMOTE_EVIDENCE_MATERIALIZATION_SCHEMA_V1);
    assert_eq!(plan.status, MeshRemoteEvidenceFetchStatus::Denied);
    assert!(!plan.body_persist_allowed);
    assert_eq!(
        plan.placeholder,
        "<remote_evidence ref=ref_secret_artifact kind=artifact status=denied>"
    );
    assert!(plan.why.contains("redacted placeholder"));
    assert_eq!(
        event_names(&plan),
        vec![
            REMOTE_EVIDENCE_REF_INDEXED_EVENT,
            REMOTE_EVIDENCE_FETCH_DENIED_EVENT
        ]
    );
}

#[test]
fn fetchable_remote_body_remains_metadata_only_until_lazy_fetch_occurs() {
    let body = b"remote body fetched later";
    let reference = MeshRemoteEvidenceRef::new(
        "ref_remote_body",
        MeshRemoteEvidenceKind::MemoryBody,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        "memory-body://mem_remote",
    )
    .with_content_hash(blake3_content_hash(body))
    .with_size_bytes(body.len() as u64)
    .with_redaction(MeshRemoteEvidenceRedaction::Shared);

    let plan = plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
        reference: &reference,
        policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(10_000),
        fetch_consent: true,
        fetched_body: None,
    });

    assert_eq!(plan.status, MeshRemoteEvidenceFetchStatus::Fetchable);
    assert!(!plan.body_persist_allowed);
    assert!(plan.why.contains("not eagerly copied"));
    assert!(plan.provenance_note.contains("memory-body://mem_remote"));
    assert_eq!(
        event_names(&plan),
        vec![
            REMOTE_EVIDENCE_REF_INDEXED_EVENT,
            REMOTE_EVIDENCE_FETCH_ALLOWED_EVENT
        ]
    );
}

#[test]
fn allowed_fetch_requires_content_hash_match_before_body_persistence() {
    let body = b"remote evidence span body";
    let expected = blake3_content_hash(body);
    let reference = MeshRemoteEvidenceRef::new(
        "ref_span",
        MeshRemoteEvidenceKind::EvidenceSpan,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        "evidence://span_001",
    )
    .with_content_hash(expected.clone())
    .with_size_bytes(body.len() as u64)
    .with_redaction(MeshRemoteEvidenceRedaction::Shared);

    let plan = plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
        reference: &reference,
        policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(10_000),
        fetch_consent: true,
        fetched_body: Some(body),
    });

    assert_eq!(plan.status, MeshRemoteEvidenceFetchStatus::HashVerified);
    assert!(plan.body_persist_allowed);
    assert_eq!(
        plan.expected_content_hash.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(plan.actual_content_hash.as_deref(), Some(expected.as_str()));
    assert!(plan.why.contains("content hash matched"));
    assert_eq!(
        event_names(&plan),
        vec![
            REMOTE_EVIDENCE_REF_INDEXED_EVENT,
            REMOTE_EVIDENCE_FETCH_ALLOWED_EVENT,
            REMOTE_EVIDENCE_HASH_VERIFIED_EVENT
        ]
    );
}

#[test]
fn hash_mismatch_quarantines_remote_material_without_persistence() {
    let expected = blake3_content_hash(b"expected body");
    let reference = MeshRemoteEvidenceRef::new(
        "ref_mismatch",
        MeshRemoteEvidenceKind::EvidenceSpan,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        "evidence://span_002",
    )
    .with_content_hash(expected.clone())
    .with_size_bytes(13)
    .with_redaction(MeshRemoteEvidenceRedaction::Shared);

    let plan = plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
        reference: &reference,
        policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(10_000),
        fetch_consent: true,
        fetched_body: Some(b"tampered body"),
    });

    assert_eq!(plan.status, MeshRemoteEvidenceFetchStatus::HashMismatch);
    assert!(!plan.body_persist_allowed);
    assert_eq!(
        plan.expected_content_hash.as_deref(),
        Some(expected.as_str())
    );
    assert_ne!(plan.actual_content_hash.as_deref(), Some(expected.as_str()));
    assert!(plan.why.contains("quarantined"));
    assert_eq!(
        plan.degraded_codes,
        vec![degraded_codes::FETCHED_BODY_HASH_MISMATCH]
    );
    assert!(plan.logs.iter().any(|log| {
        log.event == REMOTE_EVIDENCE_BODY_QUARANTINED_EVENT && log.reason == "content_hash_mismatch"
    }));
}

#[test]
fn bounded_stream_accepts_exact_cap_and_rejects_cap_plus_one_mid_stream() {
    let mut exact_reader = Cursor::new(b"1234");
    let mut exact_staging = Vec::new();
    let copied = match copy_remote_evidence_body_bounded(&mut exact_reader, &mut exact_staging, 4) {
        Ok(copied) => copied,
        Err(error) => panic!("exact-cap body should pass: {error}"),
    };
    assert_eq!(copied, 4);
    assert_eq!(exact_staging, b"1234");

    let mut oversized_reader = Cursor::new(b"12345");
    let mut oversized_staging = Vec::new();
    let error =
        match copy_remote_evidence_body_bounded(&mut oversized_reader, &mut oversized_staging, 4) {
            Ok(copied) => panic!("cap+1 body must fail, copied {copied} bytes"),
            Err(error) => error,
        };
    assert_eq!(error.code(), degraded_codes::BODY_SIZE_EXCEEDS_POLICY);
    assert!(matches!(
        error,
        MeshRemoteEvidenceStreamError::SizeLimitExceeded { max_bytes: 4 }
    ));
    assert_eq!(oversized_reader.position(), 5);
    assert_eq!(oversized_staging, b"1234");
}

#[test]
fn absent_declared_size_cannot_bypass_actual_body_cap() {
    let within_cap = b"1234";
    let within_cap_reference = MeshRemoteEvidenceRef::new(
        "ref_missing_size_ok",
        MeshRemoteEvidenceKind::MemoryBody,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        "memory-body://mem_remote",
    )
    .with_content_hash(blake3_content_hash(within_cap))
    .with_redaction(MeshRemoteEvidenceRedaction::Shared);
    let within_cap_plan =
        plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
            reference: &within_cap_reference,
            policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(4),
            fetch_consent: true,
            fetched_body: Some(within_cap),
        });
    assert_eq!(
        within_cap_plan.status,
        MeshRemoteEvidenceFetchStatus::HashVerified
    );
    assert_eq!(within_cap_plan.declared_size_bytes, None);
    assert_eq!(within_cap_plan.actual_size_bytes, Some(4));
    assert!(within_cap_plan.degraded_codes.is_empty());

    let oversized = b"12345";
    let oversized_reference = MeshRemoteEvidenceRef::new(
        "ref_missing_size_oversized",
        MeshRemoteEvidenceKind::MemoryBody,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        "memory-body://mem_remote",
    )
    .with_content_hash(blake3_content_hash(oversized))
    .with_redaction(MeshRemoteEvidenceRedaction::Shared);
    let oversized_plan =
        plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
            reference: &oversized_reference,
            policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(4),
            fetch_consent: true,
            fetched_body: Some(oversized),
        });
    assert_eq!(
        oversized_plan.status,
        MeshRemoteEvidenceFetchStatus::Quarantined
    );
    assert!(!oversized_plan.body_persist_allowed);
    assert_eq!(oversized_plan.actual_content_hash, None);
    assert_eq!(oversized_plan.actual_size_bytes, Some(5));
    assert_eq!(
        oversized_plan.degraded_codes,
        vec![degraded_codes::BODY_SIZE_EXCEEDS_POLICY]
    );
}

#[test]
fn understated_declared_size_is_quarantined_before_hashing() {
    let body = b"1234";
    let reference = MeshRemoteEvidenceRef::new(
        "ref_declared_size_lie",
        MeshRemoteEvidenceKind::EvidenceSpan,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        "evidence://span_size_lie",
    )
    .with_content_hash(blake3_content_hash(body))
    .with_size_bytes(3)
    .with_redaction(MeshRemoteEvidenceRedaction::Shared);

    let plan = plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
        reference: &reference,
        policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(4),
        fetch_consent: true,
        fetched_body: Some(body),
    });

    assert_eq!(plan.status, MeshRemoteEvidenceFetchStatus::Quarantined);
    assert!(!plan.body_persist_allowed);
    assert_eq!(plan.declared_size_bytes, Some(3));
    assert_eq!(plan.actual_size_bytes, Some(4));
    assert_eq!(plan.actual_content_hash, None);
    assert_eq!(
        plan.degraded_codes,
        vec![degraded_codes::DECLARED_SIZE_MISMATCH]
    );
    assert!(plan.logs.iter().any(|log| {
        log.event == REMOTE_EVIDENCE_BODY_QUARANTINED_EVENT
            && log.reason == "declared_size_mismatch"
    }));
}

#[test]
fn u64_max_cap_does_not_overflow_size_conversion_or_stream_probe() {
    let body = b"bounded";
    let reference = MeshRemoteEvidenceRef::new(
        "ref_u64_boundary",
        MeshRemoteEvidenceKind::Artifact,
        "wsp_remote",
        "peer_alpha",
        "mem_remote",
        "artifact://u64_boundary",
    )
    .with_content_hash(blake3_content_hash(body))
    .with_size_bytes(body.len() as u64)
    .with_redaction(MeshRemoteEvidenceRedaction::Shared);
    let plan = plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
        reference: &reference,
        policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(u64::MAX),
        fetch_consent: true,
        fetched_body: Some(body),
    });
    assert_eq!(plan.status, MeshRemoteEvidenceFetchStatus::HashVerified);

    let mut empty_reader = Cursor::new(Vec::<u8>::new());
    let mut staged = Vec::new();
    let copied = match copy_remote_evidence_body_bounded(&mut empty_reader, &mut staged, u64::MAX) {
        Ok(copied) => copied,
        Err(error) => panic!("empty stream with u64::MAX cap should not overflow: {error}"),
    };
    assert_eq!(copied, 0);
    assert!(staged.is_empty());
}

#[test]
fn unsafe_remote_evidence_uris_are_rejected_before_indexing() {
    let err = match normalize_remote_evidence_uri(
        MeshRemoteEvidenceKind::Artifact,
        "artifact://../private_bundle",
    ) {
        Ok(uri) => panic!("expected path-like artifact id to be unsafe, got {uri}"),
        Err(err) => err,
    };
    assert_eq!(err.reason(), "unsafe_uri_identifier");

    let err = match normalize_remote_evidence_uri(
        MeshRemoteEvidenceKind::EvidenceSpan,
        "evidence://localhost/span_001",
    ) {
        Ok(uri) => panic!("expected localhost URI to be unsafe, got {uri}"),
        Err(err) => err,
    };
    assert_eq!(err.reason(), "unsafe_uri");
}

fn event_names(
    plan: &ee::mesh::remote_evidence::MeshRemoteEvidenceMaterializationPlan,
) -> Vec<&'static str> {
    plan.logs.iter().map(|log| log.event).collect()
}
