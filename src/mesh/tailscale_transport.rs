//! SRR6.9 optional Tailscale transport adapter.
//!
//! Tailscale reachability is only the outer transport. This module owns the
//! deterministic ee frame contract layered on top of it: bounded JSON frames,
//! app-layer keyed signatures, capability allowlists, and fail-closed decode
//! behavior. It intentionally contains no listener, socket, HTTP stack, or
//! runtime dependency.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub const TAILSCALE_TRANSPORT_FRAME_SCHEMA_V1: &str = "ee.mesh.tailscale_transport_frame.v1";
pub const TAILSCALE_TRANSPORT_DEFAULT_MAX_FRAME_BYTES: usize = 64 * 1024;
pub const TAILSCALE_TRANSPORT_DEFAULT_MAX_PAYLOAD_BYTES: usize = 32 * 1024;
pub const TAILSCALE_TRANSPORT_DEFAULT_MAX_ELAPSED_BUDGET_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TailscaleTransportCapability {
    Hello,
    Summary,
    EventFetch,
    BodyFetch,
}

impl TailscaleTransportCapability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Summary => "summary",
            Self::EventFetch => "event_fetch",
            Self::BodyFetch => "body_fetch",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TailscaleTransportFrameKind {
    Request,
    Response,
    Error,
}

impl TailscaleTransportFrameKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TailscaleTransportBudgets {
    pub max_frame_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_elapsed_budget_ms: u64,
}

impl TailscaleTransportBudgets {
    #[must_use]
    pub const fn conservative_default() -> Self {
        Self {
            max_frame_bytes: TAILSCALE_TRANSPORT_DEFAULT_MAX_FRAME_BYTES,
            max_payload_bytes: TAILSCALE_TRANSPORT_DEFAULT_MAX_PAYLOAD_BYTES,
            max_elapsed_budget_ms: TAILSCALE_TRANSPORT_DEFAULT_MAX_ELAPSED_BUDGET_MS,
        }
    }
}

impl Default for TailscaleTransportBudgets {
    fn default() -> Self {
        Self::conservative_default()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct TailscaleTransportSigningKey([u8; 32]);

impl TailscaleTransportSigningKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for TailscaleTransportSigningKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TailscaleTransportSigningKey(<redacted>)")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleTransportPeerGrant {
    pub node_key: String,
    pub allowed_capabilities: BTreeSet<TailscaleTransportCapability>,
    pub signing_key: TailscaleTransportSigningKey,
}

impl TailscaleTransportPeerGrant {
    #[must_use]
    pub fn new(
        node_key: impl Into<String>,
        allowed_capabilities: impl IntoIterator<Item = TailscaleTransportCapability>,
        signing_key: TailscaleTransportSigningKey,
    ) -> Self {
        Self {
            node_key: node_key.into(),
            allowed_capabilities: allowed_capabilities.into_iter().collect(),
            signing_key,
        }
    }

    #[must_use]
    pub fn allows(&self, capability: TailscaleTransportCapability) -> bool {
        self.allowed_capabilities.contains(&capability)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TailscaleTransportAllowlist {
    grants: BTreeMap<String, TailscaleTransportPeerGrant>,
}

impl TailscaleTransportAllowlist {
    #[must_use]
    pub fn new(grants: impl IntoIterator<Item = TailscaleTransportPeerGrant>) -> Self {
        let grants = grants
            .into_iter()
            .map(|grant| (grant.node_key.clone(), grant))
            .collect();
        Self { grants }
    }

    #[must_use]
    pub fn grant_for(&self, node_key: &str) -> Option<&TailscaleTransportPeerGrant> {
        self.grants.get(node_key)
    }

    pub fn insert(&mut self, grant: TailscaleTransportPeerGrant) {
        self.grants.insert(grant.node_key.clone(), grant);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TailscaleTransportFrameDraft {
    pub frame_id: String,
    pub request_id: String,
    pub kind: TailscaleTransportFrameKind,
    pub source_node_key: String,
    pub target_node_key: String,
    pub capability: TailscaleTransportCapability,
    pub elapsed_budget_ms: u64,
    pub payload: JsonValue,
}

impl TailscaleTransportFrameDraft {
    #[must_use]
    pub fn request(
        frame_id: impl Into<String>,
        request_id: impl Into<String>,
        source_node_key: impl Into<String>,
        target_node_key: impl Into<String>,
        capability: TailscaleTransportCapability,
        elapsed_budget_ms: u64,
        payload: JsonValue,
    ) -> Self {
        Self {
            frame_id: frame_id.into(),
            request_id: request_id.into(),
            kind: TailscaleTransportFrameKind::Request,
            source_node_key: source_node_key.into(),
            target_node_key: target_node_key.into(),
            capability,
            elapsed_budget_ms,
            payload,
        }
    }

    #[must_use]
    pub fn response(
        frame_id: impl Into<String>,
        request_id: impl Into<String>,
        source_node_key: impl Into<String>,
        target_node_key: impl Into<String>,
        capability: TailscaleTransportCapability,
        elapsed_budget_ms: u64,
        payload: JsonValue,
    ) -> Self {
        Self {
            frame_id: frame_id.into(),
            request_id: request_id.into(),
            kind: TailscaleTransportFrameKind::Response,
            source_node_key: source_node_key.into(),
            target_node_key: target_node_key.into(),
            capability,
            elapsed_budget_ms,
            payload,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleTransportFrame {
    pub schema: String,
    pub frame_id: String,
    pub request_id: String,
    pub kind: TailscaleTransportFrameKind,
    pub source_node_key: String,
    pub target_node_key: String,
    pub capability: TailscaleTransportCapability,
    pub elapsed_budget_ms: u64,
    pub payload: JsonValue,
    pub payload_hash: String,
    pub signature: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedTailscaleTransportFrame {
    pub frame: TailscaleTransportFrame,
    pub peer_node_key: String,
    pub capability: TailscaleTransportCapability,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TailscaleTransportError {
    MalformedFrame {
        reason: &'static str,
    },
    SchemaMismatch {
        observed: String,
    },
    FrameTooLarge {
        actual_bytes: usize,
        budget_bytes: usize,
    },
    PayloadTooLarge {
        actual_bytes: usize,
        budget_bytes: usize,
    },
    BudgetDenied {
        actual_ms: u64,
        budget_ms: u64,
    },
    PayloadHashMismatch {
        expected: String,
        observed: String,
    },
    UnknownPeer {
        node_key: String,
    },
    CapabilityDenied {
        node_key: String,
        capability: TailscaleTransportCapability,
    },
    SignatureMismatch {
        node_key: String,
    },
    Json {
        message: String,
    },
}

impl std::fmt::Display for TailscaleTransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedFrame { reason } => write!(formatter, "malformed frame: {reason}"),
            Self::SchemaMismatch { observed } => {
                write!(
                    formatter,
                    "unsupported tailscale transport schema: {observed}"
                )
            }
            Self::FrameTooLarge {
                actual_bytes,
                budget_bytes,
            } => write!(
                formatter,
                "frame is {actual_bytes} bytes; budget is {budget_bytes} bytes"
            ),
            Self::PayloadTooLarge {
                actual_bytes,
                budget_bytes,
            } => write!(
                formatter,
                "payload is {actual_bytes} bytes; budget is {budget_bytes} bytes"
            ),
            Self::BudgetDenied {
                actual_ms,
                budget_ms,
            } => write!(
                formatter,
                "elapsed budget is {actual_ms}ms; maximum is {budget_ms}ms"
            ),
            Self::PayloadHashMismatch { expected, observed } => write!(
                formatter,
                "payload hash mismatch: expected {expected}, observed {observed}"
            ),
            Self::UnknownPeer { node_key } => write!(formatter, "unknown peer denied: {node_key}"),
            Self::CapabilityDenied {
                node_key,
                capability,
            } => write!(
                formatter,
                "peer {node_key} is not allowed to use capability {}",
                capability.as_str()
            ),
            Self::SignatureMismatch { node_key } => {
                write!(formatter, "frame signature mismatch for peer {node_key}")
            }
            Self::Json { message } => write!(formatter, "json error: {message}"),
        }
    }
}

impl std::error::Error for TailscaleTransportError {}

pub fn sign_frame(
    draft: TailscaleTransportFrameDraft,
    signing_key: &TailscaleTransportSigningKey,
    budgets: TailscaleTransportBudgets,
) -> Result<TailscaleTransportFrame, TailscaleTransportError> {
    validate_nonempty(&draft.frame_id, "frameId")?;
    validate_nonempty(&draft.request_id, "requestId")?;
    validate_nonempty(&draft.source_node_key, "sourceNodeKey")?;
    validate_nonempty(&draft.target_node_key, "targetNodeKey")?;
    validate_elapsed_budget(draft.elapsed_budget_ms, budgets)?;

    let payload_hash = payload_hash(&draft.payload, budgets)?;
    let mut frame = TailscaleTransportFrame {
        schema: TAILSCALE_TRANSPORT_FRAME_SCHEMA_V1.to_owned(),
        frame_id: draft.frame_id,
        request_id: draft.request_id,
        kind: draft.kind,
        source_node_key: draft.source_node_key,
        target_node_key: draft.target_node_key,
        capability: draft.capability,
        elapsed_budget_ms: draft.elapsed_budget_ms,
        payload: draft.payload,
        payload_hash,
        signature: String::new(),
    };
    frame.signature = signature_for_frame(&frame, signing_key)?;
    ensure_frame_size(&frame, budgets)?;
    Ok(frame)
}

pub fn encode_frame(
    frame: &TailscaleTransportFrame,
    budgets: TailscaleTransportBudgets,
) -> Result<Vec<u8>, TailscaleTransportError> {
    validate_frame_integrity(frame, budgets)?;
    serde_json::to_vec(frame).map_err(json_error)
}

pub fn decode_frame(
    bytes: &[u8],
    budgets: TailscaleTransportBudgets,
) -> Result<TailscaleTransportFrame, TailscaleTransportError> {
    if bytes.len() > budgets.max_frame_bytes {
        return Err(TailscaleTransportError::FrameTooLarge {
            actual_bytes: bytes.len(),
            budget_bytes: budgets.max_frame_bytes,
        });
    }
    let frame: TailscaleTransportFrame =
        serde_json::from_slice(bytes).map_err(|_| TailscaleTransportError::MalformedFrame {
            reason: "invalid_json",
        })?;
    validate_frame_integrity(&frame, budgets)?;
    Ok(frame)
}

pub fn verify_frame(
    frame: &TailscaleTransportFrame,
    allowlist: &TailscaleTransportAllowlist,
    budgets: TailscaleTransportBudgets,
) -> Result<VerifiedTailscaleTransportFrame, TailscaleTransportError> {
    validate_frame_integrity(frame, budgets)?;
    let grant = allowlist.grant_for(&frame.source_node_key).ok_or_else(|| {
        TailscaleTransportError::UnknownPeer {
            node_key: frame.source_node_key.clone(),
        }
    })?;
    if !grant.allows(frame.capability) {
        return Err(TailscaleTransportError::CapabilityDenied {
            node_key: frame.source_node_key.clone(),
            capability: frame.capability,
        });
    }
    let expected_signature = signature_for_frame(frame, &grant.signing_key)?;
    if !constant_time_eq(frame.signature.as_bytes(), expected_signature.as_bytes()) {
        return Err(TailscaleTransportError::SignatureMismatch {
            node_key: frame.source_node_key.clone(),
        });
    }
    Ok(VerifiedTailscaleTransportFrame {
        frame: frame.clone(),
        peer_node_key: grant.node_key.clone(),
        capability: frame.capability,
    })
}

fn validate_frame_integrity(
    frame: &TailscaleTransportFrame,
    budgets: TailscaleTransportBudgets,
) -> Result<(), TailscaleTransportError> {
    if frame.schema != TAILSCALE_TRANSPORT_FRAME_SCHEMA_V1 {
        return Err(TailscaleTransportError::SchemaMismatch {
            observed: frame.schema.clone(),
        });
    }
    validate_nonempty(&frame.frame_id, "frameId")?;
    validate_nonempty(&frame.request_id, "requestId")?;
    validate_nonempty(&frame.source_node_key, "sourceNodeKey")?;
    validate_nonempty(&frame.target_node_key, "targetNodeKey")?;
    validate_nonempty(&frame.signature, "signature")?;
    validate_elapsed_budget(frame.elapsed_budget_ms, budgets)?;
    let observed = payload_hash(&frame.payload, budgets)?;
    if observed != frame.payload_hash {
        return Err(TailscaleTransportError::PayloadHashMismatch {
            expected: frame.payload_hash.clone(),
            observed,
        });
    }
    ensure_frame_size(frame, budgets)
}

fn validate_nonempty(value: &str, field: &'static str) -> Result<(), TailscaleTransportError> {
    if value.trim().is_empty() {
        return Err(TailscaleTransportError::MalformedFrame { reason: field });
    }
    Ok(())
}

fn validate_elapsed_budget(
    elapsed_budget_ms: u64,
    budgets: TailscaleTransportBudgets,
) -> Result<(), TailscaleTransportError> {
    if elapsed_budget_ms == 0 || elapsed_budget_ms > budgets.max_elapsed_budget_ms {
        return Err(TailscaleTransportError::BudgetDenied {
            actual_ms: elapsed_budget_ms,
            budget_ms: budgets.max_elapsed_budget_ms,
        });
    }
    Ok(())
}

fn payload_hash(
    payload: &JsonValue,
    budgets: TailscaleTransportBudgets,
) -> Result<String, TailscaleTransportError> {
    let payload_bytes = serde_json::to_vec(payload).map_err(json_error)?;
    if payload_bytes.len() > budgets.max_payload_bytes {
        return Err(TailscaleTransportError::PayloadTooLarge {
            actual_bytes: payload_bytes.len(),
            budget_bytes: budgets.max_payload_bytes,
        });
    }
    Ok(format!("blake3:{}", blake3::hash(&payload_bytes).to_hex()))
}

fn ensure_frame_size(
    frame: &TailscaleTransportFrame,
    budgets: TailscaleTransportBudgets,
) -> Result<(), TailscaleTransportError> {
    let frame_bytes = serde_json::to_vec(frame).map_err(json_error)?;
    if frame_bytes.len() > budgets.max_frame_bytes {
        return Err(TailscaleTransportError::FrameTooLarge {
            actual_bytes: frame_bytes.len(),
            budget_bytes: budgets.max_frame_bytes,
        });
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignatureMaterial<'a> {
    schema: &'a str,
    frame_id: &'a str,
    request_id: &'a str,
    kind: TailscaleTransportFrameKind,
    source_node_key: &'a str,
    target_node_key: &'a str,
    capability: TailscaleTransportCapability,
    elapsed_budget_ms: u64,
    payload_hash: &'a str,
}

fn signature_for_frame(
    frame: &TailscaleTransportFrame,
    signing_key: &TailscaleTransportSigningKey,
) -> Result<String, TailscaleTransportError> {
    let material = SignatureMaterial {
        schema: &frame.schema,
        frame_id: &frame.frame_id,
        request_id: &frame.request_id,
        kind: frame.kind,
        source_node_key: &frame.source_node_key,
        target_node_key: &frame.target_node_key,
        capability: frame.capability,
        elapsed_budget_ms: frame.elapsed_budget_ms,
        payload_hash: &frame.payload_hash,
    };
    let bytes = serde_json::to_vec(&material).map_err(json_error)?;
    Ok(format!(
        "blake3-keyed:{}",
        blake3::keyed_hash(&signing_key.0, &bytes).to_hex()
    ))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (l_byte, r_byte) in left.iter().zip(right.iter()) {
        diff |= l_byte ^ r_byte;
    }
    std::hint::black_box(diff) == 0
}

fn json_error(error: serde_json::Error) -> TailscaleTransportError {
    TailscaleTransportError::Json {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const PEER_NODE: &str = "nodekey:peer";
    const LOCAL_NODE: &str = "nodekey:local";

    fn key(byte: u8) -> TailscaleTransportSigningKey {
        TailscaleTransportSigningKey::from_bytes([byte; 32])
    }

    fn allowlist() -> TailscaleTransportAllowlist {
        TailscaleTransportAllowlist::new([TailscaleTransportPeerGrant::new(
            PEER_NODE,
            [
                TailscaleTransportCapability::Hello,
                TailscaleTransportCapability::Summary,
                TailscaleTransportCapability::EventFetch,
            ],
            key(7),
        )])
    }

    fn summary_request() -> TailscaleTransportFrameDraft {
        TailscaleTransportFrameDraft::request(
            "frame-1",
            "request-1",
            PEER_NODE,
            LOCAL_NODE,
            TailscaleTransportCapability::Summary,
            1_000,
            json!({
                "schema": "ee.mesh.summary_request.v1",
                "workspaceId": "wsp_test",
                "sinceEventId": "evt_001"
            }),
        )
    }

    #[test]
    fn signed_request_round_trips_for_allowed_peer_and_capability() {
        let budgets = TailscaleTransportBudgets::default();
        let frame = sign_frame(summary_request(), &key(7), budgets).expect("sign frame");
        let encoded = encode_frame(&frame, budgets).expect("encode frame");
        let decoded = decode_frame(&encoded, budgets).expect("decode frame");
        let verified = verify_frame(&decoded, &allowlist(), budgets).expect("verify frame");

        assert_eq!(verified.peer_node_key, PEER_NODE);
        assert_eq!(verified.capability, TailscaleTransportCapability::Summary);
        assert_eq!(verified.frame.kind, TailscaleTransportFrameKind::Request);
        assert!(verified.frame.payload_hash.starts_with("blake3:"));
        assert!(verified.frame.signature.starts_with("blake3-keyed:"));
    }

    #[test]
    fn body_fetch_response_uses_same_signed_frame_contract() {
        let budgets = TailscaleTransportBudgets::default();
        let grant = TailscaleTransportPeerGrant::new(
            PEER_NODE,
            [TailscaleTransportCapability::BodyFetch],
            key(9),
        );
        let allowlist = TailscaleTransportAllowlist::new([grant]);
        let draft = TailscaleTransportFrameDraft::response(
            "frame-body-1",
            "request-body-1",
            PEER_NODE,
            LOCAL_NODE,
            TailscaleTransportCapability::BodyFetch,
            2_000,
            json!({
                "schema": "ee.mesh.body_fetch.response.v1",
                "eventId": "evt_body",
                "body": "redacted body material"
            }),
        );
        let frame = sign_frame(draft, &key(9), budgets).expect("sign frame");
        let verified = verify_frame(&frame, &allowlist, budgets).expect("verify frame");

        assert_eq!(verified.frame.kind, TailscaleTransportFrameKind::Response);
        assert_eq!(verified.capability, TailscaleTransportCapability::BodyFetch);
    }

    #[test]
    fn event_fetch_request_uses_same_signed_frame_contract() {
        let budgets = TailscaleTransportBudgets::default();
        let draft = TailscaleTransportFrameDraft::request(
            "frame-event-1",
            "request-event-1",
            PEER_NODE,
            LOCAL_NODE,
            TailscaleTransportCapability::EventFetch,
            1_500,
            json!({
                "schema": "ee.mesh.event_fetch.request.v1",
                "fromEventId": "evt_001",
                "limit": 64
            }),
        );
        let frame = sign_frame(draft, &key(7), budgets).expect("sign frame");
        let verified = verify_frame(&frame, &allowlist(), budgets).expect("verify frame");

        assert_eq!(verified.frame.kind, TailscaleTransportFrameKind::Request);
        assert_eq!(
            verified.capability,
            TailscaleTransportCapability::EventFetch
        );
    }

    #[test]
    fn unknown_peer_fails_closed() {
        let budgets = TailscaleTransportBudgets::default();
        let mut draft = summary_request();
        draft.source_node_key = "nodekey:unknown".to_owned();
        let frame = sign_frame(draft, &key(7), budgets).expect("sign frame");
        let err = verify_frame(&frame, &allowlist(), budgets).expect_err("unknown peer denied");

        assert_eq!(
            err,
            TailscaleTransportError::UnknownPeer {
                node_key: "nodekey:unknown".to_owned()
            }
        );
    }

    #[test]
    fn denied_capability_fails_closed() {
        let budgets = TailscaleTransportBudgets::default();
        let draft = TailscaleTransportFrameDraft::request(
            "frame-body-denied",
            "request-body-denied",
            PEER_NODE,
            LOCAL_NODE,
            TailscaleTransportCapability::BodyFetch,
            1_000,
            json!({"schema": "ee.mesh.body_fetch.request.v1", "eventId": "evt_001"}),
        );
        let frame = sign_frame(draft, &key(7), budgets).expect("sign frame");
        let err = verify_frame(&frame, &allowlist(), budgets).expect_err("capability denied");

        assert_eq!(
            err,
            TailscaleTransportError::CapabilityDenied {
                node_key: PEER_NODE.to_owned(),
                capability: TailscaleTransportCapability::BodyFetch,
            }
        );
    }

    #[test]
    fn malformed_json_is_rejected() {
        let budgets = TailscaleTransportBudgets::default();
        let err = decode_frame(b"{not json", budgets).expect_err("malformed json denied");

        assert_eq!(
            err,
            TailscaleTransportError::MalformedFrame {
                reason: "invalid_json"
            }
        );
    }

    #[test]
    fn frame_and_payload_budgets_are_enforced() {
        let tiny_payload_budget = TailscaleTransportBudgets {
            max_frame_bytes: 128,
            max_payload_bytes: 16,
            max_elapsed_budget_ms: 1_000,
        };
        let payload_err =
            sign_frame(summary_request(), &key(7), tiny_payload_budget).expect_err("payload limit");
        assert!(matches!(
            payload_err,
            TailscaleTransportError::PayloadTooLarge { .. }
        ));

        let frame = sign_frame(
            summary_request(),
            &key(7),
            TailscaleTransportBudgets::default(),
        )
        .expect("sign frame");
        let encoded = serde_json::to_vec(&frame).expect("encode json");
        let frame_err = decode_frame(
            &encoded,
            TailscaleTransportBudgets {
                max_frame_bytes: 64,
                max_payload_bytes: 32 * 1024,
                max_elapsed_budget_ms: 1_000,
            },
        )
        .expect_err("frame limit");
        assert!(matches!(
            frame_err,
            TailscaleTransportError::FrameTooLarge { .. }
        ));
    }

    #[test]
    fn tampered_payload_fails_before_capability_use() {
        let budgets = TailscaleTransportBudgets::default();
        let mut frame = sign_frame(summary_request(), &key(7), budgets).expect("sign frame");
        frame.payload = json!({"schema": "ee.mesh.summary_request.v1", "tampered": true});
        let err = verify_frame(&frame, &allowlist(), budgets).expect_err("tampering denied");

        assert!(matches!(
            err,
            TailscaleTransportError::PayloadHashMismatch { .. }
        ));
    }

    #[test]
    fn tampered_signature_fails_closed() {
        let budgets = TailscaleTransportBudgets::default();
        let mut frame = sign_frame(summary_request(), &key(7), budgets).expect("sign frame");
        frame.signature = "blake3-keyed:forged".to_owned();
        let err = verify_frame(&frame, &allowlist(), budgets).expect_err("signature denied");

        assert_eq!(
            err,
            TailscaleTransportError::SignatureMismatch {
                node_key: PEER_NODE.to_owned()
            }
        );
    }

    #[test]
    fn zero_or_overlarge_time_budget_is_denied() {
        let budgets = TailscaleTransportBudgets {
            max_frame_bytes: TAILSCALE_TRANSPORT_DEFAULT_MAX_FRAME_BYTES,
            max_payload_bytes: TAILSCALE_TRANSPORT_DEFAULT_MAX_PAYLOAD_BYTES,
            max_elapsed_budget_ms: 50,
        };
        let mut draft = summary_request();
        draft.elapsed_budget_ms = 0;
        let zero_err = sign_frame(draft, &key(7), budgets).expect_err("zero budget denied");
        assert_eq!(
            zero_err,
            TailscaleTransportError::BudgetDenied {
                actual_ms: 0,
                budget_ms: 50,
            }
        );

        let mut draft = summary_request();
        draft.elapsed_budget_ms = 51;
        let large_err = sign_frame(draft, &key(7), budgets).expect_err("large budget denied");
        assert_eq!(
            large_err,
            TailscaleTransportError::BudgetDenied {
                actual_ms: 51,
                budget_ms: 50,
            }
        );
    }
}
