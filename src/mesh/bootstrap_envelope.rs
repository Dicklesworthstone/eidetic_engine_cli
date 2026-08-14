//! T2.1 TC-D2 bootstrap envelope: the **only** unsigned pre-key wire surface
//! (`bd-tc-epic-qzk7o.3.2`, ADR 0086 TC-D2).
//!
//! Frame authentication is pairwise-keyed, so first contact — the hello probe
//! of a stranger and the join ceremony — cannot use an established pair key.
//! Pre-enrollment traffic rides this distinct envelope instead:
//!
//! - **Unsigned but strictly bounded**: one encoded envelope may never exceed
//!   [`BOOTSTRAP_MAX_ENVELOPE_BYTES`] (4096 bytes), checked before parsing.
//! - **Capabilities closed to `hello` / `join`**: anything else is refused at
//!   the triage probe, before full deserialization.
//! - **No identity fields, by construction**: the envelope carries a schema,
//!   a capability, and a payload — nothing else, and unknown fields are
//!   rejected. Caller identity comes exclusively from the accepted socket
//!   address resolved through Tailscale LocalAPI WhoIs (or a freshly queried
//!   status map) on the accept path; a caller-supplied tailnet, node-key, or
//!   owner header can never even be expressed in this envelope.
//! - **No durable mutation before proof**: this module performs no storage,
//!   audit, or index writes, and handlers dispatched from it must not either
//!   until the invite secret is proved. Unknown-node and malformed bootstrap
//!   traffic may only touch the bounded in-memory counters below.
//! - **Source-IP + global admission caps** ([`BootstrapAdmission`]): fixed
//!   windows keyed on the accepted source IP plus one listener-global bucket.
//!   Pre-authentication limits key on the address, not any claimed identity,
//!   so rotating an unsigned claimed node key cannot evade a bucket. Full
//!   `admission.rs` wiring follows in operations; these minimal caps ship
//!   with the listener itself.
//!
//! Declines are privacy-preserving: [`BootstrapDeclineV1`] carries a stable
//! code and nothing about the responder (mirroring the `hello.rs` decline
//! invariant — a "no" must not leak who we are).

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

/// Schema id for the bootstrap envelope.
pub const BOOTSTRAP_ENVELOPE_SCHEMA_V1: &str = "ee.mesh.bootstrap_envelope.v1";

/// Schema id for the bootstrap decline message.
pub const BOOTSTRAP_DECLINE_SCHEMA_V1: &str = "ee.mesh.bootstrap_decline.v1";

/// Hard outer limit on one encoded bootstrap envelope (TC-D2).
pub const BOOTSTRAP_MAX_ENVELOPE_BYTES: usize = 4096;

/// Default per-source-IP admissions inside one fixed window.
pub const BOOTSTRAP_SOURCE_IP_MAX_PER_WINDOW: u32 = 8;

/// Default listener-global admissions inside one fixed window.
pub const BOOTSTRAP_GLOBAL_MAX_PER_WINDOW: u32 = 64;

/// Default fixed-window length in milliseconds.
pub const BOOTSTRAP_WINDOW_MS: u64 = 60_000;

/// Upper bound on distinct source IPs tracked at once. When the table is
/// full of live windows, untracked sources are declined (fail closed): at
/// that point the listener is under an address-diversity flood and bounded
/// memory wins over admitting strangers.
pub const BOOTSTRAP_MAX_TRACKED_SOURCES: usize = 1024;

/// The two capabilities pre-key traffic may invoke (TC-D2). This enum is
/// deliberately closed; there is no extension arm.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapCapability {
    /// Secret-free reachability / discovery hello.
    Hello,
    /// The join ceremony (invite ID + nonce → signed challenge → secret).
    Join,
}

impl BootstrapCapability {
    /// Stable wire token for this capability.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Join => "join",
        }
    }
}

/// The unsigned pre-key envelope. Deliberately minimal: no source identity,
/// no target identity, no version negotiation beyond the schema id — the
/// payload contracts (`ee.mesh.hello.v1`, the T4.2 join-ceremony messages)
/// own their inner shapes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapEnvelopeV1 {
    /// Always [`BOOTSTRAP_ENVELOPE_SCHEMA_V1`].
    pub schema: String,
    /// Which pre-key capability the payload invokes.
    pub capability: BootstrapCapability,
    /// The inner bootstrap message (bounded by the outer envelope budget).
    pub payload: JsonValue,
}

/// Privacy-preserving decline for refused bootstrap traffic. Carries a
/// stable code and **no responder-side metadata**.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapDeclineV1 {
    /// Always [`BOOTSTRAP_DECLINE_SCHEMA_V1`].
    pub schema: String,
    /// Stable decline code ([`BootstrapEnvelopeError::decline_code`]).
    pub code: String,
}

/// Fail-closed error surface for the bootstrap envelope layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BootstrapEnvelopeError {
    /// The encoded envelope exceeds [`BOOTSTRAP_MAX_ENVELOPE_BYTES`].
    OverBudget {
        /// Observed encoded size.
        actual_bytes: usize,
    },
    /// The envelope could not be decoded.
    Malformed {
        /// Human-readable decode failure.
        message: String,
    },
    /// The envelope carried an unknown schema.
    SchemaMismatch {
        /// The schema the envelope carried.
        observed: String,
    },
    /// The envelope invoked a capability outside `hello` / `join`.
    UnsupportedCapability {
        /// The capability token the envelope carried.
        observed: String,
    },
    /// The source-IP or listener-global admission bucket is exhausted.
    RateLimited,
}

impl BootstrapEnvelopeError {
    /// Stable wire decline code for this refusal.
    #[must_use]
    pub const fn decline_code(&self) -> &'static str {
        match self {
            Self::OverBudget { .. } => "bootstrap_over_budget",
            Self::Malformed { .. } => "bootstrap_malformed",
            Self::SchemaMismatch { .. } => "bootstrap_schema_mismatch",
            Self::UnsupportedCapability { .. } => "bootstrap_unsupported_capability",
            Self::RateLimited => "bootstrap_rate_limited",
        }
    }

    /// The privacy-preserving decline message for this refusal.
    #[must_use]
    pub fn decline_message(&self) -> BootstrapDeclineV1 {
        BootstrapDeclineV1 {
            schema: BOOTSTRAP_DECLINE_SCHEMA_V1.to_owned(),
            code: self.decline_code().to_owned(),
        }
    }
}

impl std::fmt::Display for BootstrapEnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OverBudget { actual_bytes } => write!(
                f,
                "Mesh bootstrap envelope is {actual_bytes} bytes, exceeding the {BOOTSTRAP_MAX_ENVELOPE_BYTES}-byte cap"
            ),
            Self::Malformed { message } => {
                write!(f, "Mesh bootstrap envelope is malformed: {message}")
            }
            Self::SchemaMismatch { observed } => {
                write!(
                    f,
                    "Mesh bootstrap envelope rejected unknown schema {observed:?}"
                )
            }
            Self::UnsupportedCapability { observed } => write!(
                f,
                "Mesh bootstrap envelope rejected capability {observed:?}; pre-key traffic may only invoke hello or join"
            ),
            Self::RateLimited => {
                f.write_str("Mesh bootstrap admission bucket is exhausted for this window")
            }
        }
    }
}

impl std::error::Error for BootstrapEnvelopeError {}

/// Author one bootstrap envelope, refusing to serialize anything over the
/// TC-D2 budget.
pub fn encode_envelope(
    capability: BootstrapCapability,
    payload: JsonValue,
) -> Result<Vec<u8>, BootstrapEnvelopeError> {
    let envelope = BootstrapEnvelopeV1 {
        schema: BOOTSTRAP_ENVELOPE_SCHEMA_V1.to_owned(),
        capability,
        payload,
    };
    let bytes =
        serde_json::to_vec(&envelope).map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("serialize envelope: {error}"),
        })?;
    if bytes.len() > BOOTSTRAP_MAX_ENVELOPE_BYTES {
        return Err(BootstrapEnvelopeError::OverBudget {
            actual_bytes: bytes.len(),
        });
    }
    Ok(bytes)
}

/// Decode one encoded bootstrap envelope: size gate first, then schema and
/// capability triage, then the full parse.
pub fn decode_envelope(bytes: &[u8]) -> Result<BootstrapEnvelopeV1, BootstrapEnvelopeError> {
    if bytes.len() > BOOTSTRAP_MAX_ENVELOPE_BYTES {
        return Err(BootstrapEnvelopeError::OverBudget {
            actual_bytes: bytes.len(),
        });
    }
    #[derive(Deserialize)]
    struct TriageProbe {
        schema: String,
        capability: JsonValue,
    }
    let probe: TriageProbe =
        serde_json::from_slice(bytes).map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("decode envelope: {error}"),
        })?;
    if probe.schema != BOOTSTRAP_ENVELOPE_SCHEMA_V1 {
        return Err(BootstrapEnvelopeError::SchemaMismatch {
            observed: probe.schema,
        });
    }
    let capability_token = probe.capability.as_str().unwrap_or_default();
    if capability_token != BootstrapCapability::Hello.token()
        && capability_token != BootstrapCapability::Join.token()
    {
        return Err(BootstrapEnvelopeError::UnsupportedCapability {
            observed: capability_token.to_owned(),
        });
    }
    serde_json::from_slice(bytes).map_err(|error| BootstrapEnvelopeError::Malformed {
        message: format!("decode envelope body: {error}"),
    })
}

/// Resolve one live TCP target from a Tailscale IP list and the committed
/// hello port. Never scans alternate ports or falls back to a different port.
#[must_use]
pub fn bootstrap_hello_target(
    tailscale_ips: &[String],
    committed_port: u16,
) -> Option<std::net::SocketAddr> {
    if committed_port < 1024 {
        return None;
    }
    tailscale_ips.iter().find_map(|raw| {
        let trimmed = raw.trim();
        if let Ok(addr) = trimmed.parse::<std::net::SocketAddr>() {
            return (addr.port() == committed_port
                && !addr.ip().is_unspecified()
                && !addr.ip().is_loopback())
            .then_some(addr);
        }
        trimmed
            .parse::<std::net::IpAddr>()
            .ok()
            .filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
            .map(|ip| std::net::SocketAddr::new(ip, committed_port))
    })
}

/// Parse a stored peer endpoint into one TCP address. HTTP(S) sneakernet
/// placeholders are not live transport locators.
#[must_use]
pub fn parse_live_peer_endpoint(
    endpoint: &str,
    committed_port: u16,
) -> Option<std::net::SocketAddr> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() || trimmed.contains("://") || committed_port < 1024 {
        return None;
    }
    if let Ok(addr) = trimmed.parse::<std::net::SocketAddr>() {
        return (!addr.ip().is_unspecified()).then_some(addr);
    }
    trimmed
        .parse::<std::net::IpAddr>()
        .ok()
        .filter(|ip| !ip.is_unspecified())
        .map(|ip| std::net::SocketAddr::new(ip, committed_port))
}

/// Schema for one metadata-only anti-entropy round on the hello TCP socket.
pub const SYNC_ROUND_SCHEMA_V1: &str = "ee.mesh.sync_round.v1";
/// Authenticated body-fetch request over a live session.
pub const BODY_FETCH_REQUEST_SCHEMA_V1: &str = "ee.mesh.body_fetch.request.v1";
/// Authenticated body-fetch response. Body bytes are omitted unless available.
pub const BODY_FETCH_RESPONSE_SCHEMA_V1: &str = "ee.mesh.body_fetch.response.v1";

/// One origin tip advertised in a sync round.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncRoundTip {
    pub origin_node_id: String,
    pub origin_workspace_id: String,
    pub last_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_event_hash: Option<String>,
}

/// One origin event header carried in a sync round. Payload is the signed
/// header JSON already stored locally; this is not a body-lane fetch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncRoundEvent {
    pub origin_node_id: String,
    pub origin_workspace_id: String,
    pub seq: u64,
    pub event_hash: String,
    pub payload_json: String,
}

/// Caller → responder for one published body cache key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BodyFetchRequest {
    pub schema: String,
    pub body_cache_key: String,
}

/// Responder → caller. `body_hex` is present only when the cache is available.
/// `nonce_hex` is released only with authorized body bytes so the receiver
/// can recompute the event-signed commitment. It is never stored on the
/// receiver after verification.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BodyFetchResponse {
    pub schema: String,
    pub body_cache_key: String,
    pub cache_status: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce_hex: Option<String>,
}

/// Caller → responder after hello.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncRoundRequest {
    pub schema: String,
    pub tips: Vec<SyncRoundTip>,
    pub range_start_seq: u64,
    pub max_events: u32,
}

/// Responder → caller.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncRoundResponse {
    pub schema: String,
    pub tips: Vec<SyncRoundTip>,
    pub events: Vec<SyncRoundEvent>,
}

impl SyncRoundRequest {
    #[must_use]
    pub fn new(tips: Vec<SyncRoundTip>, range_start_seq: u64, max_events: u32) -> Self {
        Self {
            schema: SYNC_ROUND_SCHEMA_V1.to_owned(),
            tips,
            range_start_seq,
            max_events: max_events.max(1).min(512),
        }
    }
}

/// Parse a sync-round request; unknown schema is refused.
#[must_use]
pub fn parse_sync_round_request(bytes: &[u8]) -> Option<SyncRoundRequest> {
    let request = serde_json::from_slice::<SyncRoundRequest>(bytes).ok()?;
    (request.schema == SYNC_ROUND_SCHEMA_V1).then_some(request)
}

/// Parse a sync-round response.
#[must_use]
pub fn parse_sync_round_response(bytes: &[u8]) -> Option<SyncRoundResponse> {
    let response = serde_json::from_slice::<SyncRoundResponse>(bytes).ok()?;
    (response.schema == SYNC_ROUND_SCHEMA_V1).then_some(response)
}

/// Hello plus one bounded event-header round on the same TCP socket.
pub fn exchange_live_mesh_round(
    address: std::net::SocketAddr,
    timeout: std::time::Duration,
    hello_payload: JsonValue,
    sync_request: &SyncRoundRequest,
) -> Result<(JsonValue, SyncRoundResponse), BootstrapEnvelopeError> {
    if address.ip().is_unspecified() {
        return Err(BootstrapEnvelopeError::Malformed {
            message: "live mesh round refuses an unspecified remote address".to_owned(),
        });
    }
    let request = encode_envelope(BootstrapCapability::Hello, hello_payload)?;
    let mut stream = std::net::TcpStream::connect_timeout(&address, timeout).map_err(|error| {
        BootstrapEnvelopeError::Malformed {
            message: format!("live mesh connect: {error}"),
        }
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("live mesh timeout: {error}"),
        })?;
    write_std_framed(&mut stream, &request)?;
    let reply = read_std_framed(&mut stream)?;
    if let Ok(decline) = serde_json::from_slice::<BootstrapDeclineV1>(&reply)
        && decline.schema == BOOTSTRAP_DECLINE_SCHEMA_V1
    {
        return Err(BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap hello declined: {}", decline.code),
        });
    }
    let envelope = decode_envelope(&reply)?;
    if envelope.capability != BootstrapCapability::Hello {
        return Err(BootstrapEnvelopeError::UnsupportedCapability {
            observed: envelope.capability.token().to_owned(),
        });
    }
    let sync_bytes =
        serde_json::to_vec(sync_request).map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("serialize sync round: {error}"),
        })?;
    write_std_framed(&mut stream, &sync_bytes)?;
    let sync_reply = read_std_framed(&mut stream)?;
    let sync = parse_sync_round_response(&sync_reply).ok_or_else(|| {
        BootstrapEnvelopeError::Malformed {
            message: "peer did not return ee.mesh.sync_round.v1".to_owned(),
        }
    })?;
    Ok((envelope.payload, sync))
}

/// Exchange one unsigned bootstrap join over length-prefixed TCP.
pub fn exchange_bootstrap_join(
    address: std::net::SocketAddr,
    timeout: std::time::Duration,
    payload: JsonValue,
) -> Result<JsonValue, BootstrapEnvelopeError> {
    if address.ip().is_unspecified() {
        return Err(BootstrapEnvelopeError::Malformed {
            message: "bootstrap join refuses an unspecified remote address".to_owned(),
        });
    }
    let request = encode_envelope(BootstrapCapability::Join, payload)?;
    let mut stream = std::net::TcpStream::connect_timeout(&address, timeout).map_err(|error| {
        BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap join connect: {error}"),
        }
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap join timeout: {error}"),
        })?;
    write_std_framed(&mut stream, &request)?;
    let reply = read_std_framed(&mut stream)?;
    if let Ok(decline) = serde_json::from_slice::<BootstrapDeclineV1>(&reply)
        && decline.schema == BOOTSTRAP_DECLINE_SCHEMA_V1
    {
        return Err(BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap join declined: {}", decline.code),
        });
    }
    let envelope = decode_envelope(&reply)?;
    if envelope.capability != BootstrapCapability::Join {
        return Err(BootstrapEnvelopeError::UnsupportedCapability {
            observed: envelope.capability.token().to_owned(),
        });
    }
    Ok(envelope.payload)
}

/// Exchange one unsigned bootstrap hello over length-prefixed TCP.
pub fn exchange_bootstrap_hello(
    address: std::net::SocketAddr,
    timeout: std::time::Duration,
    payload: JsonValue,
) -> Result<JsonValue, BootstrapEnvelopeError> {
    if address.ip().is_unspecified() {
        return Err(BootstrapEnvelopeError::Malformed {
            message: "bootstrap hello refuses an unspecified remote address".to_owned(),
        });
    }
    let request = encode_envelope(BootstrapCapability::Hello, payload)?;
    let mut stream = std::net::TcpStream::connect_timeout(&address, timeout).map_err(|error| {
        BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap hello connect: {error}"),
        }
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap hello timeout: {error}"),
        })?;
    write_std_framed(&mut stream, &request)?;
    let reply = read_std_framed(&mut stream)?;
    if let Ok(decline) = serde_json::from_slice::<BootstrapDeclineV1>(&reply)
        && decline.schema == BOOTSTRAP_DECLINE_SCHEMA_V1
    {
        return Err(BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap hello declined: {}", decline.code),
        });
    }
    let envelope = decode_envelope(&reply)?;
    if envelope.capability != BootstrapCapability::Hello {
        return Err(BootstrapEnvelopeError::UnsupportedCapability {
            observed: envelope.capability.token().to_owned(),
        });
    }
    Ok(envelope.payload)
}

pub(crate) fn write_std_framed(
    stream: &mut std::net::TcpStream,
    bytes: &[u8],
) -> Result<(), BootstrapEnvelopeError> {
    if bytes.len() > BOOTSTRAP_MAX_ENVELOPE_BYTES {
        return Err(BootstrapEnvelopeError::OverBudget {
            actual_bytes: bytes.len(),
        });
    }
    let prefix = u32::try_from(bytes.len())
        .map_err(|_| BootstrapEnvelopeError::Malformed {
            message: "bootstrap frame length does not fit u32".to_owned(),
        })?
        .to_be_bytes();
    use std::io::Write;
    stream
        .write_all(&prefix)
        .and_then(|()| stream.write_all(bytes))
        .and_then(|()| stream.flush())
        .map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap hello write: {error}"),
        })
}

pub(crate) fn read_std_framed(
    stream: &mut std::net::TcpStream,
) -> Result<Vec<u8>, BootstrapEnvelopeError> {
    use std::io::Read;
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap hello read prefix: {error}"),
        })?;
    let length = usize::try_from(u32::from_be_bytes(prefix)).map_err(|_| {
        BootstrapEnvelopeError::Malformed {
            message: "bootstrap hello length does not fit usize".to_owned(),
        }
    })?;
    if length == 0 || length > BOOTSTRAP_MAX_ENVELOPE_BYTES {
        return Err(BootstrapEnvelopeError::OverBudget {
            actual_bytes: length,
        });
    }
    let mut bytes = vec![0_u8; length];
    stream
        .read_exact(&mut bytes)
        .map_err(|error| BootstrapEnvelopeError::Malformed {
            message: format!("bootstrap hello read body: {error}"),
        })?;
    Ok(bytes)
}

#[derive(Clone, Copy, Debug)]
struct WindowBucket {
    window_start_ms: u64,
    count: u32,
}

impl WindowBucket {
    const fn fresh(now_ms: u64) -> Self {
        Self {
            window_start_ms: now_ms,
            count: 0,
        }
    }

    /// Roll the fixed window forward when it has elapsed. Uses saturating
    /// arithmetic so a caller-supplied clock regression degrades to "same
    /// window" instead of panicking or reopening budget.
    fn roll(&mut self, window_ms: u64, now_ms: u64) {
        if now_ms.saturating_sub(self.window_start_ms) >= window_ms {
            *self = Self::fresh(now_ms);
        }
    }

    const fn expired(&self, window_ms: u64, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.window_start_ms) >= window_ms
    }
}

/// Bounded in-memory pre-authentication admission state: one listener-global
/// fixed-window bucket plus per-source-IP buckets capped at
/// [`BOOTSTRAP_MAX_TRACKED_SOURCES`]. Deterministic by construction — the
/// caller supplies a nondecreasing monotonic `now_ms`, and eviction only
/// removes expired windows.
#[derive(Debug)]
pub struct BootstrapAdmission {
    window_ms: u64,
    source_ip_max_per_window: u32,
    global_max_per_window: u32,
    max_tracked_sources: usize,
    global: WindowBucket,
    per_source: BTreeMap<String, WindowBucket>,
}

impl BootstrapAdmission {
    /// Admission state with the TC-D2 default caps.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(
            BOOTSTRAP_WINDOW_MS,
            BOOTSTRAP_SOURCE_IP_MAX_PER_WINDOW,
            BOOTSTRAP_GLOBAL_MAX_PER_WINDOW,
            BOOTSTRAP_MAX_TRACKED_SOURCES,
        )
    }

    /// Admission state with explicit caps (tests and future config wiring).
    #[must_use]
    pub fn with_limits(
        window_ms: u64,
        source_ip_max_per_window: u32,
        global_max_per_window: u32,
        max_tracked_sources: usize,
    ) -> Self {
        Self {
            window_ms,
            source_ip_max_per_window,
            global_max_per_window,
            max_tracked_sources,
            global: WindowBucket::fresh(0),
            per_source: BTreeMap::new(),
        }
    }

    /// Admit or decline one bootstrap attempt from `source_ip` at monotonic
    /// time `now_ms`. Buckets are only charged on admission, so a flood of
    /// declined attempts from one address cannot starve other callers'
    /// global budget.
    pub fn admit(&mut self, source_ip: &str, now_ms: u64) -> Result<(), BootstrapEnvelopeError> {
        self.global.roll(self.window_ms, now_ms);
        if self.global.count >= self.global_max_per_window {
            return Err(BootstrapEnvelopeError::RateLimited);
        }
        if !self.per_source.contains_key(source_ip) {
            if self.per_source.len() >= self.max_tracked_sources {
                let window_ms = self.window_ms;
                self.per_source
                    .retain(|_, bucket| !bucket.expired(window_ms, now_ms));
            }
            if self.per_source.len() >= self.max_tracked_sources {
                // Full of live windows: address-diversity flood. Fail closed.
                return Err(BootstrapEnvelopeError::RateLimited);
            }
        }
        let bucket = self
            .per_source
            .entry(source_ip.to_owned())
            .or_insert_with(|| WindowBucket::fresh(now_ms));
        bucket.roll(self.window_ms, now_ms);
        if bucket.count >= self.source_ip_max_per_window {
            return Err(BootstrapEnvelopeError::RateLimited);
        }
        bucket.count += 1;
        self.global.count += 1;
        Ok(())
    }

    /// Number of distinct source IPs currently tracked.
    #[must_use]
    pub fn tracked_sources(&self) -> usize {
        self.per_source.len()
    }
}

impl Default for BootstrapAdmission {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn live_peer_endpoint_rejects_http_placeholders_and_accepts_tcp() {
        assert_eq!(
            parse_live_peer_endpoint("https://peer.tailnet.test/ee/mesh", 41888),
            None
        );
        assert_eq!(parse_live_peer_endpoint("", 41888), None);
        assert_eq!(parse_live_peer_endpoint("100.64.1.8", 80), None);
        assert_eq!(
            parse_live_peer_endpoint("100.64.1.8", 41888),
            Some("100.64.1.8:41888".parse().expect("addr"))
        );
        assert_eq!(
            parse_live_peer_endpoint("127.0.0.1:41901", 41888),
            Some("127.0.0.1:41901".parse().expect("addr"))
        );
    }

    #[test]
    fn bootstrap_hello_target_ignores_loopback_and_wrong_port() {
        assert_eq!(
            bootstrap_hello_target(&["127.0.0.1".to_owned()], 41888),
            None
        );
        assert_eq!(
            bootstrap_hello_target(&["100.64.1.8:9999".to_owned()], 41888),
            None
        );
        assert_eq!(
            bootstrap_hello_target(&["100.64.1.8".to_owned()], 41888),
            Some("100.64.1.8:41888".parse().expect("addr"))
        );
    }

    #[test]
    fn exchange_bootstrap_hello_round_trips_on_loopback() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let request = read_std_framed(&mut stream).expect("read request");
            let envelope = decode_envelope(&request).expect("decode request");
            assert_eq!(envelope.capability, BootstrapCapability::Hello);
            let reply = encode_envelope(BootstrapCapability::Hello, json!({"pong": true}))
                .expect("encode reply");
            write_std_framed(&mut stream, &reply).expect("write reply");
        });
        let payload = exchange_bootstrap_hello(
            address,
            std::time::Duration::from_secs(2),
            json!({"ping": true}),
        )
        .expect("client exchange");
        assert_eq!(payload, json!({"pong": true}));
        server.join().expect("server thread");
    }

    #[test]
    fn live_mesh_round_returns_event_batch_after_hello() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = read_std_framed(&mut stream).expect("hello");
            let hello = encode_envelope(BootstrapCapability::Hello, json!({"ok": true}))
                .expect("hello reply");
            write_std_framed(&mut stream, &hello).expect("write hello");
            let sync_bytes = read_std_framed(&mut stream).expect("sync");
            let request = parse_sync_round_request(&sync_bytes).expect("parse sync");
            assert_eq!(request.schema, SYNC_ROUND_SCHEMA_V1);
            let reply = serde_json::to_vec(&SyncRoundResponse {
                schema: SYNC_ROUND_SCHEMA_V1.to_owned(),
                tips: vec![SyncRoundTip {
                    origin_node_id: "node_a".to_owned(),
                    origin_workspace_id: "wsp_a".to_owned(),
                    last_seq: 2,
                    tip_event_hash: Some("blake3:tip".to_owned()),
                }],
                events: vec![SyncRoundEvent {
                    origin_node_id: "node_a".to_owned(),
                    origin_workspace_id: "wsp_a".to_owned(),
                    seq: 2,
                    event_hash: "blake3:e2".to_owned(),
                    payload_json: "{}".to_owned(),
                }],
            })
            .expect("encode");
            write_std_framed(&mut stream, &reply).expect("write sync");
        });
        let (_hello, sync) = exchange_live_mesh_round(
            address,
            std::time::Duration::from_secs(2),
            json!({"ping": true}),
            &SyncRoundRequest::new(Vec::new(), 0, 8),
        )
        .expect("round");
        assert_eq!(sync.events.len(), 1);
        assert_eq!(sync.events[0].seq, 2);
        server.join().expect("server");
    }

    #[test]
    fn hello_and_join_envelopes_round_trip() {
        for capability in [BootstrapCapability::Hello, BootstrapCapability::Join] {
            let bytes = encode_envelope(capability, json!({"inner": "message"})).expect("encode");
            assert!(bytes.len() <= BOOTSTRAP_MAX_ENVELOPE_BYTES);
            let decoded = decode_envelope(&bytes).expect("decode");
            assert_eq!(decoded.schema, BOOTSTRAP_ENVELOPE_SCHEMA_V1);
            assert_eq!(decoded.capability, capability);
            assert_eq!(decoded.payload, json!({"inner": "message"}));
        }
    }

    #[test]
    fn budget_is_enforced_on_both_sides() {
        let oversize_payload = json!({"blob": "x".repeat(BOOTSTRAP_MAX_ENVELOPE_BYTES)});
        assert!(matches!(
            encode_envelope(BootstrapCapability::Hello, oversize_payload)
                .expect_err("encode over budget"),
            BootstrapEnvelopeError::OverBudget { .. }
        ));

        let oversize_wire = vec![b'{'; BOOTSTRAP_MAX_ENVELOPE_BYTES + 1];
        let error = decode_envelope(&oversize_wire).expect_err("decode over budget");
        assert!(matches!(error, BootstrapEnvelopeError::OverBudget { .. }));
        assert_eq!(error.decline_code(), "bootstrap_over_budget");
    }

    #[test]
    fn capabilities_are_closed_to_hello_and_join() {
        let bytes = serde_json::to_vec(&json!({
            "schema": BOOTSTRAP_ENVELOPE_SCHEMA_V1,
            "capability": "summary",
            "payload": {}
        }))
        .expect("encode");
        let error = decode_envelope(&bytes).expect_err("summary is post-enrollment only");
        assert_eq!(
            error,
            BootstrapEnvelopeError::UnsupportedCapability {
                observed: "summary".to_owned()
            }
        );
        assert_eq!(error.decline_code(), "bootstrap_unsupported_capability");

        let bytes = serde_json::to_vec(&json!({
            "schema": BOOTSTRAP_ENVELOPE_SCHEMA_V1,
            "capability": {"nested": "shape"},
            "payload": {}
        }))
        .expect("encode");
        assert!(matches!(
            decode_envelope(&bytes).expect_err("non-string capability"),
            BootstrapEnvelopeError::UnsupportedCapability { .. }
        ));
    }

    #[test]
    fn envelope_cannot_express_identity_claims() {
        // TC-D2: identity comes from the accepted socket via WhoIs, never
        // from the envelope. Unknown fields — including any smuggled
        // identity claim — are rejected at decode.
        let bytes = serde_json::to_vec(&json!({
            "schema": BOOTSTRAP_ENVELOPE_SCHEMA_V1,
            "capability": "hello",
            "payload": {},
            "sourceNodeKey": "nodekey:mallory"
        }))
        .expect("encode");
        assert!(matches!(
            decode_envelope(&bytes).expect_err("identity claim must be rejected"),
            BootstrapEnvelopeError::Malformed { .. }
        ));
    }

    #[test]
    fn unknown_schema_is_rejected() {
        let bytes = serde_json::to_vec(&json!({
            "schema": "ee.mesh.bootstrap_envelope.v2",
            "capability": "hello",
            "payload": {}
        }))
        .expect("encode");
        assert!(matches!(
            decode_envelope(&bytes).expect_err("unknown schema"),
            BootstrapEnvelopeError::SchemaMismatch { .. }
        ));
    }

    #[test]
    fn decline_message_carries_only_a_stable_code() {
        let decline = BootstrapEnvelopeError::RateLimited.decline_message();
        assert_eq!(decline.schema, BOOTSTRAP_DECLINE_SCHEMA_V1);
        assert_eq!(decline.code, "bootstrap_rate_limited");
        let value = serde_json::to_value(&decline).expect("encode");
        let object = value.as_object().expect("object");
        // Privacy invariant: exactly schema + code, nothing about us.
        assert_eq!(object.len(), 2);
    }

    #[test]
    fn per_source_window_caps_and_rolls() {
        let mut admission = BootstrapAdmission::with_limits(1_000, 2, 100, 8);
        admission.admit("100.64.0.1", 0).expect("first");
        admission.admit("100.64.0.1", 10).expect("second");
        assert_eq!(
            admission
                .admit("100.64.0.1", 20)
                .expect_err("third in window"),
            BootstrapEnvelopeError::RateLimited
        );
        // Another source is unaffected.
        admission.admit("100.64.0.2", 20).expect("other source");
        // The window rolls and the source is admitted again.
        admission.admit("100.64.0.1", 1_000).expect("fresh window");
    }

    #[test]
    fn global_window_caps_across_sources() {
        let mut admission = BootstrapAdmission::with_limits(1_000, 100, 3, 8);
        admission.admit("100.64.0.1", 0).expect("one");
        admission.admit("100.64.0.2", 1).expect("two");
        admission.admit("100.64.0.3", 2).expect("three");
        assert_eq!(
            admission.admit("100.64.0.4", 3).expect_err("global cap"),
            BootstrapEnvelopeError::RateLimited
        );
        admission.admit("100.64.0.4", 1_000).expect("fresh window");
    }

    #[test]
    fn tracked_sources_stay_bounded_and_fail_closed_under_flood() {
        let mut admission = BootstrapAdmission::with_limits(1_000, 8, 1_000, 2);
        admission.admit("100.64.0.1", 0).expect("one");
        admission.admit("100.64.0.2", 1).expect("two");
        assert_eq!(admission.tracked_sources(), 2);
        // Table full of live windows: an untracked stranger is declined.
        assert_eq!(
            admission
                .admit("100.64.0.3", 2)
                .expect_err("flood fail-closed"),
            BootstrapEnvelopeError::RateLimited
        );
        // Tracked sources keep working while the table is full.
        admission
            .admit("100.64.0.1", 3)
            .expect("tracked source still admitted");
        // Once windows expire, the stranger gets in and eviction keeps the
        // table bounded.
        admission
            .admit("100.64.0.3", 1_500)
            .expect("expired windows evicted");
        assert!(admission.tracked_sources() <= 2);
    }

    #[test]
    fn declined_attempts_do_not_charge_budget() {
        let mut admission = BootstrapAdmission::with_limits(1_000, 1, 2, 8);
        admission.admit("100.64.0.1", 0).expect("admitted");
        for _ in 0..10 {
            assert_eq!(
                admission
                    .admit("100.64.0.1", 1)
                    .expect_err("per-source cap"),
                BootstrapEnvelopeError::RateLimited
            );
        }
        // The hammering address consumed one global slot, not eleven.
        admission
            .admit("100.64.0.2", 2)
            .expect("global budget intact");
    }
}
