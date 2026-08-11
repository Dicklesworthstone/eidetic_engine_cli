//! bd-tc-epic-qzk7o.3.1 (T2.0) — typed signed origin stream, core slice.
//!
//! The `.3.2`-independent heart of T2.0: canonical event encoding, blake3
//! chain hashing, the closed-allowlist typed payloads, the salted
//! body-commitment discipline, and transactional append over the V104
//! tables. Origin SIGNING is deliberately a seam ([`OriginSigner`]): the
//! hardened key storage this stream must eventually sign with belongs to
//! the in-flight T2.1 lane, so production key wiring plugs in behind the
//! trait after that lane closes — nothing here touches
//! `transport_session`/`key_store`.
//!
//! Contracts implemented here (ADR 0086 TC-D3/D4/D12):
//! - append is origin-owned only: the API takes local mutations and chains
//!   them; inbound material has no path into it (no echo by construction);
//! - the closed metadata allowlist is enforced by `deny_unknown_fields` on
//!   the typed payloads — the schema drafts' `additionalProperties: false`
//!   with serde teeth;
//! - every body-carrying revision takes a caller-supplied fresh 32-byte
//!   nonce; the commitment is blake3 domain-separated over nonce + exact
//!   bytes, and the nonce lands only in the body-fetch-only sidecar —
//!   content-identical revisions are unlinkable to metadata-only peers.

use serde::{Deserialize, Serialize};

use crate::db::{
    CreateMeshOriginEventInput, DbConnection, MeshOriginAppendError, StoredMeshOriginEvent,
};

/// Wire schema id of the outer event (draft: docs/schemas/ee.mesh.origin_event.v1.json).
pub const ORIGIN_EVENT_SCHEMA_V1: &str = "ee.mesh.origin_event.v1";
/// Typed memory payload schema id.
pub const MEMORY_EVENT_PAYLOAD_SCHEMA_V1: &str = "ee.mesh.memory_event.v1";
/// Typed team-manifest payload schema id.
pub const MANIFEST_EVENT_PAYLOAD_SCHEMA_V1: &str = "ee.team.manifest_event.v1";
/// Domain separator for the event signature preimage.
pub const ORIGIN_EVENT_SIGNATURE_DOMAIN: &str = "ee.mesh.origin_event.signature.v1";
/// Domain separator for the salted body commitment.
pub const BODY_COMMITMENT_DOMAIN: &str = "ee.mesh.body_commitment.v1";
/// Event-id prefix; `mesh_oevt_` + 26 hash-derived chars = 36 (V104 CHECK).
pub const ORIGIN_EVENT_ID_PREFIX: &str = "mesh_oevt_";

/// Signing seam: T2.1's hardened key storage implements this after that
/// lane closes; tests use a deterministic signer. Implementations MUST
/// domain-separate (the canonical bytes passed in are NOT pre-separated).
pub trait OriginSigner {
    fn signing_key_generation(&self) -> u64;
    fn sign(&self, domain: &str, canonical_bytes: &[u8]) -> String;
}

/// Memory operation vocabulary — create | revise | tombstone | shareWithdraw.
/// There is NO `update` kind; trust/validity/bodyAvailable are post-v1.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryEventOperation {
    Create,
    Revise,
    Tombstone,
    ShareWithdraw,
}

/// Closed-allowlist memory payload. `deny_unknown_fields` IS the non-leak
/// contract: body text, titles/previews, tags, provenance URIs, raw paths,
/// evidence bodies, and the commitment nonce are structurally unrepresentable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryEventPayload {
    pub operation: MemoryEventOperation,
    pub logical_memory_id: String,
    pub revision_id: String,
    #[serde(default)]
    pub predecessor_revision_id: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub memory_kind: Option<String>,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub valid_until: Option<String>,
    #[serde(default)]
    pub project_binding: Option<String>,
    #[serde(default)]
    pub origin_trust_claim: Option<String>,
    #[serde(default)]
    pub provenance_refs: Vec<String>,
    #[serde(default)]
    pub body_representation: Option<String>,
    #[serde(default)]
    pub redaction_provenance: Option<String>,
    /// Salted commitment (`blake3:` + 64 hex); see [`body_commitment`].
    pub body_commitment: String,
}

/// Typed manifest payload (operation vocabulary finalized by T4.1).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestEventPayload {
    pub operation: String,
    pub document_id: String,
    #[serde(default)]
    pub predecessor_revision_id: Option<String>,
    pub document_payload: serde_json::Value,
}

/// One typed payload for the outer event.
#[derive(Clone, Debug, PartialEq)]
pub enum OriginEventPayload {
    Memory(MemoryEventPayload),
    Manifest(ManifestEventPayload),
}

impl OriginEventPayload {
    #[must_use]
    pub fn schema(&self) -> &'static str {
        match self {
            Self::Memory(_) => MEMORY_EVENT_PAYLOAD_SCHEMA_V1,
            Self::Manifest(_) => MANIFEST_EVENT_PAYLOAD_SCHEMA_V1,
        }
    }

    fn to_canonical_json(&self) -> Result<String, OriginStreamError> {
        let value = match self {
            Self::Memory(payload) => serde_json::to_value(payload),
            Self::Manifest(payload) => serde_json::to_value(payload),
        }
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;
        canonical_json_string(&value)
    }
}

/// Stream-core error vocabulary.
#[derive(Debug)]
pub enum OriginStreamError {
    Encode(String),
    /// The V104 chain invariant refused the append — durable fork/regression
    /// evidence, never silently repaired.
    ChainMismatch(String),
    Db(String),
    /// A payload failed the closed-allowlist parse on the way back out.
    PayloadInvalid(String),
}

impl std::fmt::Display for OriginStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(message) => write!(f, "origin event encoding failed: {message}"),
            Self::ChainMismatch(message) => write!(f, "origin chain refused append: {message}"),
            Self::Db(message) => write!(f, "origin stream storage error: {message}"),
            Self::PayloadInvalid(message) => write!(f, "origin payload invalid: {message}"),
        }
    }
}

impl std::error::Error for OriginStreamError {}

/// Deterministic canonical JSON: objects sorted by key recursively, compact
/// separators. The eventHash preimage and the signature preimage both use it.
fn canonical_json_string(value: &serde_json::Value) -> Result<String, OriginStreamError> {
    fn sort(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut sorted = serde_json::Map::new();
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    sorted.insert(key.clone(), sort(&map[key]));
                }
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(sort).collect())
            }
            other => other.clone(),
        }
    }
    serde_json::to_string(&sort(value))
        .map_err(|error| OriginStreamError::Encode(error.to_string()))
}

/// Salted body commitment: blake3 over the length-prefixed domain, the
/// 32-byte nonce, then the exact body bytes. Fresh nonce per revision makes
/// equal bodies unlinkable to metadata-only peers.
#[must_use]
pub fn body_commitment(nonce: &[u8; 32], body: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    let domain = BODY_COMMITMENT_DOMAIN.as_bytes();
    hasher.update(&(domain.len() as u64).to_le_bytes());
    hasher.update(domain);
    hasher.update(nonce);
    hasher.update(body);
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Everything the origin controls about one append, before hashing/signing.
#[derive(Clone, Debug)]
pub struct OriginAppendRequest<'a> {
    pub team_id: &'a str,
    pub origin_node_id: &'a str,
    pub payload: OriginEventPayload,
    pub required_features: Vec<String>,
    pub produced_at: &'a str,
    /// Fresh 32-byte nonce for body-carrying memory revisions; stored in the
    /// body-fetch-only sidecar, never in the event.
    pub body_nonce: Option<[u8; 32]>,
}

/// The persisted result of one append.
#[derive(Clone, Debug, PartialEq)]
pub struct AppendedOriginEvent {
    pub event_id: String,
    pub seq: u64,
    pub event_hash: String,
    pub prev_event_hash: Option<String>,
}

/// Append one origin-owned mutation: read the tip, build the canonical
/// preimage (excluding eventId and signature), hash, sign through the seam,
/// and write transactionally. The V104 chain check re-validates under the
/// write transaction, so a racing append surfaces as `ChainMismatch`, never
/// as a silent fork.
pub fn append_origin_event(
    connection: &DbConnection,
    signer: &dyn OriginSigner,
    request: &OriginAppendRequest<'_>,
) -> Result<AppendedOriginEvent, OriginStreamError> {
    let tip = connection
        .mesh_origin_tip(request.team_id, request.origin_node_id)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    let (seq, prev_event_hash) = match &tip {
        None => (0, None),
        Some((tip_seq, tip_hash)) => (tip_seq.saturating_add(1), Some(tip_hash.clone())),
    };

    let payload_json = request.payload.to_canonical_json()?;
    let mut features = request.required_features.clone();
    features.sort();
    features.dedup();
    let required_features_json = serde_json::to_string(&features)
        .map_err(|error| OriginStreamError::Encode(error.to_string()))?;

    let preimage_value = serde_json::json!({
        "schema": ORIGIN_EVENT_SCHEMA_V1,
        "teamId": request.team_id,
        "originNodeId": request.origin_node_id,
        "signingKeyGeneration": signer.signing_key_generation(),
        "seq": seq,
        "prevEventHash": prev_event_hash,
        "payloadSchema": request.payload.schema(),
        "payload": serde_json::from_str::<serde_json::Value>(&payload_json)
            .map_err(|error| OriginStreamError::Encode(error.to_string()))?,
        "requiredFeatures": features,
        "producedAt": request.produced_at,
    });
    let canonical = canonical_json_string(&preimage_value)?;
    let event_hash = format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex());
    let event_id = format!(
        "{ORIGIN_EVENT_ID_PREFIX}{}",
        &blake3::hash(event_hash.as_bytes()).to_hex().as_str()[..26]
    );
    let signature = signer.sign(ORIGIN_EVENT_SIGNATURE_DOMAIN, canonical.as_bytes());

    let input = CreateMeshOriginEventInput {
        event_id: event_id.clone(),
        team_id: request.team_id.to_owned(),
        origin_node_id: request.origin_node_id.to_owned(),
        signing_key_generation: signer.signing_key_generation(),
        seq,
        prev_event_hash: prev_event_hash.clone(),
        event_hash: event_hash.clone(),
        signature,
        payload_schema: request.payload.schema().to_owned(),
        payload_json,
        required_features_json,
        produced_at: request.produced_at.to_owned(),
        body_nonce_hex: request.body_nonce.map(hex_lower),
    };
    connection
        .append_mesh_origin_event(&input)
        .map_err(|error| match error {
            MeshOriginAppendError::ChainMismatch { .. } => {
                OriginStreamError::ChainMismatch(error.to_string())
            }
            MeshOriginAppendError::Db(db_error) => OriginStreamError::Db(db_error.to_string()),
        })?;

    Ok(AppendedOriginEvent {
        event_id,
        seq,
        event_hash,
        prev_event_hash,
    })
}

/// Parse a stored event's payload back through the closed allowlist. An
/// unknown field anywhere is a hard error — replay never widens the contract.
pub fn parse_stored_payload(
    event: &StoredMeshOriginEvent,
) -> Result<OriginEventPayload, OriginStreamError> {
    match event.payload_schema.as_str() {
        MEMORY_EVENT_PAYLOAD_SCHEMA_V1 => {
            serde_json::from_str::<MemoryEventPayload>(&event.payload_json)
                .map(OriginEventPayload::Memory)
                .map_err(|error| OriginStreamError::PayloadInvalid(error.to_string()))
        }
        MANIFEST_EVENT_PAYLOAD_SCHEMA_V1 => {
            serde_json::from_str::<ManifestEventPayload>(&event.payload_json)
                .map(OriginEventPayload::Manifest)
                .map_err(|error| OriginStreamError::PayloadInvalid(error.to_string()))
        }
        other => Err(OriginStreamError::PayloadInvalid(format!(
            "unknown payload schema {other}"
        ))),
    }
}

/// Verification seam for inbound events: T2.1's key storage (and T3.1's
/// member/node tables) later resolve `(origin_node_id, generation)` to a real
/// Ed25519 public key; tests inject deterministic verifiers.
pub trait OriginSignatureVerifier {
    fn verify(
        &self,
        origin_node_id: &str,
        signing_key_generation: u64,
        domain: &str,
        canonical_bytes: &[u8],
        signature: &str,
    ) -> bool;
}

/// One inbound wire event, exactly the outer draft-schema shape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboundOriginEvent {
    pub schema: String,
    pub event_id: String,
    pub team_id: String,
    pub origin_node_id: String,
    pub signing_key_generation: u64,
    pub seq: u64,
    pub prev_event_hash: Option<String>,
    pub event_hash: String,
    pub signature: String,
    pub payload_schema: String,
    pub payload: serde_json::Value,
    pub required_features: Vec<String>,
    pub produced_at: String,
}

/// Sparse receiver dispositions (ADR 0086 TC-D4): recorded per `(origin,
/// seq)` independently of the receipt frontier, so withheld N beside
/// applied N+1 is truthful state, not an error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IngestDisposition {
    Applied,
    /// Structurally sound but not safely applicable yet (e.g. an unknown
    /// MANDATORY feature bit); hydration later legally flips it to applied.
    Withheld {
        reason: String,
    },
    /// Integrity failure: hash/id/signature mismatch or fork evidence.
    /// Never applied, never silently dropped.
    Quarantined {
        reason: String,
    },
    /// The receiver does not understand the payload schema at all.
    Unsupported {
        reason: String,
    },
}

impl IngestDisposition {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Withheld { .. } => "withheld",
            Self::Quarantined { .. } => "quarantined",
            Self::Unsupported { .. } => "unsupported",
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Applied => "verified and applied".to_owned(),
            Self::Withheld { reason }
            | Self::Quarantined { reason }
            | Self::Unsupported { reason } => reason.clone(),
        }
    }
}

/// Receiver-side verification and disposition for one inbound origin event
/// (T2.0 acceptance: eventId / canonical / signature / team / generation /
/// fork checks; no echo; mandatory-bit omission).
///
/// Order matters and is deliberate: identity/no-echo first, then integrity
/// (canonical bytes → eventHash → eventId → signature), then fork evidence
/// against the local chain record, then feature gating, then payload
/// typing. The first failure wins and is durably recorded as the sparse
/// disposition for `(origin, seq)` — this function never applies material
/// (materialization into memories belongs to later beads); `Applied` means
/// "verified safe to apply".
pub fn ingest_origin_event(
    connection: &DbConnection,
    verifier: &dyn OriginSignatureVerifier,
    own_origin_node_id: &str,
    supported_features: &std::collections::BTreeSet<String>,
    event: &InboundOriginEvent,
    recorded_at: &str,
) -> Result<IngestDisposition, OriginStreamError> {
    let disposition = classify_inbound(
        connection,
        verifier,
        own_origin_node_id,
        supported_features,
        event,
    )?;
    connection
        .record_mesh_origin_disposition(
            &event.team_id,
            &event.origin_node_id,
            event.seq,
            disposition.as_str(),
            &disposition.reason(),
            recorded_at,
        )
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    Ok(disposition)
}

fn classify_inbound(
    connection: &DbConnection,
    verifier: &dyn OriginSignatureVerifier,
    own_origin_node_id: &str,
    supported_features: &std::collections::BTreeSet<String>,
    event: &InboundOriginEvent,
) -> Result<IngestDisposition, OriginStreamError> {
    if event.schema != ORIGIN_EVENT_SCHEMA_V1 {
        return Ok(IngestDisposition::Unsupported {
            reason: format!("unknown outer schema {}", event.schema),
        });
    }
    // No echo: origin-owned material never re-enters through ingest.
    if event.origin_node_id == own_origin_node_id {
        return Ok(IngestDisposition::Quarantined {
            reason: "echo refused: event claims THIS node as origin".to_owned(),
        });
    }

    // Integrity: recompute the canonical preimage from the received fields.
    let mut features = event.required_features.clone();
    features.sort();
    features.dedup();
    let preimage_value = serde_json::json!({
        "schema": ORIGIN_EVENT_SCHEMA_V1,
        "teamId": event.team_id,
        "originNodeId": event.origin_node_id,
        "signingKeyGeneration": event.signing_key_generation,
        "seq": event.seq,
        "prevEventHash": event.prev_event_hash,
        "payloadSchema": event.payload_schema,
        "payload": event.payload,
        "requiredFeatures": features,
        "producedAt": event.produced_at,
    });
    let canonical = canonical_json_string(&preimage_value)?;
    let expected_hash = format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex());
    if expected_hash != event.event_hash {
        return Ok(IngestDisposition::Quarantined {
            reason: "eventHash does not match the canonical bytes".to_owned(),
        });
    }
    let expected_id = format!(
        "{ORIGIN_EVENT_ID_PREFIX}{}",
        &blake3::hash(event.event_hash.as_bytes()).to_hex().as_str()[..26]
    );
    if expected_id != event.event_id {
        return Ok(IngestDisposition::Quarantined {
            reason: "eventId is not derived from eventHash".to_owned(),
        });
    }
    if !verifier.verify(
        &event.origin_node_id,
        event.signing_key_generation,
        ORIGIN_EVENT_SIGNATURE_DOMAIN,
        canonical.as_bytes(),
        &event.signature,
    ) {
        return Ok(IngestDisposition::Quarantined {
            reason: format!(
                "signature failed for origin {} at generation {}",
                event.origin_node_id, event.signing_key_generation
            ),
        });
    }

    // Fork evidence: a DIFFERENT event already occupies this (origin, seq)
    // locally (our own record of the origin's chain).
    let existing = connection
        .list_mesh_origin_events(&event.team_id, &event.origin_node_id, event.seq, 1)
        .map_err(|error| OriginStreamError::Db(error.to_string()))?;
    if let Some(existing) = existing.first()
        && existing.seq == event.seq
        && existing.event_hash != event.event_hash
    {
        return Ok(IngestDisposition::Quarantined {
            reason: format!(
                "fork evidence: seq {} already recorded with a different eventHash",
                event.seq
            ),
        });
    }

    // Feature gating: requiredFeatures can ADD checks but never remove them;
    // an unknown mandatory bit withholds (sparse), it never fails the stream.
    for feature in &features {
        if !supported_features.contains(feature) {
            return Ok(IngestDisposition::Withheld {
                reason: format!("unknown mandatory feature {feature}"),
            });
        }
    }

    // Payload typing through the closed allowlist.
    match event.payload_schema.as_str() {
        MEMORY_EVENT_PAYLOAD_SCHEMA_V1 => {
            if let Err(error) = serde_json::from_value::<MemoryEventPayload>(event.payload.clone())
            {
                return Ok(IngestDisposition::Quarantined {
                    reason: format!("memory payload failed the closed allowlist: {error}"),
                });
            }
        }
        MANIFEST_EVENT_PAYLOAD_SCHEMA_V1 => {
            if let Err(error) =
                serde_json::from_value::<ManifestEventPayload>(event.payload.clone())
            {
                return Ok(IngestDisposition::Quarantined {
                    reason: format!("manifest payload failed the closed allowlist: {error}"),
                });
            }
        }
        other => {
            return Ok(IngestDisposition::Unsupported {
                reason: format!("unknown payload schema {other}"),
            });
        }
    }

    Ok(IngestDisposition::Applied)
}

fn hex_lower(bytes: [u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const TEAM: &str = "team_0000000000000000000000001";
    const ORIGIN: &str = "node_0000000000000000000000001";

    struct TestSigner;

    impl OriginSigner for TestSigner {
        fn signing_key_generation(&self) -> u64 {
            1
        }

        fn sign(&self, domain: &str, canonical_bytes: &[u8]) -> String {
            // Deterministic stand-in until T2.1's key storage lands: a real
            // implementation signs with Ed25519; the seam's contract (domain
            // + canonical bytes in, opaque signature string out) is what the
            // stream core depends on.
            format!(
                "testsig:{}",
                blake3::hash(&[domain.as_bytes(), canonical_bytes].concat()).to_hex()
            )
        }
    }

    fn open_db() -> DbConnection {
        let connection = DbConnection::open_memory().expect("open in-memory db");
        connection.migrate().expect("migrate");
        connection
    }

    fn memory_payload(revision: &str, commitment: &str) -> OriginEventPayload {
        OriginEventPayload::Memory(MemoryEventPayload {
            operation: MemoryEventOperation::Create,
            logical_memory_id: "olm_00000000000000000000000001".to_owned(),
            revision_id: revision.to_owned(),
            predecessor_revision_id: None,
            level: Some("semantic".to_owned()),
            memory_kind: Some("fact".to_owned()),
            valid_from: None,
            valid_until: None,
            project_binding: None,
            origin_trust_claim: Some("agent_assertion".to_owned()),
            provenance_refs: Vec::new(),
            body_representation: None,
            redaction_provenance: None,
            body_commitment: commitment.to_owned(),
        })
    }

    fn append(
        connection: &DbConnection,
        payload: OriginEventPayload,
        nonce: Option<[u8; 32]>,
    ) -> Result<AppendedOriginEvent, OriginStreamError> {
        append_origin_event(
            connection,
            &TestSigner,
            &OriginAppendRequest {
                team_id: TEAM,
                origin_node_id: ORIGIN,
                payload,
                required_features: vec!["mesh.origin_stream.v1".to_owned()],
                produced_at: "2026-08-11T00:00:00Z",
                body_nonce: nonce,
            },
        )
    }

    #[test]
    fn chain_appends_link_and_survive_round_trip() {
        let connection = open_db();
        let nonce = [7_u8; 32];
        let commitment = body_commitment(&nonce, b"the exact body bytes");
        let first = append(
            &connection,
            memory_payload("rev_a", &commitment),
            Some(nonce),
        )
        .expect("first append");
        assert_eq!(first.seq, 0);
        assert_eq!(first.prev_event_hash, None);

        let second =
            append(&connection, memory_payload("rev_b", &commitment), None).expect("second append");
        assert_eq!(second.seq, 1);
        assert_eq!(
            second.prev_event_hash.as_deref(),
            Some(first.event_hash.as_str())
        );

        let events = connection
            .list_mesh_origin_events(TEAM, ORIGIN, 0, 16)
            .expect("list");
        assert_eq!(events.len(), 2);
        let parsed = parse_stored_payload(&events[0]).expect("payload parses");
        match parsed {
            OriginEventPayload::Memory(payload) => {
                assert_eq!(payload.revision_id, "rev_a");
                assert_eq!(payload.body_commitment, commitment);
            }
            OriginEventPayload::Manifest(_) => panic!("expected memory payload"),
        }
    }

    #[test]
    fn nonce_lives_only_in_the_sidecar_never_in_the_event_row() {
        let connection = open_db();
        let nonce = [9_u8; 32];
        let commitment = body_commitment(&nonce, b"secret protocol body");
        let appended = append(
            &connection,
            memory_payload("rev_a", &commitment),
            Some(nonce),
        )
        .expect("append");

        let stored = connection
            .list_mesh_origin_events(TEAM, ORIGIN, 0, 1)
            .expect("list")
            .remove(0);
        let nonce_hex = super::hex_lower(nonce);
        for field in [
            &stored.payload_json,
            &stored.event_hash,
            &stored.signature,
            &stored.required_features_json,
        ] {
            assert!(
                !field.contains(&nonce_hex),
                "event row field leaked the nonce: {field}"
            );
        }
        assert_eq!(
            connection
                .mesh_origin_event_nonce(&appended.event_id)
                .expect("nonce read"),
            Some(nonce_hex)
        );
    }

    #[test]
    fn equal_bodies_with_fresh_nonces_are_unlinkable() {
        let body = b"identical body bytes across two revisions";
        let commitment_a = body_commitment(&[1_u8; 32], body);
        let commitment_b = body_commitment(&[2_u8; 32], body);
        assert_ne!(
            commitment_a, commitment_b,
            "fresh nonce must unlink equal bodies"
        );
        // And the SAME nonce+body reproduces exactly (fetch-side verification).
        assert_eq!(commitment_a, body_commitment(&[1_u8; 32], body));
    }

    #[test]
    fn closed_allowlist_rejects_unknown_and_body_carrying_fields() {
        let smuggled = serde_json::json!({
            "operation": "create",
            "logicalMemoryId": "olm_00000000000000000000000001",
            "revisionId": "rev_x",
            "bodyCommitment": "blake3:aa",
            "bodyText": "the actual secret body"
        });
        let error = serde_json::from_value::<MemoryEventPayload>(smuggled).unwrap_err();
        assert!(
            error.to_string().contains("bodyText"),
            "unknown field must be named: {error}"
        );
        for field in ["title", "tags", "provenanceUri", "path", "nonce"] {
            let mut probe = serde_json::json!({
                "operation": "revise",
                "logicalMemoryId": "olm_00000000000000000000000001",
                "revisionId": "rev_y",
                "bodyCommitment": "blake3:aa",
            });
            probe[field] = serde_json::Value::String("x".to_owned());
            assert!(
                serde_json::from_value::<MemoryEventPayload>(probe).is_err(),
                "{field} must be structurally unrepresentable"
            );
        }
    }

    #[test]
    fn stale_tip_race_is_refused_as_chain_mismatch() {
        let connection = open_db();
        let commitment = body_commitment(&[3_u8; 32], b"body");
        append(&connection, memory_payload("rev_a", &commitment), None).expect("first");

        // Simulate a racing writer: hand-build an input at the stale seq 0.
        let stale = CreateMeshOriginEventInput {
            event_id: "mesh_oevt_00000000000000000000000000"
                .chars()
                .take(36)
                .collect(),
            team_id: TEAM.to_owned(),
            origin_node_id: ORIGIN.to_owned(),
            signing_key_generation: 1,
            seq: 0,
            prev_event_hash: None,
            event_hash: format!("blake3:{}", blake3::hash(b"stale").to_hex()),
            signature: "testsig:stale".to_owned(),
            payload_schema: MEMORY_EVENT_PAYLOAD_SCHEMA_V1.to_owned(),
            payload_json: "{}".to_owned(),
            required_features_json: "[]".to_owned(),
            produced_at: "2026-08-11T00:00:01Z".to_owned(),
            body_nonce_hex: None,
        };
        let error = connection.append_mesh_origin_event(&stale).unwrap_err();
        assert!(
            matches!(
                error,
                MeshOriginAppendError::ChainMismatch {
                    expected_seq: 1,
                    ..
                }
            ),
            "stale append must be refused as fork evidence: {error}"
        );
    }

    #[test]
    fn sparse_dispositions_hold_withheld_before_applied_and_hydrate() {
        let connection = open_db();
        connection
            .record_mesh_origin_disposition(
                TEAM,
                ORIGIN,
                4,
                "withheld",
                "unknown mandatory feature",
                "t1",
            )
            .expect("withhold 4");
        connection
            .record_mesh_origin_disposition(TEAM, ORIGIN, 5, "applied", "ok", "t1")
            .expect("apply 5");
        let rows = connection
            .list_mesh_origin_dispositions(TEAM, ORIGIN, 16)
            .expect("list");
        assert_eq!(
            rows[0],
            (
                4_u64,
                "withheld".to_owned(),
                "unknown mandatory feature".to_owned()
            )
        );
        assert_eq!(rows[1].1, "applied");

        // Hydration legally flips withheld -> applied in place.
        connection
            .record_mesh_origin_disposition(TEAM, ORIGIN, 4, "applied", "feature adopted", "t2")
            .expect("hydrate 4");
        let rows = connection
            .list_mesh_origin_dispositions(TEAM, ORIGIN, 16)
            .expect("list");
        assert_eq!(rows[0].1, "applied");
    }

    struct TestVerifier;

    impl OriginSignatureVerifier for TestVerifier {
        fn verify(
            &self,
            _origin_node_id: &str,
            _signing_key_generation: u64,
            domain: &str,
            canonical_bytes: &[u8],
            signature: &str,
        ) -> bool {
            signature
                == format!(
                    "testsig:{}",
                    blake3::hash(&[domain.as_bytes(), canonical_bytes].concat()).to_hex()
                )
        }
    }

    const PEER_ORIGIN: &str = "node_0000000000000000000000peer1";
    const OWN_NODE: &str = "node_00000000000000000000000self";

    fn make_inbound(
        seq: u64,
        prev_event_hash: Option<String>,
        features: Vec<String>,
    ) -> InboundOriginEvent {
        let payload = serde_json::json!({
            "operation": "create",
            "logicalMemoryId": "olm_00000000000000000000000009",
            "revisionId": format!("rev_{seq}"),
            "bodyCommitment": "blake3:aa",
        });
        let mut sorted_features = features.clone();
        sorted_features.sort();
        sorted_features.dedup();
        let preimage = serde_json::json!({
            "schema": ORIGIN_EVENT_SCHEMA_V1,
            "teamId": TEAM,
            "originNodeId": PEER_ORIGIN,
            "signingKeyGeneration": 1,
            "seq": seq,
            "prevEventHash": prev_event_hash,
            "payloadSchema": MEMORY_EVENT_PAYLOAD_SCHEMA_V1,
            "payload": payload,
            "requiredFeatures": sorted_features,
            "producedAt": "2026-08-11T00:00:00Z",
        });
        let canonical = canonical_json_string(&preimage).unwrap();
        let event_hash = format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex());
        let event_id = format!(
            "{ORIGIN_EVENT_ID_PREFIX}{}",
            &blake3::hash(event_hash.as_bytes()).to_hex().as_str()[..26]
        );
        let signature = TestSigner.sign(ORIGIN_EVENT_SIGNATURE_DOMAIN, canonical.as_bytes());
        InboundOriginEvent {
            schema: ORIGIN_EVENT_SCHEMA_V1.to_owned(),
            event_id,
            team_id: TEAM.to_owned(),
            origin_node_id: PEER_ORIGIN.to_owned(),
            signing_key_generation: 1,
            seq,
            prev_event_hash,
            event_hash,
            signature,
            payload_schema: MEMORY_EVENT_PAYLOAD_SCHEMA_V1.to_owned(),
            payload,
            required_features: features,
            produced_at: "2026-08-11T00:00:00Z".to_owned(),
        }
    }

    fn supported() -> std::collections::BTreeSet<String> {
        ["mesh.origin_stream.v1".to_owned()].into_iter().collect()
    }

    #[test]
    fn ingest_applies_a_faithful_event_and_records_the_disposition() {
        let connection = open_db();
        let event = make_inbound(0, None, vec!["mesh.origin_stream.v1".to_owned()]);
        let disposition = ingest_origin_event(
            &connection,
            &TestVerifier,
            OWN_NODE,
            &supported(),
            &event,
            "t1",
        )
        .expect("ingest");
        assert_eq!(disposition, IngestDisposition::Applied);
        let rows = connection
            .list_mesh_origin_dispositions(TEAM, PEER_ORIGIN, 4)
            .expect("list");
        assert_eq!(rows[0].1, "applied");
    }

    #[test]
    fn tampered_payload_and_bad_signature_quarantine() {
        let connection = open_db();
        let mut tampered = make_inbound(0, None, Vec::new());
        tampered.payload["revisionId"] = serde_json::Value::String("rev_tampered".to_owned());
        let disposition = ingest_origin_event(
            &connection,
            &TestVerifier,
            OWN_NODE,
            &supported(),
            &tampered,
            "t1",
        )
        .expect("ingest");
        assert!(
            matches!(&disposition, IngestDisposition::Quarantined { reason } if reason.contains("eventHash")),
            "tampered payload must quarantine on hash: {disposition:?}"
        );

        let mut forged = make_inbound(1, None, Vec::new());
        forged.signature = "testsig:forged".to_owned();
        let disposition = ingest_origin_event(
            &connection,
            &TestVerifier,
            OWN_NODE,
            &supported(),
            &forged,
            "t1",
        )
        .expect("ingest");
        assert!(
            matches!(&disposition, IngestDisposition::Quarantined { reason } if reason.contains("signature")),
            "forged signature must quarantine: {disposition:?}"
        );
    }

    #[test]
    fn unknown_mandatory_feature_withholds_sparsely_then_hydrates() {
        let connection = open_db();
        let gated = make_inbound(0, None, vec!["mesh.future_feature.v9".to_owned()]);
        let disposition = ingest_origin_event(
            &connection,
            &TestVerifier,
            OWN_NODE,
            &supported(),
            &gated,
            "t1",
        )
        .expect("ingest 0");
        assert!(matches!(disposition, IngestDisposition::Withheld { .. }));

        // Sparse: N+1 applies while N stays withheld (frontier-independent).
        let next = make_inbound(1, Some(gated.event_hash.clone()), Vec::new());
        let disposition = ingest_origin_event(
            &connection,
            &TestVerifier,
            OWN_NODE,
            &supported(),
            &next,
            "t1",
        )
        .expect("ingest 1");
        assert_eq!(disposition, IngestDisposition::Applied);
        let rows = connection
            .list_mesh_origin_dispositions(TEAM, PEER_ORIGIN, 4)
            .expect("list");
        assert_eq!((rows[0].0, rows[0].1.as_str()), (0_u64, "withheld"));
        assert_eq!((rows[1].0, rows[1].1.as_str()), (1_u64, "applied"));

        // Hydration: the feature becomes supported; re-ingest flips 0 in place.
        let mut wider = supported();
        wider.insert("mesh.future_feature.v9".to_owned());
        let disposition =
            ingest_origin_event(&connection, &TestVerifier, OWN_NODE, &wider, &gated, "t2")
                .expect("re-ingest 0");
        assert_eq!(disposition, IngestDisposition::Applied);
        let rows = connection
            .list_mesh_origin_dispositions(TEAM, PEER_ORIGIN, 4)
            .expect("list");
        assert_eq!((rows[0].0, rows[0].1.as_str()), (0_u64, "applied"));
    }

    #[test]
    fn fork_echo_and_unknown_schema_are_refused_distinctly() {
        let connection = open_db();
        // Seed the local record of the peer's chain at seq 0 via the append
        // path, then present a DIFFERENT seq-0 event: fork evidence.
        let commitment = body_commitment(&[4_u8; 32], b"peer body");
        append_origin_event(
            &connection,
            &TestSigner,
            &OriginAppendRequest {
                team_id: TEAM,
                origin_node_id: PEER_ORIGIN,
                payload: memory_payload("rev_recorded", &commitment),
                required_features: Vec::new(),
                produced_at: "2026-08-10T23:59:59Z",
                body_nonce: None,
            },
        )
        .expect("seed local record");
        let divergent = make_inbound(0, None, Vec::new());
        let disposition = ingest_origin_event(
            &connection,
            &TestVerifier,
            OWN_NODE,
            &supported(),
            &divergent,
            "t1",
        )
        .expect("ingest divergent");
        assert!(
            matches!(&disposition, IngestDisposition::Quarantined { reason } if reason.contains("fork")),
            "divergent seq-0 must be fork evidence: {disposition:?}"
        );

        let mut echo = make_inbound(7, None, Vec::new());
        echo.origin_node_id = OWN_NODE.to_owned();
        let disposition = ingest_origin_event(
            &connection,
            &TestVerifier,
            OWN_NODE,
            &supported(),
            &echo,
            "t1",
        )
        .expect("ingest echo");
        assert!(
            matches!(&disposition, IngestDisposition::Quarantined { reason } if reason.contains("echo")),
            "echo must be refused: {disposition:?}"
        );

        let mut alien = make_inbound(8, None, Vec::new());
        alien.payload_schema = "ee.mesh.telepathy_event.v1".to_owned();
        resign(&mut alien); // consistent hash/id/signature so the SCHEMA check is what fires
        let disposition = ingest_origin_event(
            &connection,
            &TestVerifier,
            OWN_NODE,
            &supported(),
            &alien,
            "t1",
        )
        .expect("ingest alien");
        assert!(
            matches!(disposition, IngestDisposition::Unsupported { .. }),
            "unknown payload schema is unsupported, not quarantined: {disposition:?}"
        );
    }

    /// Recompute canonical/hash/id/signature from the event's CURRENT fields
    /// so negative tests can isolate exactly one failing check.
    fn resign(event: &mut InboundOriginEvent) {
        let mut features = event.required_features.clone();
        features.sort();
        features.dedup();
        let preimage = serde_json::json!({
            "schema": ORIGIN_EVENT_SCHEMA_V1,
            "teamId": event.team_id,
            "originNodeId": event.origin_node_id,
            "signingKeyGeneration": event.signing_key_generation,
            "seq": event.seq,
            "prevEventHash": event.prev_event_hash,
            "payloadSchema": event.payload_schema,
            "payload": event.payload,
            "requiredFeatures": features,
            "producedAt": event.produced_at,
        });
        let canonical = canonical_json_string(&preimage).unwrap();
        event.event_hash = format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex());
        event.event_id = format!(
            "{ORIGIN_EVENT_ID_PREFIX}{}",
            &blake3::hash(event.event_hash.as_bytes()).to_hex().as_str()[..26]
        );
        event.signature = TestSigner.sign(ORIGIN_EVENT_SIGNATURE_DOMAIN, canonical.as_bytes());
    }

    #[test]
    fn known_answer_vectors_pin_the_preimage_encodings() {
        // tests/fixtures/mesh/origin_stream_vectors.json was generated with
        // an INDEPENDENT implementation (python struct + b3sum), so this
        // test cross-checks the Rust canonicalization, the commitment
        // preimage layout, and the id derivation against fixed constants.
        // A failure here means the WIRE ENCODING changed — that is a
        // deliberate versioned break or a bug, never a golden refresh.
        let raw = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/mesh/origin_stream_vectors.json"),
        )
        .expect("read vectors");
        let vectors: serde_json::Value = serde_json::from_str(&raw).expect("parse vectors");

        let body_vec = &vectors["bodyCommitment"];
        assert_eq!(body_vec["domain"], BODY_COMMITMENT_DOMAIN);
        let nonce_a: [u8; 32] = hex_to_32(body_vec["nonceHexA"].as_str().unwrap());
        let nonce_b: [u8; 32] = hex_to_32(body_vec["nonceHexB"].as_str().unwrap());
        let body = body_vec["bodyUtf8"].as_str().unwrap().as_bytes();
        assert_eq!(
            body_commitment(&nonce_a, body),
            body_vec["commitmentA"].as_str().unwrap()
        );
        assert_eq!(
            body_commitment(&nonce_b, body),
            body_vec["commitmentB"].as_str().unwrap()
        );

        let encoding = &vectors["eventEncoding"];
        let commitment_a = body_vec["commitmentA"].as_str().unwrap();
        let preimage = serde_json::json!({
            "schema": ORIGIN_EVENT_SCHEMA_V1,
            "teamId": TEAM,
            "originNodeId": ORIGIN,
            "signingKeyGeneration": 1,
            "seq": 0,
            "prevEventHash": serde_json::Value::Null,
            "payloadSchema": MEMORY_EVENT_PAYLOAD_SCHEMA_V1,
            "payload": {
                "operation": "create",
                "logicalMemoryId": "olm_00000000000000000000000001",
                "revisionId": "rev_vector",
                "bodyCommitment": commitment_a,
            },
            "requiredFeatures": ["mesh.origin_stream.v1"],
            "producedAt": "2026-08-11T00:00:00Z",
        });
        let canonical = canonical_json_string(&preimage).unwrap();
        assert_eq!(
            canonical,
            encoding["canonicalPreimage"].as_str().unwrap(),
            "Rust canonicalization diverged from the independent implementation"
        );
        let event_hash = format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex());
        assert_eq!(event_hash, encoding["eventHash"].as_str().unwrap());
        let event_id = format!(
            "{ORIGIN_EVENT_ID_PREFIX}{}",
            &blake3::hash(event_hash.as_bytes()).to_hex().as_str()[..26]
        );
        assert_eq!(event_id, encoding["eventId"].as_str().unwrap());
    }

    fn hex_to_32(hex: &str) -> [u8; 32] {
        let mut out = [0_u8; 32];
        for (index, chunk) in hex.as_bytes().chunks(2).enumerate() {
            out[index] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap();
        }
        out
    }

    #[test]
    fn canonical_encoding_is_key_order_independent() {
        let scrambled: serde_json::Value =
            serde_json::from_str(r#"{"b":1,"a":{"z":true,"m":[{"k":2,"a":1}]}}"#).unwrap();
        let ordered: serde_json::Value =
            serde_json::from_str(r#"{"a":{"m":[{"a":1,"k":2}],"z":true},"b":1}"#).unwrap();
        assert_eq!(
            canonical_json_string(&scrambled).unwrap(),
            canonical_json_string(&ordered).unwrap()
        );
    }
}
