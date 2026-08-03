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
