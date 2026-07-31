//! SRR6.36 remote evidence, artifact, and session-reference materialization.
//!
//! This module plans cached peer evidence and owns the bounded streaming
//! primitive that fetch adapters must use. It never opens remote files, calls
//! CASS, or persists bodies on its own.

use std::fmt;
use std::io::{self, Read, Write};

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
pub const REMOTE_EVIDENCE_BODY_QUARANTINED_EVENT: &str = "evidence_body_quarantined";

const REMOTE_EVIDENCE_STREAM_BUFFER_BYTES: usize = 16 * 1024;

pub mod degraded_codes {
    pub const BODY_SIZE_EXCEEDS_POLICY: &str = "mesh_remote_evidence_body_size_exceeds_policy";
    pub const DECLARED_SIZE_MISMATCH: &str = "mesh_remote_evidence_declared_size_mismatch";
    pub const FETCHED_BODY_HASH_MISMATCH: &str = "mesh_fetched_body_hash_mismatch";
}

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
    Quarantined,
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
            Self::Quarantined => "quarantined",
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
    pub max_bytes: u64,
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
            max_bytes: 0,
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
            max_bytes: 0,
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
            max_bytes,
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
    pub declared_size_bytes: Option<u64>,
    pub actual_size_bytes: Option<u64>,
    pub degraded_codes: Vec<&'static str>,
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

#[derive(Debug)]
pub enum MeshRemoteEvidenceStreamError {
    Read(io::Error),
    Write(io::Error),
    SizeLimitExceeded { max_bytes: u64 },
}

impl MeshRemoteEvidenceStreamError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Read(_) | Self::Write(_) => "mesh_remote_evidence_stream_io_failed",
            Self::SizeLimitExceeded { .. } => degraded_codes::BODY_SIZE_EXCEEDS_POLICY,
        }
    }
}

impl fmt::Display for MeshRemoteEvidenceStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "failed to read remote evidence body: {error}"),
            Self::Write(error) => {
                write!(
                    formatter,
                    "failed to write staged remote evidence body: {error}"
                )
            }
            Self::SizeLimitExceeded { max_bytes } => write!(
                formatter,
                "remote evidence body exceeds the {max_bytes}-byte policy limit"
            ),
        }
    }
}

impl std::error::Error for MeshRemoteEvidenceStreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) | Self::Write(error) => Some(error),
            Self::SizeLimitExceeded { .. } => None,
        }
    }
}

/// Copy a remote body through a hard streaming cap.
///
/// The reader is consumed through at most `max_bytes + 1` bytes. The extra
/// byte is an overflow probe and is never written. On error, the destination
/// can contain a partial body and must remain private staging material.
///
/// `reader` must be scoped to exactly one framed body. The overflow probe
/// consumes one byte when the body is too large, so passing a reader that also
/// exposes the next protocol frame would consume that frame's first byte when
/// an exact-cap body is followed by more traffic.
pub fn copy_remote_evidence_body_bounded(
    reader: &mut impl Read,
    writer: &mut impl Write,
    max_bytes: u64,
) -> Result<u64, MeshRemoteEvidenceStreamError> {
    let mut copied = 0_u64;
    let mut buffer = [0_u8; REMOTE_EVIDENCE_STREAM_BUFFER_BYTES];

    loop {
        if copied == max_bytes {
            let mut overflow_probe = [0_u8; 1];
            let overflow_bytes = read_remote_evidence_chunk(reader, &mut overflow_probe)?;
            return if overflow_bytes == 0 {
                Ok(copied)
            } else {
                Err(MeshRemoteEvidenceStreamError::SizeLimitExceeded { max_bytes })
            };
        }

        let remaining = max_bytes - copied;
        let read_budget = remaining.min(REMOTE_EVIDENCE_STREAM_BUFFER_BYTES as u64);
        let read_budget =
            usize::try_from(read_budget).unwrap_or(REMOTE_EVIDENCE_STREAM_BUFFER_BYTES);
        let bytes_read = read_remote_evidence_chunk(reader, &mut buffer[..read_budget])?;
        if bytes_read == 0 {
            return Ok(copied);
        }

        writer
            .write_all(&buffer[..bytes_read])
            .map_err(MeshRemoteEvidenceStreamError::Write)?;
        let bytes_read = u64::try_from(bytes_read)
            .map_err(|_| MeshRemoteEvidenceStreamError::SizeLimitExceeded { max_bytes })?;
        copied += bytes_read;
    }
}

fn read_remote_evidence_chunk(
    reader: &mut impl Read,
    buffer: &mut [u8],
) -> Result<usize, MeshRemoteEvidenceStreamError> {
    loop {
        match reader.read(buffer) {
            Ok(bytes_read) => return Ok(bytes_read),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(MeshRemoteEvidenceStreamError::Read(error)),
        }
    }
}

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

    if let Some(reason) = denial_reason(
        reference,
        input.policy,
        input.fetch_consent,
        input.fetched_body.is_none(),
    ) {
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
            None,
            Vec::new(),
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
            None,
            Vec::new(),
            logs,
            "lazy_fetch_required",
        );
    };
    let actual_size_bytes = match u64::try_from(fetched_body.len()) {
        Ok(actual_size_bytes) => actual_size_bytes,
        Err(_) => {
            logs.push(log_for(
                REMOTE_EVIDENCE_BODY_QUARANTINED_EVENT,
                reference,
                MeshRemoteEvidenceFetchStatus::Quarantined,
                "fetched_body_size_exceeds_policy",
            ));
            return plan_with(
                reference,
                MeshRemoteEvidenceFetchStatus::Quarantined,
                false,
                None,
                None,
                vec![degraded_codes::BODY_SIZE_EXCEEDS_POLICY],
                logs,
                "fetched_body_size_exceeds_policy",
            );
        }
    };
    if actual_size_bytes > input.policy.max_bytes {
        logs.push(log_for(
            REMOTE_EVIDENCE_BODY_QUARANTINED_EVENT,
            reference,
            MeshRemoteEvidenceFetchStatus::Quarantined,
            "fetched_body_size_exceeds_policy",
        ));
        return plan_with(
            reference,
            MeshRemoteEvidenceFetchStatus::Quarantined,
            false,
            None,
            Some(actual_size_bytes),
            vec![degraded_codes::BODY_SIZE_EXCEEDS_POLICY],
            logs,
            "fetched_body_size_exceeds_policy",
        );
    }
    if reference
        .size_bytes
        .is_some_and(|declared_size_bytes| declared_size_bytes != actual_size_bytes)
    {
        logs.push(log_for(
            REMOTE_EVIDENCE_BODY_QUARANTINED_EVENT,
            reference,
            MeshRemoteEvidenceFetchStatus::Quarantined,
            "declared_size_mismatch",
        ));
        return plan_with(
            reference,
            MeshRemoteEvidenceFetchStatus::Quarantined,
            false,
            None,
            Some(actual_size_bytes),
            vec![degraded_codes::DECLARED_SIZE_MISMATCH],
            logs,
            "declared_size_mismatch",
        );
    }
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
            Some(actual_size_bytes),
            Vec::new(),
            logs,
            "content_hash_verified",
        )
    } else {
        logs.push(log_for(
            REMOTE_EVIDENCE_BODY_QUARANTINED_EVENT,
            reference,
            MeshRemoteEvidenceFetchStatus::HashMismatch,
            "content_hash_mismatch",
        ));
        plan_with(
            reference,
            MeshRemoteEvidenceFetchStatus::HashMismatch,
            false,
            Some(actual_hash),
            Some(actual_size_bytes),
            vec![degraded_codes::FETCHED_BODY_HASH_MISMATCH],
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
    enforce_declared_size_preflight: bool,
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
    if let Some(size_bytes) = reference
        .size_bytes
        .filter(|_| enforce_declared_size_preflight)
    {
        if size_bytes > policy.max_bytes {
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
    actual_size_bytes: Option<u64>,
    degraded_codes: Vec<&'static str>,
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
        declared_size_bytes: reference.size_bytes,
        actual_size_bytes,
        degraded_codes,
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
        MeshRemoteEvidenceFetchStatus::Quarantined => format!(
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
    use std::io::Cursor;

    use super::*;

    #[test]
    fn bounded_stream_accepts_exact_cap_and_never_writes_probe_bytes() {
        let body = b"exactly";
        let mut reader = Cursor::new(body);
        let mut staged = Vec::new();

        let copied = copy_remote_evidence_body_bounded(
            &mut reader,
            &mut staged,
            u64::try_from(body.len()).expect("fixture length fits u64"),
        )
        .expect("exact-cap body should pass");

        assert_eq!(copied, body.len() as u64);
        assert_eq!(staged, body);
        assert_eq!(reader.position(), body.len() as u64);
    }

    #[test]
    fn bounded_stream_stops_at_cap_plus_one_without_writing_overflow() {
        let body = b"12345";
        let mut reader = Cursor::new(body);
        let mut staged = Vec::new();

        let error = copy_remote_evidence_body_bounded(&mut reader, &mut staged, 4)
            .expect_err("cap+1 body must fail");

        assert_eq!(error.code(), degraded_codes::BODY_SIZE_EXCEEDS_POLICY);
        assert!(matches!(
            error,
            MeshRemoteEvidenceStreamError::SizeLimitExceeded { max_bytes: 4 }
        ));
        assert_eq!(reader.position(), 5);
        assert_eq!(staged, b"1234");
    }

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
