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

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Shutdown, SocketAddr};
use std::num::NonZeroU64;
use std::sync::atomic::{Ordering, compiler_fence};
use std::time::Duration;

use asupersync::Cx;
use asupersync::io::{AsyncReadExt, AsyncWriteExt};
use asupersync::net::{TcpSocket, TcpStream};
use asupersync::time::{BudgetTimeExt, timeout, wall_now};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::config::{EnvVar, read_env_var};
use crate::mesh::key_store::SecretBytes;

/// Frame v2 schema identifier.
pub const TRANSPORT_FRAME_SCHEMA_V2: &str =
    crate::models::schema::MESH_TAILSCALE_TRANSPORT_FRAME_SCHEMA_V2;

/// The dead v1 schema. Rejected outright; kept only so the rejection path can
/// name what it refused.
pub const TRANSPORT_FRAME_SCHEMA_V1: &str = "ee.mesh.tailscale_transport_frame.v1";

/// Hard outer limit on one encoded frame (unchanged from v1's scaffolding).
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Hard outer limit on one frame payload (unchanged from v1's scaffolding).
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024;

/// Maximum byte length of one frame request/response correlation identifier.
pub const MAX_CORRELATION_ID_BYTES: usize = 128;

/// Maximum byte length of one base or negotiated capability token.
pub const MAX_CAPABILITY_TOKEN_BYTES: usize = 64;

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
    "1eabfe34b812d5ee51de0076ce8466fd4b7962052585a8ee6147c2a5d1fdd305";
#[cfg(test)]
const KAT_I2R_HEX: &str = "a1b31732edd1597b49ec1ac73df6269db5083382defad08a415847d0d4bd9c23";
#[cfg(test)]
const KAT_R2I_HEX: &str = "5a34c82a845b7c212b6465427484caf1cf9a0fb54f6b462ef196d88ac2348768";
#[cfg(test)]
const KAT_FRAME_MAC_HEX: &str = "a139a76c75b6272f52e3b143f483e018889f3dd5c9e456974950a55e35863c82";

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
    /// Locally verified pinned tailnet identifier. Stable IDs are scoped to
    /// this value and must never authenticate across tailnets.
    pub tailnet_id: String,
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
    /// tag, team, tailnet, initiator/responder node IDs, initiator/responder
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
        push_lp(&mut out, self.tailnet_id.as_bytes());
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

    /// Construct a ledger at an explicitly authenticated checkpoint. The
    /// caller is responsible for loading `next` from trusted session state;
    /// ordinary fresh TCP sessions always use [`Self::new`].
    #[must_use]
    pub const fn expecting(next: NonZeroU64) -> Self {
        Self {
            next: next.get(),
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
    validate_frame_binding_shape(binding)?;
    validate_correlation_id(&draft.correlation_id)?;
    if !valid_capability_token(draft.capability.token()) {
        return Err(TransportSessionError::MalformedFrame {
            message: "capability token must match [a-z0-9_]{1,64}".to_owned(),
        });
    }
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
    let frame =
        serde_json::from_slice(bytes).map_err(|error| TransportSessionError::MalformedFrame {
            message: format!("decode v2 frame: {error}"),
        })?;
    validate_frame_wire_shape(&frame)?;
    Ok(frame)
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
    validate_frame_wire_shape(frame)?;
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

    // Authenticate before consuming the exact-next counter. A binding-valid
    // frame with a forged MAC must not burn the slot (T2.7): the peer's
    // legitimate next frame would then look like a duplicate and close the
    // session. Replay still closes after a valid MAC, because ordered TCP
    // has no replay window.
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

    if let FrameCapability::Extension(name) = &frame.capability
        && !negotiated.allows(name)
    {
        return Err(TransportSessionError::ExtensionNotNegotiated { name: name.clone() });
    }

    counters.accept(frame.counter)?;

    Ok(VerifiedFrameV2 {
        frame: frame.clone(),
    })
}

fn validate_frame_binding_shape(binding: &SessionBinding) -> Result<(), TransportSessionError> {
    for (field, value) in [
        ("team_id", binding.team_id.as_str()),
        ("initiator_node_id", binding.initiator_node_id.as_str()),
        ("responder_node_id", binding.responder_node_id.as_str()),
        (
            "initiator_workspace_id",
            binding.initiator_workspace_id.as_str(),
        ),
        (
            "responder_workspace_id",
            binding.responder_workspace_id.as_str(),
        ),
        ("session_id", binding.session_id.as_str()),
    ] {
        validate_frame_identity(field, value)?;
    }
    if binding.initiator_node_id == binding.responder_node_id
        || binding.initiator_workspace_id == binding.responder_workspace_id
    {
        return Err(TransportSessionError::BindingMismatch {
            field: "source_equals_target",
        });
    }
    Ok(())
}

fn validate_frame_wire_shape(frame: &FrameV2) -> Result<(), TransportSessionError> {
    for (field, value) in [
        ("source_node_id", frame.source_node_id.as_str()),
        ("target_node_id", frame.target_node_id.as_str()),
        ("team_id", frame.team_id.as_str()),
        ("source_workspace_id", frame.source_workspace_id.as_str()),
        ("target_workspace_id", frame.target_workspace_id.as_str()),
        ("session_id", frame.session_id.as_str()),
    ] {
        validate_frame_identity(field, value)?;
    }
    validate_correlation_id(&frame.correlation_id)?;
    if !valid_capability_token(frame.capability.token()) {
        return Err(TransportSessionError::MalformedFrame {
            message: "capability token must match [a-z0-9_]{1,64}".to_owned(),
        });
    }
    if decode_hex_32(&frame.payload_hash).is_none() {
        return Err(TransportSessionError::MalformedFrame {
            message: "payloadHash must contain exactly 64 lowercase hexadecimal characters"
                .to_owned(),
        });
    }
    if decode_hex_32(&frame.mac).is_none() {
        return Err(TransportSessionError::MalformedFrame {
            message: "mac must contain exactly 64 lowercase hexadecimal characters".to_owned(),
        });
    }
    Ok(())
}

fn validate_frame_identity(field: &'static str, value: &str) -> Result<(), TransportSessionError> {
    if value.is_empty() || value.len() > MAX_SESSION_BINDING_FIELD_BYTES {
        return Err(TransportSessionError::MalformedFrame {
            message: format!("{field} must contain 1..={MAX_SESSION_BINDING_FIELD_BYTES} bytes"),
        });
    }
    Ok(())
}

fn validate_correlation_id(value: &str) -> Result<(), TransportSessionError> {
    if value.is_empty() || value.len() > MAX_CORRELATION_ID_BYTES {
        return Err(TransportSessionError::MalformedFrame {
            message: format!("correlationId must contain 1..={MAX_CORRELATION_ID_BYTES} bytes"),
        });
    }
    Ok(())
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
pub const SESSION_OPEN_SCHEMA_V1: &str = crate::models::schema::MESH_SESSION_OPEN_SCHEMA_V1;

/// Schema id for the responder's handshake confirm message.
pub const SESSION_CONFIRM_SCHEMA_V1: &str = crate::models::schema::MESH_SESSION_CONFIRM_SCHEMA_V1;

/// Schema id for the initiator's handshake finish message.
pub const SESSION_FINISH_SCHEMA_V1: &str = crate::models::schema::MESH_SESSION_FINISH_SCHEMA_V1;

/// BLAKE3 `derive_key` context for the handshake confirmation key.
pub const SESSION_CONFIRM_KEY_CONTEXT: &str = "ee.team.session.confirm.v1";

/// Version tag leading every handshake confirmation MAC preimage.
pub const SESSION_CONFIRM_MAC_TAG: &str = "ee.mesh.session_confirm_mac.v1";

/// Hard outer limit on one encoded handshake message.
pub const MAX_HANDSHAKE_MESSAGE_BYTES: usize = 4096;

/// Maximum bounded length of one locally verified current node public key.
pub const MAX_NODE_PUBKEY_BYTES: usize = 256;

/// Maximum bounded length of one identity/binding token in the handshake.
pub const MAX_SESSION_BINDING_FIELD_BYTES: usize = 256;

// Known-answer vectors captured from the BLAKE3 reference implementation
// (`b3sum`) over the fixed KAT binding: pair key `0x00..0x1f`, nonces
// `0x11 * 32` / `0x22 * 32`, generation 1, observations
// `nodekey:kat-init-observed` / `nodekey:kat-resp-observed`.
#[cfg(test)]
const KAT_RESPONDER_CONFIRM_MAC_HEX: &str =
    "2068c3c915bc8340bd41fe5213ca43b26da731730f6076f3cdf6ee6453d0d530";
#[cfg(test)]
const KAT_INITIATOR_FINISH_MAC_HEX: &str =
    "b1a79dedfdcd717737454c4958cf5bdf0749114f411fb05500180883b87f1e2c";

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
    /// Pinned tailnet identifier the responder must verify locally.
    pub tailnet_id: String,
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
    /// A locally verified current node-key observation was missing or
    /// unbounded.
    InvalidObservation {
        /// Which endpoint observation was invalid.
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
            | Self::InvalidObservation { .. }
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
                "Mesh session handshake nonce {field} is malformed, zero, reused, or not fresh"
            ),
            Self::InvalidObservation { field } => format!(
                "Mesh session handshake node-key observation {field} is missing or exceeds {MAX_NODE_PUBKEY_BYTES} bytes"
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
    /// Locally verified pinned tailnet identifier.
    pub tailnet_id: String,
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
        validate_observations(&observations)?;
        validate_nonce(&initiator_nonce, "initiator_nonce")?;
        if pair_key_generation == 0 {
            return Err(HandshakeError::GenerationMismatch {
                expected: 1,
                observed: 0,
            });
        }
        let open = SessionOpenV1 {
            schema: SESSION_OPEN_SCHEMA_V1.to_owned(),
            team_id: binding.team_id.clone(),
            tailnet_id: binding.tailnet_id.clone(),
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
        validate_session_confirm_wire_shape(confirm)?;
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
    validate_session_open_wire_shape(open)?;
    validate_observations(&observations)?;
    validate_nonce(&responder_nonce, "responder_nonce")?;
    if expectations.pair_key_generation == 0 {
        return Err(HandshakeError::GenerationMismatch {
            expected: 1,
            observed: 0,
        });
    }
    if open.schema != SESSION_OPEN_SCHEMA_V1 {
        return Err(HandshakeError::SchemaMismatch {
            observed: open.schema.clone(),
        });
    }
    if open.team_id != expectations.team_id {
        return Err(HandshakeError::BindingMismatch { field: "team_id" });
    }
    if open.tailnet_id != expectations.tailnet_id {
        return Err(HandshakeError::BindingMismatch {
            field: "tailnet_id",
        });
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
    if initiator_nonce == responder_nonce {
        return Err(HandshakeError::BadNonce {
            field: "nonce_reuse",
        });
    }
    let binding = SessionBinding {
        team_id: expectations.team_id.clone(),
        tailnet_id: expectations.tailnet_id.clone(),
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
        validate_session_finish_wire_shape(finish)?;
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
    let open = decode_handshake_message(bytes, SESSION_OPEN_SCHEMA_V1)?;
    validate_session_open_wire_shape(&open)?;
    Ok(open)
}

/// Decode one encoded `session_confirm` message.
pub fn decode_session_confirm(bytes: &[u8]) -> Result<SessionConfirmV1, HandshakeError> {
    let confirm = decode_handshake_message(bytes, SESSION_CONFIRM_SCHEMA_V1)?;
    validate_session_confirm_wire_shape(&confirm)?;
    Ok(confirm)
}

/// Decode one encoded `session_finish` message.
pub fn decode_session_finish(bytes: &[u8]) -> Result<SessionFinishV1, HandshakeError> {
    let finish = decode_handshake_message(bytes, SESSION_FINISH_SCHEMA_V1)?;
    validate_session_finish_wire_shape(&finish)?;
    Ok(finish)
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

fn validate_session_open_wire_shape(open: &SessionOpenV1) -> Result<(), HandshakeError> {
    let binding = SessionBinding {
        team_id: open.team_id.clone(),
        tailnet_id: open.tailnet_id.clone(),
        initiator_node_id: open.initiator_node_id.clone(),
        responder_node_id: open.responder_node_id.clone(),
        initiator_workspace_id: open.initiator_workspace_id.clone(),
        responder_workspace_id: open.responder_workspace_id.clone(),
        initiator_stable_id: open.initiator_stable_id.clone(),
        responder_stable_id: open.responder_stable_id.clone(),
        session_id: open.session_id.clone(),
    };
    validate_binding_shape(&binding)?;
    if open.pair_key_generation == 0 {
        return Err(HandshakeError::GenerationMismatch {
            expected: 1,
            observed: 0,
        });
    }
    let _ = decode_nonce(&open.initiator_nonce, "initiator_nonce")?;
    Ok(())
}

fn validate_session_confirm_wire_shape(confirm: &SessionConfirmV1) -> Result<(), HandshakeError> {
    validate_handshake_session_id(&confirm.session_id)?;
    let _ = decode_nonce(&confirm.responder_nonce, "responder_nonce")?;
    validate_handshake_mac(&confirm.confirm_mac, "confirm_mac")
}

fn validate_session_finish_wire_shape(finish: &SessionFinishV1) -> Result<(), HandshakeError> {
    validate_handshake_session_id(&finish.session_id)?;
    validate_handshake_mac(&finish.finish_mac, "finish_mac")
}

fn validate_handshake_session_id(session_id: &str) -> Result<(), HandshakeError> {
    if session_id.is_empty() || session_id.len() > MAX_SESSION_BINDING_FIELD_BYTES {
        return Err(HandshakeError::BindingMismatch {
            field: "session_id",
        });
    }
    Ok(())
}

fn validate_handshake_mac(value: &str, field: &'static str) -> Result<(), HandshakeError> {
    if decode_hex_32(value).is_none() {
        return Err(HandshakeError::MalformedMessage {
            message: format!("{field} must contain exactly 64 lowercase hexadecimal characters"),
        });
    }
    Ok(())
}

fn validate_binding_shape(binding: &SessionBinding) -> Result<(), HandshakeError> {
    let non_empty: [(&'static str, &str); 9] = [
        ("team_id", &binding.team_id),
        ("tailnet_id", &binding.tailnet_id),
        ("initiator_node_id", &binding.initiator_node_id),
        ("responder_node_id", &binding.responder_node_id),
        ("initiator_workspace_id", &binding.initiator_workspace_id),
        ("responder_workspace_id", &binding.responder_workspace_id),
        ("initiator_stable_id", &binding.initiator_stable_id),
        ("responder_stable_id", &binding.responder_stable_id),
        ("session_id", &binding.session_id),
    ];
    for (field, value) in non_empty {
        if value.trim().is_empty() || value.len() > MAX_SESSION_BINDING_FIELD_BYTES {
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

fn valid_bounded_identity(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_SESSION_BINDING_FIELD_BYTES
}

fn untrusted_route_selectors(
    open: &SessionOpenV1,
) -> Result<UntrustedRouteSelectors, SessionChannelError> {
    if !valid_bounded_identity(&open.team_id)
        || !valid_bounded_identity(&open.responder_workspace_id)
        || !valid_bounded_identity(&open.initiator_stable_id)
    {
        return Err(SessionChannelError::Handshake(
            HandshakeError::BindingMismatch {
                field: "route_selector",
            },
        ));
    }
    if open.pair_key_generation == 0 {
        return Err(SessionChannelError::Handshake(
            HandshakeError::GenerationMismatch {
                expected: 1,
                observed: 0,
            },
        ));
    }
    Ok(UntrustedRouteSelectors {
        team_id: open.team_id.clone(),
        responder_workspace_id: open.responder_workspace_id.clone(),
        initiator_stable_id: open.initiator_stable_id.clone(),
        pair_key_generation: open.pair_key_generation,
    })
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
    let nonce = decode_hex_32(value).ok_or(HandshakeError::BadNonce { field })?;
    validate_nonce(&nonce, field)?;
    Ok(nonce)
}

fn validate_nonce(nonce: &[u8; 32], field: &'static str) -> Result<(), HandshakeError> {
    if nonce.iter().all(|byte| *byte == 0) {
        Err(HandshakeError::BadNonce { field })
    } else {
        Ok(())
    }
}

fn validate_observations(observations: &HandshakeObservations) -> Result<(), HandshakeError> {
    for (field, value) in [
        (
            "initiator_node_pubkey",
            observations.initiator_node_pubkey.as_str(),
        ),
        (
            "responder_node_pubkey",
            observations.responder_node_pubkey.as_str(),
        ),
    ] {
        if value.trim().is_empty() || value.len() > MAX_NODE_PUBKEY_BYTES {
            return Err(HandshakeError::InvalidObservation { field });
        }
    }
    Ok(())
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

// ---------------------------------------------------------------------------
// Real Asupersync TCP session channel (T2.1 slice 4).
// ---------------------------------------------------------------------------

/// Schema for the MAC-authenticated capability offer/selection payload.
pub const CAPABILITY_NEGOTIATION_SCHEMA_V1: &str =
    crate::models::schema::MESH_SESSION_CAPABILITY_NEGOTIATION_SCHEMA_V1;

/// Maximum number of capabilities one endpoint may offer.
pub const MAX_SESSION_CAPABILITIES: usize = 16;

/// Maximum verified messages buffered while the caller services the opposite
/// half of a bidirectional request/response exchange.
pub const MAX_BUFFERED_SESSION_MESSAGES: usize = 32;

/// One embedded wire-schema descriptor owned by this transport module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportWireSchema {
    pub id: &'static str,
    pub document: &'static str,
}

/// Module-local JSON Schema catalog for every length-prefixed session
/// message. The canonical CLI registry exports the same documents.
pub const TRANSPORT_WIRE_SCHEMAS: &[TransportWireSchema] = &[
    TransportWireSchema {
        id: TRANSPORT_FRAME_SCHEMA_V2,
        document: include_str!("../../docs/schemas/ee.mesh.tailscale_transport_frame.v2.json"),
    },
    TransportWireSchema {
        id: SESSION_OPEN_SCHEMA_V1,
        document: include_str!("../../docs/schemas/ee.mesh.session_open.v1.json"),
    },
    TransportWireSchema {
        id: SESSION_CONFIRM_SCHEMA_V1,
        document: include_str!("../../docs/schemas/ee.mesh.session_confirm.v1.json"),
    },
    TransportWireSchema {
        id: SESSION_FINISH_SCHEMA_V1,
        document: include_str!("../../docs/schemas/ee.mesh.session_finish.v1.json"),
    },
    TransportWireSchema {
        id: CAPABILITY_NEGOTIATION_SCHEMA_V1,
        document: include_str!("../../docs/schemas/ee.mesh.session_capability_negotiation.v1.json"),
    },
];

/// Default per-operation socket deadline.
pub const DEFAULT_SESSION_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Default upper bound accepted for a peer's application budget request.
pub const DEFAULT_MAX_REQUEST_BUDGET_MS: u64 = 30_000;

/// Default terminal cap on authenticated frame-v2 messages in both
/// directions combined, including capability negotiation.
pub const DEFAULT_MAX_AUTHENTICATED_FRAMES: u64 = 4_096;

/// Default terminal cap on length-prefixed authenticated frame-v2 bytes in
/// both directions combined, including capability negotiation.
pub const DEFAULT_MAX_AUTHENTICATED_BYTES: u64 = 64 * 1024 * 1024;

/// Capabilities an endpoint is willing to dispatch on an authenticated
/// session. Tokens are sorted for stable negotiation; duplicates are refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCapabilities {
    tokens: Vec<String>,
}

impl SessionCapabilities {
    /// The four base T2.1 capabilities.
    #[must_use]
    pub fn base() -> Self {
        Self {
            tokens: vec![
                "body_fetch".to_owned(),
                "event_fetch".to_owned(),
                "hello".to_owned(),
                "identity_attest".to_owned(),
                "summary".to_owned(),
            ],
        }
    }

    /// Validate and canonicalize capability tokens. `hello` is mandatory
    /// because it carries the authenticated negotiation exchange itself.
    pub fn new(tokens: impl IntoIterator<Item = String>) -> Result<Self, SessionChannelError> {
        let mut tokens = tokens.into_iter().collect::<Vec<_>>();
        let original_len = tokens.len();
        tokens.sort();
        tokens.dedup();
        if tokens.len() != original_len {
            return Err(SessionChannelError::Authentication {
                message: "capability negotiation contains duplicate tokens".to_owned(),
            });
        }
        if tokens.is_empty() || tokens.len() > MAX_SESSION_CAPABILITIES {
            return Err(SessionChannelError::Authentication {
                message: format!(
                    "capability set must contain 1..={MAX_SESSION_CAPABILITIES} entries"
                ),
            });
        }
        for token in &tokens {
            if !valid_capability_token(token) {
                return Err(SessionChannelError::Authentication {
                    message: format!("invalid mesh capability token {token:?}"),
                });
            }
        }
        if !tokens.iter().any(|token| token == "hello") {
            return Err(SessionChannelError::Authentication {
                message: "mesh capability negotiation requires hello".to_owned(),
            });
        }
        Ok(Self { tokens })
    }

    /// Canonical negotiated tokens.
    #[must_use]
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    /// Whether a frame capability was selected for this session.
    #[must_use]
    pub fn allows(&self, capability: &FrameCapability) -> bool {
        self.tokens.iter().any(|token| token == capability.token())
    }

    fn extensions(&self) -> NegotiatedExtensions {
        NegotiatedExtensions::from_names(
            self.tokens
                .iter()
                .filter(|token| {
                    !matches!(
                        token.as_str(),
                        "hello" | "summary" | "event_fetch" | "body_fetch"
                    )
                })
                .cloned(),
        )
    }

    fn intersection(&self, offered: &Self) -> Result<Self, SessionChannelError> {
        Self::new(
            self.tokens
                .iter()
                .filter(|token| offered.tokens.binary_search(token).is_ok())
                .cloned(),
        )
    }
}

fn valid_capability_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_CAPABILITY_TOKEN_BYTES
        && token
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

impl Default for SessionCapabilities {
    fn default() -> Self {
        Self::base()
    }
}

/// Deadlines and peer-budget limits for one socket session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionChannelLimits {
    /// Deadline for the initiator's TCP connect.
    pub connect_timeout: Duration,
    /// Deadline for each complete length-prefixed read or write.
    pub io_timeout: Duration,
    /// Largest application processing budget accepted from the peer.
    pub max_requested_budget_ms: u64,
    /// Terminal cap on authenticated frames in both directions combined.
    pub max_authenticated_frames: u64,
    /// Terminal cap on length-prefixed authenticated frame bytes in both
    /// directions combined.
    pub max_authenticated_bytes: u64,
}

impl Default for SessionChannelLimits {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_SESSION_IO_TIMEOUT,
            io_timeout: DEFAULT_SESSION_IO_TIMEOUT,
            max_requested_budget_ms: DEFAULT_MAX_REQUEST_BUDGET_MS,
            max_authenticated_frames: DEFAULT_MAX_AUTHENTICATED_FRAMES,
            max_authenticated_bytes: DEFAULT_MAX_AUTHENTICATED_BYTES,
        }
    }
}

/// Authenticated frame-v2 resources consumed by one session. Handshake bytes
/// are excluded; capability negotiation is included because it uses frame v2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedSessionUsage {
    /// Verified inbound plus successfully written outbound frames.
    pub frames: u64,
    /// Four-byte prefixes plus frame bodies for those frames.
    pub wire_bytes: u64,
}

/// Initiator inputs supplied from enrolled peer state and fresh transport
/// observations. The session channel never discovers or persists identity.
#[derive(Debug)]
pub struct InitiatorSessionConfig {
    /// Exact local source address selected by the caller's Tailscale route
    /// authority. Port zero requests an ephemeral source port.
    pub local_address: SocketAddr,
    pub binding: SessionBinding,
    pub pair_key: SecretBytes,
    pub pair_key_generation: u64,
    pub observations: HandshakeObservations,
    pub capabilities: SessionCapabilities,
    pub limits: SessionChannelLimits,
}

/// Responder inputs selected only after the listener owner has accepted the
/// stream, resolved its source with WhoIs, and matched a registered route.
/// This module performs no listen, accept, WhoIs, or key lookup work.
#[derive(Debug)]
pub struct AcceptedSessionConfig {
    pub expectations: ResponderExpectations,
    pub pair_key: SecretBytes,
    pub observations: HandshakeObservations,
    pub capabilities: SessionCapabilities,
    pub limits: SessionChannelLimits,
}

/// The only unauthenticated selectors exposed to the responder broker. These
/// values are bounded wire claims used solely to choose local verification
/// state; they are never peer identity or authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedRouteSelectors {
    pub team_id: String,
    pub responder_workspace_id: String,
    pub initiator_stable_id: String,
    pub pair_key_generation: u64,
}

/// Accepted-source identity returned by a fresh LocalAPI WhoIs lookup (or the
/// plan's freshly queried status-map fallback). The session layer checks this
/// attestation against the kernel-observed source and the locally selected
/// expectations before it reads pair-key material into the handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedSourceAttestation {
    queried_ip: IpAddr,
    tailnet_id: String,
    stable_id: String,
    current_node_pubkey: String,
}

impl AcceptedSourceAttestation {
    /// Construct the transport handoff from a caller-verified WhoIs result.
    /// This constructor validates shape only; T2.2 owns the LocalAPI query and
    /// must pass the exact accepted source IP rather than a wire/header claim.
    pub fn from_local_whois(
        queried_ip: IpAddr,
        tailnet_id: impl Into<String>,
        stable_id: impl Into<String>,
        current_node_pubkey: impl Into<String>,
    ) -> Result<Self, SessionChannelError> {
        let tailnet_id = tailnet_id.into();
        let stable_id = stable_id.into();
        let current_node_pubkey = current_node_pubkey.into();
        if queried_ip.is_unspecified()
            || !valid_bounded_identity(&tailnet_id)
            || !valid_bounded_identity(&stable_id)
            || current_node_pubkey.trim().is_empty()
            || current_node_pubkey.len() > MAX_NODE_PUBKEY_BYTES
        {
            return Err(SessionChannelError::Authentication {
                message: "accepted-source WhoIs attestation is incomplete or malformed".to_owned(),
            });
        }
        Ok(Self {
            queried_ip,
            tailnet_id,
            stable_id,
            current_node_pubkey,
        })
    }
}

/// Route/key result selected by the responder broker. `G` is an opaque
/// source/global admission permit; the session layer holds it until the
/// handshake succeeds or fails so pre-auth capacity cannot be released early.
#[derive(Debug)]
pub struct ResolvedAcceptedRoute<G> {
    config: AcceptedSessionConfig,
    source: AcceptedSourceAttestation,
    admission_guard: G,
}

impl<G> ResolvedAcceptedRoute<G> {
    #[must_use]
    pub const fn new(
        config: AcceptedSessionConfig,
        source: AcceptedSourceAttestation,
        admission_guard: G,
    ) -> Self {
        Self {
            config,
            source,
            admission_guard,
        }
    }
}

/// One bounded `session_open` read from an accepted socket but not yet
/// authenticated. This stays private so callers cannot hold a socket beyond
/// the original accept budget or resume authentication under a fresh `Cx`.
#[derive(Debug)]
struct PendingAcceptedSession {
    stream: TcpStream,
    untrusted_open: SessionOpenV1,
    peer_address: SocketAddr,
    limits: SessionChannelLimits,
}

/// One application message returned only after all frame, capability,
/// budget, direction, counter, and correlation gates have passed.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionMessage {
    pub correlation_id: String,
    pub capability: FrameCapability,
    pub requested_budget_ms: u64,
    pub payload: JsonValue,
}

/// Machine-readable status for an enrolled route that has no pair key.
///
/// This is a transport-owned handoff contract, not a claim that a public
/// pairing ceremony exists. The `.3.3`/`.3.5` broker and caller slices may
/// render this guidance without reclassifying a missing credential as a bad
/// MAC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingGuidanceStatus {
    /// The enrolled route cannot authenticate until a pair key is installed.
    PairingRequired,
}

/// Availability of the public pairing ceremony at the current milestone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingCeremonyAvailability {
    /// M1 has the authenticated transport substrate but no public pairing
    /// command; callers must not invent or print one.
    UnavailableInM1,
}

/// Structured, secret-free recovery guidance for a missing pair key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingRequiredGuidance {
    /// Stable typed status for downstream renderers.
    pub status: PairingGuidanceStatus,
    /// Whether a real public ceremony can currently satisfy the requirement.
    pub ceremony: PairingCeremonyAvailability,
    /// Existing degraded classification owned by T2.1's emission registry.
    pub degraded_code: &'static str,
    /// Severity of refusing an unkeyed authenticated transport operation.
    pub severity: &'static str,
    /// Missing pair material blocks the authenticated session.
    pub blocks_authenticated_transport: bool,
    /// The original operation may be retried after a real ceremony succeeds.
    pub retry_after_pairing: bool,
    /// A ready-to-run command only when a real production ceremony exists.
    /// This remains `None` in M1 rather than advertising a fictional surface.
    pub command: Option<&'static str>,
}

impl PairingRequiredGuidance {
    #[must_use]
    pub const fn current() -> Self {
        Self {
            status: PairingGuidanceStatus::PairingRequired,
            ceremony: PairingCeremonyAvailability::UnavailableInM1,
            degraded_code: "mesh_frame_auth_failed",
            severity: "high",
            blocks_authenticated_transport: true,
            retry_after_pairing: true,
            command: None,
        }
    }

    #[must_use]
    pub const fn message(self) -> &'static str {
        "Mesh peer pairing is required before authenticated transport can start"
    }

    #[must_use]
    pub const fn repair(self) -> &'static str {
        "No public pairing ceremony exists in M1; complete the M2/M3 pairing flow when that production surface is installed, then retry."
    }
}

/// Convert an absent locally enrolled pair credential into the transport's
/// typed pairing-required contract. The caller retains ownership of lookup,
/// enrollment, and persistence; this function performs no I/O and never
/// creates key material on an inbound path.
pub fn require_pair_credential<T>(credential: Option<T>) -> Result<T, SessionChannelError> {
    credential.ok_or(SessionChannelError::PairingRequired)
}

/// Fail-closed error surface for real socket setup and frame exchange.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionChannelError {
    /// Global transport kill switch was set before socket/authentication work.
    TransportDisabled,
    /// A security-sensitive transport environment value was invalid and
    /// therefore failed closed.
    InvalidConfiguration { variable: &'static str },
    /// Caller-supplied session limits were incapable of bounding a session.
    InvalidLimits { message: String },
    /// The `Cx` was cancelled or its budget was exhausted.
    Cancelled {
        phase: &'static str,
        message: String,
    },
    /// A bounded operation exceeded its deadline.
    Timeout { phase: &'static str },
    /// A connect or socket I/O operation failed.
    Io {
        phase: &'static str,
        message: String,
    },
    /// The OS CSPRNG could not mint session freshness.
    Randomness { message: String },
    /// The three-message pair-key handshake failed.
    Handshake(HandshakeError),
    /// The locally selected route has no pair key. This is distinct from a
    /// peer presenting an invalid MAC and carries honest M1 recovery guidance.
    PairingRequired,
    /// Frame-v2 decoding or authentication failed.
    Frame(TransportSessionError),
    /// An authenticated negotiation or request/response invariant failed.
    Authentication { message: String },
    /// The peer safely half-closed before the expected response arrived.
    UnexpectedHalfClose,
    /// A local terminal per-session resource cap was reached.
    SessionBudgetExhausted { resource: &'static str },
    /// This local session is already closed.
    Closed,
}

impl SessionChannelError {
    /// Stable degraded code owned by T2.1.
    #[must_use]
    pub const fn degraded_code(&self) -> &'static str {
        match self {
            Self::TransportDisabled
            | Self::InvalidConfiguration { .. }
            | Self::InvalidLimits { .. }
            | Self::Cancelled { .. }
            | Self::Timeout { .. }
            | Self::Io { .. }
            | Self::UnexpectedHalfClose
            | Self::SessionBudgetExhausted { .. } => "mesh_transport_unreachable",
            // Fresh local entropy is a prerequisite for an authenticated
            // transport session, so callers cannot reach the peer when it is
            // unavailable. A locally closed channel is likewise unavailable
            // to the caller; neither condition is peer-frame authentication.
            Self::Randomness { .. } | Self::Closed => "mesh_transport_unreachable",
            Self::Handshake(error) => error.degraded_code(),
            Self::PairingRequired => "mesh_frame_auth_failed",
            Self::Frame(error) => error.degraded_code(),
            Self::Authentication { .. } => "mesh_frame_auth_failed",
        }
    }

    /// Structured guidance is present only for an actually missing local pair
    /// credential. Bad-MAC and binding failures must never be laundered into a
    /// pairing prompt.
    #[must_use]
    pub const fn pairing_guidance(&self) -> Option<PairingRequiredGuidance> {
        match self {
            Self::PairingRequired => Some(PairingRequiredGuidance::current()),
            _ => None,
        }
    }

    /// Human-readable, secret-free diagnostic.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::TransportDisabled => {
                "Mesh TCP transport is disabled by EE_MESH_TRANSPORT_DISABLED".to_owned()
            }
            Self::InvalidConfiguration { variable } => format!(
                "Mesh TCP transport refused invalid security-sensitive configuration in {variable}"
            ),
            Self::InvalidLimits { message } => {
                format!("Mesh TCP transport refused invalid session limits: {message}")
            }
            Self::Cancelled { phase, message } => {
                format!("Mesh TCP session cancelled during {phase}: {message}")
            }
            Self::Timeout { phase } => {
                format!("Mesh TCP session deadline elapsed during {phase}")
            }
            Self::Io { phase, message } => {
                format!("Mesh TCP session I/O failed during {phase}: {message}")
            }
            Self::Randomness { message } => {
                format!("Mesh TCP session could not mint fresh session material: {message}")
            }
            Self::Handshake(error) => error.message(),
            Self::PairingRequired => PairingRequiredGuidance::current().message().to_owned(),
            Self::Frame(error) => error.message(),
            Self::Authentication { message } => {
                format!("Mesh TCP session authentication failed: {message}")
            }
            Self::UnexpectedHalfClose => {
                "Mesh TCP peer half-closed before its correlated response".to_owned()
            }
            Self::SessionBudgetExhausted { resource } => {
                format!("Mesh TCP session reached its terminal authenticated {resource} budget")
            }
            Self::Closed => "Mesh TCP session is closed".to_owned(),
        }
    }
}

impl fmt::Display for SessionChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SessionChannelError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionRole {
    Initiator,
    Responder,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingRequest {
    capability: FrameCapability,
}

impl SessionRole {
    const fn outbound_direction(self) -> SessionDirection {
        match self {
            Self::Initiator => SessionDirection::InitiatorToResponder,
            Self::Responder => SessionDirection::ResponderToInitiator,
        }
    }

    const fn inbound_direction(self) -> SessionDirection {
        match self {
            Self::Initiator => SessionDirection::ResponderToInitiator,
            Self::Responder => SessionDirection::InitiatorToResponder,
        }
    }
}

/// Authenticated, negotiated frame-v2 channel over a real Asupersync TCP
/// stream. Dropping the stream closes both halves; explicit half-close keeps
/// the read half available for a final response.
#[derive(Debug)]
pub struct AuthenticatedTransportSession {
    stream: TcpStream,
    established: EstablishedSession,
    role: SessionRole,
    capabilities: SessionCapabilities,
    limits: SessionChannelLimits,
    outbound_exhausted: bool,
    pending_outbound: BTreeMap<String, PendingRequest>,
    pending_inbound: BTreeMap<String, PendingRequest>,
    buffered_requests: VecDeque<SessionMessage>,
    buffered_responses: BTreeMap<String, SessionMessage>,
    authenticated_frames: u64,
    authenticated_wire_bytes: u64,
    write_closed: bool,
    peer_write_closed: bool,
    closed: bool,
}

impl AuthenticatedTransportSession {
    fn new(
        stream: TcpStream,
        established: EstablishedSession,
        role: SessionRole,
        capabilities: SessionCapabilities,
        limits: SessionChannelLimits,
    ) -> Self {
        Self {
            stream,
            established,
            role,
            capabilities,
            limits,
            outbound_exhausted: false,
            pending_outbound: BTreeMap::new(),
            pending_inbound: BTreeMap::new(),
            buffered_requests: VecDeque::new(),
            buffered_responses: BTreeMap::new(),
            authenticated_frames: 0,
            authenticated_wire_bytes: 0,
            write_closed: false,
            peer_write_closed: false,
            closed: false,
        }
    }

    /// Binding proven by the three-message handshake.
    #[must_use]
    pub const fn binding(&self) -> &SessionBinding {
        &self.established.binding
    }

    /// Capabilities selected by both authenticated endpoints.
    #[must_use]
    pub const fn capabilities(&self) -> &SessionCapabilities {
        &self.capabilities
    }

    /// Current cumulative authenticated frame-v2 usage for this session.
    #[must_use]
    pub const fn authenticated_usage(&self) -> AuthenticatedSessionUsage {
        AuthenticatedSessionUsage {
            frames: self.authenticated_frames,
            wire_bytes: self.authenticated_wire_bytes,
        }
    }

    /// Send a request and remember its correlation until a verified response.
    pub async fn send_request(
        &mut self,
        cx: &Cx,
        message: SessionMessage,
    ) -> Result<(), SessionChannelError> {
        if let Err(error) = self.validate_application_message(&message) {
            return self.fail(error);
        }
        if self.pending_outbound.contains_key(&message.correlation_id) {
            return self.fail(SessionChannelError::Authentication {
                message: "duplicate outstanding request correlation".to_owned(),
            });
        }
        if self.pending_outbound.len() >= MAX_BUFFERED_SESSION_MESSAGES {
            return self.fail(SessionChannelError::Authentication {
                message: "outstanding outbound request ledger exceeded its bounded capacity"
                    .to_owned(),
            });
        }
        let correlation = message.correlation_id.clone();
        let pending = PendingRequest {
            capability: message.capability.clone(),
        };
        self.send_message(cx, FrameKind::Request, message).await?;
        self.pending_outbound.insert(correlation, pending);
        Ok(())
    }

    /// Receive the next verified request. `Ok(None)` is a clean peer
    /// half-close before a new prefix, and performs no application mutation.
    pub async fn receive_request(
        &mut self,
        cx: &Cx,
    ) -> Result<Option<SessionMessage>, SessionChannelError> {
        if let Some(message) = self.buffered_requests.pop_front() {
            return Ok(Some(message));
        }
        loop {
            let Some(frame) = self.receive_verified(cx).await? else {
                if !self.pending_outbound.is_empty() {
                    return self.fail(SessionChannelError::UnexpectedHalfClose);
                }
                return Ok(None);
            };
            match frame.frame.kind {
                FrameKind::Request => return self.accept_inbound_request(&frame.frame).map(Some),
                FrameKind::Response => {
                    let message = self.accept_inbound_response(&frame.frame)?;
                    self.buffer_response(message)?;
                }
            }
        }
    }

    /// Run processing for a verified inbound request under the smaller of its
    /// authenticated `requestedBudgetMs` and the caller's current `Cx`
    /// deadline. Callers must apply durable mutation only after this returns
    /// `Ok`; timeout/cancellation drops the processing future and closes the
    /// session without authorizing a response.
    pub async fn process_request<T, F>(
        &mut self,
        cx: &Cx,
        request: &SessionMessage,
        future: F,
    ) -> Result<T, SessionChannelError>
    where
        F: Future<Output = T>,
    {
        let Some(pending) = self.pending_inbound.get(&request.correlation_id) else {
            return self.fail(SessionChannelError::Authentication {
                message: "processing input is not a verified outstanding request".to_owned(),
            });
        };
        if pending.capability != request.capability || request.requested_budget_ms == 0 {
            return self.fail(SessionChannelError::Authentication {
                message: "processing input does not match its authenticated request metadata"
                    .to_owned(),
            });
        }
        let requested = Duration::from_millis(request.requested_budget_ms);
        let now = wall_now();
        let effective = cx
            .budget()
            .remaining_duration(now)
            .map_or(requested, |remaining| remaining.min(requested));
        if let Err(error) = checkpoint(cx, "request processing") {
            return self.fail(error);
        }
        let _ambient = Cx::set_current(Some(cx.clone()));
        match timeout(now, effective, future).await {
            Ok(value) => {
                if let Err(error) = checkpoint(cx, "request processing") {
                    return self.fail(error);
                }
                Ok(value)
            }
            Err(_) => {
                let error = match checkpoint(cx, "request processing") {
                    Ok(()) => SessionChannelError::Timeout {
                        phase: "request processing",
                    },
                    Err(error) => error,
                };
                self.fail(error)
            }
        }
    }

    /// Send a response only for a request this session returned successfully.
    pub async fn send_response(
        &mut self,
        cx: &Cx,
        message: SessionMessage,
    ) -> Result<(), SessionChannelError> {
        if let Err(error) = self.validate_application_message(&message) {
            return self.fail(error);
        }
        let Some(pending) = self.pending_inbound.get(&message.correlation_id) else {
            return self.fail(SessionChannelError::Authentication {
                message: "response correlation does not name a verified inbound request".to_owned(),
            });
        };
        if pending.capability != message.capability {
            return self.fail(SessionChannelError::Authentication {
                message: "response capability does not match the correlated request".to_owned(),
            });
        }
        let correlation = message.correlation_id.clone();
        self.send_message(cx, FrameKind::Response, message).await?;
        self.pending_inbound.remove(&correlation);
        Ok(())
    }

    /// Receive exactly the response correlated to `correlation_id`.
    pub async fn receive_response(
        &mut self,
        cx: &Cx,
        correlation_id: &str,
    ) -> Result<SessionMessage, SessionChannelError> {
        if !self.pending_outbound.contains_key(correlation_id) {
            return self.fail(SessionChannelError::Authentication {
                message: "response wait does not name an outstanding request".to_owned(),
            });
        }
        if let Some(message) = self.buffered_responses.remove(correlation_id) {
            self.pending_outbound.remove(correlation_id);
            return Ok(message);
        }
        loop {
            let Some(frame) = self.receive_verified(cx).await? else {
                return self.fail(SessionChannelError::UnexpectedHalfClose);
            };
            match frame.frame.kind {
                FrameKind::Request => {
                    let message = self.accept_inbound_request(&frame.frame)?;
                    self.buffer_request(message)?;
                }
                FrameKind::Response => {
                    let message = self.accept_inbound_response(&frame.frame)?;
                    if message.correlation_id == correlation_id {
                        self.pending_outbound.remove(correlation_id);
                        return Ok(message);
                    }
                    self.buffer_response(message)?;
                }
            }
        }
    }

    fn accept_inbound_request(
        &mut self,
        frame: &FrameV2,
    ) -> Result<SessionMessage, SessionChannelError> {
        let message = session_message_from_frame(frame);
        let pending = PendingRequest {
            capability: message.capability.clone(),
        };
        if self.pending_inbound.len() >= MAX_BUFFERED_SESSION_MESSAGES {
            return self.fail(SessionChannelError::Authentication {
                message: "outstanding inbound request ledger exceeded its bounded capacity"
                    .to_owned(),
            });
        }
        if self
            .pending_inbound
            .insert(message.correlation_id.clone(), pending)
            .is_some()
        {
            return self.fail(SessionChannelError::Authentication {
                message: "duplicate outstanding inbound correlation".to_owned(),
            });
        }
        Ok(message)
    }

    fn accept_inbound_response(
        &mut self,
        frame: &FrameV2,
    ) -> Result<SessionMessage, SessionChannelError> {
        let message = session_message_from_frame(frame);
        let Some(pending) = self.pending_outbound.get(&message.correlation_id) else {
            return self.fail(SessionChannelError::Authentication {
                message: "response correlation does not name an outstanding request".to_owned(),
            });
        };
        if pending.capability != message.capability {
            return self.fail(SessionChannelError::Authentication {
                message: "response capability does not match the correlated request".to_owned(),
            });
        }
        Ok(message)
    }

    fn buffer_request(&mut self, message: SessionMessage) -> Result<(), SessionChannelError> {
        if self.buffered_requests.len() >= MAX_BUFFERED_SESSION_MESSAGES {
            return self.fail(SessionChannelError::Authentication {
                message: "authenticated request inbox exceeded its bounded capacity".to_owned(),
            });
        }
        self.buffered_requests.push_back(message);
        Ok(())
    }

    fn buffer_response(&mut self, message: SessionMessage) -> Result<(), SessionChannelError> {
        if self.buffered_responses.len() >= MAX_BUFFERED_SESSION_MESSAGES
            || self
                .buffered_responses
                .insert(message.correlation_id.clone(), message)
                .is_some()
        {
            return self.fail(SessionChannelError::Authentication {
                message: "authenticated response inbox is full or duplicated".to_owned(),
            });
        }
        Ok(())
    }

    /// Safely half-close the write side while retaining the read side.
    pub async fn shutdown_write(&mut self, cx: &Cx) -> Result<(), SessionChannelError> {
        if self.closed {
            return Err(SessionChannelError::Closed);
        }
        if self.write_closed {
            return Ok(());
        }
        let result = await_io(
            cx,
            self.limits.io_timeout,
            "write half-close",
            AsyncWriteExt::shutdown(&mut self.stream),
        )
        .await;
        if let Err(error) = result {
            return self.fail(error);
        }
        self.write_closed = true;
        Ok(())
    }

    /// Close both directions. Idempotent and best-effort.
    pub fn close(&mut self) {
        self.closed = true;
        self.write_closed = true;
        self.peer_write_closed = true;
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    fn validate_application_message(
        &self,
        message: &SessionMessage,
    ) -> Result<(), SessionChannelError> {
        if self.closed || self.write_closed {
            return Err(SessionChannelError::Closed);
        }
        if message.correlation_id.is_empty()
            || message.correlation_id.len() > MAX_CORRELATION_ID_BYTES
        {
            return Err(SessionChannelError::Authentication {
                message: format!(
                    "correlation id must contain 1..={MAX_CORRELATION_ID_BYTES} bytes"
                ),
            });
        }
        if !self.capabilities.allows(&message.capability) {
            return Err(SessionChannelError::Authentication {
                message: format!(
                    "capability {:?} was not selected for this session",
                    message.capability.token()
                ),
            });
        }
        if message.requested_budget_ms > self.limits.max_requested_budget_ms {
            return Err(SessionChannelError::Authentication {
                message: "requested application budget exceeds the session limit".to_owned(),
            });
        }
        Ok(())
    }

    async fn send_message(
        &mut self,
        cx: &Cx,
        kind: FrameKind,
        message: SessionMessage,
    ) -> Result<(), SessionChannelError> {
        if self.outbound_exhausted {
            return self.fail(SessionChannelError::Frame(
                TransportSessionError::ReplayRejected {
                    violation: CounterViolation::Exhausted,
                    expected: u64::MAX,
                    observed: u64::MAX,
                },
            ));
        }
        let counter = self.established.next_outbound;
        let frame = match sign_frame(
            &self.established.binding,
            &self.established.keys,
            FrameDraft {
                direction: self.role.outbound_direction(),
                counter,
                correlation_id: message.correlation_id,
                kind,
                capability: message.capability,
                requested_budget_ms: message.requested_budget_ms,
                payload: message.payload,
            },
        ) {
            Ok(frame) => frame,
            Err(error) => return self.fail(SessionChannelError::Frame(error)),
        };
        let encoded = match serde_json::to_vec(&frame) {
            Ok(encoded) => encoded,
            Err(error) => {
                return self.fail(SessionChannelError::Frame(
                    TransportSessionError::MalformedFrame {
                        message: format!("serialize frame: {error}"),
                    },
                ));
            }
        };
        if let Err(error) = self.reserve_authenticated_frame(encoded.len()) {
            return self.fail(error);
        }
        if let Err(error) = write_packet(
            &mut self.stream,
            cx,
            self.limits.io_timeout,
            "frame write",
            &encoded,
            MAX_FRAME_BYTES,
        )
        .await
        {
            return self.fail(error);
        }
        if counter == u64::MAX {
            self.outbound_exhausted = true;
        } else {
            self.established.next_outbound += 1;
        }
        Ok(())
    }

    async fn receive_verified(
        &mut self,
        cx: &Cx,
    ) -> Result<Option<VerifiedFrameV2>, SessionChannelError> {
        if self.closed || self.peer_write_closed {
            return Err(SessionChannelError::Closed);
        }
        let encoded = match read_packet(
            &mut self.stream,
            cx,
            self.limits.io_timeout,
            "frame read",
            MAX_FRAME_BYTES,
        )
        .await
        {
            Ok(Some(encoded)) => encoded,
            Ok(None) => {
                self.peer_write_closed = true;
                return Ok(None);
            }
            Err(error) => return self.fail(error),
        };
        let frame = match decode_frame(&encoded) {
            Ok(frame) => frame,
            Err(error) => return self.fail(SessionChannelError::Frame(error)),
        };
        let verified = match verify_frame(
            &frame,
            &self.established.binding,
            self.role.inbound_direction(),
            &mut self.established.inbound,
            &self.established.keys,
            &self.capabilities.extensions(),
        ) {
            Ok(verified) => verified,
            Err(error) => return self.fail(SessionChannelError::Frame(error)),
        };
        if !self.capabilities.allows(&verified.frame.capability) {
            return self.fail(SessionChannelError::Frame(
                TransportSessionError::ExtensionNotNegotiated {
                    name: verified.frame.capability.token().to_owned(),
                },
            ));
        }
        if verified.frame.requested_budget_ms > self.limits.max_requested_budget_ms {
            return self.fail(SessionChannelError::Frame(
                TransportSessionError::MalformedFrame {
                    message: "requested application budget exceeds the session limit".to_owned(),
                },
            ));
        }
        if let Err(error) = self.reserve_authenticated_frame(encoded.len()) {
            return self.fail(error);
        }
        Ok(Some(verified))
    }

    fn fail<T>(&mut self, error: SessionChannelError) -> Result<T, SessionChannelError> {
        self.close();
        Err(error)
    }

    fn reserve_authenticated_frame(
        &mut self,
        body_bytes: usize,
    ) -> Result<(), SessionChannelError> {
        let next_frames = self
            .authenticated_frames
            .checked_add(1)
            .ok_or(SessionChannelError::SessionBudgetExhausted { resource: "frame" })?;
        if next_frames > self.limits.max_authenticated_frames {
            return Err(SessionChannelError::SessionBudgetExhausted { resource: "frame" });
        }
        let packet_bytes = u64::try_from(body_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(4))
            .ok_or(SessionChannelError::SessionBudgetExhausted { resource: "byte" })?;
        let next_bytes = self
            .authenticated_wire_bytes
            .checked_add(packet_bytes)
            .ok_or(SessionChannelError::SessionBudgetExhausted { resource: "byte" })?;
        if next_bytes > self.limits.max_authenticated_bytes {
            return Err(SessionChannelError::SessionBudgetExhausted { resource: "byte" });
        }
        self.authenticated_frames = next_frames;
        self.authenticated_wire_bytes = next_bytes;
        Ok(())
    }
}

/// Initiate TCP, run the three-message handshake, and authenticate capability
/// negotiation. The kill switch is checked before connect or authentication.
pub async fn connect_authenticated_session(
    cx: &Cx,
    address: SocketAddr,
    mut config: InitiatorSessionConfig,
) -> Result<AuthenticatedTransportSession, SessionChannelError> {
    refuse_if_transport_disabled()?;
    checkpoint(cx, "connect")?;
    validate_limits(config.limits)?;
    if config.local_address.ip().is_unspecified()
        || address.ip().is_unspecified()
        || config.local_address.is_ipv4() != address.is_ipv4()
    {
        return Err(SessionChannelError::InvalidLimits {
            message: "mesh connect requires concrete same-family local and remote addresses"
                .to_owned(),
        });
    }
    let initiator_nonce = fresh_bytes()?;
    config.binding.session_id = fresh_session_id()?;
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
    .map_err(|error| SessionChannelError::Io {
        phase: "socket create",
        message: error.to_string(),
    })?;
    socket
        .bind(config.local_address)
        .map_err(|error| SessionChannelError::Io {
            phase: "source bind",
            message: error.to_string(),
        })?;
    let stream = await_io(
        cx,
        config.limits.connect_timeout,
        "connect",
        socket.connect(address),
    )
    .await?;
    run_initiator_handshake(cx, stream, config, initiator_nonce).await
}

/// Read one bounded `session_open` for the in-scope resolver below. The value
/// never escapes the accept orchestration boundary.
async fn read_pending_accepted_session(
    cx: &Cx,
    mut stream: TcpStream,
    limits: SessionChannelLimits,
) -> Result<PendingAcceptedSession, SessionChannelError> {
    refuse_if_transport_disabled()?;
    checkpoint(cx, "accepted session")?;
    validate_limits(limits)?;
    let peer_address = stream
        .peer_addr()
        .map_err(|error| SessionChannelError::Io {
            phase: "peer address",
            message: error.to_string(),
        })?;
    let open_bytes = match required_packet(
        &mut stream,
        cx,
        limits.io_timeout,
        "session_open read",
        MAX_HANDSHAKE_MESSAGE_BYTES,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            close_stream(&stream);
            return Err(error);
        }
    };
    pending_accepted_session_from_open_bytes(stream, peer_address, limits, open_bytes)
}

fn pending_accepted_session_from_open_bytes(
    stream: TcpStream,
    peer_address: SocketAddr,
    limits: SessionChannelLimits,
    open_bytes: Vec<u8>,
) -> Result<PendingAcceptedSession, SessionChannelError> {
    let untrusted_open = match decode_session_open(&open_bytes) {
        Ok(open) => open,
        Err(error) => {
            close_stream(&stream);
            return Err(SessionChannelError::Handshake(error));
        }
    };
    Ok(PendingAcceptedSession {
        stream,
        untrusted_open,
        peer_address,
        limits,
    })
}

/// Authenticate a pending socket after the in-scope resolver has selected
/// route-specific expectations and key material.
async fn authenticate_pending_session<G>(
    cx: &Cx,
    pending: PendingAcceptedSession,
    route: ResolvedAcceptedRoute<G>,
) -> Result<AuthenticatedTransportSession, SessionChannelError> {
    refuse_if_transport_disabled()?;
    checkpoint(cx, "pending session authentication")?;
    let ResolvedAcceptedRoute {
        config,
        source,
        admission_guard,
    } = route;
    validate_limits(config.limits)?;
    if pending.limits != config.limits {
        close_stream(&pending.stream);
        return Err(SessionChannelError::InvalidLimits {
            message: "pending-open and authenticated-session limits differ".to_owned(),
        });
    }
    if pending.peer_address.ip() != source.queried_ip
        || config.expectations.tailnet_id != source.tailnet_id
        || config.expectations.initiator_stable_id != source.stable_id
        || config.observations.initiator_node_pubkey != source.current_node_pubkey
    {
        close_stream(&pending.stream);
        return Err(SessionChannelError::Authentication {
            message: "accepted source does not match the WhoIs-bound route identity".to_owned(),
        });
    }
    let responder_nonce = fresh_bytes()?;
    let result = run_responder_handshake(
        cx,
        pending.stream,
        pending.untrusted_open,
        config,
        responder_nonce,
    )
    .await;
    drop(admission_guard);
    result
}

/// Resolve and authenticate one accepted socket without exposing a resumable
/// pre-auth state. The resolver receives only bounded route selectors plus the
/// kernel source address, executes under the original `Cx` and I/O deadline,
/// and returns a WhoIs-bound local route with an opaque admission guard.
pub async fn accept_authenticated_session_with<R, F, G>(
    cx: &Cx,
    stream: TcpStream,
    limits: SessionChannelLimits,
    resolve: R,
) -> Result<AuthenticatedTransportSession, SessionChannelError>
where
    R: FnOnce(Cx, SocketAddr, UntrustedRouteSelectors) -> F,
    F: Future<Output = Result<ResolvedAcceptedRoute<G>, SessionChannelError>>,
{
    let pending = read_pending_accepted_session(cx, stream, limits).await?;
    authenticate_pending_with_resolver(cx, pending, limits, resolve).await
}

/// Same as [`accept_authenticated_session_with`] after the first framed
/// packet has already been read (so the owner can triage bootstrap hello).
pub async fn accept_authenticated_session_with_open_bytes<R, F, G>(
    cx: &Cx,
    stream: TcpStream,
    limits: SessionChannelLimits,
    open_bytes: Vec<u8>,
    resolve: R,
) -> Result<AuthenticatedTransportSession, SessionChannelError>
where
    R: FnOnce(Cx, SocketAddr, UntrustedRouteSelectors) -> F,
    F: Future<Output = Result<ResolvedAcceptedRoute<G>, SessionChannelError>>,
{
    refuse_if_transport_disabled()?;
    checkpoint(cx, "accepted session")?;
    validate_limits(limits)?;
    let peer_address = stream
        .peer_addr()
        .map_err(|error| SessionChannelError::Io {
            phase: "peer address",
            message: error.to_string(),
        })?;
    let pending =
        pending_accepted_session_from_open_bytes(stream, peer_address, limits, open_bytes)?;
    authenticate_pending_with_resolver(cx, pending, limits, resolve).await
}

async fn authenticate_pending_with_resolver<R, F, G>(
    cx: &Cx,
    pending: PendingAcceptedSession,
    limits: SessionChannelLimits,
    resolve: R,
) -> Result<AuthenticatedTransportSession, SessionChannelError>
where
    R: FnOnce(Cx, SocketAddr, UntrustedRouteSelectors) -> F,
    F: Future<Output = Result<ResolvedAcceptedRoute<G>, SessionChannelError>>,
{
    let selectors = untrusted_route_selectors(&pending.untrusted_open)?;
    let route = match await_route_resolution(
        cx,
        limits.io_timeout,
        resolve(cx.clone(), pending.peer_address, selectors),
    )
    .await
    {
        Ok(route) => route,
        Err(error) => {
            close_stream(&pending.stream);
            return Err(error);
        }
    };
    refuse_if_transport_disabled()?;
    authenticate_pending_session(cx, pending, route).await
}

async fn run_initiator_handshake(
    cx: &Cx,
    mut stream: TcpStream,
    config: InitiatorSessionConfig,
    initiator_nonce: [u8; 32],
) -> Result<AuthenticatedTransportSession, SessionChannelError> {
    let (state, open) = InitiatorHandshake::open(
        config.binding,
        initiator_nonce,
        config.pair_key_generation,
        config.observations,
    )
    .map_err(SessionChannelError::Handshake)?;
    if let Err(error) = write_json_packet(
        &mut stream,
        cx,
        config.limits.io_timeout,
        "session_open write",
        &open,
        MAX_HANDSHAKE_MESSAGE_BYTES,
    )
    .await
    {
        close_stream(&stream);
        return Err(error);
    }
    let confirm_bytes = match required_packet(
        &mut stream,
        cx,
        config.limits.io_timeout,
        "session_confirm read",
        MAX_HANDSHAKE_MESSAGE_BYTES,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            close_stream(&stream);
            return Err(error);
        }
    };
    let confirm = match decode_session_confirm(&confirm_bytes) {
        Ok(confirm) => confirm,
        Err(error) => {
            close_stream(&stream);
            return Err(SessionChannelError::Handshake(error));
        }
    };
    let (finish, established) = match state.finish(&config.pair_key, &confirm) {
        Ok(result) => result,
        Err(error) => {
            close_stream(&stream);
            return Err(SessionChannelError::Handshake(error));
        }
    };
    if let Err(error) = write_json_packet(
        &mut stream,
        cx,
        config.limits.io_timeout,
        "session_finish write",
        &finish,
        MAX_HANDSHAKE_MESSAGE_BYTES,
    )
    .await
    {
        close_stream(&stream);
        return Err(error);
    }
    let mut session = AuthenticatedTransportSession::new(
        stream,
        established,
        SessionRole::Initiator,
        config.capabilities.clone(),
        config.limits,
    );
    let correlation_id = negotiation_correlation(session.binding());
    let offer = CapabilityNegotiationV1 {
        schema: CAPABILITY_NEGOTIATION_SCHEMA_V1.to_owned(),
        phase: CapabilityNegotiationPhase::Offer,
        capabilities: config.capabilities.tokens.clone(),
    };
    session
        .send_request(
            cx,
            SessionMessage {
                correlation_id: correlation_id.clone(),
                capability: FrameCapability::Hello,
                requested_budget_ms: 0,
                payload: serde_json::to_value(offer).map_err(|error| {
                    SessionChannelError::Authentication {
                        message: format!("encode capability offer: {error}"),
                    }
                })?,
            },
        )
        .await?;
    let response = session.receive_response(cx, &correlation_id).await?;
    let selection: CapabilityNegotiationV1 = match serde_json::from_value(response.payload) {
        Ok(selection) => selection,
        Err(error) => {
            return session.fail(SessionChannelError::Authentication {
                message: format!("decode authenticated capability selection: {error}"),
            });
        }
    };
    if selection.schema != CAPABILITY_NEGOTIATION_SCHEMA_V1
        || selection.phase != CapabilityNegotiationPhase::Selection
    {
        return session.fail(SessionChannelError::Authentication {
            message: "capability response has the wrong schema or phase".to_owned(),
        });
    }
    let selected = match SessionCapabilities::new(selection.capabilities) {
        Ok(selected) => selected,
        Err(error) => return session.fail(error),
    };
    if selected
        .tokens
        .iter()
        .any(|token| config.capabilities.tokens.binary_search(token).is_err())
    {
        return session.fail(SessionChannelError::Authentication {
            message: "responder selected a capability the initiator did not offer".to_owned(),
        });
    }
    session.capabilities = selected;
    Ok(session)
}

async fn run_responder_handshake(
    cx: &Cx,
    mut stream: TcpStream,
    open: SessionOpenV1,
    config: AcceptedSessionConfig,
    responder_nonce: [u8; 32],
) -> Result<AuthenticatedTransportSession, SessionChannelError> {
    let (pending, confirm) = match responder_accept_open(
        &open,
        &config.expectations,
        responder_nonce,
        config.observations,
        &config.pair_key,
    ) {
        Ok(result) => result,
        Err(error) => {
            close_stream(&stream);
            return Err(SessionChannelError::Handshake(error));
        }
    };
    if let Err(error) = write_json_packet(
        &mut stream,
        cx,
        config.limits.io_timeout,
        "session_confirm write",
        &confirm,
        MAX_HANDSHAKE_MESSAGE_BYTES,
    )
    .await
    {
        close_stream(&stream);
        return Err(error);
    }
    let finish_bytes = match required_packet(
        &mut stream,
        cx,
        config.limits.io_timeout,
        "session_finish read",
        MAX_HANDSHAKE_MESSAGE_BYTES,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            close_stream(&stream);
            return Err(error);
        }
    };
    let finish = match decode_session_finish(&finish_bytes) {
        Ok(finish) => finish,
        Err(error) => {
            close_stream(&stream);
            return Err(SessionChannelError::Handshake(error));
        }
    };
    let established = match pending.complete(&config.pair_key, &finish) {
        Ok(established) => established,
        Err(error) => {
            close_stream(&stream);
            return Err(SessionChannelError::Handshake(error));
        }
    };
    let local_capabilities = config.capabilities;
    let mut session = AuthenticatedTransportSession::new(
        stream,
        established,
        SessionRole::Responder,
        local_capabilities.clone(),
        config.limits,
    );
    let correlation_id = negotiation_correlation(session.binding());
    let Some(offer_frame) = session.receive_verified(cx).await? else {
        return session.fail(SessionChannelError::UnexpectedHalfClose);
    };
    if offer_frame.frame.kind != FrameKind::Request
        || offer_frame.frame.correlation_id != correlation_id
        || offer_frame.frame.capability != FrameCapability::Hello
    {
        return session.fail(SessionChannelError::Authentication {
            message: "first authenticated frame is not the capability offer".to_owned(),
        });
    }
    let offer: CapabilityNegotiationV1 = match serde_json::from_value(offer_frame.frame.payload) {
        Ok(offer) => offer,
        Err(error) => {
            return session.fail(SessionChannelError::Authentication {
                message: format!("decode authenticated capability offer: {error}"),
            });
        }
    };
    if offer.schema != CAPABILITY_NEGOTIATION_SCHEMA_V1
        || offer.phase != CapabilityNegotiationPhase::Offer
    {
        return session.fail(SessionChannelError::Authentication {
            message: "capability offer has the wrong schema or phase".to_owned(),
        });
    }
    let offered = match SessionCapabilities::new(offer.capabilities) {
        Ok(offered) => offered,
        Err(error) => return session.fail(error),
    };
    let selected = match local_capabilities.intersection(&offered) {
        Ok(selected) => selected,
        Err(error) => return session.fail(error),
    };
    session.pending_inbound.insert(
        correlation_id.clone(),
        PendingRequest {
            capability: FrameCapability::Hello,
        },
    );
    session
        .send_response(
            cx,
            SessionMessage {
                correlation_id,
                capability: FrameCapability::Hello,
                requested_budget_ms: 0,
                payload: serde_json::to_value(CapabilityNegotiationV1 {
                    schema: CAPABILITY_NEGOTIATION_SCHEMA_V1.to_owned(),
                    phase: CapabilityNegotiationPhase::Selection,
                    capabilities: selected.tokens.clone(),
                })
                .map_err(|error| SessionChannelError::Authentication {
                    message: format!("encode capability selection: {error}"),
                })?,
            },
        )
        .await?;
    session.capabilities = selected;
    Ok(session)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CapabilityNegotiationPhase {
    Offer,
    Selection,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CapabilityNegotiationV1 {
    schema: String,
    phase: CapabilityNegotiationPhase,
    capabilities: Vec<String>,
}

fn negotiation_correlation(binding: &SessionBinding) -> String {
    let digest = blake3::hash(binding.session_id.as_bytes()).to_hex();
    format!("session-capabilities-{}", &digest.as_str()[..24])
}

fn session_message_from_frame(frame: &FrameV2) -> SessionMessage {
    SessionMessage {
        correlation_id: frame.correlation_id.clone(),
        capability: frame.capability.clone(),
        requested_budget_ms: frame.requested_budget_ms,
        payload: frame.payload.clone(),
    }
}

fn fresh_bytes() -> Result<[u8; 32], SessionChannelError> {
    for _ in 0..2 {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| SessionChannelError::Randomness {
            message: error.to_string(),
        })?;
        if bytes.iter().any(|byte| *byte != 0) {
            return Ok(bytes);
        }
    }
    Err(SessionChannelError::Randomness {
        message: "OS CSPRNG returned all-zero session material twice".to_owned(),
    })
}

fn fresh_session_id() -> Result<String, SessionChannelError> {
    let bytes = fresh_bytes()?;
    Ok(format!("session-{}", hex_lower(&bytes[..16])))
}

fn refuse_if_transport_disabled() -> Result<(), SessionChannelError> {
    let Some(raw) = read_env_var(EnvVar::MeshTransportDisabled) else {
        return Ok(());
    };
    match crate::config::parse_env_bool_flag(&raw) {
        Some(true) => Err(SessionChannelError::TransportDisabled),
        Some(false) => Ok(()),
        None => Err(SessionChannelError::InvalidConfiguration {
            variable: EnvVar::MeshTransportDisabled.name(),
        }),
    }
}

fn validate_limits(limits: SessionChannelLimits) -> Result<(), SessionChannelError> {
    if limits.connect_timeout.is_zero()
        || limits.io_timeout.is_zero()
        || limits.max_requested_budget_ms == 0
        || limits.max_authenticated_frames < 2
        || limits.max_authenticated_bytes == 0
    {
        return Err(SessionChannelError::InvalidLimits {
            message: "deadlines, request/byte budgets must be non-zero and the frame budget must allow the two negotiation frames".to_owned(),
        });
    }
    Ok(())
}

fn checkpoint(cx: &Cx, phase: &'static str) -> Result<(), SessionChannelError> {
    cx.checkpoint()
        .map_err(|error| SessionChannelError::Cancelled {
            phase,
            message: error.to_string(),
        })
}

async fn await_io<T, F>(
    cx: &Cx,
    duration: Duration,
    phase: &'static str,
    future: F,
) -> Result<T, SessionChannelError>
where
    F: Future<Output = io::Result<T>>,
{
    checkpoint(cx, phase)?;
    let now = wall_now();
    let effective_duration = cx
        .budget()
        .remaining_duration(now)
        .map_or(duration, |remaining| remaining.min(duration));
    if effective_duration.is_zero() {
        checkpoint(cx, phase)?;
        return Err(SessionChannelError::Timeout { phase });
    }
    let _ambient = Cx::set_current(Some(cx.clone()));
    match timeout(now, effective_duration, future).await {
        Ok(Ok(value)) => {
            checkpoint(cx, phase)?;
            Ok(value)
        }
        Ok(Err(error)) => Err(map_io_error(cx, phase, error)),
        Err(_) => match checkpoint(cx, phase) {
            Ok(()) => Err(SessionChannelError::Timeout { phase }),
            Err(cancelled) => Err(cancelled),
        },
    }
}

async fn await_route_resolution<T, F>(
    cx: &Cx,
    duration: Duration,
    future: F,
) -> Result<T, SessionChannelError>
where
    F: Future<Output = Result<T, SessionChannelError>>,
{
    const PHASE: &str = "accepted route resolution";
    checkpoint(cx, PHASE)?;
    let now = wall_now();
    let effective_duration = cx
        .budget()
        .remaining_duration(now)
        .map_or(duration, |remaining| remaining.min(duration));
    if effective_duration.is_zero() {
        checkpoint(cx, PHASE)?;
        return Err(SessionChannelError::Timeout { phase: PHASE });
    }
    let _ambient = Cx::set_current(Some(cx.clone()));
    match timeout(now, effective_duration, future).await {
        Ok(result) => {
            checkpoint(cx, PHASE)?;
            result
        }
        Err(_) => match checkpoint(cx, PHASE) {
            Ok(()) => Err(SessionChannelError::Timeout { phase: PHASE }),
            Err(cancelled) => Err(cancelled),
        },
    }
}

fn map_io_error(cx: &Cx, phase: &'static str, error: io::Error) -> SessionChannelError {
    if error.kind() == io::ErrorKind::Interrupted
        && let Err(cancelled) = cx.checkpoint()
    {
        return SessionChannelError::Cancelled {
            phase,
            message: cancelled.to_string(),
        };
    }
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
    ) {
        return SessionChannelError::Frame(TransportSessionError::MalformedFrame {
            message: error.to_string(),
        });
    }
    SessionChannelError::Io {
        phase,
        message: error.to_string(),
    }
}

async fn write_json_packet<T: Serialize>(
    stream: &mut TcpStream,
    cx: &Cx,
    duration: Duration,
    phase: &'static str,
    value: &T,
    max_bytes: usize,
) -> Result<(), SessionChannelError> {
    let bytes = serde_json::to_vec(value).map_err(|error| SessionChannelError::Authentication {
        message: format!("serialize wire message: {error}"),
    })?;
    write_packet(stream, cx, duration, phase, &bytes, max_bytes).await
}

async fn write_packet(
    stream: &mut TcpStream,
    cx: &Cx,
    duration: Duration,
    phase: &'static str,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<(), SessionChannelError> {
    if bytes.len() > max_bytes || bytes.len() > u32::MAX as usize {
        return Err(SessionChannelError::Frame(
            TransportSessionError::FrameTooLarge {
                actual_bytes: bytes.len(),
            },
        ));
    }
    let prefix = u32::try_from(bytes.len())
        .map_err(|_| SessionChannelError::Authentication {
            message: "wire message length does not fit u32".to_owned(),
        })?
        .to_be_bytes();
    await_io(cx, duration, phase, async {
        stream.write_all(&prefix).await?;
        stream.write_all(bytes).await?;
        stream.flush().await
    })
    .await
}

async fn required_packet(
    stream: &mut TcpStream,
    cx: &Cx,
    duration: Duration,
    phase: &'static str,
    max_bytes: usize,
) -> Result<Vec<u8>, SessionChannelError> {
    read_packet(stream, cx, duration, phase, max_bytes)
        .await?
        .ok_or(SessionChannelError::UnexpectedHalfClose)
}

async fn read_packet(
    stream: &mut TcpStream,
    cx: &Cx,
    duration: Duration,
    phase: &'static str,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, SessionChannelError> {
    await_io(cx, duration, phase, async {
        let mut prefix = [0_u8; 4];
        let mut prefix_read = 0;
        while prefix_read < prefix.len() {
            let count = stream.read(&mut prefix[prefix_read..]).await?;
            if count == 0 {
                if prefix_read == 0 {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial length prefix",
                ));
            }
            prefix_read += count;
        }
        let length = u32::from_be_bytes(prefix) as usize;
        if length > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("length prefix {length} exceeds {max_bytes}-byte cap"),
            ));
        }
        let mut bytes = vec![0_u8; length];
        let mut body_read = 0;
        while body_read < length {
            let count = stream.read(&mut bytes[body_read..]).await?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial length-prefixed body",
                ));
            }
            body_read += count;
        }
        Ok(Some(bytes))
    })
    .await
}

fn close_stream(stream: &TcpStream) {
    let _ = stream.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kat_binding() -> SessionBinding {
        SessionBinding {
            team_id: "team-kat".to_owned(),
            tailnet_id: "tailnet-kat.ts.net".to_owned(),
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
    fn forged_mac_does_not_consume_the_exact_next_counter() {
        let keys = kat_keys();
        let mut frame = kat_frame(&keys);
        let mut mac: Vec<u8> = frame.mac.bytes().collect();
        mac[0] = if mac[0] == b'a' { b'b' } else { b'a' };
        frame.mac = String::from_utf8(mac).expect("hex stays utf8");
        let mut counters = SessionCounters::new();
        let error = verify_frame(
            &frame,
            &kat_binding(),
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &keys,
            &NegotiatedExtensions::none(),
        )
        .expect_err("forged MAC must fail");
        assert_eq!(error, TransportSessionError::MacMismatch);
        assert_eq!(counters.expected_next(), 1);
        assert!(!counters.is_closed());
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
    fn wrong_tailnet_valid_mac_fails_under_tailnet_bound_session_key() {
        let original_binding = kat_binding();
        let original_keys = kat_keys();
        let frame = kat_frame(&original_keys);
        let mut wrong_binding = original_binding.clone();
        wrong_binding.tailnet_id = "other-tailnet.ts.net".to_owned();
        let wrong_keys =
            derive_session_keys(&kat_pair_key(), &wrong_binding, &[0x11; 32], &[0x22; 32]);
        let mut counters = SessionCounters::new();
        let error = verify_frame(
            &frame,
            &wrong_binding,
            SessionDirection::InitiatorToResponder,
            &mut counters,
            &wrong_keys,
            &NegotiatedExtensions::none(),
        )
        .expect_err("MAC valid in one tailnet must fail in another tailnet");
        assert_eq!(error, TransportSessionError::MacMismatch);
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
    fn counter_max_is_terminal_without_saturating_reuse() {
        let mut counters = SessionCounters {
            next: u64::MAX,
            closed: false,
            exhausted: false,
        };
        counters.accept(u64::MAX).expect("MAX is accepted once");
        let error = counters
            .accept(u64::MAX)
            .expect_err("MAX has no exact successor");
        assert!(matches!(
            error,
            TransportSessionError::ReplayRejected {
                violation: CounterViolation::Exhausted,
                expected: u64::MAX,
                observed: u64::MAX,
            }
        ));
        assert!(counters.is_closed());
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
            tailnet_id: "tailnet-kat.ts.net".to_owned(),
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
    fn handshake_rejects_empty_observations_zero_or_reused_nonces_and_wrong_tailnet() {
        let empty = HandshakeObservations {
            initiator_node_pubkey: String::new(),
            responder_node_pubkey: String::new(),
        };
        let error = InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, empty)
            .expect_err("empty current node-key observations must fail");
        assert!(matches!(error, HandshakeError::InvalidObservation { .. }));

        let error = InitiatorHandshake::open(kat_binding(), [0; 32], 1, kat_observations())
            .expect_err("zero initiator nonce must fail");
        assert!(matches!(error, HandshakeError::BadNonce { .. }));

        let (state, open) =
            InitiatorHandshake::open(kat_binding(), [0x11; 32], 1, kat_observations())
                .expect("valid open");
        let _ = state;
        let error = responder_accept_open(
            &open,
            &kat_expectations(),
            [0x11; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect_err("responder nonce must differ from initiator nonce");
        assert!(matches!(error, HandshakeError::BadNonce { .. }));

        let mut wrong_tailnet = kat_expectations();
        wrong_tailnet.tailnet_id = "other-tailnet.ts.net".to_owned();
        let error = responder_accept_open(
            &open,
            &wrong_tailnet,
            [0x22; 32],
            kat_observations(),
            &kat_pair_key(),
        )
        .expect_err("locally verified tailnet mismatch must fail");
        assert_eq!(
            error,
            HandshakeError::BindingMismatch {
                field: "tailnet_id"
            }
        );
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
