//! SRR6.36 remote evidence, artifact, and session-reference materialization.
//!
//! This module is a pure planning surface for cached peer evidence. It never
//! opens remote files, calls CASS, or persists bodies. Callers index the
//! reference, check policy, and then perform any permitted lazy fetch elsewhere.

use std::fmt;

use serde::Serialize;

use crate::cass::normalize_cass_session_uri;
use crate::mesh::cache::blake3_content_hash;

pub const MESH_REMOTE_EVIDENCE_SCHEMA_V1: &str = "ee.mesh.remote_evidence.v1";
pub const MESH_REMOTE_EVIDENCE_MATERIALIZATION_SCHEMA_V1: &str =
    "ee.mesh.remote_evidence_materialization.v1";

pub const REMOTE_EVIDENCE_REF_INDEXED_EVENT: &str = "evidence_ref_indexed";
pub const REMOTE_EVIDENCE_FETCH_ALLOWED_EVENT: &str = "evidence_fetch_allowed";
pub const REMOTE_EVIDENCE_FETCH_DENIED_EVENT: &str = "evidence_fetch_denied";
pub const REMOTE_EVIDENCE_HASH_VERIFIED_EVENT: &str = "evidence_hash_verified";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshRemoteEvidenceKind {
    MemoryBody,
    EvidenceSpan,
    Artifact,
    CassSession,
    SupportBundle,
}

impl MeshRemoteEvidenceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MemoryBody => "memory_body",
            Self::EvidenceSpan => "evidence_span",
            Self::Artifact => "artifact",
            Self::CassSession => "cass_session",
            Self::SupportBundle => "support_bundle",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshRemoteEvidenceRedaction {
    Shared,
    Redacted,
    Denied,
}

impl MeshRemoteEvidenceRedaction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Redacted => "redacted",
            Self::Denied => "denied",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshRemoteEvidenceFetchStatus {
    Fetchable,
    Denied,
    Unavailable,
    HashVerified,
    HashMismatch,
}

impl MeshRemoteEvidenceFetchStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fetchable => "fetchable",
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
            Self::HashVerified => "hash_verified",
            Self::HashMismatch => "hash_mismatch",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshRemoteEvidencePolicyClass {
    MemoryBody,
    EvidenceSpan,
    Artifact,
    SessionReference,
}

impl MeshRemoteEvidencePolicyClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MemoryBody => "memory_body",
            Self::EvidenceSpan => "evidence_span",
            Self::Artifact => "artifact",
            Self::SessionReference => "session_reference",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRemoteEvidenceRef {
    pub schema: &'static str,
    pub ref_id: String,
    pub kind: MeshRemoteEvidenceKind,
    pub policy_class: MeshRemoteEvidencePolicyClass,
    pub origin_workspace_id: String,
    pub producer_peer_id: String,
    pub logical_memory_id: String,
    pub evidence_uri: String,
    pub body_ref: Option<String>,
    pub content_hash: Option<String>,
    pub size_bytes: Option<u64>,
    pub redaction: MeshRemoteEvidenceRedaction,
}

impl MeshRemoteEvidenceRef {
    #[must_use]
    pub fn new(
        ref_id: impl Into<String>,
        kind: MeshRemoteEvidenceKind,
        origin_workspace_id: impl Into<String>,
        producer_peer_id: impl Into<String>,
        logical_memory_id: impl Into<String>,
        evidence_uri: impl Into<String>,
    ) -> Self {
        Self {
            schema: MESH_REMOTE_EVIDENCE_SCHEMA_V1,
            ref_id: ref_id.into(),
            kind,
            policy_class: policy_class_for_kind(kind),
            origin_workspace_id: origin_workspace_id.into(),
            producer_peer_id: producer_peer_id.into(),
            logical_memory_id: logical_memory_id.into(),
            evidence_uri: evidence_uri.into(),
            body_ref: None,
            content_hash: None,
            size_bytes: None,
            redaction: MeshRemoteEvidenceRedaction::Redacted,
        }
    }

    #[must_use]
    pub fn with_body_ref(mut self, body_ref: impl Into<String>) -> Self {
        self.body_ref = Some(body_ref.into());
        self
    }

    #[must_use]
    pub fn with_content_hash(mut self, content_hash: impl Into<String>) -> Self {
        self.content_hash = Some(content_hash.into());
        self
    }

    #[must_use]
    pub const fn with_size_bytes(mut self, size_bytes: u64) -> Self {
        self.size_bytes = Some(size_bytes);
        self
    }

    #[must_use]
    pub const fn with_redaction(mut self, redaction: MeshRemoteEvidenceRedaction) -> Self {
        self.redaction = redaction;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshRemoteEvidenceFetchPolicy {
    pub allow_memory_body: bool,
    pub allow_evidence_span: bool,
    pub allow_artifact: bool,
    pub allow_session_reference: bool,
    pub requires_consent: bool,
    pub max_bytes: Option<u64>,
}

impl MeshRemoteEvidenceFetchPolicy {
    #[must_use]
    pub const fn denied() -> Self {
        Self {
            allow_memory_body: false,
            allow_evidence_span: false,
            allow_artifact: false,
            allow_session_reference: false,
            requires_consent: true,
            max_bytes: Some(0),
        }
    }

    #[must_use]
    pub const fn metadata_only() -> Self {
        Self {
            allow_memory_body: false,
            allow_evidence_span: false,
            allow_artifact: false,
            allow_session_reference: true,
            requires_consent: true,
            max_bytes: Some(0),
        }
    }

    #[must_use]
    pub const fn trusted_fetch(max_bytes: u64) -> Self {
        Self {
            allow_memory_body: true,
            allow_evidence_span: true,
            allow_artifact: true,
            allow_session_reference: true,
            requires_consent: true,
            max_bytes: Some(max_bytes),
        }
    }

    const fn allows(self, policy_class: MeshRemoteEvidencePolicyClass) -> bool {
        match policy_class {
            MeshRemoteEvidencePolicyClass::MemoryBody => self.allow_memory_body,
            MeshRemoteEvidencePolicyClass::EvidenceSpan => self.allow_evidence_span,
            MeshRemoteEvidencePolicyClass::Artifact => self.allow_artifact,
            MeshRemoteEvidencePolicyClass::SessionReference => self.allow_session_reference,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshRemoteEvidenceMaterializationInput<'a> {
    pub reference: &'a MeshRemoteEvidenceRef,
    pub policy: MeshRemoteEvidenceFetchPolicy,
    pub fetch_consent: bool,
    pub fetched_body: Option<&'a [u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRemoteEvidenceMaterializationPlan {
    pub schema: &'static str,
    pub ref_id: String,
    pub kind: MeshRemoteEvidenceKind,
    pub policy_class: MeshRemoteEvidencePolicyClass,
    pub status: MeshRemoteEvidenceFetchStatus,
    pub body_persist_allowed: bool,
    pub placeholder: String,
    pub expected_content_hash: Option<String>,
    pub actual_content_hash: Option<String>,
    pub provenance_note: String,
    pub why: String,
    pub logs: Vec<MeshRemoteEvidenceLog>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRemoteEvidenceLog {
    pub event: &'static str,
    pub ref_id: String,
    pub policy_class: &'static str,
    pub status: &'static str,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshRemoteEvidenceUriError {
    reason: &'static str,
}

impl MeshRemoteEvidenceUriError {
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for MeshRemoteEvidenceUriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid mesh remote evidence URI: {}",
            self.reason
        )
    }
}

impl std::error::Error for MeshRemoteEvidenceUriError {}

#[must_use]
pub const fn policy_class_for_kind(kind: MeshRemoteEvidenceKind) -> MeshRemoteEvidencePolicyClass {
    match kind {
        MeshRemoteEvidenceKind::MemoryBody => MeshRemoteEvidencePolicyClass::MemoryBody,
        MeshRemoteEvidenceKind::EvidenceSpan => MeshRemoteEvidencePolicyClass::EvidenceSpan,
        MeshRemoteEvidenceKind::Artifact | MeshRemoteEvidenceKind::SupportBundle => {
            MeshRemoteEvidencePolicyClass::Artifact
        }
        MeshRemoteEvidenceKind::CassSession => MeshRemoteEvidencePolicyClass::SessionReference,
    }
}

pub fn normalize_remote_evidence_uri(
    kind: MeshRemoteEvidenceKind,
    raw: &str,
) -> Result<String, MeshRemoteEvidenceUriError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(MeshRemoteEvidenceUriError {
            reason: "empty_uri",
        });
    }
    if raw.chars().any(char::is_control) || raw.contains("://localhost") {
        return Err(MeshRemoteEvidenceUriError {
            reason: "unsafe_uri",
        });
    }
    match kind {
        MeshRemoteEvidenceKind::CassSession => normalize_cass_session_uri(raw)
            .map(|reference| reference.to_uri())
            .map_err(|_| MeshRemoteEvidenceUriError {
                reason: "invalid_cass_session_uri",
            }),
        MeshRemoteEvidenceKind::MemoryBody => normalize_prefixed_uri(raw, "memory-body://"),
        MeshRemoteEvidenceKind::EvidenceSpan => normalize_prefixed_uri(raw, "evidence://"),
        MeshRemoteEvidenceKind::Artifact => normalize_prefixed_uri(raw, "artifact://"),
        MeshRemoteEvidenceKind::SupportBundle => normalize_prefixed_uri(raw, "support-bundle://"),
    }
}

pub fn plan_remote_evidence_materialization(
    input: MeshRemoteEvidenceMaterializationInput<'_>,
) -> MeshRemoteEvidenceMaterializationPlan {
    let reference = input.reference;
    let mut logs = vec![log_for(
        REMOTE_EVIDENCE_REF_INDEXED_EVENT,
        reference,
        MeshRemoteEvidenceFetchStatus::Unavailable,
        "remote_reference_indexed_without_body_copy",
    )];

    if let Some(reason) = denial_reason(reference, input.policy, input.fetch_consent) {
        logs.push(log_for(
            REMOTE_EVIDENCE_FETCH_DENIED_EVENT,
            reference,
            MeshRemoteEvidenceFetchStatus::Denied,
            reason,
        ));
        return plan_with(
            reference,
            MeshRemoteEvidenceFetchStatus::Denied,
            false,
            None,
            logs,
            reason,
        );
    }

    logs.push(log_for(
        REMOTE_EVIDENCE_FETCH_ALLOWED_EVENT,
        reference,
        MeshRemoteEvidenceFetchStatus::Fetchable,
        "policy_allows_lazy_materialization",
    ));

    let Some(fetched_body) = input.fetched_body else {
        return plan_with(
            reference,
            MeshRemoteEvidenceFetchStatus::Fetchable,
            false,
            None,
            logs,
            "lazy_fetch_required",
        );
    };
    let actual_hash = blake3_content_hash(fetched_body);
    let hash_matches = reference
        .content_hash
        .as_deref()
        .is_some_and(|expected| expected == actual_hash);
    if hash_matches {
        logs.push(log_for(
            REMOTE_EVIDENCE_HASH_VERIFIED_EVENT,
            reference,
            MeshRemoteEvidenceFetchStatus::HashVerified,
            "content_hash_verified",
        ));
        plan_with(
            reference,
            MeshRemoteEvidenceFetchStatus::HashVerified,
            true,
            Some(actual_hash),
            logs,
            "content_hash_verified",
        )
    } else {
        plan_with(
            reference,
            MeshRemoteEvidenceFetchStatus::HashMismatch,
            false,
            Some(actual_hash),
            logs,
            "content_hash_mismatch",
        )
    }
}

fn normalize_prefixed_uri(
    raw: &str,
    expected_prefix: &'static str,
) -> Result<String, MeshRemoteEvidenceUriError> {
    let Some(rest) = raw.strip_prefix(expected_prefix) else {
        return Err(MeshRemoteEvidenceUriError {
            reason: "unexpected_uri_scheme",
        });
    };
    if rest.is_empty() || rest.contains("..") || rest.contains('/') || rest.contains('\\') {
        return Err(MeshRemoteEvidenceUriError {
            reason: "unsafe_uri_identifier",
        });
    }
    if !rest
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(MeshRemoteEvidenceUriError {
            reason: "unsupported_uri_identifier_character",
        });
    }
    Ok(format!("{expected_prefix}{rest}"))
}

fn denial_reason(
    reference: &MeshRemoteEvidenceRef,
    policy: MeshRemoteEvidenceFetchPolicy,
    fetch_consent: bool,
) -> Option<&'static str> {
    if reference.redaction == MeshRemoteEvidenceRedaction::Denied {
        return Some("redaction_policy_denied");
    }
    if !policy.allows(reference.policy_class) {
        return Some("policy_class_denied");
    }
    if policy.requires_consent && !fetch_consent {
        return Some("fetch_consent_required");
    }
    if let (Some(size_bytes), Some(max_bytes)) = (reference.size_bytes, policy.max_bytes) {
        if size_bytes > max_bytes {
            return Some("size_exceeds_policy");
        }
    }
    None
}

fn plan_with(
    reference: &MeshRemoteEvidenceRef,
    status: MeshRemoteEvidenceFetchStatus,
    body_persist_allowed: bool,
    actual_content_hash: Option<String>,
    logs: Vec<MeshRemoteEvidenceLog>,
    reason: &'static str,
) -> MeshRemoteEvidenceMaterializationPlan {
    MeshRemoteEvidenceMaterializationPlan {
        schema: MESH_REMOTE_EVIDENCE_MATERIALIZATION_SCHEMA_V1,
        ref_id: reference.ref_id.clone(),
        kind: reference.kind,
        policy_class: reference.policy_class,
        status,
        body_persist_allowed,
        placeholder: placeholder_for(reference, status),
        expected_content_hash: reference.content_hash.clone(),
        actual_content_hash,
        provenance_note: format!(
            "Remote evidence {} for memory {} is {} via {} from peer {}; reason={reason}.",
            reference.ref_id,
            reference.logical_memory_id,
            status.as_str(),
            reference.evidence_uri,
            reference.producer_peer_id
        ),
        why: why_for(reference, status, reason),
        logs,
    }
}

fn placeholder_for(
    reference: &MeshRemoteEvidenceRef,
    status: MeshRemoteEvidenceFetchStatus,
) -> String {
    format!(
        "<remote_evidence ref={} kind={} status={}>",
        reference.ref_id,
        reference.kind.as_str(),
        status.as_str()
    )
}

fn why_for(
    reference: &MeshRemoteEvidenceRef,
    status: MeshRemoteEvidenceFetchStatus,
    reason: &'static str,
) -> String {
    match status {
        MeshRemoteEvidenceFetchStatus::HashVerified => format!(
            "The remote {} evidence was fetched lazily and its content hash matched before persistence.",
            reference.kind.as_str()
        ),
        MeshRemoteEvidenceFetchStatus::Fetchable => format!(
            "The remote {} evidence is fetchable, but the body was not eagerly copied into the local cache.",
            reference.kind.as_str()
        ),
        MeshRemoteEvidenceFetchStatus::Denied => format!(
            "The remote {} evidence is represented only by a redacted placeholder because {reason}.",
            reference.kind.as_str()
        ),
        MeshRemoteEvidenceFetchStatus::HashMismatch => format!(
            "The fetched remote {} evidence is quarantined because {reason}.",
            reference.kind.as_str()
        ),
        MeshRemoteEvidenceFetchStatus::Unavailable => format!(
            "The remote {} evidence reference is indexed, but the body is unavailable.",
            reference.kind.as_str()
        ),
    }
}

fn log_for(
    event: &'static str,
    reference: &MeshRemoteEvidenceRef,
    status: MeshRemoteEvidenceFetchStatus,
    reason: &'static str,
) -> MeshRemoteEvidenceLog {
    MeshRemoteEvidenceLog {
        event,
        ref_id: reference.ref_id.clone(),
        policy_class: reference.policy_class.as_str(),
        status: status.as_str(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_cass_session_references_without_copying_body() {
        let uri = normalize_remote_evidence_uri(
            MeshRemoteEvidenceKind::CassSession,
            "cass-session://sess_ABC-123#L9-12",
        )
        .expect("valid cass URI");

        assert_eq!(uri, "cass-session://sess_ABC-123#L9-12");
        assert_eq!(
            normalize_remote_evidence_uri(
                MeshRemoteEvidenceKind::CassSession,
                "cass-session://../private/session#L1",
            )
            .expect_err("path-like session ids are rejected")
            .reason(),
            "invalid_cass_session_uri"
        );
    }

    #[test]
    fn denied_policy_keeps_redacted_placeholder_and_structured_log() {
        let reference = MeshRemoteEvidenceRef::new(
            "ref_secret",
            MeshRemoteEvidenceKind::Artifact,
            "wsp_remote",
            "peer_alpha",
            "mem_remote",
            "artifact://support_bundle",
        )
        .with_redaction(MeshRemoteEvidenceRedaction::Denied);

        let plan = plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
            reference: &reference,
            policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(10_000),
            fetch_consent: true,
            fetched_body: Some(b"secret artifact body"),
        });

        assert_eq!(plan.status, MeshRemoteEvidenceFetchStatus::Denied);
        assert!(!plan.body_persist_allowed);
        assert_eq!(
            plan.placeholder,
            "<remote_evidence ref=ref_secret kind=artifact status=denied>"
        );
        assert_eq!(
            plan.logs.iter().map(|log| log.event).collect::<Vec<_>>(),
            vec![
                REMOTE_EVIDENCE_REF_INDEXED_EVENT,
                REMOTE_EVIDENCE_FETCH_DENIED_EVENT
            ]
        );
        assert!(plan.why.contains("redacted placeholder"));
    }

    #[test]
    fn allowed_fetch_requires_hash_match_before_persisting_body() {
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
        assert!(
            plan.logs
                .iter()
                .any(|log| log.event == REMOTE_EVIDENCE_HASH_VERIFIED_EVENT)
        );
    }

    #[test]
    fn hash_mismatch_quarantines_fetched_remote_material() {
        let reference = MeshRemoteEvidenceRef::new(
            "ref_body",
            MeshRemoteEvidenceKind::MemoryBody,
            "wsp_remote",
            "peer_alpha",
            "mem_remote",
            "memory-body://mem_remote",
        )
        .with_content_hash(blake3_content_hash(b"expected"))
        .with_size_bytes(7)
        .with_redaction(MeshRemoteEvidenceRedaction::Shared);

        let plan = plan_remote_evidence_materialization(MeshRemoteEvidenceMaterializationInput {
            reference: &reference,
            policy: MeshRemoteEvidenceFetchPolicy::trusted_fetch(10_000),
            fetch_consent: true,
            fetched_body: Some(b"changed"),
        });

        assert_eq!(plan.status, MeshRemoteEvidenceFetchStatus::HashMismatch);
        assert!(!plan.body_persist_allowed);
        assert!(plan.why.contains("quarantined"));
    }
}
