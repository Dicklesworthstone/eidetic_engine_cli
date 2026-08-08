//! T2.1 frame-transport session layer: frame v2 + replay-safe directional
//! sessions (`bd-tc-epic-qzk7o.3.2`, ADR 0086 TC-D1/TC-D5).
//!
//! Supersedes the dead `ee.mesh.tailscale_transport_frame.v1` codec **before
//! its first production caller**: v1 encoded rotating Tailscale node keys as
//! durable identity and MAC'd frames under the long-term pair key. Frame v2
//! binds random ee node IDs, team, both endpoint workspace IDs, session,
//! direction, and an exact-next per-direction u64 counter, and authenticates
//! every frame under a **directional session key** — the long-term pair key
//! never MACs application frames.
//!
//! Canonical derivation contexts (pinned here, golden-vectored below; ADR
//! 0086 TC-D5 specifies the properties but deliberately left the context
//! strings to this module):
//!
//! - transcript: [`SESSION_TRANSCRIPT_CONTEXT`] over the length-prefixed
//!   session transcript ([`SessionBinding::transcript_bytes`])
//! - directional keys: [`SESSION_KEY_I2R_CONTEXT`] /
//!   [`SESSION_KEY_R2I_CONTEXT`] over `lp(pair_key) || lp(transcript_hash)`
//!
//! The MAC preimage is versioned, type-tagged, and length-prefixed
//! ([`FrameV2::mac_preimage`]); JSON field order is never authenticated.
//! Ordered TCP supplies delivery order, so v2 has **no replay window**: each
//! receiver accepts exactly the next per-direction counter and any duplicate,
//! skipped, or regressed counter closes the session. Application retries use
//! a fresh frame + counter with the same idempotency key, never replayed
//! frame bytes.
//!
//! Sessions are established by the fresh-nonce handshake in this module
//! ([`InitiatorHandshake`] / [`responder_accept_open`]): three bounded
//! messages prove pair-key possession via role-bound confirmation MACs —
//! additionally binding the pair-key generation and both current Tailscale
//! node-key observations — before either side derives directional keys.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{Shutdown, SocketAddr};
use std::sync::atomic::{Ordering, compiler_fence};
use std::time::Duration;

use asupersync::io::{AsyncReadExt, AsyncWriteExt};
use asupersync::net::TcpStream;
use asupersync::time::{timeout, wall_now};
use asupersync::Cx;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::config::{EnvVar, read_env_var};
use crate::mesh::key_store::SecretBytes;

/// Frame v2 schema identifier.
pub const TRANSPORT_FRAME_SCHEMA_V2: &str = "ee.mesh.tailscale_transport_frame.v2";

/// The dead v1 schema. Rejected outright; kept only so the rejection path can
/// name what it refused.
pub const TRANSPORT_FRAME_SCHEMA_V1: &str = "ee.mesh.tailscale_transport_frame.v1";

/// Hard outer limit on one encoded frame (unchanged from v1's scaffolding).
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Hard outer limit on one frame payload (unchanged from v1's scaffolding).
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024;

/// Version tag leading every session transcript (inside the hashed bytes).
pub const SESSION_TRANSCRIPT_TAG: &str = "ee.mesh.session_transcript.v2";

/// BLAKE3 `derive_key` context for hashing the session transcript.
pub const SESSION_TRANSCRIPT_CONTEXT: &str = "ee.team.session.transcript.v1";

/// BLAKE3 `derive_key` context for the initiator→responder session key.
pub const SESSION_KEY_I2R_CONTEXT: &str = "ee.team.session.i2r.v1";

/// BLAKE3 `derive_key` context for the responder→initiator session key.
pub const SESSION_KEY_R2I_CONTEXT: &str = "ee.team.session.r2i.v1";

/// Version tag leading every frame MAC preimage.
pub const FRAME_MAC_PREIMAGE_TAG: &str = "ee.mesh.frame_mac_preimage.v2";

/// Payload cap for the negotiated `pair_rotate` control extension (M2).
pub const PAIR_ROTATE_MAX_PAYLOAD_BYTES: usize = 4096;

/// Payload cap for the negotiated token-free `identity_attest` extension (M6).
pub const IDENTITY_ATTEST_MAX_PAYLOAD_BYTES: usize = 8192;

// Known-answer vectors captured from the BLAKE3 reference implementation
// (`b3sum`) over the fixed KAT binding in the tests below. They pin the
// derivation contexts, the transcript layout, the preimage layout, and the
// keyed-MAC wiring; an accidental edit to any of them fails the KAT test.
#[cfg(test)]
const KAT_TRANSCRIPT_HASH_HEX: &str =
    "7c9fc90baec20563de192e24b11246eb1965b3d95a9d66a2e9e34d252d3e571e";
#[cfg(test)]
const KAT_I2R_HEX: &str = "a2124625eaf2ff02018622ad52c592168e5d90e6465013caa005f1269a9a6915";
#[cfg(test)]
const KAT_R2I_HEX: &str = "3791d4c418b7bc7b6525c4eab9f2126b07fec5875fd7afd66d09c1b4b209650a";
#[cfg(test)]
const KAT_FRAME_MAC_HEX: &str = "126228fbdff27bb7a9a34f451bc91da3de49a9a6a3782a18cf3fa33ec6c3f1d7";

/// Direction of a frame inside an established session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDirection {
    /// Frames the session initiator sends to the responder.
    InitiatorToResponder,
    /// Frames the responder sends back to the initiator.
    ResponderToInitiator,
}

impl SessionDirection {
    /// Stable wire token for this direction.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::InitiatorToResponder => "initiator_to_responder",
            Self::ResponderToInitiator => "responder_to_initiator",
        }
    }
}

/// Frame kind: request or correlated response.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    /// A request frame.
    Request,
    /// A response frame correlated to a prior request.
    Response,
}

impl FrameKind {
    /// Stable wire token for this kind.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

/// Capabilities carried by v2 frames. M1 wires the four base capabilities;
/// extensions are version-negotiated and rejected unless explicitly
/// negotiated for the session ([`NegotiatedExtensions`]).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameCapability {
    /// Post-enrollment hello / liveness.
    Hello,
    /// Anti-entropy tip/summary exchange.
    Summary,
    /// Signed origin-event fetch.
    EventFetch,
    /// Policy-gated body fetch.
    BodyFetch,
    /// A negotiated extension capability (e.g. `pair_rotate`,
    /// `identity_attest`). Unknown or un-negotiated extensions fail closed.
    #[serde(untagged)]
    Extension(String),
}

impl FrameCapability {
    /// Stable wire token for this capability.
    #[must_use]
    pub fn token(&self) -> &str {
        match self {
            Self::Hello => "hello",
            Self::Summary => "summary",
            Self::EventFetch => "event_fetch",
            Self::BodyFetch => "body_fetch",
            Self::Extension(name) => name,
        }
    }
}

/// Extensions the two endpoints negotiated for this session. Base
/// capabilities are always dispatchable; extensions must be listed here or
/// the frame is refused before capability dispatch.
#[derive(Clone, Debug, Default)]
pub struct NegotiatedExtensions {
    names: Vec<String>,
}

impl NegotiatedExtensions {
    /// No extensions negotiated (the M1 default).
    #[must_use]
    pub const fn none() -> Self {
        Self { names: Vec::new() }
    }

    /// Build from negotiated extension names.
    #[must_use]
    pub fn from_names(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            names: names.into_iter().collect(),
        }
    }

    /// Whether `name` was negotiated.
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        self.names.iter().any(|candidate| candidate == name)
    }
}

/// Immutable identity a session was established under. Both endpoints
/// reconstruct this locally during the handshake; nothing here is trusted
/// from the wire after establishment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionBinding {
    /// Team the session belongs to.
    pub team_id: String,
    /// Random ee node ID of the initiator.
    pub initiator_node_id: String,
    /// Random ee node ID of the responder.
    pub responder_node_id: String,
    /// Initiator's locally selected endpoint workspace.
    pub initiator_workspace_id: String,
    /// The exact registered responder target workspace.
    pub responder_workspace_id: String,
    /// Pinned Tailscale stable node ID of the initiator.
    pub initiator_stable_id: String,
    /// Pinned Tailscale stable node ID of the responder.
    pub responder_stable_id: String,
    /// Unique session identifier minted during the handshake.
    pub session_id: String,
}

impl SessionBinding {
    /// Canonical length-prefixed transcript bytes hashed into the session
    /// transcript. Layout (every field `lp`-prefixed, u32-LE lengths):
    /// tag, team, initiator/responder node IDs, initiator/responder
    /// workspace IDs, initiator/responder stable IDs, session ID, then both
    /// fresh handshake nonces.
    #[must_use]
    pub fn transcript_bytes(
        &self,
        initiator_nonce: &[u8; 32],
        responder_nonce: &[u8; 32],
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        push_lp(&mut out, SESSION_TRANSCRIPT_TAG.as_bytes());
        push_lp(&mut out, self.team_id.as_bytes());
        push_lp(&mut out, self.initiator_node_id.as_bytes());
        push_lp(&mut out, self.responder_node_id.as_bytes());
        push_lp(&mut out, self.initiator_workspace_id.as_bytes());
        push_lp(&mut out, self.responder_workspace_id.as_bytes());
        push_lp(&mut out, self.initiator_stable_id.as_bytes());
        push_lp(&mut out, self.responder_stable_id.as_bytes());
        push_lp(&mut out, self.session_id.as_bytes());
        push_lp(&mut out, initiator_nonce);
        push_lp(&mut out, responder_nonce);
        out
    }
}

/// The two directional session keys derived from one handshake.
pub struct DirectionalSessionKeys {
    /// Key authenticating initiator→responder frames.
    pub initiator_to_responder: SecretBytes,
    /// Key authenticating responder→initiator frames.
    pub responder_to_initiator: SecretBytes,
}

impl DirectionalSessionKeys {
    /// The key for frames traveling in `direction`.
    #[must_use]
    pub const fn for_direction(&self, direction: SessionDirection) -> &SecretBytes {
        match direction {
            SessionDirection::InitiatorToResponder => &self.initiator_to_responder,
            SessionDirection::ResponderToInitiator => &self.responder_to_initiator,
        }
    }
}

impl fmt::Debug for DirectionalSessionKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DirectionalSessionKeys(<redacted>)")
    }
}

/// Derive both directional session keys from the pair key, the session
/// binding, and both fresh handshake nonces. The long-term pair key itself
/// never authenticates frames.
#[must_use]
pub fn derive_session_keys(
    pair_key: &SecretBytes,
    binding: &SessionBinding,
    initiator_nonce: &[u8; 32],
    responder_nonce: &[u8; 32],
) -> DirectionalSessionKeys {
    let transcript = binding.transcript_bytes(initiator_nonce, responder_nonce);
    let transcript_hash = blake3::derive_key(SESSION_TRANSCRIPT_CONTEXT, &transcript);
    let mut material = Vec::with_capacity(2 * (4 + 32));
    push_lp(&mut material, pair_key.as_bytes());
    push_lp(&mut material, &transcript_hash);
    let keys = DirectionalSessionKeys {
        initiator_to_responder: SecretBytes::new(blake3::derive_key(
            SESSION_KEY_I2R_CONTEXT,
            &material,
        )),
        responder_to_initiator: SecretBytes::new(blake3::derive_key(
            SESSION_KEY_R2I_CONTEXT,
            &material,
        )),
    };
    material.fill(0);
    compiler_fence(Ordering::SeqCst);
    keys
}

/// A v2 transport frame as carried on the wire (length-prefixed JSON).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameV2 {
    /// Always [`TRANSPORT_FRAME_SCHEMA_V2`].
    pub schema: String,
    /// Random ee node ID of the sender.
    pub source_node_id: String,
    /// Random ee node ID of the receiver.
    pub target_node_id: String,
    /// Team the session belongs to.
    pub team_id: String,
    /// Endpoint workspace of the sender.
    pub source_workspace_id: String,
    /// Endpoint workspace of the receiver.
    pub target_workspace_id: String,
    /// Session identifier from the handshake.
    pub session_id: String,
    /// Direction this frame travels.
    pub direction: SessionDirection,
    /// Monotonic per-direction counter; receivers accept exactly the next.
    pub counter: u64,
    /// Request/response correlation identifier.
    pub correlation_id: String,
    /// Request or response.
    pub kind: FrameKind,
    /// Capability this frame invokes.
    pub capability: FrameCapability,
    /// Requested processing budget in milliseconds (bounded by receivers).
    pub requested_budget_ms: u64,
    /// Application payload (bounded by [`MAX_PAYLOAD_BYTES`]).
    pub payload: JsonValue,
    /// Lowercase hex BLAKE3 of the canonical payload bytes.
    pub payload_hash: String,
    /// Lowercase hex keyed-BLAKE3 MAC over [`FrameV2::mac_preimage`].
    pub mac: String,
}

/// A frame that passed every verification gate.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedFrameV2 {
    /// The verified frame.
    pub frame: FrameV2,
}

/// Everything needed to author one outbound frame.
#[derive(Clone, Debug)]
pub struct FrameDraft {
    /// Direction the frame travels.
    pub direction: SessionDirection,
    /// Per-direction counter value (the sender's next outbound counter).
    pub counter: u64,
    /// Correlation identifier.
    pub correlation_id: String,
    /// Request or response.
    pub kind: FrameKind,
    /// Capability invoked.
    pub capability: FrameCapability,
    /// Requested processing budget in milliseconds.
    pub requested_budget_ms: u64,
    /// Application payload.
    pub payload: JsonValue,
}

/// Why a counter was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterViolation {
    /// The immediately previous counter arrived again.
    Duplicate,
    /// A counter beyond the expected next arrived (a gap).
    Skipped,
    /// A counter earlier than the previous one arrived.
    Regressed,
    /// The receiver already accepted `u64::MAX`; no successor exists.
    Exhausted,
}

/// Fail-closed error surface for frame v2 verification. `degraded_code`
/// partitions the failures into the T2.1 registry codes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportSessionError {
    /// The dead v1 schema was presented (downgrade attempt).
    V1Rejected,
    /// An unknown schema was presented.
    SchemaMismatch {
        /// The schema the frame carried.
        observed: String,
    },
    /// The frame could not be decoded at all.
    MalformedFrame {
        /// Human-readable decode failure.
        message: String,
    },
    /// The encoded frame exceeds [`MAX_FRAME_BYTES`].
    FrameTooLarge {
        /// Observed encoded size.
        actual_bytes: usize,
    },
    /// The canonical payload exceeds its byte budget.
    PayloadTooLarge {
        /// Observed canonical payload size.
        actual_bytes: usize,
        /// The budget that applied (base or extension-specific).
        budget_bytes: usize,
    },
    /// A binding field does not match the established session (team, node,
    /// workspace, session, direction, or origin-as-target confusion).
    BindingMismatch {
        /// Which field mismatched.
        field: &'static str,
    },
    /// The per-direction counter was not exactly the next expected value.
    ReplayRejected {
        /// How the counter violated the discipline.
        violation: CounterViolation,
        /// The counter the receiver expected.
        expected: u64,
        /// The counter the frame carried.
        observed: u64,
    },
    /// The session was already closed by a prior violation.
    SessionClosed,
    /// The payload hash does not match the canonical payload bytes.
    PayloadHashMismatch,
    /// The MAC does not verify under the directional session key.
    MacMismatch,
    /// An extension capability was presented without negotiation.
    ExtensionNotNegotiated {
        /// The extension name.
        name: String,
    },
}

impl TransportSessionError {
    /// Stable degraded code for this failure (T2.1 registry).
    #[must_use]
    pub const fn degraded_code(&self) -> &'static str {
        match self {
            Self::BindingMismatch { .. } => "mesh_frame_target_mismatch",
            Self::ReplayRejected { .. } | Self::SessionClosed => "mesh_frame_replay_rejected",
            Self::V1Rejected
            | Self::SchemaMismatch { .. }
            | Self::MalformedFrame { .. }
            | Self::FrameTooLarge { .. }
            | Self::PayloadTooLarge { .. }
            | Self::PayloadHashMismatch
            | Self::MacMismatch
            | Self::ExtensionNotNegotiated { .. } => "mesh_frame_auth_failed",
        }
    }

    /// Human-readable message.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::V1Rejected => {
                "Mesh transport rejected a dead ee.mesh.tailscale_transport_frame.v1 frame; v1 is superseded and never accepted".to_owned()
            }
            Self::SchemaMismatch { observed } => {
                format!("Mesh transport rejected unknown frame schema {observed:?}")
            }
            Self::MalformedFrame { message } => {
                format!("Mesh transport frame is malformed: {message}")
            }
            Self::FrameTooLarge { actual_bytes } => format!(
                "Mesh transport frame is {actual_bytes} bytes, exceeding the {MAX_FRAME_BYTES}-byte cap"
            ),
            Self::PayloadTooLarge {
                actual_bytes,
                budget_bytes,
            } => format!(
                "Mesh transport payload is {actual_bytes} bytes, exceeding its {budget_bytes}-byte budget"
            ),
            Self::BindingMismatch { field } => format!(
                "Mesh transport frame does not match the established session binding ({field})"
            ),
            Self::ReplayRejected {
                violation,
                expected,
                observed,
            } => format!(
                "Mesh transport counter discipline violated ({violation:?}): expected exactly {expected}, observed {observed}; session closes"
            ),
            Self::SessionClosed => {
                "Mesh transport session is closed after a prior counter violation".to_owned()
            }
            Self::PayloadHashMismatch => {
                "Mesh transport payload hash does not match the canonical payload bytes".to_owned()
            }
            Self::MacMismatch => {
                "Mesh transport frame MAC failed verification under the directional session key"
                    .to_owned()
            }
            Self::ExtensionNotNegotiated { name } => format!(
                "Mesh transport extension capability {name:?} was not negotiated for this session"
            ),
        }
    }
}

impl fmt::Display for TransportSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for TransportSessionError {}

/// Per-direction inbound counter ledger. Ordered TCP means there is no replay
/// window: the only acceptable counter is exactly the next one, and any
/// violation closes the session permanently.
#[derive(Clone, Debug)]
pub struct SessionCounters {
    next: u64,
    closed: bool,
    exhausted: bool,
}

impl Default for SessionCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionCounters {
    /// A fresh ledger expecting counter `1` first.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next: 1,
            closed: false,
            exhausted: false,
        }
    }

    /// The next counter this ledger will accept.
    #[must_use]
    pub const fn expected_next(&self) -> u64 {
        self.next
    }

    /// Whether a violation has closed this session direction.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Accept exactly the next counter or close the session.
    pub fn accept(&mut self, observed: u64) -> Result<(), TransportSessionError> {
        if self.exhausted {
            return Err(TransportSessionError::ReplayRejected {
                violation: CounterViolation::Exhausted,
                expected: u64::MAX,
                observed,
            });
        }
        if self.closed {
            return Err(TransportSessionError::SessionClosed);
        }
        if observed == self.next {
            if self.next == u64::MAX {
                self.closed = true;
                self.exhausted = true;
            } else {
                self.next += 1;
            }
            return Ok(());
        }
        self.closed = true;
        let violation = if observed.saturating_add(1) == self.next {
            CounterViolation::Duplicate
        } else if observed > self.next {
            CounterViolation::Skipped
        } else {
            CounterViolation::Regressed
        };
        Err(TransportSessionError::ReplayRejected {
            violation,
            expected: self.next,
            observed,
        })
    }
}

/// Author and MAC one outbound frame under the directional session key.
pub fn sign_frame(
    binding: &SessionBinding,
    keys: &DirectionalSessionKeys,
    draft: FrameDraft,
) -> Result<FrameV2, TransportSessionError> {
    let payload_bytes = serde_json::to_vec(&draft.payload).map_err(|error| {
        TransportSessionError::MalformedFrame {
            message: format!("serialize payload: {error}"),
        }
    })?;
    enforce_payload_budget(&draft.capability, payload_bytes.len())?;
    let payload_hash = blake3::hash(&payload_bytes);
    let (source_node_id, target_node_id, source_workspace_id, target_workspace_id) =
        match draft.direction {
            SessionDirection::InitiatorToResponder => (
                binding.initiator_node_id.clone(),
                binding.responder_node_id.clone(),
                binding.initiator_workspace_id.clone(),
                binding.responder_workspace_id.clone(),
            ),
            SessionDirection::ResponderToInitiator => (
                binding.responder_node_id.clone(),
                binding.initiator_node_id.clone(),
                binding.responder_workspace_id.clone(),
                binding.initiator_workspace_id.clone(),
            ),
        };
    let mut frame = FrameV2 {
        schema: TRANSPORT_FRAME_SCHEMA_V2.to_owned(),
        source_node_id,
        target_node_id,
        team_id: binding.team_id.clone(),
        source_workspace_id,
        target_workspace_id,
        session_id: binding.session_id.clone(),
        direction: draft.direction,
        counter: draft.counter,
        correlation_id: draft.correlation_id,
        kind: draft.kind,
        capability: draft.capability,
        requested_budget_ms: draft.requested_budget_ms,
        payload: draft.payload,
        payload_hash: hex_lower(payload_hash.as_bytes()),
        mac: String::new(),
    };
    let preimage = frame.mac_preimage();
    let key = keys.for_direction(frame.direction);
    let tag = blake3::keyed_hash(key.as_bytes(), &preimage);
    frame.mac = hex_lower(tag.as_bytes());
    Ok(frame)
}

/// Decode one encoded frame, enforcing the outer size cap and rejecting v1
/// before anything else.
pub fn decode_frame(bytes: &[u8]) -> Result<FrameV2, TransportSessionError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(TransportSessionError::FrameTooLarge {
            actual_bytes: bytes.len(),
        });
    }
    // Schema triage before full deserialization so a v1 downgrade is named
    // precisely instead of surfacing as an arbitrary shape error.
    #[derive(Deserialize)]
    struct SchemaProbe {
        schema: String,
    }
    let probe: SchemaProbe =
        serde_json::from_slice(bytes).map_err(|error| TransportSessionError::MalformedFrame {
            message: format!("decode frame envelope: {error}"),
        })?;
    if probe.schema == TRANSPORT_FRAME_SCHEMA_V1 {
        return Err(TransportSessionError::V1Rejected);
    }
    if probe.schema != TRANSPORT_FRAME_SCHEMA_V2 {
        return Err(TransportSessionError::SchemaMismatch {
            observed: probe.schema,
        });
    }
    serde_json::from_slice(bytes).map_err(|error| TransportSessionError::MalformedFrame {
        message: format!("decode v2 frame: {error}"),
    })
}

/// Verify one inbound frame against the established session. Every check
/// runs **before capability dispatch**; the first failure wins and the
/// counter ledger closes the session on replay violations.
pub fn verify_frame(
    frame: &FrameV2,
    binding: &SessionBinding,
    expected_direction: SessionDirection,
    counters: &mut SessionCounters,
    keys: &DirectionalSessionKeys,
    negotiated: &NegotiatedExtensions,
) -> Result<VerifiedFrameV2, TransportSessionError> {
    if frame.schema == TRANSPORT_FRAME_SCHEMA_V1 {
        return Err(TransportSessionError::V1Rejected);
    }
    if frame.schema != TRANSPORT_FRAME_SCHEMA_V2 {
        return Err(TransportSessionError::SchemaMismatch {
            observed: frame.schema.clone(),
        });
    }

    // Binding checks: identity, route, and direction confusion all fail
    // closed before any cryptographic work.
    if frame.team_id != binding.team_id {
        return Err(TransportSessionError::BindingMismatch { field: "team_id" });
    }
    if frame.session_id != binding.session_id {
        return Err(TransportSessionError::BindingMismatch {
            field: "session_id",
        });
    }
    if frame.direction != expected_direction {
        return Err(TransportSessionError::BindingMismatch { field: "direction" });
    }
    let (expected_source_node, expected_target_node, expected_source_ws, expected_target_ws) =
        match expected_direction {
            SessionDirection::InitiatorToResponder => (
                &binding.initiator_node_id,
                &binding.responder_node_id,
                &binding.initiator_workspace_id,
                &binding.responder_workspace_id,
            ),
            SessionDirection::ResponderToInitiator => (
                &binding.responder_node_id,
                &binding.initiator_node_id,
                &binding.responder_workspace_id,
                &binding.initiator_workspace_id,
            ),
        };
    if &frame.source_node_id != expected_source_node {
        return Err(TransportSessionError::BindingMismatch {
            field: "source_node_id",
        });
    }
    if &frame.target_node_id != expected_target_node {
        return Err(TransportSessionError::BindingMismatch {
            field: "target_node_id",
        });
    }
    if &frame.source_workspace_id != expected_source_ws {
        return Err(TransportSessionError::BindingMismatch {
            field: "source_workspace_id",
        });
    }
    if &frame.target_workspace_id != expected_target_ws {
        return Err(TransportSessionError::BindingMismatch {
            field: "target_workspace_id",
        });
    }
    // Origin-as-target confusion: a frame naming the same endpoint on both
    // sides can never be valid.
    if frame.source_workspace_id == frame.target_workspace_id
        || frame.source_node_id == frame.target_node_id
    {
        return Err(TransportSessionError::BindingMismatch {
            field: "source_equals_target",
        });
    }

    // Exact-next counter; violations close the session.
    counters.accept(frame.counter)?;

    // Canonical payload bytes: budget, hash, then MAC.
    let payload_bytes = serde_json::to_vec(&frame.payload).map_err(|error| {
        TransportSessionError::MalformedFrame {
            message: format!("serialize payload: {error}"),
        }
    })?;
    enforce_payload_budget(&frame.capability, payload_bytes.len())?;
    let payload_hash = blake3::hash(&payload_bytes);
    if !constant_time_eq_hex(&frame.payload_hash, payload_hash.as_bytes()) {
        return Err(TransportSessionError::PayloadHashMismatch);
    }
    let preimage = frame.mac_preimage();
    let key = keys.for_direction(expected_direction);
    let tag = blake3::keyed_hash(key.as_bytes(), &preimage);
    if !constant_time_eq_hex(&frame.mac, tag.as_bytes()) {
        return Err(TransportSessionError::MacMismatch);
    }

    // Capability gate last: extensions must have been negotiated.
    if let FrameCapability::Extension(name) = &frame.capability
        && !negotiated.allows(name)
    {
        return Err(TransportSessionError::ExtensionNotNegotiated { name: name.clone() });
    }

    Ok(VerifiedFrameV2 {
        frame: frame.clone(),
    })
}

impl FrameV2 {
    /// The versioned, type-tagged, length-prefixed MAC preimage. JSON field
    /// order is irrelevant; only these canonical bytes are authenticated.
    #[must_use]
    pub fn mac_preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        push_lp(&mut out, FRAME_MAC_PREIMAGE_TAG.as_bytes());
        push_lp(&mut out, self.schema.as_bytes());
        push_lp(&mut out, self.source_node_id.as_bytes());
        push_lp(&mut out, self.target_node_id.as_bytes());
        push_lp(&mut out, self.team_id.as_bytes());
        push_lp(&mut out, self.source_workspace_id.as_bytes());
        push_lp(&mut out, self.target_workspace_id.as_bytes());
        push_lp(&mut out, self.session_id.as_bytes());
        push_lp(&mut out, self.direction.token().as_bytes());
        push_lp(&mut out, &self.counter.to_le_bytes());
        push_lp(&mut out, self.correlation_id.as_bytes());
        push_lp(&mut out, self.kind.token().as_bytes());
        push_lp(&mut out, self.capability.token().as_bytes());
        push_lp(&mut out, &self.requested_budget_ms.to_le_bytes());
        let payload_hash_bytes = decode_hex_32(&self.payload_hash).unwrap_or([0_u8; 32]);
        push_lp(&mut out, &payload_hash_bytes);
        out
    }
}

fn enforce_payload_budget(
    capability: &FrameCapability,
    actual_bytes: usize,
) -> Result<(), TransportSessionError> {
    let budget = match capability {
        FrameCapability::Extension(name) if name == "pair_rotate" => PAIR_ROTATE_MAX_PAYLOAD_BYTES,
        FrameCapability::Extension(name) if name == "identity_attest" => {
            IDENTITY_ATTEST_MAX_PAYLOAD_BYTES
        }
        _ => MAX_PAYLOAD_BYTES,
    };
    if actual_bytes > budget {
        return Err(TransportSessionError::PayloadTooLarge {
            actual_bytes,
            budget_bytes: budget,
        });
    }
    Ok(())
}

fn push_lp(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_val(chunk[0])?;
        let low = hex_val(chunk[1])?;
        bytes[index] = (high << 4) | low;
    }
    Some(bytes)
}

const fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Constant-time comparison of a lowercase-hex candidate against raw bytes.
fn constant_time_eq_hex(candidate_hex: &str, expected: &[u8; 32]) -> bool {
    let Some(candidate) = decode_hex_32(candidate_hex) else {
        return false;
    };
    let mut diff = 0_u8;
    for (a, b) in candidate.iter().zip(expected.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Fresh-nonce session handshake (T2.1 slice 3, ADR 0086 TC-D5).
//
// Three bounded messages establish a session before any application frame:
//
//   initiator                                   responder
//     `session_open` (claims + fresh nonce)  →
//                                            ←  `session_confirm` (fresh nonce
//                                                + pair-key confirmation MAC,
//                                                role = responder)
//     `session_finish` (confirmation MAC,    →
//                       role = initiator)
//
// The confirmation MACs prove pair-key possession over the full session
// transcript **before** either side derives directional keys, and they bind
// what the KAT'd derivation transcript does not: the pair-key generation,
// the sender role, and both currently observed Tailscale node public keys
// (observations stay transport evidence — they never become frame identity).
// Both nonces are fresh per connection, so a replayed `session_open` meets a
// new responder nonce and every stale confirmation MAC fails.
// ---------------------------------------------------------------------------

/// Schema id for the initiator's handshake open message.
pub const SESSION_OPEN_SCHEMA_V1: &str = "ee.mesh.session_open.v1";

/// Schema id for the responder's handshake confirm message.
pub const SESSION_CONFIRM_SCHEMA_V1: &str = "ee.mesh.session_confirm.v1";

/// Schema id for the initiator's handshake finish message.
pub const SESSION_FINISH_SCHEMA_V1: &str = "ee.mesh.session_finish.v1";

/// BLAKE3 `derive_key` context for the handshake confirmation key.
pub const SESSION_CONFIRM_KEY_CONTEXT: &str = "ee.team.session.confirm.v1";

/// Version tag leading every handshake confirmation MAC preimage.
pub const SESSION_CONFIRM_MAC_TAG: &str = "ee.mesh.session_confirm_mac.v1";

/// Hard outer limit on one encoded handshake message.
pub const MAX_HANDSHAKE_MESSAGE_BYTES: usize = 4096;

// Known-answer vectors captured from the BLAKE3 reference implementation
// (`b3sum`) over the fixed KAT binding: pair key `0x00..0x1f`, nonces
// `0x11 * 32` / `0x22 * 32`, generation 1, observations
// `nodekey:kat-init-observed` / `nodekey:kat-resp-observed`.
#[cfg(test)]
const KAT_RESPONDER_CONFIRM_MAC_HEX: &str =
    "2dcc3285621e01ef65b0c0275ad8422549df8f147b952c98f34359ff5eab5162";
#[cfg(test)]
const KAT_INITIATOR_FINISH_MAC_HEX: &str =
    "d4f02ca7ca6cd9585ae3b5d58a4aca7815da0e221fd5f885bf0a648958fb4b6f";

/// Both endpoints' currently observed Tailscale node public keys at handshake
/// time. Observations are verified transport evidence supplied by the caller
/// (LocalAPI WhoIs / fresh status); binding them into the confirmation MACs
/// makes a disagreement fail the handshake without ever promoting a rotating
/// node key to durable identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandshakeObservations {
    /// The initiator's current Tailscale node public key.
    pub initiator_node_pubkey: String,
    /// The responder's current Tailscale node public key.
    pub responder_node_pubkey: String,
}

/// Role token bound into a confirmation MAC preimage; prevents reflecting a
/// responder confirmation back as an initiator finish.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HandshakeRole {
    Initiator,
    Responder,
}

impl HandshakeRole {
    const fn token(self) -> &'static str {
        match self {
            Self::Initiator => "initiator",
            Self::Responder => "responder",
        }
    }
}

/// Initiator → responder handshake open message. Everything here is a claim
/// until the responder checks it against its own local knowledge and both
/// confirmation MACs verify.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionOpenV1 {
    /// Always [`SESSION_OPEN_SCHEMA_V1`].
    pub schema: String,
    /// Team the session should belong to.
    pub team_id: String,
    /// Random ee node ID of the initiator.
    pub initiator_node_id: String,
    /// Random ee node ID of the intended responder.
    pub responder_node_id: String,
    /// The initiator's locally selected endpoint workspace.
    pub initiator_workspace_id: String,
    /// The exact registered responder target workspace.
    pub responder_workspace_id: String,
    /// Pinned Tailscale stable node ID of the initiator.
    pub initiator_stable_id: String,
    /// Pinned Tailscale stable node ID of the responder.
    pub responder_stable_id: String,
    /// Session identifier minted by the initiator for this connection.
    pub session_id: String,
    /// Pair-key generation the initiator is authenticating under.
    pub pair_key_generation: u64,
    /// Lowercase hex of the initiator's fresh 32-byte nonce.
    pub initiator_nonce: String,
}

/// Responder → initiator handshake confirm message.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionConfirmV1 {
    /// Always [`SESSION_CONFIRM_SCHEMA_V1`].
    pub schema: String,
    /// Session identifier echoed from the open message.
    pub session_id: String,
    /// Lowercase hex of the responder's fresh 32-byte nonce.
    pub responder_nonce: String,
    /// Lowercase hex confirmation MAC (role = responder).
    pub confirm_mac: String,
}

/// Initiator → responder handshake finish message.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionFinishV1 {
    /// Always [`SESSION_FINISH_SCHEMA_V1`].
    pub schema: String,
    /// Session identifier echoed from the open message.
    pub session_id: String,
    /// Lowercase hex confirmation MAC (role = initiator).
    pub finish_mac: String,
}

/// Fail-closed error surface for the session handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HandshakeError {
    /// A handshake message could not be decoded.
    MalformedMessage {
        /// Human-readable decode failure.
        message: String,
    },
    /// An encoded handshake message exceeds [`MAX_HANDSHAKE_MESSAGE_BYTES`].
    MessageTooLarge {
        /// Observed encoded size.
        actual_bytes: usize,
    },
    /// A handshake message carried the wrong schema for its position.
    SchemaMismatch {
        /// The schema the message carried.
        observed: String,
    },
    /// An open-message claim does not match local knowledge (team, node,
    /// workspace, stable-ID, or self-target confusion).
    BindingMismatch {
        /// Which field mismatched.
        field: &'static str,
    },
    /// The peer authenticated under a different pair-key generation.
    GenerationMismatch {
        /// The generation this endpoint expected.
        expected: u64,
        /// The generation the message claimed.
        observed: u64,
    },
    /// A nonce field was not exactly 32 lowercase-hex-encoded bytes.
    BadNonce {
        /// Which nonce field was malformed.
        field: &'static str,
    },
    /// A confirmation MAC failed verification under the pair key.
    ConfirmationFailed {
        /// Which role's confirmation failed.
        role: &'static str,
    },
    /// A confirm/finish message named a different session than the open.
    SessionIdMismatch,
}

impl HandshakeError {
    /// Stable degraded code for this failure (T2.1 registry).
    #[must_use]
    pub const fn degraded_code(&self) -> &'static str {
        match self {
            Self::BindingMismatch { .. } | Self::SessionIdMismatch => "mesh_frame_target_mismatch",
            Self::MalformedMessage { .. }
            | Self::MessageTooLarge { .. }
            | Self::SchemaMismatch { .. }
            | Self::GenerationMismatch { .. }
            | Self::BadNonce { .. }
            | Self::ConfirmationFailed { .. } => "mesh_frame_auth_failed",
        }
    }

    /// Human-readable message.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::MalformedMessage { message } => {
                format!("Mesh session handshake message is malformed: {message}")
            }
            Self::MessageTooLarge { actual_bytes } => format!(
                "Mesh session handshake message is {actual_bytes} bytes, exceeding the {MAX_HANDSHAKE_MESSAGE_BYTES}-byte cap"
            ),
            Self::SchemaMismatch { observed } => {
                format!("Mesh session handshake rejected unexpected schema {observed:?}")
            }
            Self::BindingMismatch { field } => format!(
                "Mesh session handshake open message does not match local knowledge ({field})"
            ),
            Self::GenerationMismatch { expected, observed } => format!(
                "Mesh session handshake pair-key generation mismatch: expected {expected}, observed {observed}"
            ),
            Self::BadNonce { field } => format!(
                "Mesh session handshake nonce {field} is not exactly 32 lowercase-hex bytes"
            ),
            Self::ConfirmationFailed { role } => format!(
                "Mesh session handshake {role} confirmation MAC failed verification under the pair key"
            ),
            Self::SessionIdMismatch => {
                "Mesh session handshake message names a different session than the open".to_owned()
            }
        }
    }
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for HandshakeError {}

/// What the responder requires an inbound `session_open` to match. Every
/// field comes from local state: the team registration, the local node
/// identity, the one registered target workspace, the local stable ID, and
/// the enrolled peer record the pair key belongs to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponderExpectations {
    /// Team this responder is registered under.
    pub team_id: String,
    /// This responder's random ee node ID.
    pub responder_node_id: String,
    /// The exact registered responder target workspace.
    pub responder_workspace_id: String,
    /// This responder's pinned Tailscale stable node ID.
    pub responder_stable_id: String,
    /// The enrolled peer's random ee node ID (from the pair-key record).
    pub initiator_node_id: String,
    /// The enrolled peer's pinned Tailscale stable node ID.
    pub initiator_stable_id: String,
    /// The pair-key generation this responder currently accepts.
    pub pair_key_generation: u64,
}

/// A fully established session: verified binding, directional keys, and a
/// fresh inbound counter ledger. The first outbound frame uses counter 1.
#[derive(Debug)]
pub struct EstablishedSession {
    /// The session binding both confirmation MACs verified.
    pub binding: SessionBinding,
    /// Directional session keys derived after mutual key confirmation.
    pub keys: DirectionalSessionKeys,
    /// Inbound counter ledger (expects exactly 1 first).
    pub inbound: SessionCounters,
    /// The counter value for this endpoint's next outbound frame.
    pub next_outbound: u64,
}

/// Initiator-side handshake state between `session_open` and the responder's
/// confirm message.
#[derive(Debug)]
pub struct InitiatorHandshake {
    binding: SessionBinding,
    initiator_nonce: [u8; 32],
    pair_key_generation: u64,
    observations: HandshakeObservations,
}

impl InitiatorHandshake {
    /// Author the `session_open` message for `binding`. The caller supplies
    /// the fresh 32-byte nonce from the OS CSPRNG, the pair-key generation it
    /// authenticates under, and its current node-key observations.
    pub fn open(
        binding: SessionBinding,
        initiator_nonce: [u8; 32],
        pair_key_generation: u64,
        observations: HandshakeObservations,
    ) -> Result<(Self, SessionOpenV1), HandshakeError> {
        validate_binding_shape(&binding)?;
        let open = SessionOpenV1 {
            schema: SESSION_OPEN_SCHEMA_V1.to_owned(),
            team_id: binding.team_id.clone(),
            initiator_node_id: binding.initiator_node_id.clone(),
            responder_node_id: binding.responder_node_id.clone(),
            initiator_workspace_id: binding.initiator_workspace_id.clone(),
            responder_workspace_id: binding.responder_workspace_id.clone(),
            initiator_stable_id: binding.initiator_stable_id.clone(),
            responder_stable_id: binding.responder_stable_id.clone(),
            session_id: binding.session_id.clone(),
            pair_key_generation,
            initiator_nonce: hex_lower(&initiator_nonce),
        };
        ensure_handshake_size(&open)?;
        Ok((
            Self {
                binding,
                initiator_nonce,
                pair_key_generation,
                observations,
            },
            open,
        ))
    }

    /// Verify the responder's confirmation MAC, author the finish message,
    /// and derive the directional session keys. Consumes the handshake so a
    /// confirm message can never be processed twice.
    pub fn finish(
        self,
        pair_key: &SecretBytes,
        confirm: &SessionConfirmV1,
    ) -> Result<(SessionFinishV1, EstablishedSession), HandshakeError> {
        if confirm.schema != SESSION_CONFIRM_SCHEMA_V1 {
            return Err(HandshakeError::SchemaMismatch {
                observed: confirm.schema.clone(),
            });
        }
        if confirm.session_id != self.binding.session_id {
            return Err(HandshakeError::SessionIdMismatch);
        }
        let responder_nonce = decode_nonce(&confirm.responder_nonce, "responder_nonce")?;
        let transcript_hash =
            session_transcript_hash(&self.binding, &self.initiator_nonce, &responder_nonce);
        let confirm_key = derive_confirm_key(pair_key, &transcript_hash);
        let expected_responder = confirmation_mac(
            &confirm_key,
            &transcript_hash,
            self.pair_key_generation,
            HandshakeRole::Responder,
            &self.observations,
        );
        if !constant_time_eq_hex(&confirm.confirm_mac, &expected_responder) {
            return Err(HandshakeError::ConfirmationFailed { role: "responder" });
        }
        let finish_tag = confirmation_mac(
            &confirm_key,
            &transcript_hash,
            self.pair_key_generation,
            HandshakeRole::Initiator,
            &self.observations,
        );
        let finish = SessionFinishV1 {
            schema: SESSION_FINISH_SCHEMA_V1.to_owned(),
            session_id: self.binding.session_id.clone(),
            finish_mac: hex_lower(&finish_tag),
        };
        ensure_handshake_size(&finish)?;
        let keys = derive_session_keys(
            pair_key,
            &self.binding,
            &self.initiator_nonce,
            &responder_nonce,
        );
        Ok((
            finish,
            EstablishedSession {
                binding: self.binding,
                keys,
                inbound: SessionCounters::new(),
                next_outbound: 1,
            },
        ))
    }
}

/// Responder-side handshake state between its confirm message and the
/// initiator's finish message.
#[derive(Debug)]
pub struct ResponderPendingSession {
    binding: SessionBinding,
    initiator_nonce: [u8; 32],
    responder_nonce: [u8; 32],
    pair_key_generation: u64,
    observations: HandshakeObservations,
}

/// Validate an inbound `session_open` against local knowledge and author the
/// confirm message. The binding is reconstructed from **verified expectation
/// values** plus the open message's initiator workspace claim; nothing the
/// wire claimed becomes identity without matching local state first.
pub fn responder_accept_open(
    open: &SessionOpenV1,
    expectations: &ResponderExpectations,
    responder_nonce: [u8; 32],
    observations: HandshakeObservations,
    pair_key: &SecretBytes,
) -> Result<(ResponderPendingSession, SessionConfirmV1), HandshakeError> {
    if open.schema != SESSION_OPEN_SCHEMA_V1 {
        return Err(HandshakeError::SchemaMismatch {
            observed: open.schema.clone(),
        });
    }
    if open.team_id != expectations.team_id {
        return Err(HandshakeError::BindingMismatch { field: "team_id" });
    }
    if open.responder_node_id != expectations.responder_node_id {
        return Err(HandshakeError::BindingMismatch {
            field: "responder_node_id",
        });
    }
    if open.responder_workspace_id != expectations.responder_workspace_id {
        return Err(HandshakeError::BindingMismatch {
            field: "responder_workspace_id",
        });
    }
    if open.responder_stable_id != expectations.responder_stable_id {
        return Err(HandshakeError::BindingMismatch {
            field: "responder_stable_id",
        });
    }
    if open.initiator_node_id != expectations.initiator_node_id {
        return Err(HandshakeError::BindingMismatch {
            field: "initiator_node_id",
        });
    }
    if open.initiator_stable_id != expectations.initiator_stable_id {
        return Err(HandshakeError::BindingMismatch {
            field: "initiator_stable_id",
        });
    }
    if open.pair_key_generation != expectations.pair_key_generation {
        return Err(HandshakeError::GenerationMismatch {
            expected: expectations.pair_key_generation,
            observed: open.pair_key_generation,
        });
    }
    if open.session_id.is_empty() {
        return Err(HandshakeError::BindingMismatch {
            field: "session_id",
        });
    }
    if open.initiator_workspace_id.is_empty()
        || open.initiator_workspace_id == open.responder_workspace_id
    {
        return Err(HandshakeError::BindingMismatch {
            field: "initiator_workspace_id",
        });
    }
    let initiator_nonce = decode_nonce(&open.initiator_nonce, "initiator_nonce")?;
    let binding = SessionBinding {
        team_id: expectations.team_id.clone(),
        initiator_node_id: expectations.initiator_node_id.clone(),
        responder_node_id: expectations.responder_node_id.clone(),
        initiator_workspace_id: open.initiator_workspace_id.clone(),
        responder_workspace_id: expectations.responder_workspace_id.clone(),
        initiator_stable_id: expectations.initiator_stable_id.clone(),
        responder_stable_id: expectations.responder_stable_id.clone(),
        session_id: open.session_id.clone(),
    };
    validate_binding_shape(&binding)?;
    let transcript_hash = session_transcript_hash(&binding, &initiator_nonce, &responder_nonce);
    let confirm_key = derive_confirm_key(pair_key, &transcript_hash);
    let confirm_tag = confirmation_mac(
        &confirm_key,
        &transcript_hash,
        expectations.pair_key_generation,
        HandshakeRole::Responder,
        &observations,
    );
    let confirm = SessionConfirmV1 {
        schema: SESSION_CONFIRM_SCHEMA_V1.to_owned(),
        session_id: binding.session_id.clone(),
        responder_nonce: hex_lower(&responder_nonce),
        confirm_mac: hex_lower(&confirm_tag),
    };
    ensure_handshake_size(&confirm)?;
    Ok((
        ResponderPendingSession {
            binding,
            initiator_nonce,
            responder_nonce,
            pair_key_generation: expectations.pair_key_generation,
            observations,
        },
        confirm,
    ))
}

impl ResponderPendingSession {
    /// Verify the initiator's finish MAC and derive the directional session
    /// keys. Consumes the pending state so a finish message can never be
    /// processed twice.
    pub fn complete(
        self,
        pair_key: &SecretBytes,
        finish: &SessionFinishV1,
    ) -> Result<EstablishedSession, HandshakeError> {
        if finish.schema != SESSION_FINISH_SCHEMA_V1 {
            return Err(HandshakeError::SchemaMismatch {
                observed: finish.schema.clone(),
            });
        }
        if finish.session_id != self.binding.session_id {
            return Err(HandshakeError::SessionIdMismatch);
        }
        let transcript_hash =
            session_transcript_hash(&self.binding, &self.initiator_nonce, &self.responder_nonce);
        let confirm_key = derive_confirm_key(pair_key, &transcript_hash);
        let expected_initiator = confirmation_mac(
            &confirm_key,
            &transcript_hash,
            self.pair_key_generation,
            HandshakeRole::Initiator,
            &self.observations,
        );
        if !constant_time_eq_hex(&finish.finish_mac, &expected_initiator) {
            return Err(HandshakeError::ConfirmationFailed { role: "initiator" });
        }
        let keys = derive_session_keys(
            pair_key,
            &self.binding,
            &self.initiator_nonce,
            &self.responder_nonce,
        );
        Ok(EstablishedSession {
            binding: self.binding,
            keys,
            inbound: SessionCounters::new(),
            next_outbound: 1,
        })
    }
}

/// Decode one encoded `session_open` message.
pub fn decode_session_open(bytes: &[u8]) -> Result<SessionOpenV1, HandshakeError> {
    decode_handshake_message(bytes, SESSION_OPEN_SCHEMA_V1)
}

/// Decode one encoded `session_confirm` message.
pub fn decode_session_confirm(bytes: &[u8]) -> Result<SessionConfirmV1, HandshakeError> {
    decode_handshake_message(bytes, SESSION_CONFIRM_SCHEMA_V1)
}

/// Decode one encoded `session_finish` message.
pub fn decode_session_finish(bytes: &[u8]) -> Result<SessionFinishV1, HandshakeError> {
    decode_handshake_message(bytes, SESSION_FINISH_SCHEMA_V1)
}

fn decode_handshake_message<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    expected_schema: &str,
) -> Result<T, HandshakeError> {
    if bytes.len() > MAX_HANDSHAKE_MESSAGE_BYTES {
        return Err(HandshakeError::MessageTooLarge {
            actual_bytes: bytes.len(),
        });
    }
    #[derive(Deserialize)]
    struct SchemaProbe {
        schema: String,
    }
    let probe: SchemaProbe =
        serde_json::from_slice(bytes).map_err(|error| HandshakeError::MalformedMessage {
            message: format!("decode handshake envelope: {error}"),
        })?;
    if probe.schema != expected_schema {
        return Err(HandshakeError::SchemaMismatch {
            observed: probe.schema,
        });
    }
    serde_json::from_slice(bytes).map_err(|error| HandshakeError::MalformedMessage {
        message: format!("decode handshake message: {error}"),
    })
}

fn validate_binding_shape(binding: &SessionBinding) -> Result<(), HandshakeError> {
    let non_empty: [(&'static str, &str); 8] = [
        ("team_id", &binding.team_id),
        ("initiator_node_id", &binding.initiator_node_id),
        ("responder_node_id", &binding.responder_node_id),
        ("initiator_workspace_id", &binding.initiator_workspace_id),
        ("responder_workspace_id", &binding.responder_workspace_id),
        ("initiator_stable_id", &binding.initiator_stable_id),
        ("responder_stable_id", &binding.responder_stable_id),
        ("session_id", &binding.session_id),
    ];
    for (field, value) in non_empty {
        if value.is_empty() {
            return Err(HandshakeError::BindingMismatch { field });
        }
    }
    if binding.initiator_node_id == binding.responder_node_id
        || binding.initiator_workspace_id == binding.responder_workspace_id
    {
        return Err(HandshakeError::BindingMismatch {
            field: "source_equals_target",
        });
    }
    Ok(())
}

fn session_transcript_hash(
    binding: &SessionBinding,
    initiator_nonce: &[u8; 32],
    responder_nonce: &[u8; 32],
) -> [u8; 32] {
    blake3::derive_key(
        SESSION_TRANSCRIPT_CONTEXT,
        &binding.transcript_bytes(initiator_nonce, responder_nonce),
    )
}

fn derive_confirm_key(pair_key: &SecretBytes, transcript_hash: &[u8; 32]) -> SecretBytes {
    let mut material = Vec::with_capacity(2 * (4 + 32));
    push_lp(&mut material, pair_key.as_bytes());
    push_lp(&mut material, transcript_hash);
    let key = SecretBytes::new(blake3::derive_key(SESSION_CONFIRM_KEY_CONTEXT, &material));
    material.fill(0);
    compiler_fence(Ordering::SeqCst);
    key
}

fn confirmation_mac(
    confirm_key: &SecretBytes,
    transcript_hash: &[u8; 32],
    pair_key_generation: u64,
    role: HandshakeRole,
    observations: &HandshakeObservations,
) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(256);
    push_lp(&mut preimage, SESSION_CONFIRM_MAC_TAG.as_bytes());
    push_lp(&mut preimage, transcript_hash);
    push_lp(&mut preimage, &pair_key_generation.to_le_bytes());
    push_lp(&mut preimage, role.token().as_bytes());
    push_lp(&mut preimage, observations.initiator_node_pubkey.as_bytes());
    push_lp(&mut preimage, observations.responder_node_pubkey.as_bytes());
    *blake3::keyed_hash(confirm_key.as_bytes(), &preimage).as_bytes()
}

fn decode_nonce(value: &str, field: &'static str) -> Result<[u8; 32], HandshakeError> {
    decode_hex_32(value).ok_or(HandshakeError::BadNonce { field })
}

fn ensure_handshake_size<T: Serialize>(message: &T) -> Result<(), HandshakeError> {
    let bytes = serde_json::to_vec(message).map_err(|error| HandshakeError::MalformedMessage {
        message: format!("serialize handshake message: {error}"),
    })?;
    if bytes.len() > MAX_HANDSHAKE_MESSAGE_BYTES {
        return Err(HandshakeError::MessageTooLarge {
            actual_bytes: bytes.len(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kat_binding() -> SessionBinding {
        SessionBinding {
            team_id: "team-kat".to_owned(),
            initiator_node_id: "node-init".to_owned(),
            responder_node_id: "node-resp".to_owned(),
            initiator_workspace_id: "ws-init".to_owned(),
            responder_workspace_id: "ws-resp".to_owned(),
            initiator_stable_id: "stable-init".to_owned(),
            responder_stable_id: "stable-resp".to_owned(),
            session_id: "sess-0001".to_owned(),
        }
    }

    fn kat_keys() -> DirectionalSessionKeys {
        let mut pair = [0_u8; 32];
        for (index, byte) in pair.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("index fits");
        }
        derive_session_keys(
            &SecretBytes::new(pair),
            &kat_binding(),
            &[0x11; 32],
            &[0x22; 32],
        )
    }

    fn kat_frame(keys: &DirectionalSessionKeys) -> FrameV2 {
        sign_frame(
            &kat_binding(),
            keys,
            FrameDraft {
                direction: SessionDirection::InitiatorToResponder,
                counter: 1,
                correlation_id: "corr-0001".to_owned(),
                kind: FrameKind::Request,
                capability: FrameCapability::Hello,
                requested_budget_ms: 30_000,
                payload: json!({}),
            },
        )
        .expect("sign frame")
    }

    #[test]
    fn derivation_matches_pinned_reference_vectors() {
        let binding = kat_binding();
        let transcript = binding.transcript_bytes(&[0x11; 32], &[0x22; 32]);
        let transcript_hash = blake3::derive_key(SESSION_TRANSCRIPT_CONTEXT, &transcript);
        assert_eq!(hex_lower(&transcript_hash), KAT_TRANSCRIPT_HASH_HEX);
        let keys = kat_keys();
        assert_eq!(
            hex_lower(keys.initiator_to_responder.as_bytes()),
            KAT_I2R_HEX
        );
        assert_eq!(
            hex_lower(keys.responder_to_initiator.as_bytes()),
            KAT_R2I_HEX
        );
    }

    #[test]
    fn frame_mac_matches_pinned_reference_vector() {
        let keys = kat_keys();
        let frame = kat_frame(&keys);
        assert_eq!(frame.mac, KAT_FRAME_MAC_HEX);
    }

    #[test]
    fn directional_keys_differ_and_wrong_direction_key_fails() {
        let keys = kat_keys();
        assert_ne!(
            keys.initiator_to_responder.as_bytes(),
            keys.responder_to_initiator.as_bytes()
        );
        let frame = kat_frame(&keys);
        let mut counters = SessionCounters::new();
        let swapped = DirectionalSessionKeys {
            initiator_to_responder: SecretBytes::new(*keys.responder_to_initiator.as_bytes()),
            responder_to_initiator: SecretBytes::new(*keys.initiator_to_responder.as_bytes()),
        };
        let error = verify_frame(
            &frame,
            &kat_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &swapped,
            &NegotiatedExtensions::none(),
        )
        .expect_err("wrong-direction key must fail");
        assert_eq!(error, TransportSessionError::MacMismatch);
        assert_eq!(error.degraded_code(), "mesh_frame_auth_failed");
    }

    #[test]
    fn round_trip_verifies_and_counter_advances() {
        let keys = kat_keys();
        let frame = kat_frame(&keys);
        let encoded = serde_json::to_vec(&frame).expect("encode");
        let decoded = decode_frame(&encoded).expect("decode");
        let mut counters = SessionCounters::new();
        let verified = verify_frame(
            &decoded,
            &kat_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &keys,
            &NegotiatedExtensions::none(),
        )
        .expect("verify");
        assert_eq!(verified.frame.capability, FrameCapability::Hello);
        assert_eq!(counters.expected_next(), 2);
        assert!(!counters.is_closed());
    }

    #[test]
    fn v1_frames_are_rejected_outright() {
        let bytes = serde_json::to_vec(&json!({
            "schema": TRANSPORT_FRAME_SCHEMA_V1,
            "frameId": "f-1"
        }))
        .expect("encode");
        assert_eq!(
            decode_frame(&bytes).expect_err("v1 must be rejected"),
            TransportSessionError::V1Rejected
        );
    }

    #[test]
    fn unknown_schema_is_rejected() {
        let bytes = serde_json::to_vec(&json!({
            "schema": "ee.mesh.tailscale_transport_frame.v3"
        }))
        .expect("encode");
        assert!(matches!(
            decode_frame(&bytes).expect_err("unknown schema"),
            TransportSessionError::SchemaMismatch { .. }
        ));
    }

    #[test]
    fn oversized_frames_are_rejected_before_decode() {
        let bytes = vec![b'{'; MAX_FRAME_BYTES + 1];
        assert!(matches!(
            decode_frame(&bytes).expect_err("oversize"),
            TransportSessionError::FrameTooLarge { .. }
        ));
    }

    #[test]
    fn every_binding_field_mismatch_is_refused() {
        let keys = kat_keys();
        let base = kat_frame(&keys);
        let mutations: Vec<(&str, Box<dyn Fn(&mut FrameV2)>)> = vec![
            ("team_id", Box::new(|f| f.team_id = "team-other".to_owned())),
            (
                "session_id",
                Box::new(|f| f.session_id = "sess-other".to_owned()),
            ),
            (
                "source_node_id",
                Box::new(|f| f.source_node_id = "node-x".to_owned()),
            ),
            (
                "target_node_id",
                Box::new(|f| f.target_node_id = "node-x".to_owned()),
            ),
            (
                "source_workspace_id",
                Box::new(|f| f.source_workspace_id = "ws-x".to_owned()),
            ),
            (
                "target_workspace_id",
                Box::new(|f| f.target_workspace_id = "ws-x".to_owned()),
            ),
            (
                "direction",
                Box::new(|f| f.direction = SessionDirection::ResponderToInitiator),
            ),
        ];
        for (field, mutate) in mutations {
            let mut frame = base.clone();
            mutate(&mut frame);
            let mut counters = SessionCounters::new();
            let error = verify_frame(
                &frame,
                &kat_binding(),
                SessionDirection::InitiatorToResponder,
                &mut counters,
                &keys,
                &NegotiatedExtensions::none(),
            )
            .expect_err("binding mismatch must fail");
            assert!(
                matches!(error, TransportSessionError::BindingMismatch { .. }),
                "field {field} produced {error:?}"
            );
            assert_eq!(error.degraded_code(), "mesh_frame_target_mismatch");
        }
    }

    #[test]
    fn counter_discipline_accepts_exactly_next_and_closes_on_violations() {
        let mut counters = SessionCounters::new();
        counters.accept(1).expect("first");
        counters.accept(2).expect("second");
        let error = counters.accept(2).expect_err("duplicate");
        assert!(matches!(
            error,
            TransportSessionError::ReplayRejected {
                violation: CounterViolation::Duplicate,
                ..
            }
        ));
        assert!(counters.is_closed());
        assert_eq!(
            counters.accept(3).expect_err("closed"),
            TransportSessionError::SessionClosed
        );

        let mut skipped = SessionCounters::new();
        let error = skipped.accept(5).expect_err("skipped");
        assert!(matches!(
            error,
            TransportSessionError::ReplayRejected {
                violation: CounterViolation::Skipped,
                ..
            }
        ));

        let mut regressed = SessionCounters::new();
        regressed.accept(1).expect("first");
        regressed.accept(2).expect("second");
        regressed.accept(3).expect("third");
        let error = regressed.accept(1).expect_err("regressed");
        assert!(matches!(
            error,
            TransportSessionError::ReplayRejected {
                violation: CounterViolation::Regressed,
                ..
            }
        ));
        assert_eq!(error.degraded_code(), "mesh_frame_replay_rejected");
    }

    #[test]
    fn tampered_payload_and_mac_fail_closed() {
        let keys = kat_keys();
        let mut frame = kat_frame(&keys);
        frame.payload = json!({"tampered": true});
        let mut counters = SessionCounters::new();
        assert_eq!(
            verify_frame(
                &frame,
                &kat_binding(),
                SessionDirection::InitiatorToResponder,
                &mut counters,
                &keys,
                &NegotiatedExtensions::none(),
            )
            .expect_err("payload tamper"),
            TransportSessionError::PayloadHashMismatch
        );

        let mut frame = kat_frame(&keys);
        frame.mac = format!("{}{}", &frame.mac[..62], "00");
        let mut counters = SessionCounters::new();
        assert_eq!(
            verify_frame(
                &frame,
                &kat_binding(),
                SessionDirection::InitiatorToResponder,
                &mut counters,
                &keys,
                &NegotiatedExtensions::none(),
            )
            .expect_err("mac tamper"),
            TransportSessionError::MacMismatch
        );
    }

    #[test]
    fn nonce_or_binding_change_changes_session_keys() {
        let mut pair = [0_u8; 32];
        for (index, byte) in pair.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("index fits");
        }
        let pair_key = SecretBytes::new(pair);
        let base = derive_session_keys(&pair_key, &kat_binding(), &[0x11; 32], &[0x22; 32]);
        let other_nonce = derive_session_keys(&pair_key, &kat_binding(), &[0x11; 32], &[0x23; 32]);
        assert_ne!(
            base.initiator_to_responder.as_bytes(),
            other_nonce.initiator_to_responder.as_bytes()
        );
        let mut other_binding = kat_binding();
        other_binding.responder_workspace_id = "ws-other".to_owned();
        let rebound = derive_session_keys(&pair_key, &other_binding, &[0x11; 32], &[0x22; 32]);
        assert_ne!(
            base.initiator_to_responder.as_bytes(),
            rebound.initiator_to_responder.as_bytes()
        );
    }

    #[test]
    fn extension_capabilities_require_negotiation_and_respect_caps() {
        let keys = kat_keys();
        let frame = sign_frame(
            &kat_binding(),
            &keys,
            FrameDraft {
                direction: SessionDirection::InitiatorToResponder,
                counter: 1,
                correlation_id: "corr-0002".to_owned(),
                kind: FrameKind::Request,
                capability: FrameCapability::Extension("pair_rotate".to_owned()),
                requested_budget_ms: 5_000,
                payload: json!({"op": "rotate"}),
            },
        )
        .expect("sign extension frame");
        let mut counters = SessionCounters::new();
        let error = verify_frame(
            &frame,
            &kat_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &keys,
            &NegotiatedExtensions::none(),
        )
        .expect_err("un-negotiated extension must fail");
        assert!(matches!(
            error,
            TransportSessionError::ExtensionNotNegotiated { .. }
        ));

        let mut counters = SessionCounters::new();
        let negotiated = NegotiatedExtensions::from_names(["pair_rotate".to_owned()]);
        verify_frame(
            &frame,
            &kat_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &keys,
            &negotiated,
        )
        .expect("negotiated extension verifies");

        let oversize_payload = json!({"blob": "x".repeat(PAIR_ROTATE_MAX_PAYLOAD_BYTES)});
        let error = sign_frame(
            &kat_binding(),
            &keys,
            FrameDraft {
                direction: SessionDirection::InitiatorToResponder,
                counter: 2,
                correlation_id: "corr-0003".to_owned(),
                kind: FrameKind::Request,
                capability: FrameCapability::Extension("pair_rotate".to_owned()),
                requested_budget_ms: 5_000,
                payload: oversize_payload,
            },
        )
        .expect_err("pair_rotate payload cap enforced");
        assert!(matches!(
            error,
            TransportSessionError::PayloadTooLarge {
                budget_bytes: PAIR_ROTATE_MAX_PAYLOAD_BYTES,
                ..
            }
        ));
    }

    #[test]
    fn retry_uses_next_counter_with_same_idempotency_key() {
        let keys = kat_keys();
        let first = kat_frame(&keys);
        let retry = sign_frame(
            &kat_binding(),
            &keys,
            FrameDraft {
                direction: SessionDirection::InitiatorToResponder,
                counter: 2,
                correlation_id: "corr-0001".to_owned(),
                kind: FrameKind::Request,
                capability: FrameCapability::Hello,
                requested_budget_ms: 30_000,
                payload: json!({}),
            },
        )
        .expect("retry frame");
        assert_ne!(first.mac, retry.mac, "retries never replay frame bytes");
        assert_eq!(first.correlation_id, retry.correlation_id);
        let mut counters = SessionCounters::new();
        verify_frame(
            &first,
            &kat_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &keys,
            &NegotiatedExtensions::none(),
        )
        .expect("first verifies");
        verify_frame(
            &retry,
            &kat_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &keys,
            &NegotiatedExtensions::none(),
        )
        .expect("retry verifies with next counter");
    }

    #[test]
    fn mac_preimage_is_field_order_independent_of_json() {
        let keys = kat_keys();
        let frame = kat_frame(&keys);
        // Re-encode with a different JSON field order and confirm the frame
        // still verifies: only canonical preimage bytes are authenticated.
        let mut value = serde_json::to_value(&frame).expect("to value");
        let object = value.as_object_mut().expect("object");
        let reordered: serde_json::Map<String, JsonValue> = object
            .iter()
            .rev()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let bytes = serde_json::to_vec(&reordered).expect("encode reordered");
        assert!(bytes.len() <= MAX_FRAME_BYTES);
        let decoded = decode_frame(&bytes).expect("decode reordered");
        let mut counters = SessionCounters::new();
        verify_frame(
            &decoded,
            &kat_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &keys,
            &NegotiatedExtensions::none(),
        )
        .expect("reordered frame verifies");
    }

    // -- fresh-nonce handshake ---------------------------------------------

    fn kat_pair_key() -> SecretBytes {
        let mut pair = [0_u8; 32];
        for (index, byte) in pair.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("index fits");
        }
        SecretBytes::new(pair)
    }

    fn kat_observations() -> HandshakeObservations {
        HandshakeObservations {
            initiator_node_pubkey: "nodekey:kat-init-observed".to_owned(),
            responder_node_pubkey: "nodekey:kat-resp-observed".to_owned(),
        }
    }

    fn kat_expectations() -> ResponderExpectations {
        ResponderExpectations {
            team_id: "team-kat".to_owned(),
            responder_node_id: "node-resp".to_owned(),
            responder_workspace_id: "ws-resp".to_owned(),
            responder_stable_id: "stable-resp".to_owned(),
            initiator_node_id: "node-init".to_owned(),
            initiator_stable_id: "stable-init".to_owned(),
            pair_key_generation: 1,
        }
    }

    /// Run the full three-message handshake with the KAT inputs.
    fn kat_handshake() -> (
        EstablishedSession,
        EstablishedSession,
        SessionConfirmV1,
        SessionFinishV1,
    ) {
        let (initiator, open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("open");
        let (pending, confirm) = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x22; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect("accept open");
        let (finish, initiator_session) = initiator
            .finish(&kat_pair_key(), &confirm)
            .expect("initiator finish");
        let responder_session = pending
            .complete(&kat_pair_key(), &finish)
            .expect("responder complete");
        (initiator_session, responder_session, confirm, finish)
    }

    #[test]
    fn handshake_confirmation_macs_match_pinned_reference_vectors() {
        let (initiator_session, responder_session, confirm, finish) = kat_handshake();
        assert_eq!(confirm.confirm_mac, KAT_RESPONDER_CONFIRM_MAC_HEX);
        assert_eq!(finish.finish_mac, KAT_INITIATOR_FINISH_MAC_HEX);
        // Both sides derived the exact keys the slice-2 KAT pinned.
        assert_eq!(
            hex_lower(initiator_session.keys.initiator_to_responder.as_bytes()),
            KAT_I2R_HEX
        );
        assert_eq!(
            hex_lower(responder_session.keys.responder_to_initiator.as_bytes()),
            KAT_R2I_HEX
        );
        assert_eq!(initiator_session.binding, responder_session.binding);
        assert_eq!(initiator_session.next_outbound, 1);
        assert_eq!(responder_session.inbound.expected_next(), 1);
    }

    #[test]
    fn handshake_establishes_sessions_that_verify_frames_end_to_end() {
        let (initiator_session, mut responder_session, _confirm, _finish) = kat_handshake();
        let frame = sign_frame(
            &initiator_session.binding,
            &initiator_session.keys,
            FrameDraft {
                direction: SessionDirection::InitiatorToResponder,
                counter: initiator_session.next_outbound,
                correlation_id: "corr-hs-1".to_owned(),
                kind: FrameKind::Request,
                capability: FrameCapability::Hello,
                requested_budget_ms: 30_000,
                payload: json!({}),
            },
        )
        .expect("sign");
        verify_frame(
            &frame,
            &responder_session.binding,
            SessionDirection::InitiatorToResponder,
            &mut responder_session.inbound,
            &responder_session.keys,
            &NegotiatedExtensions::none(),
        )
        .expect("frame from handshake-derived session verifies");
    }

    #[test]
    fn responder_rejects_every_open_claim_mismatch() {
        let (_initiator, base_open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("open");
        let mutations: Vec<(&str, Box<dyn Fn(&mut SessionOpenV1)>)> = vec![
            ("team_id", Box::new(|o| o.team_id = "team-other".to_owned())),
            (
                "responder_node_id",
                Box::new(|o| o.responder_node_id = "node-x".to_owned()),
            ),
            (
                "responder_workspace_id",
                Box::new(|o| o.responder_workspace_id = "ws-x".to_owned()),
            ),
            (
                "responder_stable_id",
                Box::new(|o| o.responder_stable_id = "stable-x".to_owned()),
            ),
            (
                "initiator_node_id",
                Box::new(|o| o.initiator_node_id = "node-x".to_owned()),
            ),
            (
                "initiator_stable_id",
                Box::new(|o| o.initiator_stable_id = "stable-x".to_owned()),
            ),
            (
                "initiator_workspace_id",
                Box::new(|o| o.initiator_workspace_id = "ws-resp".to_owned()),
            ),
            ("session_id", Box::new(|o| o.session_id = String::new())),
        ];
        for (field, mutate) in mutations {
            let mut open = base_open.clone();
            mutate(&mut open);
            let error = responder_accept_open(
                &open,
                &kat_expectations(),
                [0x22; 32],
                kat_observations(),
                &kat_pair_key(),
            )
            .expect_err("claim mismatch must fail");
            assert!(
                matches!(error, HandshakeError::BindingMismatch { .. }),
                "field {field} produced {error:?}"
            );
            assert_eq!(error.degraded_code(), "mesh_frame_target_mismatch");
        }

        let mut open = base_open.clone();
        open.pair_key_generation = 2;
        let error = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x22; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect_err("generation mismatch must fail");
        assert_eq!(
            error,
            HandshakeError::GenerationMismatch {
                expected: 1,
                observed: 2
            }
        );
        assert_eq!(error.degraded_code(), "mesh_frame_auth_failed");

        let mut open = base_open;
        open.initiator_nonce = "zz".repeat(32);
        let error = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x22; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect_err("bad nonce must fail");
        assert_eq!(
            error,
            HandshakeError::BadNonce {
                field: "initiator_nonce"
            }
        );
    }

    #[test]
    fn handshake_fails_when_either_side_lacks_the_pair_key() {
        // Responder confirms under the wrong pair key: the initiator refuses.
        let (initiator, open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("open");
        let wrong_key = SecretBytes::new([0xAA; 32]);
        let (_pending, forged_confirm) = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x22; 32],
            kat_observations(),
            &wrong_key,
        )
        .expect("accept under wrong key still authors a confirm");
        let error = initiator
            .finish(&kat_pair_key(), &forged_confirm)
            .expect_err("unkeyed responder must fail");
        assert_eq!(
            error,
            HandshakeError::ConfirmationFailed { role: "responder" }
        );
        assert_eq!(error.degraded_code(), "mesh_frame_auth_failed");

        // Initiator finishes under the wrong pair key: the responder refuses.
        let (initiator, open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("open");
        let (pending, confirm) = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x22; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect("accept");
        // The unkeyed initiator cannot verify the real confirm; simulate a
        // forger that skips verification and MACs the finish with its key.
        let error = initiator
            .finish(&wrong_key, &confirm)
            .expect_err("wrong key cannot verify the responder confirmation");
        assert_eq!(
            error,
            HandshakeError::ConfirmationFailed { role: "responder" }
        );
        let forged_finish = SessionFinishV1 {
            schema: SESSION_FINISH_SCHEMA_V1.to_owned(),
            session_id: confirm.session_id.clone(),
            finish_mac: "00".repeat(32),
        };
        let error = pending
            .complete(&kat_pair_key(), &forged_finish)
            .expect_err("forged finish must fail");
        assert_eq!(
            error,
            HandshakeError::ConfirmationFailed { role: "initiator" }
        );
    }

    #[test]
    fn handshake_rejects_role_reflection_and_session_id_confusion() {
        let (initiator, open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("open");
        let (pending, confirm) = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x22; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect("accept");

        // Reflecting the responder's confirm MAC back as the finish MAC must
        // fail: the role token is bound into the preimage.
        let reflected = SessionFinishV1 {
            schema: SESSION_FINISH_SCHEMA_V1.to_owned(),
            session_id: confirm.session_id.clone(),
            finish_mac: confirm.confirm_mac.clone(),
        };
        let error = pending
            .complete(&kat_pair_key(), &reflected)
            .expect_err("role reflection must fail");
        assert_eq!(
            error,
            HandshakeError::ConfirmationFailed { role: "initiator" }
        );

        // A confirm naming a different session must fail before any crypto.
        let mut renamed = confirm;
        renamed.session_id = "sess-other".to_owned();
        let error = initiator
            .finish(&kat_pair_key(), &renamed)
            .expect_err("session id confusion must fail");
        assert_eq!(error, HandshakeError::SessionIdMismatch);
        assert_eq!(error.degraded_code(), "mesh_frame_target_mismatch");
    }

    #[test]
    fn handshake_binds_current_node_key_observations() {
        let (initiator, open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("open");
        let skewed = HandshakeObservations {
            initiator_node_pubkey: "nodekey:kat-init-observed".to_owned(),
            responder_node_pubkey: "nodekey:rotated-elsewhere".to_owned(),
        };
        let (_pending, confirm) = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x22; 32],
            skewed,
            &kat_pair_key(),
        )
        .expect("accept");
        let error = initiator
            .finish(&kat_pair_key(), &confirm)
            .expect_err("observation disagreement must fail the handshake");
        assert_eq!(
            error,
            HandshakeError::ConfirmationFailed { role: "responder" }
        );
    }

    #[test]
    fn replayed_open_meets_fresh_nonce_and_stale_finish_fails() {
        // Original run captures the finish message.
        let (initiator, open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("open");
        let (pending, confirm) = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x22; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect("accept");
        let (captured_finish, _session) =
            initiator.finish(&kat_pair_key(), &confirm).expect("finish");
        pending
            .complete(&kat_pair_key(), &captured_finish)
            .expect("original run completes");

        // An attacker replays the identical open; the responder mints a fresh
        // nonce, so the captured finish MAC no longer verifies.
        let (replay_pending, _replay_confirm) = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x33; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect("replayed open is accepted only up to confirmation");
        let error = replay_pending
            .complete(&kat_pair_key(), &captured_finish)
            .expect_err("stale finish must fail against a fresh nonce");
        assert_eq!(
            error,
            HandshakeError::ConfirmationFailed { role: "initiator" }
        );
    }

    #[test]
    fn handshake_wire_gates_enforce_schema_size_and_shape() {
        let (_initiator, open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("open");
        let bytes = serde_json::to_vec(&open).expect("encode");
        let decoded = decode_session_open(&bytes).expect("round trip");
        assert_eq!(decoded, open);

        // Oversize rejected before parsing.
        let oversize = vec![b'{'; MAX_HANDSHAKE_MESSAGE_BYTES + 1];
        assert!(matches!(
            decode_session_open(&oversize).expect_err("oversize"),
            HandshakeError::MessageTooLarge { .. }
        ));

        // Wrong-position schema rejected.
        assert!(matches!(
            decode_session_confirm(&bytes).expect_err("wrong schema for position"),
            HandshakeError::SchemaMismatch { .. }
        ));

        // Unknown fields rejected: a handshake message can never smuggle
        // extra identity claims.
        let mut value = serde_json::to_value(&open).expect("to value");
        value
            .as_object_mut()
            .expect("object")
            .insert("claimedOwnerLogin".to_owned(), json!("mallory@example.com"));
        let smuggled = serde_json::to_vec(&value).expect("encode");
        assert!(matches!(
            decode_session_open(&smuggled).expect_err("unknown field"),
            HandshakeError::MalformedMessage { .. }
        ));

        // Empty / self-target bindings never author an open.
        let mut binding = kat_binding();
        binding.initiator_workspace_id = "ws-resp".to_owned();
        assert_eq!(
            InitiatorHandshake::open(binding, [0x11; 32], 1, kat_observations())
                .expect_err("self-target binding"),
            HandshakeError::BindingMismatch {
                field: "source_equals_target"
            }
        );
        let mut binding = kat_binding();
        binding.team_id = String::new();
        assert_eq!(
            InitiatorHandshake::open(binding, [0x11; 32], 1, kat_observations())
                .expect_err("empty team"),
            HandshakeError::BindingMismatch { field: "team_id" }
        );
    }
}
