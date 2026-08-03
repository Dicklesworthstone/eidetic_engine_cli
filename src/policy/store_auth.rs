//! Store-local authentication root (ADR 0086 TC-D14, plan P0.6, bead
//! `bd-tc-epic-qzk7o.2.4` slice 1).
//!
//! One hardened per-store authentication root anchors every store-local MAC in
//! the confederation design: native-import authentication (slice 3), and the
//! lane / body exposure-approval envelopes (T1.4 / T5.9). The root is a single
//! 32-byte OS-CSPRNG secret from which purpose-specific BLAKE3 subkeys are
//! derived under fixed, non-overlapping domain strings. Cross-domain reuse is
//! impossible by construction: each [`MacDomain`] derives its own subkey via
//! `blake3::derive_key`, and MACs are `blake3::keyed_hash` under that subkey.
//!
//! Security contract (TC-D14): the raw root and any derived subkey never enter
//! the database, command output, logs, audit, support bundles, or the redacted
//! `ee backup` format. The only durable home for the root is the hardened key
//! file this module owns (`0700` directory / `0600` file, owner-only, no
//! symlinked components). The `Secret` newtype has a redacted `Debug` and best-effort
//! `Drop` zeroization, and no secret bytes are ever serialized except as the
//! raw-root hex inside that one key file.
//!
//! Availability failures (missing/corrupt/insecure key store, randomness
//! failure, or a primitive self-test regression) all fail closed with
//! [`StoreAuthError`], whose [`StoreAuthError::degraded_code`] is
//! [`MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE`]. Callers surface that as a
//! `high`-severity degraded entry and admit nothing on the strength of native
//! trust.
//!
//! This slice establishes the key lifecycle, hardened storage, known-answer
//! self-check, rotation window, and the fallible derivation API. Export header
//! MACs (slice 2) and import verification plus the `human_explicit` bypass
//! closure (slice 3) consume this module without re-implementing any of it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{Ordering, compiler_fence};

use serde::{Deserialize, Serialize};

use super::hex_lower;

/// Length of the root secret and every derived subkey, in bytes.
const KEY_LEN: usize = 32;
/// Length of a MAC / tag output, in bytes.
const MAC_LEN: usize = 32;
/// Length of an opaque key identifier, in bytes.
const KEY_ID_LEN: usize = 16;
/// File name of the hardened key store inside the injected keys directory.
const KEY_FILE_NAME: &str = "store_auth_root.json";
/// Temp sibling used for atomic replace during rotation.
const KEY_FILE_TMP_NAME: &str = "store_auth_root.json.tmp";
/// On-disk key-file schema tag.
const KEY_FILE_SCHEMA: &str = "ee.store_auth.keyfile.v1";
/// Maximum retired keys retained for the same-store verification window.
const MAX_RETIRED_KEYS: usize = 4;
/// Hard cap on the key-file size we will read (a valid file is well under 1 KiB).
const MAX_KEY_FILE_BYTES: u64 = 64 * 1024;

/// Degraded code emitted whenever the store-local authentication root cannot be
/// established or verified. Fail-closed: nothing is admitted at native trust.
pub const MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE: &str =
    "mesh_store_authentication_unavailable";

/// Canonical on-disk location of a workspace's store-authentication key
/// directory. Exporters and importers must open the same root, so every
/// caller resolves the directory through this helper.
#[must_use]
pub fn workspace_keys_dir(workspace_path: &Path) -> PathBuf {
    workspace_path
        .join(crate::config::WORKSPACE_MARKER)
        .join("keys")
}

/// Internal derivation context for the key-file integrity self-check. This is
/// deliberately *not* a public [`MacDomain`]: it authenticates the key file's
/// own consistency, not any consumer payload.
const SELF_CHECK_CONTEXT: &str = "ee.store_auth.self_check.v1";
/// Fixed message MAC'd under the self-check subkey. Its keyed digest is stored
/// in the key file and re-verified on open to detect corruption/truncation.
const SELF_CHECK_MESSAGE: &[u8] = b"ee.store_auth.self_check.message.v1";

// Known-answer vectors captured from the BLAKE3 reference implementation
// (`b3sum`) over the fixed root `0x00..0x1f`. They pin both the primitive
// wiring (`derive_key` + `keyed_hash`) and the exact domain strings so an
// accidental edit to a context string or a swapped crypto backend fails the
// self-test before any key material is generated or trusted.
const KAT_MESSAGE: &[u8] = b"ee-store-auth-kat-message";
const KAT_SUBKEY_HEX: &str = "cb573690cdf5ecbcfbc91c2dc82459a8d8161e673e52abd8e2be14dba253037f";
const KAT_MAC_HEX: &str = "d95066c3c600bbb4fb8f307bcfb553a56862e442155d2f5e3d80497ead8bd0c0";
const KAT_SELF_CHECK_HEX: &str = "2dc8db78eb25d723bae6ec5280656f8fe9070a417e476c859d194ef6f22d6f3e";

/// Fail-closed error surface for the store-local authentication root. Every
/// variant maps to [`MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE`]: the store is
/// unavailable, so native trust is refused rather than degraded silently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreAuthError {
    /// The OS CSPRNG failed to supply key material.
    Randomness { message: String },
    /// A filesystem operation on the key store failed.
    Io { path: String, message: String },
    /// The key file or its directory is accessible beyond the owner.
    InsecurePermissions { path: String, detail: String },
    /// A component of the key path is a symbolic link.
    SymlinkComponent { path: String },
    /// The key file could not be parsed or violates a structural invariant.
    Malformed { message: String },
    /// The key file schema tag did not match the supported version.
    SchemaMismatch { found: String, expected: String },
    /// The key file's stored integrity MAC did not match the current root.
    SelfCheckFailed,
    /// The BLAKE3 primitive self-test disagreed with the pinned reference
    /// vectors — the crypto backend is wrong or the domain strings drifted.
    PrimitiveKnownAnswerFailed { detail: String },
    /// `create` was asked to initialize a store that already exists.
    AlreadyInitialized { path: String },
    /// `open` was asked to load a store that has not been initialized.
    NotInitialized { path: String },
}

impl StoreAuthError {
    /// The single degraded code every store-auth failure surfaces.
    #[must_use]
    pub fn degraded_code(&self) -> &'static str {
        MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE
    }

    /// Human-readable, secret-free description for the degraded `message` field.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Randomness { message } => {
                format!("Store-authentication randomness failed: {message}")
            }
            Self::Io { path, message } => {
                format!("Store-authentication key store I/O failed at {path}: {message}")
            }
            Self::InsecurePermissions { path, detail } => {
                format!("Store-authentication key store at {path} is not owner-only: {detail}")
            }
            Self::SymlinkComponent { path } => {
                format!("Store-authentication key path {path} traverses a symbolic link")
            }
            Self::Malformed { message } => {
                format!("Store-authentication key store is malformed: {message}")
            }
            Self::SchemaMismatch { found, expected } => {
                format!(
                    "Store-authentication key store schema {found} is not the supported {expected}"
                )
            }
            Self::SelfCheckFailed => {
                "Store-authentication key store failed its integrity self-check".to_owned()
            }
            Self::PrimitiveKnownAnswerFailed { detail } => {
                format!("Store-authentication primitive self-test failed: {detail}")
            }
            Self::AlreadyInitialized { path } => {
                format!("Store-authentication key store already exists at {path}")
            }
            Self::NotInitialized { path } => {
                format!("Store-authentication key store is not initialized at {path}")
            }
        }
    }

    /// Actionable, secret-free repair hint for the degraded `repair` field.
    #[must_use]
    pub fn repair(&self) -> String {
        match self {
            Self::InsecurePermissions { .. } => {
                "Restrict the key directory to 0700 and the key file to 0600 (owner-only), \
                 then re-run."
                    .to_owned()
            }
            Self::SymlinkComponent { .. } => {
                "Replace the symlinked key path with a real owner-only directory and re-run."
                    .to_owned()
            }
            Self::SelfCheckFailed | Self::Malformed { .. } | Self::SchemaMismatch { .. } => {
                "The key store is unusable. Restore the protected key directory from a secure \
                 backup, or re-initialize the store (imported native-trust rows must be \
                 re-attested)."
                    .to_owned()
            }
            Self::NotInitialized { .. } => {
                "Initialize the store-authentication root before importing at native trust."
                    .to_owned()
            }
            _ => "Resolve the underlying key-store fault and re-run; nothing was admitted."
                .to_owned(),
        }
    }
}

impl fmt::Display for StoreAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for StoreAuthError {}

/// Opaque, non-secret key identifier. Random at creation, it names which key
/// authenticated an artifact without revealing anything about the root. Safe to
/// carry in import headers and to compare with `==`.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct KeyId([u8; KEY_ID_LEN]);

impl KeyId {
    /// Lowercase hex rendering (32 characters).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex_lower(&self.0)
    }

    /// Parse a 32-character lowercase-or-uppercase hex identifier.
    pub fn from_hex(value: &str) -> Result<Self, StoreAuthError> {
        Ok(Self(decode_hex_fixed::<KEY_ID_LEN>(value, "key id")?))
    }

    /// Raw identifier bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; KEY_ID_LEN] {
        &self.0
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({})", self.to_hex())
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A domain-separated authentication tag. This is a *public* authenticator (it
/// travels in import headers and approval envelopes), not secret key material,
/// so it is hex-serializable. Equality is constant-time to deny timing oracles
/// on verification.
#[derive(Clone, Copy)]
pub struct Mac([u8; MAC_LEN]);

impl Mac {
    /// Lowercase hex rendering (64 characters).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex_lower(&self.0)
    }

    /// Parse a 64-character hex tag.
    pub fn from_hex(value: &str) -> Result<Self, StoreAuthError> {
        Ok(Self(decode_hex_fixed::<MAC_LEN>(value, "mac")?))
    }

    /// Raw tag bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; MAC_LEN] {
        &self.0
    }
}

impl PartialEq for Mac {
    /// Constant-time comparison over the fixed-width tag.
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(&self.0, &other.0)
    }
}

impl Eq for Mac {}

impl fmt::Debug for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mac({})", self.to_hex())
    }
}

/// Fixed subkey-derivation domains. Each derives a distinct BLAKE3 subkey from
/// the same root, so a tag minted for one purpose can never authenticate bytes
/// for another. The lane/body approval domains are reserved here so T1.4 and
/// T5.9 consume their own subkeys rather than resharing the import key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MacDomain {
    /// `ee export` → `ee import jsonl` native-trust header MAC (slice 3).
    NativeImportRecordsRoot,
    /// `ee playbook import` header MAC — distinct domain and record tag.
    PlaybookImportRecordsRoot,
    /// T1.4 lane-approval canonical snapshot tag.
    LaneApprovalSnapshotTag,
    /// T1.4 lane-approval envelope MAC.
    LaneApprovalEnvelopeMac,
    /// T1.4 lane-approval durable audit identifier.
    LaneApprovalAuditId,
    /// T5.9 body-approval canonical snapshot tag.
    BodyApprovalSnapshotTag,
    /// T5.9 body-approval envelope MAC.
    BodyApprovalEnvelopeMac,
    /// T5.9 body-approval durable audit identifier.
    BodyApprovalAuditId,
}

impl MacDomain {
    /// The fixed, non-overlapping derivation context for this domain.
    #[must_use]
    pub const fn context(self) -> &'static str {
        match self {
            Self::NativeImportRecordsRoot => "ee.store_auth.native_import.records_root.v1",
            Self::PlaybookImportRecordsRoot => "ee.store_auth.playbook_import.records_root.v1",
            Self::LaneApprovalSnapshotTag => "ee.store_auth.lane_approval.snapshot_tag.v1",
            Self::LaneApprovalEnvelopeMac => "ee.store_auth.lane_approval.envelope_mac.v1",
            Self::LaneApprovalAuditId => "ee.store_auth.lane_approval.audit_id.v1",
            Self::BodyApprovalSnapshotTag => "ee.store_auth.body_approval.snapshot_tag.v1",
            Self::BodyApprovalEnvelopeMac => "ee.store_auth.body_approval.envelope_mac.v1",
            Self::BodyApprovalAuditId => "ee.store_auth.body_approval.audit_id.v1",
        }
    }
}

/// Which key in the verification window authenticated an artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyClass {
    /// The store's current key.
    Current,
    /// A retired key still inside the bounded same-store window.
    Retired,
}

/// Outcome of verifying a candidate MAC against a specific key identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyVerification {
    /// The candidate matched the named key.
    Match { key_class: KeyClass },
    /// The named key exists in the window but the candidate did not match.
    Mismatch,
    /// The named key is not the current key and not inside the retired window.
    KeyOutsideWindow,
}

/// A 32-byte secret with a redacted `Debug` and best-effort `Drop` zeroization.
///
/// Zeroization is best-effort only: `#![forbid(unsafe_code)]` rules out a
/// volatile write, so the `Drop` clears the bytes and inserts a compiler fence.
/// This defeats accidental reuse and casual inspection but is not a hardware
/// guarantee against a copying optimizer; a full `zeroize`-backed guarantee is
/// deferred to avoid adding a dependency in this slice.
struct Secret([u8; KEY_LEN]);

impl Secret {
    fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            *byte = 0;
        }
        compiler_fence(Ordering::SeqCst);
    }
}

/// One (key id, root) pair. Derivation and MAC construction happen here so the
/// raw root never escapes; derived subkeys live only for the duration of a MAC
/// and are zeroized on drop.
#[derive(Debug)]
struct KeyEntry {
    key_id: KeyId,
    root: Secret,
}

impl KeyEntry {
    fn derive(&self, context: &str) -> Secret {
        Secret(blake3::derive_key(context, self.root.as_bytes()))
    }

    fn mac(&self, domain: MacDomain, message: &[u8]) -> Mac {
        let subkey = self.derive(domain.context());
        let tag = blake3::keyed_hash(subkey.as_bytes(), message);
        Mac(*tag.as_bytes())
    }

    fn self_check(&self) -> Mac {
        let subkey = self.derive(SELF_CHECK_CONTEXT);
        let tag = blake3::keyed_hash(subkey.as_bytes(), SELF_CHECK_MESSAGE);
        Mac(*tag.as_bytes())
    }

    fn to_file_entry(&self) -> KeyFileEntry {
        KeyFileEntry {
            key_id: self.key_id.to_hex(),
            root: hex_lower(self.root.as_bytes()),
        }
    }
}

/// The loaded store-local authentication root: a current key plus a bounded
/// window of retired keys usable only for verifying same-store artifacts.
pub struct StoreAuthRoot {
    keys_dir: PathBuf,
    current: KeyEntry,
    retired: Vec<KeyEntry>,
}

impl fmt::Debug for StoreAuthRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreAuthRoot")
            .field("keys_dir", &self.keys_dir)
            .field("current_key_id", &self.current.key_id)
            .field("retired_keys", &self.retired.len())
            .finish()
    }
}

impl StoreAuthRoot {
    /// Open the store if it exists, otherwise create it. The common wiring path.
    pub fn open_or_create(keys_dir: impl AsRef<Path>) -> Result<Self, StoreAuthError> {
        let keys_dir = keys_dir.as_ref();
        if keys_dir.join(KEY_FILE_NAME).exists() {
            Self::open(keys_dir)
        } else {
            match Self::create(keys_dir) {
                // A concurrent creator won the race between our existence check
                // and the exclusive create; adopt their root instead of ours.
                Err(StoreAuthError::AlreadyInitialized { .. }) => Self::open(keys_dir),
                other => other,
            }
        }
    }

    /// Create a fresh root, failing if one already exists. The key file is
    /// claimed exclusively (`O_EXCL`) so two racing processes cannot mint
    /// divergent roots.
    pub fn create(keys_dir: impl AsRef<Path>) -> Result<Self, StoreAuthError> {
        primitive_known_answer_check()?;
        let keys_dir = keys_dir.as_ref();
        let path = keys_dir.join(KEY_FILE_NAME);
        reject_symlink_components(keys_dir, &path)?;
        ensure_hardened_dir(keys_dir)?;

        let current = KeyEntry {
            key_id: KeyId(random_bytes::<KEY_ID_LEN>()?),
            root: Secret(random_bytes::<KEY_LEN>()?),
        };
        let root = Self {
            keys_dir: keys_dir.to_path_buf(),
            current,
            retired: Vec::new(),
        };
        let serialized = root.serialize()?;
        write_exclusive(&path, &serialized)?;
        Ok(root)
    }

    /// Load and verify an existing root: hardened permissions, no symlinked
    /// components, schema match, bounded window, and an integrity self-check.
    pub fn open(keys_dir: impl AsRef<Path>) -> Result<Self, StoreAuthError> {
        primitive_known_answer_check()?;
        let keys_dir = keys_dir.as_ref();
        let path = keys_dir.join(KEY_FILE_NAME);
        reject_symlink_components(keys_dir, &path)?;
        if !path.exists() {
            return Err(StoreAuthError::NotInitialized {
                path: path.display().to_string(),
            });
        }
        enforce_owner_only_dir(keys_dir)?;
        enforce_owner_only_file(&path)?;

        let bytes = read_key_file(&path)?;
        let doc: KeyFileDoc =
            serde_json::from_slice(&bytes).map_err(|error| StoreAuthError::Malformed {
                message: format!("key file JSON: {error}"),
            })?;
        if doc.schema != KEY_FILE_SCHEMA {
            return Err(StoreAuthError::SchemaMismatch {
                found: doc.schema,
                expected: KEY_FILE_SCHEMA.to_owned(),
            });
        }
        if doc.retired.len() > MAX_RETIRED_KEYS {
            return Err(StoreAuthError::Malformed {
                message: format!(
                    "retired window has {} keys, exceeds max {MAX_RETIRED_KEYS}",
                    doc.retired.len()
                ),
            });
        }

        let current = doc.current.into_entry()?;
        let retired = doc
            .retired
            .into_iter()
            .map(KeyFileEntry::into_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let root = Self {
            keys_dir: keys_dir.to_path_buf(),
            current,
            retired,
        };

        let expected = Mac::from_hex(&doc.self_check)?;
        if root.current.self_check() != expected {
            return Err(StoreAuthError::SelfCheckFailed);
        }
        Ok(root)
    }

    /// Rotate to a fresh root. The prior current key moves into the bounded
    /// retired window (oldest evicted past `MAX_RETIRED_KEYS`); the key file
    /// is atomically replaced. Returns the new current key id.
    pub fn rotate(&mut self) -> Result<KeyId, StoreAuthError> {
        let new_entry = KeyEntry {
            key_id: KeyId(random_bytes::<KEY_ID_LEN>()?),
            root: Secret(random_bytes::<KEY_LEN>()?),
        };
        let previous = std::mem::replace(&mut self.current, new_entry);
        self.retired.insert(0, previous);
        self.retired.truncate(MAX_RETIRED_KEYS);

        let path = self.keys_dir.join(KEY_FILE_NAME);
        let tmp = self.keys_dir.join(KEY_FILE_TMP_NAME);
        let serialized = self.serialize()?;
        write_replace(&tmp, &path, &serialized)?;
        Ok(self.current.key_id)
    }

    /// The current key identifier.
    #[must_use]
    pub fn current_key_id(&self) -> KeyId {
        self.current.key_id
    }

    /// All key identifiers accepted for same-store verification (current first,
    /// then retired, most-recent-first).
    #[must_use]
    pub fn window_key_ids(&self) -> Vec<KeyId> {
        let mut ids = Vec::with_capacity(1 + self.retired.len());
        ids.push(self.current.key_id);
        ids.extend(self.retired.iter().map(|entry| entry.key_id));
        ids
    }

    /// Compute a domain-separated MAC under the current key.
    ///
    /// Fallible by contract (TC-D14 / P0.6): the in-memory backend cannot fail
    /// today, but the signature reserves fallibility for a future keychain- or
    /// HSM-backed root without churning every call site.
    pub fn mac(&self, domain: MacDomain, message: &[u8]) -> Result<Mac, StoreAuthError> {
        Ok(self.current.mac(domain, message))
    }

    /// Constant-time verify a candidate MAC against the current key.
    pub fn verify(
        &self,
        domain: MacDomain,
        message: &[u8],
        candidate: &Mac,
    ) -> Result<bool, StoreAuthError> {
        Ok(self.current.mac(domain, message) == *candidate)
    }

    /// Verify a candidate MAC against the key named by `key_id`, honoring the
    /// bounded retired window. Approval consumers must additionally require
    /// [`KeyClass::Current`]; import may accept [`KeyClass::Retired`].
    pub fn verify_with_key(
        &self,
        key_id: KeyId,
        domain: MacDomain,
        message: &[u8],
        candidate: &Mac,
    ) -> Result<KeyVerification, StoreAuthError> {
        let (entry, key_class) = if self.current.key_id == key_id {
            (&self.current, KeyClass::Current)
        } else if let Some(entry) = self.retired.iter().find(|entry| entry.key_id == key_id) {
            (entry, KeyClass::Retired)
        } else {
            return Ok(KeyVerification::KeyOutsideWindow);
        };
        if entry.mac(domain, message) == *candidate {
            Ok(KeyVerification::Match { key_class })
        } else {
            Ok(KeyVerification::Mismatch)
        }
    }

    fn serialize(&self) -> Result<Vec<u8>, StoreAuthError> {
        let doc = KeyFileDoc {
            schema: KEY_FILE_SCHEMA.to_owned(),
            current: self.current.to_file_entry(),
            retired: self.retired.iter().map(KeyEntry::to_file_entry).collect(),
            self_check: self.current.self_check().to_hex(),
        };
        serde_json::to_vec_pretty(&doc).map_err(|error| StoreAuthError::Io {
            path: self.keys_dir.join(KEY_FILE_NAME).display().to_string(),
            message: format!("serialize key file: {error}"),
        })
    }
}

/// On-disk key-file document. The `root` fields carry the raw root as hex; this
/// file is the sole legitimate home for that material.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyFileDoc {
    schema: String,
    current: KeyFileEntry,
    #[serde(default)]
    retired: Vec<KeyFileEntry>,
    self_check: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyFileEntry {
    key_id: String,
    root: String,
}

impl KeyFileEntry {
    fn into_entry(self) -> Result<KeyEntry, StoreAuthError> {
        Ok(KeyEntry {
            key_id: KeyId::from_hex(&self.key_id)?,
            root: Secret(decode_hex_fixed::<KEY_LEN>(&self.root, "root")?),
        })
    }
}

/// Verify the BLAKE3 wiring and domain strings against pinned reference vectors
/// before any key material is generated or trusted.
fn primitive_known_answer_check() -> Result<(), StoreAuthError> {
    let root: [u8; KEY_LEN] = std::array::from_fn(|index| index as u8);

    let subkey = blake3::derive_key(MacDomain::NativeImportRecordsRoot.context(), &root);
    if hex_lower(&subkey) != KAT_SUBKEY_HEX {
        return Err(StoreAuthError::PrimitiveKnownAnswerFailed {
            detail: "derive_key subkey mismatch".to_owned(),
        });
    }
    let mac = blake3::keyed_hash(&subkey, KAT_MESSAGE);
    if hex_lower(mac.as_bytes()) != KAT_MAC_HEX {
        return Err(StoreAuthError::PrimitiveKnownAnswerFailed {
            detail: "keyed_hash MAC mismatch".to_owned(),
        });
    }
    let self_check_subkey = blake3::derive_key(SELF_CHECK_CONTEXT, &root);
    let self_check = blake3::keyed_hash(&self_check_subkey, SELF_CHECK_MESSAGE);
    if hex_lower(self_check.as_bytes()) != KAT_SELF_CHECK_HEX {
        return Err(StoreAuthError::PrimitiveKnownAnswerFailed {
            detail: "self-check construction mismatch".to_owned(),
        });
    }
    Ok(())
}

fn random_bytes<const N: usize>() -> Result<[u8; N], StoreAuthError> {
    let mut buffer = [0_u8; N];
    getrandom::fill(&mut buffer).map_err(|error| StoreAuthError::Randomness {
        message: error.to_string(),
    })?;
    Ok(buffer)
}

/// Constant-time equality over equal-length byte slices.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn decode_hex_fixed<const N: usize>(value: &str, label: &str) -> Result<[u8; N], StoreAuthError> {
    let trimmed = value.trim();
    if trimmed.len() != N * 2 {
        return Err(StoreAuthError::Malformed {
            message: format!(
                "{label} must be {} hex chars, found {}",
                N * 2,
                trimmed.len()
            ),
        });
    }
    let bytes = trimmed.as_bytes();
    let mut out = [0_u8; N];
    let mut index = 0;
    while index < N {
        let high = hex_nibble(bytes[index * 2], label)?;
        let low = hex_nibble(bytes[index * 2 + 1], label)?;
        out[index] = (high << 4) | low;
        index += 1;
    }
    Ok(out)
}

fn hex_nibble(byte: u8, label: &str) -> Result<u8, StoreAuthError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(StoreAuthError::Malformed {
            message: format!("{label} contains a non-hex character"),
        }),
    }
}

fn read_key_file(path: &Path) -> Result<Vec<u8>, StoreAuthError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| StoreAuthError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    if metadata.len() > MAX_KEY_FILE_BYTES {
        return Err(StoreAuthError::Malformed {
            message: format!("key file is {} bytes, exceeds cap", metadata.len()),
        });
    }
    std::fs::read(path).map_err(|error| StoreAuthError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

fn reject_symlink_components(keys_dir: &Path, path: &Path) -> Result<(), StoreAuthError> {
    for candidate in [keys_dir, path] {
        if let Ok(metadata) = std::fs::symlink_metadata(candidate)
            && metadata.file_type().is_symlink()
        {
            return Err(StoreAuthError::SymlinkComponent {
                path: candidate.display().to_string(),
            });
        }
    }
    Ok(())
}

fn ensure_hardened_dir(keys_dir: &Path) -> Result<(), StoreAuthError> {
    std::fs::create_dir_all(keys_dir).map_err(|error| StoreAuthError::Io {
        path: keys_dir.display().to_string(),
        message: error.to_string(),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(keys_dir, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| StoreAuthError::Io {
                path: keys_dir.display().to_string(),
                message: format!("harden directory permissions: {error}"),
            },
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn enforce_owner_only_dir(keys_dir: &Path) -> Result<(), StoreAuthError> {
    enforce_owner_only_mode(keys_dir, "key directory")
}

#[cfg(unix)]
fn enforce_owner_only_file(path: &Path) -> Result<(), StoreAuthError> {
    enforce_owner_only_mode(path, "key file")
}

#[cfg(unix)]
fn enforce_owner_only_mode(path: &Path, label: &str) -> Result<(), StoreAuthError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| StoreAuthError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(StoreAuthError::InsecurePermissions {
            path: path.display().to_string(),
            detail: format!("{label} mode {mode:04o} grants group/other access"),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_owner_only_dir(_keys_dir: &Path) -> Result<(), StoreAuthError> {
    // Non-Unix permission hardening relies on the default per-user profile ACL;
    // an explicit ACL tightening pass is a documented residual for this slice.
    Ok(())
}

#[cfg(not(unix))]
fn enforce_owner_only_file(_path: &Path) -> Result<(), StoreAuthError> {
    Ok(())
}

/// Exclusively create the key file (`O_EXCL`), owner-only on Unix.
fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), StoreAuthError> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            StoreAuthError::AlreadyInitialized {
                path: path.display().to_string(),
            }
        } else {
            StoreAuthError::Io {
                path: path.display().to_string(),
                message: error.to_string(),
            }
        }
    })?;
    file.write_all(bytes).map_err(|error| StoreAuthError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    file.sync_all().map_err(|error| StoreAuthError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    Ok(())
}

/// Atomically replace an existing key file via a hardened temp sibling + rename.
fn write_replace(tmp: &Path, path: &Path, bytes: &[u8]) -> Result<(), StoreAuthError> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(tmp).map_err(|error| StoreAuthError::Io {
        path: tmp.display().to_string(),
        message: error.to_string(),
    })?;
    file.write_all(bytes).map_err(|error| StoreAuthError::Io {
        path: tmp.display().to_string(),
        message: error.to_string(),
    })?;
    file.sync_all().map_err(|error| StoreAuthError::Io {
        path: tmp.display().to_string(),
        message: error.to_string(),
    })?;
    std::fs::rename(tmp, path).map_err(|error| StoreAuthError::Io {
        path: path.display().to_string(),
        message: format!("atomic replace: {error}"),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_dir() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("tempdir")
    }

    #[test]
    fn primitive_known_answer_check_passes_against_reference_vectors() {
        primitive_known_answer_check().expect("BLAKE3 wiring must match pinned b3sum vectors");
    }

    #[test]
    fn create_then_open_round_trips_and_macs_are_stable() {
        let dir = keys_dir();
        let created = StoreAuthRoot::create(dir.path()).expect("create");
        let message = b"records-root-digest";
        let mac_before = created
            .mac(MacDomain::NativeImportRecordsRoot, message)
            .expect("mac");
        drop(created);

        let opened = StoreAuthRoot::open(dir.path()).expect("open");
        let mac_after = opened
            .mac(MacDomain::NativeImportRecordsRoot, message)
            .expect("mac");
        assert_eq!(mac_before, mac_after, "MAC must survive a reopen");
        assert!(
            opened
                .verify(MacDomain::NativeImportRecordsRoot, message, &mac_before)
                .expect("verify")
        );
    }

    #[test]
    fn open_or_create_is_idempotent() {
        let dir = keys_dir();
        let first = StoreAuthRoot::open_or_create(dir.path()).expect("first");
        let id = first.current_key_id();
        drop(first);
        let second = StoreAuthRoot::open_or_create(dir.path()).expect("second");
        assert_eq!(id, second.current_key_id(), "must adopt the existing root");
    }

    #[test]
    fn create_twice_is_already_initialized() {
        let dir = keys_dir();
        StoreAuthRoot::create(dir.path()).expect("create");
        let error = StoreAuthRoot::create(dir.path()).expect_err("second create must fail");
        assert!(matches!(error, StoreAuthError::AlreadyInitialized { .. }));
    }

    #[test]
    fn open_uninitialized_is_not_initialized() {
        let dir = keys_dir();
        let error = StoreAuthRoot::open(dir.path()).expect_err("open must fail");
        assert!(matches!(error, StoreAuthError::NotInitialized { .. }));
    }

    #[test]
    fn distinct_domains_yield_distinct_macs() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let message = b"same-bytes";
        let native = root
            .mac(MacDomain::NativeImportRecordsRoot, message)
            .expect("mac");
        let playbook = root
            .mac(MacDomain::PlaybookImportRecordsRoot, message)
            .expect("mac");
        assert_ne!(
            native, playbook,
            "cross-domain MACs over identical bytes must differ"
        );
    }

    #[test]
    fn tampered_message_fails_verification() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let mac = root
            .mac(MacDomain::NativeImportRecordsRoot, b"authentic")
            .expect("mac");
        assert!(
            !root
                .verify(MacDomain::NativeImportRecordsRoot, b"tampered", &mac)
                .expect("verify")
        );
    }

    #[test]
    fn two_stores_have_independent_roots() {
        let dir_a = keys_dir();
        let dir_b = keys_dir();
        let root_a = StoreAuthRoot::create(dir_a.path()).expect("a");
        let root_b = StoreAuthRoot::create(dir_b.path()).expect("b");
        assert_ne!(root_a.current_key_id(), root_b.current_key_id());
        let message = b"cross-store";
        let mac_a = root_a
            .mac(MacDomain::NativeImportRecordsRoot, message)
            .expect("mac");
        assert!(
            !root_b
                .verify(MacDomain::NativeImportRecordsRoot, message, &mac_a)
                .expect("verify"),
            "a foreign store's MAC must not verify"
        );
    }

    #[test]
    fn rotation_moves_prior_key_into_the_window() {
        let dir = keys_dir();
        let mut root = StoreAuthRoot::create(dir.path()).expect("create");
        let old_id = root.current_key_id();
        let message = b"pre-rotation";
        let old_mac = root
            .mac(MacDomain::NativeImportRecordsRoot, message)
            .expect("mac");

        let new_id = root.rotate().expect("rotate");
        assert_ne!(old_id, new_id, "rotation must mint a new key id");
        assert_eq!(new_id, root.current_key_id());

        // Old key still verifies within the window, classed Retired.
        let verdict = root
            .verify_with_key(
                old_id,
                MacDomain::NativeImportRecordsRoot,
                message,
                &old_mac,
            )
            .expect("verify");
        assert_eq!(
            verdict,
            KeyVerification::Match {
                key_class: KeyClass::Retired
            }
        );

        // Current key over the same message is a distinct tag.
        let current_verdict = root
            .verify_with_key(
                new_id,
                MacDomain::NativeImportRecordsRoot,
                message,
                &old_mac,
            )
            .expect("verify");
        assert_eq!(current_verdict, KeyVerification::Mismatch);
    }

    #[test]
    fn rotation_window_evicts_the_oldest_key() {
        let dir = keys_dir();
        let mut root = StoreAuthRoot::create(dir.path()).expect("create");
        let oldest = root.current_key_id();
        let message = b"windowed";
        let oldest_mac = root
            .mac(MacDomain::NativeImportRecordsRoot, message)
            .expect("mac");

        // Rotate MAX_RETIRED_KEYS + 1 times so the very first key falls out.
        for _ in 0..(MAX_RETIRED_KEYS + 1) {
            root.rotate().expect("rotate");
        }
        assert_eq!(root.window_key_ids().len(), MAX_RETIRED_KEYS + 1);
        let verdict = root
            .verify_with_key(
                oldest,
                MacDomain::NativeImportRecordsRoot,
                message,
                &oldest_mac,
            )
            .expect("verify");
        assert_eq!(verdict, KeyVerification::KeyOutsideWindow);
    }

    #[test]
    fn rotation_persists_and_reopens() {
        let dir = keys_dir();
        let mut root = StoreAuthRoot::create(dir.path()).expect("create");
        let old_id = root.current_key_id();
        let message = b"persisted-rotation";
        let old_mac = root
            .mac(MacDomain::NativeImportRecordsRoot, message)
            .expect("mac");
        let new_id = root.rotate().expect("rotate");
        drop(root);

        let reopened = StoreAuthRoot::open(dir.path()).expect("reopen");
        assert_eq!(reopened.current_key_id(), new_id);
        let verdict = reopened
            .verify_with_key(
                old_id,
                MacDomain::NativeImportRecordsRoot,
                message,
                &old_mac,
            )
            .expect("verify");
        assert_eq!(
            verdict,
            KeyVerification::Match {
                key_class: KeyClass::Retired
            }
        );
    }

    #[test]
    fn corrupted_root_fails_the_self_check() {
        let dir = keys_dir();
        StoreAuthRoot::create(dir.path()).expect("create");
        let path = dir.path().join(KEY_FILE_NAME);
        let raw = std::fs::read_to_string(&path).expect("read");
        let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("json");
        // Flip the stored root but leave selfCheck untouched.
        doc["current"]["root"] = serde_json::Value::String("00".repeat(KEY_LEN));
        std::fs::write(&path, doc.to_string()).expect("write");

        let error = StoreAuthRoot::open(dir.path()).expect_err("self-check must fail");
        assert_eq!(error, StoreAuthError::SelfCheckFailed);
    }

    #[test]
    fn schema_mismatch_is_rejected() {
        let dir = keys_dir();
        StoreAuthRoot::create(dir.path()).expect("create");
        let path = dir.path().join(KEY_FILE_NAME);
        let raw = std::fs::read_to_string(&path).expect("read");
        let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("json");
        doc["schema"] = serde_json::Value::String("ee.store_auth.keyfile.v0".to_owned());
        std::fs::write(&path, doc.to_string()).expect("write");

        let error = StoreAuthRoot::open(dir.path()).expect_err("schema must fail");
        assert!(matches!(error, StoreAuthError::SchemaMismatch { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn group_readable_key_file_is_insecure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = keys_dir();
        StoreAuthRoot::create(dir.path()).expect("create");
        let path = dir.path().join(KEY_FILE_NAME);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");

        let error = StoreAuthRoot::open(dir.path()).expect_err("insecure perms must fail");
        assert!(matches!(error, StoreAuthError::InsecurePermissions { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn created_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = keys_dir();
        StoreAuthRoot::create(dir.path()).expect("create");
        let path = dir.path().join(KEY_FILE_NAME);
        let mode = std::fs::symlink_metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode & 0o077, 0, "created key file must be owner-only");
    }

    #[test]
    fn secret_debug_is_redacted_and_mac_debug_shows_hex() {
        let secret = Secret([7_u8; KEY_LEN]);
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        let mac = Mac([0xab_u8; MAC_LEN]);
        assert!(format!("{mac:?}").contains(&"ab".repeat(MAC_LEN)));
    }

    #[test]
    fn key_id_hex_round_trips() {
        let id = KeyId([0x3c_u8; KEY_ID_LEN]);
        let parsed = KeyId::from_hex(&id.to_hex()).expect("round trip");
        assert_eq!(id, parsed);
        assert!(KeyId::from_hex("zz").is_err());
    }

    #[test]
    fn error_degraded_code_is_the_store_auth_code() {
        let error = StoreAuthError::SelfCheckFailed;
        assert_eq!(
            error.degraded_code(),
            MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE
        );
        assert!(!error.message().is_empty());
        assert!(!error.repair().is_empty());
    }

    #[test]
    fn constant_time_eq_matches_semantic_equality() {
        assert!(constant_time_eq(b"abcd", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abce"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
