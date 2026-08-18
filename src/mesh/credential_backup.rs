//! First-class encrypted mesh credential backup (`bd-tc-followup-oo7d2.7`).
//!
//! Ordinary `ee backup` redacts `peerCredentials` and tells restore to
//! re-pair. This module is the separate recovery path: collect every live
//! pair-key and signing-seed slot from [`MeshKeyStore`], encrypt the payload
//! with a passphrase-derived `CHACHA20-POLY1305` key, and write the envelope
//! through [`SecureLocalDir`] so the file inherits Unix `0600` / Windows
//! TokenUser+SYSTEM DACL parity.
//!
//! The passphrase is never accepted on argv. Callers read it from stdin.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{Ordering, compiler_fence};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use super::key_store::{
    KeyStoreError, MeshKeyStore, PairKeyClass, SECRET_BYTES_LEN, SecretBytes, SecureLocalDir,
    SigningKeyClass,
};

/// Schema for the on-disk encrypted envelope.
pub const CREDENTIAL_BACKUP_SCHEMA: &str = "ee.mesh.credentials.backup.v1";

/// Schema for the decrypted payload.
pub const CREDENTIAL_BACKUP_PAYLOAD_SCHEMA: &str = "ee.mesh.credentials.backup.payload.v1";

/// Schema for the CLI/JSON report.
pub const CREDENTIAL_BACKUP_REPORT_SCHEMA: &str = "ee.mesh.credentials.backup.report.v1";

/// BLAKE3 `derive_key` context mixed with the random salt.
pub const CREDENTIAL_BACKUP_KDF_CONTEXT: &str = "ee.mesh.credentials.backup.v1";

/// Stable KDF token recorded in the envelope.
pub const CREDENTIAL_BACKUP_KDF: &str = "blake3-derive-key";

/// Stable AEAD token recorded in the envelope.
pub const CREDENTIAL_BACKUP_AEAD: &str = "chacha20-poly1305";

/// Default file name written into the hardened backup directory.
pub const DEFAULT_CREDENTIAL_BACKUP_FILE_NAME: &str = "credentials.backup.v1.json";

/// Hard cap for one encrypted envelope. Larger than a single key record so a
/// full v1 team (20 members × four nodes × current+next slots) fits.
pub const MAX_CREDENTIAL_BACKUP_BYTES: u64 = 1024 * 1024;

/// Minimum accepted passphrase length. Short secrets fail closed.
pub const MIN_PASSPHRASE_CHARS: usize = 12;

/// Maximum accepted passphrase length (DoS bound).
pub const MAX_PASSPHRASE_CHARS: usize = 1024;

const SALT_LEN: usize = 32;

/// Fail-closed error surface for credential backup/restore.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CredentialBackupError {
    /// The hardened key store or backup directory refused the operation.
    KeyStore(KeyStoreError),
    /// Passphrase policy failed (empty, too short, too long).
    Passphrase {
        /// Human-readable reason.
        message: String,
    },
    /// Encryption, decryption, or CSPRNG failed.
    Crypto {
        /// Human-readable reason. Never includes key or passphrase material.
        message: String,
    },
    /// Envelope or payload bytes failed validation.
    Malformed {
        /// Human-readable reason.
        message: String,
    },
    /// Filesystem I/O outside the key store (plaintext envelope read).
    Io {
        /// Path the operation was acting on.
        path: String,
        /// Operating-system error text.
        message: String,
    },
    /// Restore would overwrite an existing slot without `--overwrite`.
    Conflict {
        /// Human-readable reason naming the conflicting slots.
        message: String,
    },
}

impl std::fmt::Display for CredentialBackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyStore(error) => write!(f, "{error}"),
            Self::Passphrase { message }
            | Self::Crypto { message }
            | Self::Malformed { message }
            | Self::Conflict { message } => f.write_str(message),
            Self::Io { path, message } => {
                write!(f, "credential backup I/O failed at {path}: {message}")
            }
        }
    }
}

impl std::error::Error for CredentialBackupError {}

impl From<KeyStoreError> for CredentialBackupError {
    fn from(error: KeyStoreError) -> Self {
        Self::KeyStore(error)
    }
}

/// Default hardened directory for credential-backup envelopes.
#[must_use]
pub fn mesh_credential_backup_dir(workspace_path: &Path) -> PathBuf {
    crate::policy::store_auth::workspace_keys_dir(workspace_path).join("mesh-credential-backup")
}

/// On-disk encrypted envelope. Ciphertext is hex; no key material appears in
/// the clear.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialBackupEnvelope {
    /// [`CREDENTIAL_BACKUP_SCHEMA`].
    pub schema: String,
    /// [`CREDENTIAL_BACKUP_KDF`].
    pub kdf: String,
    /// [`CREDENTIAL_BACKUP_KDF_CONTEXT`].
    pub kdf_context: String,
    /// [`CREDENTIAL_BACKUP_AEAD`].
    pub aead: String,
    /// 32-byte lowercase hex salt mixed into the KDF.
    pub salt_hex: String,
    /// 12-byte lowercase hex AEAD nonce.
    pub nonce_hex: String,
    /// AEAD ciphertext plus tag, lowercase hex.
    pub ciphertext_hex: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// Number of pair-key slots sealed inside the payload.
    pub pair_count: usize,
    /// Number of signing-seed slots sealed inside the payload.
    pub signing_count: usize,
}

/// Decrypted payload. Secret hex fields are wiped when this value drops.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialBackupPayload {
    /// [`CREDENTIAL_BACKUP_PAYLOAD_SCHEMA`].
    pub schema: String,
    /// Live pair-key slots.
    pub pairs: Vec<BackupPairSlot>,
    /// Live signing-seed slots.
    pub signing: Vec<BackupSigningSlot>,
}

impl std::fmt::Debug for CredentialBackupPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialBackupPayload")
            .field("schema", &self.schema)
            .field("pair_count", &self.pairs.len())
            .field("signing_count", &self.signing.len())
            .finish()
    }
}

impl Drop for CredentialBackupPayload {
    fn drop(&mut self) {
        for slot in &mut self.pairs {
            wipe_string(&mut slot.key_hex);
        }
        for slot in &mut self.signing {
            wipe_string(&mut slot.seed_hex);
        }
    }
}

/// One pair-key slot inside the payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupPairSlot {
    /// Enrolled peer handle.
    pub peer_handle: String,
    /// `current` or `next`.
    pub key_class: String,
    /// Nonzero pair-key generation.
    pub generation: NonZeroU64,
    /// 32-byte pair key as lowercase hex.
    pub key_hex: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
}

impl std::fmt::Debug for BackupPairSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupPairSlot")
            .field("peer_handle", &self.peer_handle)
            .field("key_class", &self.key_class)
            .field("generation", &self.generation)
            .field("key_hex", &"<redacted>")
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// One signing-seed slot inside the payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupSigningSlot {
    /// Local node handle.
    pub node_handle: String,
    /// `current` or `next`.
    pub key_class: String,
    /// Nonzero signing-lineage generation.
    pub generation: NonZeroU64,
    /// 32-byte Ed25519 seed as lowercase hex.
    pub seed_hex: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
}

impl std::fmt::Debug for BackupSigningSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupSigningSlot")
            .field("node_handle", &self.node_handle)
            .field("key_class", &self.key_class)
            .field("generation", &self.generation)
            .field("seed_hex", &"<redacted>")
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Machine-facing report for backup or restore.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialBackupReport {
    /// [`CREDENTIAL_BACKUP_REPORT_SCHEMA`].
    pub schema: String,
    /// `backup` or `restore`.
    pub action: String,
    /// Envelope path that was written or read.
    pub path: String,
    /// Whether a live key store existed at backup time (always true after a
    /// successful restore that created the store).
    pub store_present: bool,
    /// Pair-key slots in the payload.
    pub pair_count: usize,
    /// Signing-seed slots in the payload.
    pub signing_count: usize,
    /// Whether restore was allowed to replace existing slots.
    pub overwrite: bool,
    /// RFC 3339 timestamp for this report.
    pub created_at: String,
}

/// Collect every live slot from `store` into a payload. Missing slots are
/// omitted; retired records stay on disk and are not exported.
pub fn collect_credential_payload(
    store: &MeshKeyStore,
) -> Result<CredentialBackupPayload, CredentialBackupError> {
    let mut pairs = Vec::new();
    for (peer_handle, key_class) in store.list_pair_slots()? {
        let Some(record) = store.load_pair_key(&peer_handle, key_class)? else {
            continue;
        };
        pairs.push(BackupPairSlot {
            peer_handle: record.peer_handle,
            key_class: record.key_class.token().to_owned(),
            generation: record.generation,
            key_hex: hex_lower(record.key.as_bytes()),
            created_at: record.created_at,
        });
    }
    let mut signing = Vec::new();
    for (node_handle, key_class) in store.list_signing_slots()? {
        let Some(record) = store.load_signing_key(&node_handle, key_class)? else {
            continue;
        };
        signing.push(BackupSigningSlot {
            node_handle: record.node_handle,
            key_class: record.key_class.token().to_owned(),
            generation: record.generation,
            seed_hex: hex_lower(record.seed.as_bytes()),
            created_at: record.created_at,
        });
    }
    Ok(CredentialBackupPayload {
        schema: CREDENTIAL_BACKUP_PAYLOAD_SCHEMA.to_owned(),
        pairs,
        signing,
    })
}

/// Encrypt `payload` under `passphrase` and return the envelope.
pub fn encrypt_credential_payload(
    payload: &CredentialBackupPayload,
    passphrase: &str,
    created_at: &str,
) -> Result<CredentialBackupEnvelope, CredentialBackupError> {
    validate_passphrase(passphrase)?;
    validate_created_at(created_at)?;
    let salt = random_bytes::<SALT_LEN>()?;
    let nonce_bytes = random_bytes::<NONCE_LEN>()?;
    let key = derive_backup_key(passphrase, &salt);
    let mut plaintext =
        serde_json::to_vec(payload).map_err(|error| CredentialBackupError::Malformed {
            message: format!("serialize credential payload: {error}"),
        })?;
    let aad = backup_aad(
        CREDENTIAL_BACKUP_SCHEMA,
        CREDENTIAL_BACKUP_KDF,
        CREDENTIAL_BACKUP_AEAD,
        &salt,
        &nonce_bytes,
    );
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, key.as_bytes()).map_err(|_| {
        CredentialBackupError::Crypto {
            message: "failed to bind backup AEAD key".to_owned(),
        }
    })?;
    let sealing_key = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    sealing_key
        .seal_in_place_append_tag(nonce, Aad::from(&aad), &mut plaintext)
        .map_err(|_| CredentialBackupError::Crypto {
            message: "failed to seal credential backup".to_owned(),
        })?;
    let envelope = CredentialBackupEnvelope {
        schema: CREDENTIAL_BACKUP_SCHEMA.to_owned(),
        kdf: CREDENTIAL_BACKUP_KDF.to_owned(),
        kdf_context: CREDENTIAL_BACKUP_KDF_CONTEXT.to_owned(),
        aead: CREDENTIAL_BACKUP_AEAD.to_owned(),
        salt_hex: hex_lower(&salt),
        nonce_hex: hex_lower(&nonce_bytes),
        ciphertext_hex: hex_lower(&plaintext),
        created_at: created_at.to_owned(),
        pair_count: payload.pairs.len(),
        signing_count: payload.signing.len(),
    };
    plaintext.fill(0);
    compiler_fence(Ordering::SeqCst);
    Ok(envelope)
}

/// Decrypt `envelope` with `passphrase` and validate the payload.
pub fn decrypt_credential_payload(
    envelope: &CredentialBackupEnvelope,
    passphrase: &str,
) -> Result<CredentialBackupPayload, CredentialBackupError> {
    validate_passphrase(passphrase)?;
    if envelope.schema != CREDENTIAL_BACKUP_SCHEMA {
        return Err(CredentialBackupError::Malformed {
            message: "unexpected credential-backup envelope schema".to_owned(),
        });
    }
    if envelope.kdf != CREDENTIAL_BACKUP_KDF
        || envelope.kdf_context != CREDENTIAL_BACKUP_KDF_CONTEXT
        || envelope.aead != CREDENTIAL_BACKUP_AEAD
    {
        return Err(CredentialBackupError::Malformed {
            message: "credential-backup envelope names an unsupported kdf or aead".to_owned(),
        });
    }
    validate_created_at(&envelope.created_at)?;
    let salt = decode_hex_exact::<SALT_LEN>(&envelope.salt_hex, "saltHex")?;
    let nonce_bytes = decode_hex_exact::<NONCE_LEN>(&envelope.nonce_hex, "nonceHex")?;
    let mut ciphertext = decode_hex_var(&envelope.ciphertext_hex, "ciphertextHex")?;
    let key = derive_backup_key(passphrase, &salt);
    let aad = backup_aad(
        &envelope.schema,
        &envelope.kdf,
        &envelope.aead,
        &salt,
        &nonce_bytes,
    );
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, key.as_bytes()).map_err(|_| {
        CredentialBackupError::Crypto {
            message: "failed to bind backup AEAD key".to_owned(),
        }
    })?;
    let opening_key = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let opened = opening_key
        .open_in_place(nonce, Aad::from(&aad), &mut ciphertext)
        .map_err(|_| CredentialBackupError::Crypto {
            message: "credential backup could not be decrypted".to_owned(),
        })?;
    let parsed: Result<CredentialBackupPayload, _> = serde_json::from_slice(opened);
    opened.fill(0);
    compiler_fence(Ordering::SeqCst);
    let payload = parsed.map_err(|_| CredentialBackupError::Malformed {
        message: "decrypted credential payload is not valid canonical JSON".to_owned(),
    })?;
    if payload.schema != CREDENTIAL_BACKUP_PAYLOAD_SCHEMA {
        return Err(CredentialBackupError::Malformed {
            message: "unexpected credential-backup payload schema".to_owned(),
        });
    }
    if payload.pairs.len() != envelope.pair_count || payload.signing.len() != envelope.signing_count
    {
        return Err(CredentialBackupError::Malformed {
            message: "credential-backup envelope counts do not match the payload".to_owned(),
        });
    }
    validate_payload_slots(&payload)?;
    Ok(payload)
}

/// Write `envelope` into a hardened directory under `workspace_path`.
pub fn write_credential_backup_envelope(
    workspace_path: &Path,
    dir: &Path,
    file_name: &str,
    envelope: &CredentialBackupEnvelope,
    overwrite: bool,
) -> Result<PathBuf, CredentialBackupError> {
    let secure = SecureLocalDir::open_or_create(workspace_path, dir)?;
    let mut bytes =
        serde_json::to_vec_pretty(envelope).map_err(|error| CredentialBackupError::Malformed {
            message: format!("serialize credential-backup envelope: {error}"),
        })?;
    bytes.push(b'\n');
    let result = if overwrite {
        secure.write_replace_capped(file_name, &bytes, MAX_CREDENTIAL_BACKUP_BYTES)
    } else {
        secure.write_exclusive_capped(file_name, &bytes, MAX_CREDENTIAL_BACKUP_BYTES)
    };
    bytes.fill(0);
    compiler_fence(Ordering::SeqCst);
    result?;
    Ok(secure.path().join(file_name))
}

/// Read an envelope from `path`. The path may live outside the workspace
/// because the bytes are ciphertext; symlink targets are still refused.
pub fn read_credential_backup_envelope(
    path: &Path,
) -> Result<CredentialBackupEnvelope, CredentialBackupError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| CredentialBackupError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(CredentialBackupError::Io {
            path: path.display().to_string(),
            message: "refusing to read a symbolic link".to_owned(),
        });
    }
    if !metadata.is_file() {
        return Err(CredentialBackupError::Io {
            path: path.display().to_string(),
            message: "credential backup must be a regular file".to_owned(),
        });
    }
    if metadata.len() > MAX_CREDENTIAL_BACKUP_BYTES {
        return Err(CredentialBackupError::KeyStore(
            KeyStoreError::CapExceeded {
                path: path.display().to_string(),
                len: metadata.len(),
            },
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| CredentialBackupError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    serde_json::from_slice(&bytes).map_err(|_| CredentialBackupError::Malformed {
        message: "credential-backup envelope is not valid canonical JSON".to_owned(),
    })
}

/// Restore `payload` into `store`. Without `overwrite`, any existing slot
/// fails closed before the first write.
pub fn restore_credential_payload(
    store: &MeshKeyStore,
    payload: &CredentialBackupPayload,
    overwrite: bool,
) -> Result<(), CredentialBackupError> {
    validate_payload_slots(payload)?;
    if !overwrite {
        let mut conflicts = Vec::new();
        for slot in &payload.pairs {
            let class = parse_pair_class(&slot.key_class)?;
            if store.load_pair_key(&slot.peer_handle, class)?.is_some() {
                conflicts.push(format!("pair.{}.{}", slot.peer_handle, slot.key_class));
            }
        }
        for slot in &payload.signing {
            let class = parse_signing_class(&slot.key_class)?;
            if store.load_signing_key(&slot.node_handle, class)?.is_some() {
                conflicts.push(format!("signing.{}.{}", slot.node_handle, slot.key_class));
            }
        }
        if !conflicts.is_empty() {
            return Err(CredentialBackupError::Conflict {
                message: format!(
                    "restore would overwrite existing slots ({}); pass --overwrite to replace",
                    conflicts.join(", ")
                ),
            });
        }
    }
    for slot in &payload.pairs {
        let class = parse_pair_class(&slot.key_class)?;
        let key = decode_secret(&slot.key_hex, "keyHex")?;
        store.store_pair_key(
            &slot.peer_handle,
            class,
            slot.generation,
            &key,
            &slot.created_at,
            overwrite,
        )?;
    }
    for slot in &payload.signing {
        let class = parse_signing_class(&slot.key_class)?;
        let seed = decode_secret(&slot.seed_hex, "seedHex")?;
        store.store_signing_key(
            &slot.node_handle,
            class,
            slot.generation,
            &seed,
            &slot.created_at,
            overwrite,
        )?;
    }
    Ok(())
}

/// Backup the workspace key store to a hardened envelope file.
pub fn backup_workspace_credentials(
    workspace_path: &Path,
    output_dir: &Path,
    file_name: &str,
    passphrase: &str,
    overwrite: bool,
    created_at: &str,
) -> Result<CredentialBackupReport, CredentialBackupError> {
    let (payload, store_present) = match MeshKeyStore::open_existing(workspace_path)? {
        Some(store) => (collect_credential_payload(&store)?, true),
        None => (
            CredentialBackupPayload {
                schema: CREDENTIAL_BACKUP_PAYLOAD_SCHEMA.to_owned(),
                pairs: Vec::new(),
                signing: Vec::new(),
            },
            false,
        ),
    };
    let pair_count = payload.pairs.len();
    let signing_count = payload.signing.len();
    let envelope = encrypt_credential_payload(&payload, passphrase, created_at)?;
    drop(payload);
    let path = write_credential_backup_envelope(
        workspace_path,
        output_dir,
        file_name,
        &envelope,
        overwrite,
    )?;
    Ok(CredentialBackupReport {
        schema: CREDENTIAL_BACKUP_REPORT_SCHEMA.to_owned(),
        action: "backup".to_owned(),
        path: path.display().to_string(),
        store_present,
        pair_count,
        signing_count,
        overwrite,
        created_at: created_at.to_owned(),
    })
}

/// Restore an envelope into the workspace key store.
pub fn restore_workspace_credentials(
    workspace_path: &Path,
    input_path: &Path,
    passphrase: &str,
    overwrite: bool,
    created_at: &str,
) -> Result<CredentialBackupReport, CredentialBackupError> {
    let envelope = read_credential_backup_envelope(input_path)?;
    let payload = decrypt_credential_payload(&envelope, passphrase)?;
    let pair_count = payload.pairs.len();
    let signing_count = payload.signing.len();
    let store = MeshKeyStore::open_or_create(workspace_path)?;
    restore_credential_payload(&store, &payload, overwrite)?;
    Ok(CredentialBackupReport {
        schema: CREDENTIAL_BACKUP_REPORT_SCHEMA.to_owned(),
        action: "restore".to_owned(),
        path: input_path.display().to_string(),
        store_present: true,
        pair_count,
        signing_count,
        overwrite,
        created_at: created_at.to_owned(),
    })
}

fn validate_passphrase(passphrase: &str) -> Result<(), CredentialBackupError> {
    let chars = passphrase.chars().count();
    if passphrase.is_empty() || chars < MIN_PASSPHRASE_CHARS {
        return Err(CredentialBackupError::Passphrase {
            message: format!("passphrase must be at least {MIN_PASSPHRASE_CHARS} characters"),
        });
    }
    if chars > MAX_PASSPHRASE_CHARS {
        return Err(CredentialBackupError::Passphrase {
            message: format!("passphrase must be at most {MAX_PASSPHRASE_CHARS} characters"),
        });
    }
    Ok(())
}

fn validate_created_at(created_at: &str) -> Result<(), CredentialBackupError> {
    if created_at.is_empty() || created_at.len() > 64 {
        return Err(CredentialBackupError::Malformed {
            message: "created_at must be a short RFC 3339 timestamp".to_owned(),
        });
    }
    chrono::DateTime::parse_from_rfc3339(created_at).map_err(|_| {
        CredentialBackupError::Malformed {
            message: "created_at must be a valid RFC 3339 timestamp".to_owned(),
        }
    })?;
    Ok(())
}

fn validate_payload_slots(payload: &CredentialBackupPayload) -> Result<(), CredentialBackupError> {
    for slot in &payload.pairs {
        parse_pair_class(&slot.key_class)?;
        decode_secret(&slot.key_hex, "keyHex")?;
        validate_created_at(&slot.created_at)?;
        if slot.peer_handle.is_empty() {
            return Err(CredentialBackupError::Malformed {
                message: "pair slot is missing a peer handle".to_owned(),
            });
        }
    }
    for slot in &payload.signing {
        parse_signing_class(&slot.key_class)?;
        decode_secret(&slot.seed_hex, "seedHex")?;
        validate_created_at(&slot.created_at)?;
        if slot.node_handle.is_empty() {
            return Err(CredentialBackupError::Malformed {
                message: "signing slot is missing a node handle".to_owned(),
            });
        }
    }
    Ok(())
}

fn parse_pair_class(token: &str) -> Result<PairKeyClass, CredentialBackupError> {
    PairKeyClass::from_token(token).ok_or_else(|| CredentialBackupError::Malformed {
        message: format!("unknown pair key class {token:?}"),
    })
}

fn parse_signing_class(token: &str) -> Result<SigningKeyClass, CredentialBackupError> {
    SigningKeyClass::from_token(token).ok_or_else(|| CredentialBackupError::Malformed {
        message: format!("unknown signing key class {token:?}"),
    })
}

fn derive_backup_key(passphrase: &str, salt: &[u8; SALT_LEN]) -> SecretBytes {
    let mut material = Vec::with_capacity(passphrase.len() + SALT_LEN);
    material.extend_from_slice(passphrase.as_bytes());
    material.extend_from_slice(salt);
    let key = SecretBytes::new(blake3::derive_key(CREDENTIAL_BACKUP_KDF_CONTEXT, &material));
    material.fill(0);
    compiler_fence(Ordering::SeqCst);
    key
}

fn backup_aad(schema: &str, kdf: &str, aead: &str, salt: &[u8], nonce: &[u8]) -> Vec<u8> {
    let mut aad = Vec::new();
    aad.extend_from_slice(schema.as_bytes());
    aad.push(0);
    aad.extend_from_slice(kdf.as_bytes());
    aad.push(0);
    aad.extend_from_slice(aead.as_bytes());
    aad.push(0);
    aad.extend_from_slice(salt);
    aad.push(0);
    aad.extend_from_slice(nonce);
    aad
}

fn random_bytes<const N: usize>() -> Result<[u8; N], CredentialBackupError> {
    let rng = SystemRandom::new();
    let mut bytes = [0_u8; N];
    rng.fill(&mut bytes)
        .map_err(|_| CredentialBackupError::Crypto {
            message: "secure random generation failed".to_owned(),
        })?;
    Ok(bytes)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(*byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(*byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

fn decode_hex_nibble(byte: u8, label: &str) -> Result<u8, CredentialBackupError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(CredentialBackupError::Malformed {
            message: format!("{label} contains a non-lowercase-hex character"),
        }),
    }
}

fn decode_hex_var(value: &str, label: &str) -> Result<Vec<u8>, CredentialBackupError> {
    if value.len() % 2 != 0 {
        return Err(CredentialBackupError::Malformed {
            message: format!("{label} must have even length"),
        });
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks_exact(2) {
        let high = decode_hex_nibble(chunk[0], label)?;
        let low = decode_hex_nibble(chunk[1], label)?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn decode_hex_exact<const N: usize>(
    value: &str,
    label: &str,
) -> Result<[u8; N], CredentialBackupError> {
    let decoded = decode_hex_var(value, label)?;
    if decoded.len() != N {
        return Err(CredentialBackupError::Malformed {
            message: format!("{label} must be {} characters, got {}", N * 2, value.len()),
        });
    }
    let mut bytes = [0_u8; N];
    bytes.copy_from_slice(&decoded);
    Ok(bytes)
}

fn decode_secret(value: &str, label: &str) -> Result<SecretBytes, CredentialBackupError> {
    let bytes = decode_hex_exact::<SECRET_BYTES_LEN>(value, label)?;
    Ok(SecretBytes::new(bytes))
}

fn wipe_string(value: &mut String) {
    let mut bytes = std::mem::take(value).into_bytes();
    bytes.fill(0);
    compiler_fence(Ordering::SeqCst);
}

#[cfg(all(
    test,
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "redox",
        target_vendor = "apple"
    )
))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const CREATED_AT: &str = "2026-08-18T00:00:00Z";
    const PASSPHRASE: &str = "correct horse battery";

    fn generation(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("nonzero")
    }

    fn sample_key(fill: u8) -> SecretBytes {
        SecretBytes::new([fill; SECRET_BYTES_LEN])
    }

    fn seeded_store() -> (tempfile::TempDir, MeshKeyStore) {
        let workspace = tempfile::TempDir::new().expect("tempdir");
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(1),
                &sample_key(7),
                CREATED_AT,
                false,
            )
            .expect("pair current");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Next,
                generation(2),
                &sample_key(8),
                CREATED_AT,
                false,
            )
            .expect("pair next");
        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Current,
                generation(3),
                &sample_key(9),
                CREATED_AT,
                false,
            )
            .expect("signing current");
        (workspace, store)
    }

    #[test]
    fn collect_encrypt_decrypt_restore_round_trips() {
        let (source, store) = seeded_store();
        let payload = collect_credential_payload(&store).expect("collect");
        assert_eq!(payload.pairs.len(), 2);
        assert_eq!(payload.signing.len(), 1);
        let envelope = encrypt_credential_payload(&payload, PASSPHRASE, CREATED_AT).expect("seal");
        assert_eq!(envelope.schema, CREDENTIAL_BACKUP_SCHEMA);
        assert_eq!(envelope.pair_count, 2);
        assert_eq!(envelope.signing_count, 1);
        assert!(
            !envelope
                .ciphertext_hex
                .contains(&"07".repeat(SECRET_BYTES_LEN)),
            "envelope must not echo pair-key hex"
        );

        let dest = tempfile::TempDir::new().expect("dest");
        let dest_store = MeshKeyStore::open_or_create(dest.path()).expect("dest store");
        let opened = decrypt_credential_payload(&envelope, PASSPHRASE).expect("open");
        restore_credential_payload(&dest_store, &opened, false).expect("restore");
        let restored = dest_store
            .load_pair_key("peer-a1", PairKeyClass::Next)
            .expect("load")
            .expect("present");
        assert_eq!(restored.key.as_bytes(), &[8; SECRET_BYTES_LEN]);
        let signing = dest_store
            .load_signing_key("node-local-1", SigningKeyClass::Current)
            .expect("load signing")
            .expect("present");
        assert_eq!(signing.seed.as_bytes(), &[9; SECRET_BYTES_LEN]);
        drop(source);
    }

    #[test]
    fn wrong_passphrase_fails_closed() {
        let (_workspace, store) = seeded_store();
        let payload = collect_credential_payload(&store).expect("collect");
        let envelope = encrypt_credential_payload(&payload, PASSPHRASE, CREATED_AT).expect("seal");
        let error = decrypt_credential_payload(&envelope, "wrong horse battery")
            .expect_err("wrong passphrase");
        assert!(matches!(error, CredentialBackupError::Crypto { .. }));
    }

    #[test]
    fn restore_without_overwrite_conflicts() {
        let (_workspace, store) = seeded_store();
        let payload = collect_credential_payload(&store).expect("collect");
        let error = restore_credential_payload(&store, &payload, false)
            .expect_err("existing slots must conflict");
        assert!(matches!(error, CredentialBackupError::Conflict { .. }));
        restore_credential_payload(&store, &payload, true).expect("overwrite");
    }

    #[test]
    fn workspace_backup_writes_owner_only_envelope() {
        let (workspace, _store) = seeded_store();
        let dir = mesh_credential_backup_dir(workspace.path());
        let report = backup_workspace_credentials(
            workspace.path(),
            &dir,
            DEFAULT_CREDENTIAL_BACKUP_FILE_NAME,
            PASSPHRASE,
            false,
            CREATED_AT,
        )
        .expect("backup");
        assert_eq!(report.action, "backup");
        assert_eq!(report.pair_count, 2);
        assert!(report.store_present);
        let path = PathBuf::from(&report.path);
        let metadata = std::fs::metadata(&path).expect("envelope metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let dir_meta = std::fs::metadata(&dir).expect("dir metadata");
        assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700);

        let dest = tempfile::TempDir::new().expect("dest");
        restore_workspace_credentials(dest.path(), &path, PASSPHRASE, false, CREATED_AT)
            .expect("restore");
        let dest_store = MeshKeyStore::open_existing(dest.path())
            .expect("open dest")
            .expect("store present");
        let pair = dest_store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect("load")
            .expect("present");
        assert_eq!(pair.key.as_bytes(), &[7; SECRET_BYTES_LEN]);
    }

    #[test]
    fn empty_store_backup_is_valid() {
        let workspace = tempfile::TempDir::new().expect("tempdir");
        let dir = mesh_credential_backup_dir(workspace.path());
        let report = backup_workspace_credentials(
            workspace.path(),
            &dir,
            DEFAULT_CREDENTIAL_BACKUP_FILE_NAME,
            PASSPHRASE,
            false,
            CREATED_AT,
        )
        .expect("empty backup");
        assert!(!report.store_present);
        assert_eq!(report.pair_count, 0);
        assert_eq!(report.signing_count, 0);
    }

    #[test]
    fn short_passphrase_is_rejected() {
        let error = validate_passphrase("too-short").expect_err("short");
        assert!(matches!(error, CredentialBackupError::Passphrase { .. }));
    }
}
