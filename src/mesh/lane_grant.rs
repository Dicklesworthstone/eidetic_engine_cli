//! Authenticated lane-grant approval tokens (ADR 0086 TC-D6 / TC-D14).
//!
//! A token is a short-lived bearer that proves a specific, canonical preview
//! was authenticated by this store's *current* key. The serialized envelope is
//! deliberately fixed-width and contains no store, workspace, or key ID:
//! version, fresh nonce, issue/expiry seconds, nonce-salted snapshot tag, and
//! a context-bound envelope MAC. The store is bound implicitly by its secret
//! root; workspace and command surface are supplied again at verification and
//! are covered by the MAC without being serialized.
//!
//! Verification is intentionally split. [`verify_authentic`] performs fixed
//! parsing and the current-key envelope-MAC check, returning only `invalid` on
//! bearer/context/key/future-time failure. Only then may [`compare_snapshot`]
//! rebuild the canonical preview inside the caller's write transaction;
//! expiry or authenticated snapshot drift returns `stale`. This split is the
//! security boundary that keeps a forged token distinguishable from an
//! authentic preview whose inputs changed.

use std::fmt;
use std::io::Read as _;
use std::sync::atomic::{Ordering, compiler_fence};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::policy::store_auth::{KeyId, Mac, MacDomain, StoreAuthError, StoreAuthRoot};

/// Recognizable prefix used by ee's central secret redactor.
pub const APPROVAL_TOKEN_PREFIX: &str = "eeap1_";
/// Public schema identifier for the explicitly opted-in sensitive projection.
pub const APPROVAL_TOKEN_SCHEMA_V1: &str = "ee.mesh.approval_token.v1";
/// V1 approval bearer lifetime. Key rotation or a successful generation-CAS
/// mutation can invalidate it earlier.
pub const APPROVAL_TOKEN_TTL_SECONDS: i64 = 900;
/// Error code for malformed, forged, wrong-context, wrong-current-key, or
/// future-issued approval tokens.
pub const MESH_APPROVAL_TOKEN_INVALID_CODE: &str = "mesh_approval_token_invalid";
/// Error code for an authentic but expired or preview-drifted token.
pub const MESH_APPROVAL_TOKEN_STALE_CODE: &str = "mesh_approval_token_stale";
/// Bound for canonical preview material accepted by this security primitive.
/// Candidate enumeration and redacted samples are already bounded by their
/// public preview contract; this is a final allocation/DoS backstop.
pub const MAX_CANONICAL_APPROVAL_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum bytes read from stdin for one bearer, including surrounding ASCII
/// whitespace. A valid v1 bearer is much smaller; the slack is intentional for
/// one trailing newline and future fixed-width envelope versions.
pub const MAX_APPROVAL_TOKEN_INPUT_BYTES: usize = 512;

const TOKEN_VERSION: u8 = 1;
const NONCE_LEN: usize = 32;
const TAG_LEN: usize = 32;
const MAC_LEN: usize = 32;
const VERSION_OFFSET: usize = 0;
const NONCE_OFFSET: usize = VERSION_OFFSET + 1;
const ISSUED_AT_OFFSET: usize = NONCE_OFFSET + NONCE_LEN;
const EXPIRES_AT_OFFSET: usize = ISSUED_AT_OFFSET + 8;
const SNAPSHOT_TAG_OFFSET: usize = EXPIRES_AT_OFFSET + 8;
const ENVELOPE_MAC_OFFSET: usize = SNAPSHOT_TAG_OFFSET + TAG_LEN;
const ENVELOPE_LEN: usize = ENVELOPE_MAC_OFFSET + MAC_LEN;
const ENCODED_ENVELOPE_LEN: usize = 151;
const DECODE_BUFFER_LEN: usize = ENCODED_ENVELOPE_LEN.div_ceil(4) * 3;
/// Exact byte length of a v1 `eeap1_` bearer.
pub const APPROVAL_TOKEN_BEARER_LEN: usize = APPROVAL_TOKEN_PREFIX.len() + ENCODED_ENVELOPE_LEN;

const MAX_WORKSPACE_CONTEXT_BYTES: usize = 4 * 1024;
const MAX_SURFACE_CONTEXT_BYTES: usize = 256;
const LANE_ENVELOPE_MAC_MESSAGE_DOMAIN: &[u8] = b"ee.mesh.lane_approval.envelope.v1";
const LANE_SNAPSHOT_TAG_MESSAGE_DOMAIN: &[u8] = b"ee.mesh.lane_approval.snapshot.v1";
const LANE_AUDIT_ID_MESSAGE_DOMAIN: &[u8] = b"ee.mesh.lane_approval.audit_id.v1";
const BODY_ENVELOPE_MAC_MESSAGE_DOMAIN: &[u8] = b"ee.mesh.body_approval.envelope.v1";
const BODY_SNAPSHOT_TAG_MESSAGE_DOMAIN: &[u8] = b"ee.mesh.body_approval.snapshot.v1";
const BODY_AUDIT_ID_MESSAGE_DOMAIN: &[u8] = b"ee.mesh.body_approval.audit_id.v1";
const CONFIG_DIGEST_MESSAGE_DOMAIN: &[u8] = b"ee.mesh.lane_approval.config.v1\0";
const AUDIT_ID_PREFIX: &str = "eela1_";

/// Which approval authority authenticates a preview bearer.
///
/// Body exposure is intentionally separate from ordinary lane consent. The
/// purpose is invocation context, not a wire field: choosing the wrong purpose
/// changes all three derived subkeys and makes the envelope invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalPurpose {
    /// T1.4 material-lane consent, including the lane named `Body`.
    Lane,
    /// T5.9 unredacted body-sharing consent, not a material-lane grant.
    Body,
}

impl ApprovalPurpose {
    const fn envelope_mac_domain(self) -> MacDomain {
        match self {
            Self::Lane => MacDomain::LaneApprovalEnvelopeMac,
            Self::Body => MacDomain::BodyApprovalEnvelopeMac,
        }
    }

    const fn snapshot_tag_domain(self) -> MacDomain {
        match self {
            Self::Lane => MacDomain::LaneApprovalSnapshotTag,
            Self::Body => MacDomain::BodyApprovalSnapshotTag,
        }
    }

    const fn audit_id_domain(self) -> MacDomain {
        match self {
            Self::Lane => MacDomain::LaneApprovalAuditId,
            Self::Body => MacDomain::BodyApprovalAuditId,
        }
    }

    const fn envelope_message_domain(self) -> &'static [u8] {
        match self {
            Self::Lane => LANE_ENVELOPE_MAC_MESSAGE_DOMAIN,
            Self::Body => BODY_ENVELOPE_MAC_MESSAGE_DOMAIN,
        }
    }

    const fn snapshot_message_domain(self) -> &'static [u8] {
        match self {
            Self::Lane => LANE_SNAPSHOT_TAG_MESSAGE_DOMAIN,
            Self::Body => BODY_SNAPSHOT_TAG_MESSAGE_DOMAIN,
        }
    }

    const fn audit_message_domain(self) -> &'static [u8] {
        match self {
            Self::Lane => LANE_AUDIT_ID_MESSAGE_DOMAIN,
            Self::Body => BODY_AUDIT_ID_MESSAGE_DOMAIN,
        }
    }
}

/// Bind one durable allow override to the exact config-file bytes reviewed by
/// the operator. Runtime policy lookup recomputes this digest from the current
/// file and converts an absent or mismatched allow binding into an explicit
/// deny, closing the filesystem/DB commit race fail-closed.
#[must_use]
pub fn approval_config_digest(config_bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CONFIG_DIGEST_MESSAGE_DOMAIN);
    hasher.update(&(config_bytes.len() as u64).to_be_bytes());
    hasher.update(config_bytes);
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Security-core outcome. It deliberately exposes only the invalid/stale split
/// required by the public error contract. Store-key failures retain their
/// existing fail-closed error so callers can surface
/// `mesh_store_authentication_unavailable` rather than misclassifying them as
/// attacker-controlled input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApprovalTokenError {
    /// The bearer could not be authenticated in the invocation context.
    Invalid,
    /// The bearer was authentic, but expired or no longer matches the preview.
    Stale,
    /// The store-local authentication root or OS randomness was unavailable.
    StoreAuth(StoreAuthError),
}

impl ApprovalTokenError {
    /// Stable public error/degraded code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => MESH_APPROVAL_TOKEN_INVALID_CODE,
            Self::Stale => MESH_APPROVAL_TOKEN_STALE_CODE,
            Self::StoreAuth(error) => error.degraded_code(),
        }
    }

    /// Secret-free message. Detailed parse/MAC failure causes are deliberately
    /// collapsed so the error surface does not become an authentication oracle.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Invalid => {
                "The mesh approval token is invalid for this store, workspace, and command."
                    .to_owned()
            }
            Self::Stale => {
                "The mesh approval token is authentic but its approved preview is stale.".to_owned()
            }
            Self::StoreAuth(error) => error.message(),
        }
    }

    /// Secret-free recovery instruction. No error path mints or embeds a
    /// replacement bearer.
    #[must_use]
    pub fn repair(&self) -> String {
        match self {
            Self::Invalid | Self::Stale => {
                "Run the read-only lane preview again and submit its new approval token through bounded stdin."
                    .to_owned()
            }
            Self::StoreAuth(error) => error.repair(),
        }
    }
}

impl fmt::Display for ApprovalTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ApprovalTokenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StoreAuth(error) => Some(error),
            Self::Invalid | Self::Stale => None,
        }
    }
}

impl From<StoreAuthError> for ApprovalTokenError {
    fn from(error: StoreAuthError) -> Self {
        Self::StoreAuth(error)
    }
}

/// Fixed-width bearer bytes. There is intentionally no `Display`, `Serialize`,
/// or ordinary string accessor: emitting the bearer is an explicit boundary
/// operation through [`ApprovalToken::expose_bearer`]. `Debug` is redacted and
/// the fixed buffer is cleared on drop as a defense against accidental reuse.
pub struct ApprovalToken {
    envelope: [u8; ENVELOPE_LEN],
}

impl ApprovalToken {
    /// Parse a bounded, fixed-width v1 bearer. Leading/trailing whitespace is
    /// accepted for stdin ergonomics; internal whitespace, padding, alternate
    /// alphabets, extra fields, and all other shapes are rejected.
    pub fn parse(input: &str) -> Result<Self, ApprovalTokenError> {
        if input.len() > MAX_APPROVAL_TOKEN_INPUT_BYTES {
            return Err(ApprovalTokenError::Invalid);
        }
        let trimmed = input.trim();
        if trimmed.len() != APPROVAL_TOKEN_BEARER_LEN {
            return Err(ApprovalTokenError::Invalid);
        }
        let encoded = trimmed
            .strip_prefix(APPROVAL_TOKEN_PREFIX)
            .ok_or(ApprovalTokenError::Invalid)?;
        if encoded.len() != ENCODED_ENVELOPE_LEN {
            return Err(ApprovalTokenError::Invalid);
        }

        // base64's checked slice decoder requires its conservative next-full-
        // triple estimate (114 bytes here), while the unpadded envelope is
        // exactly 113. Decode into that bounded scratch and copy only on an
        // exact-length match.
        let mut decoded = [0_u8; DECODE_BUFFER_LEN];
        let decoded_len = URL_SAFE_NO_PAD
            .decode_slice(encoded.as_bytes(), &mut decoded)
            .map_err(|_| ApprovalTokenError::Invalid)?;
        if decoded_len != ENVELOPE_LEN || decoded[VERSION_OFFSET] != TOKEN_VERSION {
            decoded.fill(0);
            compiler_fence(Ordering::SeqCst);
            return Err(ApprovalTokenError::Invalid);
        }
        let mut envelope = [0_u8; ENVELOPE_LEN];
        envelope.copy_from_slice(&decoded[..ENVELOPE_LEN]);
        decoded.fill(0);
        compiler_fence(Ordering::SeqCst);
        Ok(Self { envelope })
    }

    /// Explicitly render the sensitive bearer for the opted-in robot response.
    /// Callers must not log, audit, persist, or place this value in argv/env.
    #[must_use]
    pub fn expose_bearer(&self) -> String {
        let mut bearer = String::with_capacity(APPROVAL_TOKEN_BEARER_LEN);
        bearer.push_str(APPROVAL_TOKEN_PREFIX);
        URL_SAFE_NO_PAD.encode_string(self.envelope.as_slice(), &mut bearer);
        bearer
    }
}

impl fmt::Debug for ApprovalToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApprovalToken(<redacted>)")
    }
}

impl Drop for ApprovalToken {
    fn drop(&mut self) {
        self.envelope.fill(0);
        compiler_fence(Ordering::SeqCst);
    }
}

/// Token plus non-secret issuance metadata needed by the explicit robot
/// projection. Custom `Debug` preserves the bearer redaction boundary.
pub struct IssuedApprovalToken {
    token: ApprovalToken,
    issued_at_unix_seconds: i64,
    expires_at_unix_seconds: i64,
}

impl IssuedApprovalToken {
    /// Opaque bearer wrapper; render only with its explicit exposure method.
    #[must_use]
    pub fn token(&self) -> &ApprovalToken {
        &self.token
    }

    /// Unix issue timestamp authenticated inside the bearer.
    #[must_use]
    pub const fn issued_at_unix_seconds(&self) -> i64 {
        self.issued_at_unix_seconds
    }

    /// Unix expiry timestamp authenticated inside the bearer.
    #[must_use]
    pub const fn expires_at_unix_seconds(&self) -> i64 {
        self.expires_at_unix_seconds
    }
}

impl fmt::Debug for IssuedApprovalToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedApprovalToken")
            .field("token", &"<redacted>")
            .field("issued_at_unix_seconds", &self.issued_at_unix_seconds)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .finish()
    }
}

/// MAC-authenticated envelope state. All fields stay private so callers cannot
/// compare the snapshot tag or use the nonce as an equality oracle. It has no
/// serialization surface and a redacted `Debug`.
pub struct AuthenticatedApprovalToken {
    purpose: ApprovalPurpose,
    nonce: [u8; NONCE_LEN],
    issued_at_unix_seconds: i64,
    expires_at_unix_seconds: i64,
    snapshot_tag: Mac,
    authenticated_key_id: KeyId,
}

impl fmt::Debug for AuthenticatedApprovalToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthenticatedApprovalToken")
            .field("envelope", &"<authenticated-redacted>")
            .field("issued_at_unix_seconds", &self.issued_at_unix_seconds)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .finish()
    }
}

/// Domain-keyed, non-replayable identifier suitable for the consent audit row.
/// It authenticates only the random token nonce and cannot recover or validate
/// the bearer, snapshot tag, samples, or canonical snapshot.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ApprovalAuditId([u8; MAC_LEN]);

impl ApprovalAuditId {
    /// Stable opaque text for the durable audit record.
    #[must_use]
    pub fn to_opaque_string(&self) -> String {
        let mut output = String::with_capacity(AUDIT_ID_PREFIX.len() + 43);
        output.push_str(AUDIT_ID_PREFIX);
        URL_SAFE_NO_PAD.encode_string(self.0.as_slice(), &mut output);
        output
    }
}

impl fmt::Debug for ApprovalAuditId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ApprovalAuditId")
            .field(&self.to_opaque_string())
            .finish()
    }
}

/// Successful authentication and current-snapshot comparison.
#[derive(Debug, Eq, PartialEq)]
pub struct VerifiedApproval {
    audit_id: ApprovalAuditId,
    issued_at_unix_seconds: i64,
    expires_at_unix_seconds: i64,
}

impl VerifiedApproval {
    /// Opaque identifier to persist in the same transaction as grant + audit.
    #[must_use]
    pub const fn audit_id(&self) -> ApprovalAuditId {
        self.audit_id
    }

    /// Authenticated Unix issue timestamp.
    #[must_use]
    pub const fn issued_at_unix_seconds(&self) -> i64 {
        self.issued_at_unix_seconds
    }

    /// Authenticated Unix expiry timestamp.
    #[must_use]
    pub const fn expires_at_unix_seconds(&self) -> i64 {
        self.expires_at_unix_seconds
    }
}

/// Mint one fresh approval token under the current store key.
///
/// `canonical_snapshot` must be the exact versioned bytes rendered by preview
/// and recomputed by apply. The bytes do not enter the bearer; only a
/// nonce-salted keyed tag does.
pub fn issue(
    root: &StoreAuthRoot,
    purpose: ApprovalPurpose,
    workspace_id: &str,
    surface: &str,
    canonical_snapshot: &[u8],
    now_unix_seconds: i64,
) -> Result<IssuedApprovalToken, ApprovalTokenError> {
    validate_context(workspace_id, surface)?;
    if now_unix_seconds < 0 || canonical_snapshot.len() > MAX_CANONICAL_APPROVAL_SNAPSHOT_BYTES {
        return Err(ApprovalTokenError::Invalid);
    }
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|error| {
        ApprovalTokenError::StoreAuth(StoreAuthError::Randomness {
            message: error.to_string(),
        })
    })?;
    issue_with_nonce(
        root,
        purpose,
        workspace_id,
        surface,
        canonical_snapshot,
        now_unix_seconds,
        nonce,
    )
}

/// Authenticate a bearer against the current store key and invocation context.
/// This does **not** inspect the current preview snapshot or classify expiry;
/// callers do both with [`compare_snapshot`] inside the write transaction.
pub fn verify_authentic(
    root: &StoreAuthRoot,
    purpose: ApprovalPurpose,
    workspace_id: &str,
    surface: &str,
    token_text: &str,
    now_unix_seconds: i64,
) -> Result<AuthenticatedApprovalToken, ApprovalTokenError> {
    let token = ApprovalToken::parse(token_text)?;
    verify_authentic_token(
        root,
        purpose,
        workspace_id,
        surface,
        &token,
        now_unix_seconds,
    )
}

/// Authenticate an already bounded/parsed bearer, avoiding a second explicit
/// string exposure for stdin consumers.
pub fn verify_authentic_token(
    root: &StoreAuthRoot,
    purpose: ApprovalPurpose,
    workspace_id: &str,
    surface: &str,
    token: &ApprovalToken,
    now_unix_seconds: i64,
) -> Result<AuthenticatedApprovalToken, ApprovalTokenError> {
    validate_context(workspace_id, surface)?;
    if now_unix_seconds < 0 {
        return Err(ApprovalTokenError::Invalid);
    }

    let envelope = &token.envelope;
    let candidate_mac = Mac::from_bytes(copy_array::<MAC_LEN>(&envelope[ENVELOPE_MAC_OFFSET..]));
    let mac_message = envelope_mac_message(
        purpose,
        workspace_id,
        surface,
        &envelope[..ENVELOPE_MAC_OFFSET],
    )?;
    if !root.verify(purpose.envelope_mac_domain(), &mac_message, &candidate_mac)? {
        return Err(ApprovalTokenError::Invalid);
    }

    // Temporal-shape validation occurs only after the constant-time MAC check.
    // A future-issued token is invalid, while expiry is intentionally deferred
    // to compare_snapshot so only an authentic envelope can be called stale.
    let issued_at_unix_seconds = read_i64(envelope, ISSUED_AT_OFFSET);
    let expires_at_unix_seconds = read_i64(envelope, EXPIRES_AT_OFFSET);
    let expected_expiry = issued_at_unix_seconds.checked_add(APPROVAL_TOKEN_TTL_SECONDS);
    if issued_at_unix_seconds < 0
        || issued_at_unix_seconds > now_unix_seconds
        || expected_expiry != Some(expires_at_unix_seconds)
    {
        return Err(ApprovalTokenError::Invalid);
    }

    Ok(AuthenticatedApprovalToken {
        purpose,
        nonce: copy_array::<NONCE_LEN>(&envelope[NONCE_OFFSET..ISSUED_AT_OFFSET]),
        issued_at_unix_seconds,
        expires_at_unix_seconds,
        snapshot_tag: Mac::from_bytes(copy_array::<TAG_LEN>(
            &envelope[SNAPSHOT_TAG_OFFSET..ENVELOPE_MAC_OFFSET],
        )),
        authenticated_key_id: root.current_key_id(),
    })
}

/// Compare an authenticated token with the canonical snapshot rebuilt inside
/// the grant transaction. Expiry and every preview-input drift return `stale`;
/// a key rotation between phases remains `invalid`.
pub fn compare_snapshot(
    root: &StoreAuthRoot,
    authenticated: &AuthenticatedApprovalToken,
    canonical_snapshot: &[u8],
    now_unix_seconds: i64,
) -> Result<VerifiedApproval, ApprovalTokenError> {
    if root.current_key_id() != authenticated.authenticated_key_id
        || now_unix_seconds < authenticated.issued_at_unix_seconds
    {
        return Err(ApprovalTokenError::Invalid);
    }
    if now_unix_seconds >= authenticated.expires_at_unix_seconds
        || canonical_snapshot.len() > MAX_CANONICAL_APPROVAL_SNAPSHOT_BYTES
    {
        return Err(ApprovalTokenError::Stale);
    }

    let current_tag = snapshot_tag(
        root,
        authenticated.purpose,
        &authenticated.nonce,
        canonical_snapshot,
    )?;
    if current_tag != authenticated.snapshot_tag {
        return Err(ApprovalTokenError::Stale);
    }
    let audit_mac = root.mac(
        authenticated.purpose.audit_id_domain(),
        &audit_id_message(authenticated.purpose, &authenticated.nonce),
    )?;
    Ok(VerifiedApproval {
        audit_id: ApprovalAuditId(*audit_mac.as_bytes()),
        issued_at_unix_seconds: authenticated.issued_at_unix_seconds,
        expires_at_unix_seconds: authenticated.expires_at_unix_seconds,
    })
}

/// Read one approval bearer from a bounded stdin-like source. The returned
/// wrapper is fixed-width and redacts `Debug`; no raw bearer string survives
/// this function. I/O, UTF-8, size, and shape failures all collapse to
/// `mesh_approval_token_invalid`.
pub fn read_bounded_token(
    reader: &mut impl std::io::Read,
) -> Result<ApprovalToken, ApprovalTokenError> {
    let read_cap =
        u64::try_from(MAX_APPROVAL_TOKEN_INPUT_BYTES).map_err(|_| ApprovalTokenError::Invalid)? + 1;
    let mut bytes = Vec::with_capacity(MAX_APPROVAL_TOKEN_INPUT_BYTES + 1);
    if reader.take(read_cap).read_to_end(&mut bytes).is_err()
        || bytes.len() > MAX_APPROVAL_TOKEN_INPUT_BYTES
    {
        bytes.fill(0);
        compiler_fence(Ordering::SeqCst);
        return Err(ApprovalTokenError::Invalid);
    }
    let result = std::str::from_utf8(&bytes)
        .map_err(|_| ApprovalTokenError::Invalid)
        .and_then(ApprovalToken::parse);
    bytes.fill(0);
    compiler_fence(Ordering::SeqCst);
    result
}

fn issue_with_nonce(
    root: &StoreAuthRoot,
    purpose: ApprovalPurpose,
    workspace_id: &str,
    surface: &str,
    canonical_snapshot: &[u8],
    now_unix_seconds: i64,
    nonce: [u8; NONCE_LEN],
) -> Result<IssuedApprovalToken, ApprovalTokenError> {
    validate_context(workspace_id, surface)?;
    if now_unix_seconds < 0 || canonical_snapshot.len() > MAX_CANONICAL_APPROVAL_SNAPSHOT_BYTES {
        return Err(ApprovalTokenError::Invalid);
    }
    let expires_at_unix_seconds = now_unix_seconds
        .checked_add(APPROVAL_TOKEN_TTL_SECONDS)
        .ok_or(ApprovalTokenError::Invalid)?;
    let tag = snapshot_tag(root, purpose, &nonce, canonical_snapshot)?;

    let mut envelope = [0_u8; ENVELOPE_LEN];
    envelope[VERSION_OFFSET] = TOKEN_VERSION;
    envelope[NONCE_OFFSET..ISSUED_AT_OFFSET].copy_from_slice(&nonce);
    envelope[ISSUED_AT_OFFSET..EXPIRES_AT_OFFSET].copy_from_slice(&now_unix_seconds.to_be_bytes());
    envelope[EXPIRES_AT_OFFSET..SNAPSHOT_TAG_OFFSET]
        .copy_from_slice(&expires_at_unix_seconds.to_be_bytes());
    envelope[SNAPSHOT_TAG_OFFSET..ENVELOPE_MAC_OFFSET].copy_from_slice(tag.as_bytes());
    let mac_message = envelope_mac_message(
        purpose,
        workspace_id,
        surface,
        &envelope[..ENVELOPE_MAC_OFFSET],
    )?;
    let envelope_mac = root.mac(purpose.envelope_mac_domain(), &mac_message)?;
    envelope[ENVELOPE_MAC_OFFSET..].copy_from_slice(envelope_mac.as_bytes());

    Ok(IssuedApprovalToken {
        token: ApprovalToken { envelope },
        issued_at_unix_seconds: now_unix_seconds,
        expires_at_unix_seconds,
    })
}

fn validate_context(workspace_id: &str, surface: &str) -> Result<(), ApprovalTokenError> {
    if workspace_id.is_empty()
        || workspace_id.len() > MAX_WORKSPACE_CONTEXT_BYTES
        || surface.is_empty()
        || surface.len() > MAX_SURFACE_CONTEXT_BYTES
    {
        return Err(ApprovalTokenError::Invalid);
    }
    Ok(())
}

fn snapshot_tag(
    root: &StoreAuthRoot,
    purpose: ApprovalPurpose,
    nonce: &[u8; NONCE_LEN],
    canonical_snapshot: &[u8],
) -> Result<Mac, ApprovalTokenError> {
    if canonical_snapshot.len() > MAX_CANONICAL_APPROVAL_SNAPSHOT_BYTES {
        return Err(ApprovalTokenError::Stale);
    }
    let mut message = Vec::with_capacity(
        purpose.snapshot_message_domain().len() + NONCE_LEN + 8 + canonical_snapshot.len(),
    );
    message.extend_from_slice(purpose.snapshot_message_domain());
    message.extend_from_slice(nonce);
    append_len_prefixed(&mut message, canonical_snapshot)?;
    root.mac(purpose.snapshot_tag_domain(), &message)
        .map_err(Into::into)
}

fn envelope_mac_message(
    purpose: ApprovalPurpose,
    workspace_id: &str,
    surface: &str,
    authenticated_envelope_fields: &[u8],
) -> Result<Vec<u8>, ApprovalTokenError> {
    let mut message = Vec::with_capacity(
        purpose.envelope_message_domain().len()
            + 8
            + workspace_id.len()
            + 8
            + surface.len()
            + authenticated_envelope_fields.len(),
    );
    message.extend_from_slice(purpose.envelope_message_domain());
    append_len_prefixed(&mut message, workspace_id.as_bytes())?;
    append_len_prefixed(&mut message, surface.as_bytes())?;
    message.extend_from_slice(authenticated_envelope_fields);
    Ok(message)
}

fn audit_id_message(purpose: ApprovalPurpose, nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut message = Vec::with_capacity(purpose.audit_message_domain().len() + 1 + NONCE_LEN);
    message.extend_from_slice(purpose.audit_message_domain());
    message.push(TOKEN_VERSION);
    message.extend_from_slice(nonce);
    message
}

fn append_len_prefixed(output: &mut Vec<u8>, value: &[u8]) -> Result<(), ApprovalTokenError> {
    let len = u64::try_from(value.len()).map_err(|_| ApprovalTokenError::Invalid)?;
    output.extend_from_slice(&len.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn read_i64(envelope: &[u8; ENVELOPE_LEN], offset: usize) -> i64 {
    i64::from_be_bytes(copy_array::<8>(&envelope[offset..offset + 8]))
}

fn copy_array<const N: usize>(slice: &[u8]) -> [u8; N] {
    let mut output = [0_u8; N];
    output.copy_from_slice(&slice[..N]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "ws_approval_tests";
    const SURFACE: &str = "ee.mesh.grant.v1";
    const NOW: i64 = 1_800_000_000;

    fn root() -> (tempfile::TempDir, StoreAuthRoot) {
        let directory = tempfile::TempDir::new().expect("tempdir");
        let root = StoreAuthRoot::create(directory.path()).expect("store auth root");
        (directory, root)
    }

    fn issue_deterministic(
        root: &StoreAuthRoot,
        snapshot: &[u8],
        nonce_byte: u8,
    ) -> IssuedApprovalToken {
        issue_with_nonce(
            root,
            ApprovalPurpose::Lane,
            WORKSPACE,
            SURFACE,
            snapshot,
            NOW,
            [nonce_byte; NONCE_LEN],
        )
        .expect("issue")
    }

    #[test]
    fn fixed_envelope_round_trips_without_serialized_context_ids() {
        let (_directory, root) = root();
        let snapshot = br#"{"schema":"ee.mesh.lane_grant_preview.v2","generation":7}"#;
        let issued = issue_deterministic(&root, snapshot, 0x11);
        let bearer = issued.token().expose_bearer();

        assert_eq!(bearer.len(), APPROVAL_TOKEN_BEARER_LEN);
        assert!(bearer.starts_with(APPROVAL_TOKEN_PREFIX));
        assert!(!bearer.contains(WORKSPACE));
        assert!(!bearer.contains(SURFACE));
        assert_eq!(format!("{:?}", issued.token()), "ApprovalToken(<redacted>)");
        assert!(!format!("{issued:?}").contains(&bearer));

        let authenticated = verify_authentic(
            &root,
            ApprovalPurpose::Lane,
            WORKSPACE,
            SURFACE,
            &bearer,
            NOW,
        )
        .expect("authenticate");
        let verified =
            compare_snapshot(&root, &authenticated, snapshot, NOW).expect("compare snapshot");
        assert!(
            verified
                .audit_id()
                .to_opaque_string()
                .starts_with(AUDIT_ID_PREFIX)
        );
    }

    #[test]
    fn body_approval_uses_separate_current_key_domains_without_a_wire_key_id() {
        let (_directory, mut root) = root();
        let snapshot = br#"{"schema":"ee.mesh.lane_grant_preview.v2","lane":"body"}"#;
        let nonce = [0x19; NONCE_LEN];
        let body = issue_with_nonce(
            &root,
            ApprovalPurpose::Body,
            WORKSPACE,
            SURFACE,
            snapshot,
            NOW,
            nonce,
        )
        .expect("issue body approval");
        let lane = issue_with_nonce(
            &root,
            ApprovalPurpose::Lane,
            WORKSPACE,
            SURFACE,
            snapshot,
            NOW,
            nonce,
        )
        .expect("issue ordinary lane approval");
        let body_bearer = body.token().expose_bearer();
        let lane_bearer = lane.token().expose_bearer();

        assert_ne!(
            body_bearer, lane_bearer,
            "body snapshot and envelope domains must differ"
        );
        assert!(!body_bearer.contains(&root.current_key_id().to_hex()));
        assert_eq!(body_bearer.len(), APPROVAL_TOKEN_BEARER_LEN);
        assert!(matches!(
            verify_authentic(
                &root,
                ApprovalPurpose::Lane,
                WORKSPACE,
                SURFACE,
                &body_bearer,
                NOW,
            ),
            Err(ApprovalTokenError::Invalid)
        ));

        let body_authenticated = verify_authentic(
            &root,
            ApprovalPurpose::Body,
            WORKSPACE,
            SURFACE,
            &body_bearer,
            NOW,
        )
        .expect("authenticate body approval");
        let lane_authenticated = verify_authentic(
            &root,
            ApprovalPurpose::Lane,
            WORKSPACE,
            SURFACE,
            &lane_bearer,
            NOW,
        )
        .expect("authenticate ordinary lane approval");
        let body_audit = compare_snapshot(&root, &body_authenticated, snapshot, NOW)
            .expect("verify body snapshot")
            .audit_id();
        let lane_audit = compare_snapshot(&root, &lane_authenticated, snapshot, NOW)
            .expect("verify lane snapshot")
            .audit_id();
        assert_ne!(
            body_audit, lane_audit,
            "body audit IDs need a separate subkey"
        );

        root.rotate().expect("rotate current key");
        assert!(matches!(
            verify_authentic(
                &root,
                ApprovalPurpose::Body,
                WORKSPACE,
                SURFACE,
                &body_bearer,
                NOW,
            ),
            Err(ApprovalTokenError::Invalid)
        ));
    }

    #[test]
    fn config_digest_is_exact_byte_bound_and_canonical() {
        let first = approval_config_digest(b"[mesh]\nenabled = true\n");
        let equal = approval_config_digest(b"[mesh]\nenabled = true\n");
        let formatting_drift = approval_config_digest(b"[mesh]\nenabled=true\n");

        assert_eq!(first, equal);
        assert_ne!(first, formatting_drift);
        assert!(crate::db::is_canonical_blake3_hash(&first));
    }

    #[test]
    fn every_envelope_byte_tamper_and_wrong_context_are_invalid_before_snapshot_comparison() {
        let (_directory, root) = root();
        let issued = issue_deterministic(&root, b"canonical-preview", 0x22);
        for byte_index in 0..ENVELOPE_LEN {
            let mut envelope = issued.token.envelope;
            envelope[byte_index] ^= 0x01;
            let tampered = ApprovalToken { envelope }.expose_bearer();
            assert!(
                matches!(
                    verify_authentic(
                        &root,
                        ApprovalPurpose::Lane,
                        WORKSPACE,
                        SURFACE,
                        &tampered,
                        NOW,
                    ),
                    Err(ApprovalTokenError::Invalid)
                ),
                "tampering envelope byte {byte_index} must be invalid",
            );
        }
        let authentic_bearer = issued.token().expose_bearer();
        assert!(matches!(
            verify_authentic(
                &root,
                ApprovalPurpose::Lane,
                "ws_other",
                SURFACE,
                &authentic_bearer,
                NOW,
            ),
            Err(ApprovalTokenError::Invalid)
        ));
        assert!(matches!(
            verify_authentic(
                &root,
                ApprovalPurpose::Lane,
                WORKSPACE,
                "ee.team.share_bodies.v1",
                &authentic_bearer,
                NOW
            ),
            Err(ApprovalTokenError::Invalid)
        ));
    }

    #[test]
    fn equal_previews_have_unlinkable_nonce_salted_bearers() {
        let (_directory, root) = root();
        let first = issue_deterministic(&root, b"same canonical snapshot", 0x31);
        let second = issue_deterministic(&root, b"same canonical snapshot", 0x32);

        assert_ne!(
            first.token().expose_bearer(),
            second.token().expose_bearer(),
            "fresh nonces must unlink equal previews"
        );
    }

    #[test]
    fn only_authentic_expiry_or_snapshot_drift_is_stale() {
        let (_directory, root) = root();
        let issued = issue_deterministic(&root, b"preview-a", 0x43);
        let bearer = issued.token().expose_bearer();
        let authenticated = verify_authentic(
            &root,
            ApprovalPurpose::Lane,
            WORKSPACE,
            SURFACE,
            &bearer,
            NOW,
        )
        .expect("authenticate");

        assert!(matches!(
            compare_snapshot(&root, &authenticated, b"preview-b", NOW),
            Err(ApprovalTokenError::Stale)
        ));
        assert!(matches!(
            compare_snapshot(
                &root,
                &authenticated,
                b"preview-a",
                NOW + APPROVAL_TOKEN_TTL_SECONDS
            ),
            Err(ApprovalTokenError::Stale)
        ));
    }

    #[test]
    fn future_issued_tokens_are_invalid_only_after_a_valid_mac() {
        let (_directory, root) = root();
        let issued = issue_deterministic(&root, b"preview", 0x54);
        let bearer = issued.token().expose_bearer();

        assert!(matches!(
            verify_authentic(
                &root,
                ApprovalPurpose::Lane,
                WORKSPACE,
                SURFACE,
                &bearer,
                NOW - 1,
            ),
            Err(ApprovalTokenError::Invalid)
        ));
    }

    #[test]
    fn current_key_rotation_invalidates_outstanding_tokens() {
        let (_directory, mut root) = root();
        let issued = issue_deterministic(&root, b"preview", 0x65);
        let bearer = issued.token().expose_bearer();
        root.rotate().expect("rotate current key");

        assert!(matches!(
            verify_authentic(
                &root,
                ApprovalPurpose::Lane,
                WORKSPACE,
                SURFACE,
                &bearer,
                NOW,
            ),
            Err(ApprovalTokenError::Invalid)
        ));
    }

    #[test]
    fn a_foreign_store_cannot_authenticate_the_envelope() {
        let (_directory_a, root_a) = root();
        let (_directory_b, root_b) = root();
        let issued = issue_deterministic(&root_a, b"preview", 0x66);
        let bearer = issued.token().expose_bearer();

        assert!(matches!(
            verify_authentic(
                &root_b,
                ApprovalPurpose::Lane,
                WORKSPACE,
                SURFACE,
                &bearer,
                NOW,
            ),
            Err(ApprovalTokenError::Invalid)
        ));
    }

    #[test]
    fn opaque_audit_id_is_stable_for_one_nonce_and_not_the_bearer() {
        let (_directory, root) = root();
        let issued = issue_deterministic(&root, b"preview", 0x76);
        let bearer = issued.token().expose_bearer();
        let authenticated = verify_authentic(
            &root,
            ApprovalPurpose::Lane,
            WORKSPACE,
            SURFACE,
            &bearer,
            NOW,
        )
        .expect("authenticate");
        let first = compare_snapshot(&root, &authenticated, b"preview", NOW)
            .expect("compare")
            .audit_id()
            .to_opaque_string();
        let second = compare_snapshot(&root, &authenticated, b"preview", NOW)
            .expect("compare")
            .audit_id()
            .to_opaque_string();

        assert_eq!(first, second);
        assert!(!bearer.contains(&first));
        assert!(!first.contains(&bearer));
    }

    #[test]
    fn bounded_stdin_reader_accepts_one_newline_and_rejects_oversize() {
        let (_directory, root) = root();
        let issued = issue_deterministic(&root, b"preview", 0x87);
        let input = format!("{}\n", issued.token().expose_bearer());
        let parsed = read_bounded_token(&mut input.as_bytes()).expect("bounded token");
        verify_authentic_token(
            &root,
            ApprovalPurpose::Lane,
            WORKSPACE,
            SURFACE,
            &parsed,
            NOW,
        )
        .expect("authenticate");

        let oversized = "x".repeat(MAX_APPROVAL_TOKEN_INPUT_BYTES + 1);
        assert!(matches!(
            read_bounded_token(&mut oversized.as_bytes()),
            Err(ApprovalTokenError::Invalid)
        ));
    }

    #[test]
    fn malformed_lengths_padding_and_short_tokens_are_invalid() {
        let padded = format!(
            "{}{}=",
            APPROVAL_TOKEN_PREFIX,
            "A".repeat(ENCODED_ENVELOPE_LEN)
        );
        for value in ["", "eeap1_short", " eeap1_short ", &padded] {
            assert!(matches!(
                ApprovalToken::parse(value),
                Err(ApprovalTokenError::Invalid)
            ));
        }
    }

    #[test]
    fn error_surface_never_contains_bearer_material() {
        let invalid = ApprovalTokenError::Invalid;
        let stale = ApprovalTokenError::Stale;
        assert_eq!(invalid.code(), MESH_APPROVAL_TOKEN_INVALID_CODE);
        assert_eq!(stale.code(), MESH_APPROVAL_TOKEN_STALE_CODE);
        assert!(!invalid.message().contains(APPROVAL_TOKEN_PREFIX));
        assert!(!stale.message().contains(APPROVAL_TOKEN_PREFIX));
        assert!(!invalid.repair().contains(APPROVAL_TOKEN_PREFIX));
    }
}
