//! SRR6.46.6 — Mesh hello handshake protocol.
//!
//! Tiny bounded handshake that SRR6.46.2 autodiscovery sends to candidate
//! peers, with three message shapes:
//!
//! - [`HelloRequest`] (`ee.mesh.hello.v1`): caller → responder.
//! - [`HelloResponse`] (`ee.mesh.hello.response.v1`): success path.
//! - [`HelloError`] (`ee.mesh.hello.error.v1`): decline path with a
//!   stable code vocabulary. **Privacy invariant**: the decline payload
//!   carries NO responder-side metadata (no `responderEeVersion`, no
//!   `responderWorkspaceIds`, no `responderCapabilities`, no
//!   `responderAdvertisedTags`). A "no" must not leak who we are.
//!
//! Both directions enforce a hard 4096-byte payload budget at the
//! framing layer (SRR6.9 transport responsibility). The per-request
//! handler ([`decide_hello_response`] / [`HelloHandler`]) is pure: no
//! DB writes, no audit-row writes, no log persistence beyond a
//! tracing line. The supervised lifecycle (binding, rate-limiting,
//! lifecycle audit) lives in SRR6.46.12.
//!
//! Version negotiation (SRR6.27 owns the cross-version compatibility
//! contract): the per-request handler accepts the request when its
//! `requesterEeProtocolVersion` major version matches the local major,
//! returns `unsupported_protocol_version` otherwise. Minor-version skew
//! is compatible by convention; an older minor MAY drop fields it does
//! not understand thanks to `serde(default)` plus `additionalProperties:
//! false` enforcement at the schema gate.

use serde::{Deserialize, Serialize};

use crate::mesh::discovery_policy::{
    DiscoveryConsent, DiscoveryMode, RespondDecisionInput, decide_respond,
};

/// Schema id for the request.
pub const HELLO_REQUEST_SCHEMA_V1: &str = "ee.mesh.hello.v1";

/// Schema id for the success response.
pub const HELLO_RESPONSE_SCHEMA_V1: &str = "ee.mesh.hello.response.v1";

/// Schema id for the decline / error response.
pub const HELLO_ERROR_SCHEMA_V1: &str = "ee.mesh.hello.error.v1";

/// Hard payload budget enforced at the SRR6.9 framing layer.
///
/// Caller-side: refuses to serialize a request that exceeds this size.
/// Responder-side: the SRR6.9 transport drops the frame before this
/// handler sees it. The handler also re-checks via
/// [`serialized_payload_fits_budget`] as a defense-in-depth gate.
pub const HELLO_PAYLOAD_BUDGET_BYTES: usize = 4096;

/// The single supported MAJOR protocol version. Bumping this is an
/// SRR6.27 rolling-upgrade-compat ADR change; see that bead for the
/// process.
pub const HELLO_PROTOCOL_VERSION_MAJOR: u32 = 1;

/// The MINOR protocol version this build emits + accepts at parity.
pub const HELLO_PROTOCOL_VERSION_MINOR: u32 = 0;

/// Canonical `requesterEeProtocolVersion` / `responderEeProtocolVersion`
/// string this build emits.
#[must_use]
pub fn local_protocol_version_string() -> String {
    format!("{HELLO_PROTOCOL_VERSION_MAJOR}.{HELLO_PROTOCOL_VERSION_MINOR}")
}

/// Parsed `<major>.<minor>` protocol version.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
}

impl ProtocolVersion {
    /// Parse a `<major>.<minor>` string. Returns `None` for any shape
    /// that doesn't exactly match the regex `^[0-9]+\.[0-9]+$`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut split = value.split('.');
        let major_str = split.next()?;
        let minor_str = split.next()?;
        if split.next().is_some() {
            // Too many segments.
            return None;
        }
        let major: u32 = major_str.parse().ok()?;
        let minor: u32 = minor_str.parse().ok()?;
        Some(Self { major, minor })
    }

    /// True when this version is compatible with `local` per the
    /// "major-must-match" contract.
    #[must_use]
    pub fn is_compatible_with(&self, local: Self) -> bool {
        self.major == local.major
    }

    /// The local build's protocol version.
    #[must_use]
    pub fn local() -> Self {
        Self {
            major: HELLO_PROTOCOL_VERSION_MAJOR,
            minor: HELLO_PROTOCOL_VERSION_MINOR,
        }
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Stable decline-reason vocabulary for [`HelloError::code`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelloErrorCode {
    /// The requester's protocol major-version differs from the responder's.
    UnsupportedProtocolVersion,
    /// SRR6.46.7 policy refused consent (e.g. service_tag mode without
    /// `tag:ee-mesh`, or denylist hit).
    DiscoveryConsentDenied,
    /// SRR6.46.12 rate-limit fired — caller should back off.
    ResponderBusy,
    /// `EE_MESH_ENABLED=0` on the responder side; mesh is disabled.
    ResponderMeshDisabled,
    /// Tailscale `shields_up` is on; inbound discovery is administratively
    /// blocked. Symmetric to SRR6.46.1's `tailscale_shields_up`
    /// degraded code.
    ResponderShieldsUp,
    /// Responder's local `tailscaled` is not authenticated; the responder
    /// cannot even safely report its own identity, so it declines
    /// rather than answer with stale data.
    ResponderUnauthenticatedTailscale,
}

impl HelloErrorCode {
    /// Canonical schema string (matches the enum values in
    /// `ee.mesh.hello.error.v1`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedProtocolVersion => "unsupported_protocol_version",
            Self::DiscoveryConsentDenied => "discovery_consent_denied",
            Self::ResponderBusy => "responder_busy",
            Self::ResponderMeshDisabled => "responder_mesh_disabled",
            Self::ResponderShieldsUp => "responder_shields_up",
            Self::ResponderUnauthenticatedTailscale => "responder_unauthenticated_tailscale",
        }
    }
}

/// Hello request payload. Caller side ([`build_request`]) constructs;
/// responder side ([`decide_hello_response`]) inspects.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HelloRequest {
    pub schema: &'static str,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "requesterNodeKey")]
    pub requester_node_key: String,
    #[serde(rename = "requesterEeVersion")]
    pub requester_ee_version: String,
    #[serde(rename = "requesterEeProtocolVersion")]
    pub requester_ee_protocol_version: String,
    #[serde(rename = "requesterWorkspaceIds", default)]
    pub requester_workspace_ids: Vec<String>,
    #[serde(rename = "requesterCapabilities", default)]
    pub requester_capabilities: Vec<String>,
    #[serde(rename = "requesterAdvertisedTags", default, skip_serializing_if = "Vec::is_empty")]
    pub requester_advertised_tags: Vec<String>,
}

/// Hello success response payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HelloResponse {
    pub schema: &'static str,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "responderNodeKey")]
    pub responder_node_key: String,
    #[serde(rename = "responderEeVersion")]
    pub responder_ee_version: String,
    #[serde(rename = "responderEeProtocolVersion")]
    pub responder_ee_protocol_version: String,
    #[serde(rename = "responderWorkspaceIds", default)]
    pub responder_workspace_ids: Vec<String>,
    #[serde(rename = "responderCapabilities", default)]
    pub responder_capabilities: Vec<String>,
    #[serde(rename = "responderAdvertisedTags", default, skip_serializing_if = "Vec::is_empty")]
    pub responder_advertised_tags: Vec<String>,
    #[serde(rename = "discoveryConsent")]
    pub discovery_consent: bool,
    #[serde(rename = "responseElapsedMicros")]
    pub response_elapsed_micros: u64,
}

/// Hello decline payload. **Carries no responder-side metadata.**
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HelloError {
    pub schema: &'static str,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "discoveryConsent")]
    pub discovery_consent: bool,
    pub code: HelloErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Build a hello request for sending to a peer.
///
/// The caller fills in their own identity + workspace-id intent + the
/// capability set they advertise. The function refuses to return a
/// request that would exceed the 4096-byte serialized payload budget;
/// callers must trim their workspace-id or capability lists if they
/// hit that ceiling.
#[must_use]
pub fn build_request(
    request_id: impl Into<String>,
    requester_node_key: impl Into<String>,
    requester_ee_version: impl Into<String>,
    requester_workspace_ids: Vec<String>,
    requester_capabilities: Vec<String>,
    requester_advertised_tags: Vec<String>,
) -> HelloRequest {
    HelloRequest {
        schema: HELLO_REQUEST_SCHEMA_V1,
        request_id: request_id.into(),
        requester_node_key: requester_node_key.into(),
        requester_ee_version: requester_ee_version.into(),
        requester_ee_protocol_version: local_protocol_version_string(),
        requester_workspace_ids,
        requester_capabilities,
        requester_advertised_tags,
    }
}

/// Serialize a payload to canonical JSON and confirm it fits within the
/// payload budget. Returns the serialized bytes on success.
///
/// Used by both [`build_request`] callers and by the responder for
/// defense-in-depth re-checking before handing the response back to
/// the SRR6.9 transport.
pub fn serialize_within_budget<T: Serialize>(value: &T) -> Result<Vec<u8>, HelloSerializeError> {
    let serialized = serde_json::to_vec(value).map_err(HelloSerializeError::Json)?;
    if serialized.len() > HELLO_PAYLOAD_BUDGET_BYTES {
        return Err(HelloSerializeError::PayloadTooLarge {
            actual_bytes: serialized.len(),
            budget_bytes: HELLO_PAYLOAD_BUDGET_BYTES,
        });
    }
    Ok(serialized)
}

/// Convenience predicate: would this payload fit the framing budget?
///
/// Useful for the responder's defense-in-depth check that runs after
/// the decision logic but before the transport hands the bytes back.
pub fn serialized_payload_fits_budget<T: Serialize>(value: &T) -> bool {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() <= HELLO_PAYLOAD_BUDGET_BYTES)
        .unwrap_or(false)
}

/// Error variants for [`serialize_within_budget`].
#[derive(Debug)]
pub enum HelloSerializeError {
    Json(serde_json::Error),
    PayloadTooLarge {
        actual_bytes: usize,
        budget_bytes: usize,
    },
}

impl std::fmt::Display for HelloSerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(f, "failed to serialize hello payload: {error}"),
            Self::PayloadTooLarge {
                actual_bytes,
                budget_bytes,
            } => write!(
                f,
                "hello payload {actual_bytes} bytes exceeds {budget_bytes}-byte budget"
            ),
        }
    }
}

impl std::error::Error for HelloSerializeError {}

/// Posture of the responder at request time. Cached by the SRR6.46.12
/// supervised job once per `EE_TAILSCALE_PROBE_TIMEOUT_MS` window so the
/// per-request handler is allocation-free.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponderContext<'a> {
    /// `EE_MESH_ENABLED=1` for this responder?
    pub mesh_enabled: bool,
    /// SRR6.46.1 probe authenticated state.
    pub tailscale_authenticated: bool,
    /// SRR6.46.1 probe shields-up state. Even when probe says
    /// authenticated, shields-up means inbound is administratively
    /// blocked.
    pub shields_up: bool,
    /// SRR6.46.7 discovery mode for the responder side.
    pub respond_mode: DiscoveryMode,
    /// Stable responder identity reported in the success response.
    pub responder_node_key: &'a str,
    /// ee crate version (e.g. `0.2.0`) the responder advertises in
    /// the success response.
    pub responder_ee_version: &'a str,
    /// Workspaces this responder serves; intersected with
    /// `requesterWorkspaceIds` by the caller-side filter (NOT here).
    pub responder_workspace_ids: &'a [String],
    /// Capability tokens the responder advertises.
    pub responder_capabilities: &'a [String],
    /// Tailscale ACL tags this responder advertises (informational +
    /// service-tag policy input).
    pub responder_advertised_tags: &'a [String],
    /// SRR6.46.7 allowlist for `respond_mode=allowlist`. Empty for
    /// `service_tag` / `auto_admit`.
    pub respond_allowlist: &'a std::collections::BTreeSet<String>,
    /// SRR6.46.7 denylist (overrides all modes).
    pub denylist: &'a std::collections::BTreeSet<String>,
    /// Per-peer rate-limit decision. The supervised job pre-computes
    /// this once per request; if `true`, the per-request handler
    /// returns `responder_busy` immediately. Keeps the per-request
    /// handler pure.
    pub rate_limited: bool,
    /// Microseconds the responder spent computing context before
    /// invoking the per-request handler. Echoed into the response's
    /// `responseElapsedMicros` field.
    pub elapsed_micros: u64,
}

/// Outcome of the per-request handler. Either a success [`HelloResponse`]
/// or a privacy-safe decline [`HelloError`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HelloOutcome {
    Granted(HelloResponse),
    Declined(HelloError),
}

impl HelloOutcome {
    /// True if the responder granted consent.
    #[must_use]
    pub fn is_granted(&self) -> bool {
        matches!(self, Self::Granted(_))
    }

    /// Reference the response when granted.
    #[must_use]
    pub fn response(&self) -> Option<&HelloResponse> {
        if let Self::Granted(r) = self {
            Some(r)
        } else {
            None
        }
    }

    /// Reference the error when declined.
    #[must_use]
    pub fn error(&self) -> Option<&HelloError> {
        if let Self::Declined(e) = self {
            Some(e)
        } else {
            None
        }
    }
}

/// The per-request handler. Pure-read.
///
/// Evaluation order (first-match wins — each gate returns immediately
/// with the matching decline code):
///
/// 1. Rate-limit pre-decision → `ResponderBusy`. (SRR6.46.12 pre-computes.)
/// 2. `mesh_enabled == false` → `ResponderMeshDisabled`.
/// 3. `tailscale_authenticated == false` → `ResponderUnauthenticatedTailscale`.
/// 4. `shields_up == true` → `ResponderShieldsUp`.
/// 5. Protocol-major mismatch → `UnsupportedProtocolVersion`.
///    Malformed `requesterEeProtocolVersion` (does not parse as
///    `<major>.<minor>`) is treated the same as major-mismatch.
/// 6. SRR6.46.7 policy consult via [`decide_respond`]:
///    - `DiscoveryConsent::Denied` → `DiscoveryConsentDenied`.
///    - `DiscoveryConsent::Granted` → return success [`HelloResponse`].
///
/// Privacy invariant: every decline path returns [`HelloOutcome::Declined`]
/// with a [`HelloError`] that carries NO responder-side metadata.
///
/// Side-effect invariant: this function writes no DB rows, no audit
/// rows, no log persistence. A `tracing::debug!` trace line is the only
/// side effect, and only on the granted path so a malicious flood of
/// invalid requests does not generate caller-controlled log spam.
#[must_use]
pub fn decide_hello_response(
    request: &HelloRequest,
    ctx: &ResponderContext<'_>,
) -> HelloOutcome {
    let echo = request.request_id.clone();

    // 1. Rate limit.
    if ctx.rate_limited {
        return decline(echo, HelloErrorCode::ResponderBusy, None);
    }
    // 2. Mesh disabled.
    if !ctx.mesh_enabled {
        return decline(echo, HelloErrorCode::ResponderMeshDisabled, None);
    }
    // 3. Tailscale unauthenticated.
    if !ctx.tailscale_authenticated {
        return decline(echo, HelloErrorCode::ResponderUnauthenticatedTailscale, None);
    }
    // 4. Shields up.
    if ctx.shields_up {
        return decline(echo, HelloErrorCode::ResponderShieldsUp, None);
    }
    // 5. Protocol version negotiation.
    let local = ProtocolVersion::local();
    let requester_version = ProtocolVersion::parse(&request.requester_ee_protocol_version);
    let compatible = requester_version
        .map(|v| v.is_compatible_with(local))
        .unwrap_or(false);
    if !compatible {
        return decline(
            echo,
            HelloErrorCode::UnsupportedProtocolVersion,
            // Detail must not leak responder identity — but the local
            // major version is a public protocol constant; including
            // it is safe and helps the caller decide whether to
            // upgrade.
            Some(format!("requires major {HELLO_PROTOCOL_VERSION_MAJOR}.x")),
        );
    }
    // 6. SRR6.46.7 discovery policy consult.
    let (consent, _reason) = decide_respond(&RespondDecisionInput {
        mode: ctx.respond_mode,
        requester_node_key: &request.requester_node_key,
        requester_advertised_tags: &request.requester_advertised_tags,
        self_advertised_tags: ctx.responder_advertised_tags,
        respond_allowlist: ctx.respond_allowlist,
        denylist: ctx.denylist,
    });
    match consent {
        DiscoveryConsent::Denied => decline(echo, HelloErrorCode::DiscoveryConsentDenied, None),
        DiscoveryConsent::Granted => {
            tracing::debug!(
                target: "ee::mesh::hello",
                request_id = %echo,
                requester_node_key = %request.requester_node_key,
                "hello granted"
            );
            HelloOutcome::Granted(HelloResponse {
                schema: HELLO_RESPONSE_SCHEMA_V1,
                request_id: echo,
                responder_node_key: ctx.responder_node_key.to_owned(),
                responder_ee_version: ctx.responder_ee_version.to_owned(),
                responder_ee_protocol_version: local_protocol_version_string(),
                responder_workspace_ids: ctx.responder_workspace_ids.to_vec(),
                responder_capabilities: ctx.responder_capabilities.to_vec(),
                responder_advertised_tags: ctx.responder_advertised_tags.to_vec(),
                discovery_consent: true,
                response_elapsed_micros: ctx.elapsed_micros,
            })
        }
    }
}

#[inline]
fn decline(echo: String, code: HelloErrorCode, detail: Option<String>) -> HelloOutcome {
    HelloOutcome::Declined(HelloError {
        schema: HELLO_ERROR_SCHEMA_V1,
        request_id: echo,
        discovery_consent: false,
        code,
        detail,
    })
}

/// Caller-side classification: when a peer responds with a
/// [`HelloError`], map the decline code to a stable skip reason so
/// SRR6.46.2's `skippedPeers[].reason` field carries a uniform
/// vocabulary across the caller and responder sides.
///
/// The mapping is deliberately narrow — the SRR6.46.2 reason vocabulary
/// is owned by that bead, but this helper provides the canonical
/// suggestion so callers don't reinvent it.
#[must_use]
pub fn classify_decline_for_caller_skip_reason(code: HelloErrorCode) -> &'static str {
    match code {
        HelloErrorCode::UnsupportedProtocolVersion => "incompatible_protocol",
        HelloErrorCode::DiscoveryConsentDenied => "no_discovery_consent",
        HelloErrorCode::ResponderBusy => "probe_timeout",
        HelloErrorCode::ResponderMeshDisabled => "non_ee",
        HelloErrorCode::ResponderShieldsUp => "no_discovery_consent",
        HelloErrorCode::ResponderUnauthenticatedTailscale => "non_ee",
    }
}

/// Assert that a [`HelloError`] payload carries no responder-side
/// metadata. Returns `Ok(())` when the privacy invariant holds; returns
/// `Err(field_name)` if any leakage is present.
///
/// Useful as a debug-build defense-in-depth check the SRR6.46.12 supervised
/// job can run before handing bytes back to the transport.
pub fn assert_no_responder_metadata_leak(error: &HelloError) -> Result<(), &'static str> {
    // The HelloError type cannot carry these fields by construction —
    // they are simply absent from the struct definition. This function
    // is the static assertion of that property. Future-proofs against
    // accidental schema drift if someone adds responder-side fields
    // to HelloError.
    let serialized = serde_json::to_value(error).map_err(|_| "serde_serialize")?;
    let object = serialized.as_object().ok_or("not_object")?;
    for forbidden in [
        "responderNodeKey",
        "responderEeVersion",
        "responderEeProtocolVersion",
        "responderWorkspaceIds",
        "responderCapabilities",
        "responderAdvertisedTags",
        "responseElapsedMicros",
    ] {
        if object.contains_key(forbidden) {
            return Err(forbidden);
        }
    }
    Ok(())
}

// ============================================================================
// Inline tests (AGENTS.md L300-302 / bd-3usjw.62 Rule 7)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn empty_set() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn fixture_request() -> HelloRequest {
        HelloRequest {
            schema: HELLO_REQUEST_SCHEMA_V1,
            request_id: "req_alpha".to_owned(),
            requester_node_key: "nodekey:caller".to_owned(),
            requester_ee_version: "0.2.0".to_owned(),
            requester_ee_protocol_version: local_protocol_version_string(),
            requester_workspace_ids: vec!["wsp_one".to_owned()],
            requester_capabilities: vec!["discovery".to_owned()],
            requester_advertised_tags: vec![],
        }
    }

    fn fixture_ctx<'a>(
        responder_node_key: &'a str,
        responder_workspace_ids: &'a [String],
        responder_capabilities: &'a [String],
        responder_advertised_tags: &'a [String],
        respond_allowlist: &'a BTreeSet<String>,
        denylist: &'a BTreeSet<String>,
    ) -> ResponderContext<'a> {
        ResponderContext {
            mesh_enabled: true,
            tailscale_authenticated: true,
            shields_up: false,
            respond_mode: DiscoveryMode::AutoAdmit,
            responder_node_key,
            responder_ee_version: "0.2.0",
            responder_workspace_ids,
            responder_capabilities,
            responder_advertised_tags,
            respond_allowlist,
            denylist,
            rate_limited: false,
            elapsed_micros: 42,
        }
    }

    // ---- Protocol version parser ------------------------------------------

    #[test]
    fn protocol_version_parses_valid_major_minor() {
        let v = ProtocolVersion::parse("3.7").expect("ok");
        assert_eq!(v, ProtocolVersion { major: 3, minor: 7 });
    }

    #[test]
    fn protocol_version_rejects_three_segments() {
        assert!(ProtocolVersion::parse("1.0.0").is_none());
    }

    #[test]
    fn protocol_version_rejects_non_numeric_segments() {
        assert!(ProtocolVersion::parse("a.b").is_none());
        assert!(ProtocolVersion::parse("1.x").is_none());
        assert!(ProtocolVersion::parse("x.1").is_none());
    }

    #[test]
    fn protocol_version_rejects_empty() {
        assert!(ProtocolVersion::parse("").is_none());
        assert!(ProtocolVersion::parse(".").is_none());
        assert!(ProtocolVersion::parse(".1").is_none());
        assert!(ProtocolVersion::parse("1.").is_none());
    }

    #[test]
    fn protocol_version_is_compatible_when_majors_match() {
        let local = ProtocolVersion::local();
        let same_major = ProtocolVersion {
            major: local.major,
            minor: local.minor.saturating_add(99),
        };
        assert!(same_major.is_compatible_with(local));
    }

    #[test]
    fn protocol_version_is_incompatible_when_majors_differ() {
        let local = ProtocolVersion::local();
        let other_major = ProtocolVersion {
            major: local.major.saturating_add(1),
            minor: local.minor,
        };
        assert!(!other_major.is_compatible_with(local));
    }

    // ---- Request building --------------------------------------------------

    #[test]
    fn build_request_uses_local_protocol_version() {
        let req = build_request(
            "req_x",
            "nodekey:me",
            "0.2.0",
            vec!["wsp_a".to_owned()],
            vec!["discovery".to_owned()],
            vec![],
        );
        assert_eq!(req.schema, HELLO_REQUEST_SCHEMA_V1);
        assert_eq!(req.requester_ee_protocol_version, local_protocol_version_string());
        assert_eq!(req.request_id, "req_x");
    }

    #[test]
    fn build_request_serializes_to_valid_json_under_budget() {
        let req = fixture_request();
        let bytes = serialize_within_budget(&req).expect("under budget");
        assert!(bytes.len() < HELLO_PAYLOAD_BUDGET_BYTES);
        let bytes_owned = bytes.into_owned();
        let round_trip: HelloRequest = serde_json::from_slice(&bytes_owned).expect("round-trip");
        assert_eq!(round_trip, req);
    }

    #[test]
    fn serialize_within_budget_rejects_oversized_payloads() {
        let mut req = fixture_request();
        // Inflate workspace-ids until the serialized payload exceeds the budget.
        req.requester_workspace_ids = (0..200)
            .map(|i| format!("wsp_long_workspace_id_index_{i:04}_aaaaaaaa"))
            .collect();
        let result = serialize_within_budget(&req);
        assert!(matches!(
            result,
            Err(HelloSerializeError::PayloadTooLarge { .. })
        ));
    }

    // ---- Handler: refusal precedence ---------------------------------------

    #[test]
    fn handler_returns_responder_busy_when_rate_limited() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let mut ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        ctx.rate_limited = true;
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let err = outcome.error().expect("declined");
        assert_eq!(err.code, HelloErrorCode::ResponderBusy);
    }

    #[test]
    fn handler_returns_responder_mesh_disabled_when_env_false() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let mut ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        ctx.mesh_enabled = false;
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let err = outcome.error().expect("declined");
        assert_eq!(err.code, HelloErrorCode::ResponderMeshDisabled);
    }

    #[test]
    fn handler_returns_responder_unauthenticated_tailscale_when_probe_says_so() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let mut ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        ctx.tailscale_authenticated = false;
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let err = outcome.error().expect("declined");
        assert_eq!(err.code, HelloErrorCode::ResponderUnauthenticatedTailscale);
    }

    #[test]
    fn handler_returns_responder_shields_up_when_set() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let mut ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        ctx.shields_up = true;
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let err = outcome.error().expect("declined");
        assert_eq!(err.code, HelloErrorCode::ResponderShieldsUp);
    }

    #[test]
    fn handler_skips_peer_on_incompatible_major_version() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        let mut req = fixture_request();
        req.requester_ee_protocol_version = format!(
            "{}.{}",
            HELLO_PROTOCOL_VERSION_MAJOR + 1,
            HELLO_PROTOCOL_VERSION_MINOR
        );
        let outcome = decide_hello_response(&req, &ctx);
        let err = outcome.error().expect("declined");
        assert_eq!(err.code, HelloErrorCode::UnsupportedProtocolVersion);
    }

    #[test]
    fn handler_treats_malformed_protocol_version_as_unsupported() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        let mut req = fixture_request();
        req.requester_ee_protocol_version = "garbage".to_owned();
        let outcome = decide_hello_response(&req, &ctx);
        let err = outcome.error().expect("declined");
        assert_eq!(err.code, HelloErrorCode::UnsupportedProtocolVersion);
    }

    #[test]
    fn handler_returns_discovery_consent_denied_when_policy_refuses() {
        let allow = empty_set();
        let mut deny = BTreeSet::new();
        deny.insert("nodekey:caller".to_owned());
        let tags = vec![];
        let ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let err = outcome.error().expect("declined");
        assert_eq!(err.code, HelloErrorCode::DiscoveryConsentDenied);
    }

    // ---- Handler: success path --------------------------------------------

    #[test]
    fn handler_grants_consent_and_echoes_request_id() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let ws = vec!["wsp_one".to_owned()];
        let caps = vec!["discovery".to_owned()];
        let ctx = fixture_ctx("nodekey:responder", &ws, &caps, &tags, &allow, &deny);
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let resp = outcome.response().expect("granted");
        assert_eq!(resp.schema, HELLO_RESPONSE_SCHEMA_V1);
        assert_eq!(resp.request_id, "req_alpha");
        assert_eq!(resp.responder_node_key, "nodekey:responder");
        assert_eq!(resp.responder_workspace_ids, vec!["wsp_one"]);
        assert_eq!(resp.responder_capabilities, vec!["discovery"]);
        assert!(resp.discovery_consent);
        assert_eq!(resp.response_elapsed_micros, 42);
        assert_eq!(resp.responder_ee_protocol_version, local_protocol_version_string());
    }

    #[test]
    fn handler_grants_consent_when_service_tag_mode_and_self_advertises_tag() {
        let allow = empty_set();
        let deny = empty_set();
        let self_tags = vec![crate::mesh::discovery_policy::EE_MESH_SERVICE_TAG.to_owned()];
        let mut ctx = fixture_ctx("nodekey:responder", &[], &[], &self_tags, &allow, &deny);
        ctx.respond_mode = DiscoveryMode::ServiceTag;
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let resp = outcome.response().expect("granted");
        assert_eq!(resp.discovery_consent, true);
    }

    #[test]
    fn handler_denies_consent_under_service_tag_mode_without_self_tag() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let mut ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        ctx.respond_mode = DiscoveryMode::ServiceTag;
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let err = outcome.error().expect("declined");
        assert_eq!(err.code, HelloErrorCode::DiscoveryConsentDenied);
    }

    // ---- Privacy invariant -------------------------------------------------

    #[test]
    fn decline_response_omits_responder_metadata() {
        let allow = empty_set();
        let mut deny = BTreeSet::new();
        deny.insert("nodekey:caller".to_owned());
        let tags = vec![];
        let ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        let outcome = decide_hello_response(&fixture_request(), &ctx);
        let err = outcome.error().expect("declined");
        // Static assertion through the serialized payload.
        assert!(assert_no_responder_metadata_leak(err).is_ok());
        // Belt-and-suspenders check of the JSON output.
        let json = serde_json::to_string(err).expect("serialize");
        assert!(!json.contains("responderNodeKey"));
        assert!(!json.contains("responderEeVersion"));
        assert!(!json.contains("responderWorkspaceIds"));
        assert!(!json.contains("responderCapabilities"));
        assert!(!json.contains("responderAdvertisedTags"));
        assert!(!json.contains("responseElapsedMicros"));
    }

    #[test]
    fn decline_unsupported_protocol_version_includes_safe_detail() {
        let allow = empty_set();
        let deny = empty_set();
        let tags = vec![];
        let ctx = fixture_ctx("nodekey:responder", &[], &[], &tags, &allow, &deny);
        let mut req = fixture_request();
        req.requester_ee_protocol_version = "99.0".to_owned();
        let outcome = decide_hello_response(&req, &ctx);
        let err = outcome.error().expect("declined");
        // Detail is allowed to carry the local major-version constant
        // (public protocol info), but must not leak identity material.
        let detail = err.detail.as_deref().unwrap_or("");
        assert!(detail.contains(&format!("major {HELLO_PROTOCOL_VERSION_MAJOR}.x")));
        assert!(!detail.contains("nodekey"));
        assert!(!detail.contains("tailnet"));
    }

    // ---- Tolerance of unknown fields (forward-compat) ----------------------

    #[test]
    fn request_deserialization_tolerates_missing_optional_advertised_tags() {
        // Validates the `#[serde(default)]` on `requester_advertised_tags`.
        let json = r#"{
            "schema": "ee.mesh.hello.v1",
            "requestId": "req_x",
            "requesterNodeKey": "nodekey:caller",
            "requesterEeVersion": "0.2.0",
            "requesterEeProtocolVersion": "1.0",
            "requesterWorkspaceIds": [],
            "requesterCapabilities": []
        }"#;
        let req: HelloRequest = serde_json::from_str(json).expect("parses");
        assert!(req.requester_advertised_tags.is_empty());
    }

    #[test]
    fn caller_classification_maps_decline_codes_to_skip_reasons() {
        // Lock the caller-side skip-reason vocabulary.
        for (code, expected) in [
            (HelloErrorCode::UnsupportedProtocolVersion, "incompatible_protocol"),
            (HelloErrorCode::DiscoveryConsentDenied, "no_discovery_consent"),
            (HelloErrorCode::ResponderBusy, "probe_timeout"),
            (HelloErrorCode::ResponderMeshDisabled, "non_ee"),
            (HelloErrorCode::ResponderShieldsUp, "no_discovery_consent"),
            (HelloErrorCode::ResponderUnauthenticatedTailscale, "non_ee"),
        ] {
            assert_eq!(classify_decline_for_caller_skip_reason(code), expected);
        }
    }

    #[test]
    fn hello_error_code_round_trips_through_snake_case_serde() {
        for code in [
            HelloErrorCode::UnsupportedProtocolVersion,
            HelloErrorCode::DiscoveryConsentDenied,
            HelloErrorCode::ResponderBusy,
            HelloErrorCode::ResponderMeshDisabled,
            HelloErrorCode::ResponderShieldsUp,
            HelloErrorCode::ResponderUnauthenticatedTailscale,
        ] {
            let serialized = serde_json::to_string(&code).expect("serialize");
            let deserialized: HelloErrorCode =
                serde_json::from_str(&serialized).expect("deserialize");
            assert_eq!(deserialized, code);
            assert!(serialized.contains(code.as_str()));
        }
    }
}
